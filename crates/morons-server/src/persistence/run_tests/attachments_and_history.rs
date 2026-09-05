use super::*;

#[tokio::test(flavor = "current_thread")]
async fn transcript_windows_reach_both_edges_beyond_the_old_client_limit() {
    let root = TestRoot::new("long-transcript-window");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x71; 16]), None)
        .await
        .expect("session should be created");
    for index in 0_u64..513 {
        let mut request_id = [0_u8; 16];
        request_id[8..].copy_from_slice(&index.saturating_add(1).to_be_bytes());
        let command = format!("printf MESSAGE-{index:03}");
        let accepted = store
            .accept_local_command(
                MutationRequestId::from_bytes(request_id),
                session.id,
                command,
                false,
            )
            .await
            .expect("bounded fixture command should be accepted");
        assert!(
            store
                .activate_local_command(accepted.id)
                .await
                .expect("fixture command should activate")
        );
        store
            .complete_local_command(
                accepted.id,
                ToolResult::Ok {
                    output: ToolOutput::Bash {
                        exit_code: Some(0),
                        signal: None,
                        stdout: format!("MESSAGE-{index:03}"),
                        stderr: String::new(),
                    },
                },
            )
            .await
            .expect("fixture command should complete");
    }

    let latest = store
        .list_session_transcript_window(session.id, None, TranscriptPageDirection::Older, 1)
        .await
        .expect("latest edge should load");
    assert!(latest.older_cursor.is_some());
    assert!(latest.newer_cursor.is_none());
    assert!(matches!(
        &latest.entries[..],
        [TranscriptEntry::LocalCommand { command, .. }] if command == "printf MESSAGE-512"
    ));

    let oldest = store
        .list_session_transcript_window(session.id, None, TranscriptPageDirection::Newer, 1)
        .await
        .expect("oldest edge should load");
    assert!(oldest.older_cursor.is_none());
    assert!(oldest.newer_cursor.is_some());
    assert!(matches!(
        &oldest.entries[..],
        [TranscriptEntry::LocalCommand { command, .. }] if command == "printf MESSAGE-000"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn deleting_an_archived_session_removes_file_backed_images_only_from_morons_state() {
    let root = TestRoot::new("delete-image-session");
    let selected = TestRoot::new("delete-image-selected");
    let sentinel = selected.path().join("sentinel");
    fs::write(&sentinel, "keep").expect("sentinel should be written");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x0e; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x44; 16]).expect("fixture image should normalize");
    let orphan_bytes = image.bytes.clone();
    let attachment = crate::persistence::PreparedImageAttachment {
        display_name: "picture.png".to_owned(),
        marker_start: 0,
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        digest: Sha256::digest(&image.bytes).into(),
        bytes: image.bytes,
    };
    let mut selection = model_selection();
    selection.supports_image_input = true;
    let accepted = store
        .accept_session_input_with_skills(
            MutationRequestId::from_bytes([0x0f; 16]),
            session.id,
            "[picture.png]".to_owned(),
            selection,
            crate::skills::RunSkillContext::default(),
            vec![attachment],
        )
        .await
        .expect("image-bearing run should be accepted");
    let attachment_session_directory = fs::read_dir(root.path().join("attachments"))
        .expect("attachment root should be readable")
        .next()
        .expect("attachment directory should exist")
        .expect("attachment directory should be readable")
        .path();
    let orphan = attachment_session_directory.join("11111111111111111111111111111111.image");
    fs::write(&orphan, orphan_bytes).expect("orphan fixture should be written");
    #[cfg(unix)]
    fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600))
        .expect("orphan fixture should be private");
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .expect("run should stop before archiving");
    store
        .set_session_archived(MutationRequestId::from_bytes([0x10; 16]), session.id, true)
        .await
        .expect("session should archive");
    store
        .delete_session(MutationRequestId::from_bytes([0x11; 16]), session.id)
        .await
        .expect("session should delete");
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment root should remain readable")
            .count(),
        0
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    drop(store);
    let reopened = SessionStore::open_at(root.path()).expect("deleted store should reopen");
    assert!(
        reopened
            .get_session(session.id)
            .await
            .expect("session query should succeed")
            .is_none()
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
}

#[tokio::test(flavor = "current_thread")]
async fn image_attachments_are_fingerprint_bound_file_backed_and_durable() {
    let root = TestRoot::new("run-image-context");
    let selected = TestRoot::new("run-image-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xc1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x44; 16]).expect("fixture image should normalize");
    let attachment = crate::persistence::PreparedImageAttachment {
        display_name: "picture.png".to_owned(),
        marker_start: 5,
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        digest: Sha256::digest(&image.bytes).into(),
        bytes: image.bytes,
    };
    let mut selection = model_selection();
    selection.supports_image_input = true;
    let request_id = MutationRequestId::from_bytes([0xc2; 16]);
    let accepted = store
        .accept_session_input_with_skills(
            request_id,
            session.id,
            "look [picture.png]".to_owned(),
            selection.clone(),
            crate::skills::RunSkillContext::default(),
            vec![attachment.clone()],
        )
        .await
        .expect("image-bearing run should be accepted");
    let retry = store
        .accept_session_input_with_skills(
            request_id,
            session.id,
            "look [picture.png]".to_owned(),
            selection,
            crate::skills::RunSkillContext::default(),
            vec![attachment.clone()],
        )
        .await
        .expect("exact image retry should resolve");
    assert_eq!(retry.run.id, accepted.run.id);
    assert!(!retry.newly_accepted);

    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("image transcript should load");
    let [TranscriptEntry::UserMessage { attachments, .. }] = &page.entries[..] else {
        panic!("user message should remain canonical");
    };
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].display_name, "picture.png");
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("image context should load");
    assert_eq!(context.attachment_data.len(), 1);
    assert_eq!(
        context
            .attachment_data
            .get(&attachments[0].id)
            .map(Vec::as_slice),
        Some(attachment.bytes.as_slice())
    );
    assert!(matches!(
        store
            .accept_session_input_with_skills(
                MutationRequestId::from_bytes([0xc3; 16]),
                session.id,
                "continue without vision".to_owned(),
                model_selection(),
                crate::skills::RunSkillContext::default(),
                Vec::new(),
            )
            .await,
        Err(PersistenceError::ImageInputUnsupported)
    ));
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment directory should be readable")
            .count(),
        1
    );
    drop(store);
    let attachment_session_directory = fs::read_dir(root.path().join("attachments"))
        .expect("attachment root should be readable")
        .next()
        .expect("attachment session directory should exist")
        .expect("attachment session entry should load")
        .path();
    let orphan = attachment_session_directory.join("11111111111111111111111111111111.image");
    fs::write(&orphan, &attachment.bytes).expect("orphan fixture should be written");
    #[cfg(unix)]
    fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600))
        .expect("orphan should be owner-only");
    let reopened =
        SessionStore::open_at(root.path()).expect("image attachment should survive reopen");
    assert!(!orphan.exists());
    drop(reopened);
    let durable_file = fs::read_dir(attachment_session_directory)
        .expect("attachment directory should remain readable")
        .next()
        .expect("durable attachment should exist")
        .expect("durable attachment entry should load")
        .path();
    #[cfg(unix)]
    {
        let directory_metadata = fs::symlink_metadata(
            durable_file
                .parent()
                .expect("durable attachment should have a parent"),
        )
        .expect("attachment session directory metadata should load");
        assert_eq!(directory_metadata.mode() & 0o777, 0o700);
        let metadata =
            fs::symlink_metadata(&durable_file).expect("durable attachment metadata should load");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    }
    fs::remove_file(durable_file).expect("durable attachment should be removed for corruption");
    let error = match SessionStore::open_at(root.path()) {
        Ok(_) => panic!("missing durable attachment should fail closed"),
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::Io(_)));
}
