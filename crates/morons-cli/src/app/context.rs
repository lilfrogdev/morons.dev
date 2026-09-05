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
    SafeText::from_untrusted(&format!(
        "Model: {} / {}\nEstimate: ~{} / {} tokens ({source})\nConservative guard: {} / {} (hard limits unchanged)\nAuto threshold: {} · output reserve: {}\nEntry and image limits apply independently.\nCheckpoint: {checkpoint}\n\n{call}\nCompleted compactions: {} · last elapsed {duration}\nRoot usage excludes compaction, subagents and failed calls.\nThese observations are not a complete bill.\n\nEnter/Esc close",
        super::service_label(context.service),
        context.model_id,
        context.estimated_input_tokens,
        context.maximum_input_tokens,
        context.conservative_input_tokens,
        context.maximum_input_tokens,
        context.compaction_threshold_tokens,
        context.maximum_output_tokens,
        context.completed_compactions,
    ))
}
