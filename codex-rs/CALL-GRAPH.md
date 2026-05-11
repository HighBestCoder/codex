# Codex Call-Graph Tools

Build a project-local call graph, expose it to the model as **native
in-process tools**, and drive Codex through GitHub Copilot with a fully
embedded chat<->responses bridge (no separate proxy process).

## Why this exists

Codex's default model providers only speak the OpenAI Responses API.
GitHub Copilot speaks chat/completions. This tree ships:

1. A **Copilot bridge** that lives inside `codex` itself as a custom
   `HttpTransport`. When the active provider is `copilot`, codex
   short-circuits its reqwest path and routes Responses requests
   through `CopilotTransport`, which translates them to chat/completions
   and back. No localhost daemon required.
2. A **call-graph engine** (store / perception / algo) that can index
   Rust, C, and C++ projects.
3. **Four native Codex tools** (`graph_map`, `graph_why`,
   `graph_plan`, `graph_trace`) registered as in-process
   `ToolHandler`s next to `ApplyPatchHandler`, plus matching slash
   commands (`/graph-map` etc.).

Zero MCP child process, zero HTTP proxy daemon; everything runs inside
the `codex` binary itself.

## Crates in this tree

| Crate | Role |
|---|---|
| `codex-copilot-bridge` | OAuth + session-token refresh, chat-completions client, Responses<->chat translation, **`CopilotTransport` (`impl HttpTransport`)**. No `codex-*` deps. |
| `codex-call-graph-store` | sled-backed embedded graph DB. Schema is byte-for-byte compatible with claw-pgs. |
| `codex-call-graph-perception` | Source parsers: syn (Rust), tree-sitter (C/C++), SCIP (rust-analyzer / scip-clang), dynamic finstrument runtime + event reconstruction, git history. |
| `codex-call-graph-algo` | petgraph-backed algorithms: SCC, topo sort, PageRank, BFS, Steiner subgraph, bounded loader, `CallerIndexCache`. |
| `codex-call-graph-tools` | Pure `run_map` / `run_why` / `run_plan` / `run_trace` entry points + request/response types. |
| `codex-core` (extended) | Hosts the four `Graph*Handler` impls plus the `CodexTransport` enum (`Reqwest \| Copilot`) and the `build_transport_for_provider` factory. |

## One-time setup

### 1. Build

```bash
cd codex-rs
cargo build -p codex-cli --bin codex
```

That's the only binary. No proxy server.

### 2. Put the binary on PATH

```bash
sudo ln -sf $(pwd)/target/release/codex /usr/local/bin/codex
```

### 3. Provide Copilot credentials

Drop the OAuth JSON at any of these locations (first match wins):

- `$CODEX_COPILOT_AUTH_FILE`
- `$CLAW_COPILOT_AUTH_FILE`
- `~/.config/codex/copilot_auth.json`
- `~/.config/claw/copilot_auth.json`

```json
{
  "github_oauth_token": "gho_...",
  "session_token": "tid=...;exp=...",
  "expires_at": 1778000000,
  "endpoint": "https://api.enterprise.githubcopilot.com"
}
```

The bridge auto-refreshes the session token against
`https://api.github.com/copilot_internal/v2/token` when needed.

### 4. `~/.codex/config.toml`

```toml
model = "claude-opus-4.7-1m-internal"
model_provider = "copilot"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.copilot]
name = "copilot"
base_url = "https://api.enterprise.githubcopilot.com"
wire_api = "responses"
namespace_tools = false
requires_openai_auth = false
env_key = "COPILOT_PROXY_DUMMY_KEY"

[projects."/path/to/your/project"]
trust_level = "trusted"
```

Key configuration choices, all defaults:

- `name = "copilot"` -- triggers the embedded `CopilotTransport`.
- `approval_policy = "never"` + `sandbox_mode = "danger-full-access"`
  -- so the model can run tools and shell without prompting.
- `namespace_tools = false` -- exposes `graph_*` to the model directly
  rather than behind `tool_search` namespacing.
- No `[mcp_servers.*]` section needed; the graph tools are native
  handlers.
- No proxy / no localhost daemon -- `base_url` points at the real
  Copilot endpoint but the embedded transport intercepts requests
  before they leave the process.

## Daily use

Two ways to drive the tools.

**(a) Free-form prompt** (let the model decide):
```bash
COPILOT_PROXY_DUMMY_KEY=anything codex exec --skip-git-repo-check \
  --cd /path/to/your/project \
  "Use graph_map then graph_why to find every caller of foo()."
```

`--dangerously-bypass-approvals-and-sandbox` is unnecessary now that
the config sets `approval_policy = "never"` + `sandbox_mode =
"danger-full-access"`.

**(b) Slash commands** (in `codex` interactive TUI):
```
/graph-map
/graph-why ValidateCollectionName
/graph-plan ValidateCollectionName Create Get
/graph-trace ./build/my_tests
```
Slash commands execute the in-process handler directly and print the
result into conversation history -- no LLM round trip.

**(c) Direct exec command** (no TUI):
```bash
codex exec --cd /path/to/your/project \
  "/graph-map"  # works because slash commands also fire in exec mode
```

## The four native tools

### `graph_map`

| Field | Notes |
|---|---|
| Input  | `{ project_root: absolute path }` |
| Detect | Cargo.toml / compile_commands.json / CMakeLists.txt / extension scan → ProjectKind |
| Parse  | Rust syn fast path, tree-sitter for C/C++, Mixed walks each part, Unknown tries Rust then C++ |
| Output | `project_kind`, `files_seen`, `files_indexed`, `failures`, `nodes_added`, `edges_added`, `elapsed_ms`, `pgs_path` |

The PGS lands at `$CODEX_HOME/call-graph/<8-byte blake3 hash of the
canonical project_root>/`.

### `graph_why`

| Field | Notes |
|---|---|
| Input  | `{ project_root, symbol, max_callers?, max_callees? }` |
| Output | For each definition that matches the simple name: `file`, `line`, callers + callees with their `source` (`simple_name` / `scip_internal` / `scip_external` / `dynamic`) and `confidence` |

Simple-name caller lookup falls back to scanning `iter_out_edges`,
which keeps caller lists populated even on tree-sitter-only projects.

### `graph_plan`

| Field | Notes |
|---|---|
| Input  | `{ project_root, seed_symbols: 1..32, max_path_depth?, max_subgraph_size? }` |
| Output | `seeds_resolved`, `seeds_unresolved`, `subgraph` (sorted by file/line), `topo_order`, `cycle_detected`, `unresolved_callees` (capped at 50) |

Uses `LoadedGraph::from_pgs_bounded` so the algorithm never reads the
entire call graph; walks `max_path_depth` BFS hops from the seeds.

### `graph_trace`

| Field | Notes |
|---|---|
| Input  | `{ project_root, test_command: 1..64 args }` |
| Output | `runtime_so`, `events_path`, `event_count`, `unique_edges`, `edges_written`, `test_exit_code`, `elapsed_ms` |

Builds `libclaw_finstrument.so` once into
`$project_root/.codex-call-graph-cache/`, runs the test command under
`LD_PRELOAD` + `CLAW_TRACE_OUTPUT`, demangles Itanium symbols, then
upserts the observed edges into the PGS under the synthetic file
`<dynamic-trace>` with `EdgeSource::Dynamic`.

Caller must build the test binary with `-finstrument-functions` and
link with `-rdynamic` so `dladdr` resolves symbol names.

## Validated workflows

Real measurements on this host:

| Scenario | Time | Notes |
|---|---|---|
| `/tmp/codex-graph-demo` 2-file Rust map+why | 16 s | Native handlers; was 30-50 s through MCP |
| `cortex.core` 530-file C++ map+why | 37 s | Tree-sitter index + caller lookup |
| `cortex.core` graph_plan(single seed) | 83 s | Bounded BFS over ~100k edges |

## Source touchpoints inside codex

Stages 6 + 7 touch these files inside the `codex/` workspace:

- `model-provider-info/src/lib.rs` — `namespace_tools` field.
- `model-provider/src/provider.rs` — `ConfiguredModelProvider::capabilities()` reads it.
- `core/src/tools/handlers/call_graph_spec.rs` — 4 `ToolSpec` builders.
- `core/src/tools/handlers/call_graph.rs` — 4 `ToolHandler` impls.
- `core/src/tools/handlers/mod.rs` — module + re-exports.
- `core/src/tools/spec_plan.rs` — `if config.call_graph_tools { register all four }`.
- `core/src/transport.rs` — `CodexTransport` enum + `build_transport_for_provider` factory.
- `core/src/lib.rs` — `mod transport;`.
- `core/src/client.rs` — 4 call sites swapped from `ReqwestTransport::new(...)` to `build_transport_for_provider(...)`.
- `tools/src/tool_config.rs` — `call_graph_tools: bool` (default true) + `with_call_graph_tools` builder.
- `tui/src/slash_command.rs` — 4 new variants with descriptions / inline-args / available-during-task entries.
- `tui/src/chatwidget/slash_dispatch.rs` — dispatch arms that execute the in-process handler directly.

## Known limits / follow-ups

- **`graph_trace` for cargo projects**: today only CMake + git projects
  get the auto-rebuild-in-worktree path; cargo projects still need the
  caller to inject the right rustc flags manually.
- **scip-clang / rust-analyzer refresh policy**: `graph_map` runs SCIP
  every call when the binary is on PATH. No "stale-cache" check yet, so
  a re-map after a small change re-spawns scip-clang.
- **Slash result formatting**: `/graph-plan` and `/graph-trace` print a
  one-line summary; deep output (e.g. the full subgraph node list) is
  not yet pretty-printed in the TUI history.
