// SPDX-License-Identifier: AGPL-3.0-or-later
//! Language-independent clean-up of text before it is spoken (part of FR-TTS-02):
//! Unicode NFC, URLs read as their domain, emoji and pictographs removed, typographic
//! quotes and dashes made plain, whitespace collapsed.

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{EmojiStatus, UnicodeEmoji};

/// Emoji variation selector 16: "show the previous character as emoji".
const EMOJI_SELECTOR: char = '\u{FE0F}';

/// Characters that only exist to build emoji sequences: variation selectors, the
/// zero-width joiner, the combining keycap and tag characters (subdivision flags).
fn is_emoji_glue(c: char) -> bool {
    matches!(
        c,
        '\u{FE0E}' | '\u{FE0F}' | '\u{200D}' | '\u{20E3}' | '\u{E0020}'..='\u{E007F}'
    )
}

/// Remove emoji, keeping text symbols that carry meaning (♯, ♭, ✕, ©, ™, digits).
///
/// By Unicode emoji properties, not by block: a character goes if it is shown as emoji
/// by default (`Emoji_Presentation`, which also covers skin tones and flag letters), or
/// if a text-default emoji character is followed by the emoji selector (U+FE0F), like
/// "❤️" — except a keycap base ("1️⃣" keeps its "1"). Sequence glue goes too.
fn strip_emoji(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if is_emoji_glue(c) {
            continue;
        }
        let shown_as_emoji = matches!(
            c.emoji_status(),
            EmojiStatus::EmojiPresentation
                | EmojiStatus::EmojiPresentationAndModifierBase
                | EmojiStatus::EmojiPresentationAndEmojiComponent
                | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
        );
        let keycap_base = c.is_ascii_digit() || matches!(c, '#' | '*');
        let selected_as_emoji =
            c.is_emoji_char() && !keycap_base && chars.get(i + 1) == Some(&EMOJI_SELECTOR);
        if !(shown_as_emoji || selected_as_emoji) {
            out.push(c);
        }
    }
    out
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
    let plain: String = strip_emoji(&nfc)
        .chars()
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
        assert_eq!(
            speakable("Scotland 🏴\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F} wins"),
            "Scotland wins"
        );
        assert_eq!(
            speakable("Rainbow 🏳\u{FE0F}\u{200D}🌈 flag"),
            "Rainbow flag"
        );
        assert_eq!(speakable("# hashtag *star*"), "# hashtag *star*");
    }

    /// Codex's reproductions on #38: text symbols near emoji in the code space carry
    /// meaning and must reach the normaliser intact.
    #[test]
    fn meaningful_symbols_are_kept() {
        assert_eq!(speakable("6✕7=42"), "6✕7=42");
        assert_eq!(
            speakable("The piece is in C♯ minor."),
            "The piece is in C♯ minor."
        );
        assert_eq!(
            speakable("Use ♭ to lower the note."),
            "Use ♭ to lower the note."
        );
        for kept in [
            "© 2026",
            "Brand™",
            "→ next",
            "★★★",
            "☐ to do",
            "x² ≤ 4",
            "♠ ♥",
            "№ 5",
        ] {
            assert_eq!(speakable(kept), kept);
        }
        // A text-default symbol becomes emoji only when the selector asks for it.
        assert_eq!(speakable("I ❤ it"), "I ❤ it");
        assert_eq!(speakable("I ❤\u{FE0F} it"), "I it");
        assert_eq!(speakable("Done ✔\u{FE0F}"), "Done");
        // Text presentation is kept, minus the selector.
        assert_eq!(speakable("Phone ☎\u{FE0E}"), "Phone ☎");
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
