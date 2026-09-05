use super::*;

#[tokio::test(flavor = "current_thread")]
async fn explicit_and_used_models_determine_the_durable_global_default() {
    let root = TestRoot::new("default-model");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    assert!(
        store
            .default_model()
            .await
            .expect("empty default should be readable")
            .is_none()
    );

    let request_id = MutationRequestId::from_bytes([0x40; 16]);
    let explicit = DefaultModelSelection {
        service: RunOpenCodeService::Go,
        model_id: "grok-4.6".to_owned(),
    };
    assert_eq!(
        store
            .set_default_model(request_id, explicit.clone())
            .await
            .expect("default should be selected"),
        explicit
    );
    assert_eq!(
        store
            .set_default_model(request_id, explicit.clone())
            .await
            .expect("an exact selection retry should resolve"),
        explicit
    );
    let conflict = store
        .set_default_model(
            request_id,
            DefaultModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: TEST_MODEL.to_owned(),
            },
        )
        .await
        .expect_err("a conflicting selection retry should fail");
    assert!(matches!(conflict, PersistenceError::RequestConflict));
    assert_eq!(
        store
            .default_model()
            .await
            .expect("selected default should be readable"),
        Some(explicit)
    );

    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x41; 16]), None)
        .await
        .expect("session should be created");
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0x42; 16]),
            session.id,
            "use the newer model".to_owned(),
            model_selection(),
        )
        .await
        .expect("run should be accepted");
    let used = DefaultModelSelection {
        service: RunOpenCodeService::Zen,
        model_id: TEST_MODEL.to_owned(),
    };
    assert_eq!(
        store
            .default_model()
            .await
            .expect("last used model should be readable"),
        Some(used.clone())
    );

    drop(store);
    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    assert_eq!(
        reopened
            .default_model()
            .await
            .expect("default should survive restart"),
        Some(used)
    );
    drop(reopened);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE default_model_selections SET operation_fingerprint = zeroblob(32)",
            [],
        )
        .expect("default fingerprint should be corruptible for test");
    drop(connection);
    let corrupted = SessionStore::open_at(root.path());
    assert!(matches!(
        corrupted,
        Err(PersistenceError::InvalidState { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_model_setting_is_global_idempotent_and_durable() {
    let root = TestRoot::new("subagent-model-setting");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    assert_eq!(
        store
            .subagent_model_setting()
            .await
            .expect("initial setting should load"),
        SubagentModelSetting::InheritParent {}
    );

    let request_id = MutationRequestId::from_bytes([0x43; 16]);
    let selected = SubagentModelSetting::OpenCode {
        service: RunOpenCodeService::Go,
        model_id: "glm-5.3-flash".to_owned(),
    };
    for _ in 0..2 {
        assert_eq!(
            store
                .set_subagent_model_setting(request_id, selected.clone())
                .await
                .expect("setting should be selected idempotently"),
            selected
        );
    }
    assert!(matches!(
        store
            .set_subagent_model_setting(request_id, SubagentModelSetting::InheritParent {})
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert_eq!(
        store
            .subagent_model_setting()
            .await
            .expect("selected setting should load"),
        selected
    );

    let inherit = SubagentModelSetting::InheritParent {};
    store
        .set_subagent_model_setting(MutationRequestId::from_bytes([0x44; 16]), inherit.clone())
        .await
        .expect("inherit setting should be restored");
    drop(store);
    let reopened = SessionStore::open_at(root.path()).expect("setting should survive restart");
    assert_eq!(
        reopened
            .subagent_model_setting()
            .await
            .expect("reopened setting should load"),
        inherit
    );
    drop(reopened);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE subagent_model_selections SET operation_fingerprint = zeroblob(32)",
            [],
        )
        .expect("setting fingerprint should be corruptible for test");
    drop(connection);
    assert!(matches!(
        SessionStore::open_at(root.path()),
        Err(PersistenceError::InvalidState { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_run_binds_active_skill_instructions_and_catalog_metadata() {
    let root = TestRoot::new("run-skill-context");
    let selected = TestRoot::new("run-skill-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xb1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let skills = crate::skills::RunSkillContext {
        skills: vec![crate::skills::SkillSnapshot {
            name: "release-helper".to_owned(),
            description: "Helps prepare a release when explicitly requested.".to_owned(),
            skill_file: selected
                .path()
                .join(".agents/skills/release-helper/SKILL.md")
                .to_string_lossy()
                .into_owned(),
            source: crate::skills::SkillSource::Project,
            active: true,
            instructions: Some(
                "---\nname: release-helper\ndescription: Helps prepare a release when explicitly requested.\n---\nRun checks first.\n"
                    .to_owned(),
            ),
        }],
    };
    let accepted = store
        .accept_session_input_with_skills(
            MutationRequestId::from_bytes([0xb2; 16]),
            session.id,
            "@release-helper prepare this release".to_owned(),
            model_selection(),
            skills.clone(),
            Vec::new(),
        )
        .await
        .expect("skill-bearing run should be accepted");
    assert_eq!(
        accepted.run.context_policy_version,
        crate::persistence::run_types::CONTEXT_POLICY_VERSION
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("skill-bearing context should load");
    assert_eq!(context.skills, skills);
    assert!(
        context
            .skills
            .developer_text()
            .is_some_and(|text| text.contains("Run checks first."))
    );
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    assert_eq!(
        reopened
            .load_run_context(accepted.run.id)
            .await
            .expect("durable skill context should reload")
            .skills,
        skills
    );
    drop(reopened);
    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE run_skill_snapshots SET skill_name = 'INVALID' WHERE run_id = ?1",
            [&accepted.run.id.as_bytes()[..]],
        )
        .expect("skill snapshot should be corrupted");
    drop(connection);
    let error = match SessionStore::open_at(root.path()) {
        Ok(_) => panic!("invalid skill snapshot should fail closed"),
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}
