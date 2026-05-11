use std::env;

use codex_copilot_bridge::{
    ChatMessage, ChatRequest, CopilotAuth, CopilotChatClient, CopilotError, CopilotResult,
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

    let client = CopilotChatClient::new(auth)?;
    let model = env::var("COPILOT_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let prompt = env::args().nth(1).unwrap_or_else(|| {
        "Say 'hello from copilot bridge' in five words or fewer.".to_string()
    });

    let request = ChatRequest::new(
        model,
        vec![
            ChatMessage::system("You are a terse assistant. Reply in one short sentence."),
            ChatMessage::user(prompt),
        ],
    );
    let response = client.chat_completion(&request).await?;
    let text = response
        .choices
        .first()
        .and_then(|c| c.message.content.as_deref())
        .unwrap_or("<no content>");
    println!("{text}");
    if let Some(usage) = response.usage.as_ref() {
        eprintln!(
            "[usage] prompt={:?} completion={:?} total={:?}",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }
    Ok(())
}
