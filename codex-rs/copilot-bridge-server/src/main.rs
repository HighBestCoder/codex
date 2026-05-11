use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::Parser;
use codex_copilot_bridge::{CopilotAuth, CopilotError};
use codex_copilot_bridge_server::{build_state, serve};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "codex-copilot-proxy", version)]
#[command(about = "Local Responses-API proxy that forwards to GitHub Copilot")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1", env = "COPILOT_PROXY_HOST")]
    host: IpAddr,

    #[arg(long, default_value_t = 14318u16, env = "COPILOT_PROXY_PORT")]
    port: u16,

    #[arg(long, env = "CODEX_COPILOT_AUTH_FILE")]
    auth_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("codex-copilot-proxy: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let env_filter =
        EnvFilter::try_from_env("CODEX_COPILOT_PROXY_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    let auth = match cli.auth_file {
        Some(path) => CopilotAuth::from_auth_file(path).map_err(|e| e.to_string())?,
        None => match CopilotAuth::from_env() {
            Ok(a) => a,
            Err(CopilotError::MissingCredentials(msg)) => {
                eprintln!("missing copilot credentials: {msg}");
                std::process::exit(2);
            }
            Err(other) => return Err(other.to_string()),
        },
    };

    let state = build_state(auth).map_err(|e| e.to_string())?;
    let addr = SocketAddr::new(cli.host, cli.port);
    serve(addr, state).await.map_err(|e| e.to_string())?;
    Ok(())
}
