use std::{
    fmt,
    io::{self, Read, Write},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const SANDBOX_PROTOCOL_VERSION: u16 = 2;
const FRAME_MAGIC: &[u8; 4] = b"MSBX";
const HEADER_BYTES: usize = 10;
const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxLimits {
    pub wall_time_milliseconds: u64,
    pub output_bytes_per_stream: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRequest {
    pub protocol_version: u16,
    pub operation_id: [u8; 16],
    pub candidate_root: String,
    pub scratch_root: String,
    pub image_root: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub limits: SandboxLimits,
}

impl fmt::Debug for SandboxRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxRequest")
            .field("protocol_version", &self.protocol_version)
            .field("operation_id", &self.operation_id)
            .field("candidate_root", &"[REDACTED]")
            .field("scratch_root", &"[REDACTED]")
            .field("image_root", &"[REDACTED]")
            .field("executable", &self.executable)
            .field("argument_count", &self.arguments.len())
            .field("working_directory", &self.working_directory)
            .field("limits", &self.limits)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxExit {
    pub code: Option<i32>,
    pub signal: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    Exited,
    Signalled,
    Crashed,
    Cancelled,
    TimedOut,
    OutputLimit,
    ResourceLimit,
    RequestRejected,
    BackendUnavailable,
    LaunchFailed,
    ProcessTreeUncertain,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxResult {
    pub protocol_version: u16,
    pub operation_id: [u8; 16],
    pub status: SandboxStatus,
    pub exit: Option<SandboxExit>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub candidate_eligible: bool,
}

impl SandboxResult {
    #[must_use]
    pub const fn failure(operation_id: [u8; 16], status: SandboxStatus) -> Self {
        Self {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id,
            status,
            exit: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            candidate_eligible: false,
        }
    }

    fn is_valid(&self) -> bool {
        let exited = self.status == SandboxStatus::Exited;
        let signalled = self.status == SandboxStatus::Signalled;
        let crashed = self.status == SandboxStatus::Crashed;
        let exit_valid = self.exit.is_some_and(|exit| {
            ((exited || crashed) && exit.code.is_some() && exit.signal.is_none())
                || (signalled && exit.code.is_none() && exit.signal.is_some())
        });
        let completed = exited || signalled || crashed;
        let identifier_valid = self.operation_id.iter().any(|byte| *byte != 0)
            || self.status == SandboxStatus::RequestRejected;
        self.protocol_version == SANDBOX_PROTOCOL_VERSION
            && identifier_valid
            && self.stdout.len() <= MAX_STREAM_BYTES
            && self.stderr.len() <= MAX_STREAM_BYTES
            && self.candidate_eligible == exited
            && exit_valid == completed
            && (completed || self.stdout.is_empty() && self.stderr.is_empty())
    }
}

impl fmt::Debug for SandboxResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxResult")
            .field("protocol_version", &self.protocol_version)
            .field("operation_id", &self.operation_id)
            .field("status", &self.status)
            .field("exit", &self.exit)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("candidate_eligible", &self.candidate_eligible)
            .finish()
    }
}

pub fn read_request(reader: &mut impl Read) -> io::Result<SandboxRequest> {
    read_frame(reader, MAX_REQUEST_BYTES)
}

pub fn write_request(writer: &mut impl Write, request: &SandboxRequest) -> io::Result<()> {
    write_frame(writer, request, MAX_REQUEST_BYTES)
}

pub fn read_result(reader: &mut impl Read) -> io::Result<SandboxResult> {
    let result: SandboxResult = read_frame(reader, MAX_RESULT_BYTES)?;
    if !result.is_valid() {
        return Err(invalid_data("the sandbox result is inconsistent"));
    }
    Ok(result)
}

pub fn write_result(writer: &mut impl Write, result: &SandboxResult) -> io::Result<()> {
    if !result.is_valid() {
        return Err(invalid_data("the sandbox result is inconsistent"));
    }
    write_frame(writer, result, MAX_RESULT_BYTES)
}

fn read_frame<T: DeserializeOwned + Serialize>(
    reader: &mut impl Read,
    maximum: usize,
) -> io::Result<T> {
    let mut header = [0_u8; HEADER_BYTES];
    reader.read_exact(&mut header)?;
    if &header[..FRAME_MAGIC.len()] != FRAME_MAGIC {
        return Err(invalid_data("the sandbox frame magic is invalid"));
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != SANDBOX_PROTOCOL_VERSION {
        return Err(invalid_data("the sandbox frame version is unsupported"));
    }
    let length = u32::from_be_bytes(
        header[6..10]
            .try_into()
            .map_err(|_| invalid_data("the sandbox frame length is malformed"))?,
    ) as usize;
    if length == 0 || length > maximum {
        return Err(invalid_data("the sandbox frame length is invalid"));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    let value: T = serde_json::from_slice(&payload)
        .map_err(|_| invalid_data("the sandbox payload is invalid"))?;
    let canonical = serde_json::to_vec(&value)
        .map_err(|_| invalid_data("the sandbox payload could not be canonicalized"))?;
    if payload != canonical {
        return Err(invalid_data("the sandbox payload is not canonical"));
    }
    Ok(value)
}

fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T, maximum: usize) -> io::Result<()> {
    let payload = serde_json::to_vec(value)
        .map_err(|_| invalid_data("the sandbox payload could not be encoded"))?;
    if payload.is_empty() || payload.len() > maximum {
        return Err(invalid_data("the sandbox payload exceeds its bound"));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| invalid_data("the sandbox payload length is invalid"))?;
    writer.write_all(FRAME_MAGIC)?;
    writer.write_all(&SANDBOX_PROTOCOL_VERSION.to_be_bytes())?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> SandboxRequest {
        SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [7; 16],
            candidate_root: "/private/candidate".to_owned(),
            scratch_root: "/private/scratch".to_owned(),
            image_root: "/private/image".to_owned(),
            executable: "bin/cargo".to_owned(),
            arguments: vec!["check".to_owned(), "--locked".to_owned()],
            working_directory: ".".to_owned(),
            limits: SandboxLimits {
                wall_time_milliseconds: 30_000,
                output_bytes_per_stream: 64 * 1024,
            },
        }
    }

    #[test]
    fn request_frame_uses_explicit_header_and_round_trips() {
        let request = request();
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request).expect("request encodes");
        assert_eq!(&bytes[..4], FRAME_MAGIC);
        assert_eq!(&bytes[4..6], &SANDBOX_PROTOCOL_VERSION.to_be_bytes());
        assert_eq!(
            u32::from_be_bytes(bytes[6..10].try_into().expect("length")) as usize,
            bytes.len() - HEADER_BYTES
        );
        assert_eq!(
            read_request(&mut bytes.as_slice()).expect("request decodes"),
            request
        );
    }

    #[test]
    fn malformed_and_unknown_request_fields_are_rejected() {
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request()).expect("request encodes");
        bytes[0] = b'X';
        assert!(read_request(&mut bytes.as_slice()).is_err());

        let payload = br#"{"protocol_version":2,"operation_id":[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7],"candidate_root":"/c","scratch_root":"/s","image_root":"/i","executable":"bin/cargo","arguments":[],"working_directory":".","limits":{"wall_time_milliseconds":1,"output_bytes_per_stream":1},"extra":true}"#;
        let mut frame = Vec::new();
        frame.extend_from_slice(FRAME_MAGIC);
        frame.extend_from_slice(&SANDBOX_PROTOCOL_VERSION.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        assert!(read_request(&mut frame.as_slice()).is_err());

        let mut canonical = Vec::new();
        write_request(&mut canonical, &request()).expect("request encodes");
        canonical.push(b' ');
        let expanded = u32::try_from(canonical.len() - HEADER_BYTES).expect("expanded length");
        canonical[6..10].copy_from_slice(&expanded.to_be_bytes());
        assert!(read_request(&mut canonical.as_slice()).is_err());
    }

    #[test]
    fn inconsistent_results_cannot_cross_the_helper_boundary() {
        let invalid = SandboxResult {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [7; 16],
            status: SandboxStatus::Exited,
            exit: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            candidate_eligible: true,
        };
        assert!(write_result(&mut Vec::new(), &invalid).is_err());

        let invalid = SandboxResult {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [7; 16],
            status: SandboxStatus::TimedOut,
            exit: None,
            stdout: b"partial".to_vec(),
            stderr: Vec::new(),
            candidate_eligible: false,
        };
        assert!(write_result(&mut Vec::new(), &invalid).is_err());

        let signalled = SandboxResult {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [7; 16],
            status: SandboxStatus::Signalled,
            exit: Some(SandboxExit {
                code: None,
                signal: Some(15),
            }),
            stdout: b"before-signal".to_vec(),
            stderr: Vec::new(),
            candidate_eligible: false,
        };
        assert!(write_result(&mut Vec::new(), &signalled).is_ok());

        let mut invalid = signalled;
        invalid.candidate_eligible = true;
        assert!(write_result(&mut Vec::new(), &invalid).is_err());

        let crashed = SandboxResult {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [7; 16],
            status: SandboxStatus::Crashed,
            exit: Some(SandboxExit {
                code: Some(-1_073_741_819),
                signal: None,
            }),
            stdout: Vec::new(),
            stderr: Vec::new(),
            candidate_eligible: false,
        };
        assert!(write_result(&mut Vec::new(), &crashed).is_ok());
    }

    #[test]
    fn debug_output_redacts_roots_and_stream_bytes() {
        let request_debug = format!("{:?}", request());
        assert!(!request_debug.contains("/private/candidate"));
        let result = SandboxResult {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [7; 16],
            status: SandboxStatus::Exited,
            exit: Some(SandboxExit {
                code: Some(0),
                signal: None,
            }),
            stdout: b"repository output".to_vec(),
            stderr: b"private diagnostic".to_vec(),
            candidate_eligible: true,
        };
        let result_debug = format!("{result:?}");
        assert!(!result_debug.contains("repository output"));
        assert!(!result_debug.contains("private diagnostic"));
    }
}
