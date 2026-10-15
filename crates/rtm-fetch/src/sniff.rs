// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content types: the allow-list, and checking the body agrees with the declared type
//! (FR-FET-04).

use crate::FetchError;

/// What a fetched article is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Html,
    PlainText,
}

/// The media type of a `Content-Type` value, lowercased, without parameters.
fn essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The `charset` parameter of a `Content-Type` value.
pub fn charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|param| {
        let (name, value) = param.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
            .filter(|v| !v.is_empty())
    })
}

fn declared_kind(content_type: &str) -> Result<Kind, FetchError> {
    match essence(content_type).as_str() {
        "text/html" | "application/xhtml+xml" => Ok(Kind::Html),
        "text/plain" => Ok(Kind::PlainText),
        // PDF is on the FR-FET-04 list but needs the upload work's limits and extractor.
        other => Err(FetchError::UnsupportedContentType(if other.is_empty() {
            "unknown".into()
        } else {
            other.into()
        })),
    }
}

/// Refuse a declared type before downloading the body.
pub fn check_declared(content_type: &str) -> Result<(), FetchError> {
    declared_kind(content_type).map(|_| ())
}

/// Signatures of common binary formats a page might really be.
const BINARY_MAGIC: &[&[u8]] = &[
    b"%PDF-",
    b"\x89PNG",
    b"GIF8",
    b"\xFF\xD8\xFF",
    b"PK\x03\x04",
    b"\x1F\x8B",
    b"RIFF",
    b"\x00\x00\x01\x00",
    b"ID3",
    b"OggS",
    b"\x7FELF",
    b"MZ",
];

fn looks_binary(head: &[u8]) -> bool {
    BINARY_MAGIC.iter().any(|m| head.starts_with(m)) || head.contains(&0)
}

fn looks_like_html(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let text = text.trim_start_matches('\u{feff}').trim_start();
    [
        "<!doctype html",
        "<html",
        "<head",
        "<body",
        "<article",
        "<?xml",
    ]
    .iter()
    .any(|tag| text.starts_with(tag))
}

/// The article's kind, from the declared type checked against the body, or from the
/// body alone when nothing was declared.
pub fn reconcile(declared: Option<&str>, body: &[u8]) -> Result<Kind, FetchError> {
    let head = &body[..body.len().min(1024)];
    if looks_binary(head) {
        return Err(FetchError::UnsupportedContentType("binary".into()));
    }
    match declared {
        Some(declared) => declared_kind(declared),
        None if looks_like_html(head) => Ok(Kind::Html),
        None => Ok(Kind::PlainText),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types() {
        assert_eq!(declared_kind("text/html; charset=UTF-8"), Ok(Kind::Html));
        assert_eq!(declared_kind("Application/XHTML+XML"), Ok(Kind::Html));
        assert_eq!(declared_kind("text/plain"), Ok(Kind::PlainText));
        for bad in [
            "image/png",
            "application/pdf",
            "application/json",
            "",
            "text/javascript",
        ] {
            assert!(declared_kind(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn charsets() {
        assert_eq!(
            charset("text/html; charset=\"ISO-8859-1\""),
            Some("iso-8859-1".into())
        );
        assert_eq!(charset("text/html;CHARSET=utf-8 "), Some("utf-8".into()));
        assert_eq!(charset("text/html"), None);
    }

    #[test]
    fn the_body_must_agree() {
        assert!(reconcile(Some("text/html"), b"\x89PNG\r\n\x1a\n...").is_err());
        assert!(reconcile(Some("text/html"), b"%PDF-1.7").is_err());
        assert!(reconcile(Some("text/plain"), b"hello\0world").is_err());
        assert_eq!(
            reconcile(Some("text/html"), b"<!DOCTYPE html><p>hi"),
            Ok(Kind::Html)
        );
        assert_eq!(
            reconcile(None, b"\xEF\xBB\xBF  <!doctype html>"),
            Ok(Kind::Html)
        );
        assert_eq!(reconcile(None, b"Just words."), Ok(Kind::PlainText));
    }
}
