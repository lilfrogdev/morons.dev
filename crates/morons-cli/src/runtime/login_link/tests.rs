use super::*;
mod fixture;
use fixture::Helper;

#[test]
fn selection_failure_codes_produce_only_fixed_stage_messages() {
    assert_eq!(copy_acknowledgment(READY), Ok(()));
    for (code, expected) in [
        (
            crate::login_link::INVALID_SELECTION,
            "Copy failed: helper rejected selection",
        ),
        (
            crate::login_link::CLIPBOARD_CONNECT_FAILED,
            "Copy failed: clipboard connection failed",
        ),
        (
            crate::login_link::CLIPBOARD_WRITE_FAILED,
            "Copy uncertain: clipboard write failed",
        ),
        (255, "Copy uncertain: invalid helper acknowledgment"),
    ] {
        assert_eq!(copy_acknowledgment(code), Err(expected));
    }
}

#[tokio::test]
async fn selection_reports_startup_stages_without_publishing_success() {
    let fixture = Helper::new().await;
    for (kind, expected) in [
        ("bad_ack", "Copy uncertain: invalid helper acknowledgment"),
        ("hold", "Copy uncertain: helper startup timed out"),
    ] {
        let (_cancel, cancellation) = watch::channel(false);
        assert_eq!(
            run_text(
                fixture.command(kind),
                LinkAction::Copy,
                "synthetic selection",
                cancellation,
                || panic!("failure cannot publish success")
            )
            .await,
            Err(expected)
        );
    }
}

#[tokio::test]
async fn clipboard_wait_keeps_ownership_pipe_open_until_cancelled() {
    let fixture = Helper::new().await;
    let (cancel, cancellation) = watch::channel(false);
    let (ready, acknowledged) = tokio::sync::oneshot::channel();
    let command = fixture.command("copy");
    let mut task = tokio::spawn(async move {
        run_text(
            command,
            LinkAction::Copy,
            "synthetic selection",
            cancellation,
            || {
                let _ = ready.send(());
            },
        )
        .await
    });
    time::timeout(Duration::from_secs(10), acknowledged)
        .await
        .unwrap()
        .unwrap();
    let early_exit = time::timeout(Duration::from_millis(250), &mut task).await;
    cancel.send_replace(true);
    assert!(
        early_exit.is_err(),
        "helper exited while its ownership pipe should be open: {early_exit:?}"
    );
    assert_eq!(
        time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}

fn url() -> OpenAiAuthorizationUrl {
    OpenAiAuthorizationUrl::new(
        "https://auth.openai.com/oauth/authorize?state=synthetic-link&scope=openid%20email".into(),
    )
    .unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn browser_launch_is_nonqueued_scoped_and_cancellable_without_retry() {
    let fixture = Helper::new().await;
    let mut runtime = LinkRuntime::default();
    let scope = Arc::new(());
    runtime
        .start_with(
            LinkAction::Open,
            Arc::clone(&scope),
            url(),
            Ok(fixture.command("hold")),
        )
        .await;
    runtime
        .start_with(LinkAction::Open, Arc::clone(&scope), url(), Err(()))
        .await;
    assert!(runtime.events.try_recv().is_err());
    let other = Arc::new(());
    runtime.reconcile(Some(&other));
    runtime.shutdown().await;
    assert!(runtime.events.try_recv().is_err());
    assert!(runtime.browser.is_none());
    runtime
        .start_with(
            LinkAction::Open,
            Arc::clone(&other),
            url(),
            Ok(fixture.command("ok")),
        )
        .await;
    let event = time::timeout(Duration::from_secs(10), runtime.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&other, &event.scope));
    assert_eq!(event.message, success(LinkAction::Open));
    assert!(!event.message.contains("synthetic-link"));
    runtime.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn clipboard_ownership_is_held_until_scope_cancellation_then_drained() {
    let fixture = Helper::new().await;
    let mut runtime = LinkRuntime::default();
    let scope = Arc::new(());
    runtime
        .start_with(
            LinkAction::Copy,
            Arc::clone(&scope),
            url(),
            Ok(fixture.command("copy")),
        )
        .await;
    let event = time::timeout(Duration::from_secs(10), runtime.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&scope, &event.scope));
    assert_eq!(event.message, success(LinkAction::Copy));
    assert!(!runtime.clipboard.as_ref().unwrap().task.is_finished());
    // A repeated explicit copy replaces only its previous owned helper.
    runtime
        .start_with(
            LinkAction::Copy,
            Arc::clone(&scope),
            url(),
            Ok(fixture.command("copy")),
        )
        .await;
    assert_eq!(
        time::timeout(Duration::from_secs(10), runtime.events.recv())
            .await
            .unwrap()
            .unwrap()
            .message,
        success(LinkAction::Copy)
    );
    runtime.reconcile(None);
    runtime.shutdown().await;
    assert!(runtime.clipboard.is_none());
    assert!(runtime.events.try_recv().is_err());
}
#[tokio::test(flavor = "current_thread")]
async fn launcher_failures_timeouts_and_malformed_copy_acknowledgements_are_bounded() {
    let fixture = Helper::new().await;
    for (action, kind) in [
        (LinkAction::Open, "reject"),
        (LinkAction::Open, "hold"),
        (LinkAction::Copy, "bad_ack"),
    ] {
        let mut runtime = LinkRuntime::default();
        runtime
            .start_with(action, Arc::new(()), url(), Ok(fixture.command(kind)))
            .await;
        let event = time::timeout(Duration::from_secs(12), runtime.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.message, failure(action));
        assert!(!event.message.contains("synthetic-link"));
        runtime.shutdown().await;
        assert!(runtime.events.try_recv().is_err());
    }
    let (_cancel, cancellation) = watch::channel(true);
    assert!(
        run(
            fixture.command("ok"),
            LinkAction::Open,
            &url(),
            cancellation,
            || panic!("cancelled work cannot publish")
        )
        .await
        .is_ok()
    );
}
