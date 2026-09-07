mod authentication;
mod control;
mod credential;
mod openai_auth;
pub use openai_auth::{
    OpenAiAuthorizationUrl, OpenAiCredentialState, OpenAiCredentialStatus, OpenAiLoginFailure,
    OpenAiLoginResult, OpenAiTokenResponseFailure,
};
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
    ApplicationSettings, DataUsePolicy, ImageAttachmentId, ImageAttachmentSummary, ImageUpload,
    LocalCommandId, LocalCommandStatus, MessageId, ModelCapabilities, ModelRetention,
    ModelSelection, ModelService, ModelSummary, ModelTrainingUse, ProviderProtocol, RunFailureKind,
    RunId, RunState, RunSummary, SubagentModelSetting, ToolCallId, ToolKind, ToolResultStatus,
    TranscriptCursor, TranscriptEntry, TranscriptPageDirection,
};
pub use session::{
    APPLICATION_IDENTIFIER_BYTES, ApplicationError, ApplicationEvent, ApplicationRequest,
    ApplicationResponse, BackgroundCompactionJob, BackgroundCompactionState,
    BackgroundCompactionStatus, MAX_WORKING_DIRECTORY_PATH_BYTES, MutationRequestId,
    ProjectContextSummary, RecentProviderUsage, ResourceLimit, SessionCatalogEventCursor,
    SessionContextStatus, SessionEventCursor, SessionId, SessionListCursor, SessionSummary,
    SkillSource, SkillSummary,
};

pub const PROTOCOL_VERSION: u32 = 43;
