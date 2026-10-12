// SPDX-License-Identifier: AGPL-3.0-or-later
//! NFR-SEC3: request logs carry the matched route, never the raw path or query string,
//! which will contain feed and access tokens.
//!
//! Kept in its own test binary with a *global* subscriber: `tracing` caches whether each
//! callsite is enabled, so a thread-local subscriber racing with parallel tests in the
//! same process can miss events and make the assertion meaningless.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn request(path: &str) {
    let response = rtm_api::router()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    response.into_body().collect().await.unwrap();
}

#[tokio::test]
async fn request_logs_never_contain_paths_or_queries() {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    request("/healthz?token=rtm_pat_SECRET").await;
    request("/feeds/rtm_feed_SECRET.xml").await;

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains(r#"route="/healthz""#), "{logs}");
    assert!(logs.contains(r#"route="unmatched""#), "{logs}");
    assert!(!logs.contains("SECRET"), "token leaked into logs:\n{logs}");
}
