// SPDX-License-Identifier: AGPL-3.0-or-later
//! Triage (ADR-0006): score every extractor, run the best, check the result, and fall
//! back to the next candidate if it fails the quality check.

use rtm_article::{Article, Block, Limits};

use crate::{Extractor, PlainText, Readability, SourceDocument, Trafilatura, TriageView};

#[derive(Debug, Clone)]
pub struct TriageConfig {
    /// Candidates must score at least this.
    pub threshold: f32,
    /// Extractors tried before giving up.
    pub max_attempts: usize,
    /// FR-EXT-05: fewer words than this fails the quality check.
    pub min_words: usize,
    pub limits: Limits,
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            max_attempts: 3,
            min_words: 200,
            limits: Limits::default(),
        }
    }
}

/// A successful extraction and how it was made (FR-EXT-04).
#[derive(Debug, Clone, PartialEq)]
pub struct Extraction {
    pub article: Article,
    pub extractor: &'static str,
    pub version: u32,
    pub words: usize,
    /// The document had no title, so one was derived ([`fallback_title`]).
    pub derived_title: bool,
}

/// Why no acceptable article was found.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// No extractor handles this kind of document.
    Unsupported,
    /// Every attempt failed the quality check or found nothing. `best` is the attempt
    /// with the most words, kept so the user can force it (FR-EXT-05).
    LowConfidence {
        best: Option<Box<Extraction>>,
        reasons: Vec<String>,
    },
}

pub struct Registry {
    extractors: Vec<Box<dyn Extractor>>,
    config: TriageConfig,
}

impl Registry {
    /// The built-in extractors: `generic-html` (trafilatura), its `readability`
    /// fallback (dom_smoothie), and `plain-text`.
    pub fn builtin(config: TriageConfig) -> Self {
        Self {
            extractors: vec![
                Box::new(Trafilatura),
                Box::new(Readability),
                Box::new(PlainText),
            ],
            config,
        }
    }

    pub fn with(extractors: Vec<Box<dyn Extractor>>, config: TriageConfig) -> Self {
        Self { extractors, config }
    }

    /// Candidates for `doc`, best first: by specificity, then confidence.
    fn candidates(&self, doc: &SourceDocument) -> Vec<&dyn Extractor> {
        let view = TriageView {
            kind: doc.kind,
            final_url: doc.final_url.as_ref(),
            prefix: &doc.bytes[..doc.bytes.len().min(TriageView::PREFIX)],
        };
        let mut scored: Vec<_> = self
            .extractors
            .iter()
            .filter_map(|e| e.score(&view).map(|c| (e.as_ref(), c)))
            .filter(|(_, c)| c.0 >= self.config.threshold)
            .collect();
        scored.sort_by(|(a, ca), (b, cb)| {
            b.specificity()
                .cmp(&a.specificity())
                .then(cb.0.total_cmp(&ca.0))
        });
        scored.into_iter().map(|(e, _)| e).collect()
    }

    pub fn extract(&self, doc: &SourceDocument) -> Result<Extraction, Failure> {
        let candidates = self.candidates(doc);
        if candidates.is_empty() {
            return Err(Failure::Unsupported);
        }
        let mut best: Option<Extraction> = None;
        // The best attempt that passed everything except having a title.
        let mut untitled: Option<Extraction> = None;
        let mut reasons = Vec::new();
        for extractor in candidates.into_iter().take(self.config.max_attempts) {
            let id = extractor.id();
            let article = match extractor.extract(doc) {
                Ok(article) => article,
                Err(e) => {
                    reasons.push(format!("{id}: {e}"));
                    continue;
                }
            };
            let article = match article.validate(&self.config.limits) {
                Ok(article) => article,
                Err(e) => {
                    reasons.push(format!("{id}: {e}"));
                    continue;
                }
            };
            let attempt = Extraction {
                words: article.word_count(),
                article,
                extractor: id,
                version: extractor.version(),
                derived_title: false,
            };
            match self.quality(&attempt) {
                Quality::Pass => return Ok(attempt),
                // Another extractor may find the title (for instance in metadata), so
                // try the next before settling for a derived one.
                Quality::Untitled => {
                    reasons.push(format!("{id}: no title"));
                    if untitled.as_ref().is_none_or(|u| attempt.words > u.words) {
                        untitled = Some(attempt);
                    }
                }
                Quality::TooShort(problem) => {
                    reasons.push(format!("{id}: {problem}"));
                    if best.as_ref().is_none_or(|b| attempt.words > b.words) {
                        best = Some(attempt);
                    }
                }
            }
        }
        if let Some(mut attempt) = untitled {
            attempt.article.title = fallback_title(doc, &attempt.article);
            // Revalidate and recount: the result must meet the same checks as any other.
            match attempt.article.validate(&self.config.limits) {
                Ok(article) => {
                    return Ok(Extraction {
                        words: article.word_count(),
                        article,
                        derived_title: true,
                        ..attempt
                    });
                }
                Err(e) => reasons.push(format!("{}: derived title: {e}", attempt.extractor)),
            }
        }
        Err(Failure::LowConfidence {
            best: best.map(Box::new),
            reasons,
        })
    }

    /// The post-extraction quality check (ADR-0006 step 4).
    fn quality(&self, attempt: &Extraction) -> Quality {
        if attempt.words < self.config.min_words {
            return Quality::TooShort(format!(
                "only {} words (minimum {})",
                attempt.words, self.config.min_words
            ));
        }
        if attempt.article.title.trim().is_empty() {
            return Quality::Untitled;
        }
        Quality::Pass
    }
}

enum Quality {
    Pass,
    Untitled,
    TooShort(String),
}

/// Words taken from the start of the text for a derived title.
const TITLE_WORDS: usize = 10;

/// Longest derived title, in characters, well inside the metadata field limit.
const MAX_DERIVED_TITLE_CHARS: usize = 120;

/// `title` cut to [`MAX_DERIVED_TITLE_CHARS`], marking any cut with an ellipsis.
fn bounded(title: String) -> String {
    if title.chars().count() <= MAX_DERIVED_TITLE_CHARS {
        return title;
    }
    let mut cut: String = title.chars().take(MAX_DERIVED_TITLE_CHARS - 1).collect();
    cut.truncate(cut.trim_end_matches('…').trim_end().len());
    cut.push('…');
    cut
}

/// A title for an article that has none (ADR-0006's quality check needs one, and the
/// narrator reads it first): the last segment of the URL's path when it reads like
/// words (`/2026/10/still-water-history.html` → "Still water history"), otherwise the
/// opening words of the text.
pub fn fallback_title(doc: &SourceDocument, article: &Article) -> String {
    bounded(unbounded_title(doc, article))
}

fn unbounded_title(doc: &SourceDocument, article: &Article) -> String {
    let from_url = doc
        .final_url
        .as_ref()
        .and_then(|u| {
            u.path_segments()?
                .rev()
                .find(|s| !s.is_empty())
                .map(str::to_string)
        })
        .map(|segment| {
            let stem = segment
                .rsplit_once('.')
                .filter(|(_, ext)| matches!(*ext, "html" | "htm" | "php" | "aspx" | "txt"))
                .map_or(segment.as_str(), |(stem, _)| stem)
                .to_string();
            stem.replace(['-', '_', '+'], " ")
        })
        .filter(|s| {
            s.split_whitespace()
                .filter(|w| w.chars().any(char::is_alphabetic))
                .count()
                >= 2
        });
    if let Some(words) = from_url {
        let words = rtm_article::clean_text(&words);
        let mut chars = words.chars();
        return chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
    }
    let opening = article.blocks.iter().find_map(|b| match b {
        Block::Paragraph { text } | Block::Heading { text, .. } => Some(text.as_str()),
        _ => None,
    });
    match opening {
        Some(text) => {
            let words: Vec<&str> = text.split_whitespace().collect();
            let mut title = words[..words.len().min(TITLE_WORDS)].join(" ");
            if words.len() > TITLE_WORDS {
                title.push('…');
            }
            title
        }
        None => "Untitled article".into(),
    }
}
