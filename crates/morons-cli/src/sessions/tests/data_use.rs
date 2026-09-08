use super::*;

#[tokio::test]
async fn inconsistent_or_out_of_range_data_use_status_is_rejected() {
    for sequence in [0, u64::MAX] {
        let (stream, mut server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            assert_eq!(
                read_request(&mut server, 1).await,
                ApplicationRequest::GetApplicationSettings
            );
            write_server_message(
                &mut server,
                &ServerMessage::response(
                    1,
                    ApplicationResponse::ApplicationSettings {
                        settings: ApplicationSettings {
                            subagent_model: SubagentModelSetting::InheritParent {},
                            data_use: morons_protocol::DataUsePolicy {
                                sequence,
                                block_training_use: true,
                                require_zero_retention: false,
                            },
                        },
                    },
                ),
            )
            .await
            .unwrap();
        });
        let mut client = ApplicationClient::from_negotiated_connection(stream);
        assert!(matches!(
            client.application_settings().await,
            Err(ApplicationClientError::EventScopeMismatch)
        ));
        server.await.unwrap();
    }
}
