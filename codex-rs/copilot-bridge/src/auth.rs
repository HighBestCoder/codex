use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{CopilotError, CopilotResult};

pub const COPILOT_DEFAULT_ENDPOINT: &str = "https://api.enterprise.githubcopilot.com";
pub const COPILOT_EDITOR_VERSION: &str = "vscode/1.95.0";
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
pub const COPILOT_TOKEN_REGISTRY_URL: &str = "https://api.github.com/copilot_internal/v2/token";

const SESSION_TOKEN_REFRESH_LEAD_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAuth {
    pub github_oauth_token: String,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
    #[serde(default)]
    endpoints: Option<CopilotEndpoints>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotEndpoints {
    #[serde(default)]
    api: Option<String>,
}

#[derive(Debug)]
struct AuthState {
    github_oauth_token: String,
    session_token: Option<String>,
    expires_at: u64,
    endpoint: String,
    auth_path: Option<PathBuf>,
}

impl AuthState {
    fn is_session_token_fresh(&self) -> bool {
        if self.session_token.is_none() {
            return false;
        }
        let now = unix_now();
        self.expires_at > now + SESSION_TOKEN_REFRESH_LEAD_SECS
    }

    fn persist(&self) {
        let Some(path) = &self.auth_path else { return };
        let stored = StoredAuth {
            github_oauth_token: self.github_oauth_token.clone(),
            session_token: self.session_token.clone(),
            expires_at: Some(self.expires_at),
            endpoint: Some(self.endpoint.clone()),
        };
        if let Ok(json) = serde_json::to_string_pretty(&stored) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, json);
        }
    }
}

#[derive(Debug, Clone)]
pub struct CopilotAuth {
    state: Arc<RwLock<AuthState>>,
    http: reqwest::Client,
}

impl CopilotAuth {
    pub fn from_stored(stored: StoredAuth, auth_path: Option<PathBuf>) -> CopilotResult<Self> {
        let endpoint = stored
            .endpoint
            .unwrap_or_else(|| COPILOT_DEFAULT_ENDPOINT.to_string());
        let state = AuthState {
            github_oauth_token: stored.github_oauth_token,
            session_token: stored.session_token,
            expires_at: stored.expires_at.unwrap_or(0),
            endpoint,
            auth_path,
        };
        let http = reqwest::Client::builder()
            .user_agent("codex-copilot-bridge/0.0.0")
            .build()?;
        Ok(Self {
            state: Arc::new(RwLock::new(state)),
            http,
        })
    }

    pub fn from_auth_file(path: PathBuf) -> CopilotResult<Self> {
        let raw = std::fs::read_to_string(&path).map_err(|source| CopilotError::AuthFileIo {
            path: path.clone(),
            source,
        })?;
        let stored: StoredAuth =
            serde_json::from_str(&raw).map_err(|source| CopilotError::AuthFileParse {
                path: path.clone(),
                source,
            })?;
        Self::from_stored(stored, Some(path))
    }

    pub fn from_env() -> CopilotResult<Self> {
        if let Ok(path) = std::env::var("CODEX_COPILOT_AUTH_FILE") {
            return Self::from_auth_file(PathBuf::from(path));
        }
        if let Ok(path) = std::env::var("CLAW_COPILOT_AUTH_FILE") {
            return Self::from_auth_file(PathBuf::from(path));
        }
        for candidate in default_auth_candidates() {
            if candidate.exists() {
                return Self::from_auth_file(candidate);
            }
        }
        Err(CopilotError::MissingCredentials(
            "set CODEX_COPILOT_AUTH_FILE or place credentials at \
             ~/.config/codex/copilot_auth.json or ~/.config/claw/copilot_auth.json"
                .to_string(),
        ))
    }

    pub async fn ensure_session_token(&self) -> CopilotResult<(String, String)> {
        {
            let read = self.state.read().await;
            if read.is_session_token_fresh() {
                if let Some(token) = &read.session_token {
                    return Ok((token.clone(), read.endpoint.clone()));
                }
            }
        }
        self.refresh_session_token().await
    }

    async fn refresh_session_token(&self) -> CopilotResult<(String, String)> {
        let mut write = self.state.write().await;
        if write.is_session_token_fresh() {
            if let Some(token) = &write.session_token {
                return Ok((token.clone(), write.endpoint.clone()));
            }
        }

        let response = self
            .http
            .get(COPILOT_TOKEN_REGISTRY_URL)
            .header(
                "Authorization",
                format!("token {}", write.github_oauth_token),
            )
            .header("User-Agent", "codex-copilot-bridge/0.0.0")
            .header("Editor-Version", COPILOT_EDITOR_VERSION)
            .header("Editor-Plugin-Version", "copilot-chat/0.22.0")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(CopilotError::TokenRefresh {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: CopilotTokenResponse =
            serde_json::from_str(&body).map_err(|source| CopilotError::JsonParse {
                body: body.clone(),
                source,
            })?;
        write.session_token = Some(parsed.token.clone());
        write.expires_at = parsed.expires_at;
        if let Some(api) = parsed.endpoints.and_then(|e| e.api) {
            write.endpoint = api;
        }
        write.persist();
        Ok((parsed.token, write.endpoint.clone()))
    }

    pub async fn snapshot_endpoint(&self) -> String {
        self.state.read().await.endpoint.clone()
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn default_auth_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        out.push(home.join(".config").join("codex").join("copilot_auth.json"));
        out.push(home.join(".config").join("claw").join("copilot_auth.json"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn from_auth_file_round_trips_minimal_oauth_only_record() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(&path, r#"{"github_oauth_token":"gho_xxx"}"#).expect("write");
        let auth = CopilotAuth::from_auth_file(path).expect("auth");
        let endpoint = futures_blocking(auth.snapshot_endpoint());
        assert_eq!(endpoint, COPILOT_DEFAULT_ENDPOINT);
    }

    #[test]
    fn from_auth_file_reports_io_error_with_path() {
        let path = PathBuf::from("/nonexistent/codex-copilot/auth.json");
        let err = CopilotAuth::from_auth_file(path.clone()).expect_err("should fail");
        match err {
            CopilotError::AuthFileIo { path: p, .. } => assert_eq!(p, path),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn from_auth_file_reports_parse_error_with_path() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(&path, "{not json").expect("write");
        let err = CopilotAuth::from_auth_file(path.clone()).expect_err("should fail");
        match err {
            CopilotError::AuthFileParse { path: p, .. } => assert_eq!(p, path),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn from_env_returns_missing_credentials_with_actionable_hint() {
        let _guard = scoped_env(&[
            ("CODEX_COPILOT_AUTH_FILE", None),
            ("CLAW_COPILOT_AUTH_FILE", None),
            ("HOME", Some("/tmp/codex-copilot-bridge-missing")),
        ]);
        let err = CopilotAuth::from_env().expect_err("should fail");
        match err {
            CopilotError::MissingCredentials(msg) => {
                assert!(msg.contains("CODEX_COPILOT_AUTH_FILE"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn futures_blocking<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(fut)
    }

    struct ScopedEnv {
        previous: Vec<(String, Option<String>)>,
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            for (key, val) in self.previous.drain(..) {
                match val {
                    Some(v) => unsafe { std::env::set_var(&key, v) },
                    None => unsafe { std::env::remove_var(&key) },
                }
            }
        }
    }

    fn scoped_env(items: &[(&str, Option<&str>)]) -> ScopedEnv {
        let mut previous = Vec::new();
        for (key, value) in items {
            previous.push(((*key).to_string(), std::env::var(key).ok()));
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        ScopedEnv { previous }
    }
}
