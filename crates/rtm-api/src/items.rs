// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/items`: submit an article (FR-SUB-01/02) and check its status (FR-STAT-01).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use rtm_domain::{Item, ItemId, ItemState, Millis, Stage, UserId};
use rtm_ports::ItemRepository;
use serde::{Deserialize, Serialize};

use crate::problem::Problem;

/// FR-SUB-02.
pub const MAX_URL_LEN: usize = 2_048;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    url: String,
}

#[derive(Debug, Serialize)]
struct ItemView {
    id: String,
    url: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<FailureView>,
    status_url: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
struct FailureView {
    code: &'static str,
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// The FR-STAT-01 status for an item. Derived from the stage it's waiting on; telling
/// "queued" from "running" needs worker state, which arrives with the workers.
fn status(state: ItemState) -> &'static str {
    match state {
        ItemState::Pending { next } => match next {
            Stage::Fetch => "fetching",
            Stage::Extract | Stage::Script => "extracting",
            Stage::Synthesise => "synthesising",
            Stage::Encode | Stage::Publish => "encoding",
        },
        ItemState::Published => "published",
        ItemState::Failed { .. } => "failed",
    }
}

fn rfc3339(ms: Millis) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

fn status_url(id: ItemId) -> String {
    format!("/v1/items/{id}")
}

impl From<Item> for ItemView {
    fn from(item: Item) -> Self {
        let failure = match item.state {
            ItemState::Failed { stage, code } => Some(FailureView {
                code: code.as_str(),
                stage: stage.as_str(),
                message: item.failure_message,
            }),
            _ => None,
        };
        Self {
            id: item.id.to_string(),
            url: item.source_url,
            status: status(item.state),
            failure,
            status_url: status_url(item.id),
            created_at: rfc3339(item.created_at),
            updated_at: rfc3339(item.updated_at),
        }
    }
}

fn now() -> Millis {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
        Millis::try_from(d.as_millis()).unwrap_or(Millis::MAX)
    })
}

fn invalid_url(detail: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "INVALID_URL",
        "The URL can't be submitted",
    )
    .detail(detail)
}

/// FR-SUB-02: `http` or `https`, at most 2,048 characters, a host, and no credentials.
/// Returns the URL in normalised form. Whether the destination may be fetched is the
/// egress guard's decision, made at fetch time.
pub fn validate_url(raw: &str) -> Result<String, Problem> {
    let raw = raw.trim();
    if raw.len() > MAX_URL_LEN {
        return Err(invalid_url("The URL is longer than 2,048 characters."));
    }
    let url = url::Url::parse(raw).map_err(|_| invalid_url("That isn't a valid URL."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid_url("Only http and https URLs are supported."));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_url(
            "URLs containing a username or password aren't accepted.",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(invalid_url("The URL has no host."));
    }
    let normalised = url.to_string();
    if normalised.len() > MAX_URL_LEN {
        return Err(invalid_url("The URL is longer than 2,048 characters."));
    }
    Ok(normalised)
}

pub async fn submit<R: ItemRepository>(
    State(items): State<Arc<R>>,
    Extension(user): Extension<UserId>,
    body: Result<Json<Submission>, JsonRejection>,
) -> Response {
    let Json(submission) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return Problem::new(
                rejection.status(),
                "INVALID_REQUEST",
                "Invalid request body",
            )
            .detail(rejection.body_text())
            .into_response();
        }
    };
    let url = match validate_url(&submission.url) {
        Ok(url) => url,
        Err(problem) => return problem.into_response(),
    };
    match items.submit(&user, &url, now()).await {
        Ok(item) => {
            let location = status_url(item.id);
            let mut response = (StatusCode::ACCEPTED, Json(ItemView::from(item))).into_response();
            if let Ok(v) = HeaderValue::from_str(&location) {
                response.headers_mut().insert(header::LOCATION, v);
            }
            response
        }
        Err(e) => {
            tracing::error!(error = %e, "could not store submission");
            Problem::internal().into_response()
        }
    }
}

pub async fn get<R: ItemRepository>(
    State(items): State<Arc<R>>,
    Extension(user): Extension<UserId>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    // A malformed ID (including one that isn't valid UTF-8) is just another item that
    // doesn't exist.
    let Some(id) = id.ok().and_then(|Path(id)| id.parse::<ItemId>().ok()) else {
        return Problem::not_found().into_response();
    };
    match items.get(&user, id).await {
        Ok(Some(item)) => Json(ItemView::from(item)).into_response(),
        Ok(None) => Problem::not_found().into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not load item");
            Problem::internal().into_response()
        }
    }
}
