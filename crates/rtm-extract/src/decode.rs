// SPDX-License-Identifier: AGPL-3.0-or-later
//! Turning a source's bytes into text: byte-order mark first, then the declared
//! charset, then an HTML `<meta>` declaration, then UTF-8 (WHATWG order, simplified).

use encoding_rs::{Encoding, UTF_8};

/// How far into an HTML document to look for a `<meta charset>` (WHATWG prescan: 1024).
const PRESCAN: usize = 1024;

/// The charset declared in an HTML `<meta charset=…>` or `<meta http-equiv … content=…
/// charset=…>` near the start of the document.
fn meta_charset(head: &[u8]) -> Option<&'static Encoding> {
    let text = String::from_utf8_lossy(&head[..head.len().min(PRESCAN)]).to_ascii_lowercase();
    let at = text.find("charset=")? + "charset=".len();
    let label: String = text[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    Encoding::for_label(label.as_bytes())
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
}
