// SPDX-License-Identifier: AGPL-3.0-or-later
//! The owner's extraction corpus (`spike/corpus/data`, git-ignored: personal and
//! third-party content). Ignored by default; run locally with
//! `cargo test -p rtm-extract --test corpus -- --ignored --nocapture`.

use std::path::Path;

use rtm_domain::SourceKind;
use rtm_extract::{Failure, Registry, SourceDocument, TriageConfig};

#[test]
#[ignore = "needs the local corpus"]
fn corpus_extracts() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spike/corpus/data");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(data.join("manifest.json")).unwrap())
            .unwrap();
    let registry = Registry::builtin(TriageConfig::default());
    let (mut ok, mut fallback, mut low, mut total) = (0, 0, 0, 0);
    for item in manifest["items"].as_array().unwrap() {
        if item["status"] != "ok" {
            continue;
        }
        let dir = data.join(item["id"].as_str().unwrap());
        let Ok(bytes) = std::fs::read(dir.join("source.html")) else {
            continue;
        };
        total += 1;
        let doc = SourceDocument {
            bytes,
            kind: SourceKind::Html,
            charset: None,
            final_url: item["url"].as_str().and_then(|u| u.parse().ok()),
        };
        match registry.extract(&doc) {
            Ok(e) => {
                ok += 1;
                if e.extractor != "generic-html" {
                    fallback += 1;
                }
                println!(
                    "ok   {:5} words  {:13} {}",
                    e.words, e.extractor, item["id"]
                );
            }
            Err(Failure::LowConfidence { best, .. }) => {
                low += 1;
                let words = best.map_or(0, |b| b.words);
                println!("LOW  {words:5} words  {:13} {}", "-", item["id"]);
            }
            Err(Failure::Unsupported) => println!("UNSUPPORTED {}", item["id"]),
        }
    }
    println!("\n{ok}/{total} extracted ({fallback} by fallback), {low} low confidence");
    assert!(ok * 100 >= total * 80, "fewer than 80% extracted");
}
