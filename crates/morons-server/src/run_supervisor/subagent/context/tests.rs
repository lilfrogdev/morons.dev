use super::*;

fn message(text: &str) -> ProviderInputItem {
    ProviderInputItem::Message {
        role: ProviderMessageRole::User,
        text: text.into(),
        phase: None,
    }
}

fn batch(id: &str, bytes: usize) -> Vec<ProviderInputItem> {
    vec![
        ProviderInputItem::FunctionCall {
            call_id: id.into(),
            name: "read".into(),
            arguments: "{}".into(),
            opaque_continuation: None,
        },
        ProviderInputItem::FunctionCallOutput {
            call_id: id.into(),
            output: "x".repeat(bytes),
        },
    ]
}

#[test]
fn repeated_compaction_preserves_pinned_prefix_and_latest_complete_batch() {
    let mut input = vec![message("owner assignment"), message("pinned guidance")];
    let prefix = journal::items(&input);
    let mut context = Context::new(input.len());
    input.extend(batch("first", 12000));
    for index in 0..3 {
        let start = input.len();
        let recent = batch(&format!("recent_{index}"), 12000);
        input.extend(recent.clone());
        context.completed_batch(start);
        let plan = context.plan(&input, 30000).unwrap().unwrap();
        if index > 0 {
            assert!(
                journal::items(&plan.request)
                    .to_string()
                    .contains("verified finding")
            );
        }
        let before = estimate_provider_input(&input).unwrap();
        context
            .install(
                &mut input,
                &plan,
                "verified finding; earlier effects remain".into(),
            )
            .unwrap();
        assert_eq!(journal::items(&input[..2]), prefix);
        assert_eq!(journal::items(&input[3..]), journal::items(&recent));
        assert!(estimate_provider_input(&input).unwrap() < before);
        assert_eq!(context.latest_batch, Some(3));
    }
}

#[test]
fn summary_source_excludes_opaque_reasoning_but_retains_attributed_results() {
    let mut input = vec![message("pinned")];
    input.push(ProviderInputItem::Reasoning {
        id: "reasoning_id".into(),
        summaries: vec![],
        encrypted_content: Some("secret_ciphertext".into()),
    });
    let mut old = batch("old", 12000);
    if let ProviderInputItem::FunctionCall {
        opaque_continuation,
        ..
    } = &mut old[0]
    {
        *opaque_continuation = Some("opaque_signature".into());
    }
    input.extend(old);
    let start = input.len();
    input.extend(batch("recent", 12000));
    let mut context = Context::new(1);
    context.completed_batch(start);
    let plan = context.plan(&input, 30000).unwrap().unwrap();
    let source = journal::items(&plan.request).to_string();
    assert!(!source.contains("secret_ciphertext"));
    assert!(!source.contains("opaque_signature"));
    assert!(source.contains("old"));
    assert!(!source.contains("recent"));
}

#[test]
fn invalid_summaries_leave_input_and_cursor_unchanged() {
    let mut input = vec![message("pinned")];
    input.extend(batch("old", 12000));
    let start = input.len();
    input.extend(batch("recent", 12000));
    let mut context = Context::new(1);
    context.completed_batch(start);
    let plan = context.plan(&input, 30000).unwrap().unwrap();
    let before = journal::items(&input);
    for summary in [" ".into(), "x".repeat(SUMMARY_BYTES + 1)] {
        assert!(context.install(&mut input, &plan, summary).is_err());
        assert_eq!(journal::items(&input), before);
        assert_eq!(context.latest_batch, Some(start));
    }
}

#[test]
fn compaction_can_recover_from_a_jump_above_the_main_input_limit() {
    let limit = 20_000;
    let mut input = vec![message("pinned")];
    input.extend(batch("old", 12_000));
    let start = input.len();
    input.extend(batch("latest", 12_000));
    let mut context = Context::new(1);
    context.completed_batch(start);

    assert!(estimate_provider_input(&input).unwrap() > limit);
    let plan = context.plan(&input, limit).unwrap().unwrap();
    assert!(plan.estimate <= limit);
    context
        .install(&mut input, &plan, "verified finding".into())
        .unwrap();
    assert!(estimate_provider_input(&input).unwrap() <= limit);
}

#[test]
fn compaction_preserves_an_oversized_pinned_prefix_and_latest_batch() {
    let limit = 30_000;
    let mut input = vec![message(&"p".repeat(25_000))];
    let pinned = journal::items(&input);
    input.extend(batch("old", 12_000));
    let start = input.len();
    let latest = batch("latest", 25_000);
    input.extend(latest.clone());
    let mut context = Context::new(1);
    context.completed_batch(start);

    let plan = context.plan(&input, limit).unwrap().unwrap();
    context
        .install(&mut input, &plan, "verified finding".into())
        .unwrap();

    assert_eq!(journal::items(&input[..1]), pinned);
    assert_eq!(journal::items(&input[2..]), journal::items(&latest));
    assert!(estimate_provider_input(&input).unwrap() > limit);
}

#[test]
fn nonreducing_summary_leaves_input_and_cursor_unchanged() {
    let mut input = vec![message("pinned"), message("old"), message("latest")];
    let mut context = Context::new(1);
    context.completed_batch(2);
    let plan = Plan {
        cut: 2,
        request: vec![],
        estimate: 1,
    };
    let before = journal::items(&input);

    assert!(context.install(&mut input, &plan, "x".repeat(100)).is_err());
    assert_eq!(journal::items(&input), before);
    assert_eq!(context.latest_batch, Some(2));
}

#[test]
fn compaction_requires_complete_old_history_and_bounded_summary_request() {
    let mut input = vec![message("pinned")];
    input.extend(batch("only", 25000));
    let mut context = Context::new(1);
    context.completed_batch(1);
    assert!(context.plan(&input, 30000).unwrap().is_none());
    let start = input.len();
    input.extend(batch("recent", 12000));
    context.completed_batch(start);
    assert!(context.plan(&input, 10000).is_err());
}
