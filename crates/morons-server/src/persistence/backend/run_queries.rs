use rusqlite::params;
use sha2::Digest as _;

use super::{
    Backend,
    records::{load_session, sequence_to_sql},
    repository_import::workspace_summary_at_sequence,
    run_records::{load_required_run, load_scoped_run, transcript_entry_from_row},
    session_events::{active_run_id_at_sequence, load_run_at_sequence, session_event_high_water},
};
use crate::persistence::{
    PersistenceError, Run, RunId, SessionEventCursor, SessionId, TranscriptCursor, TranscriptEntry,
    TranscriptPage,
    run_types::{
        CONTEXT_POLICY_VERSION, LEGACY_CONTEXT_POLICY_VERSION, LEGACY_SKILL_CONTEXT_POLICY_VERSION,
        RunContext,
    },
};

impl Backend {
    pub(crate) fn get_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<Run>, PersistenceError> {
        if load_session(&self.connection, session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }
        load_scoped_run(&self.connection, session_id, run_id)
    }

    pub(crate) fn list_session_transcript(
        &self,
        session_id: SessionId,
        cursor: Option<TranscriptCursor>,
        limit: u16,
    ) -> Result<TranscriptPage, PersistenceError> {
        let session =
            load_session(&self.connection, session_id)?.ok_or(PersistenceError::SessionNotFound)?;
        let current_entry_high_water = self
            .connection
            .query_row(
                "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .and_then(|value| {
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
            })?;
        let current_event_high_water = session_event_high_water(&self.connection, session_id)?;
        if cursor.is_some_and(|cursor| cursor.session_id() != session_id) {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor belongs to another session",
            });
        }
        let (snapshot_entry_sequence, snapshot_event_sequence, after_entry_sequence) = cursor
            .map_or(
                (current_entry_high_water, current_event_high_water, 0),
                |cursor| {
                    (
                        cursor.snapshot_entry_sequence(),
                        cursor.snapshot_event_sequence(),
                        cursor.after_entry_sequence(),
                    )
                },
            );
        if snapshot_entry_sequence > current_entry_high_water
            || snapshot_event_sequence > current_event_high_water
            || after_entry_sequence > snapshot_entry_sequence
        {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor is outside the available snapshot",
            });
        }
        let snapshot_entry_high_water = self.connection.query_row(
            "SELECT COALESCE(MAX(entry_sequence), 0) FROM (
                 SELECT entry_sequence FROM session_entries
                 WHERE session_id = ?1 AND fact_sequence <= ?2
                 UNION ALL
                 SELECT entry_sequence FROM local_commands
                 WHERE session_id = ?1 AND updated_sequence <= ?2 AND state BETWEEN 3 AND 5
             )",
            params![
                &session_id.as_bytes()[..],
                sequence_to_sql(snapshot_event_sequence)?
            ],
            |row| row.get::<_, i64>(0),
        )?;
        if u64::try_from(snapshot_entry_high_water).ok() != Some(snapshot_entry_sequence) {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor has inconsistent snapshot high waters",
            });
        }
        let cuts_run_acceptance: bool = self.connection.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM session_entries AS entry
                 INNER JOIN run_accepted_facts AS run ON run.run_id = entry.run_id
                 WHERE entry.session_id = ?1
                   AND entry.entry_sequence <= ?2
                   AND run.fact_sequence > ?3
             )",
            params![
                &session_id.as_bytes()[..],
                sequence_to_sql(snapshot_entry_sequence)?,
                sequence_to_sql(snapshot_event_sequence)?
            ],
            |row| row.get(0),
        )?;
        if cuts_run_acceptance {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor cuts an atomic run acceptance",
            });
        }

        let mut statement = self.connection.prepare(
            "SELECT
                entry.entry_sequence,
                entry.message_id,
                entry.run_id,
                entry.entry_kind,
                entry.open_code_service,
                entry.model_id,
                entry.text,
                entry.refusal,
                entry.created_at_milliseconds,
                entry.assistant_phase,
                entry.tool_call_id,
                call.operation_id,
                call.provider_operation_id,
                call.input_payload,
                result.result_payload
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1
               AND entry.entry_sequence > ?2
               AND entry.entry_sequence <= ?3
               AND entry.fact_sequence <= ?4
             ORDER BY entry.entry_sequence
             LIMIT ?5",
        )?;
        let entries = statement
            .query_map(
                params![
                    &session_id.as_bytes()[..],
                    sequence_to_sql(after_entry_sequence)?,
                    sequence_to_sql(snapshot_entry_sequence)?,
                    sequence_to_sql(snapshot_event_sequence)?,
                    i64::from(limit) + 1,
                ],
                transcript_entry_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut entries = entries;
        entries.extend(self.list_local_command_entries(
            session_id,
            after_entry_sequence,
            snapshot_entry_sequence,
            snapshot_event_sequence,
            limit.saturating_add(1),
        )?);
        entries.sort_by_key(TranscriptEntry::entry_sequence);
        entries.truncate(usize::from(limit) + 1);
        let has_more = entries.len() > usize::from(limit);
        if has_more {
            entries.pop();
        }
        self.attach_image_metadata(session_id, &mut entries)?;
        let next_cursor = if has_more {
            let after_entry_sequence = entries
                .last()
                .ok_or(PersistenceError::InvalidState {
                    reason: "a transcript page lost its continuation entry",
                })?
                .entry_sequence();
            Some(TranscriptCursor::new(
                session_id,
                snapshot_entry_sequence,
                snapshot_event_sequence,
                after_entry_sequence,
            ))
        } else {
            None
        };
        let active_run_id =
            active_run_id_at_sequence(&self.connection, session_id, snapshot_event_sequence)?;
        let mut run_ids = entries
            .iter()
            .filter_map(TranscriptEntry::run_id)
            .collect::<Vec<_>>();
        if let Some(active_run_id) = active_run_id
            && !run_ids.contains(&active_run_id)
        {
            run_ids.push(active_run_id);
        }
        let runs = run_ids
            .into_iter()
            .map(|run_id| load_run_at_sequence(&self.connection, run_id, snapshot_event_sequence))
            .collect::<Result<Vec<_>, _>>()?;
        let workspace =
            workspace_summary_at_sequence(&self.connection, session_id, snapshot_event_sequence)?;
        let active_command_id = self.active_local_command(session_id)?;
        Ok(TranscriptPage {
            session,
            workspace,
            entries,
            runs,
            active_run_id,
            active_command_id,
            next_cursor,
            event_cursor: SessionEventCursor::new(session_id, snapshot_event_sequence),
        })
    }

    fn attach_image_metadata(
        &self,
        session_id: SessionId,
        entries: &mut [TranscriptEntry],
    ) -> Result<(), PersistenceError> {
        for entry in entries {
            if let TranscriptEntry::UserMessage {
                id,
                text,
                attachments,
                ..
            } = entry
            {
                let loaded = self.load_message_image_attachments(session_id, *id)?;
                if !crate::persistence::images::valid_stored_attachments(text, &loaded) {
                    return Err(PersistenceError::InvalidState {
                        reason: "user message image attachment metadata is invalid",
                    });
                }
                *attachments = loaded;
            }
        }
        Ok(())
    }

    pub(crate) fn load_run_context(&self, run_id: RunId) -> Result<RunContext, PersistenceError> {
        let run = load_required_run(&self.connection, run_id)?;
        if !matches!(
            run.context_policy_version,
            CONTEXT_POLICY_VERSION
                | LEGACY_SKILL_CONTEXT_POLICY_VERSION
                | LEGACY_CONTEXT_POLICY_VERSION
        ) {
            return Err(PersistenceError::InvalidState {
                reason: "a run uses an unsupported context policy version",
            });
        }
        let current_entry_high_water = self.connection.query_row(
            "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
            [&run.session_id.as_bytes()[..]],
            |row| row.get::<_, i64>(0),
        )?;
        let current_entry_high_water = u64::try_from(current_entry_high_water).map_err(|_| {
            PersistenceError::InvalidState {
                reason: "the current run entry high water is invalid",
            }
        })?;
        if current_entry_high_water < run.source_entry_high_water {
            return Err(PersistenceError::InvalidState {
                reason: "the current run context precedes its accepted source",
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT
                entry.entry_sequence,
                entry.message_id,
                entry.run_id,
                entry.entry_kind,
                entry.open_code_service,
                entry.model_id,
                entry.text,
                entry.refusal,
                entry.created_at_milliseconds,
                entry.assistant_phase,
                entry.tool_call_id,
                call.operation_id,
                call.provider_operation_id,
                call.input_payload,
                result.result_payload
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1 AND entry.entry_sequence <= ?2
             ORDER BY entry.entry_sequence",
        )?;
        let entries = statement
            .query_map(
                params![
                    &run.session_id.as_bytes()[..],
                    sequence_to_sql(current_entry_high_water)?
                ],
                transcript_entry_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut entries = entries;
        self.attach_image_metadata(run.session_id, &mut entries)?;
        entries.extend(self.list_local_command_entries(
            run.session_id,
            0,
            current_entry_high_water,
            session_event_high_water(&self.connection, run.session_id)?,
            u16::try_from(crate::persistence::run_types::MAX_CONTEXT_ENTRIES).map_err(|_| {
                PersistenceError::InvalidState {
                    reason: "the context entry limit is invalid",
                }
            })?,
        )?);
        entries.sort_by_key(TranscriptEntry::entry_sequence);
        let all_entries_reach_high_water =
            entries.last().map(TranscriptEntry::entry_sequence) == Some(current_entry_high_water);
        entries.retain(|entry| {
            !matches!(
                entry,
                TranscriptEntry::LocalCommand {
                    context_visible: false,
                    ..
                }
            )
        });
        if entries.is_empty() || !all_entries_reach_high_water {
            return Err(PersistenceError::InvalidState {
                reason: "a run context high water is not present",
            });
        }
        let mut attachment_data = std::collections::HashMap::new();
        let mut attachment_count = 0_usize;
        let mut attachment_bytes = 0_u64;
        for entry in &entries {
            match entry {
                TranscriptEntry::UserMessage { attachments, .. } => {
                    attachment_count = attachment_count.checked_add(attachments.len()).ok_or(
                        PersistenceError::ResourceLimit {
                            resource: crate::persistence::PersistenceResourceLimit::Context,
                        },
                    )?;
                    for attachment in attachments {
                        attachment_bytes = attachment_bytes.checked_add(attachment.bytes).ok_or(
                            PersistenceError::ResourceLimit {
                                resource: crate::persistence::PersistenceResourceLimit::Context,
                            },
                        )?;
                        if attachment_count > crate::persistence::images::MAX_CONTEXT_IMAGES
                            || attachment_bytes
                                > crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES
                        {
                            return Err(PersistenceError::ResourceLimit {
                                resource: crate::persistence::PersistenceResourceLimit::Context,
                            });
                        }
                        let bytes = self.read_image_attachment(run.session_id, attachment.id)?;
                        if bytes.len() as u64 != attachment.bytes
                            || sha2::Sha256::digest(&bytes)[..] != attachment.digest
                            || attachment_data.insert(attachment.id, bytes).is_some()
                        {
                            return Err(PersistenceError::InvalidState {
                                reason: "run context image attachment data is invalid",
                            });
                        }
                    }
                }
                TranscriptEntry::ToolResult {
                    result:
                        crate::tools::ToolResult::Ok {
                            output: crate::tools::ToolOutput::ReadImage { image, .. },
                        },
                    ..
                } => {
                    attachment_bytes = attachment_bytes.checked_add(image.bytes).ok_or(
                        PersistenceError::ResourceLimit {
                            resource: crate::persistence::PersistenceResourceLimit::Context,
                        },
                    )?;
                    let attachment_id = image
                        .attachment_id
                        .map(crate::persistence::ImageAttachmentId::from_bytes)
                        .ok_or(PersistenceError::InvalidState {
                            reason: "read image result is missing its attachment identifier",
                        })?;
                    let bytes = self.read_image_attachment(run.session_id, attachment_id)?;
                    let digest = sha2::Sha256::digest(&bytes)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    if bytes.len() as u64 != image.bytes
                        || digest != image.sha256
                        || attachment_data.insert(attachment_id, bytes).is_some()
                    {
                        return Err(PersistenceError::InvalidState {
                            reason: "read image attachment data is invalid",
                        });
                    }
                    attachment_count =
                        attachment_count
                            .checked_add(1)
                            .ok_or(PersistenceError::ResourceLimit {
                                resource: crate::persistence::PersistenceResourceLimit::Context,
                            })?;
                    if attachment_count > crate::persistence::images::MAX_CONTEXT_IMAGES
                        || attachment_bytes > crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES
                    {
                        return Err(PersistenceError::ResourceLimit {
                            resource: crate::persistence::PersistenceResourceLimit::Context,
                        });
                    }
                }
                _ => {}
            }
        }
        if run.context_policy_version != CONTEXT_POLICY_VERSION && attachment_count != 0 {
            return Err(PersistenceError::InvalidState {
                reason: "a legacy run unexpectedly contains image attachments",
            });
        }
        let skills = load_run_skills(&self.connection, run_id)?;
        if run.context_policy_version == LEGACY_CONTEXT_POLICY_VERSION && !skills.skills.is_empty()
        {
            return Err(PersistenceError::InvalidState {
                reason: "a legacy run unexpectedly contains a skill context",
            });
        }
        let skill_context_bytes =
            skills
                .context_bytes()
                .ok_or(PersistenceError::ResourceLimit {
                    resource: crate::persistence::PersistenceResourceLimit::Context,
                })?;
        let context_bytes = entries
            .iter()
            .try_fold(0_u64, |total, entry| {
                let bytes = match entry {
                    TranscriptEntry::UserMessage { text, .. }
                    | TranscriptEntry::AssistantMessage { text, .. } => text.len(),
                    TranscriptEntry::ToolCall { input, .. } => {
                        serde_json::to_vec(input).ok()?.len()
                    }
                    TranscriptEntry::ToolResult { result, .. } => {
                        result.provider_output().ok()?.len()
                    }
                    TranscriptEntry::LocalCommand {
                        command,
                        stdout,
                        stderr,
                        ..
                    } => command
                        .len()
                        .checked_add(stdout.len())?
                        .checked_add(stderr.len())?,
                };
                total.checked_add(bytes as u64)
            })
            .and_then(|bytes| bytes.checked_add(skill_context_bytes as u64))
            .and_then(|bytes| bytes.checked_add((attachment_count as u64).checked_mul(8_192)?))
            .ok_or(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Context,
            })?;
        let context_items = entries
            .len()
            .checked_add(skills.skills.len())
            .and_then(|items| items.checked_add(attachment_count))
            .ok_or(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Context,
            })?;
        let estimated_input_tokens =
            crate::persistence::run_types::conservative_input_token_estimate(
                context_bytes,
                context_items as u64,
            )
            .filter(|estimate| *estimate <= run.maximum_input_tokens)
            .ok_or(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Context,
            })?;
        let working_directory = self.connection.query_row(
            "SELECT working_directory FROM sessions WHERE session_id = ?1",
            [&run.session_id.as_bytes()[..]],
            |row| row.get::<_, Option<String>>(0),
        )?;
        let legacy_workspace_exists: bool = self.connection.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM repository_import_requests
                WHERE session_id = ?1 AND state = 2
             )",
            [&run.session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        let versions = (run.tool_catalog_version, run.tool_limits_version);
        let valid_tool_versions = versions == (0, 0)
            || (working_directory.is_some()
                && versions
                    == (
                        crate::tools::TOOL_CATALOG_VERSION,
                        crate::tools::TOOL_LIMITS_VERSION,
                    ))
            || (legacy_workspace_exists
                && matches!(
                    versions,
                    (
                        crate::tools::LEGACY_WORKTREE_TOOL_CATALOG_VERSION,
                        crate::tools::LEGACY_WORKTREE_TOOL_LIMITS_VERSION
                    ) | (
                        crate::tools::LEGACY_SANDBOX_TOOL_CATALOG_VERSION,
                        crate::tools::LEGACY_SANDBOX_TOOL_LIMITS_VERSION
                    )
                ));
        if !valid_tool_versions {
            return Err(PersistenceError::InvalidState {
                reason: "the run tool catalog conflicts with its session execution context",
            });
        }
        Ok(RunContext {
            run,
            skills,
            attachment_data,
            entries,
            current_entry_high_water,
            estimated_input_tokens,
            working_directory,
        })
    }
}

pub(crate) fn load_run_skills(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> Result<crate::skills::RunSkillContext, PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT skill_index, skill_name, description, skill_file,
                skill_source, active, instructions
         FROM run_skill_snapshots WHERE run_id = ?1 ORDER BY skill_index",
    )?;
    let rows = statement
        .query_map([&run_id.as_bytes()[..]], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                crate::skills::SkillSnapshot {
                    name: row.get(1)?,
                    description: row.get(2)?,
                    skill_file: row.get(3)?,
                    source: crate::skills::SkillSource::from_record(row.get(4)?).ok_or_else(
                        || {
                            rusqlite::Error::InvalidColumnType(
                                4,
                                "skill_source".to_owned(),
                                rusqlite::types::Type::Integer,
                            )
                        },
                    )?,
                    active: row.get(5)?,
                    instructions: row.get(6)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows
        .iter()
        .enumerate()
        .any(|(index, (stored_index, _))| usize::try_from(*stored_index).ok() != Some(index + 1))
    {
        return Err(PersistenceError::InvalidState {
            reason: "a run skill snapshot has invalid ordering",
        });
    }
    let skills = crate::skills::RunSkillContext {
        skills: rows.into_iter().map(|(_, skill)| skill).collect(),
    };
    if !skills.is_valid() {
        return Err(PersistenceError::InvalidState {
            reason: "a run skill snapshot is invalid",
        });
    }
    Ok(skills)
}
