// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `Article` model (docs/03 §4, ADR-0006): what extraction produces and narration
//! consumes. Structured blocks, never HTML. Versioned: any change to its meaning bumps
//! [`SCHEMA_VERSION`], which is part of the extraction cache key (ADR-0008).
//!
//! Articles cross the sandbox boundary as JSON from an untrusted process, so
//! [`Article::validate`] bounds and cleans everything before the server uses it.

use serde::{Deserialize, Serialize};

/// Bump on any change to the model's shape or meaning.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Article {
    pub title: String,
    pub byline: Option<String>,
    pub site_name: Option<String>,
    /// As found in the page; dates are normalised during narration, not here.
    pub published: Option<String>,
    pub canonical_url: Option<String>,
    /// A BCP 47 language tag, if the page declared or the extractor detected one.
    pub lang: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    List {
        ordered: bool,
        items: Vec<String>,
    },
    Quote {
        blocks: Vec<Block>,
    },
    Code {
        text: String,
    },
    Figure {
        alt: Option<String>,
        caption: Option<String>,
    },
    Table {
        caption: Option<String>,
        rows: Vec<Vec<String>>,
    },
    Separator,
}

/// Upper bounds on an article from the extract worker. Generous for real articles
/// (30,000 words is the narration limit), but they stop a compromised or confused
/// worker handing the server something enormous.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_blocks: usize,
    pub max_text_bytes: usize,
    pub max_field_bytes: usize,
    pub max_nesting: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_blocks: 20_000,
            max_text_bytes: 4 * 1024 * 1024,
            max_field_bytes: 2_048,
            max_nesting: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("too many blocks")]
    TooManyBlocks,
    #[error("too much text")]
    TooMuchText,
    #[error("a metadata field is too long")]
    FieldTooLong,
    #[error("blocks are nested too deeply")]
    TooDeep,
    #[error("heading level out of range")]
    HeadingLevel,
}

/// Collapse runs of whitespace to single spaces and drop control and format characters
/// that have no business in narration (NUL, bidi overrides, zero-width joiners, ...).
pub fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control() || is_format(c) {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// Unicode "format" characters (category Cf) commonly found in web text: soft hyphens,
/// zero-width characters and bidirectional controls.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' |
        '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

impl Article {
    /// Words of narratable text: title plus every block.
    pub fn word_count(&self) -> usize {
        let words = |s: &str| s.split_whitespace().count();
        words(&self.title) + self.blocks.iter().map(|b| b.word_count()).sum::<usize>()
    }

    /// Check the article is within `limits`, and clean every string in it.
    pub fn validate(mut self, limits: &Limits) -> Result<Article, Invalid> {
        let mut budget = Budget {
            blocks: 0,
            text: 0,
            limits,
        };
        for field in [
            Some(&mut self.title),
            self.byline.as_mut(),
            self.site_name.as_mut(),
            self.published.as_mut(),
            self.canonical_url.as_mut(),
            self.lang.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            *field = clean_text(field);
            if field.len() > limits.max_field_bytes {
                return Err(Invalid::FieldTooLong);
            }
        }
        self.blocks = budget.blocks(std::mem::take(&mut self.blocks), 0)?;
        Ok(self)
    }
}

impl Block {
    fn word_count(&self) -> usize {
        let words = |s: &str| s.split_whitespace().count();
        match self {
            Block::Heading { text, .. } | Block::Paragraph { text } | Block::Code { text } => {
                words(text)
            }
            Block::List { items, .. } => items.iter().map(|i| words(i)).sum(),
            Block::Quote { blocks } => blocks.iter().map(Block::word_count).sum(),
            Block::Figure { alt, caption } => {
                alt.as_deref().map_or(0, words) + caption.as_deref().map_or(0, words)
            }
            Block::Table { caption, rows } => {
                caption.as_deref().map_or(0, words)
                    + rows.iter().flatten().map(|c| words(c)).sum::<usize>()
            }
            Block::Separator => 0,
        }
    }
}

struct Budget<'a> {
    blocks: usize,
    text: usize,
    limits: &'a Limits,
}

impl Budget<'_> {
    fn text(&mut self, s: &mut String) -> Result<(), Invalid> {
        *s = clean_text(s);
        self.text += s.len();
        if self.text > self.limits.max_text_bytes {
            return Err(Invalid::TooMuchText);
        }
        Ok(())
    }

    fn opt(&mut self, s: &mut Option<String>) -> Result<(), Invalid> {
        if let Some(text) = s {
            self.text(text)?;
            if text.is_empty() {
                *s = None;
            }
        }
        Ok(())
    }

    /// Clean and count `blocks`, dropping any left empty by cleaning.
    fn blocks(&mut self, blocks: Vec<Block>, depth: usize) -> Result<Vec<Block>, Invalid> {
        if depth > self.limits.max_nesting {
            return Err(Invalid::TooDeep);
        }
        let mut out = Vec::with_capacity(blocks.len());
        for mut block in blocks {
            self.blocks += 1;
            if self.blocks > self.limits.max_blocks {
                return Err(Invalid::TooManyBlocks);
            }
            let keep = match &mut block {
                Block::Heading { level, text } => {
                    if !(1..=6).contains(level) {
                        return Err(Invalid::HeadingLevel);
                    }
                    self.text(text)?;
                    !text.is_empty()
                }
                Block::Paragraph { text } | Block::Code { text } => {
                    self.text(text)?;
                    !text.is_empty()
                }
                Block::List { items, .. } => {
                    for item in items.iter_mut() {
                        self.text(item)?;
                    }
                    items.retain(|i| !i.is_empty());
                    !items.is_empty()
                }
                Block::Quote { blocks } => {
                    *blocks = self.blocks(std::mem::take(blocks), depth + 1)?;
                    !blocks.is_empty()
                }
                Block::Figure { alt, caption } => {
                    self.opt(alt)?;
                    self.opt(caption)?;
                    alt.is_some() || caption.is_some()
                }
                Block::Table { caption, rows } => {
                    self.opt(caption)?;
                    for cell in rows.iter_mut().flatten() {
                        self.text(cell)?;
                    }
                    rows.retain(|r| r.iter().any(|c| !c.is_empty()));
                    !rows.is_empty()
                }
                Block::Separator => true,
            };
            if keep {
                out.push(block);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str) -> Block {
        Block::Paragraph { text: text.into() }
    }

    fn article(blocks: Vec<Block>) -> Article {
        Article {
            title: "A title".into(),
            byline: None,
            site_name: None,
            published: None,
            canonical_url: None,
            lang: None,
            blocks,
        }
    }

    #[test]
    fn cleaning_collapses_whitespace_and_drops_invisible_characters() {
        assert_eq!(clean_text("  a \n\t b  "), "a b");
        assert_eq!(clean_text("co\u{00AD}operate"), "cooperate");
        assert_eq!(clean_text("x\u{0}y\u{202E}z\u{200B}"), "xyz");
        assert_eq!(clean_text("\u{FEFF}Title"), "Title");
    }

    #[test]
    fn validation_cleans_and_drops_empty_blocks() {
        let a = article(vec![
            para("  Hello   world "),
            para("\u{200B} "),
            Block::List {
                ordered: false,
                items: vec!["one".into(), " ".into()],
            },
            Block::Quote {
                blocks: vec![para("")],
            },
        ])
        .validate(&Limits::default())
        .unwrap();
        assert_eq!(
            a.blocks,
            vec![
                para("Hello world"),
                Block::List {
                    ordered: false,
                    items: vec!["one".into()]
                }
            ]
        );
        assert_eq!(a.word_count(), 2 + 2 + 1);
    }

    #[test]
    fn validation_enforces_limits() {
        let tight = Limits {
            max_blocks: 2,
            max_text_bytes: 10,
            max_field_bytes: 8,
            max_nesting: 1,
        };
        let three = article(vec![para("a"), para("b"), para("c")]);
        assert_eq!(three.validate(&tight), Err(Invalid::TooManyBlocks));
        let long = article(vec![para("01234567890")]);
        assert_eq!(long.validate(&tight), Err(Invalid::TooMuchText));
        let mut titled = article(vec![]);
        titled.title = "far too long".into();
        assert_eq!(titled.validate(&tight), Err(Invalid::FieldTooLong));
        let deep = article(vec![Block::Quote {
            blocks: vec![Block::Quote {
                blocks: vec![Block::Quote {
                    blocks: vec![para("x")],
                }],
            }],
        }]);
        assert_eq!(
            deep.validate(&Limits {
                max_nesting: 1,
                ..Limits::default()
            }),
            Err(Invalid::TooDeep)
        );
        let bad = article(vec![Block::Heading {
            level: 9,
            text: "x".into(),
        }]);
        assert_eq!(bad.validate(&Limits::default()), Err(Invalid::HeadingLevel));
    }

    #[test]
    fn json_is_tagged_and_strict() {
        let a = article(vec![para("Hi"), Block::Separator]);
        let json = serde_json::to_string(&a).unwrap();
        assert!(
            json.contains(r#"{"type":"paragraph","text":"Hi"}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Article>(&json).unwrap(), a);
        let extra = json.replacen("\"title\"", "\"html\":\"<script>\",\"title\"", 1);
        assert!(serde_json::from_str::<Article>(&extra).is_err());
    }
}
