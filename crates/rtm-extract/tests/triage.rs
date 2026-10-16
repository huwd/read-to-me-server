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
    title: &'static str,
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
            title: self.title.into(),
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
        title: "T",
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

fn untitled(
    id: &'static str,
    score: f32,
    words: usize,
    log: &'static Mutex<Vec<&'static str>>,
) -> Box<dyn Extractor> {
    Box::new(Fake {
        id,
        title: "  ",
        specificity: Specificity::Generic,
        score: Some(score),
        words: Some(words),
        log,
    })
}

fn doc_at(url: &str) -> SourceDocument {
    SourceDocument {
        final_url: Some(url.parse().unwrap()),
        ..doc()
    }
}

/// ADR-0006's quality check needs a title: an untitled result lets the next extractor
/// try, since it may find one in the page's metadata.
#[test]
fn an_untitled_result_falls_back_to_a_titled_one() {
    static LOG: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let registry = Registry::with(
        vec![
            untitled("no-title", 0.9, 500, &LOG),
            fake("titled", Specificity::Generic, Some(0.8), Some(300), &LOG),
        ],
        config(),
    );
    let e = registry.extract(&doc()).unwrap();
    assert_eq!(
        (e.extractor, e.article.title.as_str(), e.derived_title),
        ("titled", "T", false)
    );
    assert_eq!(*LOG.lock().unwrap(), ["no-title", "titled"]);
}

/// When nothing finds a title, the longest untitled result is accepted with a title
/// derived from the URL, rather than refusing an otherwise good article.
#[test]
fn when_no_extractor_finds_a_title_one_is_derived() {
    static LOG: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let registry = Registry::with(
        vec![
            untitled("a", 0.9, 300, &LOG),
            untitled("b", 0.8, 400, &LOG),
            fake("short", Specificity::Generic, Some(0.7), Some(10), &LOG),
        ],
        config(),
    );
    let e = registry
        .extract(&doc_at(
            "https://example.test/2026/10/still-water-history.html",
        ))
        .unwrap();
    assert_eq!(e.extractor, "b");
    assert_eq!(e.article.title, "Still water history");
    assert!(e.derived_title);
    assert_eq!(*LOG.lock().unwrap(), ["a", "b", "short"]);
}

/// Codex's reproduction: a long plain-text document with no title line.
#[test]
fn an_untitled_plain_text_document_gets_a_title() {
    let registry = Registry::builtin(TriageConfig::default());
    let text = SourceDocument {
        bytes: "word ".repeat(250).into_bytes(),
        kind: SourceKind::PlainText,
        charset: None,
        final_url: Some("https://example.test/notes/4444".parse().unwrap()),
    };
    let e = registry.extract(&text).unwrap();
    assert!(e.derived_title);
    // "4444" doesn't read as words, so the opening words are used.
    assert_eq!(e.article.title, format!("{}…", ["word"; 10].join(" ")));
}

#[test]
fn derived_titles() {
    let body = Article {
        title: String::new(),
        byline: None,
        site_name: None,
        published: None,
        canonical_url: None,
        lang: None,
        blocks: vec![Block::Paragraph {
            text: "Short opening line.".into(),
        }],
    };
    let title = |url: &str| rtm_extract::fallback_title(&doc_at(url), &body);
    assert_eq!(
        title("https://x.test/a/the_long-road+home/"),
        "The long road home"
    );
    assert_eq!(title("https://x.test/story.php"), "Short opening line.");
    assert_eq!(title("https://x.test/p/2026-10-16"), "Short opening line.");
    assert_eq!(title("https://x.test/"), "Short opening line.");
    let empty = Article {
        blocks: vec![],
        ..body.clone()
    };
    assert_eq!(
        rtm_extract::fallback_title(&doc(), &empty),
        "Untitled article"
    );
}
