use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use codex_copilot_bridge::{
    CopilotAuth, CopilotChatClient, CopilotResponsesClient, ResponseItem, ResponsesRequest,
    ResponsesResponse,
};
use futures::stream::{self, Stream};
use serde::Serialize;
use serde_json::json;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct AppState {
    pub responses: Arc<CopilotResponsesClient>,
}

pub fn build_state(auth: CopilotAuth) -> Result<AppState, codex_copilot_bridge::CopilotError> {
    let chat = CopilotChatClient::new(auth)?;
    Ok(AppState {
        responses: Arc::new(CopilotResponsesClient::new(chat)),
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/responses", post(create_response))
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, state: AppState) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    info!("codex-copilot-proxy listening on http://{actual}");
    axum::serve(listener, router(state)).await
}

async fn healthz() -> &'static str {
    "ok"
}

async fn list_models() -> Json<serde_json::Value> {
    Json(json!({
        "object": "list",
        "data": [
            {"id": "gpt-4o", "object": "model"},
            {"id": "gpt-4o-mini", "object": "model"},
            {"id": "gpt-4.1", "object": "model"},
            {"id": "claude-3.5-sonnet", "object": "model"},
            {"id": "claude-3.7-sonnet", "object": "model"},
            {"id": "claude-opus-4.7-1m-internal", "object": "model"}
        ]
    }))
}

async fn create_response(
    State(state): State<AppState>,
    Json(request): Json<ResponsesRequest>,
) -> Response {
    tracing::debug!(
        "incoming responses request: model={} tools={} stream={}",
        request.model,
        request.tools.len(),
        request.stream
    );
    if !request.tools.is_empty() {
        let names: Vec<String> = request
            .tools
            .iter()
            .filter_map(|t| {
                let obj = t.as_object()?;
                obj.get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| obj.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
            })
            .collect();
        tracing::info!("tool names: {names:?}");
    }
    let want_stream = request.stream;
    let result = state.responses.create_response(&request).await;
    match result {
        Ok(resp) => {
            if want_stream {
                stream_response_as_sse(resp).into_response()
            } else {
                Json(resp).into_response()
            }
        }
        Err(err) => {
            let body = err.to_string();
            error!("copilot create_response failed: {body}");
            let status = if err.is_auth_problem() {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_GATEWAY
            };
            (
                status,
                Json(json!({
                    "error": {
                        "message": body,
                        "type": "copilot_bridge_upstream",
                    }
                })),
            )
                .into_response()
        }
    }
}

fn stream_response_as_sse(
    resp: ResponsesResponse,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let events = build_sse_events(&resp);
    let owned: Vec<Event> = events
        .into_iter()
        .map(|payload| Event::default().json_data(&payload).unwrap_or_default())
        .collect();
    let stream = stream::iter(owned.into_iter().map(Ok::<_, Infallible>));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn build_sse_events(resp: &ResponsesResponse) -> Vec<SsePayload> {
    let mut payloads = Vec::with_capacity(resp.output.len() + 2);
    payloads.push(SsePayload::ResponseCreated);
    for item in &resp.output {
        payloads.push(SsePayload::OutputItemDone {
            item: item.clone(),
        });
    }
    payloads.push(SsePayload::ResponseCompleted {
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
    if resp.output.is_empty() {
        warn!("upstream returned no output items; codex will treat the response as empty");
    }
    payloads
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
    use codex_copilot_bridge::{ContentItem, ResponseItem, ResponsesUsage};

    fn make_message_response(text: &str) -> ResponsesResponse {
        ResponsesResponse {
            id: "resp_abc".into(),
            model: "gpt-4o".into(),
            output: vec![ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: text.to_string(),
                }],
            }],
            usage: Some(ResponsesUsage {
                input_tokens: 10,
                output_tokens: 2,
                total_tokens: 12,
            }),
        }
    }

    #[test]
    fn sse_payloads_start_with_created_and_end_with_completed() {
        let resp = make_message_response("hi");
        let payloads = build_sse_events(&resp);
        assert!(matches!(payloads[0], SsePayload::ResponseCreated));
        assert!(matches!(
            payloads.last().unwrap(),
            SsePayload::ResponseCompleted { .. }
        ));
    }

    #[test]
    fn sse_payload_kinds_match_codex_sse_dispatch_table() {
        let resp = make_message_response("hello");
        let payloads = build_sse_events(&resp);
        let kinds: Vec<String> = payloads
            .iter()
            .map(|p| {
                let v = serde_json::to_value(p).expect("serialize");
                v["type"].as_str().unwrap_or_default().to_string()
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "response.created",
                "response.output_item.done",
                "response.completed",
            ]
        );
    }

    #[test]
    fn completed_payload_carries_id_and_usage() {
        let resp = make_message_response("hi");
        let payloads = build_sse_events(&resp);
        let completed = payloads.last().expect("completed");
        let json = serde_json::to_value(completed).expect("serialize");
        assert_eq!(json["type"], "response.completed");
        assert_eq!(json["response"]["id"], "resp_abc");
        assert_eq!(json["response"]["usage"]["input_tokens"], 10);
        assert_eq!(json["response"]["usage"]["output_tokens"], 2);
        assert_eq!(json["response"]["usage"]["total_tokens"], 12);
        assert_eq!(json["response"]["end_turn"], true);
    }

    #[test]
    fn output_item_done_payload_passes_message_through_verbatim() {
        let resp = make_message_response("hello there");
        let payloads = build_sse_events(&resp);
        let middle = &payloads[1];
        let json = serde_json::to_value(middle).expect("serialize");
        assert_eq!(json["type"], "response.output_item.done");
        assert_eq!(json["item"]["type"], "message");
        assert_eq!(json["item"]["role"], "assistant");
        assert_eq!(json["item"]["content"][0]["text"], "hello there");
    }
}
