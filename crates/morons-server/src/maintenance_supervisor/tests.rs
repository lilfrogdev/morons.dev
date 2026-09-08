use super::*;
use crate::{
    persistence::maintenance::MaintenanceState,
    run_supervisor::tests::maintenance::{eligible, wait_state},
};
use tokio::sync::oneshot;

#[tokio::test(flavor = "current_thread")]
async fn overlapping_cancellation_waits_for_owned_execution_to_drain() {
    for abandon_first in [false, true] {
        let (_root, _selected, store, session, trigger) = eligible(true).await;
        let work = store.prepare_maintenance(trigger).await.unwrap().unwrap();
        assert!(store.dispatch_maintenance(work.id).await.unwrap());
        let provider = ModelProviders::for_test(Arc::clone(&store), "http://127.0.0.1:1");
        let shutdown = watch::channel(false).0;
        let supervisor = MaintenanceSupervisor::new(Arc::clone(&store), provider, shutdown.clone());
        let permit = Arc::clone(&supervisor.slot).try_acquire_owned().unwrap();
        let (cancel, mut cancellation) = provider_cancellation();
        let (started, draining) = oneshot::channel();
        let (release, released) = oneshot::channel();
        *supervisor.task.lock().await = Some(Active {
            session,
            cancel,
            task: tokio::spawn(async move {
                let _permit = permit;
                cancellation.cancelled().await;
                started.send(()).unwrap();
                released.await.unwrap();
            }),
        });
        let first = {
            let supervisor = Arc::clone(&supervisor);
            tokio::spawn(async move { supervisor.cancel_session(session).await })
        };
        time::timeout(Duration::from_secs(5), draining)
            .await
            .unwrap()
            .unwrap();
        let first = if abandon_first {
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            assert!(
                supervisor.task.lock().await.is_some(),
                "abandoning cancellation detached owned execution"
            );
            None
        } else {
            Some(first)
        };
        let mut second = {
            let supervisor = Arc::clone(&supervisor);
            tokio::spawn(async move { supervisor.cancel_session(session).await })
        };
        let premature = time::timeout(Duration::from_millis(100), &mut second).await;
        // Release even on regression so the fixture never leaves an owned task behind.
        release.send(()).unwrap();
        if let Some(first) = first {
            first.await.unwrap().unwrap();
        }
        if premature.is_err() {
            second.await.unwrap().unwrap();
        }
        assert!(
            premature.is_err(),
            "another cancellation completed before owned execution drained"
        );
        wait_state(&store, session, MaintenanceState::Uncertain).await;
        assert!(!*shutdown.borrow());
        supervisor.shutdown().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_admission_skips_lifecycle_cleanup_without_queueing() {
    let (_root, _selected, store, session, trigger) = eligible(true).await;
    let provider = ModelProviders::for_test(Arc::clone(&store), "http://127.0.0.1:1");
    let supervisor =
        MaintenanceSupervisor::new(Arc::clone(&store), provider, watch::channel(false).0);
    let guard = supervisor.task.lock().await;
    let admission = time::timeout(
        Duration::from_millis(100),
        supervisor.maybe_start(session, trigger),
    )
    .await;
    drop(guard);
    assert!(
        admission.is_ok(),
        "maintenance queued behind lifecycle cleanup"
    );
    assert!(store.prepare_maintenance(trigger).await.unwrap().is_some());
    supervisor.shutdown().await;
}
