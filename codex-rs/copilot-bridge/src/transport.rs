use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use codex_client::{
    HttpTransport, Request, RequestBody, Response, StreamResponse, TransportError,
};
use futures::stream::{self, StreamExt};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};

use crate::bridge::CopilotResponsesClient;
use crate::error::CopilotError;
use crate::responses::{ResponseItem, ResponsesRequest, ResponsesResponse};

const RESPONSES_PATH: &str = "/responses";
const MODELS_PATH: &str = "/models";

/// HttpTransport that talks to GitHub Copilot's chat/completions endpoint
/// while pretending, to the rest of codex, that it's a vanilla
/// OpenAI Responses API server. Lets codex-api keep its single wire_api
/// while letting Copilot stay on the chat protocol.
pub struct CopilotTransport {
    inner: Arc<CopilotResponsesClient>,
}

impl CopilotTransport {
    pub fn new(client: CopilotResponsesClient) -> Self {
        Self {
            inner: Arc::new(client),
        }
    }
}

#[async_trait]
impl HttpTransport for CopilotTransport {
    async fn execute(&self, req: Request) -> Result<Response, TransportError> {
        let path = path_from_url(&req.url);
        if req.method == Method::GET && path == MODELS_PATH {
            return Ok(json_response(StatusCode::OK, models_payload()));
        }
        if req.method == Method::POST && path == RESPONSES_PATH {
            let parsed = parse_responses_request(req.body.as_ref())?;
            match self.inner.create_response(&parsed).await {
                Ok(resp) => Ok(json_response(
                    StatusCode::OK,
                    serde_json::to_value(&resp).unwrap_or(Value::Null),
                )),
                Err(err) => Err(copilot_to_transport_error(err, req.url)),
            }
        } else {
            Err(TransportError::Network(format!(
                "copilot transport does not handle {} {}",
                req.method, req.url
            )))
        }
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        let path = path_from_url(&req.url);
        if !(req.method == Method::POST && path == RESPONSES_PATH) {
            return Err(TransportError::Network(format!(
                "copilot transport stream: unsupported {} {}",
                req.method, req.url
            )));
        }
        let parsed = parse_responses_request(req.body.as_ref())?;
        let resp = match self.inner.create_response(&parsed).await {
            Ok(r) => r,
            Err(err) => return Err(copilot_to_transport_error(err, req.url)),
        };
        Ok(sse_stream_for(resp))
    }
}

fn path_from_url(url: &str) -> &str {
    let trimmed = url.trim_end_matches('?');
    let after_scheme = trimmed.split("://").nth(1).unwrap_or(trimmed);
    let path = after_scheme.find('/').map(|i| &after_scheme[i..]).unwrap_or(after_scheme);
    path.split('?').next().unwrap_or(path)
}

fn parse_responses_request(body: Option<&RequestBody>) -> Result<ResponsesRequest, TransportError> {
    let value = match body {
        Some(RequestBody::Json(v)) => v.clone(),
        Some(RequestBody::Raw(bytes)) => serde_json::from_slice(bytes).map_err(|err| {
            TransportError::Build(format!("copilot transport: raw body is not JSON: {err}"))
        })?,
        None => {
            return Err(TransportError::Build(
                "copilot transport: POST /responses requires a JSON body".to_string(),
            ));
        }
    };
    serde_json::from_value(value).map_err(|err| {
        TransportError::Build(format!("copilot transport: ResponsesRequest parse failed: {err}"))
    })
}

fn copilot_to_transport_error(err: CopilotError, url: String) -> TransportError {
    let status = if err.is_auth_problem() {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::BAD_GATEWAY
    };
    TransportError::Http {
        status,
        url: Some(url),
        headers: None,
        body: Some(err.to_string()),
    }
}

fn json_response(status: StatusCode, value: Value) -> Response {
    let bytes = Bytes::from(serde_json::to_vec(&value).unwrap_or_default());
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Response {
        status,
        headers,
        body: bytes,
    }
}

fn sse_stream_for(resp: ResponsesResponse) -> StreamResponse {
    let payloads = build_sse_payloads(&resp);
    let chunks: Vec<Result<Bytes, TransportError>> = payloads
        .into_iter()
        .map(|payload| {
            let encoded = serde_json::to_string(&payload)
                .unwrap_or_else(|_| "{\"type\":\"error\"}".to_string());
            let formatted = format!("data: {encoded}\n\n");
            Ok(Bytes::from(formatted))
        })
        .collect();
    let bytes = stream::iter(chunks).boxed();
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    StreamResponse {
        status: StatusCode::OK,
        headers,
        bytes,
    }
}

fn build_sse_payloads(resp: &ResponsesResponse) -> Vec<SsePayload> {
    let mut out = Vec::with_capacity(resp.output.len() + 2);
    out.push(SsePayload::ResponseCreated);
    for item in &resp.output {
        out.push(SsePayload::OutputItemDone { item: item.clone() });
    }
    out.push(SsePayload::ResponseCompleted {
        response: CompletedResponse {
            id: resp.id.clone(),
            usage: resp.usage.as_ref().map(|u| CompletedUsage {
                input_tokens: u.input_tokens as i64,
                output_tokens: u.output_tokens as i64,
                total_tokens: u.total_tokens as i64,
            }),
            end_turn: Some(true),
        },
    });
    out
}

fn models_payload() -> Value {
    json!({
        "object": "list",
        "data": [
            {"id": "gpt-4o", "object": "model"},
            {"id": "gpt-4o-mini", "object": "model"},
            {"id": "gpt-4.1", "object": "model"},
            {"id": "claude-3.5-sonnet", "object": "model"},
            {"id": "claude-3.7-sonnet", "object": "model"},
            {"id": "claude-opus-4.7-1m-internal", "object": "model"},
        ]
    })
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum SsePayload {
    #[serde(rename = "response.created")]
    ResponseCreated,
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: ResponseItem },
    #[serde(rename = "response.completed")]
    ResponseCompleted { response: CompletedResponse },
}

#[derive(Debug, Serialize)]
struct CompletedResponse {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<CompletedUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_turn: Option<bool>,
}

#[derive(Debug, Serialize)]
struct CompletedUsage {
    input_tokens: i64,
    output_tokens: i64,
    total_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_extraction_handles_https_url() {
        assert_eq!(path_from_url("https://api.openai.com/v1/responses"), "/v1/responses");
        assert_eq!(path_from_url("http://localhost:14318/v1/responses?x=1"), "/v1/responses");
    }

    #[test]
    fn sse_first_payload_is_created_last_is_completed() {
        let resp = ResponsesResponse {
            id: "resp_x".into(),
            model: "gpt-4o".into(),
            output: vec![],
            usage: None,
        };
        let payloads = build_sse_payloads(&resp);
        assert_eq!(payloads.len(), 2);
        assert!(matches!(payloads[0], SsePayload::ResponseCreated));
        assert!(matches!(payloads[1], SsePayload::ResponseCompleted { .. }));
    }

    #[test]
    fn copilot_auth_error_maps_to_unauthorized() {
        let err = CopilotError::MissingCredentials("test".into());
        let mapped = copilot_to_transport_error(err, "http://x/v1/responses".into());
        match mapped {
            TransportError::Http { status, .. } => assert_eq!(status, StatusCode::UNAUTHORIZED),
            other => panic!("expected Http, got {other:?}"),
        }
    }
}
