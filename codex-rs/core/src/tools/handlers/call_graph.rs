use codex_call_graph_tools::{
    MapRequest, PlanRequest, TraceRequest, WhyRequest, run_map, run_plan, run_trace, run_why,
};
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::call_graph_spec::{
    GRAPH_MAP_TOOL_NAME, GRAPH_PLAN_TOOL_NAME, GRAPH_TRACE_TOOL_NAME, GRAPH_WHY_TOOL_NAME,
    create_graph_map_tool, create_graph_plan_tool, create_graph_trace_tool, create_graph_why_tool,
};
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct GraphMapHandler;
pub struct GraphWhyHandler;
pub struct GraphPlanHandler;
pub struct GraphTraceHandler;

impl ToolHandler for GraphMapHandler {
    type Output = FunctionToolOutput;
    fn tool_name(&self) -> ToolName {
        ToolName::plain(GRAPH_MAP_TOOL_NAME)
    }
    fn spec(&self) -> Option<ToolSpec> {
        Some(create_graph_map_tool())
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let args = function_arguments(&invocation, GRAPH_MAP_TOOL_NAME)?;
        let req: MapRequest = parse_arguments(&args, GRAPH_MAP_TOOL_NAME)?;
        let result = tokio::task::spawn_blocking(move || run_map(&req))
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(serialize_output(&result))
    }
}

impl ToolHandler for GraphWhyHandler {
    type Output = FunctionToolOutput;
    fn tool_name(&self) -> ToolName {
        ToolName::plain(GRAPH_WHY_TOOL_NAME)
    }
    fn spec(&self) -> Option<ToolSpec> {
        Some(create_graph_why_tool())
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let args = function_arguments(&invocation, GRAPH_WHY_TOOL_NAME)?;
        let req: WhyRequest = parse_arguments(&args, GRAPH_WHY_TOOL_NAME)?;
        let result = tokio::task::spawn_blocking(move || run_why(&req))
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(serialize_output(&result))
    }
}

impl ToolHandler for GraphPlanHandler {
    type Output = FunctionToolOutput;
    fn tool_name(&self) -> ToolName {
        ToolName::plain(GRAPH_PLAN_TOOL_NAME)
    }
    fn spec(&self) -> Option<ToolSpec> {
        Some(create_graph_plan_tool())
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let args = function_arguments(&invocation, GRAPH_PLAN_TOOL_NAME)?;
        let req: PlanRequest = parse_arguments(&args, GRAPH_PLAN_TOOL_NAME)?;
        let result = tokio::task::spawn_blocking(move || run_plan(&req))
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(serialize_output(&result))
    }
}

impl ToolHandler for GraphTraceHandler {
    type Output = FunctionToolOutput;
    fn tool_name(&self) -> ToolName {
        ToolName::plain(GRAPH_TRACE_TOOL_NAME)
    }
    fn spec(&self) -> Option<ToolSpec> {
        Some(create_graph_trace_tool())
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let args = function_arguments(&invocation, GRAPH_TRACE_TOOL_NAME)?;
        let req: TraceRequest = parse_arguments(&args, GRAPH_TRACE_TOOL_NAME)?;
        let result = tokio::task::spawn_blocking(move || run_trace(&req))
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(serialize_output(&result))
    }
}

fn function_arguments<'a>(
    invocation: &'a ToolInvocation,
    tool: &'static str,
) -> Result<&'a str, FunctionCallError> {
    match &invocation.payload {
        ToolPayload::Function { arguments } => Ok(arguments.as_str()),
        _ => Err(FunctionCallError::RespondToModel(format!(
            "{tool} handler received unsupported payload"
        ))),
    }
}

fn parse_arguments<T: DeserializeOwned>(
    arguments: &str,
    tool: &'static str,
) -> Result<T, FunctionCallError> {
    let value: Value = if arguments.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("{tool}: invalid arguments: {err}"))
        })?
    };
    serde_json::from_value(value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("{tool}: schema mismatch: {err}"))
    })
}

fn serialize_output<T: serde::Serialize>(value: &T) -> FunctionToolOutput {
    let text = serde_json::to_string(value).unwrap_or_else(|err| {
        format!("{{\"error\":\"failed to serialize tool output: {err}\"}}")
    });
    FunctionToolOutput::from_text(text, Some(true))
}
