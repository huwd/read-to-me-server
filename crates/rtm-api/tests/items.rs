// SPDX-License-Identifier: AGPL-3.0-or-later
//! Submission and status: FR-SUB-01/02, FR-STAT-01.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, Reply, TOKEN, app};
use rtm_domain::Stage;
use rtm_ports::JobQueue;

fn post(body: &str, token: Option<&str>) -> Request<Body> {
    let mut req = Request::post("/v1/items").header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    req.body(Body::from(body.to_string())).unwrap()
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut req = Request::get(path);
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    req.body(Body::empty()).unwrap()
}

async fn submit(app: &App, url: &str) -> Reply {
    let body = serde_json::json!({ "url": url }).to_string();
    app.send(post(&body, Some(TOKEN))).await
}

fn assert_problem(reply: &Reply, status: StatusCode, code: &str) {
    assert_eq!(reply.status, status, "{}", reply.json);
    assert_eq!(
        reply.headers[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(reply.json["status"], status.as_u16());
    assert_eq!(reply.json["code"], code, "{}", reply.json);
}

// FR-SUB-01
#[tokio::test]
async fn submitting_a_url_queues_an_item() {
    let app = app().await;
    let reply = submit(&app, "https://example.org/essay").await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.json);
    let id = reply.json["id"].as_str().unwrap().to_string();
    let status_url = format!("/v1/items/{id}");
    assert_eq!(reply.json["status_url"], status_url);
    assert_eq!(reply.headers[header::LOCATION], status_url.as_str());
    assert_eq!(reply.json["url"], "https://example.org/essay");
    assert_eq!(reply.json["status"], "fetching");

    // The item's first job is on the queue.
    let job = app
        .store
        .claim("w1", i64::MAX / 2, 1_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((job.item.to_string(), job.stage), (id, Stage::Fetch));
}

// FR-STAT-01
#[tokio::test]
async fn status_reflects_the_pipeline() {
    let app = app().await;
    let id = submit(&app, "https://example.org/essay").await.json["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/v1/items/{id}");

    let reply = app.send(get(&path, Some(TOKEN))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json["id"], id.as_str());
    assert_eq!(reply.json["status"], "fetching");
    assert!(reply.json["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(reply.json.get("failure").is_none());

    let now = i64::MAX / 2;
    let job = app.store.claim("w1", now, 1_000).await.unwrap().unwrap();
    app.store.complete(&job, "w1", now).await.unwrap();
    assert_eq!(
        app.send(get(&path, Some(TOKEN))).await.json["status"],
        "extracting"
    );

    let job = app.store.claim("w1", now, 1_000).await.unwrap().unwrap();
    app.store
        .fail(
            &job,
            "w1",
            rtm_domain::FailureCode::ExtractionLowConfidence,
            "Too little text.",
            now,
        )
        .await
        .unwrap();
    let reply = app.send(get(&path, Some(TOKEN))).await;
    assert_eq!(reply.json["status"], "failed");
    assert_eq!(
        reply.json["failure"],
        serde_json::json!({
            "code": "EXTRACTION_LOW_CONFIDENCE",
            "stage": "extract",
            "message": "Too little text."
        })
    );
}

#[tokio::test]
async fn unknown_and_malformed_ids_are_404() {
    let app = app().await;
    for path in [
        "/v1/items/0190a7c4-0000-7000-8000-000000000000",
        "/v1/items/not-an-id",
        "/v1/items/%FF",
        "/v1/items/%00",
    ] {
        assert_problem(
            &app.send(get(path, Some(TOKEN))).await,
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
        );
    }
}

#[tokio::test]
async fn requests_without_a_valid_token_are_401() {
    let app = app().await;
    let id = submit(&app, "https://example.org/essay").await.json["id"]
        .as_str()
        .unwrap()
        .to_string();
    let body = r#"{"url":"https://example.org/x"}"#;
    let wrong = "wrong-token-0123456789-abcdefghijklmnop";
    for reply in [
        app.send(post(body, None)).await,
        app.send(post(body, Some(wrong))).await,
        app.send(get(&format!("/v1/items/{id}"), None)).await,
        app.send(get(&format!("/v1/items/{id}"), Some(wrong))).await,
    ] {
        assert_problem(&reply, StatusCode::UNAUTHORIZED, "UNAUTHENTICATED");
        assert_eq!(reply.headers[header::WWW_AUTHENTICATE], "Bearer");
    }
    // The scheme name is case-insensitive and may be followed by several spaces.
    for value in [
        format!("bearer {TOKEN}"),
        format!("BEARER {TOKEN}"),
        format!("Bearer   {TOKEN}"),
    ] {
        let req = Request::get(format!("/v1/items/{id}"))
            .header(header::AUTHORIZATION, value.as_str())
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.send(req).await.status, StatusCode::OK, "{value:?}");
    }
    // The token itself is exact.
    for value in [
        format!("Bearer {}", TOKEN.to_uppercase()),
        format!("Bearer {TOKEN} "),
        format!("Bearer{TOKEN}"),
        "Bearer ".to_string(),
    ] {
        let req = Request::get(format!("/v1/items/{id}"))
            .header(header::AUTHORIZATION, value.as_str())
            .body(Body::empty())
            .unwrap();
        assert_problem(
            &app.send(req).await,
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
        );
    }
    // A non-Bearer scheme carrying the right token is still refused.
    let basic = Request::get(format!("/v1/items/{id}"))
        .header(header::AUTHORIZATION, format!("Basic {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    assert_problem(
        &app.send(basic).await,
        StatusCode::UNAUTHORIZED,
        "UNAUTHENTICATED",
    );
}

// FR-SUB-02
#[tokio::test]
async fn invalid_urls_are_rejected() {
    let app = app().await;
    let long = format!("https://example.org/{}", "a".repeat(2_048));
    for url in [
        "ftp://example.org/file",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,hello",
        "gopher://example.org/",
        "https://user:pass@example.org/",
        "https://user@example.org/",
        "not a url",
        "/relative/path",
        "https://",
        long.as_str(),
    ] {
        assert_problem(
            &submit(&app, url).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_URL",
        );
    }
    // Nothing reached the queue.
    assert!(
        app.store
            .claim("w1", i64::MAX / 2, 1_000)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn urls_are_stored_normalised() {
    let app = app().await;
    let reply = submit(&app, "  HTTPS://Example.ORG:443/a b?q=1#frag ").await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.json);
    assert_eq!(reply.json["url"], "https://example.org/a%20b?q=1#frag");
}

#[tokio::test]
async fn malformed_bodies_are_problems() {
    let app = app().await;
    for (body, status) in [
        ("{not json", StatusCode::BAD_REQUEST),
        (
            r#"{"link":"https://example.org/"}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            r#"{"url":"https://example.org/","extra":1}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        assert_problem(
            &app.send(post(body, Some(TOKEN))).await,
            status,
            "INVALID_REQUEST",
        );
    }
    let no_type = Request::post("/v1/items")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(r#"{"url":"https://example.org/"}"#))
        .unwrap();
    assert_problem(
        &app.send(no_type).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "INVALID_REQUEST",
    );

    let huge = format!(r#"{{"url":"https://example.org/{}"}}"#, "a".repeat(20_000));
    assert_problem(
        &app.send(post(&huge, Some(TOKEN))).await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "INVALID_REQUEST",
    );
}

#[test]
fn short_tokens_are_refused() {
    let err = rtm_api::StaticToken::new("too-short", rtm_domain::UserId("owner".into()));
    assert!(err.is_err());
}
