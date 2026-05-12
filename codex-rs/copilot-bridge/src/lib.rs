mod auth;
mod bridge;
mod chat;
mod device_flow;
mod error;
mod responses;
mod transport;

pub use auth::{
    CopilotAuth, StoredAuth, COPILOT_DEFAULT_ENDPOINT, COPILOT_EDITOR_VERSION,
    COPILOT_INTEGRATION_ID, COPILOT_TOKEN_REGISTRY_URL,
};
pub use bridge::{
    chat_response_to_responses, responses_request_to_chat, CopilotResponsesClient,
};
pub use chat::{
    ChatChoice, ChatChoiceMessage, ChatMessage, ChatRequest, ChatResponse, ChatRole, ChatToolCall,
    ChatToolCallFunction, ChatUsage, CopilotChatClient,
};
pub use device_flow::{
    poll_device_code_for_oauth_token, start_device_code_flow, DeviceCodeResponse,
    COPILOT_GITHUB_CLIENT_ID,
};
pub use error::{CopilotError, CopilotResult};
pub use responses::{
    ContentItem, FunctionCallOutputPayload, ResponseItem, ResponsesRequest, ResponsesResponse,
    ResponsesUsage,
};
pub use transport::CopilotTransport;
