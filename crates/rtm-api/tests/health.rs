// SPDX-License-Identifier: AGPL-3.0-or-later
//! NFR-M5: liveness and readiness endpoints.

mod common;

use axum::http::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn get(path: &str) -> (StatusCode, serde_json::Value) {
    let reply = common::app().await.get(path).await;
    (reply.status, reply.json)
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
    let app = common::app().await;
    let server = tokio::spawn(rtm_api::serve(listener, app.router.clone(), async {
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
