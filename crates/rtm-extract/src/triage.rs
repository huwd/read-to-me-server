// SPDX-License-Identifier: AGPL-3.0-or-later
//! Triage (ADR-0006): score every extractor, run the best, check the result, and fall
//! back to the next candidate if it fails the quality check.

use rtm_article::{Article, Limits};

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
            };
            match self.quality_problem(&attempt) {
                None => return Ok(attempt),
                Some(problem) => {
                    reasons.push(format!("{id}: {problem}"));
                    if best.as_ref().is_none_or(|b| attempt.words > b.words) {
                        best = Some(attempt);
                    }
                }
            }
        }
        Err(Failure::LowConfidence {
            best: best.map(Box::new),
            reasons,
        })
    }

    /// The post-extraction quality check (ADR-0006 step 4), or `None` if it passes.
    fn quality_problem(&self, attempt: &Extraction) -> Option<String> {
        if attempt.words < self.config.min_words {
            return Some(format!(
                "only {} words (minimum {})",
                attempt.words, self.config.min_words
            ));
        }
        None
    }
}
