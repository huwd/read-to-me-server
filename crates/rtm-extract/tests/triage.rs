// SPDX-License-Identifier: AGPL-3.0-or-later
//! Triage (ADR-0006 steps 1–6) with scripted extractors.

use std::sync::Mutex;

use rtm_article::{Article, Block};
use rtm_domain::SourceKind;
use rtm_extract::{
    Confidence, ExtractError, Extractor, Failure, Registry, SourceDocument, Specificity,
    TriageConfig, TriageView,
};

/// An extractor with a fixed score that returns `words` words (or fails), and records
/// that it ran.
struct Fake {
    id: &'static str,
    specificity: Specificity,
    score: Option<f32>,
    words: Option<usize>,
    log: &'static Mutex<Vec<&'static str>>,
}

impl Extractor for Fake {
    fn id(&self) -> &'static str {
        self.id
    }
    fn version(&self) -> u32 {
        7
    }
    fn specificity(&self) -> Specificity {
        self.specificity
    }
    fn score(&self, _: &TriageView<'_>) -> Option<Confidence> {
        self.score.map(Confidence)
    }
    fn extract(&self, _: &SourceDocument) -> Result<Article, ExtractError> {
        self.log.lock().unwrap().push(self.id);
        let words = self
            .words
            .ok_or_else(|| ExtractError::NothingFound("nothing".into()))?;
        Ok(Article {
            title: "T".into(),
            byline: None,
            site_name: None,
            published: None,
            canonical_url: None,
            lang: None,
            blocks: vec![Block::Paragraph {
                text: vec!["w"; words].join(" "),
            }],
        })
    }
}

fn doc() -> SourceDocument {
    SourceDocument {
        bytes: b"<p>x</p>".to_vec(),
        kind: SourceKind::Html,
        charset: None,
        final_url: None,
    }
}

fn fake(
    id: &'static str,
    specificity: Specificity,
    score: Option<f32>,
    words: Option<usize>,
    log: &'static Mutex<Vec<&'static str>>,
) -> Box<dyn Extractor> {
    Box::new(Fake {
        id,
        specificity,
        score,
        words,
        log,
    })
}

fn config() -> TriageConfig {
    TriageConfig {
        min_words: 100,
        ..TriageConfig::default()
    }
}

#[test]
fn specificity_beats_confidence_and_the_threshold_filters() {
    static LOG: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let registry = Registry::with(
        vec![
            fake(
                "generic-sure",
                Specificity::Generic,
                Some(0.99),
                Some(500),
                &LOG,
            ),
            fake("site", Specificity::Site, Some(0.6), Some(500), &LOG),
            fake("site-unsure", Specificity::Site, Some(0.4), Some(500), &LOG),
            fake("not-me", Specificity::Site, None, Some(500), &LOG),
        ],
        config(),
    );
    let e = registry.extract(&doc()).unwrap();
    assert_eq!((e.extractor, e.version, e.words), ("site", 7, 501));
    assert_eq!(*LOG.lock().unwrap(), ["site"]);
}

#[test]
fn falls_back_after_a_failed_quality_check_or_error() {
    static LOG: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let registry = Registry::with(
        vec![
            fake("first", Specificity::Generic, Some(0.9), Some(10), &LOG),
            fake("second", Specificity::Generic, Some(0.8), None, &LOG),
            fake("third", Specificity::Generic, Some(0.7), Some(300), &LOG),
        ],
        config(),
    );
    assert_eq!(registry.extract(&doc()).unwrap().extractor, "third");
    assert_eq!(*LOG.lock().unwrap(), ["first", "second", "third"]);
}

// FR-EXT-05
#[test]
fn gives_up_after_three_attempts_keeping_the_best() {
    static LOG: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let registry = Registry::with(
        vec![
            fake("a", Specificity::Generic, Some(0.9), Some(10), &LOG),
            fake("b", Specificity::Generic, Some(0.8), Some(50), &LOG),
            fake("c", Specificity::Generic, Some(0.7), Some(20), &LOG),
            fake("d", Specificity::Generic, Some(0.6), Some(999), &LOG),
        ],
        config(),
    );
    let Err(Failure::LowConfidence { best, reasons }) = registry.extract(&doc()) else {
        panic!("expected low confidence");
    };
    assert_eq!(best.unwrap().extractor, "b");
    assert_eq!(reasons.len(), 3);
    assert_eq!(
        *LOG.lock().unwrap(),
        ["a", "b", "c"],
        "a fourth attempt ran"
    );
}

#[test]
fn no_candidate_is_unsupported() {
    static LOG: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let registry = Registry::with(
        vec![fake("x", Specificity::Generic, Some(0.2), Some(500), &LOG)],
        config(),
    );
    assert_eq!(registry.extract(&doc()), Err(Failure::Unsupported));
    assert!(LOG.lock().unwrap().is_empty());
}
