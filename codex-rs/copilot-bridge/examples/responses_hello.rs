use std::env;

use codex_copilot_bridge::{
    ContentItem, CopilotAuth, CopilotChatClient, CopilotError, CopilotResponsesClient,
    CopilotResult, ResponseItem, ResponsesRequest,
};

#[tokio::main]
async fn main() -> CopilotResult<()> {
    let auth = match CopilotAuth::from_env() {
        Ok(a) => a,
        Err(CopilotError::MissingCredentials(msg)) => {
            eprintln!("missing copilot credentials: {msg}");
            std::process::exit(2);
        }
        Err(other) => return Err(other),
    };

    let chat = CopilotChatClient::new(auth)?;
    let client = CopilotResponsesClient::new(chat);

    let model = env::var("COPILOT_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let prompt = env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with exactly: PONG".to_string());

    let request = ResponsesRequest {
        model: model.clone(),
        instructions: "You are a terse assistant. Reply in one short sentence.".into(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: prompt }],
        }],
        tools: Vec::new(),
        tool_choice: "auto".into(),
        parallel_tool_calls: false,
        stream: false,
    };

    let response = client.create_response(&request).await?;

    eprintln!("[responses] id={} model={}", response.id, response.model);
    for item in &response.output {
        match item {
            ResponseItem::Message { content, .. } => {
                for piece in content {
                    if let Some(text) = piece.text() {
                        println!("{text}");
                    }
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                eprintln!(
                    "[function_call] {name} call_id={call_id} args={arguments}"
                );
            }
            other => eprintln!("[output] {other:?}"),
        }
    }
    if let Some(usage) = response.usage.as_ref() {
        eprintln!(
            "[usage] input={} output={} total={}",
            usage.input_tokens, usage.output_tokens, usage.total_tokens
        );
    }
    Ok(())
}
