use serde::{Deserialize, Serialize};

use super::{Backend, tool_execution::GenerationPublication};
use crate::{
    persistence::{
        CommandResources, PersistenceError, RepositoryImportOutcome, RunId, ToolCallId,
        TranscriptEntry, run_types::ToolOperationId,
    },
    tools::{ToolInput, ToolResult},
};

pub(super) const COMMAND_POLICY_VERSION: u16 = 1;
pub(super) const COMMAND_LIMITS_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandBinding {
    pub workspace_id: [u8; 16],
    pub active_generation_id: [u8; 16],
    pub generation_id: [u8; 16],
    pub image_generation_id: [u8; 16],
    pub image_manifest_digest: [u8; 32],
    pub source_manifest_digest: [u8; 32],
    pub source_file_count: u64,
    pub source_directory_count: u64,
    pub source_logical_bytes: u64,
    pub policy_version: u16,
    pub limits_version: u16,
}

impl Backend {
    pub(crate) fn prepare_command_operation(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        resources: CommandResources,
        generation_id: [u8; 16],
        source: RepositoryImportOutcome,
    ) -> Result<CommandBinding, PersistenceError> {
        let run = super::run_records::load_required_run(&self.connection, run_id)?;
        if run.execution_image_generation != Some(resources.image_generation_id)
            || self.active_worktree_generation(&resources.workspace_id)?
                != resources.active_generation_id
        {
            return Err(PersistenceError::InvalidState {
                reason: "command resources changed before preparation",
            });
        }
        let input = self.connection.query_row(
            "SELECT input_payload FROM tool_calls WHERE call_id = ?1 AND run_id = ?2",
            [&call_id.as_bytes()[..], &run_id.as_bytes()[..]],
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        let input: ToolInput =
            serde_json::from_slice(&input).map_err(|_| PersistenceError::InvalidState {
                reason: "a command call has invalid typed input",
            })?;
        if !matches!(input, ToolInput::RunCommand { .. }) {
            return Err(PersistenceError::InvalidState {
                reason: "a non-command tool used command preparation",
            });
        }
        let binding = CommandBinding {
            workspace_id: resources.workspace_id,
            active_generation_id: resources.active_generation_id,
            generation_id,
            image_generation_id: resources.image_generation_id,
            image_manifest_digest: resources.image_manifest_digest,
            source_manifest_digest: source.manifest_digest,
            source_file_count: source.file_count,
            source_directory_count: source.directory_count,
            source_logical_bytes: source.logical_bytes,
            policy_version: COMMAND_POLICY_VERSION,
            limits_version: COMMAND_LIMITS_VERSION,
        };
        let payload = serde_json::to_vec(&binding).map_err(|_| PersistenceError::InvalidState {
            reason: "a command binding could not be encoded",
        })?;
        self.prepare_tool_operation(run_id, call_id, operation_id, Some(payload))?;
        Ok(binding)
    }

    pub(crate) fn complete_command_result(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        result: ToolResult,
        publication: Option<(CommandBinding, RepositoryImportOutcome)>,
    ) -> Result<TranscriptEntry, PersistenceError> {
        let publication = publication.map(|(binding, outcome)| GenerationPublication {
            workspace_id: binding.workspace_id,
            predecessor_generation_id: binding.active_generation_id,
            generation_id: binding.generation_id,
            outcome,
        });
        self.complete_tool_result_with_publication(
            run_id,
            call_id,
            operation_id,
            result,
            publication,
        )
    }
}

pub(crate) fn command_binding_is_valid(payload: &[u8]) -> bool {
    serde_json::from_slice::<CommandBinding>(payload).is_ok_and(|binding| {
        binding.workspace_id.iter().any(|byte| *byte != 0)
            && binding.active_generation_id.iter().any(|byte| *byte != 0)
            && binding.generation_id.iter().any(|byte| *byte != 0)
            && binding.image_generation_id.iter().any(|byte| *byte != 0)
            && binding.policy_version == COMMAND_POLICY_VERSION
            && binding.limits_version == COMMAND_LIMITS_VERSION
            && binding.generation_id != binding.active_generation_id
    })
}
