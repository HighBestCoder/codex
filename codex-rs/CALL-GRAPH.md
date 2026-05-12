# Codex Call-Graph and Embedded Copilot Architecture

This tree extends Codex with two tightly integrated capabilities:

1. **Embedded GitHub Copilot transport** inside the `codex` process
2. **Native in-process call-graph tools** and matching slash commands

The result is a single-binary workflow:

- no external Copilot proxy daemon;
- no call-graph MCP server;
- no extra localhost bridge process.

Everything in this document describes the **current integrated architecture**, not the earlier prototype path.

## Why this exists

Codex internally speaks the OpenAI Responses protocol. GitHub Copilot exposes chat/completions. This branch bridges that gap by implementing a custom `HttpTransport` that lives inside Codex itself.

At the same time, the branch adds a project-local call-graph engine and exposes it to Codex as native handlers rather than MCP tools. That lets the model use graph structure directly, while also allowing users to run graph actions manually through slash commands.

## High-level architecture

### Embedded Copilot path

When the configured provider name is `copilot` (or `github-copilot`), Codex does **not** use the normal reqwest transport path directly. Instead, `core/src/transport.rs` builds `CodexTransport::Copilot`, which wraps `codex_copilot_bridge::CopilotTransport`.

That transport:

- loads GitHub OAuth credentials from the local auth file or environment;
- refreshes the Copilot session token via `https://api.github.com/copilot_internal/v2/token`;
- translates Responses requests to Copilot chat/completions requests;
- returns normal Codex response/streaming data back to the rest of the app.

### Native call-graph path

The graph stack is fully in-process:

- `graph_map`
- `graph_why`
- `graph_plan`
- `graph_trace`

These are registered as native `ToolHandler`s in `codex-core`, and also exposed through TUI slash commands:

- `/graph-map`
- `/graph-why <symbol>`
- `/graph-plan <sym1> [sym2 ...]`
- `/graph-trace <program> [args ...]`

Slash commands execute the handler directly and write the result into conversation history without spending an extra model turn.

### Automatic graph-aware prompt decoration

After a project has been indexed, `core/src/call_graph_decorator.rs` inspects the latest user message, extracts likely identifiers, looks them up in the local graph store, and injects a compact graph summary into the model instructions.

If no graph exists yet for the current project, Codex appends a hint telling the user to run `/graph-map` first.

## Crates in this tree

| Crate | Role |
|---|---|
| `codex-copilot-bridge` | Copilot auth loading, device-flow support, session-token refresh, chat-completions client, Responses↔chat translation, and `CopilotTransport` (`impl HttpTransport`). |
| `codex-call-graph-store` | sled-backed embedded graph store. |
| `codex-call-graph-perception` | Rust/C/C++ parsing, SCIP enrichment, dynamic instrumentation support, git/worktree helpers. |
| `codex-call-graph-algo` | Graph algorithms, bounded loading, and `CallerIndexCache`. |
| `codex-call-graph-tools` | `run_map`, `run_why`, `run_plan`, `run_trace`, request/response types, and PGS path handling. |
| `codex-core` | Transport selection, graph-aware prompt decoration, native graph tool handlers, and registration logic. |

## Build and install

Build the single Codex binary from the Rust workspace:

```bash
cd codex-rs
cargo build -p codex-cli --bin codex --release
```

Optionally place the binary on `PATH`:

```bash
sudo ln -sf "$(pwd)/target/release/codex" /usr/local/bin/codex
```

## Copilot credentials and login

The preferred flow is now built into the CLI:

```bash
codex copilot login
codex copilot status
codex copilot logout
```

`codex copilot login` starts the GitHub device-code flow, stores the OAuth token locally, and lets the embedded transport refresh the session token on first use.

Credential resolution order:

1. `$CODEX_COPILOT_AUTH_FILE`
2. `$CLAW_COPILOT_AUTH_FILE`
3. `~/.config/codex/copilot_auth.json`
4. `~/.config/claw/copilot_auth.json`

Minimal stored auth shape:

```json
{
  "github_oauth_token": "gho_...",
  "session_token": "tid=...;exp=...",
  "expires_at": 1778000000,
  "endpoint": "https://api.enterprise.githubcopilot.com"
}
```

The session token and endpoint are optional on first login; the bridge fills them in when it refreshes the token.

## Recommended `~/.codex/config.toml`

Use a provider named `copilot`, and disable namespacing if you want the model to call `graph_*` directly by name:

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

Important clarifications:

- `name = "copilot"` is what triggers the embedded transport path.
- `namespace_tools = false` is a **recommended user setting** when you want the model to call `graph_map`, `graph_why`, and friends by their direct names.
- Internally, the tools configuration still defaults `call_graph_tools` to enabled.
- No `[mcp_servers.*]` section is required for the graph workflow.
- No dummy proxy environment variable is required.

## Daily use

### Interactive TUI workflow

Start Codex in your project:

```bash
codex --cd /absolute/path/to/your/project
```

Then run:

```text
/graph-map
/graph-why ValidateCollectionName
/graph-plan ValidateCollectionName Create Get
/graph-trace ./build/my_tests
```

### Exec workflow

Slash commands also work in exec mode:

```bash
codex exec --cd /absolute/path/to/your/project "/graph-map"
codex exec --cd /absolute/path/to/your/project "/graph-why ValidateCollectionName"
```

You can also rely on normal prompts and let the model decide whether to use graph tools:

```bash
codex exec --cd /absolute/path/to/your/project \
  "Use graph_map and graph_why to find every caller of ValidateCollectionName."
```

Because approvals and sandboxing are configurable through `config.toml`, you no longer need proxy-era shell wrappers or bypass flags just to make the integrated workflow usable.

## Native tool semantics

### `graph_map`

**Input**

```json
{ "project_root": "/absolute/path/to/project" }
```

**Behavior**

- detects Rust, CMake/C++, mixed, or unknown projects;
- picks the fast parser path for the detected kind;
- automatically attempts SCIP enrichment when `rust-analyzer` or `scip-clang` is available on `PATH`;
- writes results into a persistent local PGS directory;
- invalidates the caller index cache after writing.

**Output highlights**

- `project_kind`
- `files_seen`
- `files_indexed`
- `failures`
- `nodes_added`
- `edges_added`
- `scip_used`
- `scip_skipped_reason`
- `elapsed_ms`
- `pgs_path`

There is no separate `--refresh` mode to remember; re-running `graph_map` already performs the current indexing flow.

### `graph_why`

**Input**

```json
{ "project_root": "/absolute/path/to/project", "symbol": "ValidateCollectionName" }
```

**Behavior**

- resolves definitions by simple name;
- returns callers and callees for each match;
- includes edge provenance such as simple-name, SCIP, or dynamic trace edges.

### `graph_plan`

**Input**

```json
{ "project_root": "/absolute/path/to/project", "seed_symbols": ["ValidateCollectionName", "Create"] }
```

**Behavior**

- performs bounded loading instead of scanning the whole graph;
- uses cached caller indexes for large projects;
- returns a sorted subgraph, topological order, cycle signal, and unresolved callees.

### `graph_trace`

**Input**

```json
{ "project_root": "/absolute/path/to/project", "test_command": ["./build/my_tests"] }
```

**Behavior**

- requires an existing PGS and asks you to run `graph_map` first if none exists;
- builds the instrumentation runtime into `.codex-call-graph-cache/`;
- runs the command under `LD_PRELOAD` and `CLAW_TRACE_OUTPUT`;
- reconstructs dynamic edges and writes them back into the graph store.

For **CMake + git** projects, `graph_trace` also creates a temporary worktree, runs CMake configure/build with `-finstrument-functions` and `-rdynamic`, then executes the test command from that rebuilt tree. There is no separate `--rebuild` flag to pass.

## PGS storage path

The local graph store path is derived from the canonical project root:

- base directory: `$CODEX_HOME/call-graph/`
- fallback base directory when `CODEX_HOME` is unset: `~/.codex/call-graph/`
- final leaf: the first 8 bytes of the Blake3 hash of the canonical project root, rendered as hex

In code, this is implemented by `call-graph-tools/src/pgs_path.rs`.

## Source touchpoints inside codex

The main implementation entry points are:

- `core/src/transport.rs` — embedded transport selection via `build_transport_for_provider`
- `core/src/client.rs` — transport call sites now use `build_transport_for_provider(...)`
- `core/src/call_graph_decorator.rs` — automatic graph-aware instruction injection
- `core/src/tools/handlers/call_graph_spec.rs` — tool specs for the four graph tools
- `core/src/tools/handlers/call_graph.rs` — tool handler implementations
- `core/src/tools/spec_plan.rs` — graph handler registration when `call_graph_tools` is enabled
- `tools/src/tool_config.rs` — `call_graph_tools: true` by default
- `tui/src/slash_command.rs` — `/graph-*` command registration and UX strings
- `tui/src/chatwidget/slash_dispatch.rs` — direct slash-command dispatch into graph handlers
- `cli/src/main.rs` — `codex copilot login/logout/status`

## Known limits

- **Cargo-specific dynamic trace rebuild path**: the automatic rebuild path currently targets CMake + git projects. Cargo projects still need the right compiler/instrumentation setup outside this helper path.
- **SCIP rerun policy**: `graph_map` attempts SCIP enrichment whenever the relevant indexer is on `PATH`; it does not yet do stale-cache detection.
- **Slash output formatting**: `/graph-plan` and `/graph-trace` still favor concise summaries over rich pretty-printing in the chat history.

## Related docs

- Top-level usage guide: [`../README.md`](../README.md)
- Rust workspace overview: [`README.md`](./README.md)
