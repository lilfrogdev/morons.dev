use crate::terminal::SafeText;
use morons_protocol::SessionContextStatus;

pub(super) fn description(context: Option<&SessionContextStatus>) -> SafeText {
    let Some(context) = context else {
        return SafeText::from_untrusted("No context observation available.\n\nEnter/Esc close");
    };
    let source = if context.estimate_uses_provider_usage {
        "provider usage + bounded tail"
    } else {
        "conservative byte estimate"
    };
    let checkpoint = context.checkpoint_source_entry_high_water.map_or_else(
        || "none".to_owned(),
        |entry| {
            format!(
                "through entry {entry}, ~{} summary tokens",
                context.checkpoint_estimated_summary_tokens.unwrap_or(0)
            )
        },
    );
    let call = context.latest_provider_usage.as_ref().map_or_else(|| "No matching successful root-call observation.".to_owned(), |usage| {
        let elapsed = usage.elapsed_milliseconds.map_or_else(|| "unavailable".to_owned(), |ms| format!("{ms} ms"));
        format!("Last matching successful root call:\nInput {} · cached {} · cache writes {}\nOutput {} · elapsed {elapsed}",
            usage.input_tokens, usage.cached_input_tokens, usage.cache_write_input_tokens, usage.output_tokens)
    });
    let duration = context
        .last_compaction_milliseconds
        .map_or_else(|| "unavailable".to_owned(), |ms| format!("{ms} ms"));
    let mut project = String::from("Project guidance (last accepted run):");
    match &context.project_context {
        None => project.push_str(" not captured"),
        Some(context) if !context.enabled => project.push_str(" disabled by server owner"),
        Some(context) => {
            if context.files.is_empty() {
                project.push_str(" no files loaded");
            }
            for path in &context.files {
                project.push_str(&format!("\nLoaded: {path}"));
            }
            for warning in &context.warnings {
                project.push_str(&format!("\nWarning: {warning}"));
            }
        }
    }
    let background = &context.background_compaction;
    let enabled = if background.enabled {
        "enabled (additional billable inference)"
    } else {
        "disabled"
    };
    let latest = background.latest.as_ref().map_or_else(
        || "No maintenance attempt.".to_owned(),
        |job| {
            let usage = job.usage.as_ref().map_or_else(
                || "No completed usage; a dispatched request may still incur cost.".to_owned(),
                |usage| {
                    format!(
                        "Input {} · cached {} · cache writes {} · output {} · elapsed {} ms",
                        usage.input_tokens,
                        usage.cached_input_tokens,
                        usage.cache_write_input_tokens,
                        usage.output_tokens,
                        usage.elapsed_milliseconds.unwrap_or(0)
                    )
                },
            );
            format!(
                "Last maintenance: {:?} · {} / {} · source through {}\n{usage}",
                job.state,
                super::service_label(job.service),
                job.model_id,
                job.source_entry_high_water
            )
        },
    );
    let policy = if context.usage_admission {
        "native usage policy 1; fallback when unavailable"
    } else {
        "legacy conservative policy"
    };
    let source_limit = context.maximum_source_bytes.map_or_else(
        || "legacy envelope".to_owned(),
        |value| format!("{value} byte cap"),
    );
    SafeText::from_untrusted(&format!(
        "Model: {} / {}\nEstimate: ~{} / {} tokens ({source})\nLegacy byte-heavy estimate: {} / {}\nAdmission: ~{} ({policy})\nSource: {} bytes / {source_limit}\nAuto threshold: {} · output reserve: {}\nEntry and image limits apply independently.\nCheckpoint: {checkpoint}\n\n{call}\nCompleted foreground compactions: {} · latest checkpoint foreground elapsed {duration}\nRoot usage excludes compaction, subagents and failed calls.\nThese observations are not a complete bill.\n\nBackground compaction: {enabled}\n{latest}\nUnused summaries can still cost quota/money. Status refreshes when this view is reopened.\n\n{project}\n\nUp/Down/PageUp/PageDown/Home/End scroll · Enter/Esc close",
        super::service_label(context.service),
        context.model_id,
        context.estimated_input_tokens,
        context.maximum_input_tokens,
        context.conservative_input_tokens,
        context.maximum_input_tokens,
        context.admission_input_tokens,
        context.source_bytes,
        context.compaction_threshold_tokens,
        context.maximum_output_tokens,
        context.completed_compactions,
    ))
}
