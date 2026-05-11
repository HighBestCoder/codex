mod auth;
mod chat;
mod error;

pub use auth::{
    CopilotAuth, StoredAuth, COPILOT_DEFAULT_ENDPOINT, COPILOT_EDITOR_VERSION,
    COPILOT_INTEGRATION_ID, COPILOT_TOKEN_REGISTRY_URL,
};
pub use chat::{
    ChatChoice, ChatChoiceMessage, ChatMessage, ChatRequest, ChatResponse, ChatRole, ChatUsage,
    CopilotChatClient,
};
pub use error::{CopilotError, CopilotResult};
