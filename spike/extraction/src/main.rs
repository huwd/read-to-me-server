// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 0 spike (issue #13): compare Rust article extractors on the corpus from #3.
//!
//! Throwaway. Reads `spike/corpus/data/`, writes each extractor's text to
//! `<article>/extracted/<extractor>.txt` (git-ignored, beside the source) and a report to
//! `spike/corpus/data/extraction-report.md`, then prints the summary.
//!
//! Scoring: bag-of-words token overlap (lowercased alphanumeric tokens) against a reference:
//! - feed articles whose feed carries full text (≥ 300 words) use the feed text;
//! - the LRB long-form pieces use their long `<p>` paragraphs (full text hidden by the paywall).
//!
//! Precision = shared / extracted, recall = shared / reference. "Clean" means F1 ≥ 0.90
//! (AC-EXT-02). A feed reference is treated as a likely *excerpt* (and left out of the
//! clean rate) when most extractors return more than 1.6× its length.

use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde::Deserialize;

const CLEAN_F1: f64 = 0.90;
const EXCERPT_RATIO: f64 = 1.6;

#[derive(Deserialize)]
struct Manifest {
    items: Vec<Item>,
}

#[derive(Deserialize, Clone)]
struct Item {
    id: String,
    kind: String,
    url: String,
    status: String,
    #[serde(default)]
    feed_title: Option<String>,
    #[serde(default)]
    feed_has_full_text: bool,
    #[serde(default)]
    source_words: Option<usize>,
}

type Extract = fn(&str, &str) -> Result<String, String>;

fn ex_dom_smoothie(html: &str, url: &str) -> Result<String, String> {
    let mut r = dom_smoothie::Readability::new(html, Some(url), None).map_err(|e| e.to_string())?;
    r.parse()
        .map(|a| a.text_content.to_string())
        .map_err(|e| e.to_string())
}

fn ex_readabilityrs(html: &str, url: &str) -> Result<String, String> {
    let r = readabilityrs::Readability::new(html, Some(url), None).map_err(|e| e.to_string())?;
    r.parse()
        .and_then(|a| a.text_content)
        .ok_or_else(|| "no article".to_string())
}

fn ex_legible(html: &str, url: &str) -> Result<String, String> {
    legible::parse(html, Some(url), None)
        .map(|a| a.text_content)
        .map_err(|e| e.to_string())
}

fn ex_trafilatura(html: &str, _url: &str) -> Result<String, String> {
    trafilatura::extract(html, &trafilatura::Options::default())
        .map(|r| r.content_text)
        .map_err(|e| e.to_string())
}

fn ex_trafilatura_recall(html: &str, _url: &str) -> Result<String, String> {
    let opts = trafilatura::Options::default()
        .with_fallback(true)
        .with_focus(trafilatura::ExtractionFocus::FavorRecall);
    trafilatura::extract(html, &opts)
        .map(|r| r.content_text)
        .map_err(|e| e.to_string())
}

fn ex_readability_old(html: &str, url: &str) -> Result<String, String> {
    let url = url::Url::parse(url).map_err(|e| e.to_string())?;
    readability::extractor::extract(&mut html.as_bytes(), &url)
        .map(|p| p.text)
        .map_err(|e| e.to_string())
}

/// Baseline: every visible word on the page (scripts, styles and tags removed).
fn ex_all_text(html: &str, _url: &str) -> Result<String, String> {
    Ok(strip_tags(html))
}

const EXTRACTORS: &[(&str, Extract)] = &[
    ("dom_smoothie", ex_dom_smoothie),
    ("readabilityrs", ex_readabilityrs),
    ("legible", ex_legible),
    ("trafilatura", ex_trafilatura),
    ("trafilatura_recall", ex_trafilatura_recall),
    ("readability_0.3", ex_readability_old),
    ("all_text", ex_all_text),
];

fn strip_tags(html: &str) -> String {
    let no_code = Regex::new(
        r"(?is)<(script|style|noscript|template)\b.*?</(script|style|noscript|template)\s*>",
    )
    .unwrap()
    .replace_all(html, " ");
    let text = Regex::new(r"(?s)<[^>]+>")
        .unwrap()
        .replace_all(&no_code, " ");
    unescape(&text)
}

fn unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&rsquo;", "'")
        .replace("&lsquo;", "'")
        .replace("&ldquo;", "\"")
        .replace("&rdquo;", "\"")
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

struct Score {
    precision: f64,
    recall: f64,
    f1: f64,
}

fn score(extracted: &[String], reference: &[String]) -> Score {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for t in reference {
        *counts.entry(t).or_default() += 1;
    }
    let mut shared = 0i64;
    for t in extracted {
        if let Some(c) = counts.get_mut(t.as_str())
            && *c > 0
        {
            *c -= 1;
            shared += 1;
        }
    }
    let precision = if extracted.is_empty() {
        0.0
    } else {
        shared as f64 / extracted.len() as f64
    };
    let recall = if reference.is_empty() {
        0.0
    } else {
        shared as f64 / reference.len() as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Score {
        precision,
        recall,
        f1,
    }
}

/// Reference text for an LRB page: the long paragraphs (the essay), excluding the app promo.
fn long_paragraph_text(html: &str) -> String {
    let p = Regex::new(r"(?is)<p\b[^>]*>(.*?)</p>").unwrap();
    p.captures_iter(html)
        .map(|c| strip_tags(&c[1]))
        .filter(|t| {
            t.split_whitespace().count() > 25 && !t.trim_start().starts_with("Read anywhere")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct Tally {
    runs: usize,
    errors: usize,
    panics: usize,
    millis: u128,
    trusted: usize,
    clean: usize,
    f1_sum: f64,
    p_sum: f64,
    r_sum: f64,
    lrb_full: usize,
    short_words: Vec<usize>,
}

struct Outcome {
    text: Option<String>,
    error: Option<String>,
}

fn run(f: Extract, html: &str, url: &str) -> (Outcome, bool, u128) {
    let start = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| f(html, url)));
    let millis = start.elapsed().as_millis();
    match result {
        Ok(Ok(text)) => (
            Outcome {
                text: Some(text),
                error: None,
            },
            false,
            millis,
        ),
        Ok(Err(e)) => (
            Outcome {
                text: None,
                error: Some(e),
            },
            false,
            millis,
        ),
        Err(_) => (
            Outcome {
                text: None,
                error: Some("panicked".into()),
            },
            true,
            millis,
        ),
    }
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let data: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/data"));
    let manifest: Manifest = serde_json::from_str(
        &fs::read_to_string(data.join("manifest.json")).expect("manifest.json"),
    )
    .expect("manifest");

    let mut tallies: Vec<Tally> = EXTRACTORS.iter().map(|_| Tally::default()).collect();
    let mut rows = Vec::new(); // per-article lines for the report
    let mut excerpt_refs = Vec::new();
    let (mut n_feed_ref, mut n_lrb, mut n_short, mut n_other) = (0, 0, 0, 0);

    for item in manifest.items.iter().filter(|i| i.status == "ok") {
        let dir = data.join(&item.id);
        let Ok(bytes) = fs::read(dir.join("source.html")) else {
            continue;
        };
        let html = String::from_utf8_lossy(&bytes).into_owned();
        let out_dir = dir.join("extracted");
        let _ = fs::create_dir_all(&out_dir);

        let is_lrb = item.kind == "long-form" && item.url.contains("lrb.co.uk");
        let is_short = item.kind == "feed" && item.source_words.unwrap_or(0) < 300;
        let reference: Option<Vec<String>> = if is_lrb {
            Some(tokens(&long_paragraph_text(&html)))
        } else if item.kind == "feed" && item.feed_has_full_text {
            fs::read_to_string(dir.join("feed.html"))
                .ok()
                .map(|f| tokens(&strip_tags(&f)))
        } else {
            None
        };
        match (is_lrb, is_short, reference.is_some()) {
            (true, ..) => n_lrb += 1,
            (_, true, _) => n_short += 1,
            (_, _, true) => n_feed_ref += 1,
            _ => n_other += 1,
        }

        let outcomes: Vec<(Outcome, bool, u128)> = EXTRACTORS
            .iter()
            .map(|(_, f)| run(*f, &html, &item.url))
            .collect();

        // Is the feed reference just an excerpt? (most real extractors return far more)
        let mut excerpt = false;
        if let (Some(r), false) = (&reference, is_lrb) {
            let mut lens: Vec<usize> = outcomes[..EXTRACTORS.len() - 1]
                .iter()
                .filter_map(|(o, ..)| o.text.as_ref().map(|t| tokens(t).len()))
                .collect();
            lens.sort_unstable();
            if let Some(&median) = lens.get(lens.len() / 2) {
                excerpt = median as f64 > EXCERPT_RATIO * r.len() as f64;
            }
            if excerpt {
                excerpt_refs.push(item.id.clone());
            }
        }

        let mut cells = Vec::new();
        for (i, ((name, _), (outcome, panicked, millis))) in
            EXTRACTORS.iter().zip(outcomes).enumerate()
        {
            let t = &mut tallies[i];
            t.runs += 1;
            t.millis += millis;
            if panicked {
                t.panics += 1;
            }
            let text = match outcome.text {
                Some(text) => text,
                None => {
                    t.errors += 1;
                    let _ = fs::write(
                        out_dir.join(format!("{name}.error.txt")),
                        outcome.error.unwrap_or_default(),
                    );
                    cells.push("err".to_string());
                    continue;
                }
            };
            let _ = fs::write(out_dir.join(format!("{name}.txt")), &text);
            let toks = tokens(&text);
            if is_short {
                t.short_words.push(toks.len());
            }
            match &reference {
                Some(r) if is_lrb => {
                    let s = score(&toks, r);
                    if s.recall >= 0.90 {
                        t.lrb_full += 1;
                    }
                    cells.push(format!("R{:.2}", s.recall));
                }
                Some(r) if !excerpt && !is_short => {
                    let s = score(&toks, r);
                    t.trusted += 1;
                    t.f1_sum += s.f1;
                    t.p_sum += s.precision;
                    t.r_sum += s.recall;
                    if s.f1 >= CLEAN_F1 {
                        t.clean += 1;
                    }
                    cells.push(format!("{:.2}", s.f1));
                }
                _ => cells.push(format!("{}w", toks.len())),
            }
        }
        let label = item.feed_title.clone().unwrap_or_else(|| item.kind.clone());
        rows.push(format!(
            "| {} | {} | {} |",
            item.id.chars().take(42).collect::<String>(),
            label.chars().take(24).collect::<String>(),
            cells.join(" | ")
        ));
    }

    // ---- report ----
    let mut out = String::new();
    out.push_str("# Extraction spike report (issue #13)\n\n");
    out.push_str(&format!(
        "Articles: {n_feed_ref} with a feed full-text reference, {n_lrb} LRB long-form (hidden full text), \
         {n_short} very short pages (< 300 words), {n_other} with no reference.\n\
         Feed references treated as excerpts and excluded from the clean rate: {} ({}).\n\n",
        excerpt_refs.len(),
        excerpt_refs.join(", ")
    ));
    out.push_str("| Extractor | Clean (F1 ≥ 0.90) | Mean F1 | Mean precision | Mean recall | LRB full text | Errors | Panics | Median words on short pages | Total ms |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for ((name, _), t) in EXTRACTORS.iter().zip(&tallies) {
        let n = t.trusted.max(1) as f64;
        let mut sw = t.short_words.clone();
        sw.sort_unstable();
        out.push_str(&format!(
            "| {name} | {}/{} ({:.0}%) | {:.3} | {:.3} | {:.3} | {}/{n_lrb} | {} | {} | {} | {} |\n",
            t.clean,
            t.trusted,
            100.0 * t.clean as f64 / n,
            t.f1_sum / n,
            t.p_sum / n,
            t.r_sum / n,
            t.lrb_full,
            t.errors,
            t.panics,
            sw.get(sw.len() / 2).copied().unwrap_or(0),
            t.millis
        ));
    }
    out.push_str("\n## Per article\n\nFeed-reference articles: F1. LRB: `R` = recall of the hidden essay. Others: words extracted.\n\n");
    out.push_str(&format!(
        "| Article | Feed | {} |\n|---|---|{}\n",
        EXTRACTORS
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(" | "),
        "---|".repeat(EXTRACTORS.len())
    ));
    for r in &rows {
        out.push_str(r);
        out.push('\n');
    }
    let report = data.join("extraction-report.md");
    fs::write(&report, &out).expect("write report");
    // Print the summary (everything before the per-article table).
    print!("{}", out.split("\n## Per article").next().unwrap_or(&out));
    println!("\nFull report: {}", report.display());
}
