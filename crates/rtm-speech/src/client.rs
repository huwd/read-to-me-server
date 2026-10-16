// SPDX-License-Identifier: AGPL-3.0-or-later
//! The server's client for the `rtm tts` process (ADR-0007).
//!
//! The TTS process lives at an operator-configured internal address, not a user-chosen
//! URL, so this doesn't go through the egress guard (which would rightly refuse a
//! private address). It's a minimal HTTP/1.1 client on hyper, one connection per
//! request: requests are long and few.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::Pcm;
use crate::protocol::{FINGERPRINT_HEADER, SynthesizeRequest, VoicesResponse};

/// Largest response accepted: 4,000 characters of speech is a few minutes of 24 kHz
/// audio, well under this.
const MAX_RESPONSE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The TTS process couldn't be reached or didn't answer in time; worth retrying.
    #[error("TTS service unreachable: {0}")]
    Unreachable(String),
    /// The service refused the request (bad text, unknown voice).
    #[error("TTS service refused the request ({status}): {code}: {message}")]
    Refused {
        status: u16,
        code: String,
        message: String,
    },
    /// The service failed, or answered with something unusable.
    #[error("TTS service error: {0}")]
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TtsClient {
    host: String,
    port: u16,
    timeout: Duration,
}

impl TtsClient {
    /// A client for `url`, which must be `http://host:port` (with an optional trailing
    /// slash). `timeout` bounds each whole request.
    pub fn new(url: &str, timeout: Duration) -> Result<Self, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("{url:?}: the TTS URL must start with http://"))?;
        let authority = rest.trim_end_matches('/');
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| format!("{url:?}: the TTS URL needs a port"))?;
        let port = port
            .parse()
            .map_err(|_| format!("{url:?}: {port:?} is not a port"))?;
        if host.is_empty() || host.contains('/') {
            return Err(format!("{url:?}: expected http://host:port"));
        }
        Ok(Self {
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string(),
            port,
            timeout,
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(StatusCode, hyper::HeaderMap, Bytes), ClientError> {
        let exchange = async {
            let stream = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(|e| ClientError::Unreachable(e.to_string()))?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|e| ClientError::Unreachable(e.to_string()))?;
            tokio::spawn(connection);
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(HOST, format!("{}:{}", self.host, self.port));
            if body.is_some() {
                request = request.header(CONTENT_TYPE, "application/json");
            }
            let request = request
                .body(Full::new(Bytes::from(body.unwrap_or_default())))
                .map_err(|e| ClientError::Failed(e.to_string()))?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|e| ClientError::Unreachable(e.to_string()))?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = Limited::new(response.into_body(), MAX_RESPONSE)
                .collect()
                .await
                .map_err(|e| ClientError::Failed(format!("reading the response: {e}")))?
                .to_bytes();
            Ok((status, headers, body))
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .unwrap_or_else(|_| Err(ClientError::Unreachable("timed out".into())))
    }

    fn refused(status: StatusCode, body: &[u8]) -> ClientError {
        let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        let text = |k: &str| json[k].as_str().unwrap_or_default().to_string();
        if status.is_server_error() {
            ClientError::Failed(format!("{status}: {}", text("code")))
        } else {
            ClientError::Refused {
                status: status.as_u16(),
                code: text("code"),
                message: text("message"),
            }
        }
    }

    pub async fn voices(&self) -> Result<VoicesResponse, ClientError> {
        let (status, _, body) = self.send(Method::GET, "/voices", None).await?;
        if !status.is_success() {
            return Err(Self::refused(status, &body));
        }
        serde_json::from_slice(&body).map_err(|e| ClientError::Failed(format!("bad voices: {e}")))
    }

    /// Speak `text` with `voice`, returning the audio and the engine fingerprint that
    /// produced it.
    pub async fn synthesize(&self, text: &str, voice: &str) -> Result<(Pcm, String), ClientError> {
        let request = serde_json::to_vec(&SynthesizeRequest {
            text: text.to_string(),
            voice: voice.to_string(),
        })
        .map_err(|e| ClientError::Failed(e.to_string()))?;
        let (status, headers, body) = self
            .send(Method::POST, "/synthesize", Some(request))
            .await?;
        if !status.is_success() {
            return Err(Self::refused(status, &body));
        }
        let fingerprint = headers
            .get(FINGERPRINT_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ClientError::Failed("no engine fingerprint in the response".into()))?
            .to_string();
        let pcm = Pcm::from_wav(&body).map_err(|e| ClientError::Failed(e.to_string()))?;
        Ok((pcm, fingerprint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let ok = |u: &str| TtsClient::new(u, Duration::from_secs(1)).map(|c| (c.host, c.port));
        assert_eq!(ok("http://127.0.0.1:8090"), Ok(("127.0.0.1".into(), 8090)));
        assert_eq!(ok("http://tts:8090/"), Ok(("tts".into(), 8090)));
        assert_eq!(ok("http://[::1]:8090"), Ok(("::1".into(), 8090)));
        for bad in [
            "https://tts:8090",
            "tts:8090",
            "http://tts",
            "http://tts:x",
            "http://:80",
            "http://tts:80/path",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
    }
}
