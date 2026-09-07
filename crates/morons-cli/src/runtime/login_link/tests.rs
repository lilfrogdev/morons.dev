use super::*;

fn url() -> OpenAiAuthorizationUrl {
    OpenAiAuthorizationUrl::new(
        "https://auth.openai.com/oauth/authorize?state=synthetic-link&scope=openid%20email".into(),
    )
    .unwrap()
}
fn fixture(kind: &str) -> Command {
    #[cfg(unix)]
    {
        let script = match kind {
            "ok" => "exit 0",
            "reject" => "exit 1",
            "hold" => "exec sleep 30",
            "copy" => "printf '\\001'; cat >/dev/null",
            "bad_ack" => "printf x; cat >/dev/null",
            _ => panic!("unknown fixture"),
        };
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    #[cfg(windows)]
    {
        let script = match kind {
            "ok" => "exit 0",
            "reject" => "exit 1",
            "hold" => "Start-Sleep -Seconds 30",
            "copy" => {
                "[Console]::OpenStandardOutput().WriteByte(1); [Console]::OpenStandardOutput().Flush(); [Console]::OpenStandardInput().CopyTo([System.IO.Stream]::Null)"
            }
            "bad_ack" => {
                "[Console]::Out.Write('x'); [Console]::Out.Flush(); [Console]::OpenStandardInput().CopyTo([System.IO.Stream]::Null)"
            }
            _ => panic!("unknown fixture"),
        };
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        command
    }
}
#[tokio::test(flavor = "current_thread")]
async fn browser_launch_is_nonqueued_scoped_and_cancellable_without_retry() {
    let mut runtime = LinkRuntime::default();
    let scope = Arc::new(());
    runtime
        .start_with(
            LinkAction::Open,
            Arc::clone(&scope),
            url(),
            Ok(fixture("hold")),
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
            Ok(fixture("ok")),
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
    let mut runtime = LinkRuntime::default();
    let scope = Arc::new(());
    runtime
        .start_with(
            LinkAction::Copy,
            Arc::clone(&scope),
            url(),
            Ok(fixture("copy")),
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
            Ok(fixture("copy")),
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
    for (action, kind) in [
        (LinkAction::Open, "reject"),
        (LinkAction::Open, "hold"),
        (LinkAction::Copy, "bad_ack"),
    ] {
        let mut runtime = LinkRuntime::default();
        runtime
            .start_with(action, Arc::new(()), url(), Ok(fixture(kind)))
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
            fixture("ok"),
            LinkAction::Open,
            &url(),
            cancellation,
            || panic!("cancelled work cannot publish")
        )
        .await
        .is_ok()
    );
}
