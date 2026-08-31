use std::{error::Error, fmt};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncWrite};
use zeroize::Zeroizing;

use crate::framing::{FrameError, read_frame, write_frame};

pub const AUTH_PROTOCOL_VERSION: u32 = 1;
pub const AUTHENTICATION_KEY_BYTES: usize = 32;
const AUTHENTICATION_NONCE_BYTES: usize = 32;
const AUTHENTICATION_PROOF_BYTES: usize = 32;
pub const HOST_EPOCH_BYTES: usize = 16;
const MAX_AUTHENTICATION_FRAME_PAYLOAD_BYTES: usize = 65;

const AUTHENTICATION_CONTEXT: &[u8] = b"morons.dev/local-ipc-auth";
const SERVER_CHALLENGE_TAG: u8 = 0x01;
const CLIENT_PROOF_TAG: u8 = 0x02;
const SERVER_PROOF_TAG: u8 = 0x03;
const CLIENT_PROOF_ROLE: u8 = 0x01;
const SERVER_PROOF_ROLE: u8 = 0x02;
const SERVER_CHALLENGE_BYTES: usize = 53;
const CLIENT_PROOF_BYTES: usize = 65;
const SERVER_PROOF_BYTES: usize = 33;

type HmacSha256 = Hmac<Sha256>;

pub struct AuthenticationKey([u8; AUTHENTICATION_KEY_BYTES]);

impl AuthenticationKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; AUTHENTICATION_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) fn generate() -> Result<Self, RandomnessError> {
        random_bytes().map(Self)
    }

    #[must_use]
    pub(crate) const fn as_bytes(&self) -> &[u8; AUTHENTICATION_KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for AuthenticationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticationKey([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostEpoch([u8; HOST_EPOCH_BYTES]);

impl HostEpoch {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; HOST_EPOCH_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) fn generate() -> Result<Self, RandomnessError> {
        random_bytes().map(Self)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HOST_EPOCH_BYTES] {
        &self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct AuthenticationNonce([u8; AUTHENTICATION_NONCE_BYTES]);

impl AuthenticationNonce {
    #[must_use]
    const fn from_bytes(bytes: [u8; AUTHENTICATION_NONCE_BYTES]) -> Self {
        Self(bytes)
    }

    fn generate() -> Result<Self, RandomnessError> {
        random_bytes().map(Self)
    }

    #[must_use]
    const fn as_bytes(&self) -> &[u8; AUTHENTICATION_NONCE_BYTES] {
        &self.0
    }
}

impl fmt::Debug for AuthenticationNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticationNonce([REDACTED])")
    }
}

struct AuthenticationProof([u8; AUTHENTICATION_PROOF_BYTES]);

impl fmt::Debug for AuthenticationProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticationProof([REDACTED])")
    }
}

#[derive(Debug)]
pub struct RandomnessError(getrandom::Error);

impl fmt::Display for RandomnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "operating-system randomness failed: {}", self.0)
    }
}

impl Error for RandomnessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum AuthenticationRecordError {
    EmptyPayload,
    UnknownTag {
        tag: u8,
    },
    InvalidPayloadLength {
        tag: u8,
        expected_payload_bytes: usize,
        received_payload_bytes: usize,
    },
}

impl fmt::Display for AuthenticationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPayload => formatter.write_str("authentication record is empty"),
            Self::UnknownTag { tag } => {
                write!(formatter, "authentication record tag {tag:#04x} is unknown")
            }
            Self::InvalidPayloadLength {
                tag,
                expected_payload_bytes,
                received_payload_bytes,
            } => write!(
                formatter,
                "authentication record tag {tag:#04x} is {received_payload_bytes} bytes; expected {expected_payload_bytes} bytes"
            ),
        }
    }
}

impl Error for AuthenticationRecordError {}

#[derive(Debug)]
#[non_exhaustive]
pub enum AuthenticationError {
    Frame(FrameError),
    InvalidRecord(AuthenticationRecordError),
    Randomness(RandomnessError),
    PeerDisconnected,
    ProtocolVersionMismatch {
        expected_protocol_version: u32,
        received_protocol_version: u32,
    },
    HostEpochMismatch,
    UnexpectedRecord,
    InvalidProof,
}

impl fmt::Display for AuthenticationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "authentication frame failed: {error}"),
            Self::InvalidRecord(error) => {
                write!(formatter, "authentication record failed: {error}")
            }
            Self::Randomness(error) => {
                write!(formatter, "authentication randomness failed: {error}")
            }
            Self::PeerDisconnected => {
                formatter.write_str("peer disconnected during authentication")
            }
            Self::ProtocolVersionMismatch {
                expected_protocol_version,
                received_protocol_version,
            } => write!(
                formatter,
                "authentication protocol version mismatch: expected {expected_protocol_version}, received {received_protocol_version}"
            ),
            Self::HostEpochMismatch => formatter.write_str("authentication Host Epoch mismatch"),
            Self::UnexpectedRecord => {
                formatter.write_str("authentication record arrived out of order")
            }
            Self::InvalidProof => formatter.write_str("authentication proof is invalid"),
        }
    }
}

impl Error for AuthenticationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::InvalidRecord(error) => Some(error),
            Self::Randomness(error) => Some(error),
            Self::PeerDisconnected
            | Self::ProtocolVersionMismatch { .. }
            | Self::HostEpochMismatch
            | Self::UnexpectedRecord
            | Self::InvalidProof => None,
        }
    }
}

impl From<FrameError> for AuthenticationError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<AuthenticationRecordError> for AuthenticationError {
    fn from(error: AuthenticationRecordError) -> Self {
        Self::InvalidRecord(error)
    }
}

impl From<RandomnessError> for AuthenticationError {
    fn from(error: RandomnessError) -> Self {
        Self::Randomness(error)
    }
}

#[derive(Debug)]
enum AuthenticationRecord {
    ServerChallenge {
        authentication_protocol_version: u32,
        host_epoch: HostEpoch,
        server_nonce: AuthenticationNonce,
    },
    ClientProof {
        client_nonce: AuthenticationNonce,
        proof: AuthenticationProof,
    },
    ServerProof {
        proof: AuthenticationProof,
    },
}

impl AuthenticationRecord {
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::ServerChallenge {
                authentication_protocol_version,
                host_epoch,
                server_nonce,
            } => {
                let mut payload = Vec::with_capacity(SERVER_CHALLENGE_BYTES);
                payload.push(SERVER_CHALLENGE_TAG);
                payload.extend_from_slice(&authentication_protocol_version.to_be_bytes());
                payload.extend_from_slice(host_epoch.as_bytes());
                payload.extend_from_slice(server_nonce.as_bytes());
                payload
            }
            Self::ClientProof {
                client_nonce,
                proof,
            } => {
                let mut payload = Vec::with_capacity(CLIENT_PROOF_BYTES);
                payload.push(CLIENT_PROOF_TAG);
                payload.extend_from_slice(client_nonce.as_bytes());
                payload.extend_from_slice(&proof.0);
                payload
            }
            Self::ServerProof { proof } => {
                let mut payload = Vec::with_capacity(SERVER_PROOF_BYTES);
                payload.push(SERVER_PROOF_TAG);
                payload.extend_from_slice(&proof.0);
                payload
            }
        }
    }

    fn decode(payload: &[u8]) -> Result<Self, AuthenticationRecordError> {
        let Some(&tag) = payload.first() else {
            return Err(AuthenticationRecordError::EmptyPayload);
        };

        let expected_payload_bytes = match tag {
            SERVER_CHALLENGE_TAG => SERVER_CHALLENGE_BYTES,
            CLIENT_PROOF_TAG => CLIENT_PROOF_BYTES,
            SERVER_PROOF_TAG => SERVER_PROOF_BYTES,
            _ => return Err(AuthenticationRecordError::UnknownTag { tag }),
        };

        if payload.len() != expected_payload_bytes {
            return Err(AuthenticationRecordError::InvalidPayloadLength {
                tag,
                expected_payload_bytes,
                received_payload_bytes: payload.len(),
            });
        }

        match tag {
            SERVER_CHALLENGE_TAG => Ok(Self::ServerChallenge {
                authentication_protocol_version: u32::from_be_bytes(copy_array(&payload[1..5])),
                host_epoch: HostEpoch::from_bytes(copy_array(&payload[5..21])),
                server_nonce: AuthenticationNonce::from_bytes(copy_array(&payload[21..53])),
            }),
            CLIENT_PROOF_TAG => Ok(Self::ClientProof {
                client_nonce: AuthenticationNonce::from_bytes(copy_array(&payload[1..33])),
                proof: AuthenticationProof(copy_array(&payload[33..65])),
            }),
            SERVER_PROOF_TAG => Ok(Self::ServerProof {
                proof: AuthenticationProof(copy_array(&payload[1..33])),
            }),
            _ => unreachable!("authentication record tag was validated"),
        }
    }
}

pub async fn authenticate_client<S>(
    connection: &mut S,
    key: &AuthenticationKey,
    expected_host_epoch: &HostEpoch,
) -> Result<(), AuthenticationError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let challenge = read_authentication_record(connection)
        .await?
        .ok_or(AuthenticationError::PeerDisconnected)?;
    let AuthenticationRecord::ServerChallenge {
        authentication_protocol_version,
        host_epoch,
        server_nonce,
    } = challenge
    else {
        return Err(AuthenticationError::UnexpectedRecord);
    };

    if authentication_protocol_version != AUTH_PROTOCOL_VERSION {
        return Err(AuthenticationError::ProtocolVersionMismatch {
            expected_protocol_version: AUTH_PROTOCOL_VERSION,
            received_protocol_version: authentication_protocol_version,
        });
    }
    if host_epoch != *expected_host_epoch {
        return Err(AuthenticationError::HostEpochMismatch);
    }

    let client_nonce = AuthenticationNonce::generate()?;
    let proof = create_client_proof(key, expected_host_epoch, &server_nonce, &client_nonce);
    write_authentication_record(
        connection,
        &AuthenticationRecord::ClientProof {
            client_nonce,
            proof,
        },
    )
    .await?;

    let response = read_authentication_record(connection)
        .await?
        .ok_or(AuthenticationError::PeerDisconnected)?;
    let AuthenticationRecord::ServerProof { proof } = response else {
        return Err(AuthenticationError::UnexpectedRecord);
    };

    if !verify_server_proof(
        key,
        expected_host_epoch,
        &server_nonce,
        &client_nonce,
        &proof,
    ) {
        return Err(AuthenticationError::InvalidProof);
    }

    Ok(())
}

pub async fn authenticate_server<S>(
    connection: &mut S,
    key: &AuthenticationKey,
    host_epoch: &HostEpoch,
) -> Result<(), AuthenticationError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let server_nonce = AuthenticationNonce::generate()?;
    write_authentication_record(
        connection,
        &AuthenticationRecord::ServerChallenge {
            authentication_protocol_version: AUTH_PROTOCOL_VERSION,
            host_epoch: *host_epoch,
            server_nonce,
        },
    )
    .await?;

    let response = read_authentication_record(connection)
        .await?
        .ok_or(AuthenticationError::PeerDisconnected)?;
    let AuthenticationRecord::ClientProof {
        client_nonce,
        proof,
    } = response
    else {
        return Err(AuthenticationError::UnexpectedRecord);
    };

    if !verify_client_proof(key, host_epoch, &server_nonce, &client_nonce, &proof) {
        return Err(AuthenticationError::InvalidProof);
    }

    let proof = create_server_proof(key, host_epoch, &server_nonce, &client_nonce);
    write_authentication_record(connection, &AuthenticationRecord::ServerProof { proof }).await
}

#[must_use]
fn create_client_proof(
    key: &AuthenticationKey,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
) -> AuthenticationProof {
    create_proof(
        key,
        CLIENT_PROOF_ROLE,
        host_epoch,
        server_nonce,
        client_nonce,
    )
}

#[must_use]
fn create_server_proof(
    key: &AuthenticationKey,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
) -> AuthenticationProof {
    create_proof(
        key,
        SERVER_PROOF_ROLE,
        host_epoch,
        server_nonce,
        client_nonce,
    )
}

#[must_use]
fn verify_client_proof(
    key: &AuthenticationKey,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
    proof: &AuthenticationProof,
) -> bool {
    verify_proof(
        key,
        CLIENT_PROOF_ROLE,
        host_epoch,
        server_nonce,
        client_nonce,
        proof,
    )
}

#[must_use]
fn verify_server_proof(
    key: &AuthenticationKey,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
    proof: &AuthenticationProof,
) -> bool {
    verify_proof(
        key,
        SERVER_PROOF_ROLE,
        host_epoch,
        server_nonce,
        client_nonce,
        proof,
    )
}

async fn write_authentication_record<W>(
    writer: &mut W,
    record: &AuthenticationRecord,
) -> Result<(), AuthenticationError>
where
    W: AsyncWrite + Unpin,
{
    let payload = Zeroizing::new(record.encode());
    write_frame(writer, &payload, MAX_AUTHENTICATION_FRAME_PAYLOAD_BYTES).await?;
    Ok(())
}

async fn read_authentication_record<R>(
    reader: &mut R,
) -> Result<Option<AuthenticationRecord>, AuthenticationError>
where
    R: AsyncRead + Unpin,
{
    read_frame(reader, MAX_AUTHENTICATION_FRAME_PAYLOAD_BYTES)
        .await?
        .map(|payload| AuthenticationRecord::decode(&payload).map_err(AuthenticationError::from))
        .transpose()
}

fn create_proof(
    key: &AuthenticationKey,
    role: u8,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
) -> AuthenticationProof {
    let bytes = proof_mac(key, role, host_epoch, server_nonce, client_nonce)
        .finalize()
        .into_bytes();
    let mut proof = [0_u8; AUTHENTICATION_PROOF_BYTES];
    proof.copy_from_slice(&bytes);
    AuthenticationProof(proof)
}

fn verify_proof(
    key: &AuthenticationKey,
    role: u8,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
    proof: &AuthenticationProof,
) -> bool {
    proof_mac(key, role, host_epoch, server_nonce, client_nonce)
        .verify_slice(&proof.0)
        .is_ok()
}

fn proof_mac(
    key: &AuthenticationKey,
    role: u8,
    host_epoch: &HostEpoch,
    server_nonce: &AuthenticationNonce,
    client_nonce: &AuthenticationNonce,
) -> HmacSha256 {
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(AUTHENTICATION_CONTEXT);
    mac.update(&[role]);
    mac.update(&AUTH_PROTOCOL_VERSION.to_be_bytes());
    mac.update(host_epoch.as_bytes());
    mac.update(server_nonce.as_bytes());
    mac.update(client_nonce.as_bytes());
    mac
}

fn random_bytes<const N: usize>() -> Result<[u8; N], RandomnessError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(RandomnessError)?;
    Ok(bytes)
}

fn copy_array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut output = [0_u8; N];
    output.copy_from_slice(bytes);
    output
}

#[cfg(test)]
mod tests;
