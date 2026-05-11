use async_trait::async_trait;
use codex_api::Provider;
use codex_client::{
    HttpTransport, ReqwestTransport, Request, Response, StreamResponse, TransportError,
};
use codex_copilot_bridge::{
    CopilotAuth, CopilotChatClient, CopilotError, CopilotResponsesClient, CopilotTransport,
};
use codex_login::default_client::build_reqwest_client;
use std::sync::OnceLock;

/// Codex's default transport is reqwest-over-HTTPS to whatever `base_url`
/// the provider declared. When the active provider is the embedded Copilot
/// provider (`name == "copilot"`), short-circuit to an in-process bridge
/// that translates Responses requests to Copilot's chat/completions API.
/// Other providers continue to use the reqwest path so OpenAI / Azure /
/// OSS all behave exactly as before.
pub enum CodexTransport {
    Reqwest(ReqwestTransport),
    Copilot(CopilotTransport),
}

#[async_trait]
impl HttpTransport for CodexTransport {
    async fn execute(&self, req: Request) -> Result<Response, TransportError> {
        match self {
            Self::Reqwest(t) => t.execute(req).await,
            Self::Copilot(t) => t.execute(req).await,
        }
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        match self {
            Self::Reqwest(t) => t.stream(req).await,
            Self::Copilot(t) => t.stream(req).await,
        }
    }
}

pub fn build_transport_for_provider(provider: &Provider) -> CodexTransport {
    if is_embedded_copilot(provider) {
        match build_copilot_transport() {
            Ok(t) => return CodexTransport::Copilot(t),
            Err(err) => {
                tracing::warn!(
                    "embedded copilot transport unavailable ({err}); falling back to HTTP"
                );
            }
        }
    }
    CodexTransport::Reqwest(ReqwestTransport::new(build_reqwest_client()))
}

fn is_embedded_copilot(provider: &Provider) -> bool {
    provider.name.eq_ignore_ascii_case("copilot")
        || provider.name.eq_ignore_ascii_case("github-copilot")
}

fn build_copilot_transport() -> Result<CopilotTransport, CopilotError> {
    static CLIENT: OnceLock<CopilotChatClient> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(CopilotTransport::new(CopilotResponsesClient::new(client.clone())));
    }
    let auth = CopilotAuth::from_env()?;
    let client = CopilotChatClient::new(auth)?;
    let stored = CLIENT.get_or_init(|| client.clone());
    Ok(CopilotTransport::new(CopilotResponsesClient::new(stored.clone())))
}
