#[derive(Debug, thiserror::Error)]
pub enum CopilotError {
    #[error("missing GitHub Copilot credentials: {0}")]
    MissingCredentials(String),

    #[error("could not read auth file {path}: {source}")]
    AuthFileIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse auth file {path}: {source}")]
    AuthFileParse {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("HTTP error talking to GitHub Copilot: {0}")]
    Http(#[from] reqwest::Error),

    #[error("token refresh failed: HTTP {status}: {body}")]
    TokenRefresh { status: u16, body: String },

    #[error("chat completion failed: HTTP {status}: {body}")]
    ChatCompletion { status: u16, body: String },

    #[error("could not parse upstream JSON: {source}\n--- body ---\n{body}")]
    JsonParse {
        body: String,
        #[source]
        source: serde_json::Error,
    },
}

impl CopilotError {
    pub fn is_auth_problem(&self) -> bool {
        matches!(
            self,
            Self::MissingCredentials(_)
                | Self::AuthFileIo { .. }
                | Self::AuthFileParse { .. }
                | Self::TokenRefresh { .. }
        )
    }
}

pub type CopilotResult<T> = std::result::Result<T, CopilotError>;
