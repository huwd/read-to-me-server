// SPDX-License-Identifier: AGPL-3.0-or-later
//! Text to Kokoro phonemes, ported from the Phase 0 spike (ADR-0007 Amendment 1):
//! normalising the characters misaki-rs doesn't know, pronunciation overrides as
//! Misaki inline syntax, mapping espeak-style IPA to Kokoro's single-symbol
//! diphthongs, and packing sentences into chunks the model can take.

use rtm_speech::segment::sentences;

/// Typographic quotes and apostrophes to ASCII (misaki-rs marks them unknown), odd spaces
/// to plain ones, and invisible characters removed.
pub fn normalise(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{02BC}' | '\u{2032}' => Some('\''),
            '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' | '\u{00AB}' | '\u{00BB}' => {
                Some('"')
            }
            '\u{00A0}' | '\u{2009}' | '\u{200A}' | '\u{202F}' => Some(' '),
            '\u{200B}' | '\u{00AD}' => None,
            other => Some(other),
        })
        .collect()
}

/// Pronunciation overrides: `word<TAB>phonemes` lines; blank lines and `#` comments are
/// ignored.
pub fn parse_overrides(tsv: &str) -> Vec<(String, String)> {
    tsv.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (word, phonemes) = l.split_once('\t')?;
            Some((word.trim().to_string(), phonemes.trim().to_string()))
        })
        .collect()
}

/// Rewrite whole-word matches of each override as Misaki inline pronunciations:
/// `[word](/phonemes/)`.
pub fn apply_overrides(text: &str, overrides: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (word, phonemes) in overrides {
        let mut result = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(word.as_str()) {
            let before = rest[..i].chars().next_back();
            let after = rest[i + word.len()..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            result.push_str(&rest[..i]);
            if boundary(before) && boundary(after) {
                result.push_str(&format!("[{word}](/{phonemes}/)"));
            } else {
                result.push_str(word);
            }
            rest = &rest[i + word.len()..];
        }
        result.push_str(rest);
        out = result;
    }
    out
}

/// Map IPA diphthongs and affricates (as espeak writes them, joined with U+200D or not)
/// to the single symbols in Kokoro's vocabulary, following Misaki's conventions.
///
/// Only sequences joined by U+200D (espeak's notation) are rewritten, plus bare pairs
/// that never occur in Misaki's own notation. Plain "əl", "iə" and so on are left alone:
/// in the upstream lexicon they are ordinary sequences, not diphthongs.
pub fn kokoro_symbols(phonemes: &str) -> String {
    let mapped = [
        ("e\u{200D}ɪ", "A"),
        ("a\u{200D}ɪ", "I"),
        ("ə\u{200D}ʊ", "Q"),
        ("o\u{200D}ʊ", "O"),
        ("a\u{200D}ʊ", "W"),
        ("ɔ\u{200D}ɪ", "Y"),
        ("d\u{200D}ʒ", "ʤ"),
        ("t\u{200D}ʃ", "ʧ"),
        ("ə\u{200D}l", "ᵊl"),
        ("i\u{200D}ə", "ɪə"),
        ("e\u{200D}ə", "ɛː"),
        ("ʊ\u{200D}ə", "ʊə"),
        ("eɪ", "A"),
        ("aɪ", "I"),
        ("əʊ", "Q"),
        ("oʊ", "O"),
        ("aʊ", "W"),
        ("ɔɪ", "Y"),
        ("dʒ", "ʤ"),
        ("tʃ", "ʧ"),
    ]
    .iter()
    .fold(phonemes.to_string(), |acc, (from, to)| {
        acc.replace(from, to)
    });
    tidy_spacing(&mapped.replace('\u{200D}', ""))
}

/// Misaki's (Python) layout: single spaces, closing punctuation attached to the previous
/// word, em dashes spaced.
fn tidy_spacing(phonemes: &str) -> String {
    let mut out = String::with_capacity(phonemes.len());
    for word in phonemes.split_whitespace() {
        let punctuation_only = word.chars().all(|c| ".,;:!?…\"()".contains(c));
        if !out.is_empty() && !punctuation_only {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Pack `text`'s sentences into chunks whose phonemes fit the model (`fits`), as the
/// spike did: whole sentences where possible, an overlong sentence split between words.
pub fn chunks(text: &str, fits: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let joined = |a: &str, b: &str| {
        if a.is_empty() {
            b.to_string()
        } else {
            format!("{a} {b}")
        }
    };
    for sentence in sentences(text) {
        let candidate = joined(&current, sentence);
        if fits(&candidate) {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if fits(sentence) {
            current = sentence.to_string();
            continue;
        }
        for word in sentence.split_whitespace() {
            let candidate = joined(&current, word);
            if fits(&candidate) {
                current = candidate;
            } else {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                current = word.to_string();
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalising_quotes_spaces_and_invisibles() {
        assert_eq!(
            normalise("“Don’t,” she said\u{00A0}—\u{200B} co\u{00AD}op"),
            "\"Don't,\" she said — coop"
        );
    }

    #[test]
    fn overrides_apply_to_whole_words_only() {
        let o = parse_overrides("# comment\n\nLanchester\tlˈanʧɪstə\nOmicron\tˈQmɪkɹɒn\n");
        assert_eq!(o.len(), 2);
        assert_eq!(
            apply_overrides("John Lanchester on Omicron, not Lanchesters.", &o),
            "John [Lanchester](/lˈanʧɪstə/) on [Omicron](/ˈQmɪkɹɒn/), not Lanchesters."
        );
    }

    #[test]
    fn espeak_diphthongs_become_kokoro_symbols() {
        assert_eq!(kokoro_symbols("ha\u{200D}ʊs"), "hWs");
        assert_eq!(kokoro_symbols("d\u{200D}ʒʌmp"), "ʤʌmp");
        assert_eq!(kokoro_symbols("bɔɪ"), "bY");
        // Plain sequences in Misaki's own notation are left alone.
        assert_eq!(kokoro_symbols("hˈɪəɹ"), "hˈɪəɹ");
        // Spacing follows Misaki: punctuation attaches, em dashes stay spaced.
        assert_eq!(kokoro_symbols("wʌn  ,  tuː — θɹiː ."), "wʌn, tuː — θɹiː.");
    }

    #[test]
    fn chunks_pack_sentences_and_split_overlong_ones() {
        let fits = |s: &str| s.len() <= 20;
        assert_eq!(
            chunks("One two. Three. Four five six.", fits),
            ["One two. Three.", "Four five six."]
        );
        assert_eq!(
            chunks("aaaa bbbb cccc dddd eeee ffff.", fits),
            ["aaaa bbbb cccc dddd", "eeee ffff."]
        );
        assert!(chunks("", fits).is_empty());
    }

    #[test]
    fn the_shipped_overrides_parse() {
        let o = parse_overrides(include_str!("../pronunciations.tsv"));
        assert!(o.iter().any(|(w, _)| w == "Lanchester"));
        assert!(o.iter().all(|(w, p)| !w.is_empty() && !p.is_empty()));
    }
}
