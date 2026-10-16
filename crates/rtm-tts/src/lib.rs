// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `rtm tts` process (ADR-0007): a [`SpeechEngine`] behind the HTTP contract in
//! `rtm_speech::protocol`. It runs as its own process so inference crashes and memory
//! stay away from the feed server (NFR-R3, NFR-P7).
//!
//! Synthesis is CPU-bound and the engine uses every core it's given, so requests are
//! served one at a time, each on a blocking thread; others wait their turn.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rtm_speech::protocol::{
    FINGERPRINT_HEADER, MAX_TEXT_CHARS, SynthesizeRequest, VoiceInfo, VoicesResponse,
};
use rtm_speech::{SpeechEngine, SpeechError};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Tts {
    engine: Arc<dyn SpeechEngine>,
    one_at_a_time: Arc<Semaphore>,
}

fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    let body = serde_json::json!({ "code": code, "message": message.into() });
    (status, Json(body)).into_response()
}

async fn healthz(State(tts): State<Tts>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "engine": tts.engine.id() }))
}

async fn voices(State(tts): State<Tts>) -> Json<VoicesResponse> {
    let engine = &tts.engine;
    Json(VoicesResponse {
        engine: engine.id().to_string(),
        sample_rate: engine.sample_rate(),
        voices: engine
            .voices()
            .into_iter()
            .filter_map(|voice| {
                let fingerprint = engine.fingerprint(&voice.id)?;
                Some(VoiceInfo { voice, fingerprint })
            })
            .collect(),
    })
}

async fn synthesize(
    State(tts): State<Tts>,
    body: Result<Json<SynthesizeRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(e) => return error(e.status(), "INVALID_REQUEST", e.body_text()),
    };
    let chars = request.text.chars().count();
    if request.text.trim().is_empty() || chars > MAX_TEXT_CHARS {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BAD_TEXT",
            format!("text must be 1 to {MAX_TEXT_CHARS} characters; got {chars}"),
        );
    }
    let Some(fingerprint) = tts.engine.fingerprint(&request.voice) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "UNKNOWN_VOICE",
            format!("no voice {:?}", request.voice),
        );
    };
    // An owned permit, moved into the blocking task: if the caller disconnects and this
    // handler is dropped, the synthesis keeps running, so the permit must stay with it
    // until it finishes or unwinds, not with the handler.
    let Ok(turn) = Arc::clone(&tts.one_at_a_time).acquire_owned().await else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SHUTTING_DOWN",
            "shutting down",
        );
    };
    let engine = Arc::clone(&tts.engine);
    let started = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let _turn = turn;
        engine.synthesize(&request.text, &request.voice)
    })
    .await;
    match result {
        Ok(Ok(pcm)) => {
            tracing::info!(
                chars,
                audio_ms = pcm.duration_ms(),
                took_ms = started.elapsed().as_millis() as u64,
                "synthesised"
            );
            let mut response = (
                [(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"))],
                pcm.to_wav(),
            )
                .into_response();
            if let Ok(v) = HeaderValue::from_str(&fingerprint) {
                response.headers_mut().insert(FINGERPRINT_HEADER, v);
            }
            response
        }
        Ok(Err(SpeechError::BadText(m))) => error(StatusCode::UNPROCESSABLE_ENTITY, "BAD_TEXT", m),
        Ok(Err(SpeechError::UnknownVoice(v))) => {
            error(StatusCode::UNPROCESSABLE_ENTITY, "UNKNOWN_VOICE", v)
        }
        Ok(Err(SpeechError::Failed(m))) => {
            tracing::error!(error = %m, "synthesis failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SYNTHESIS_FAILED",
                "synthesis failed",
            )
        }
        Err(e) => {
            // The engine panicked; the process carries on for the next request.
            tracing::error!(error = %e, "synthesis panicked");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SYNTHESIS_FAILED",
                "synthesis failed",
            )
        }
    }
}

/// The TTS service's routes.
pub fn router(engine: Arc<dyn SpeechEngine>) -> Router {
    let tts = Tts {
        engine,
        one_at_a_time: Arc::new(Semaphore::new(1)),
    };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/voices", get(voices))
        .route("/synthesize", post(synthesize))
        // JSON-escaped text of MAX_TEXT_CHARS characters fits comfortably.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(tts)
}
