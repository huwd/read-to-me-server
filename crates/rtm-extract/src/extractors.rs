// SPDX-License-Identifier: AGPL-3.0-or-later
//! The built-in extractors (ADR-0006, FR-EXT-02).

use rtm_article::{Article, Block, clean_text};
use rtm_domain::SourceKind;

use crate::blocks::{drop_repeated_title, from_html};
use crate::{Confidence, ExtractError, Extractor, SourceDocument, Specificity, TriageView};

fn non_empty(s: &str) -> Option<String> {
    let s = clean_text(s);
    (!s.is_empty()).then_some(s)
}

/// `generic-html`: trafilatura (ADR-0006 Amendment 1), the default for HTML.
pub struct Trafilatura;

impl Extractor for Trafilatura {
    fn id(&self) -> &'static str {
        "generic-html"
    }

    fn version(&self) -> u32 {
        1
    }

    fn specificity(&self) -> Specificity {
        Specificity::Generic
    }

    fn score(&self, view: &TriageView<'_>) -> Option<Confidence> {
        (view.kind == SourceKind::Html).then_some(Confidence(0.8))
    }

    fn extract(&self, doc: &SourceDocument) -> Result<Article, ExtractError> {
        let mut options = trafilatura::Options::default();
        if let Some(url) = &doc.final_url {
            options = options.with_url(url.clone());
        }
        let result = trafilatura::extract(&doc.text(), &options)
            .map_err(|e| ExtractError::NothingFound(e.to_string()))?;
        let meta = result.metadata;
        let title = clean_text(&meta.title);
        let mut blocks = from_html(&result.content_html);
        drop_repeated_title(&mut blocks, &title);
        Ok(Article {
            title,
            byline: non_empty(&meta.author),
            site_name: non_empty(&meta.sitename),
            published: meta.date.map(|d| d.to_string()),
            canonical_url: non_empty(&meta.url),
            lang: non_empty(&meta.language),
            blocks,
        })
    }
}

/// `readability`: dom_smoothie, the fallback when trafilatura's result fails the
/// quality check (ADR-0006 Amendment 1).
pub struct Readability;

impl Extractor for Readability {
    fn id(&self) -> &'static str {
        "readability"
    }

    fn version(&self) -> u32 {
        1
    }

    fn specificity(&self) -> Specificity {
        Specificity::Generic
    }

    fn score(&self, view: &TriageView<'_>) -> Option<Confidence> {
        (view.kind == SourceKind::Html).then_some(Confidence(0.6))
    }

    fn extract(&self, doc: &SourceDocument) -> Result<Article, ExtractError> {
        let text = doc.text();
        let url = doc.final_url.as_ref().map(url::Url::as_str);
        let mut reader = dom_smoothie::Readability::new(text.as_str(), url, None)
            .map_err(|e| ExtractError::NothingFound(e.to_string()))?;
        let found = reader
            .parse()
            .map_err(|e| ExtractError::NothingFound(e.to_string()))?;
        let title = clean_text(&found.title);
        let mut blocks = from_html(&found.content);
        drop_repeated_title(&mut blocks, &title);
        Ok(Article {
            title,
            byline: found.byline.as_deref().and_then(non_empty),
            site_name: found.site_name.as_deref().and_then(non_empty),
            published: found.published_time.as_deref().and_then(non_empty),
            canonical_url: doc.final_url.as_ref().map(ToString::to_string),
            lang: found.lang.as_deref().and_then(non_empty),
            blocks,
        })
    }
}

/// `plain-text`: paragraphs separated by blank lines; a short first line is the title.
pub struct PlainText;

/// A first line longer than this is body text, not a title.
const MAX_TITLE_CHARS: usize = 150;

impl Extractor for PlainText {
    fn id(&self) -> &'static str {
        "plain-text"
    }

    fn version(&self) -> u32 {
        1
    }

    fn specificity(&self) -> Specificity {
        Specificity::Format
    }

    fn score(&self, view: &TriageView<'_>) -> Option<Confidence> {
        (view.kind == SourceKind::PlainText).then_some(Confidence(0.9))
    }

    fn extract(&self, doc: &SourceDocument) -> Result<Article, ExtractError> {
        let text = doc.text().replace("\r\n", "\n");
        let mut paragraphs: Vec<String> = text
            .split("\n\n")
            .map(clean_text)
            .filter(|p| !p.is_empty())
            .collect();
        let title = match paragraphs.first() {
            Some(first) if first.chars().count() <= MAX_TITLE_CHARS && paragraphs.len() > 1 => {
                paragraphs.remove(0)
            }
            _ => String::new(),
        };
        Ok(Article {
            title,
            byline: None,
            site_name: None,
            published: None,
            canonical_url: doc.final_url.as_ref().map(ToString::to_string),
            lang: None,
            blocks: paragraphs
                .into_iter()
                .map(|text| Block::Paragraph { text })
                .collect(),
        })
    }
}
