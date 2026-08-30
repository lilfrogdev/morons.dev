mod records;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use self::records::{
    CREDENTIAL_AUDIT_ACCEPTED, CREDENTIAL_AUDIT_COMPLETED, CREDENTIAL_AUDIT_DISPATCHED,
    CREDENTIAL_AUDIT_NOT_APPLIED, CREDENTIAL_FACT_COMPLETED, CREDENTIAL_FACT_DISPATCHED,
    CREDENTIAL_FACT_NOT_APPLIED, CREDENTIAL_REQUEST_COMPLETED, CREDENTIAL_REQUEST_DISPATCHED,
    CREDENTIAL_REQUEST_NOT_APPLIED, CREDENTIAL_REQUEST_PREPARED, CredentialMutationRequest,
    completed_request_result, insert_credential_audit_fact, insert_credential_operation_fact,
    load_incomplete_credential_requests, load_required_credential_request,
    next_credential_generation, update_credential_request_outcome,
    validate_credential_request_records, validate_current_request, validate_request_identity,
};
use super::{
    Backend,
    records::{
        MUTATION_OPERATION_CREDENTIAL_REMOVE, MUTATION_OPERATION_CREDENTIAL_SET,
        current_time_milliseconds, load_mutation_operation, next_sequence,
        nonnegative_integer_from_row, random_identifier, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
    credentials::StoredOpenCodeApiKey,
};

const MAX_CREDENTIAL_MUTATIONS: i64 = 10_000;

impl Backend {
    pub(crate) fn open_code_credential_status(
        &mut self,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        self.recover_credential_mutations()?;
        self.validate_credential_state()?;
        Ok(self.credentials.status())
    }

    pub(crate) fn set_open_code_credential(
        &mut self,
        request_id: MutationRequestId,
        expected_generation: u64,
        api_key: StoredOpenCodeApiKey,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        self.mutate_credential(
            request_id,
            MUTATION_OPERATION_CREDENTIAL_SET,
            expected_generation,
            Some(api_key),
        )
    }

    pub(crate) fn remove_open_code_credential(
        &mut self,
        request_id: MutationRequestId,
        expected_generation: u64,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        self.mutate_credential(
            request_id,
            MUTATION_OPERATION_CREDENTIAL_REMOVE,
            expected_generation,
            None,
        )
    }

    fn mutate_credential(
        &mut self,
        request_id: MutationRequestId,
        operation_kind: i64,
        expected_generation: u64,
        api_key: Option<StoredOpenCodeApiKey>,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        self.recover_credential_mutations()?;
        match load_mutation_operation(&self.connection, request_id)? {
            Some(existing_operation) if existing_operation != operation_kind => {
                return Err(PersistenceError::RequestConflict);
            }
            Some(_) => {
                let existing = load_required_credential_request(&self.connection, request_id)?;
                validate_request_identity(&existing, operation_kind, expected_generation)?;
                return completed_request_result(&existing);
            }
            None => {}
        }

        if self.credentials.status().generation != expected_generation {
            return Err(PersistenceError::CredentialGenerationConflict);
        }
        next_credential_generation(expected_generation)?;
        let request =
            self.prepare_credential_mutation(request_id, operation_kind, expected_generation)?;
        if let Err(error) = self.dispatch_credential_mutation(&request) {
            self.recover_credential_mutations()?;
            return Err(error);
        }
        if let Err(error) =
            self.credentials
                .apply(expected_generation, *request_id.as_bytes(), api_key)
        {
            if self.credentials.is_consistent() {
                self.recover_credential_mutations()?;
            }
            return Err(error);
        }
        match self.complete_credential_mutation(&request) {
            Ok(status) => Ok(status),
            Err(error) => {
                self.recover_credential_mutations()?;
                Err(error)
            }
        }
    }

    fn prepare_credential_mutation(
        &mut self,
        request_id: MutationRequestId,
        operation_kind: i64,
        expected_generation: u64,
    ) -> Result<CredentialMutationRequest, PersistenceError> {
        let audit_id = random_identifier()?;
        let accepted_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_mutation_operation(&transaction, request_id)?.is_some() {
            return Err(PersistenceError::RequestConflict);
        }
        let credential_mutations: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM credential_mutation_requests",
            [],
            |row| row.get(0),
        )?;
        if credential_mutations >= MAX_CREDENTIAL_MUTATIONS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::CredentialMutations,
            });
        }
        let accepted_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id,
                operation_kind,
                accepted_sequence,
                accepted_at_milliseconds
            ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                operation_kind,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO credential_mutation_requests (
                request_id,
                operation_kind,
                expected_generation,
                accepted_sequence,
                accepted_at_milliseconds,
                state,
                result_generation,
                result_configured
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
            params![
                &request_id.as_bytes()[..],
                operation_kind,
                sequence_to_sql(expected_generation)?,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
                CREDENTIAL_REQUEST_PREPARED,
            ],
        )?;
        transaction.execute(
            "INSERT INTO credential_audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                actor_kind,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            params![
                &audit_id[..],
                sequence_to_sql(audit_sequence)?,
                &request_id.as_bytes()[..],
                CREDENTIAL_AUDIT_ACCEPTED,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.commit()?;
        Ok(CredentialMutationRequest {
            request_id,
            operation_kind,
            expected_generation,
            accepted_sequence,
            accepted_at_milliseconds,
            state: CREDENTIAL_REQUEST_PREPARED,
            result: None,
        })
    }

    fn dispatch_credential_mutation(
        &mut self,
        request: &CredentialMutationRequest,
    ) -> Result<(), PersistenceError> {
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let created_at_milliseconds = current_time_milliseconds()?;
        let credential_generation = next_credential_generation(request.expected_generation)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_current_request(&transaction, request, CREDENTIAL_REQUEST_PREPARED)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_credential_operation_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            request.request_id,
            CREDENTIAL_FACT_DISPATCHED,
            credential_generation,
            created_at_milliseconds,
        )?;
        insert_credential_audit_fact(
            &transaction,
            &audit_id,
            audit_sequence,
            request.request_id,
            CREDENTIAL_AUDIT_DISPATCHED,
            created_at_milliseconds,
        )?;
        let changed = transaction.execute(
            "UPDATE credential_mutation_requests
             SET state = ?2
             WHERE request_id = ?1 AND state = ?3",
            params![
                &request.request_id.as_bytes()[..],
                CREDENTIAL_REQUEST_DISPATCHED,
                CREDENTIAL_REQUEST_PREPARED,
            ],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a credential mutation changed before dispatch",
            });
        }
        transaction.commit()?;
        Ok(())
    }

    fn complete_credential_mutation(
        &mut self,
        request: &CredentialMutationRequest,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        let status = self.credentials.status();
        let expected_generation = next_credential_generation(request.expected_generation)?;
        if status.generation != expected_generation
            || status.configured != (request.operation_kind == MUTATION_OPERATION_CREDENTIAL_SET)
            || self.credentials.state().mutation_marker() != request.request_id.as_bytes()
        {
            return Err(PersistenceError::InvalidState {
                reason: "credential state does not match its dispatched mutation",
            });
        }

        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let created_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_current_request(&transaction, request, CREDENTIAL_REQUEST_DISPATCHED)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_credential_operation_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            request.request_id,
            CREDENTIAL_FACT_COMPLETED,
            status.generation,
            created_at_milliseconds,
        )?;
        insert_credential_audit_fact(
            &transaction,
            &audit_id,
            audit_sequence,
            request.request_id,
            CREDENTIAL_AUDIT_COMPLETED,
            created_at_milliseconds,
        )?;
        update_credential_request_outcome(
            &transaction,
            request.request_id,
            CREDENTIAL_REQUEST_DISPATCHED,
            CREDENTIAL_REQUEST_COMPLETED,
            Some(status),
        )?;
        transaction.commit()?;
        Ok(status)
    }

    pub(super) fn recover_credential_mutations(&mut self) -> Result<(), PersistenceError> {
        self.credentials.ensure_consistent()?;
        validate_credential_request_records(&self.connection)?;
        let requests = load_incomplete_credential_requests(&self.connection)?;
        for request in requests {
            match request.state {
                CREDENTIAL_REQUEST_PREPARED => {
                    self.mark_credential_mutation_not_applied(&request)?
                }
                CREDENTIAL_REQUEST_DISPATCHED => {
                    let status = self.credentials.status();
                    let installed_generation =
                        next_credential_generation(request.expected_generation)?;
                    let installed = status.generation == installed_generation
                        && status.configured
                            == (request.operation_kind == MUTATION_OPERATION_CREDENTIAL_SET)
                        && self.credentials.state().mutation_marker()
                            == request.request_id.as_bytes();
                    if installed {
                        self.complete_credential_mutation(&request)?;
                    } else if status.generation == request.expected_generation {
                        self.mark_credential_mutation_not_applied(&request)?;
                    } else {
                        return Err(PersistenceError::InvalidState {
                            reason: "credential state cannot reconcile its dispatched mutation",
                        });
                    }
                }
                _ => {
                    return Err(PersistenceError::InvalidState {
                        reason: "an incomplete credential mutation has an invalid state",
                    });
                }
            }
        }
        self.validate_credential_state()
    }

    fn mark_credential_mutation_not_applied(
        &mut self,
        request: &CredentialMutationRequest,
    ) -> Result<(), PersistenceError> {
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let created_at_milliseconds = current_time_milliseconds()?;
        let credential_generation = next_credential_generation(request.expected_generation)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_current_request(&transaction, request, request.state)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_credential_operation_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            request.request_id,
            CREDENTIAL_FACT_NOT_APPLIED,
            credential_generation,
            created_at_milliseconds,
        )?;
        insert_credential_audit_fact(
            &transaction,
            &audit_id,
            audit_sequence,
            request.request_id,
            CREDENTIAL_AUDIT_NOT_APPLIED,
            created_at_milliseconds,
        )?;
        update_credential_request_outcome(
            &transaction,
            request.request_id,
            request.state,
            CREDENTIAL_REQUEST_NOT_APPLIED,
            None,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn validate_credential_state(&self) -> Result<(), PersistenceError> {
        self.credentials.ensure_consistent()?;
        validate_credential_request_records(&self.connection)?;
        let status = self.credentials.status();
        let completed_count = self.connection.query_row(
            "SELECT COUNT(*) FROM credential_mutation_requests WHERE state = ?1",
            [CREDENTIAL_REQUEST_COMPLETED],
            |row| nonnegative_integer_from_row(row, 0),
        )?;
        if completed_count != status.generation {
            return Err(PersistenceError::InvalidState {
                reason: "credential generations are not contiguous",
            });
        }
        let latest = self
            .connection
            .query_row(
                "SELECT request_id, result_generation, result_configured
                 FROM credential_mutation_requests
                 WHERE state = ?1
                 ORDER BY result_generation DESC
                 LIMIT 1",
                [CREDENTIAL_REQUEST_COMPLETED],
                |row| {
                    Ok((
                        row.get::<_, [u8; 16]>(0)?,
                        nonnegative_integer_from_row(row, 1)?,
                        row.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()?;
        match latest {
            None if status
                == (OpenCodeCredentialStatus {
                    configured: false,
                    generation: 0,
                }) =>
            {
                Ok(())
            }
            Some((request_id, generation, configured))
                if generation == status.generation
                    && configured == status.configured
                    && &request_id == self.credentials.state().mutation_marker() =>
            {
                Ok(())
            }
            _ => Err(PersistenceError::InvalidState {
                reason: "credential file state conflicts with durable mutation history",
            }),
        }
    }
}
