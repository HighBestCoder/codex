use crate::chat::{
    ChatChoiceMessage, ChatMessage, ChatRequest, ChatResponse, ChatRole, ChatToolCall,
    ChatToolCallFunction, CopilotChatClient,
};
use crate::error::CopilotResult;
use crate::responses::{
    ContentItem, FunctionCallOutputPayload, ResponseItem, ResponsesRequest, ResponsesResponse,
    ResponsesUsage,
};

pub fn responses_request_to_chat(req: &ResponsesRequest) -> ChatRequest {
    let mut messages = Vec::with_capacity(req.input.len() + 1);
    if !req.instructions.is_empty() {
        messages.push(ChatMessage::system(req.instructions.clone()));
    }
    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let text = collect_text(content);
                let chat_role = parse_role(role);
                if matches!(chat_role, ChatRole::Assistant) && text.is_empty() {
                    continue;
                }
                messages.push(ChatMessage {
                    role: chat_role,
                    content: Some(text),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                });
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                messages.push(ChatMessage::assistant_tool_calls(vec![ChatToolCall {
                    id: call_id.clone(),
                    kind: "function".into(),
                    function: ChatToolCallFunction {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                }]));
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                messages.push(ChatMessage::tool_result(call_id.clone(), output.content.clone()));
            }
            ResponseItem::Reasoning { .. } => {}
        }
    }

    let mut chat = ChatRequest::new(req.model.clone(), messages);
    chat.stream = Some(false);
    if !req.tools.is_empty() {
        chat.tools = req.tools.clone();
        if !req.tool_choice.is_empty() {
            chat.tool_choice = Some(serde_json::Value::String(req.tool_choice.clone()));
        }
        if req.parallel_tool_calls {
            chat.parallel_tool_calls = Some(true);
        }
    }
    chat
}

pub fn chat_response_to_responses(
    chat: &ChatResponse,
    model_fallback: &str,
) -> ResponsesResponse {
    let id = chat
        .id
        .clone()
        .unwrap_or_else(|| format!("resp_{}", short_random_id()));
    let model = chat.model.clone().unwrap_or_else(|| model_fallback.to_string());
    let mut output = Vec::new();
    for choice in &chat.choices {
        output.extend(choice_message_to_response_items(&choice.message));
    }
    let usage = chat.usage.as_ref().map(|u| ResponsesUsage {
        input_tokens: u.prompt_tokens.unwrap_or(0),
        output_tokens: u.completion_tokens.unwrap_or(0),
        total_tokens: u.total_tokens.unwrap_or(0),
    });
    ResponsesResponse {
        id,
        model,
        output,
        usage,
    }
}

fn choice_message_to_response_items(message: &ChatChoiceMessage) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    if !message.tool_calls.is_empty() {
        for tc in &message.tool_calls {
            items.push(ResponseItem::FunctionCall {
                id: None,
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
                call_id: tc.id.clone(),
            });
        }
    }
    if let Some(text) = message.content.as_deref() {
        if !text.is_empty() {
            items.push(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: text.to_string(),
                }],
            });
        }
    }
    items
}

fn collect_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(ContentItem::text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_role(role: &str) -> ChatRole {
    match role {
        "system" | "developer" => ChatRole::System,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        _ => ChatRole::User,
    }
}

fn short_random_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

#[derive(Debug, Clone)]
pub struct CopilotResponsesClient {
    chat: CopilotChatClient,
}

impl CopilotResponsesClient {
    pub fn new(chat: CopilotChatClient) -> Self {
        Self { chat }
    }

    pub fn into_inner(self) -> CopilotChatClient {
        self.chat
    }

    pub async fn create_response(
        &self,
        request: &ResponsesRequest,
    ) -> CopilotResult<ResponsesResponse> {
        let chat_req = responses_request_to_chat(request);
        let chat_resp = self.chat.chat_completion(&chat_req).await?;
        Ok(chat_response_to_responses(&chat_resp, &request.model))
    }
}

impl FunctionCallOutputPayload {
    pub fn from_str(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            success: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatChoice, ChatChoiceMessage, ChatResponse, ChatUsage};

    #[test]
    fn instructions_prepend_as_system_message() {
        let req = ResponsesRequest {
            model: "gpt-4o".into(),
            instructions: "be terse".into(),
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText {
                    text: "hi".into(),
                }],
            }],
            ..Default::default()
        };
        let chat = responses_request_to_chat(&req);
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, ChatRole::System);
        assert_eq!(chat.messages[0].content.as_deref(), Some("be terse"));
        assert_eq!(chat.messages[1].role, ChatRole::User);
        assert_eq!(chat.messages[1].content.as_deref(), Some("hi"));
        assert_eq!(chat.stream, Some(false));
    }

    #[test]
    fn function_call_item_becomes_assistant_tool_call_message() {
        let req = ResponsesRequest {
            model: "gpt-4o".into(),
            instructions: String::new(),
            input: vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".into(),
                    content: vec![ContentItem::InputText {
                        text: "look it up".into(),
                    }],
                },
                ResponseItem::FunctionCall {
                    id: None,
                    name: "lookup".into(),
                    arguments: r#"{"q":"hi"}"#.into(),
                    call_id: "call_42".into(),
                },
                ResponseItem::FunctionCallOutput {
                    call_id: "call_42".into(),
                    output: FunctionCallOutputPayload::from_str("ok"),
                },
            ],
            ..Default::default()
        };
        let chat = responses_request_to_chat(&req);
        assert_eq!(chat.messages.len(), 3);
        assert_eq!(chat.messages[0].role, ChatRole::User);
        assert_eq!(chat.messages[1].role, ChatRole::Assistant);
        assert!(chat.messages[1].content.is_none());
        assert_eq!(chat.messages[1].tool_calls.len(), 1);
        assert_eq!(chat.messages[1].tool_calls[0].id, "call_42");
        assert_eq!(chat.messages[1].tool_calls[0].function.name, "lookup");
        assert_eq!(chat.messages[2].role, ChatRole::Tool);
        assert_eq!(chat.messages[2].tool_call_id.as_deref(), Some("call_42"));
        assert_eq!(chat.messages[2].content.as_deref(), Some("ok"));
    }

    #[test]
    fn chat_assistant_text_becomes_message_output_item() {
        let chat = ChatResponse {
            id: Some("chatcmpl_1".into()),
            model: Some("gpt-4o".into()),
            choices: vec![ChatChoice {
                index: Some(0),
                message: ChatChoiceMessage {
                    role: ChatRole::Assistant,
                    content: Some("hello back".into()),
                    tool_calls: Vec::new(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(ChatUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(2),
                total_tokens: Some(12),
            }),
        };
        let resp = chat_response_to_responses(&chat, "gpt-4o");
        assert_eq!(resp.id, "chatcmpl_1");
        assert_eq!(resp.model, "gpt-4o");
        assert_eq!(resp.output.len(), 1);
        match &resp.output[0] {
            ResponseItem::Message { role, content, .. } => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                assert_eq!(content[0].text(), Some("hello back"));
            }
            other => panic!("expected Message, got {other:?}"),
        }
        let usage = resp.usage.expect("usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.total_tokens, 12);
    }

    #[test]
    fn chat_tool_calls_become_function_call_items() {
        let chat = ChatResponse {
            id: None,
            model: None,
            choices: vec![ChatChoice {
                index: Some(0),
                message: ChatChoiceMessage {
                    role: ChatRole::Assistant,
                    content: None,
                    tool_calls: vec![ChatToolCall {
                        id: "call_1".into(),
                        kind: "function".into(),
                        function: ChatToolCallFunction {
                            name: "lookup".into(),
                            arguments: r#"{"q":"x"}"#.into(),
                        },
                    }],
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };
        let resp = chat_response_to_responses(&chat, "gpt-4o");
        assert_eq!(resp.output.len(), 1);
        match &resp.output[0] {
            ResponseItem::FunctionCall {
                name,
                call_id,
                arguments,
                ..
            } => {
                assert_eq!(name, "lookup");
                assert_eq!(call_id, "call_1");
                assert_eq!(arguments, r#"{"q":"x"}"#);
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn chat_response_with_both_content_and_tool_calls_yields_both_items() {
        let chat = ChatResponse {
            id: None,
            model: None,
            choices: vec![ChatChoice {
                index: Some(0),
                message: ChatChoiceMessage {
                    role: ChatRole::Assistant,
                    content: Some("let me check".into()),
                    tool_calls: vec![ChatToolCall {
                        id: "call_1".into(),
                        kind: "function".into(),
                        function: ChatToolCallFunction {
                            name: "lookup".into(),
                            arguments: "{}".into(),
                        },
                    }],
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };
        let resp = chat_response_to_responses(&chat, "gpt-4o");
        assert_eq!(resp.output.len(), 2);
        assert!(matches!(resp.output[0], ResponseItem::FunctionCall { .. }));
        assert!(matches!(resp.output[1], ResponseItem::Message { .. }));
    }

    #[test]
    fn parses_developer_role_as_system() {
        let req = ResponsesRequest {
            model: "gpt-4o".into(),
            instructions: String::new(),
            input: vec![ResponseItem::Message {
                id: None,
                role: "developer".into(),
                content: vec![ContentItem::InputText {
                    text: "be terse".into(),
                }],
            }],
            ..Default::default()
        };
        let chat = responses_request_to_chat(&req);
        assert_eq!(chat.messages[0].role, ChatRole::System);
    }

    #[test]
    fn tool_choice_is_omitted_when_no_tools_are_provided() {
        let req = ResponsesRequest {
            model: "gpt-4o".into(),
            instructions: String::new(),
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText { text: "hi".into() }],
            }],
            tools: Vec::new(),
            tool_choice: "auto".into(),
            parallel_tool_calls: true,
            stream: false,
        };
        let chat = responses_request_to_chat(&req);
        assert!(chat.tools.is_empty());
        assert!(
            chat.tool_choice.is_none(),
            "tool_choice must be dropped when tools are empty (copilot rejects otherwise)"
        );
        assert!(chat.parallel_tool_calls.is_none());
    }

    #[test]
    fn tool_choice_and_parallel_pass_through_when_tools_present() {
        let req = ResponsesRequest {
            model: "gpt-4o".into(),
            instructions: String::new(),
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText { text: "hi".into() }],
            }],
            tools: vec![serde_json::json!({"type": "function", "function": {"name": "x"}})],
            tool_choice: "auto".into(),
            parallel_tool_calls: true,
            stream: false,
        };
        let chat = responses_request_to_chat(&req);
        assert_eq!(chat.tools.len(), 1);
        assert_eq!(
            chat.tool_choice.as_ref().and_then(|v| v.as_str()),
            Some("auto")
        );
        assert_eq!(chat.parallel_tool_calls, Some(true));
    }
}
