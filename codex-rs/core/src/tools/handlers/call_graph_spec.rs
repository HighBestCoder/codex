use std::collections::BTreeMap;

use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;

pub const GRAPH_MAP_TOOL_NAME: &str = "graph_map";
pub const GRAPH_WHY_TOOL_NAME: &str = "graph_why";
pub const GRAPH_PLAN_TOOL_NAME: &str = "graph_plan";
pub const GRAPH_TRACE_TOOL_NAME: &str = "graph_trace";

pub fn create_graph_map_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "project_root".to_string(),
        JsonSchema::string(Some(
            "Absolute path of the project to index. Detection picks rust/cpp/mixed automatically."
                .to_string(),
        )),
    )]);
    ToolSpec::Function(ResponsesApiTool {
        name: GRAPH_MAP_TOOL_NAME.to_string(),
        description:
            "Walk the project, parse every supported source file (Rust syn / C/C++ tree-sitter), \
             and persist the call graph to a project-local store. Call once per project; \
             subsequent graph_why/plan/trace calls reuse the same store."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["project_root".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_graph_why_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "project_root".to_string(),
            JsonSchema::string(Some(
                "Absolute path of the indexed project.".to_string(),
            )),
        ),
        (
            "symbol".to_string(),
            JsonSchema::string(Some(
                "Simple function name to look up (no namespace prefix).".to_string(),
            )),
        ),
        (
            "max_callers".to_string(),
            JsonSchema::integer(Some(
                "Optional cap on callers returned per match (default 20).".to_string(),
            )),
        ),
        (
            "max_callees".to_string(),
            JsonSchema::integer(Some(
                "Optional cap on callees returned per match (default 20).".to_string(),
            )),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: GRAPH_WHY_TOOL_NAME.to_string(),
        description:
            "Look up a function by simple name in the call graph. Returns every definition that \
             matches, with up to N callers and callees per match including source location and \
             edge provenance (simple_name / scip_internal / scip_external / dynamic)."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["project_root".to_string(), "symbol".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_graph_plan_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "project_root".to_string(),
            JsonSchema::string(Some(
                "Absolute path of the indexed project.".to_string(),
            )),
        ),
        (
            "seed_symbols".to_string(),
            JsonSchema::array(
                JsonSchema::string(None),
                Some(
                    "One to 32 function names. Each name resolves against the by_name index; \
                     multi-hit names contribute every definition as a separate seed."
                        .to_string(),
                ),
            ),
        ),
        (
            "max_path_depth".to_string(),
            JsonSchema::integer(Some(
                "BFS depth from each seed for the bounded loader (default 6, cap 20).".to_string(),
            )),
        ),
        (
            "max_subgraph_size".to_string(),
            JsonSchema::integer(Some(
                "Hard cap on subgraph nodes (default 50, cap 500).".to_string(),
            )),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: GRAPH_PLAN_TOOL_NAME.to_string(),
        description:
            "Given one or more seed function names, return the Steiner subgraph that connects \
             them, plus a topological ordering, a cycle flag, and any call sites whose callee \
             name the by_name index never saw. Use this to plan a multi-symbol edit."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec![
                "project_root".to_string(),
                "seed_symbols".to_string(),
            ]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_graph_trace_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "project_root".to_string(),
            JsonSchema::string(Some(
                "Absolute path of the indexed project.".to_string(),
            )),
        ),
        (
            "test_command".to_string(),
            JsonSchema::array(
                JsonSchema::string(None),
                Some(
                    "1..64 entries: program + args of the test binary to run under LD_PRELOAD. \
                     Binary must be built with -finstrument-functions and linked -rdynamic."
                        .to_string(),
                ),
            ),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: GRAPH_TRACE_TOOL_NAME.to_string(),
        description:
            "Build the LD_PRELOAD instrumentation runtime, run the test binary under it, \
             reconstruct (caller, callee) edges from the captured events, demangle Itanium \
             symbols, and upsert dynamic edges into the project's call graph store under the \
             synthetic '<dynamic-trace>' file."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec![
                "project_root".to_string(),
                "test_command".to_string(),
            ]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}
