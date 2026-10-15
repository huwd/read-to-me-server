// SPDX-License-Identifier: AGPL-3.0-or-later
//! Authentication for the walking skeleton: one static bearer token for one user.
//! OIDC and personal access tokens replace this in the MVP (ADR-0004).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rtm_domain::UserId;

use crate::problem::Problem;

/// Shorter tokens are refused at startup: the token is the only thing protecting the API.
pub const MIN_TOKEN_LEN: usize = 32;

/// A single bearer token that authenticates as `user`.
#[derive(Clone)]
pub struct StaticToken {
    digest: blake3::Hash,
    user: UserId,
}

impl StaticToken {
    pub fn new(token: &str, user: UserId) -> Result<Self, String> {
        if token.len() < MIN_TOKEN_LEN {
            return Err(format!(
                "the API token must be at least {MIN_TOKEN_LEN} characters"
            ));
        }
        Ok(Self {
            digest: blake3::hash(token.as_bytes()),
            user,
        })
    }

    /// The user a presented token belongs to. Compares digests, and `blake3::Hash`
    /// equality is constant-time, so timing reveals nothing about the token.
    fn verify(&self, presented: &str) -> Option<&UserId> {
        (blake3::hash(presented.as_bytes()) == self.digest).then_some(&self.user)
    }
}

/// The token from an `Authorization` value using the Bearer scheme. The scheme name is
/// case-insensitive (RFC 9110 §11.1) and may be followed by several spaces (RFC 6750
/// §2.1); the token itself is compared exactly.
fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    let token = rest.trim_start_matches(' ');
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// Middleware: reject requests without a valid `Authorization: Bearer` token, and make
/// the authenticated [`UserId`] available to handlers as an extension.
pub async fn require_token(
    State(auth): State<Arc<StaticToken>>,
    mut request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token);
    match presented.and_then(|t| auth.verify(t)) {
        Some(user) => {
            let user = user.clone();
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        None => Problem::unauthorized().into_response(),
    }
}
