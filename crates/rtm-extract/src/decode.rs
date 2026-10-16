// SPDX-License-Identifier: AGPL-3.0-or-later
//! Turning a source's bytes into text: byte-order mark first, then the declared
//! charset, then an HTML `<meta>` declaration, then UTF-8 (WHATWG order, simplified).

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};

/// How far into an HTML document to look for a `<meta>` charset (WHATWG prescan: 1024).
const PRESCAN: usize = 1024;

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0C')
}

/// Reads tag attributes the way the WHATWG prescan does: names and values, with
/// optional whitespace around `=`, values quoted or not.
struct Attrs<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Attrs<'_> {
    fn skip_space(&mut self) {
        while self.at < self.bytes.len() && is_space(self.bytes[self.at]) {
            self.at += 1;
        }
    }

    /// The next `(name, value)`, lowercased, or `None` at the end of the tag.
    fn next(&mut self) -> Option<(String, String)> {
        let b = self.bytes;
        while self.at < b.len() && (is_space(b[self.at]) || b[self.at] == b'/') {
            self.at += 1;
        }
        if self.at >= b.len() || b[self.at] == b'>' {
            return None;
        }
        let start = self.at;
        while self.at < b.len()
            && !is_space(b[self.at])
            && !matches!(b[self.at], b'=' | b'>' | b'/')
        {
            self.at += 1;
        }
        let name = String::from_utf8_lossy(&b[start..self.at]).to_ascii_lowercase();
        self.skip_space();
        if self.at >= b.len() || b[self.at] != b'=' {
            return Some((name, String::new()));
        }
        self.at += 1;
        self.skip_space();
        let value = match b.get(self.at) {
            Some(&q @ (b'"' | b'\'')) => {
                self.at += 1;
                let start = self.at;
                while self.at < b.len() && b[self.at] != q {
                    self.at += 1;
                }
                let v = &b[start..self.at];
                self.at += 1;
                v
            }
            _ => {
                let start = self.at;
                while self.at < b.len() && !is_space(b[self.at]) && b[self.at] != b'>' {
                    self.at += 1;
                }
                &b[start..self.at]
            }
        };
        Some((name, String::from_utf8_lossy(value).to_ascii_lowercase()))
    }
}

/// The charset in a `content` value such as `text/html; charset=ISO-8859-1`.
fn charset_in_content(content: &str) -> Option<&str> {
    let mut rest = content;
    loop {
        let at = rest.find("charset")? + "charset".len();
        rest = rest[at..].trim_start();
        if let Some(after) = rest.strip_prefix('=') {
            let value = after.trim_start().trim_start_matches(['"', '\'']);
            let end = value
                .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
                .unwrap_or(value.len());
            return Some(&value[..end]).filter(|v| !v.is_empty());
        }
    }
}

/// An encoding named in a `<meta>` tag, as the prescan would take it: UTF-16 labels
/// mean UTF-8 (the bytes can't be UTF-16 if we read this far), x-user-defined means
/// windows-1252.
fn meta_encoding(label: &str) -> Option<&'static Encoding> {
    let encoding = Encoding::for_label(label.trim().as_bytes())?;
    Some(if encoding == UTF_16LE || encoding == UTF_16BE {
        UTF_8
    } else if encoding == X_USER_DEFINED {
        WINDOWS_1252
    } else {
        encoding
    })
}

/// The charset declared by the first usable `<meta>` tag in the first 1024 bytes,
/// skipping comments and declarations that name no known encoding.
fn meta_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let b = &bytes[..bytes.len().min(PRESCAN)];
    let mut at = 0;
    while at < b.len() {
        let rest = &b[at..];
        if rest.starts_with(b"<!--") {
            // Skip to the end of the comment ("-->" may share the opening dashes).
            at += rest[2..]
                .windows(3)
                .position(|w| w == b"-->")
                .map_or(b.len(), |p| p + 5);
            continue;
        }
        let is_meta = rest.len() > 5
            && rest[..5].eq_ignore_ascii_case(b"<meta")
            && (is_space(rest[5]) || rest[5] == b'/');
        if !is_meta {
            at += 1;
            continue;
        }
        let mut attrs = Attrs {
            bytes: b,
            at: at + 5,
        };
        let (mut charset, mut content, mut content_type) = (None, None, false);
        while let Some((name, value)) = attrs.next() {
            match name.as_str() {
                "charset" if charset.is_none() => charset = Some(value),
                "content" if content.is_none() => content = Some(value),
                "http-equiv" => content_type |= value == "content-type",
                _ => {}
            }
        }
        let label = charset.or_else(|| {
            content
                .filter(|_| content_type)
                .and_then(|c| charset_in_content(&c).map(str::to_string))
        });
        if let Some(encoding) = label.as_deref().and_then(meta_encoding) {
            return Some(encoding);
        }
        at = attrs.at.max(at + 1);
    }
    None
}

/// Decode `bytes` to text. Malformed sequences become U+FFFD rather than failing.
pub fn decode(bytes: &[u8], declared: Option<&str>, html: bool) -> String {
    let encoding = Encoding::for_bom(bytes)
        .map(|(e, _)| e)
        .or_else(|| declared.and_then(|d| Encoding::for_label(d.as_bytes())))
        .or_else(|| html.then(|| meta_charset(bytes)).flatten())
        .unwrap_or(UTF_8);
    // `decode` strips a matching BOM itself.
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_meta_bom_and_default() {
        // "café" in Latin-1.
        let latin1 = b"caf\xe9";
        assert_eq!(decode(latin1, Some("iso-8859-1"), false), "café");
        assert_eq!(decode(latin1, None, false), "caf\u{FFFD}");
        let meta = b"<meta charset=\"windows-1252\"><p>caf\xe9</p>";
        assert_eq!(
            decode(meta, None, true),
            "<meta charset=\"windows-1252\"><p>café</p>"
        );
        let equiv =
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=ISO-8859-1\">\xe9";
        assert!(decode(equiv, None, true).ends_with('é'));
        // A BOM beats a wrong declaration.
        let utf16 = [0xFF, 0xFE, b'h', 0, b'i', 0];
        assert_eq!(decode(&utf16, Some("utf-8"), true), "hi");
        assert_eq!(decode("naïve".as_bytes(), None, true), "naïve");
    }

    fn sniffed(page: &[u8]) -> Option<&'static str> {
        meta_charset(page).map(Encoding::name)
    }

    #[test]
    fn meta_prescan_follows_html_syntax() {
        // Whitespace around "=", any case, unquoted, single-quoted, self-closing.
        assert_eq!(
            sniffed(b"<meta charset = windows-1252>"),
            Some("windows-1252")
        );
        assert_eq!(sniffed(b"<META CHARSET='ISO-8859-2'/>"), Some("ISO-8859-2"));
        assert_eq!(sniffed(b"<meta\ncharset=\n\"koi8-r\">"), Some("KOI8-R"));
        assert_eq!(
            sniffed(b"<meta content=\"text/html; charset = Shift_JIS\" http-equiv=Content-Type>"),
            Some("Shift_JIS")
        );
    }

    #[test]
    fn meta_prescan_ignores_what_is_not_a_declaration() {
        // Comments, text, other tags and attribute values that merely mention charset.
        assert_eq!(
            sniffed(b"<!-- charset=windows-1252 --><meta charset=utf-8>"),
            Some("UTF-8")
        );
        assert_eq!(sniffed(b"<p>charset=windows-1252</p>"), None);
        assert_eq!(sniffed(b"<metadata charset=koi8-r>"), None);
        assert_eq!(sniffed(b"<meta name=x content=\"charset=koi8-r\">"), None);
        // An unknown label is skipped and the next declaration used.
        assert_eq!(
            sniffed(b"<meta charset=nonsense><meta charset=iso-8859-5>"),
            Some("ISO-8859-5")
        );
        // UTF-16 in <meta> means UTF-8; x-user-defined means windows-1252.
        assert_eq!(sniffed(b"<meta charset=utf-16le>"), Some("UTF-8"));
        assert_eq!(
            sniffed(b"<meta charset=x-user-defined>"),
            Some("windows-1252")
        );
        // Only the first 1024 bytes count.
        let mut late = vec![b' '; 1100];
        late.extend_from_slice(b"<meta charset=koi8-r>");
        assert_eq!(sniffed(&late), None);
    }

    #[test]
    fn codex_reproductions_decode_correctly() {
        let spaced = b"<meta charset = windows-1252><p>caf\xe9</p>";
        assert!(decode(spaced, None, true).ends_with("café</p>"));
        let commented = "<!-- charset=windows-1252 --><meta charset=utf-8><p>café</p>";
        assert!(decode(commented.as_bytes(), None, true).ends_with("café</p>"));
    }
}
