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

/// The byte order of a UTF-16 body, from its byte-order mark or, failing that, the
/// declared charset. UTF-16 text is full of NUL bytes, so it must be decoded before
/// looking for binary content.
fn utf16_order(head: &[u8], charset: Option<&str>) -> Option<Endian> {
    match head {
        [0xFF, 0xFE, ..] => return Some(Endian::Little),
        [0xFE, 0xFF, ..] => return Some(Endian::Big),
        _ => {}
    }
    match charset? {
        "utf-16le" => Some(Endian::Little),
        "utf-16be" => Some(Endian::Big),
        // No BOM and no order given: ASCII-range text has its NULs on the high byte.
        "utf-16" => Some(if head.first() == Some(&0) {
            Endian::Big
        } else {
            Endian::Little
        }),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

/// The start of the body as text, decoding UTF-16 when that's what it is.
fn head_text(head: &[u8], charset: Option<&str>) -> String {
    match utf16_order(head, charset) {
        Some(order) => {
            let units = head.chunks_exact(2).map(|pair| match order {
                Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
                Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
            });
            char::decode_utf16(units)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        }
        None => String::from_utf8_lossy(head).into_owned(),
    }
}

fn looks_binary(head: &[u8], text: &str) -> bool {
    BINARY_MAGIC.iter().any(|m| head.starts_with(m)) || text.contains('\0')
}

fn looks_like_html(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
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
    let text = head_text(head, declared.and_then(charset).as_deref());
    if looks_binary(head, &text) {
        return Err(FetchError::UnsupportedContentType("binary".into()));
    }
    match declared {
        Some(declared) => declared_kind(declared),
        None if looks_like_html(&text) => Ok(Kind::Html),
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

    fn utf16(text: &str, order: Endian, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in (if bom { "\u{feff}" } else { "" })
            .encode_utf16()
            .chain(text.encode_utf16())
        {
            out.extend_from_slice(&match order {
                Endian::Little => unit.to_le_bytes(),
                Endian::Big => unit.to_be_bytes(),
            });
        }
        out
    }

    #[test]
    fn utf16_text_is_not_mistaken_for_binary() {
        let page = "<!doctype html><p>Words.</p>";
        for (order, charset) in [(Endian::Little, "utf-16le"), (Endian::Big, "utf-16be")] {
            let declared = format!("text/html; charset={charset}");
            // Declared charset, with and without a BOM.
            for bom in [false, true] {
                let body = utf16(page, order, bom);
                assert_eq!(reconcile(Some(&declared), &body), Ok(Kind::Html));
            }
            // "utf-16" with no byte order given.
            let body = utf16(page, order, false);
            assert_eq!(
                reconcile(Some("text/html; charset=utf-16"), &body),
                Ok(Kind::Html)
            );
            // A BOM alone, with no type declared, is enough to sniff HTML.
            assert_eq!(reconcile(None, &utf16(page, order, true)), Ok(Kind::Html));
            let plain = utf16("Just words.", order, true);
            assert_eq!(reconcile(Some("text/plain"), &plain), Ok(Kind::PlainText));
        }
    }

    #[test]
    fn utf16_does_not_excuse_binary() {
        // A PNG claiming to be UTF-16 HTML is still refused by its signature.
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        assert!(reconcile(Some("text/html; charset=utf-16le"), png).is_err());
        // NULs inside the decoded text are still binary.
        let body = utf16("<p>a\0b</p>", Endian::Little, true);
        assert!(reconcile(Some("text/html"), &body).is_err());
        // Without a UTF-16 signal, NULs mean binary as before.
        let body = utf16("<p>Words.</p>", Endian::Little, false);
        assert!(reconcile(Some("text/html"), &body).is_err());
    }
}
