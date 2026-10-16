// SPDX-License-Identifier: AGPL-3.0-or-later
//! Article extraction (ADR-0006): turn a fetched source into an [`Article`] through a
//! triage of registered extractors.
//!
//! This crate parses untrusted documents. It runs inside the sandboxed extract worker,
//! never in the server process (FR-EXT-06).

mod blocks;
mod decode;
mod extractors;
mod triage;

use rtm_article::Article;
use rtm_domain::SourceKind;
use url::Url;

pub use extractors::{PlainText, Readability, Trafilatura};
pub use triage::{Extraction, Failure, Registry, TriageConfig, fallback_title};

/// A fetched document. Never trusted.
#[derive(Debug, Clone)]
pub struct SourceDocument {
    pub bytes: Vec<u8>,
    pub kind: SourceKind,
    /// The declared character set, if any.
    pub charset: Option<String>,
    pub final_url: Option<Url>,
}

impl SourceDocument {
    /// The document as text, decoded per its byte-order mark, declared charset or
    /// `<meta>` declaration.
    pub fn text(&self) -> String {
        decode::decode(
            &self.bytes,
            self.charset.as_deref(),
            self.kind == SourceKind::Html,
        )
    }
}

/// What an extractor may look at when scoring: cheap, bounded, no parsing.
#[derive(Debug, Clone, Copy)]
pub struct TriageView<'a> {
    pub kind: SourceKind,
    pub final_url: Option<&'a Url>,
    /// At most the first [`TriageView::PREFIX`] bytes.
    pub prefix: &'a [u8],
}

impl TriageView<'_> {
    pub const PREFIX: usize = 8 * 1024;
}

/// How sure an extractor is that it suits a document, from 0.0 to 1.0.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(pub f32);

/// Site adapters beat format extractors, which beat generic ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Specificity {
    Generic,
    Format,
    Site,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractError {
    #[error("no article found: {0}")]
    NothingFound(String),
}

/// One way of turning a source into an article (ADR-0006).
pub trait Extractor: Send + Sync {
    fn id(&self) -> &'static str;
    /// Bump on any change that alters output for the same input.
    fn version(&self) -> u32;
    fn specificity(&self) -> Specificity;
    /// Cheap, pure, no I/O: whether and how well this extractor suits the document.
    fn score(&self, view: &TriageView<'_>) -> Option<Confidence>;
    fn extract(&self, doc: &SourceDocument) -> Result<Article, ExtractError>;
}
