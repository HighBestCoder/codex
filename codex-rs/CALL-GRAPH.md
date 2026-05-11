# Codex Call-Graph Tools

Build a project-local call graph, expose it to the model as MCP tools,
and (optionally) drive Codex through GitHub Copilot using a local
chat-to-responses proxy.

## Why this exists

Codex's default model providers only speak the OpenAI Responses API.
GitHub Copilot speaks chat/completions. This tree ships:

1. A **Copilot bridge** (proxy) that lets Codex run unchanged on
   Copilot-served models.
2. A **call-graph engine** (store / perception / algo) that can index
   Rust, C, and C++ projects.
3. An **MCP server** that exposes the engine to the model as four
   tools: `graph_map`, `graph_why`, `graph_plan`, `graph_trace`.

Zero invasive changes to Codex's existing built-in providers: a
single new opt-in field (`namespace_tools`) on `ModelProviderInfo`
controls whether MCP tools are surfaced lazily through `tool_search`
or directly to the model.

## Crates in this tree

| Crate | Role |
|---|---|
| `codex-copilot-bridge` | Standalone library: OAuth + session-token refresh, chat-completions client, Responses<->chat translation. No `codex-*` dependencies. |
| `codex-copilot-bridge-server` | Binary `codex-copilot-proxy`. axum HTTP server that owns `/healthz`, `/v1/models`, `/v1/responses`. |
| `codex-call-graph-store` | sled-backed embedded graph DB. Schema is byte-for-byte compatible with claw-pgs. |
| `codex-call-graph-perception` | Source parsers: syn (Rust), tree-sitter (C/C++), SCIP (rust-analyzer / scip-clang), dynamic finstrument runtime + event reconstruction, git history. |
| `codex-call-graph-algo` | petgraph-backed algorithms: SCC, topo sort, PageRank, BFS, Steiner subgraph, bounded loader. |
| `codex-call-graph-mcp` | Binary `codex-call-graph-mcp`. rmcp stdio server exposing the four tools. |

## One-time setup

### 1. Build

```bash
cd codex-rs
cargo build -p codex-cli                   --bin codex
cargo build -p codex-copilot-bridge-server --bin codex-copilot-proxy
cargo build -p codex-call-graph-mcp        --bin codex-call-graph-mcp
```

### 2. Put the three binaries on PATH

```bash
sudo ln -sf $(pwd)/target/debug/codex                  /usr/local/bin/codex
sudo ln -sf $(pwd)/target/debug/codex-copilot-proxy    /usr/local/bin/codex-copilot-proxy
sudo ln -sf $(pwd)/target/debug/codex-call-graph-mcp   /usr/local/bin/codex-call-graph-mcp
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

If `session_token` is missing or expired the proxy refreshes against
`https://api.github.com/copilot_internal/v2/token` and writes the
refreshed token back to the same file.

### 4. `~/.codex/config.toml`

```toml
model = "claude-opus-4.7-1m-internal"
model_provider = "copilot"

[model_providers.copilot]
name = "GitHub Copilot (via local bridge)"
base_url = "http://127.0.0.1:14318/v1"
wire_api = "responses"
namespace_tools = false                # surface graph_* tools directly
requires_openai_auth = false
env_key = "COPILOT_PROXY_DUMMY_KEY"

[mcp_servers.call_graph]
command = "codex-call-graph-mcp"

[projects."/path/to/your/project"]
trust_level = "trusted"
```

`namespace_tools = false` is the key bit: with the default
(`true`) MCP tools are hidden behind a `tool_search` namespace, which
the model only knows about on `supports_search_tool` models. Setting
it to false hands `graph_map` / `graph_why` / `graph_plan` /
`graph_trace` to the model directly.

### 5. Start the proxy (once per machine)

```bash
codex-copilot-proxy --port 14318 &
```

Or run it under your favorite supervisor / systemd unit. The proxy is
stateless; restarting it picks up a fresh session token on the next
request.

## Daily use

```bash
COPILOT_PROXY_DUMMY_KEY=anything \
  codex exec --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
  --cd /path/to/your/project \
  "Use graph_map then graph_why to find every caller of foo()."
```

## The four MCP tools

### `graph_map`

| Field | Notes |
|---|---|
| Input  | `{ project_root: absolute path }` |
| Detect | Cargo.toml / compile_commands.json / CMakeLists.txt / extension scan → ProjectKind |
| Parse  | Rust syn fast path, tree-sitter for C/C++, Mixed walks each part, Unknown tries Rust then C++ |
| Output | `project_kind`, `files_seen`, `files_indexed`, `failures`, `nodes_added`, `edges_added`, `elapsed_ms`, `pgs_path` |

The PGS lands at `$CODEX_HOME/call-graph/<8-byte blake3 hash of the
canonical project_root>/`. The hash makes the location stable per
project; the same project always reuses the same store.

### `graph_why`

| Field | Notes |
|---|---|
| Input  | `{ project_root, symbol, max_callers?, max_callees? }` |
| Output | For each definition that matches the simple name: `file`, `line`, callers + callees with their `source` (`simple_name` / `scip_internal` / `scip_external` / `dynamic`) and `confidence` |

Simple-name caller lookup falls back to scanning `iter_out_edges`,
which keeps caller lists populated even on tree-sitter-only projects
where `edges_in` is empty.

### `graph_plan`

| Field | Notes |
|---|---|
| Input  | `{ project_root, seed_symbols: 1..32, max_path_depth?, max_subgraph_size? }` |
| Output | `seeds_resolved`, `seeds_unresolved`, `subgraph` (sorted by file/line), `topo_order`, `cycle_detected`, `unresolved_callees` (call sites whose callee name the by_name index never saw, capped at 50) |

Uses `LoadedGraph::from_pgs_bounded` so the algorithm never reads the
entire call graph; it walks `max_path_depth` BFS hops from the seeds,
capped at `max_subgraph_size` nodes.

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
link with `-rdynamic` so `dladdr` resolves symbol names. Without
`-rdynamic` you get hex addresses instead of names.

## Validated workflows

```bash
# Map cortex.core (530 C++ files):
codex exec --cd /builds/cortex.core --dangerously-bypass-approvals-and-sandbox \
  "graph_map /builds/cortex.core"

# Inspect a symbol's callers (returns 20 real gRPC handler call sites):
codex exec --cd /builds/cortex.core --dangerously-bypass-approvals-and-sandbox \
  "graph_why ValidateCollectionName in /builds/cortex.core"

# Plan around a symbol (returns subgraph + unresolved-callee summary):
codex exec --cd /builds/cortex.core --dangerously-bypass-approvals-and-sandbox \
  "graph_plan seeds=[ValidateCollectionName] in /builds/cortex.core"
```

## Known limits / follow-ups

- **`graph_trace --rebuild`**: the `claw test --trace --rebuild` worktree
  path is not yet exposed. Today the caller has to build with the right
  flags themselves.
- **Cross-call cache**: `LoadedGraph::from_pgs_bounded` rebuilds the
  callers-by-callee inverse index every call. A simple per-PGS LRU
  would amortize successive `graph_plan` calls.
- **scip-clang / rust-analyzer**: the SCIP precise paths in
  perception are present but `graph_map` only invokes the tree-sitter
  / syn fast paths. Adding a `refresh: bool` flag would wire them up.
- **Codex slash commands**: `/map`, `/why`, etc. are not added on the
  codex side. The four MCP tools already cover the capability; slash
  is pure UX sugar that we deferred.

## Five codex source touchpoints

Stage 3.2 added one TOML field and 26 lines of glue, scattered across:

- `model-provider-info/src/lib.rs` — new `namespace_tools` field.
- `model-provider/src/provider.rs` — `ConfiguredModelProvider::capabilities()` reads it.
- `core/src/tools/spec_plan.rs` — when `namespace_tools = false`, push every MCP function as `ToolSpec::Function` (not a `Namespace` placeholder).
- `codex-mcp/src/connection_manager.rs` — `resolve_tool_info` falls back to `callable_name` when the model calls a tool without namespace.
- `config/src/thread_config{,/remote}.rs` — keep struct literals in sync.

Every other change in this tree is additive (new crates).
