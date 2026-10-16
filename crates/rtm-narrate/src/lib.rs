// SPDX-License-Identifier: AGPL-3.0-or-later
//! The narrator (FR-TTS-01): turns an [`Article`] into a [`Script`], the plain-text
//! utterances and pauses the speech engine reads.
//!
//! Scripts are plain text only (FR-TTS-05): nothing from the page is passed to the
//! engine as markup. Spoken phrases ("By", "Published") come from a per-language
//! [`Phrases`] table (FR-TTS-07); English is the only one so far.
//!
//! Sentence splitting into model-sized pieces happens later, in synthesis (FR-TTS-04).
//! Number, date and abbreviation normalisation (FR-TTS-02) is the pronunciation
//! programme's work (#18); this crate does the language-independent clean-up.

mod clean;

use rtm_article::{Article, Block};
use serde::{Deserialize, Serialize};

pub use clean::speakable;

/// Bump on any change to the script model or to how articles are narrated: it's part of
/// the script's cache key.
pub const NARRATOR_VERSION: u32 = 1;

/// What the engine reads, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    /// BCP 47 language of the narration, e.g. "en".
    pub lang: String,
    pub utterances: Vec<Utterance>,
}

/// One stretch of speech and the silence after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Utterance {
    pub text: String,
    pub pause_after_ms: u32,
}

impl Script {
    pub fn word_count(&self) -> usize {
        self.utterances
            .iter()
            .map(|u| u.text.split_whitespace().count())
            .sum()
    }
}

/// Pauses, in milliseconds. Defaults follow the spike, which the owner listened to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pauses {
    pub after_title: u32,
    pub after_intro: u32,
    pub paragraph: u32,
    pub before_heading: u32,
    pub after_heading: u32,
    pub list_item: u32,
    pub separator: u32,
}

impl Default for Pauses {
    fn default() -> Self {
        Self {
            after_title: 900,
            after_intro: 1_200,
            paragraph: 500,
            before_heading: 800,
            after_heading: 500,
            list_item: 300,
            separator: 1_000,
        }
    }
}

/// How optional parts of an article are read. Part of the script's cache key, so any
/// change gives a new script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    pub pauses: Pauses,
    /// Say where block quotes start and end.
    pub announce_quotes: bool,
    /// Read figure captions.
    pub read_captions: bool,
}

/// The spoken phrases for one language (FR-TTS-07).
#[derive(Debug, Clone, Copy)]
pub struct Phrases {
    pub lang: &'static str,
    pub by: &'static str,
    pub published: &'static str,
    pub months: [&'static str; 12],
    pub quote: &'static str,
    pub end_quote: &'static str,
    pub caption: &'static str,
    pub code_omitted: &'static str,
    pub table_omitted: &'static str,
}

pub const ENGLISH: Phrases = Phrases {
    lang: "en",
    by: "By",
    published: "Published",
    months: [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ],
    quote: "Quote.",
    end_quote: "End of quote.",
    caption: "Caption:",
    code_omitted: "Code omitted.",
    table_omitted: "Table omitted.",
};

/// The phrases for an article's language. `None` if the language isn't supported;
/// an article with no declared language is assumed to be English.
pub fn phrases_for(lang: Option<&str>) -> Option<&'static Phrases> {
    let primary = lang
        .unwrap_or("en")
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    (primary == "en").then_some(&ENGLISH)
}

/// "2026-03-14" (or a longer timestamp starting with it) as "14 March 2026".
fn spoken_date(published: &str, phrases: &Phrases) -> Option<String> {
    let date = published.get(..10)?;
    let mut parts = date.split('-');
    let year: u32 = parts.next()?.parse().ok()?;
    let month: usize = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1000 {
        return None;
    }
    Some(format!("{day} {} {year}", phrases.months[month - 1]))
}

struct Writer<'a> {
    phrases: &'a Phrases,
    options: &'a Options,
    out: Vec<Utterance>,
}

impl Writer<'_> {
    /// Add `text` (cleaned) with a pause after it, skipping it if nothing speakable is
    /// left.
    fn say(&mut self, text: &str, pause_after_ms: u32) {
        let text = speakable(text);
        if !text.is_empty() {
            self.out.push(Utterance {
                text,
                pause_after_ms,
            });
        }
    }

    /// Lengthen the pause after the last utterance to at least `ms`.
    fn pause(&mut self, ms: u32) {
        if let Some(last) = self.out.last_mut() {
            last.pause_after_ms = last.pause_after_ms.max(ms);
        }
    }

    fn block(&mut self, block: &Block) {
        let p = self.options.pauses;
        match block {
            Block::Heading { text, .. } => {
                self.pause(p.before_heading);
                self.say(text, p.after_heading);
            }
            Block::Paragraph { text } => self.say(text, p.paragraph),
            Block::List { ordered, items } => {
                for (n, item) in items.iter().enumerate() {
                    if *ordered {
                        // Ordinal cues (FR-TTS-01): "1. Survey the route."
                        self.say(&format!("{}. {item}", n + 1), p.list_item);
                    } else {
                        self.say(item, p.list_item);
                    }
                }
                self.pause(p.paragraph);
            }
            Block::Quote { blocks } => {
                if self.options.announce_quotes {
                    self.say(self.phrases.quote, p.list_item);
                }
                for inner in blocks {
                    self.block(inner);
                }
                if self.options.announce_quotes {
                    self.say(self.phrases.end_quote, p.paragraph);
                }
            }
            // FR-TTS-01: code is summarised, not read.
            Block::Code { .. } => self.say(self.phrases.code_omitted, p.paragraph),
            Block::Figure { caption, .. } => {
                if let Some(caption) = caption.as_deref().filter(|_| self.options.read_captions) {
                    self.say(&format!("{} {caption}", self.phrases.caption), p.paragraph);
                }
            }
            Block::Table { caption, .. } => {
                let text = match caption {
                    Some(c) => format!("{} {c}", self.phrases.table_omitted),
                    None => self.phrases.table_omitted.to_string(),
                };
                self.say(&text, p.paragraph);
            }
            Block::Separator => self.pause(p.separator),
        }
    }
}

/// Narrate `article` in `phrases`' language: title, then byline and date, then the
/// body (FR-TTS-01).
pub fn narrate(article: &Article, phrases: &Phrases, options: &Options) -> Script {
    let mut w = Writer {
        phrases,
        options,
        out: Vec::new(),
    };
    let p = options.pauses;
    w.say(&article.title, p.after_title);
    let byline = article
        .byline
        .as_deref()
        .map(|by| format!("{} {by}.", phrases.by));
    let date = article
        .published
        .as_deref()
        .and_then(|d| spoken_date(d, phrases))
        .map(|d| format!("{} {d}.", phrases.published));
    let intro: Vec<String> = byline.into_iter().chain(date).collect();
    if !intro.is_empty() {
        w.say(&intro.join(" "), p.after_intro);
    } else {
        w.pause(p.after_intro);
    }
    for block in &article.blocks {
        w.block(block);
    }
    // No trailing silence: the episode ends when the speech does.
    if let Some(last) = w.out.last_mut() {
        last.pause_after_ms = 0;
    }
    Script {
        lang: phrases.lang.to_string(),
        utterances: w.out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(blocks: Vec<Block>) -> Article {
        Article {
            title: "The Quiet Engineering of Canals".into(),
            byline: Some("Ada Lovelock".into()),
            site_name: Some("Waterways Weekly".into()),
            published: Some("2026-03-14".into()),
            canonical_url: None,
            lang: Some("en-GB".into()),
            blocks,
        }
    }

    fn texts(script: &Script) -> Vec<&str> {
        script.utterances.iter().map(|u| u.text.as_str()).collect()
    }

    #[test]
    fn title_then_byline_and_date_then_body() {
        let a = article(vec![Block::Paragraph {
            text: "Canals are old.".into(),
        }]);
        let s = narrate(&a, &ENGLISH, &Options::default());
        assert_eq!(s.lang, "en");
        assert_eq!(
            texts(&s),
            [
                "The Quiet Engineering of Canals",
                "By Ada Lovelock. Published 14 March 2026.",
                "Canals are old."
            ]
        );
        let p = Pauses::default();
        assert_eq!(
            s.utterances
                .iter()
                .map(|u| u.pause_after_ms)
                .collect::<Vec<_>>(),
            [p.after_title, p.after_intro, 0]
        );
    }

    #[test]
    fn missing_intro_parts_are_left_out() {
        let mut a = article(vec![Block::Paragraph {
            text: "Body.".into(),
        }]);
        a.byline = None;
        a.published = Some("sometime last week".into());
        let s = narrate(&a, &ENGLISH, &Options::default());
        assert_eq!(texts(&s), ["The Quiet Engineering of Canals", "Body."]);
        // The intro's pause still separates title from body.
        assert_eq!(
            s.utterances[0].pause_after_ms,
            Pauses::default().after_intro
        );
        a.byline = Some("Ada".into());
        a.published = Some("2026-02-01T09:00:00Z".into());
        assert_eq!(
            texts(&narrate(&a, &ENGLISH, &Options::default()))[1],
            "By Ada. Published 1 February 2026."
        );
    }

    #[test]
    fn structure_becomes_pauses_and_cues() {
        let p = Pauses::default();
        let a = article(vec![
            Block::Paragraph {
                text: "Intro.".into(),
            },
            Block::Heading {
                level: 2,
                text: "Locks".into(),
            },
            Block::List {
                ordered: true,
                items: vec!["Survey.".into(), "Cut.".into()],
            },
            Block::List {
                ordered: false,
                items: vec!["Staircase.".into()],
            },
            Block::Separator,
            Block::Code {
                text: "fn main() {}".into(),
            },
            Block::Table {
                caption: Some("Lock rises.".into()),
                rows: vec![vec!["1".into()]],
            },
            Block::Figure {
                alt: Some("A lock".into()),
                caption: Some("Lock 7.".into()),
            },
            Block::Paragraph {
                text: "End.".into(),
            },
        ]);
        let s = narrate(&a, &ENGLISH, &Options::default());
        let body: Vec<(&str, u32)> = s.utterances[2..]
            .iter()
            .map(|u| (u.text.as_str(), u.pause_after_ms))
            .collect();
        assert_eq!(
            body,
            [
                ("Intro.", p.before_heading),
                ("Locks", p.after_heading),
                ("1. Survey.", p.list_item),
                ("2. Cut.", p.paragraph),
                ("Staircase.", p.separator),
                ("Code omitted.", p.paragraph),
                ("Table omitted. Lock rises.", p.paragraph),
                ("End.", 0),
            ]
        );
    }

    #[test]
    fn quotes_and_captions_can_be_announced() {
        let a = article(vec![
            Block::Quote {
                blocks: vec![Block::Paragraph {
                    text: "Water is patient.".into(),
                }],
            },
            Block::Figure {
                alt: None,
                caption: Some("Lock 7.".into()),
            },
        ]);
        let plain = narrate(&a, &ENGLISH, &Options::default());
        assert_eq!(&texts(&plain)[2..], ["Water is patient."]);
        let announced = narrate(
            &a,
            &ENGLISH,
            &Options {
                announce_quotes: true,
                read_captions: true,
                ..Options::default()
            },
        );
        assert_eq!(
            &texts(&announced)[2..],
            [
                "Quote.",
                "Water is patient.",
                "End of quote.",
                "Caption: Lock 7."
            ]
        );
    }

    #[test]
    fn languages() {
        for lang in [None, Some("en"), Some("en-GB"), Some("EN_us")] {
            assert!(phrases_for(lang).is_some(), "{lang:?}");
        }
        for lang in [Some("fr"), Some("de-DE"), Some("")] {
            assert!(phrases_for(lang).is_none(), "{lang:?}");
        }
    }

    #[test]
    fn scripts_round_trip_as_json() {
        let s = narrate(&article(vec![]), &ENGLISH, &Options::default());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Script>(&json).unwrap(), s);
    }
}
