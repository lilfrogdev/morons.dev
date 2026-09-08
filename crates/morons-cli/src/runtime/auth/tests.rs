use super::*;
use morons_protocol::{OpenAiAuthorizationUrl, read_client_message, write_server_message};
#[tokio::test(flavor = "current_thread")]
async fn authentication_connection_cancels_exact_attempt_without_reconnecting() {
    let (mut client, mut server) = tokio::io::duplex(8192);
    let (events, mut receiver) = mpsc::channel(4);
    let (cancel, cancellation) = watch::channel(false);
    let task = tokio::spawn(async move {
        exchange(&mut client, Command::Begin(5), &events, cancellation).await
    });
    let Some(ClientMessage::Request {
        request_id: 1,
        request:
            Request::BeginOpenAiLogin {
                mutation_request_id,
                expected_generation: 5,
            },
    }) = read_client_message(&mut server).await.unwrap()
    else {
        panic!("scoped begin expected")
    };
    write_server_message(
        &mut server,
        &ServerMessage::response(
            1,
            Response::OpenAiLoginStarted {
                attempt_id: mutation_request_id,
                url: OpenAiAuthorizationUrl::new(
                    "https://auth.openai.com/oauth/authorize?state=fixture".into(),
                )
                .unwrap(),
            },
        ),
    )
    .await
    .unwrap();
    assert!(matches!(receiver.recv().await, Some(AuthEvent::Started(_))));
    cancel.send_replace(true);
    assert_eq!(
        read_client_message(&mut server).await.unwrap(),
        Some(ClientMessage::request(
            2,
            Request::CancelOpenAiLogin {
                attempt_id: mutation_request_id
            }
        ))
    );
    write_server_message(
        &mut server,
        &ServerMessage::OpenAiLoginFinished {
            attempt_id: mutation_request_id,
            outcome: OpenAiLoginResult::CancelledBeforeInstallation,
        },
    )
    .await
    .unwrap();
    assert!(task.await.unwrap().is_ok());
    assert!(matches!(
        receiver.recv().await,
        Some(AuthEvent::Finished(
            OpenAiLoginResult::CancelledBeforeInstallation
        ))
    ));
    assert!(read_client_message(&mut server).await.unwrap().is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn malformed_or_lost_auth_outcomes_are_not_retried() {
    for failure in ["scope", "generation", "disconnect"] {
        let (mut client, mut server) = tokio::io::duplex(8192);
        let (events, _receiver) = mpsc::channel(4);
        let (_cancel, cancellation) = watch::channel(false);
        let task = tokio::spawn(async move {
            exchange(&mut client, Command::Begin(0), &events, cancellation).await
        });
        let Some(ClientMessage::Request {
            request:
                Request::BeginOpenAiLogin {
                    mutation_request_id,
                    ..
                },
            ..
        }) = read_client_message(&mut server).await.unwrap()
        else {
            panic!("begin expected")
        };
        if failure == "disconnect" {
            drop(server);
        } else {
            write_server_message(
                &mut server,
                &ServerMessage::response(
                    1,
                    Response::OpenAiLoginStarted {
                        attempt_id: mutation_request_id,
                        url: OpenAiAuthorizationUrl::new(
                            "https://auth.openai.com/oauth/authorize?state=fixture".into(),
                        )
                        .unwrap(),
                    },
                ),
            )
            .await
            .unwrap();
            write_server_message(
                &mut server,
                &ServerMessage::OpenAiLoginFinished {
                    attempt_id: if failure == "scope" {
                        MutationRequestId::from_bytes([9; 16])
                    } else {
                        mutation_request_id
                    },
                    outcome: OpenAiLoginResult::Installed { generation: 99 },
                },
            )
            .await
            .unwrap();
        }
        assert!(task.await.unwrap().is_err());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn cancellation_before_start_reply_closes_instead_of_replaying_begin() {
    let (mut client, mut server) = tokio::io::duplex(8192);
    let (events, _) = mpsc::channel(4);
    let (cancel, cancellation) = watch::channel(false);
    let task = tokio::spawn(async move {
        exchange(&mut client, Command::Begin(0), &events, cancellation).await
    });
    assert!(read_client_message(&mut server).await.unwrap().is_some());
    cancel.send_replace(true);
    assert!(task.await.unwrap().is_err());
    assert!(read_client_message(&mut server).await.unwrap().is_none());
}
