// SPDX-License-Identifier: AGPL-3.0-or-later
//! The TTS service's HTTP contract, with the fake engine.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use rtm_speech::protocol::{FINGERPRINT_HEADER, VoicesResponse};
use rtm_speech::{FakeEngine, Pcm, SpeechEngine, SpeechError, Voice};
use tower::ServiceExt;

async fn send(
    router: axum::Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

fn synth(body: &str) -> Request<Body> {
    Request::post("/synthesize")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn fake() -> axum::Router {
    rtm_tts::router(Arc::new(FakeEngine))
}

#[tokio::test]
async fn synthesize_returns_wav_and_the_fingerprint() {
    let (status, headers, body) = send(
        fake(),
        synth(r#"{"text":"Canals are old. Locks are clever.","voice":"bf_emma"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/wav");
    assert_eq!(headers[FINGERPRINT_HEADER], "fake@1/bf_emma");
    let pcm = Pcm::from_wav(&body).unwrap();
    assert_eq!(pcm.sample_rate, 24_000);
    assert_eq!(
        pcm,
        FakeEngine
            .synthesize("Canals are old. Locks are clever.", "bf_emma")
            .unwrap()
    );
}

#[tokio::test]
async fn bad_requests_are_refused() {
    let long = format!(r#"{{"text":"{}","voice":"bf_emma"}}"#, "a".repeat(4_001));
    for (body, status, code) in [
        (
            r#"{"text":"Hi.","voice":"nobody"}"#.to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "UNKNOWN_VOICE",
        ),
        (
            r#"{"text":"   ","voice":"bf_emma"}"#.to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "BAD_TEXT",
        ),
        (long, StatusCode::UNPROCESSABLE_ENTITY, "BAD_TEXT"),
        (
            r#"{"text":"Hi.","voice":"bf_emma","ssml":"<x/>"}"#.to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_REQUEST",
        ),
        (
            "not json".to_string(),
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
        ),
    ] {
        let (got, _, body) = send(fake(), synth(&body)).await;
        assert_eq!(got, status, "{}", String::from_utf8_lossy(&body));
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], code);
    }
}

#[tokio::test]
async fn voices_and_health() {
    let (status, _, body) =
        send(fake(), Request::get("/voices").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let voices: VoicesResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (voices.engine.as_str(), voices.sample_rate),
        ("fake", 24_000)
    );
    assert!(
        voices
            .voices
            .iter()
            .any(|v| v.voice.id == "bf_emma" && v.fingerprint == "fake@1/bf_emma")
    );
    let (status, _, body) = send(
        fake(),
        Request::get("/healthz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("\"engine\":\"fake\""));
}

/// An engine that records how many syntheses overlap, and can be told to fail or panic.
struct Probe {
    running: AtomicUsize,
    most: AtomicUsize,
}

impl SpeechEngine for Probe {
    fn id(&self) -> &str {
        "probe"
    }
    fn fingerprint(&self, _: &str) -> Option<String> {
        Some("probe@1".into())
    }
    fn voices(&self) -> Vec<Voice> {
        vec![Voice {
            id: "v".into(),
            lang: "en".into(),
        }]
    }
    fn sample_rate(&self) -> u32 {
        8_000
    }
    fn synthesize(&self, text: &str, _: &str) -> Result<Pcm, SpeechError> {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(30));
        self.running.fetch_sub(1, Ordering::SeqCst);
        match text {
            "panic" => panic!("engine bug"),
            "fail" => Err(SpeechError::Failed("model error".into())),
            _ => Ok(Pcm {
                sample_rate: 8_000,
                samples: vec![0; 80],
            }),
        }
    }
}

#[tokio::test]
async fn one_synthesis_at_a_time_and_failures_are_contained() {
    let probe = Arc::new(Probe {
        running: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let router = rtm_tts::router(probe.clone());
    let calls: Vec<_> = (0..6)
        .map(|_| send(router.clone(), synth(r#"{"text":"ok","voice":"v"}"#)))
        .collect();
    for (status, _, _) in futures_join(calls).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(probe.most.load(Ordering::SeqCst), 1, "syntheses overlapped");

    for text in ["fail", "panic"] {
        let (status, _, body) = send(
            router.clone(),
            synth(&format!(r#"{{"text":"{text}","voice":"v"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
        assert!(
            !String::from_utf8_lossy(&body).contains("model error"),
            "detail leaked"
        );
    }
    // Still serving after a panic.
    let (status, _, _) = send(router, synth(r#"{"text":"ok","voice":"v"}"#)).await;
    assert_eq!(status, StatusCode::OK);
}

async fn futures_join<F>(futures: Vec<F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}
