use codex_call_graph_mcp::run_stdio_server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_stdio_server().await
}
