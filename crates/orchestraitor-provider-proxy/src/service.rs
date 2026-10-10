//! Hyper/tower-compatible provider proxy service.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, Response, StatusCode};
use orchestraitor_cost_ledger::CostLedger;
use orchestraitor_provider_api::ModelEvent;
use serde_json::{Value, json};
use tower::Service;
use uuid::Uuid;

use crate::body::{CompletionOutput, render_completion};
use crate::cost::{CompletionCostInput, CostAttribution, CostRecorder};
use crate::protocol::{model_from_body, model_request_from_body};
use crate::{
    LocalTokenIssuer, ProtocolSurface, ProviderRegistry, ProxyError, ProxyResult,
    TrustBoundaryReport,
};

/// Header exposing the provider-proxy trust-boundary warning on every response.
pub const TRUST_BOUNDARY_HEADER: &str = "x-orchestraitor-trust-boundary";

/// Request body type accepted by [`ProxyHttpService`].
pub type ProxyRequestBody = Full<Bytes>;

/// Response body type returned by [`ProxyHttpService`].
pub type ProxyResponseBody = Full<Bytes>;

/// Tower service wrapping a provider proxy handler.
#[derive(Clone)]
pub struct ProxyHttpService {
    inner: Arc<ProxyHandler>,
}

impl ProxyHttpService {
    /// Constructs a proxy service from registry, auth, ledger, and attribution context.
    #[must_use]
    pub fn new(
        registry: ProviderRegistry,
        local_tokens: LocalTokenIssuer,
        ledger: Arc<Mutex<CostLedger>>,
        attribution: CostAttribution,
    ) -> Self {
        Self {
            inner: Arc::new(ProxyHandler {
                registry,
                local_tokens,
                ledger,
                attribution,
            }),
        }
    }

    /// Issues a new short-lived local bearer token.
    ///
    /// # Errors
    /// Returns an authentication error if the token table is unavailable.
    pub fn issue_local_token(&self) -> ProxyResult<crate::LocalAuthToken> {
        self.inner.local_tokens.issue()
    }

    async fn handle(&self, request: Request<ProxyRequestBody>) -> Response<ProxyResponseBody> {
        match self.inner.handle(request).await {
            Ok(response) => response,
            Err(error) => error_response(&error),
        }
    }
}

impl Service<Request<ProxyRequestBody>> for ProxyHttpService {
    type Response = Response<ProxyResponseBody>;
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<ProxyRequestBody>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { Ok(service.handle(request).await) })
    }
}

struct ProxyHandler {
    registry: ProviderRegistry,
    local_tokens: LocalTokenIssuer,
    ledger: Arc<Mutex<CostLedger>>,
    attribution: CostAttribution,
}

impl ProxyHandler {
    async fn handle(
        &self,
        request: Request<ProxyRequestBody>,
    ) -> ProxyResult<Response<ProxyResponseBody>> {
        let authorization = request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        self.local_tokens.validate_bearer(authorization)?;
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        match (method, path.as_str()) {
            (Method::POST, "/v1/chat/completions") => {
                self.completion(request, ProtocolSurface::OpenAiChatCompletions)
                    .await
            }
            (Method::POST, "/v1/responses") => {
                self.completion(request, ProtocolSurface::OpenAiResponses)
                    .await
            }
            (Method::POST, "/v1/messages") => {
                self.completion(request, ProtocolSurface::AnthropicMessages)
                    .await
            }
            (Method::POST | Method::GET, "/v1/models") => self.models(),
            (Method::GET, "/v1/orchestraitor/trust-boundary") => json_response(
                StatusCode::OK,
                &serde_json::to_value(TrustBoundaryReport::provider_proxy_only())?,
            ),
            (method, _) => Err(ProxyError::UnsupportedEndpoint {
                method: method.to_string(),
                path,
            }),
        }
    }

    async fn completion(
        &self,
        request: Request<ProxyRequestBody>,
        surface: ProtocolSurface,
    ) -> ProxyResult<Response<ProxyResponseBody>> {
        let body = body_json(request).await?;
        let model = model_from_body(&body)?;
        let resolved = self.registry.resolve(surface, model)?;
        let request_id = Uuid::new_v4().to_string();
        let started_at = Utc::now();
        debug_assert_eq!(
            resolved.transport.descriptor().protocol,
            surface.provider_protocol()
        );
        let model_request = model_request_from_body(surface, &body, &resolved.route)?;
        let stream = resolved.transport.stream(model_request).await?;
        let events = stream.collect::<Result<Vec<ModelEvent>, _>>()?;
        let output = CompletionOutput::from_events(events);
        let completed_at = Utc::now();
        let ledger = self.ledger.lock().map_err(|_| ProxyError::Http)?;
        CostRecorder::new(&ledger, self.attribution.clone()).record_completion(
            CompletionCostInput {
                surface,
                route: &resolved.route,
                request_id: &request_id,
                usage: output.usage,
                started_at,
                completed_at,
            },
        )?;
        json_response(StatusCode::OK, &render_completion(surface, model, &output))
    }

    fn models(&self) -> ProxyResult<Response<ProxyResponseBody>> {
        let report = TrustBoundaryReport::provider_proxy_only();
        let body = json!({
            "object": "list",
            "data": self.registry.model_entries(),
            "orchestraitor_trust_boundary": report
        });
        json_response(StatusCode::OK, &body)
    }
}

async fn body_json(request: Request<ProxyRequestBody>) -> ProxyResult<Value> {
    let collected = request
        .into_body()
        .collect()
        .await
        .map_err(|_| ProxyError::Body)?;
    serde_json::from_slice(&collected.to_bytes()).map_err(Into::into)
}

fn json_response(status: StatusCode, value: &Value) -> ProxyResult<Response<ProxyResponseBody>> {
    let bytes = serde_json::to_vec(value)?;
    response(status, Bytes::from(bytes))
}

fn error_response(error: &ProxyError) -> Response<ProxyResponseBody> {
    let status = match error {
        ProxyError::LocalAuthentication => StatusCode::UNAUTHORIZED,
        ProxyError::RouteMissing { .. } | ProxyError::UnsupportedEndpoint { .. } => {
            StatusCode::NOT_FOUND
        }
        ProxyError::InvalidRequest { .. } | ProxyError::Body | ProxyError::Json(_) => {
            StatusCode::BAD_REQUEST
        }
        ProxyError::UpstreamAuth(_)
        | ProxyError::Provider(_)
        | ProxyError::Ledger(_)
        | ProxyError::Http => StatusCode::BAD_GATEWAY,
    };
    let body = json!({
        "error": {
            "message": error.to_string(),
            "type": "orchestraitor_provider_proxy_error"
        },
        "orchestraitor_trust_boundary": TrustBoundaryReport::provider_proxy_only()
    });
    match json_response(status, &body) {
        Ok(response) => response,
        Err(_) => Response::new(Full::new(Bytes::new())),
    }
}

fn response(status: StatusCode, body: Bytes) -> ProxyResult<Response<ProxyResponseBody>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(
            TRUST_BOUNDARY_HEADER,
            "provider-proxy-only; no filesystem-or-shell-containment",
        )
        .body(Full::new(body))
        .map_err(|_| ProxyError::Http)
}
