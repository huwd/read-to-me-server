// SPDX-License-Identifier: AGPL-3.0-or-later
//! The extract worker's protocol (ADR-0006 "Sandbox boundary").
//!
//! Request on stdin: a little-endian `u32` length, that many bytes of JSON
//! [`RequestHeader`], then the source document's bytes to end of input.
//! Response on stdout: one JSON [`Response`].
//!
//! The worker is untrusted once it has parsed the document, so the server bounds what
//! it reads back and validates the article again (`Article::validate`).

use std::io::{self, Read, Write};

use rtm_article::Article;
use rtm_domain::SourceKind;
use serde::{Deserialize, Serialize};

use crate::{Failure, Registry, SourceDocument, TriageConfig};

/// Largest header accepted (a URL and a few short fields).
const MAX_HEADER: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestHeader {
    pub kind: SourceKindWire,
    pub charset: Option<String>,
    pub final_url: Option<String>,
    pub min_words: usize,
}

/// `SourceKind` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindWire {
    Html,
    Text,
}

impl From<SourceKind> for SourceKindWire {
    fn from(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Html => SourceKindWire::Html,
            SourceKind::PlainText => SourceKindWire::Text,
        }
    }
}

impl From<SourceKindWire> for SourceKind {
    fn from(kind: SourceKindWire) -> Self {
        match kind {
            SourceKindWire::Html => SourceKind::Html,
            SourceKindWire::Text => SourceKind::PlainText,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Extracted {
        article: Article,
        extractor: String,
        version: u32,
        derived_title: bool,
    },
    /// FR-EXT-05: nothing acceptable. `best_words` is the best attempt's length.
    LowConfidence {
        best_words: usize,
        reasons: Vec<String>,
    },
    /// An empty struct, not a unit variant: serde ignores `deny_unknown_fields` on unit
    /// variants of internally tagged enums, which would let extra fields through.
    Unsupported {},
}

/// Write a request: what the server sends the worker.
pub fn write_request(mut out: impl Write, header: &RequestHeader, source: &[u8]) -> io::Result<()> {
    let header = serde_json::to_vec(header)?;
    let len = u32::try_from(header.len()).map_err(io::Error::other)?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(&header)?;
    out.write_all(source)?;
    out.flush()
}

/// Read a request, refusing a source larger than `max_source` bytes.
pub fn read_request(
    mut input: impl Read,
    max_source: usize,
) -> io::Result<(RequestHeader, Vec<u8>)> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "header too large",
        ));
    }
    let mut header = vec![0u8; len];
    input.read_exact(&mut header)?;
    let header: RequestHeader = serde_json::from_slice(&header)?;
    let mut source = Vec::new();
    input.take(max_source as u64 + 1).read_to_end(&mut source)?;
    if source.len() > max_source {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source too large",
        ));
    }
    Ok((header, source))
}

/// The worker's job: extract the request's document and describe the outcome.
pub fn handle(header: RequestHeader, source: Vec<u8>) -> Response {
    let doc = SourceDocument {
        bytes: source,
        kind: header.kind.into(),
        charset: header.charset,
        final_url: header.final_url.and_then(|u| u.parse().ok()),
    };
    let registry = Registry::builtin(TriageConfig {
        min_words: header.min_words,
        ..TriageConfig::default()
    });
    match registry.extract(&doc) {
        Ok(e) => Response::Extracted {
            article: e.article,
            extractor: e.extractor.to_string(),
            version: e.version,
            derived_title: e.derived_title,
        },
        Err(Failure::LowConfidence { best, reasons }) => Response::LowConfidence {
            best_words: best.map_or(0, |b| b.words),
            reasons,
        },
        Err(Failure::Unsupported) => Response::Unsupported {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> RequestHeader {
        RequestHeader {
            kind: SourceKindWire::Text,
            charset: Some("utf-8".into()),
            final_url: Some("https://example.test/a".into()),
            min_words: 5,
        }
    }

    #[test]
    fn requests_round_trip() {
        let mut wire = Vec::new();
        write_request(
            &mut wire,
            &header(),
            b"Title\n\none two three four five six",
        )
        .unwrap();
        let (h, source) = read_request(wire.as_slice(), 1024).unwrap();
        assert_eq!(h, header());
        assert_eq!(source, b"Title\n\none two three four five six");
        let Response::Extracted {
            article, extractor, ..
        } = handle(h, source)
        else {
            panic!("expected an article");
        };
        assert_eq!(
            (article.title.as_str(), extractor.as_str()),
            ("Title", "plain-text")
        );
    }

    #[test]
    fn oversized_or_malformed_requests_are_refused() {
        let mut wire = Vec::new();
        write_request(&mut wire, &header(), &[b'a'; 100]).unwrap();
        assert!(read_request(wire.as_slice(), 99).is_err());
        let mut huge = (MAX_HEADER as u32 + 1).to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(read_request(huge.as_slice(), 1024).is_err());
        let mut junk = 2u32.to_le_bytes().to_vec();
        junk.extend_from_slice(b"{]");
        assert!(read_request(junk.as_slice(), 1024).is_err());
        assert!(read_request(&b"\x01"[..], 1024).is_err());
    }

    #[test]
    fn responses_are_tagged() {
        let json = serde_json::to_string(&Response::Unsupported {}).unwrap();
        assert_eq!(json, r#"{"result":"unsupported"}"#);
        assert_eq!(
            serde_json::from_str::<Response>(&json).unwrap(),
            Response::Unsupported {}
        );
        // Extra fields are refused on every variant.
        for extra in [
            r#"{"result":"unsupported","x":1}"#,
            r#"{"result":"low_confidence","best_words":1,"reasons":[],"x":1}"#,
        ] {
            assert!(serde_json::from_str::<Response>(extra).is_err(), "{extra}");
        }
        let low = Response::LowConfidence {
            best_words: 3,
            reasons: vec!["x".into()],
        };
        let back: Response = serde_json::from_str(&serde_json::to_string(&low).unwrap()).unwrap();
        assert_eq!(back, low);
    }
}
