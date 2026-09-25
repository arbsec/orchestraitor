//! Deterministic mock of the `OpenAI` Chat Completions wire protocol
//! (spec §21.3): non-streaming responses, SSE streaming, and structured
//! output (`response_format`). Every request is captured so tests can assert
//! exact client behavior; IDs and timestamps derive from the request
//! sequence, never from wall clock or randomness.

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Fixed base timestamp for every emitted response (deterministic, §21.3).
const BASE_CREATED: u64 = 1_700_000_000;

/// One planned response to one `POST /v1/chat/completions` call, consumed in
/// request order. The last planned response repeats when the script runs out,
/// keeping long conversations well-defined instead of erroring.
#[derive(Debug, Clone)]
pub enum PlannedResponse {
    /// A complete non-streaming completion with the given message content.
    NonStreaming {
        /// Message content returned in `choices[0].message.content`.
        content: String,
    },
    /// An SSE stream of content deltas (`choices[0].delta.content` per chunk),
    /// terminated by a finish frame and `[DONE]`.
    Streaming {
        /// Ordered content deltas.
        chunks: Vec<String>,
    },
    /// Structured output: the payload is serialized into the message content,
    /// in response to a request carrying `response_format`.
    Structured {
        /// JSON payload serialized as content text.
        payload: Value,
    },
    /// HTTP-level failure with an OpenAI-style error body (retry/behavior
    /// testing; e.g. 429 with `retry_after` in the body).
    Failure {
        /// HTTP status code.
        status: u16,
        /// OpenAI-style error body.
        body: Value,
    },
}

/// A request captured by the simulator, in request-index (arrival) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    /// Request index (0-based), assigned by an atomic arrival counter before
    /// capture; `captured_requests` returns records ordered by this index.
    pub index: usize,
    /// Requested model id from the JSON body.
    pub model: Option<String>,
    /// Whether the client asked for streaming (`stream == true`).
    pub streaming: bool,
    /// The raw `response_format` value when present (structured output demand).
    pub response_format: Option<String>,
    /// Number of messages in the request.
    pub message_count: usize,
}

/// Shared state behind every connection.
#[derive(Debug)]
struct SimState {
    script: Vec<PlannedResponse>,
    captured: Vec<CapturedRequest>,
}

/// Handle to a running `OpenAI` Chat Completions mock server.
#[derive(Debug)]
pub struct OpenAiMockServer {
    base_url: String,
    state: Arc<Mutex<SimState>>,
    server: JoinHandle<()>,
}

impl OpenAiMockServer {
    /// Spawns the mock server on `127.0.0.1` with an ephemeral port.
    ///
    /// # Errors
    ///
    /// Returns an IO error when binding the listener fails.
    pub async fn serve(script: Vec<PlannedResponse>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(SimState {
            script,
            captured: Vec::new(),
        }));
        let counter = Arc::new(AtomicUsize::new(0));
        let server = {
            let state = Arc::clone(&state);
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let conn_state = Arc::clone(&state);
                    let conn_counter = Arc::clone(&counter);
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let _result: Result<(), hyper::Error> = http1::Builder::new()
                            .serve_connection(
                                io,
                                service_fn(move |req| {
                                    handle(Arc::clone(&conn_state), Arc::clone(&conn_counter), req)
                                }),
                            )
                            .await;
                    });
                }
            })
        };
        Ok(Self {
            base_url: format!("http://{addr}/v1"),
            state,
            server,
        })
    }

    /// Base URL clients should target, including the `/v1` API prefix (e.g.
    /// `POST <base_url>/chat/completions`). The value can be passed directly
    /// to a provider `base_url` setting, matching the OpenAI-SDK-style
    /// convention used by this repository's providers: default base URLs
    /// include `/v1` and transports post to `{base}/chat/completions`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns all captured requests in request-index (arrival) order. A
    /// poisoned mutex (a panicking test) still yields the recordings made so
    /// far — diagnostics beat data loss in test infrastructure.
    #[must_use]
    pub fn captured_requests(&self) -> Vec<CapturedRequest> {
        let mut captured = match self.state.lock() {
            Ok(guard) => guard.captured.clone(),
            Err(poisoned) => poisoned.into_inner().captured.clone(),
        };
        // Indices come from an atomic counter outside the capture mutex, so
        // lock-acquisition order may differ from arrival order; indices are
        // unique, making this sort a total, deterministic order.
        captured.sort_by_key(|request| request.index);
        captured
    }
}

impl Drop for OpenAiMockServer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Builds a plain response; builder failures collapse into a 500 body so the
/// infallible service contract holds.
fn plain(status: StatusCode, content_type: &str, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from_static(b"simulator render error")))
                .unwrap_or_default()
        })
}

/// Handles one HTTP request against the script.
async fn handle(
    state: Arc<Mutex<SimState>>,
    counter: Arc<AtomicUsize>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    if parts.method != hyper::Method::POST || parts.uri.path() != "/v1/chat/completions" {
        return Ok(plain(
            StatusCode::NOT_FOUND,
            "text/plain",
            "unknown route: only POST /v1/chat/completions is supported".to_string(),
        ));
    }
    let body_bytes = match http_body_util::BodyExt::collect(body).await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Ok(plain(
                StatusCode::BAD_REQUEST,
                "text/plain",
                "unreadable request body".to_string(),
            ));
        }
    };
    let parsed: Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) => value,
        Err(_) => {
            return Ok(plain(
                StatusCode::BAD_REQUEST,
                "text/plain",
                "invalid JSON body".to_string(),
            ));
        }
    };

    let index = counter.fetch_add(1, Ordering::SeqCst);
    let captured = CapturedRequest {
        index,
        model: parsed
            .get("model")
            .and_then(Value::as_str)
            .map(String::from),
        streaming: parsed
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        response_format: parsed.get("response_format").map(ToString::to_string),
        message_count: parsed
            .get("messages")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
    };
    let plan = {
        let mut guard = match state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.captured.push(captured);
        if guard.script.is_empty() {
            PlannedResponse::Failure {
                status: 500,
                body: json!({"error": {"message": "empty simulator script", "type": "simulator"}}),
            }
        } else {
            guard
                .script
                .get(index)
                .or_else(|| guard.script.last())
                .map_or_else(
                    || PlannedResponse::NonStreaming {
                        content: String::new(),
                    },
                    Clone::clone,
                )
        }
    };
    Ok(render(&plan, index))
}

/// Renders one planned response with deterministic ids and timestamps.
fn render(plan: &PlannedResponse, index: usize) -> Response<Full<Bytes>> {
    let id = format!("chatcmpl-testkit-{index:05}");
    let created = BASE_CREATED + u64::from(u32::try_from(index).unwrap_or(u32::MAX));
    let chunk = |delta: Value, finish: Option<&str>| {
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": "testkit-model",
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish
            }]
        })
    };
    match plan {
        PlannedResponse::NonStreaming { content } => {
            let body = json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": "testkit-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": u32::try_from(content.split_whitespace().count()).unwrap_or(0),
                    "total_tokens": u32::try_from(content.split_whitespace().count()).unwrap_or(0)
                }
            });
            plain(StatusCode::OK, "application/json", body.to_string())
        }
        PlannedResponse::Streaming { chunks } => {
            let mut sse = String::new();
            for delta in chunks {
                let frame = chunk(json!({"role": "assistant", "content": delta}), None);
                sse.push_str("data: ");
                sse.push_str(&frame.to_string());
                sse.push_str("\n\n");
            }
            let finish_frame = chunk(json!({}), Some("stop"));
            sse.push_str("data: ");
            sse.push_str(&finish_frame.to_string());
            sse.push_str("\n\ndata: [DONE]\n\n");
            plain(StatusCode::OK, "text/event-stream", sse)
        }
        PlannedResponse::Structured { payload } => {
            let body = json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": "testkit-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": payload.to_string()},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
            });
            plain(StatusCode::OK, "application/json", body.to_string())
        }
        PlannedResponse::Failure { status, body } => plain(
            StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "application/json",
            body.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn chat_request(
        base: &str,
        body: Value,
    ) -> impl Future<Output = Result<reqwest::Response, reqwest::Error>> {
        let url = format!("{base}/chat/completions");
        async move { reqwest::Client::new().post(url).json(&body).send().await }
    }

    fn request_body(model: &str, stream: bool) -> Value {
        json!({
            "model": model,
            "stream": stream,
            "messages": [{"role": "user", "content": "hello"}]
        })
    }

    #[tokio::test]
    async fn non_streaming_completion_has_deterministic_shape() -> TestResult {
        let sim = OpenAiMockServer::serve(vec![PlannedResponse::NonStreaming {
            content: "answer".to_string(),
        }])
        .await?;
        let resp = chat_request(sim.base_url(), request_body("glm-5.2", false)).await?;
        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await?;
        assert_eq!(body["id"], "chatcmpl-testkit-00000");
        assert_eq!(body["created"], 1_700_000_000);
        assert_eq!(body["choices"][0]["message"]["content"], "answer");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        let captured = sim.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model.as_deref(), Some("glm-5.2"));
        assert!(!captured[0].streaming);
        assert_eq!(captured[0].message_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn streaming_completion_emits_sse_frames_in_order() -> TestResult {
        let sim = OpenAiMockServer::serve(vec![PlannedResponse::Streaming {
            chunks: vec!["Hello".to_string(), ", ".to_string(), "world".to_string()],
        }])
        .await?;
        let resp = chat_request(sim.base_url(), request_body("glm-5.2", true)).await?;
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        let text = resp.text().await?;
        let frames: Vec<&str> = text
            .split("\n\n")
            .filter(|frame| frame.starts_with("data: "))
            .collect();
        assert_eq!(
            frames.len(),
            5,
            "expected 3 deltas + 1 finish frame + [DONE], got: {frames:?}"
        );
        let mut assembled = String::new();
        for frame in &frames[..frames.len() - 2] {
            let parsed: Value = serde_json::from_str(frame["data: ".len()..].trim())?;
            let delta = parsed["choices"][0]["delta"]["content"]
                .as_str()
                .ok_or("delta content missing")?;
            assembled.push_str(delta);
            assert_eq!(parsed["object"], "chat.completion.chunk");
        }
        assert_eq!(assembled, "Hello, world");
        let finish: Value =
            serde_json::from_str(frames[frames.len() - 2]["data: ".len()..].trim())?;
        assert_eq!(finish["choices"][0]["finish_reason"], "stop");
        assert_eq!(frames[frames.len() - 1], "data: [DONE]");
        assert!(sim.captured_requests()[0].streaming);
        Ok(())
    }

    #[tokio::test]
    async fn structured_output_honors_response_format() -> TestResult {
        let payload = json!({"name": "Widget", "confidence": 0.99});
        let sim = OpenAiMockServer::serve(vec![PlannedResponse::Structured {
            payload: payload.clone(),
        }])
        .await?;
        let mut req = request_body("glm-5.2", false);
        req["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "thing"}});
        let resp = chat_request(sim.base_url(), req).await?;
        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await?;
        let parsed: Value = serde_json::from_str(
            body["choices"][0]["message"]["content"]
                .as_str()
                .ok_or("structured content missing")?,
        )?;
        assert_eq!(parsed, payload);
        assert!(sim.captured_requests()[0].response_format.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn script_orders_responses_and_last_plan_repeats() -> TestResult {
        let sim = OpenAiMockServer::serve(vec![
            PlannedResponse::NonStreaming {
                content: "first".to_string(),
            },
            PlannedResponse::Failure {
                status: 429,
                body: json!({"error": {"message": "rate limited", "type": "rate_limit"}}),
            },
        ])
        .await?;
        let r0: Value = chat_request(sim.base_url(), request_body("m", false))
            .await?
            .json()
            .await?;
        assert_eq!(r0["choices"][0]["message"]["content"], "first");
        let r1 = chat_request(sim.base_url(), request_body("m", false)).await?;
        assert_eq!(r1.status(), 429);
        // script exhausted: last plan repeats
        let r2 = chat_request(sim.base_url(), request_body("m", false)).await?;
        assert_eq!(r2.status(), 429);
        assert_eq!(sim.captured_requests().len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_route_and_bad_json_are_4xx() -> TestResult {
        let sim = OpenAiMockServer::serve(vec![]).await?;
        let unknown = reqwest::Client::new()
            .get(format!("{}/models", sim.base_url()))
            .send()
            .await?;
        assert_eq!(unknown.status(), 404);
        let bad = reqwest::Client::new()
            .post(format!("{}/chat/completions", sim.base_url()))
            .header("content-type", "application/json")
            .body("not json")
            .send()
            .await?;
        assert_eq!(bad.status(), 400);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_captures_are_returned_in_request_index_order() -> TestResult {
        const N: usize = 64;
        let sim = Arc::new(
            OpenAiMockServer::serve(vec![PlannedResponse::NonStreaming {
                content: "ok".to_string(),
            }])
            .await?,
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(N));
        let client = reqwest::Client::new();
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let sim = Arc::clone(&sim);
            let barrier = Arc::clone(&barrier);
            let client = client.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                client
                    .post(format!("{}/chat/completions", sim.base_url()))
                    .json(&request_body("glm-5.2", false))
                    .send()
                    .await
            }));
        }
        for handle in handles {
            let resp = handle.await??;
            assert_eq!(resp.status(), 200);
        }
        let captured = sim.captured_requests();
        assert_eq!(captured.len(), N);
        for (expected, record) in captured.iter().enumerate() {
            assert_eq!(
                record.index, expected,
                "captured_requests must be in request-index (arrival) order"
            );
        }
        Ok(())
    }
}
