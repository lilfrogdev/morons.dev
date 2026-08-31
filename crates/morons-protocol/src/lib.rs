mod authentication;
mod control;
mod credential;
mod endpoint;
mod framing;
mod messages;
mod run;
mod session;

pub use authentication::{
    AUTH_PROTOCOL_VERSION, AUTHENTICATION_KEY_BYTES, AuthenticationError, AuthenticationKey,
    AuthenticationRecordError, HOST_EPOCH_BYTES, HostEpoch, RandomnessError, authenticate_client,
    authenticate_server,
};
pub use control::{ClientEndpoint, ClientEndpointDiscovery, ControlError, ServerEndpoint};
pub use credential::{
    MAX_OPENCODE_API_KEY_BYTES, OpenCodeApiKey, OpenCodeApiKeyError, OpenCodeCredentialStatus,
};
pub use endpoint::{authorize_accepted_peer, verify_connected_server_peer};
pub use framing::{
    FrameError, MAX_FRAME_PAYLOAD_BYTES, read_client_message, read_server_message,
    write_client_message, write_server_message,
};
pub use messages::{ClientMessage, ServerMessage};
pub use run::{
    MessageId, OpenCodeModelCapabilities, OpenCodeModelRetention, OpenCodeModelSummary,
    OpenCodeModelTrainingUse, OpenCodeService, RunFailureKind, RunId, RunState, RunSummary,
    TranscriptCursor, TranscriptEntry,
};
pub use session::{
    APPLICATION_IDENTIFIER_BYTES, ApplicationError, ApplicationEvent, ApplicationRequest,
    ApplicationResponse, MutationRequestId, ResourceLimit, SessionCatalogEventCursor,
    SessionEventCursor, SessionId, SessionListCursor, SessionSummary,
};

pub const PROTOCOL_VERSION: u32 = 8;
