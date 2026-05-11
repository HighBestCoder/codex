# codex-copilot-bridge

Run [Codex CLI](https://github.com/openai/codex) on top of GitHub Copilot.

Codex only speaks the OpenAI Responses API (`wire_api = "responses"`), while
Copilot's backend speaks the legacy Chat Completions API. This crate ships a
local reverse proxy that translates between the two so Codex can be pointed at
Copilot with **zero source changes inside Codex itself**.

## Components

| Crate | What it does |
|---|---|
| `codex-copilot-bridge` | Pure Rust library. Copilot auth (OAuth + session-token refresh), minimal chat-completions client, and a `Responses <-> chat/completions` translator. |
| `codex-copilot-bridge-server` | Binary `codex-copilot-proxy`. Local HTTP server (axum) that listens on `127.0.0.1:14318` by default, exposes `GET /healthz`, `GET /v1/models`, `POST /v1/responses` (both streaming SSE and non-streaming JSON), and forwards to Copilot via the bridge. |

## Usage

### 1. Provide Copilot credentials

Place the OAuth + session token JSON file at any of these locations
(first match wins):

- `$CODEX_COPILOT_AUTH_FILE` (path to file)
- `$CLAW_COPILOT_AUTH_FILE` (kept for compatibility with `claw-code` users)
- `~/.config/codex/copilot_auth.json`
- `~/.config/claw/copilot_auth.json`

Shape:

```json
{
  "github_oauth_token": "gho_...",
  "session_token": "tid=...;exp=...",
  "expires_at": 1778000000,
  "endpoint": "https://api.enterprise.githubcopilot.com"
}
```

If `session_token` is absent or expired, the proxy will refresh it
automatically against `https://api.github.com/copilot_internal/v2/token`
and persist the refreshed token back to the same file.

### 2. Start the proxy

```bash
cargo run -p codex-copilot-bridge-server --bin codex-copilot-proxy -- --port 14318
```

Or install the binary:

```bash
cargo install --path codex-rs/copilot-bridge-server
codex-copilot-proxy --port 14318
```

### 3. Point Codex at it

In `~/.codex/config.toml`:

```toml
model = "gpt-4o-mini"            # any model Copilot serves to your account
model_provider = "copilot"

[model_providers.copilot]
name = "GitHub Copilot (via local bridge)"
base_url = "http://127.0.0.1:14318/v1"
wire_api = "responses"
requires_openai_auth = false
env_key = "COPILOT_PROXY_DUMMY_KEY"
env_key_instructions = "Set to anything; the local proxy ignores it."
```

### 4. Run

```bash
COPILOT_PROXY_DUMMY_KEY=anything codex exec --skip-git-repo-check "Reply with HELLO"
```

## What the bridge translates

- `instructions` -> leading `system` message in chat
- `ResponseItem::Message` (user / assistant / `developer` -> system) round-trips with `input_text` / `output_text` content
- `ResponseItem::FunctionCall` -> assistant message with `tool_calls`
- `ResponseItem::FunctionCallOutput` -> tool-role message with `tool_call_id`
- Flat Responses-style function tools (`{type:"function", name, parameters}`) are rewritten to chat's nested shape (`{type:"function", function:{name, parameters}}`)
- Non-function tools (`local_shell`, `web_search`, `custom`, ...) are dropped before forwarding because Copilot's chat endpoint rejects them
- `tool_choice` is dropped when no compatible tools remain (Copilot returns 400 otherwise)
- The Responses SSE stream is synthesized from a single chat completion: `response.created` -> `response.output_item.done` for each output item -> `response.completed` with `id`, `usage`, `end_turn`

## What the bridge does **not** do yet

- True streaming (chat is awaited fully, then chunked into SSE events)
- `Reasoning` items - dropped on input, never produced on output
- Image inputs - `input_image` not forwarded
- WebSocket transport - HTTP only

## End-to-end validation

`cargo test -p codex-copilot-bridge -p codex-copilot-bridge-server`
covers schema round trips, tool normalization, SSE event ordering, and the
two real-world quirks discovered during integration:

- Copilot rejects `tool_choice` when `tools` is empty
- Codex sends function tools in flat shape (`name` at top level) - they must
  be re-nested before reaching Copilot

`examples/responses_hello.rs` exercises the library bridge end-to-end against
the real Copilot endpoint and prints the model's reply.
