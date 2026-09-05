use super::*;

#[tokio::test(flavor = "current_thread")]
async fn image_submission_requires_vision_and_maps_durable_bytes_to_multimodal_content() {
    let root = TestRoot::new("image-context");
    let selected = TestRoot::new("image-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xc1; 16]),
            0,
            b"not-a-real-image-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xc2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x88; 16]).expect("fixture image should normalize");
    let upload = morons_protocol::ImageUpload {
        display_name: "puppies.png".to_owned(),
        marker_start: 4,
        data_base64: morons_image::encode_base64(&image.bytes),
    };
    let (base, captured_request, provider_task) = spawn_image_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());

    let unsupported = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xc3; 16]),
            session_id,
            text: "see [puppies.png]".to_owned(),
            attachments: vec![upload.clone()],
            service: OpenCodeService::Zen,
            model_id: "gpt-5.3-codex-spark".to_owned(),
        })
        .await;
    assert!(matches!(
        unsupported,
        Err(ApplicationError::UnsupportedModel)
    ));
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment directory should be readable")
            .count(),
        0
    );

    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xc4; 16]),
            session_id,
            text: "see [puppies.png]".to_owned(),
            attachments: vec![upload],
            service: OpenCodeService::Zen,
            model_id: "gpt-5.4".to_owned(),
        })
        .await
        .expect("vision run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("image provider should finish");
    let request = captured_request
        .await
        .expect("image request should be captured");
    assert!(request.contains("\"type\":\"input_text\""));
    assert!(request.contains("\"type\":\"input_image\""));
    assert!(request.contains("data:image/png;base64,"));
    assert!(request.contains("[puppies.png]"));

    let page = application
        .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
            session_id,
            cursor: None,
            direction: morons_protocol::TranscriptPageDirection::Newer,
            limit: 1,
        })
        .await
        .expect("image transcript should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
        entries, ..
    }) = page
    else {
        panic!("transcript should return a page");
    };
    assert!(matches!(
        &entries[..],
        [morons_protocol::TranscriptEntry::UserMessage { attachments, .. }]
            if attachments.len() == 1 && attachments[0].display_name == "puppies.png"
    ));
    application.shutdown().await;
    drop(application);
    let database =
        fs::read(root.path().join("data/sessions.sqlite3")).expect("database should be readable");
    assert!(!contains_bytes(&database, &image.bytes));
    assert!(!contains_bytes(
        &database,
        morons_image::encode_base64(&image.bytes).as_bytes()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn exact_skill_invocation_binds_full_instructions_while_catalog_stays_progressive() {
    let root = TestRoot::new("skill-context");
    let selected = TestRoot::new("skill-directory");
    write_test_skill(
        selected.path(),
        "release-helper",
        "Prepares releases when the user asks for release work.",
        "ACTIVE_RELEASE_INSTRUCTIONS",
    );
    write_test_skill(
        selected.path(),
        "inactive-helper",
        "Handles inactive test work.",
        "INACTIVE_PRIVATE_INSTRUCTIONS",
    );
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xb1; 16]),
            0,
            b"not-a-real-skill-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xb2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (base, captured_request, complete_provider, provider_task) =
        spawn_successful_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let protocol_session_id = SessionId::from_bytes(*session.id.as_bytes());
    let catalog = application
        .execute_for_local_owner(ApplicationRequest::ListSessionSkills {
            session_id: protocol_session_id,
        })
        .await
        .expect("skill catalog should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionSkillsListed {
        skills,
        warnings,
        ..
    }) = catalog
    else {
        panic!("skill catalog should return a response");
    };
    assert!(warnings.is_empty());
    assert_eq!(
        skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["inactive-helper", "release-helper", "skill-creator"]
    );
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xb3; 16]),
            session_id: protocol_session_id,
            text: "@release-helper prepare a release".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("skill-bearing input should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    let request = time::timeout(Duration::from_secs(5), captured_request)
        .await
        .expect("provider request should dispatch")
        .expect("provider request should be captured");
    assert!(request.contains("Prepares releases when the user asks for release work."));
    assert!(request.contains("Handles inactive test work."));
    assert!(request.contains("ACTIVE_RELEASE_INSTRUCTIONS"));
    assert!(!request.contains("INACTIVE_PRIVATE_INSTRUCTIONS"));
    complete_provider
        .send(())
        .unwrap_or_else(|_| panic!("provider completion should be released"));
    provider_task.await.expect("provider fixture should finish");
    assert_eq!(
        wait_for_terminal(&application, run.session_id, run.id).await,
        RunState::Succeeded
    );
    application.shutdown().await;
}
