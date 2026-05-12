use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{CopilotError, CopilotResult};

pub const COPILOT_GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct OAuthTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub async fn start_device_code_flow() -> CopilotResult<DeviceCodeResponse> {
    let client = reqwest::Client::builder()
        .user_agent("codex-copilot-bridge")
        .build()?;
    let body = serde_json::json!({
        "client_id": COPILOT_GITHUB_CLIENT_ID,
        "scope": "read:user",
    });
    let response = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        return Err(CopilotError::TokenRefresh {
            status: status.as_u16(),
            body: text,
        });
    }
    serde_json::from_str(&text).map_err(|source| CopilotError::JsonParse { body: text, source })
}

pub async fn poll_device_code_for_oauth_token(
    device_code: &str,
    interval_secs: u64,
    overall_timeout: Duration,
) -> CopilotResult<String> {
    let client = reqwest::Client::builder()
        .user_agent("codex-copilot-bridge")
        .build()?;
    let started = Instant::now();
    loop {
        if started.elapsed() > overall_timeout {
            return Err(CopilotError::TokenRefresh {
                status: 408,
                body: "device-code authorization timed out before user completed it".to_string(),
            });
        }
        let body = serde_json::json!({
            "client_id": COPILOT_GITHUB_CLIENT_ID,
            "device_code": device_code,
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        });
        let response = client
            .post(ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await?;
        let text = response.text().await?;
        let parsed: OAuthTokenResponse =
            serde_json::from_str(&text).map_err(|source| CopilotError::JsonParse {
                body: text.clone(),
                source,
            })?;
        if let Some(token) = parsed.access_token {
            return Ok(token);
        }
        match parsed.error.as_deref() {
            Some("authorization_pending") | None => {
                tokio::time::sleep(Duration::from_secs(interval_secs.max(1))).await;
            }
            Some("slow_down") => {
                tokio::time::sleep(Duration::from_secs(interval_secs.max(1) + 5)).await;
            }
            Some(other) => {
                return Err(CopilotError::TokenRefresh {
                    status: 400,
                    body: format!("device-code error: {other} (body: {text})"),
                });
            }
        }
    }
}
