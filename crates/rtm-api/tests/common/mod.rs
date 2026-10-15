// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real router over a real store in a temporary directory.

#![allow(dead_code)] // each test binary uses a different subset

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rtm_api::StaticToken;
use rtm_domain::UserId;
use rtm_store::{QueuePolicy, Store};
use tower::ServiceExt;

pub const TOKEN: &str = "test-token-0123456789-abcdefghijklmnop";

pub struct App {
    pub router: Router,
    pub store: Arc<Store>,
    _dir: tempfile::TempDir,
}

pub async fn app() -> App {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
            .await
            .unwrap(),
    );
    let auth = StaticToken::new(TOKEN, UserId("owner".into())).unwrap();
    App {
        router: rtm_api::router(Arc::clone(&store), auth),
        store,
        _dir: dir,
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub json: serde_json::Value,
}

impl App {
    pub async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        Reply {
            status,
            headers,
            json,
        }
    }

    pub async fn get(&self, path: &str) -> Reply {
        self.send(Request::get(path).body(Body::empty()).unwrap())
            .await
    }
}
