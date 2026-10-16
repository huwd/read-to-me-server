// SPDX-License-Identifier: AGPL-3.0-or-later
//! NFR-SEC3: request logs carry the matched route, never the raw path or query string,
//! which will contain feed and access tokens.
//!
//! Kept in its own test binary with a *global* subscriber: `tracing` caches whether each
//! callsite is enabled, so a thread-local subscriber racing with parallel tests in the
//! same process can miss events and make the assertion meaningless.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;

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

async fn request(app: &common::App, path: &str) {
    let request = Request::get(path)
        .header("authorization", format!("Bearer {}", common::TOKEN))
        .body(Body::empty())
        .unwrap();
    app.send(request).await;
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

    let app = common::app().await;
    let item = rtm_api::items::validate_url("https://example.org/SECRET-article").unwrap();
    let item = rtm_ports::ItemRepository::submit(
        app.store.as_ref(),
        &rtm_domain::UserId("owner".into()),
        &item,
        0,
    )
    .await
    .unwrap();
    request(&app, "/healthz?token=rtm_pat_SECRET").await;
    request(&app, "/feeds/rtm_feed_SECRET.xml").await;
    request(
        &app,
        "/feeds/rtm_feed_SECRET/items/01a1444e-9fa9-77c0-9c37-85a1beabacd1.mp3",
    )
    .await;
    request(&app, "/nowhere/rtm_pat_SECRET").await;
    request(&app, &format!("/v1/items/{}?SECRET", item.id)).await;

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains(r#"route="/healthz""#), "{logs}");
    assert!(logs.contains(r#"route="unmatched""#), "{logs}");
    // Feed tokens are capabilities in the path: only the route pattern is logged.
    assert!(logs.contains(r#"route="/feeds/{file}""#), "{logs}");
    assert!(
        logs.contains(r#"route="/feeds/{token}/items/{file}""#),
        "{logs}"
    );
    assert!(logs.contains(r#"route="/v1/items/{id}""#), "{logs}");
    assert!(!logs.contains("SECRET"), "token leaked into logs:\n{logs}");
    assert!(
        !logs.contains(common::TOKEN),
        "bearer token leaked into logs:\n{logs}"
    );
    assert!(
        !logs.contains(&item.id.to_string()),
        "item ID leaked into logs:\n{logs}"
    );
}
