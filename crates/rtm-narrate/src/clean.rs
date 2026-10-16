// SPDX-License-Identifier: AGPL-3.0-or-later
//! Language-independent clean-up of text before it is spoken (part of FR-TTS-02):
//! Unicode NFC, URLs read as their domain, emoji and pictographs removed, typographic
//! quotes and dashes made plain, whitespace collapsed.

use unicode_normalization::UnicodeNormalization;

/// Pictographic and emoji characters (and their joiners and modifiers), which a speech
/// engine would either skip or read as noise.
fn is_pictograph(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF   // mahjong, cards, emoji, pictographs, skin tones
        | 0x2600..=0x27BF   // miscellaneous symbols, dingbats
        | 0x2B00..=0x2BFF   // arrows and symbols
        | 0xFE00..=0xFE0F   // variation selectors
        | 0x200D            // zero-width joiner
        | 0xE0020..=0xE007F // tag characters (flags)
        | 0x20E3) // combining keycap
}

/// The domain of a URL-like token ("https://www.example.org/a/b?c" → "example.org"),
/// or `None` if the token isn't a URL.
fn url_domain(token: &str) -> Option<String> {
    // Keep punctuation around the URL: "(see https://x.test/a)." → "(see x.test).".
    let opening = token.len() - token.trim_start_matches(['(', '[', '"', '\'']).len();
    let (lead, token) = token.split_at(opening);
    let trimmed = token.trim_end_matches(['.', ',', ';', ':', ')', ']', '"', '\'', '!', '?']);
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("www.").map(|_| trimmed))?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    let host = host.rsplit('@').next()?.split(':').next()?;
    (host.contains('.')
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-')))
    .then(|| format!("{lead}{host}{}", &token[trimmed.len()..]))
}

/// Text as it should be handed to the speech engine.
pub fn speakable(text: &str) -> String {
    let nfc: String = text.nfc().collect();
    let plain: String = nfc
        .chars()
        .filter(|c| !is_pictograph(*c))
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => '"',
            '\u{00A0}' | '\u{2007}' | '\u{202F}' => ' ',
            other => other,
        })
        .collect();
    let words: Vec<String> = plain
        .split_whitespace()
        .map(|w| url_domain(w).unwrap_or_else(|| w.to_string()))
        .collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_read_as_domains() {
        assert_eq!(
            speakable("See https://www.example.org/a/b?c=1 for more."),
            "See example.org for more."
        );
        assert_eq!(speakable("Visit www.gov.uk/guidance."), "Visit gov.uk.");
        assert_eq!(
            speakable("(see http://user@site.test:8080/x)."),
            "(see site.test)."
        );
        assert_eq!(speakable("[https://a.test/b]"), "[a.test]");
        assert_eq!(speakable("\"https://a.test/\","), "\"a.test\",");
        // Not URLs.
        assert_eq!(speakable("e.g. 3.5 per cent"), "e.g. 3.5 per cent");
    }

    #[test]
    fn emoji_are_removed() {
        assert_eq!(speakable("Great news 🎉🎉 today 👍🏽!"), "Great news today !");
        assert_eq!(speakable("Flag 🇬🇧 and family 👨‍👩‍👧"), "Flag and family");
        assert_eq!(speakable("Keycap 1️⃣ done"), "Keycap 1 done");
    }

    #[test]
    fn quotes_spaces_and_normal_form() {
        assert_eq!(speakable("“Don’t,” she said"), "\"Don't,\" she said");
        assert_eq!(speakable("10\u{00A0}km\u{202F}away"), "10 km away");
        // Decomposed "é" becomes the composed form.
        assert_eq!(speakable("cafe\u{0301}"), "café");
        assert_eq!(speakable("  spaced   out  "), "spaced out");
    }
}
