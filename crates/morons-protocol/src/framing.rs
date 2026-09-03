use std::{error::Error, fmt, io};

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::{ClientMessage, ServerMessage};

pub(crate) const FRAME_HEADER_BYTES: usize = std::mem::size_of::<u32>();

/// Maximum JSON payload size, excluding the four-byte frame header.
pub const MAX_FRAME_PAYLOAD_BYTES: usize = 12 * 1024 * 1024;

#[derive(Debug)]
#[non_exhaustive]
pub enum FrameError {
    Io(io::Error),
    Json(serde_json::Error),
    PayloadTooLarge {
        payload_bytes: usize,
        maximum_payload_bytes: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "frame I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "frame JSON is invalid: {error}"),
            Self::PayloadTooLarge {
                payload_bytes,
                maximum_payload_bytes,
            } => write!(
                formatter,
                "frame payload is {payload_bytes} bytes; maximum is {maximum_payload_bytes} bytes"
            ),
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::PayloadTooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub async fn write_client_message<W>(
    writer: &mut W,
    message: &ClientMessage,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    write_message(writer, message).await
}

pub async fn write_server_message<W>(
    writer: &mut W,
    message: &ServerMessage,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    write_message(writer, message).await
}

/// Reads a client message, returning `None` for clean EOF; close the connection after any error.
pub async fn read_client_message<R>(reader: &mut R) -> Result<Option<ClientMessage>, FrameError>
where
    R: AsyncRead + Unpin,
{
    read_message(reader).await
}

/// Reads a server message, returning `None` for clean EOF; close the connection after any error.
pub async fn read_server_message<R>(reader: &mut R) -> Result<Option<ServerMessage>, FrameError>
where
    R: AsyncRead + Unpin,
{
    read_message(reader).await
}

async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = Zeroizing::new(serde_json::to_vec(message)?);
    write_frame(writer, &payload, MAX_FRAME_PAYLOAD_BYTES).await
}

async fn read_message<R, T>(reader: &mut R) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let Some(payload) = read_frame(reader, MAX_FRAME_PAYLOAD_BYTES).await? else {
        return Ok(None);
    };
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(FrameError::from)
}

pub(crate) async fn write_frame<W>(
    writer: &mut W,
    payload: &[u8],
    maximum_payload_bytes: usize,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let payload_bytes = payload.len();
    validate_payload_size(payload_bytes, maximum_payload_bytes)?;

    let payload_length = u32::try_from(payload_bytes).map_err(|_| FrameError::PayloadTooLarge {
        payload_bytes,
        maximum_payload_bytes,
    })?;

    writer.write_all(&payload_length.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;

    Ok(())
}

pub(crate) async fn read_frame<R>(
    reader: &mut R,
    maximum_payload_bytes: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; FRAME_HEADER_BYTES];

    if reader.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }

    reader.read_exact(&mut header[1..]).await?;

    let payload_bytes = u32::from_be_bytes(header) as usize;
    validate_payload_size(payload_bytes, maximum_payload_bytes)?;

    let mut payload = Zeroizing::new(vec![0_u8; payload_bytes]);
    reader.read_exact(&mut payload).await?;

    Ok(Some(payload))
}

fn validate_payload_size(
    payload_bytes: usize,
    maximum_payload_bytes: usize,
) -> Result<(), FrameError> {
    if payload_bytes > maximum_payload_bytes {
        return Err(FrameError::PayloadTooLarge {
            payload_bytes,
            maximum_payload_bytes,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

    use super::{
        FRAME_HEADER_BYTES, FrameError, MAX_FRAME_PAYLOAD_BYTES, read_client_message,
        read_server_message, write_client_message, write_server_message,
    };
    use crate::{ClientMessage, ServerMessage};

    const TEST_CLIENT_VERSION: &str = "test-client-version";
    const TEST_SERVER_VERSION: &str = "test-server-version";

    #[tokio::test(flavor = "current_thread")]
    async fn client_message_round_trips_through_frame() {
        let expected = ClientMessage::hello(TEST_CLIENT_VERSION);
        let (mut writer, mut reader) = tokio::io::duplex(1024);

        write_client_message(&mut writer, &expected)
            .await
            .expect("client message should be written");

        let actual = read_client_message(&mut reader)
            .await
            .expect("client message should be read")
            .expect("stream should contain a frame");

        assert_eq!(actual, expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_message_round_trips_through_frame() {
        let expected = ServerMessage::hello(TEST_SERVER_VERSION);
        let (mut writer, mut reader) = tokio::io::duplex(1024);

        write_server_message(&mut writer, &expected)
            .await
            .expect("server message should be written");

        let actual = read_server_message(&mut reader)
            .await
            .expect("server message should be read")
            .expect("stream should contain a frame");

        assert_eq!(actual, expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn frame_uses_big_endian_payload_length() {
        let message = ClientMessage::hello(TEST_CLIENT_VERSION);
        let expected_payload = message.encode_json().expect("message should encode");
        let expected_length =
            u32::try_from(expected_payload.len()).expect("test payload should fit in u32");
        let (mut writer, mut reader) = tokio::io::duplex(1024);

        write_client_message(&mut writer, &message)
            .await
            .expect("client message should be written");

        let mut actual_header = [0_u8; FRAME_HEADER_BYTES];
        reader
            .read_exact(&mut actual_header)
            .await
            .expect("frame header should be readable");

        let mut actual_payload = vec![0_u8; expected_payload.len()];
        reader
            .read_exact(&mut actual_payload)
            .await
            .expect("frame payload should be readable");

        assert_eq!(actual_header, expected_length.to_be_bytes());
        assert_eq!(actual_payload, expected_payload);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clean_disconnect_between_frames_returns_none() {
        let (writer, mut reader) = tokio::io::duplex(64);
        drop(writer);

        let actual = read_client_message(&mut reader)
            .await
            .expect("clean disconnect should not be an error");

        assert_eq!(actual, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_incoming_frame_is_rejected_from_header() {
        let declared_payload_bytes = MAX_FRAME_PAYLOAD_BYTES + 1;
        let declared_length =
            u32::try_from(declared_payload_bytes).expect("test payload length should fit in u32");
        let (mut writer, mut reader) = tokio::io::duplex(FRAME_HEADER_BYTES);

        writer
            .write_all(&declared_length.to_be_bytes())
            .await
            .expect("frame header should be writable");

        let error = read_client_message(&mut reader)
            .await
            .expect_err("oversized frame should be rejected");

        assert!(matches!(
                error,
                FrameError::PayloadTooLarge {
                    payload_bytes,
                    maximum_payload_bytes,
                } if payload_bytes == declared_payload_bytes && maximum_payload_bytes == MAX_FRAME_PAYLOAD_BYTES
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_outgoing_frame_is_rejected() {
        let message = ClientMessage::hello("x".repeat(MAX_FRAME_PAYLOAD_BYTES));
        let payload_bytes = message
            .encode_json()
            .expect("test message should encode")
            .len();
        let mut writer = tokio::io::sink();

        let error = write_client_message(&mut writer, &message)
            .await
            .expect_err("oversized frame should be rejected");

        assert!(matches!(
                error,
                FrameError::PayloadTooLarge {
                    payload_bytes: actual,
                    maximum_payload_bytes,
                } if actual == payload_bytes && maximum_payload_bytes == MAX_FRAME_PAYLOAD_BYTES
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_json_frame_is_rejected() {
        let payload = br#"{"type":"not_a_client_message"}"#;
        let (mut writer, mut reader) = tokio::io::duplex(128);

        write_raw_frame(&mut writer, payload).await;

        let error = read_client_message(&mut reader)
            .await
            .expect_err("malformed client message should be rejected");

        assert!(matches!(error, FrameError::Json(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn truncated_frame_is_rejected() {
        let declared_payload_bytes = 10_u32;
        let (mut writer, mut reader) = tokio::io::duplex(64);

        writer
            .write_all(&declared_payload_bytes.to_be_bytes())
            .await
            .expect("frame header should be writable");
        writer
            .write_all(b"{}")
            .await
            .expect("partial frame payload should be writable");
        drop(writer);

        let error = read_client_message(&mut reader)
            .await
            .expect_err("truncated frame should be rejected");

        assert!(matches!(
                error,
                FrameError::Io(source) if source.kind() == ErrorKind::UnexpectedEof
        ));
    }

    async fn write_raw_frame<W>(writer: &mut W, payload: &[u8])
    where
        W: AsyncWrite + Unpin,
    {
        let payload_length =
            u32::try_from(payload.len()).expect("test payload length should fit in u32");

        writer
            .write_all(&payload_length.to_be_bytes())
            .await
            .expect("frame header should be writable");
        writer
            .write_all(payload)
            .await
            .expect("frame payload should be writable");
        writer.flush().await.expect("frame should flush");
    }
}
