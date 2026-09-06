use super::*;
#[tokio::test(flavor = "current_thread")]
async fn lost_installation_acknowledgement_is_uncertain_and_closes_future_admission() {
    let (_root, store, mut supervisor) = setup();
    Arc::get_mut(&mut supervisor).unwrap().installation_timeout = Duration::from_millis(100);
    store
        .set_openai_credential(
            crate::persistence::MutationRequestId::from_bytes([1; 16]),
            0,
            tokens(),
        )
        .await
        .unwrap();
    let lease = store.begin_openai_access(1).await.unwrap();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut connection = start(&supervisor, &provider, 1).await;
    let (address, state) = callback(connection.url.as_ref().unwrap());
    let _ = tokio::join!(exchange(&provider), submit(address, state, "fixture-code"));
    assert_eq!(
        connection.finish().await,
        Outcome::Failed {
            failure: Failure::InstallationUncertain
        }
    );
    assert!(*supervisor.shutdown.borrow());
    assert!(matches!(
        supervisor.start_with(id(), 1, std::future::pending()).await,
        Err(ApplicationError::ServiceUnavailable)
    ));
    drop(lease);
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        1
    );
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn conflicting_mutation_identity_is_not_reported_as_an_installed_login() {
    let (root, store, supervisor) = setup();
    store
        .set_open_code_credential(
            crate::persistence::MutationRequestId::from_bytes(*id().as_bytes()),
            0,
            b"opencode-fixture".to_vec(),
        )
        .await
        .unwrap();
    let before = fs::read(root.path().join("credentials/opencode.state")).unwrap();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut connection = start(&supervisor, &provider, 0).await;
    let (address, state) = callback(connection.url.as_ref().unwrap());
    let _ = tokio::join!(exchange(&provider), submit(address, state, "fixture-code"));
    assert_eq!(
        connection.finish().await,
        Outcome::Failed {
            failure: Failure::Unavailable
        }
    );
    assert!(!*supervisor.shutdown.borrow());
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        0
    );
    assert_eq!(
        fs::read(root.path().join("credentials/opencode.state")).unwrap(),
        before
    );
}
