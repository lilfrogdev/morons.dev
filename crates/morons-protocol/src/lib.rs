mod authentication;
mod control;
mod endpoint;
mod framing;
mod messages;
mod session;

pub use authentication::{
    AUTH_PROTOCOL_VERSION, AUTHENTICATION_KEY_BYTES, AuthenticationError, AuthenticationKey,
    AuthenticationRecordError, HOST_EPOCH_BYTES, HostEpoch, RandomnessError, authenticate_client,
    authenticate_server,
};
pub use control::{ClientEndpoint, ControlError, ServerEndpoint};
pub use endpoint::{authorize_accepted_peer, verify_connected_server_peer};
pub use framing::{
    FrameError, MAX_FRAME_PAYLOAD_BYTES, read_client_message, read_server_message,
    write_client_message, write_server_message,
};
pub use messages::{ClientMessage, ServerMessage};
pub use session::{
    APPLICATION_IDENTIFIER_BYTES, ApplicationError, ApplicationRequest, ApplicationResponse,
    MutationRequestId, ResourceLimit, SessionId, SessionListCursor, SessionSummary,
};

pub const PROTOCOL_VERSION: u32 = 2;
