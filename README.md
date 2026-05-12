# Codex with Embedded GitHub Copilot and Native Call-Graph Tools

This branch turns Codex into a **GitHub Copilot-first local coding agent** with **native in-process call-graph tools**.

There is **no MCP server** for the graph workflow and **no external Copilot proxy daemon**. The `codex` binary now contains:

- an embedded Copilot transport that translates Codex Responses traffic to GitHub Copilot chat/completions in-process;
- four native call-graph tools: `graph_map`, `graph_why`, `graph_plan`, `graph_trace`;
- matching TUI slash commands: `/graph-map`, `/graph-why`, `/graph-plan`, `/graph-trace`;
- automatic graph-aware prompt decoration after a project has been indexed.

If you want architecture and implementation details, start with [`codex-rs/CALL-GRAPH.md`](./codex-rs/CALL-GRAPH.md). This README focuses on **how to use the workflow day to day**.

## Quick start

```bash
cd codex-rs
cargo build -p codex-cli --bin codex --release
codex copilot login
codex --cd /absolute/path/to/your/project
```

Then inside the TUI:

```text
/graph-map
```

After that, you can either keep chatting normally or use `/graph-why`, `/graph-plan`, and `/graph-trace` directly.

## What is new on this branch

### 1. Embedded GitHub Copilot transport

When your active provider is named `copilot`, Codex switches from the normal reqwest HTTP path to the embedded `CopilotTransport` inside the process.

- No localhost proxy
- No extra bridge server to manage
- No OpenAI API key required for the Copilot path

### 2. Native call-graph tools

These tools are registered as native Codex handlers, not MCP tools:

- `graph_map` — build or refresh the project call graph
- `graph_why` — show callers and callees for a symbol
- `graph_plan` — build a bounded subgraph plan from one or more seed symbols
- `graph_trace` — collect dynamic edges by running an instrumented test command

### 3. Native slash commands

Inside the TUI, these commands run directly against the in-process handlers:

- `/graph-map`
- `/graph-why <symbol>`
- `/graph-plan <sym1> [sym2 ...]`
- `/graph-trace <program> [args ...]`

They do **not** spend an extra LLM round trip.

### 4. Automatic graph-aware behavior

After you run `/graph-map` for a project, Codex can automatically inject relevant call-graph context into later prompts. That means ordinary prompts like “who calls `ValidateCollectionName`?” can already be grounded by the indexed graph, even before you explicitly call `graph_why` or `graph_plan`.

If no graph exists yet, Codex adds a hint telling you to run `/graph-map` first.

## Build and install

Build from the Rust workspace:

```bash
cd codex-rs
cargo build -p codex-cli --bin codex --release
```

Optionally place the binary on your `PATH`:

```bash
sudo ln -sf "$(pwd)/target/release/codex" /usr/local/bin/codex
```

## GitHub Copilot login

This branch adds built-in Copilot credential management:

```bash
codex copilot login
codex copilot status
codex copilot logout
```

### What `codex copilot login` does

`codex copilot login` starts a GitHub device-code flow, asks you to open a verification URL, and stores the resulting OAuth token locally.

Default credential lookup order:

1. `$CODEX_COPILOT_AUTH_FILE`
2. `$CLAW_COPILOT_AUTH_FILE`
3. `~/.config/codex/copilot_auth.json`
4. `~/.config/claw/copilot_auth.json`

The session token is refreshed automatically on first use and persisted back to the auth file.

This makes the binary shareable: another developer can use the same binary and run `codex copilot login` with their own GitHub account.

## Recommended `~/.codex/config.toml`

Use a Copilot-backed provider and keep tools un-namespaced so `graph_*` is directly available to the model:

```toml
model = "claude-opus-4.7-1m-internal"
model_provider = "copilot"
model_context_window = 1000000
model_auto_compact_token_limit = 900000
approval_policy = "never"
sandbox_mode = "danger-full-access"
namespace_tools = false

[model_providers.copilot]
name = "copilot"
base_url = "https://api.enterprise.githubcopilot.com"
wire_api = "responses"
requires_openai_auth = false

[projects."/absolute/path/to/your/project"]
trust_level = "trusted"
```

Important notes:

- `name = "copilot"` is what triggers the embedded Copilot transport.
- `namespace_tools = false` is the recommended user setting when you want the model to call `graph_*` directly.
- You do **not** need an MCP graph server.
- You do **not** need a dummy proxy env var.

## Basic workflow

### 1. Start Codex in your project

```bash
codex --cd /absolute/path/to/your/project
```

Or use `exec` mode:

```bash
codex exec --cd /absolute/path/to/your/project "Map this repo and explain the main call path for FooBar"
```

### 2. Build the graph once

In the TUI:

```text
/graph-map
```

This indexes the current project into a local PGS store.

`graph_map` already does the aggressive defaults for you:

- auto-detects Rust / C / C++ / mixed projects;
- uses the fast parser path for the detected project kind;
- automatically runs SCIP enrichment when `rust-analyzer` or `scip-clang` is available on `PATH`.

There is no extra `--refresh` flag you need to remember.

### 3. Ask graph questions

Examples in the TUI:

```text
/graph-why ValidateCollectionName
/graph-plan ValidateCollectionName Create Get
/graph-trace ./build/my_tests
```

Examples in `exec` mode:

```bash
codex exec --cd /absolute/path/to/your/project "/graph-map"
codex exec --cd /absolute/path/to/your/project "/graph-why ValidateCollectionName"
```

### 4. Keep chatting normally

After the graph exists, ordinary prompts can benefit from automatic graph context injection.

Examples:

- “Who calls `ValidateCollectionName`?”
- “What functions are likely affected if I change `CreateCollection`?”
- “Plan the safest order to refactor these three entry points.”

For deeper answers, Codex can still call `graph_why` or `graph_plan` explicitly.

## What to use when

- Use `/graph-map` when you enter a repository and want Codex to understand structure.
- Use `/graph-why <symbol>` when you want callers/callees for one symbol.
- Use `/graph-plan <sym1> [sym2 ...]` when you want impact analysis or refactor order.
- Use `/graph-trace <program> [args ...]` when static analysis misses runtime-only edges.
- Use normal prompts after mapping if you want Codex to reason with graph context automatically.

## Tool behavior summary

### `graph_map`

- Input: current project root
- Output: project kind, files seen/indexed, node/edge counts, PGS path, timing, SCIP status
- Best for: creating or refreshing the local graph before structural analysis

### `graph_why`

- Input: project root + symbol name
- Output: matching definitions plus callers/callees
- Best for: “who calls this?” or “what does this call?”

### `graph_plan`

- Input: project root + 1..N seed symbols
- Output: bounded subgraph, topological order, cycle detection, unresolved callees
- Best for: refactor planning and impact analysis

### `graph_trace`

- Input: project root + test command
- Output: dynamic call edges reconstructed from runtime events
- Best for: filling in runtime-only edges missing from static analysis

For CMake + git projects, `graph_trace` automatically creates a temporary worktree, rebuilds with instrumentation flags, and runs the test command there. There is no extra `--rebuild` flag to pass.

## Where graph data lives

Codex stores project-local call-graph state under `CODEX_HOME/call-graph/` or, by default, `~/.codex/call-graph/`, using a stable hash of the canonical project root. The `graph_map` response prints the exact `pgs_path` it used.

## Common issues

### Model metadata warning for `claude-opus-4.7-1m-internal`

If you see a warning that metadata for `claude-opus-4.7-1m-internal` was not found, the model can still run with explicit config overrides such as:

- `model_context_window = 1000000`
- `model_auto_compact_token_limit = 900000`

That warning is about catalog metadata fallback, not about the embedded Copilot transport itself.

### Copilot credentials not found

Run:

```bash
codex copilot login
codex copilot status
```

If needed, set `CODEX_COPILOT_AUTH_FILE` to point at a specific auth JSON file.

### No graph context appears in normal prompts

Make sure you have indexed the project first:

```text
/graph-map
```

The automatic graph-aware prompt decoration only kicks in after a PGS exists for the current project.

## Developer-facing source entry points

If you want to inspect or extend the implementation, start here:

- `codex-rs/core/src/transport.rs` — embedded Copilot transport selection
- `codex-rs/cli/src/main.rs` — `codex copilot login/logout/status`
- `codex-rs/core/src/call_graph_decorator.rs` — automatic graph context injection
- `codex-rs/core/src/tools/handlers/call_graph.rs` — native graph tool handlers
- `codex-rs/tui/src/slash_command.rs` — `/graph-*` command registration
- `codex-rs/tui/src/chatwidget/slash_dispatch.rs` — direct slash-command execution
- `codex-rs/call-graph-tools/src/` — graph tool implementations

## More documentation

- [Implementation notes: `codex-rs/CALL-GRAPH.md`](./codex-rs/CALL-GRAPH.md)
- [Rust workspace overview: `codex-rs/README.md`](./codex-rs/README.md)
- [Install/build docs](./docs/install.md)
