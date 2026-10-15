// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP API for read-to-me.
//!
//! Health endpoints (NFR-M5), plus item submission and status under `/v1`, which
//! require authentication.

pub mod auth;
pub mod items;
pub mod problem;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, MatchedPath};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use rtm_ports::ItemRepository;
use serde::Serialize;
use tokio::net::TcpListener;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Requests taking longer than this get `408 Request Timeout`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct Readiness {
    status: &'static str,
    /// Dependency checks (database, object store, TTS). None exist yet.
    checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
}

/// Liveness: the process is up and serving HTTP.
async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: VERSION,
    })
}

/// Readiness: every dependency the server needs is reachable.
async fn readyz() -> (StatusCode, Json<Readiness>) {
    let checks: Vec<Check> = Vec::new();
    let ready = checks.iter().all(|c| c.ok);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = Readiness {
        status: if ready { "ready" } else { "not-ready" },
        checks,
    };
    (status, Json(body))
}

/// Span for each request: method and the *matched route pattern* only. Raw paths and
/// query strings are never recorded, because they will carry feed and access tokens
/// (NFR-SEC3, ADR-0011).
fn request_span<B>(request: &Request<B>) -> tracing::Span {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    tracing::info_span!("request", method = %request.method(), route)
}

/// Request bodies under `/v1` are small JSON documents.
const API_BODY_LIMIT: usize = 16 * 1024;

pub use auth::StaticToken;

/// The application router with its middleware stack.
pub fn router<R: ItemRepository + 'static>(items: Arc<R>, auth: StaticToken) -> Router {
    let v1 = Router::new()
        .route("/v1/items", post(items::submit::<R>))
        .route("/v1/items/{id}", get(items::get::<R>))
        .layer(DefaultBodyLimit::max(API_BODY_LIMIT))
        .route_layer(middleware::from_fn_with_state(
            Arc::new(auth),
            auth::require_token,
        ))
        .with_state(items);
    Router::new()
        .merge(v1)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
}

/// Serve `router` on `listener` until `shutdown` completes, then drain in-flight requests.
pub async fn serve<F>(listener: TcpListener, router: Router, shutdown: F) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}
