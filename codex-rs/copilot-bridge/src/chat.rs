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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ChatToolCall>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: Some(content.into()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: Some(content.into()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: Some(content.into()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn assistant_tool_calls(calls: Vec<ChatToolCall>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: None,
            name: None,
            tool_call_id: None,
            tool_calls: calls,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: Some(content.into()),
            name: None,
            tool_call_id: Some(call_id.into()),
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub kind: String,
    pub function: ChatToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatToolCallFunction {
    pub name: String,
    pub arguments: String,
}

fn default_tool_call_type() -> String {
    "function".to_string()
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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parallel_tool_calls: Option<bool>,
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
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
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
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
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
        assert!(json.get("tools").is_none(), "empty tools must be omitted");
    }

    #[test]
    fn chat_request_with_tool_calls_round_trips() {
        let tool_call = ChatToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: ChatToolCallFunction {
                name: "lookup".into(),
                arguments: r#"{"q":"hello"}"#.into(),
            },
        };
        let req = ChatRequest::new(
            "gpt-4o",
            vec![
                ChatMessage::user("call lookup please"),
                ChatMessage::assistant_tool_calls(vec![tool_call.clone()]),
                ChatMessage::tool_result("call_1", "{\"answer\":\"hi\"}"),
            ],
        );
        let json = serde_json::to_value(&req).expect("serialize");
        let messages = json["messages"].as_array().expect("messages array");
        assert_eq!(messages[1]["role"], "assistant");
        assert!(messages[1].get("content").is_none(), "assistant tool-call message has no content");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "lookup");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
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
