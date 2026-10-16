// SPDX-License-Identifier: AGPL-3.0-or-later
//! Golden tests (ADR-0006): extraction output for each fixture is snapshotted together
//! with the extractor's `id@version`. Output that changes without a version bump fails.
//!
//! Regenerate after a deliberate change (and a version bump) with
//! `UPDATE_GOLDEN=1 cargo test -p rtm-extract --test golden`.

use std::path::Path;

use rtm_domain::SourceKind;
use rtm_extract::{Failure, Registry, SourceDocument, TriageConfig};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn doc(name: &str, kind: SourceKind, charset: Option<&str>) -> SourceDocument {
    SourceDocument {
        bytes: fixture(name),
        kind,
        charset: charset.map(Into::into),
        final_url: Some(format!("https://example.test/{name}").parse().unwrap()),
    }
}

fn check_golden(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.expected.json"));
    let rendered = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!("no snapshot {}; run with UPDATE_GOLDEN=1", path.display())
        }))
        .unwrap();
    if expected == *actual {
        return;
    }
    let same_version = expected["extractor"] == actual["extractor"];
    panic!(
        "{name}: extraction output changed.\n{}\nexpected:\n{}\nactual:\n{rendered}",
        if same_version {
            "The extractor's version is unchanged: bump it (output changes must be \
             versioned, ADR-0006), then regenerate with UPDATE_GOLDEN=1."
        } else {
            "The extractor's version changed: regenerate with UPDATE_GOLDEN=1."
        },
        serde_json::to_string_pretty(&expected).unwrap(),
    );
}

fn extracted(doc: &SourceDocument) -> Value {
    let e = Registry::builtin(TriageConfig::default())
        .extract(doc)
        .unwrap();
    json!({
        "extractor": format!("{}@{}", e.extractor, e.version),
        "words": e.words,
        "article": e.article,
    })
}

#[test]
fn structured_article() {
    let v = extracted(&doc("structured.html", SourceKind::Html, Some("utf-8")));
    check_golden("structured", &v);

    // Properties the snapshot must keep, whatever else changes.
    let a = &v["article"];
    assert_eq!(v["extractor"], "generic-html@1");
    assert_eq!(a["title"], "The Quiet Engineering of Canals");
    assert_eq!(a["byline"], "Ada Lovelock");
    assert_eq!(a["lang"], "en");
    let text = a["blocks"].to_string();
    assert!(!text.contains("towpath.1"), "footnote marker kept");
    assert!(!text.contains("minute books"), "footnote text kept");
    for boilerplate in ["Subscribe", "Copyright", "Related", "Narrowboat prices"] {
        assert!(!text.contains(boilerplate), "{boilerplate} kept");
    }
    let first = &a["blocks"][0];
    assert_ne!(
        first["text"], "The Quiet Engineering of Canals",
        "title repeated"
    );
    let blocks = a["blocks"].as_array().unwrap();
    let has = |f: &dyn Fn(&Value) -> bool| blocks.iter().any(f);
    assert!(has(&|b| b["type"] == "list" && b["ordered"] == true));
    assert!(has(&|b| b["type"] == "list" && b["ordered"] == false));
    assert!(has(&|b| b["type"] == "quote"));
    assert!(has(&|b| b["type"] == "heading" && b["level"] == 2));
}

#[test]
fn legacy_charset_from_meta() {
    let v = extracted(&doc("windows-1252.html", SourceKind::Html, None));
    check_golden("windows-1252", &v);
    assert_eq!(v["article"]["title"], "Café society");
    let text = v["article"]["blocks"].to_string();
    assert!(
        text.contains("naïve résumé") && text.contains("“quoted”"),
        "{text}"
    );
}

/// Codex's reproductions, through `SourceDocument::text()`: whitespace around `=` in a
/// `<meta charset>`, and a comment that mentions a charset before the real declaration.
#[test]
fn meta_charsets_are_read_as_html_declares_them() {
    let page = |bytes: &[u8]| SourceDocument {
        bytes: bytes.to_vec(),
        kind: SourceKind::Html,
        charset: None,
        final_url: None,
    };
    let spaced = page(b"<meta charset = windows-1252><p>caf\xe9</p>");
    assert_eq!(spaced.text(), "<meta charset = windows-1252><p>café</p>");
    let commented = page("<!-- charset=windows-1252 --><meta charset=utf-8><p>café</p>".as_bytes());
    assert!(
        commented.text().ends_with("<p>café</p>"),
        "{}",
        commented.text()
    );
}

#[test]
fn plain_text() {
    let v = extracted(&doc("plain.txt", SourceKind::PlainText, None));
    check_golden("plain", &v);
    assert_eq!(v["extractor"], "plain-text@1");
    assert_eq!(v["article"]["title"], "A Short History of Still Water");
    assert_eq!(v["article"]["blocks"].as_array().unwrap().len(), 4);
}

// FR-EXT-05
#[test]
fn a_login_wall_is_low_confidence() {
    let registry = Registry::builtin(TriageConfig::default());
    let result = registry.extract(&doc("login-wall.html", SourceKind::Html, None));
    let Err(Failure::LowConfidence { reasons, .. }) = result else {
        panic!("expected low confidence, got {result:?}");
    };
    // Both HTML extractors were tried.
    assert!(
        reasons.iter().any(|r| r.starts_with("generic-html")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.starts_with("readability")),
        "{reasons:?}"
    );
}
