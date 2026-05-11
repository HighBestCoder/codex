use std::borrow::Cow;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::map::{MapRequest, MapResponse, run_map};
use crate::plan::{PlanRequest, PlanResponse, run_plan};
use crate::why::{WhyRequest, WhyResponse, run_why};

const MAP_TOOL: &str = "graph_map";
const PLAN_TOOL: &str = "graph_plan";
const WHY_TOOL: &str = "graph_why";

#[derive(Clone, Default)]
pub struct CallGraphMcpServer {
    tools: Arc<Vec<Tool>>,
}

impl CallGraphMcpServer {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(vec![map_tool(), why_tool(), plan_tool()]),
        }
    }
}

impl ServerHandler for CallGraphMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Build and query a project-local code call-graph. \
                 Use graph_map to index a project root once per session, \
                 then graph_why to inspect callers and callees of a symbol."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..ServerInfo::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let tools = Arc::clone(&self.tools);
        async move {
            Ok(ListToolsResult {
                tools: (*tools).clone(),
                next_cursor: None,
                meta: None,
            })
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let value = Value::Object(
            request
                .arguments
                .unwrap_or_default()
                .into_iter()
                .collect::<serde_json::Map<String, Value>>(),
        );
        let structured: Value = match request.name.as_ref() {
            MAP_TOOL => {
                let req: MapRequest = parse_args(value)?;
                let resp = tokio::task::spawn_blocking(move || run_map(&req))
                    .await
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?;
                json!(resp)
            }
            WHY_TOOL => {
                let req: WhyRequest = parse_args(value)?;
                let resp = tokio::task::spawn_blocking(move || run_why(&req))
                    .await
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?;
                json!(resp)
            }
            PLAN_TOOL => {
                let req: PlanRequest = parse_args(value)?;
                let resp = tokio::task::spawn_blocking(move || run_plan(&req))
                    .await
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?;
                json!(resp)
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("unknown tool: {other}"),
                    None,
                ));
            }
        };
        Ok(CallToolResult {
            content: vec![Content::text(structured.to_string())],
            structured_content: Some(structured),
            is_error: Some(false),
            meta: None,
        })
    }
}

pub async fn run_server<T, E, A>(transport: T) -> anyhow::Result<()>
where
    T: rmcp::transport::IntoTransport<rmcp::RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    CallGraphMcpServer::new().serve(transport).await?.waiting().await?;
    Ok(())
}

pub async fn run_stdio_server() -> anyhow::Result<()> {
    run_server((tokio::io::stdin(), tokio::io::stdout())).await
}

fn map_tool() -> Tool {
    let mut tool = Tool::new(
        Cow::Borrowed(MAP_TOOL),
        Cow::Borrowed(
            "Walk a project root, parse every supported source file, and persist the \
             resulting call graph to a project-local store. Returns counts and the on-disk \
             path. Call once per project; subsequent graph_why calls reuse the store.",
        ),
        Arc::new(input_schema::<MapRequest>()),
    );
    tool.output_schema = Some(Arc::new(output_schema::<MapResponse>()));
    tool.annotations = Some(ToolAnnotations::new());
    tool
}

fn why_tool() -> Tool {
    let mut tool = Tool::new(
        Cow::Borrowed(WHY_TOOL),
        Cow::Borrowed(
            "Look up a function by its simple name in the call graph store. Returns every \
             definition that matches, with up to N callers and callees per match including \
             source location and edge provenance.",
        ),
        Arc::new(input_schema::<WhyRequest>()),
    );
    tool.output_schema = Some(Arc::new(output_schema::<WhyResponse>()));
    tool.annotations = Some(ToolAnnotations::new().read_only(true));
    tool
}

fn plan_tool() -> Tool {
    let mut tool = Tool::new(
        Cow::Borrowed(PLAN_TOOL),
        Cow::Borrowed(
            "Given one or more seed function names, return the Steiner subgraph that \
             connects them together with a topological ordering and any cycle warning. \
             Use this to plan an edit that touches multiple symbols: the subgraph is the \
             minimal set of related functions, and topo_order suggests a safe walk order.",
        ),
        Arc::new(input_schema::<PlanRequest>()),
    );
    tool.output_schema = Some(Arc::new(output_schema::<PlanResponse>()));
    tool.annotations = Some(ToolAnnotations::new().read_only(true));
    tool
}

fn parse_args<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, McpError> {
    serde_json::from_value(value).map_err(|err| McpError::invalid_params(err.to_string(), None))
}

fn input_schema<T: JsonSchema>() -> serde_json::Map<String, Value> {
    let generator = schemars::r#gen::SchemaSettings::draft07().into_generator();
    let schema = generator.into_root_schema_for::<T>();
    schema_object_to_map(serde_json::to_value(schema).unwrap_or(Value::Null))
}

fn output_schema<T: JsonSchema>() -> serde_json::Map<String, Value> {
    input_schema::<T>()
}

fn schema_object_to_map(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}
