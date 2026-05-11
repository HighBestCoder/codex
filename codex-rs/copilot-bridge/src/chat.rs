use serde::{Deserialize, Serialize};

use crate::auth::{CopilotAuth, COPILOT_EDITOR_VERSION, COPILOT_INTEGRATION_ID};
use crate::error::{CopilotError, CopilotResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            name: None,
            tool_call_id: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            name: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            name: None,
            tool_call_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stream: Option<bool>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: Some(false),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub id: Option<String>,
    pub model: Option<String>,
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    pub index: Option<u32>,
    pub message: ChatChoiceMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoiceMessage {
    pub role: ChatRole,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct CopilotChatClient {
    auth: CopilotAuth,
    http: reqwest::Client,
}

impl CopilotChatClient {
    pub fn new(auth: CopilotAuth) -> CopilotResult<Self> {
        let http = reqwest::Client::builder()
            .user_agent("codex-copilot-bridge/0.0.0")
            .build()?;
        Ok(Self { auth, http })
    }

    pub async fn chat_completion(&self, request: &ChatRequest) -> CopilotResult<ChatResponse> {
        let (session_token, endpoint) = self.auth.ensure_session_token().await?;
        let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
        let response = self
            .http
            .post(&url)
            .bearer_auth(session_token)
            .header("Editor-Version", COPILOT_EDITOR_VERSION)
            .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
            .header("Accept", "application/json")
            .json(request)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(CopilotError::ChatCompletion {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: ChatResponse =
            serde_json::from_str(&body).map_err(|source| CopilotError::JsonParse {
                body: body.clone(),
                source,
            })?;
        Ok(parsed)
    }

    pub fn auth(&self) -> &CopilotAuth {
        &self.auth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_with_minimal_fields() {
        let req = ChatRequest::new(
            "gpt-4o",
            vec![ChatMessage::system("be terse"), ChatMessage::user("hi")],
        );
        let json = serde_json::to_value(&req).expect("serialize");
        let messages = json["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hi");
        assert!(json.get("max_tokens").is_none());
        assert_eq!(json["stream"], false);
    }

    #[test]
    fn chat_response_deserializes_typical_openai_payload() {
        let raw = r#"{
            "id": "chatcmpl_abc",
            "model": "gpt-4o",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}
            ],
            "usage": {"prompt_tokens": 7, "completion_tokens": 1, "total_tokens": 8}
        }"#;
        let resp: ChatResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(resp.id.as_deref(), Some("chatcmpl_abc"));
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.role, ChatRole::Assistant);
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("hello"));
        assert_eq!(resp.usage.and_then(|u| u.total_tokens), Some(8));
    }

    #[test]
    fn chat_response_tolerates_missing_optional_fields() {
        let raw = r#"{"choices":[{"message":{"role":"assistant"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(resp.choices.len(), 1);
        assert!(resp.choices[0].message.content.is_none());
    }
}
