# Codex Call-Graph Tools

Build a project-local call graph, expose it to the model as **native
in-process tools**, and (optionally) drive Codex through GitHub Copilot
using a local chat-to-responses proxy.

## Why this exists

Codex's default model providers only speak the OpenAI Responses API.
GitHub Copilot speaks chat/completions. This tree ships:

1. A **Copilot bridge** (proxy) that lets Codex run unchanged on
   Copilot-served models.
2. A **call-graph engine** (store / perception / algo) that can index
   Rust, C, and C++ projects.
3. **Four native Codex tools** (`graph_map`, `graph_why`,
   `graph_plan`, `graph_trace`) registered as in-process
   `ToolHandler`s next to `ApplyPatchHandler`, plus matching slash
   commands (`/graph-map` etc.).

Zero MCP child process; everything runs inside `codex` itself.

## Crates in this tree

| Crate | Role |
|---|---|
| `codex-copilot-bridge` | Standalone library: OAuth + session-token refresh, chat-completions client, Responses<->chat translation. No `codex-*` dependencies. |
| `codex-copilot-bridge-server` | Binary `codex-copilot-proxy`. axum HTTP server that owns `/healthz`, `/v1/models`, `/v1/responses`. |
| `codex-call-graph-store` | sled-backed embedded graph DB. Schema is byte-for-byte compatible with claw-pgs. |
| `codex-call-graph-perception` | Source parsers: syn (Rust), tree-sitter (C/C++), SCIP (rust-analyzer / scip-clang), dynamic finstrument runtime + event reconstruction, git history. |
| `codex-call-graph-algo` | petgraph-backed algorithms: SCC, topo sort, PageRank, BFS, Steiner subgraph, bounded loader. |
| `codex-call-graph-tools` | Pure `run_map` / `run_why` / `run_plan` / `run_trace` entry points + request/response types. |
| `codex-core` (extended) | Hosts the four `GraphMapHandler` / `GraphWhyHandler` / `GraphPlanHandler` / `GraphTraceHandler` impls plus their `ToolSpec` builders under `core/src/tools/handlers/call_graph{,_spec}.rs`. |

## One-time setup

### 1. Build

```bash
cd codex-rs
cargo build -p codex-cli                   --bin codex
cargo build -p codex-copilot-bridge-server --bin codex-copilot-proxy
```

(No third binary needed; the four graph tools live inside `codex`.)

### 2. Put the two binaries on PATH

```bash
sudo ln -sf $(pwd)/target/debug/codex                  /usr/local/bin/codex
sudo ln -sf $(pwd)/target/debug/codex-copilot-proxy    /usr/local/bin/codex-copilot-proxy
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

### 4. `~/.codex/config.toml`

```toml
model = "claude-opus-4.7-1m-internal"
model_provider = "copilot"

[model_providers.copilot]
name = "GitHub Copilot (via local bridge)"
base_url = "http://127.0.0.1:14318/v1"
wire_api = "responses"
namespace_tools = false
requires_openai_auth = false
env_key = "COPILOT_PROXY_DUMMY_KEY"

[projects."/path/to/your/project"]
trust_level = "trusted"
```

No `[mcp_servers.*]` section needed for the graph tools - they are
native handlers compiled into `codex` itself.

### 5. Start the proxy (once per machine)

```bash
codex-copilot-proxy --port 14318 &
```

## Daily use

Three ways to drive the tools.

**(a) Free-form prompt** (let the model decide):
```bash
COPILOT_PROXY_DUMMY_KEY=anything codex exec --skip-git-repo-check \
  --dangerously-bypass-approvals-and-sandbox \
  --cd /path/to/your/project \
  "Use graph_map then graph_why to find every caller of foo()."
```

**(b) Slash commands** (in `codex` interactive TUI):
```
/graph-map
/graph-why ValidateCollectionName
/graph-plan ValidateCollectionName Create Get
/graph-trace ./build/my_tests
```
Each slash injects a single-shot prompt that names the tool; the model
then issues exactly one `tool_call` to the in-process handler.

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

Stage 6 added:

- `model-provider-info/src/lib.rs` — `namespace_tools` field (kept; useful for any opt-in MCP provider in the future).
- `model-provider/src/provider.rs` — `ConfiguredModelProvider::capabilities()` reads it.
- `core/src/tools/handlers/call_graph_spec.rs` — 4 `ToolSpec` builders.
- `core/src/tools/handlers/call_graph.rs` — 4 `ToolHandler` impls.
- `core/src/tools/handlers/mod.rs` — module + re-exports.
- `core/src/tools/spec_plan.rs` — `if config.call_graph_tools { register all four }`.
- `tools/src/tool_config.rs` — `call_graph_tools: bool` (default true) + `with_call_graph_tools` builder.
- `tui/src/slash_command.rs` — 4 new variants with descriptions / inline-args / available-during-task entries.
- `tui/src/chatwidget/slash_dispatch.rs` — dispatch arms that inject a single-shot prompt naming the tool.

No other touchpoints. The original Stage 3.2 `namespace_tools` glue is
left in place because it's still the correct behavior for *any*
custom MCP provider running through the Copilot bridge; the graph
tools just no longer rely on it.

## Known limits / follow-ups

- **`graph_trace --rebuild`**: worktree-based rebuild with the right
  flags is not yet exposed; today the caller must build with
  `-finstrument-functions -rdynamic` themselves.
- **Cross-call cache**: `LoadedGraph::from_pgs_bounded` rebuilds the
  callers-by-callee inverse index every call.
- **scip-clang / rust-analyzer refresh**: the SCIP precise paths in
  perception are present but `graph_map` only invokes the tree-sitter
  / syn fast paths. A `refresh: bool` arg would wire them up.
- **Slash command direct dispatch**: today `/graph-*` inject a prompt
  the model has to act on; a follow-up could short-circuit straight to
  the handler and append the result as a `tool_call_output` without a
  model turn.
