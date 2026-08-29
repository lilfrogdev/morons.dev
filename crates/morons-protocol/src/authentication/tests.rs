use tokio::io::AsyncWriteExt;

use super::{
    AUTHENTICATION_KEY_BYTES, AUTHENTICATION_NONCE_BYTES, AUTHENTICATION_PROOF_BYTES,
    AuthenticationError, AuthenticationKey, AuthenticationNonce, AuthenticationRecord,
    AuthenticationRecordError, CLIENT_PROOF_BYTES, HOST_EPOCH_BYTES, HostEpoch,
    MAX_AUTHENTICATION_FRAME_PAYLOAD_BYTES, SERVER_CHALLENGE_BYTES, SERVER_PROOF_BYTES,
    authenticate_client, authenticate_server, create_client_proof, create_server_proof,
    read_authentication_record, verify_client_proof, verify_server_proof,
    write_authentication_record,
};
use crate::{FrameError, framing::FRAME_HEADER_BYTES};

#[test]
fn proofs_match_known_hmac_sha256_vectors() {
    let key = AuthenticationKey::from_bytes(sequential_bytes::<AUTHENTICATION_KEY_BYTES>(0));
    let host_epoch = HostEpoch::from_bytes(sequential_bytes::<HOST_EPOCH_BYTES>(0xa0));
    let server_nonce =
        AuthenticationNonce::from_bytes(sequential_bytes::<AUTHENTICATION_NONCE_BYTES>(0x20));
    let client_nonce =
        AuthenticationNonce::from_bytes(sequential_bytes::<AUTHENTICATION_NONCE_BYTES>(0x40));

    let client_proof = create_client_proof(&key, &host_epoch, &server_nonce, &client_nonce);
    let server_proof = create_server_proof(&key, &host_epoch, &server_nonce, &client_nonce);

    assert_eq!(
        client_proof.0,
        decode_hex("0bb8d968b8150bc2179cb3d9be8e6a1b0f5266a6e19b5f92d3d99c8bd0448ed0")
    );
    assert_eq!(
        server_proof.0,
        decode_hex("37b9603c229d4f3a49476558732230317366cf52f7741456be4b48d1fdb80088")
    );
}

#[test]
fn proof_is_bound_to_role_key_epoch_and_nonces() {
    let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let wrong_key = AuthenticationKey::from_bytes([0x12; AUTHENTICATION_KEY_BYTES]);
    let host_epoch = HostEpoch::from_bytes([0x21; HOST_EPOCH_BYTES]);
    let wrong_host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);
    let server_nonce = AuthenticationNonce::from_bytes([0x31; AUTHENTICATION_NONCE_BYTES]);
    let wrong_server_nonce = AuthenticationNonce::from_bytes([0x32; AUTHENTICATION_NONCE_BYTES]);
    let client_nonce = AuthenticationNonce::from_bytes([0x41; AUTHENTICATION_NONCE_BYTES]);
    let wrong_client_nonce = AuthenticationNonce::from_bytes([0x42; AUTHENTICATION_NONCE_BYTES]);
    let proof = create_client_proof(&key, &host_epoch, &server_nonce, &client_nonce);

    assert!(verify_client_proof(
        &key,
        &host_epoch,
        &server_nonce,
        &client_nonce,
        &proof,
    ));
    assert!(!verify_server_proof(
        &key,
        &host_epoch,
        &server_nonce,
        &client_nonce,
        &proof,
    ));
    assert!(!verify_client_proof(
        &wrong_key,
        &host_epoch,
        &server_nonce,
        &client_nonce,
        &proof,
    ));
    assert!(!verify_client_proof(
        &key,
        &wrong_host_epoch,
        &server_nonce,
        &client_nonce,
        &proof,
    ));
    assert!(!verify_client_proof(
        &key,
        &host_epoch,
        &wrong_server_nonce,
        &client_nonce,
        &proof,
    ));
    assert!(!verify_client_proof(
        &key,
        &host_epoch,
        &server_nonce,
        &wrong_client_nonce,
        &proof,
    ));
}

#[test]
fn authentication_records_have_stable_binary_shapes() {
    let host_epoch = HostEpoch::from_bytes([0x11; HOST_EPOCH_BYTES]);
    let server_nonce = AuthenticationNonce::from_bytes([0x22; AUTHENTICATION_NONCE_BYTES]);
    let client_nonce = AuthenticationNonce::from_bytes([0x33; AUTHENTICATION_NONCE_BYTES]);
    let key = AuthenticationKey::from_bytes([0x44; AUTHENTICATION_KEY_BYTES]);
    let client_proof = create_client_proof(&key, &host_epoch, &server_nonce, &client_nonce);
    let server_proof = create_server_proof(&key, &host_epoch, &server_nonce, &client_nonce);

    let challenge = AuthenticationRecord::ServerChallenge {
        authentication_protocol_version: super::AUTH_PROTOCOL_VERSION,
        host_epoch,
        server_nonce,
    }
    .encode();
    assert_eq!(challenge.len(), SERVER_CHALLENGE_BYTES);
    assert_eq!(challenge[0], super::SERVER_CHALLENGE_TAG);
    assert_eq!(
        &challenge[1..5],
        &super::AUTH_PROTOCOL_VERSION.to_be_bytes()
    );
    assert_eq!(&challenge[5..21], host_epoch.as_bytes());
    assert_eq!(&challenge[21..53], server_nonce.as_bytes());

    let client = AuthenticationRecord::ClientProof {
        client_nonce,
        proof: client_proof,
    }
    .encode();
    assert_eq!(client.len(), CLIENT_PROOF_BYTES);
    assert_eq!(client[0], super::CLIENT_PROOF_TAG);
    assert_eq!(&client[1..33], client_nonce.as_bytes());

    let server = AuthenticationRecord::ServerProof {
        proof: server_proof,
    }
    .encode();
    assert_eq!(server.len(), SERVER_PROOF_BYTES);
    assert_eq!(server[0], super::SERVER_PROOF_TAG);
}

#[test]
fn authentication_records_round_trip() {
    let records = [
        AuthenticationRecord::ServerChallenge {
            authentication_protocol_version: super::AUTH_PROTOCOL_VERSION,
            host_epoch: HostEpoch::from_bytes([0x11; HOST_EPOCH_BYTES]),
            server_nonce: AuthenticationNonce::from_bytes([0x22; AUTHENTICATION_NONCE_BYTES]),
        },
        AuthenticationRecord::ClientProof {
            client_nonce: AuthenticationNonce::from_bytes([0x33; AUTHENTICATION_NONCE_BYTES]),
            proof: super::AuthenticationProof([0x44; AUTHENTICATION_PROOF_BYTES]),
        },
        AuthenticationRecord::ServerProof {
            proof: super::AuthenticationProof([0x55; AUTHENTICATION_PROOF_BYTES]),
        },
    ];

    for expected in records {
        let encoded = expected.encode();
        let actual =
            AuthenticationRecord::decode(&encoded).expect("authentication record should decode");
        assert_eq!(actual.encode(), encoded);
    }
}

#[test]
fn authentication_record_rejects_unknown_tag_and_wrong_length() {
    let unknown =
        AuthenticationRecord::decode(&[0xff]).expect_err("unknown authentication tag should fail");
    assert!(matches!(
        unknown,
        AuthenticationRecordError::UnknownTag { tag: 0xff }
    ));

    let wrong_length = AuthenticationRecord::decode(&[super::SERVER_PROOF_TAG, 0x00])
        .expect_err("wrong authentication length should fail");
    assert!(matches!(
        wrong_length,
        AuthenticationRecordError::InvalidPayloadLength {
            tag: super::SERVER_PROOF_TAG,
            expected_payload_bytes: SERVER_PROOF_BYTES,
            received_payload_bytes: 2,
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn client_and_server_mutually_authenticate() {
    let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let server_key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);
    let (mut client, mut server) = tokio::io::duplex(1024);

    let client_authentication = authenticate_client(&mut client, &key, &host_epoch);
    let server_authentication = authenticate_server(&mut server, &server_key, &host_epoch);
    let (client_result, server_result) = tokio::join!(client_authentication, server_authentication);

    client_result.expect("client should authenticate server");
    server_result.expect("server should authenticate client");
}

#[tokio::test(flavor = "current_thread")]
async fn client_rejects_authentication_protocol_version_mismatch() {
    let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);
    let server_nonce = AuthenticationNonce::from_bytes([0x33; AUTHENTICATION_NONCE_BYTES]);
    let (mut client, mut server) = tokio::io::duplex(1024);

    let client_authentication = authenticate_client(&mut client, &key, &host_epoch);
    let challenge = AuthenticationRecord::ServerChallenge {
        authentication_protocol_version: super::AUTH_PROTOCOL_VERSION + 1,
        host_epoch,
        server_nonce,
    };
    let fake_server = write_authentication_record(&mut server, &challenge);
    let (client_result, server_result) = tokio::join!(client_authentication, fake_server);

    server_result.expect("challenge should be written");
    assert!(matches!(
        client_result,
        Err(AuthenticationError::ProtocolVersionMismatch {
            expected_protocol_version: super::AUTH_PROTOCOL_VERSION,
            received_protocol_version,
        }) if received_protocol_version == super::AUTH_PROTOCOL_VERSION + 1
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn client_rejects_unregistered_host_epoch() {
    let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let expected_host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);
    let received_host_epoch = HostEpoch::from_bytes([0x23; HOST_EPOCH_BYTES]);
    let server_nonce = AuthenticationNonce::from_bytes([0x33; AUTHENTICATION_NONCE_BYTES]);
    let (mut client, mut server) = tokio::io::duplex(1024);

    let client_authentication = authenticate_client(&mut client, &key, &expected_host_epoch);
    let challenge = AuthenticationRecord::ServerChallenge {
        authentication_protocol_version: super::AUTH_PROTOCOL_VERSION,
        host_epoch: received_host_epoch,
        server_nonce,
    };
    let fake_server = write_authentication_record(&mut server, &challenge);
    let (client_result, server_result) = tokio::join!(client_authentication, fake_server);

    server_result.expect("challenge should be written");
    assert!(matches!(
        client_result,
        Err(AuthenticationError::HostEpochMismatch)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_keys_fail_without_server_proof() {
    let client_key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let server_key = AuthenticationKey::from_bytes([0x12; AUTHENTICATION_KEY_BYTES]);
    let host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);
    let (mut client, mut server) = tokio::io::duplex(1024);

    let client_authentication = authenticate_client(&mut client, &client_key, &host_epoch);
    let server_authentication = async move {
        let result = authenticate_server(&mut server, &server_key, &host_epoch).await;
        drop(server);
        result
    };
    let (client_result, server_result) = tokio::join!(client_authentication, server_authentication);

    assert!(matches!(
        server_result,
        Err(AuthenticationError::InvalidProof)
    ));
    assert!(matches!(
        client_result,
        Err(AuthenticationError::PeerDisconnected)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn reflected_client_proof_is_not_a_server_proof() {
    let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);
    let server_nonce = AuthenticationNonce::from_bytes([0x33; AUTHENTICATION_NONCE_BYTES]);
    let (mut client, mut server) = tokio::io::duplex(1024);

    let client_authentication = authenticate_client(&mut client, &key, &host_epoch);
    let fake_server = async {
        write_authentication_record(
            &mut server,
            &AuthenticationRecord::ServerChallenge {
                authentication_protocol_version: super::AUTH_PROTOCOL_VERSION,
                host_epoch,
                server_nonce,
            },
        )
        .await
        .expect("challenge should be written");

        let response = read_authentication_record(&mut server)
            .await
            .expect("client proof should be read")
            .expect("client should send a proof");
        let AuthenticationRecord::ClientProof { proof, .. } = response else {
            panic!("client should send a client proof");
        };

        write_authentication_record(&mut server, &AuthenticationRecord::ServerProof { proof })
            .await
            .expect("reflected proof should be written");
    };
    let (client_result, ()) = tokio::join!(client_authentication, fake_server);

    assert!(matches!(
        client_result,
        Err(AuthenticationError::InvalidProof)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_authentication_frame_is_rejected_from_header() {
    let declared_payload_bytes = MAX_AUTHENTICATION_FRAME_PAYLOAD_BYTES + 1;
    let declared_length =
        u32::try_from(declared_payload_bytes).expect("test payload should fit in u32");
    let (mut writer, mut reader) = tokio::io::duplex(FRAME_HEADER_BYTES);

    writer
        .write_all(&declared_length.to_be_bytes())
        .await
        .expect("authentication frame header should be writable");

    let error = read_authentication_record(&mut reader)
        .await
        .expect_err("oversized authentication frame should fail");
    assert!(matches!(
        error,
        AuthenticationError::Frame(FrameError::PayloadTooLarge {
            payload_bytes,
            maximum_payload_bytes,
        }) if payload_bytes == declared_payload_bytes
            && maximum_payload_bytes == MAX_AUTHENTICATION_FRAME_PAYLOAD_BYTES
    ));
}

#[test]
fn authentication_material_debug_output_is_redacted() {
    let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
    let nonce = AuthenticationNonce::from_bytes([0x22; AUTHENTICATION_NONCE_BYTES]);
    let host_epoch = HostEpoch::from_bytes([0x33; HOST_EPOCH_BYTES]);
    let proof = create_client_proof(&key, &host_epoch, &nonce, &nonce);

    assert_eq!(format!("{key:?}"), "AuthenticationKey([REDACTED])");
    assert_eq!(format!("{nonce:?}"), "AuthenticationNonce([REDACTED])");
    assert_eq!(format!("{proof:?}"), "AuthenticationProof([REDACTED])");
}

fn sequential_bytes<const N: usize>(start: u8) -> [u8; N] {
    std::array::from_fn(|index| {
        start.wrapping_add(u8::try_from(index).expect("test byte index should fit in u8"))
    })
}

fn decode_hex<const N: usize>(encoded: &str) -> [u8; N] {
    assert_eq!(encoded.len(), N * 2);
    std::array::from_fn(|index| {
        let start = index * 2;
        u8::from_str_radix(&encoded[start..start + 2], 16)
            .expect("test vector should contain hexadecimal bytes")
    })
}
