// SPDX-License-Identifier: AGPL-3.0-or-later
//! Splitting text into sentences, and sentences into pieces of bounded length
//! (FR-TTS-04). Pieces are the unit of synthesis caching and crash resumption; engines
//! split pieces further into model-sized chunks themselves, since only they can count
//! phonemes.

/// Characters that end a sentence when followed by whitespace or the end of the text.
fn is_terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}

/// Characters that may trail a terminal and still belong to the sentence.
fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | '”' | '’' | ')' | ']')
}

/// Abbreviations that end with a full stop but rarely end a sentence, lower case and
/// without the final stop.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "st", "sr", "jr", "rev", "gen", "col", "capt", "lt", "sgt",
    "hon", "e.g", "i.e", "cf", "vs", "etc", "approx", "vol", "fig", "p", "pp", "ch",
];

/// The word ending at byte `end` (exclusive), without leading brackets or quotes.
fn word_before(text: &str, end: usize) -> &str {
    let start = text[..end].rfind(char::is_whitespace).map_or(0, |i| {
        i + text[i..].chars().next().map_or(1, char::len_utf8)
    });
    text[start..end].trim_start_matches(['(', '[', '"', '\'', '“', '‘'])
}

/// Sentences in `text`, trimmed. A sentence ends at `.`, `!`, `?` or `…` (with any
/// closing quotes or brackets) followed by whitespace, except where the next word
/// starts in lower case or a full stop ends a common abbreviation ("Dr.", "e.g."), so
/// "3.5", "Dr. Smith" and "e.g. this" stay whole.
pub fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        if is_terminal(chars[i].1) {
            let mut j = i + 1;
            while j < chars.len() && (is_terminal(chars[j].1) || is_closer(chars[j].1)) {
                j += 1;
            }
            let next_word = chars[j..].iter().map(|(_, c)| *c).find(|c| {
                !c.is_whitespace() && !is_closer(*c) && !matches!(c, '(' | '[' | '“' | '‘')
            });
            let continues = next_word.is_some_and(char::is_lowercase)
                || (chars[i].1 == '.'
                    && ABBREVIATIONS
                        .contains(&word_before(text, chars[i].0).to_lowercase().as_str()));
            if (j == chars.len() || chars[j].1.is_whitespace()) && !continues {
                let end = chars.get(j).map_or(text.len(), |(at, _)| *at);
                out.push(text[start..end].trim());
                start = end;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out.push(text[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// `text` packed into pieces of at most `max_chars` characters, breaking between
/// sentences. A sentence longer than `max_chars` is broken between words, and a word
/// longer than that stands alone.
pub fn pieces(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let len = |s: &str| s.chars().count();
    let push = |unit: &str, current: &mut String, out: &mut Vec<String>| {
        if current.is_empty() {
            current.push_str(unit);
        } else if len(current) + 1 + len(unit) <= max_chars {
            current.push(' ');
            current.push_str(unit);
        } else {
            out.push(std::mem::take(current));
            current.push_str(unit);
        }
    };
    for sentence in sentences(text) {
        if len(sentence) <= max_chars {
            push(sentence, &mut current, &mut out);
        } else {
            for word in sentence.split_whitespace() {
                push(word, &mut current, &mut out);
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
    fn sentence_boundaries() {
        assert_eq!(
            sentences("It rose 3.5% today. Really?! \"Yes,\" he said. (Quite.) Done…"),
            [
                "It rose 3.5% today.",
                "Really?!",
                "\"Yes,\" he said.",
                "(Quite.)",
                "Done…"
            ]
        );
        assert_eq!(sentences("No terminal at all"), ["No terminal at all"]);
        assert_eq!(sentences("Use e.g. this one."), ["Use e.g. this one."]);
        assert_eq!(
            sentences("Dr. Smith met Mr. Jones at St. Paul's. Then they left."),
            ["Dr. Smith met Mr. Jones at St. Paul's.", "Then they left."]
        );
        assert_eq!(
            sentences("See pp. 19 and fig. 3. Next."),
            ["See pp. 19 and fig. 3.", "Next."]
        );
        // A lower-case continuation isn't a new sentence; a capital after a real word is.
        assert_eq!(
            sentences("It cost $5 etc. and more."),
            ["It cost $5 etc. and more."]
        );
        assert_eq!(
            sentences("I went home. It rained."),
            ["I went home.", "It rained."]
        );
        // "no." usually ends a sentence, so it isn't treated as an abbreviation.
        assert_eq!(
            sentences("I said no. Then we left."),
            ["I said no.", "Then we left."]
        );
        assert!(sentences("   ").is_empty());
        assert_eq!(sentences("“Quoted.” Next."), ["“Quoted.”", "Next."]);
    }

    #[test]
    fn pieces_pack_sentences_within_the_limit() {
        let text = "One two. Three four. Five six seven.";
        assert_eq!(pieces(text, 100), ["One two. Three four. Five six seven."]);
        assert_eq!(
            pieces(text, 20),
            ["One two. Three four.", "Five six seven."]
        );
        // An overlong sentence breaks between words.
        assert_eq!(pieces("aaa bbb ccc ddd.", 7), ["aaa bbb", "ccc", "ddd."]);
        // A word longer than the limit stands alone rather than being cut.
        assert_eq!(
            pieces("x supercalifragilistic y", 5),
            ["x", "supercalifragilistic", "y"]
        );
        // Every piece respects the limit unless a single word exceeds it.
        let long = "word ".repeat(1000);
        assert!(pieces(&long, 300).iter().all(|p| p.chars().count() <= 300));
        assert_eq!(pieces(&long, 300).join(" "), long.trim());
    }
}
