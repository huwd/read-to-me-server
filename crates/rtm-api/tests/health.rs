// SPDX-License-Identifier: AGPL-3.0-or-later
//! NFR-M5: liveness and readiness endpoints.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

async fn get(path: &str) -> (StatusCode, serde_json::Value) {
    let response = rtm_api::router()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn healthz_reports_ok_and_version() {
    let (status, body) = get("/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn readyz_is_ready_with_no_dependencies() {
    let (status, body) = get("/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["checks"], serde_json::json!([]));
}

#[tokio::test]
async fn unknown_routes_are_404() {
    let (status, _) = get("/v1/nothing-here").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Over a real socket, and shutting down cleanly when asked.
#[tokio::test]
async fn serves_over_tcp_and_shuts_down() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(rtm_api::serve(listener, async {
        let _ = stopped.await;
    }));

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains(r#""status":"ok""#), "{response}");

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// NFR-SEC3: request logs carry the matched route, never the raw path or query string,
/// which will contain feed and access tokens.
#[tokio::test]
async fn request_logs_never_contain_paths_or_queries() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

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

    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    get("/healthz?token=rtm_pat_SECRET").await;
    get("/feeds/rtm_feed_SECRET.xml").await;

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains(r#"route="/healthz""#), "{logs}");
    assert!(logs.contains(r#"route="unmatched""#), "{logs}");
    assert!(!logs.contains("SECRET"), "token leaked into logs:\n{logs}");
}
