use super::*;
use crate::run_supervisor::build_compaction_request_for_run;
use std::time::Instant;

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual local maintenance timing probe; fake credentials, no network or timing assertions"]
async fn measure_background_maintenance() {
    for sample in 0..5 {
        let (root, _selected, store, session, trigger) = eligible(true).await;
        let began = Instant::now();
        let work = store.prepare_maintenance(trigger).await.unwrap().unwrap();
        let prepare = began.elapsed();
        let began = Instant::now();
        let request = build_compaction_request_for_run(work.id, &work.run, &work.plan).unwrap();
        let encode = began.elapsed();
        let request_bytes = request.encoded_body().len();
        let began = Instant::now();
        assert!(store.dispatch_maintenance(work.id).await.unwrap());
        store
            .complete_maintenance(work.id, summary())
            .await
            .unwrap();
        let ledger = began.elapsed();
        lower_advisory_usage(&root);
        let run = next_run(&store, session, "CURRENT_INPUT", 6).await;
        let before = store
            .session_context_status(session, hardening::selection())
            .await
            .unwrap();
        let began = Instant::now();
        store.maintenance_boundary(run).await.unwrap();
        let install = began.elapsed();
        let after = store
            .session_context_status(session, hardening::selection())
            .await
            .unwrap();
        assert_eq!(
            after.background_compaction.latest.unwrap().state,
            MaintenanceState::Installed
        );
        store.finish_run_stopped(run, None).await.unwrap();
        drop(store);
        let began = Instant::now();
        let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
        let reopen = began.elapsed();
        drop(store);
        eprintln!(
            "maintenance sample={sample} prepare_us={} encode_us={} ledger_us={} install_us={} reopen_us={} request_bytes={request_bytes} conservative_before={} conservative_after={}",
            prepare.as_micros(),
            encode.as_micros(),
            ledger.as_micros(),
            install.as_micros(),
            reopen.as_micros(),
            before.conservative_input_tokens,
            after.conservative_input_tokens
        );
    }
}
