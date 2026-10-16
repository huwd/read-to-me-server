// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP API for read-to-me.
//!
//! Health endpoints (NFR-M5), plus item submission and status under `/v1`, which
//! require authentication.

pub mod auth;
pub mod feed;
pub mod items;
pub mod problem;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, MatchedPath};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use rtm_ports::{FeedRepository, ItemRepository};
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
pub fn router<R: ItemRepository + FeedRepository + 'static>(
    store: Arc<R>,
    auth: StaticToken,
    feeds: FeedConfig,
) -> Router {
    let auth = Arc::new(auth);
    let v1 = Router::new()
        .route("/v1/items", post(items::submit::<R>))
        .route("/v1/items/{id}", get(items::get::<R>))
        .layer(DefaultBodyLimit::max(API_BODY_LIMIT))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&auth),
            auth::require_token,
        ))
        .with_state(Arc::clone(&store));
    let feed_state = feed::FeedState {
        store,
        blobs: feeds.blobs,
        public_url: feeds.public_url,
    };
    let feed_admin = Router::new()
        .route("/v1/feed/token", post(feed::rotate_token::<R>))
        .route_layer(middleware::from_fn_with_state(auth, auth::require_token))
        .with_state(feed_state.clone());
    // Feed URLs are capabilities (FR-FEED-05): the token in the path is the credential.
    let feeds = Router::new()
        .route("/feeds/{file}", get(feed::feed::<R>))
        .route("/feeds/{token}/artwork.png", get(feed::artwork::<R>))
        .route("/feeds/{token}/items/{file}", get(feed::enclosure::<R>))
        .with_state(feed_state);
    Router::new()
        .merge(v1)
        .merge(feed_admin)
        .merge(feeds)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
}

/// Where episodes are stored, and where the server is reached from outside, for the
/// absolute URLs a feed must contain.
#[derive(Debug, Clone)]
pub struct FeedConfig {
    pub blobs: rtm_blobs::BlobStore,
    pub public_url: url::Url,
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
