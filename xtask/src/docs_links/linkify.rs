// SPDX-License-Identifier: AGPL-3.0-or-later
//! `docs-links fix`: add definition anchors, then turn references into relative links.
//!
//! References are rewritten one line at a time. Text that must not be touched (existing
//! links, anchors, URLs, code spans, freshly generated links) is swapped for a
//! `\0N\0` placeholder first and restored at the end, so later patterns never see it.

use std::io;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::model::{ADR_INDEX, Docs, is_fence, is_heading, rel};

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| re($pattern));
    };
}

// Step A: definition anchors.
regex!(DEF_FR, r"^(\s*)- \*\*(FR-[A-Z]+-\d+)\*\*");
regex!(DEF_AC, r"^### (AC-[A-Z]+) ");
regex!(DEF_NFR, r"^\| (NFR-(?:[A-Z]+|\$)\d+b?) \|");
regex!(DEF_ASSUMPTION, r"^\| (A[1-7]) \|");
regex!(DEF_QUESTION, r"^\| (Q[1-9]) \|");
regex!(DEF_FOLLOWUP, r"^\| (F[1-9]) \|");
regex!(THREAT_ID, r"\b(?:AML\.T\d{4}|T\d{4}(?:\.\d{3})?)\b");

// Step B: protection.
regex!(EXISTING_LINK, r"\[[^\]]*\]\([^)]*\)");
regex!(ANCHOR_TAG, r#"<a id="[^"]*"></a>"#);
regex!(URL, r"https?://\S+");
regex!(CODE_SPAN, r"`[^`]*`");
regex!(
    DEF_TOKEN,
    r"(\x00\d+\x00)(\*\*FR-[A-Z]+-\d+\*\*|(?:NFR-(?:[A-Z]+|\$)\d+b?|Q[1-9]|F[1-9]|A[1-7])\b)"
);
regex!(PLACEHOLDER, r"\x00(\d+)\x00");

// Step B: references.
regex!(ADR_REF, r"ADR-(\d{4})(?: (§\d+(?:\.\d+)*|F\d)\b)?");
regex!(FOLLOWUP_REF, r"follow-up (F\d)\b");
regex!(LOCAL_FOLLOWUP_REF, r"\((?:see )?F\d\b");
regex!(DOCS_SLASH, r"\bdocs (0[1-7])/(0[1-7])\b");
regex!(DOCS_AND, r"\bdocs (0[1-7]) and (0[1-7])\b");
regex!(DOC07_QUESTION, r"\b[Dd]oc 07 (Q[1-9])\b");
regex!(DOC07_DEFERRED, r#"\b[Dd]oc 07 "Deferred""#);
regex!(
    DOC_REF,
    r"\b[Dd]oc (0[1-7])(?: (§\d+(?:\.\d+)*(?:–\d+(?:\.\d+)*)?)(?: and (§\d+(?:\.\d+)*))?)?"
);
regex!(BARE_SECTION, r"§(\d+(?:\.\d+)*)\b");
regex!(RFC_BEFORE, r"RFC \d{4} $");
regex!(FR_REF, r"\b(FR-[A-Z]+-\d+)((?:/\d+)*)");
regex!(FR_EXTRA, r"/(\d+)");
regex!(FR_GROUP_REF, r"\bFR-([A-Z]+)-\*");
regex!(
    NFR_REF,
    r"NFR-((?:[A-Z]+|\$)\d+b?)((?:/(?:[A-Z]+|\$)\d+b?)*)"
);
regex!(NFR_EXTRA, r"/((?:[A-Z]+|\$)\d+b?)");
regex!(AC_REF, r"\bAC-([A-Z]+)(?:-\d+(?:/\d+)?)?\b");
regex!(QUESTION_REF, r"\bQ([1-9])\b");
regex!(ASSUMPTION_REF, r"\b(assumptions?) (A[1-7])(?:–(A[1-7]))?");
regex!(ATLAS_REF, r"\bAML\.T\d{4}\b");
regex!(TECHNIQUE_REF, r"\bT\d{4}(?:\.\d{3})?\b");
regex!(CWE_REF, r"\bCWE-(\d+)\b");

/// Code spans that name a repo file get linked to it.
const CODE_FILES: &[(&str, &str)] = &[
    ("`deny.toml`", "deny.toml"),
    ("`CONTRIBUTING.md`", "CONTRIBUTING.md"),
    ("`docs/adr/`", ADR_INDEX),
];

pub struct FixReport {
    pub changed_files: usize,
    pub missing_anchors: Vec<String>,
}

fn mitre_technique(id: &str) -> String {
    match id.split_once('.') {
        Some((base, sub)) => format!("https://attack.mitre.org/techniques/{base}/{sub}/"),
        None => format!("https://attack.mitre.org/techniques/{id}/"),
    }
}

fn threat_url(id: &str) -> String {
    if id.starts_with("AML") {
        format!("https://atlas.mitre.org/techniques/{id}")
    } else {
        mitre_technique(id)
    }
}

fn threat_anchor(id: &str) -> String {
    id.to_lowercase().replace('.', "-")
}

fn nfr_anchor(id: &str) -> String {
    format!("nfr-{}", id.to_lowercase().replace('$', "dollar"))
}

/// Step A for one file: insert `<a id>` anchors at definitions. Idempotent.
pub fn add_anchors(docs: &Docs, path: &str, text: &str) -> String {
    let is = |key: &str, map: &std::collections::BTreeMap<String, String>| {
        map.get(key).is_some_and(|p| p == path)
    };
    let (d01, d03, d06, d07) = (
        is("01", &docs.doc),
        is("03", &docs.doc),
        is("06", &docs.doc),
        is("07", &docs.doc),
    );
    let a10 = is("0010", &docs.adr);
    let anchored_row = |re: &Regex, line: &str, id: fn(&str) -> String| -> String {
        re.replace(line, |c: &Captures| {
            format!("| <a id=\"{}\"></a>{} |", id(&c[1]), &c[1])
        })
        .into_owned()
    };
    text.split('\n')
        .map(|line| {
            let mut line = line.to_string();
            let has_anchor = line.contains("<a id");
            if d01 && !has_anchor {
                line = DEF_FR
                    .replace(&line, |c: &Captures| {
                        format!(
                            "{}- <a id=\"{}\"></a>**{}**",
                            &c[1],
                            c[2].to_lowercase(),
                            &c[2]
                        )
                    })
                    .into_owned();
                line = DEF_AC
                    .replace(&line, |c: &Captures| {
                        format!("### <a id=\"{}\"></a>{} ", c[1].to_lowercase(), &c[1])
                    })
                    .into_owned();
            }
            if d03 {
                line = anchored_row(&DEF_NFR, &line, nfr_anchor);
                line = anchored_row(&DEF_ASSUMPTION, &line, |s| s.to_lowercase());
            }
            if d07 {
                line = anchored_row(&DEF_QUESTION, &line, |s| s.to_lowercase());
            }
            if a10 {
                line = anchored_row(&DEF_FOLLOWUP, &line, |s| s.to_lowercase());
            }
            if d06 && line.starts_with("| ") && !has_anchor {
                let mut cells: Vec<String> = line.split('|').map(String::from).collect();
                if cells.len() > 1 {
                    let first = cells[1].clone();
                    let ids: Vec<String> = THREAT_ID
                        .find_iter(&first)
                        .map(|m| m.as_str().to_string())
                        .collect();
                    if !ids.is_empty() {
                        let anchors: String = ids
                            .iter()
                            .map(|i| format!("<a id=\"{}\"></a>", threat_anchor(i)))
                            .collect();
                        let linked = THREAT_ID.replace_all(&first, |c: &Captures| {
                            format!("[{}]({})", &c[0], threat_url(&c[0]))
                        });
                        cells[1] = format!(" {anchors}{}", linked.trim_start());
                        line = cells.join("|");
                    }
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Per-line rewriting state.
struct Line<'a> {
    docs: &'a Docs,
    src: &'a str,
    store: Vec<String>,
    missing: &'a mut Vec<String>,
}

impl Line<'_> {
    fn put(&mut self, markdown: String) -> String {
        self.store.push(markdown);
        format!("\x00{}\x00", self.store.len() - 1)
    }

    fn link(&mut self, text: &str, dst: &str, anchor: Option<String>) -> String {
        let anchor = match anchor {
            Some(a) if !self.docs.has_anchor(dst, &a) => {
                self.missing
                    .push(format!("{}: '{text}' -> {dst}#{a}", self.src));
                None
            }
            other => other,
        };
        if dst == self.src && anchor.is_none() {
            return text.to_string();
        }
        let target = rel(self.src, dst, anchor.as_deref());
        self.put(format!("[{text}]({target})"))
    }

    fn doc_path(&self, number: &str) -> Option<String> {
        self.docs.doc.get(number).cloned()
    }

    fn doc_link(&mut self, text: &str, number: &str, section: Option<&str>) -> String {
        match self.doc_path(number) {
            Some(dst) => {
                let anchor = section.and_then(|s| self.docs.section(&dst, s));
                self.link(text, &dst, anchor)
            }
            None => text.to_string(),
        }
    }

    fn restore(&self, mut line: String) -> String {
        while line.contains('\x00') {
            line = PLACEHOLDER
                .replace_all(&line, |c: &Captures| {
                    let i: usize = c[1].parse().expect("placeholder index");
                    self.store[i].clone()
                })
                .into_owned();
        }
        line
    }
}

/// Rewrite the references in one body line of `src`.
pub fn linkify_line(docs: &Docs, src: &str, line: &str, missing: &mut Vec<String>) -> String {
    let d01 = docs.doc.get("01").cloned().unwrap_or_default();
    let d03 = docs.doc.get("03").cloned().unwrap_or_default();
    let d06 = docs.doc.get("06").cloned().unwrap_or_default();
    let d07 = docs.doc.get("07").cloned().unwrap_or_default();
    let a10 = docs.adr.get("0010").cloned().unwrap_or_default();
    let mut cx = Line {
        docs,
        src,
        store: Vec::new(),
        missing,
    };
    let mut line = line.to_string();
    macro_rules! sub {
        ($re:expr, $f:expr) => {
            line = $re.replace_all(&line, $f).into_owned();
        };
    }

    // Protect what must not be touched.
    sub!(EXISTING_LINK, |c: &Captures| cx.put(c[0].to_string()));
    sub!(ANCHOR_TAG, |c: &Captures| cx.put(c[0].to_string()));
    sub!(URL, |c: &Captures| cx.put(c[0].to_string()));
    sub!(CODE_SPAN, |c: &Captures| {
        match CODE_FILES.iter().find(|(code, _)| *code == &c[0]) {
            Some((code, target)) if *target != src => {
                let md = format!("[{code}]({})", rel(src, target, None));
                cx.put(md)
            }
            _ => cx.put(c[0].to_string()),
        }
    });
    // A definition sits right after its anchor; keep it plain.
    sub!(DEF_TOKEN, |c: &Captures| {
        let token = cx.put(c[2].to_string());
        format!("{}{token}", &c[1])
    });

    // ADRs.
    sub!(ADR_REF, |c: &Captures| {
        let dst = docs
            .adr
            .get(&c[1])
            .cloned()
            .unwrap_or_else(|| ADR_INDEX.to_string());
        let anchor = match c.get(2).map(|m| m.as_str()) {
            Some(extra) if extra.starts_with('§') => docs.section(&dst, extra),
            Some(extra) => Some(extra.to_lowercase()),
            None => None,
        };
        cx.link(&c[0], &dst, anchor)
    });
    sub!(FOLLOWUP_REF, |c: &Captures| cx.link(
        &c[0],
        &a10,
        Some(c[1].to_lowercase())
    ));
    if src == a10 {
        sub!(LOCAL_FOLLOWUP_REF, |c: &Captures| {
            let text = &c[0][1..];
            let id = text[text.len() - 2..].to_lowercase();
            format!("({}", cx.link(text, &a10, Some(id)))
        });
    }

    // Docs and sections.
    sub!(DOCS_SLASH, |c: &Captures| {
        let a = cx.doc_link(&c[1], &c[1], None);
        let b = cx.doc_link(&c[2], &c[2], None);
        format!("docs {a}/{b}")
    });
    sub!(DOCS_AND, |c: &Captures| {
        let a = cx.doc_link(&c[1], &c[1], None);
        let b = cx.doc_link(&c[2], &c[2], None);
        format!("docs {a} and {b}")
    });
    sub!(DOC07_QUESTION, |c: &Captures| cx.link(
        &c[0],
        &d07,
        Some(c[1].to_lowercase())
    ));
    sub!(DOC07_DEFERRED, |c: &Captures| {
        let anchor = docs.deferred.clone();
        cx.link(&c[0], &d07, anchor)
    });
    sub!(DOC_REF, |c: &Captures| {
        let number = &c[1];
        let first_section = c.get(2).map(|m| m.as_str());
        match c.get(3) {
            Some(second) => {
                let text = format!("{} {number} {}", &c[0][..3], first_section.unwrap_or(""));
                let first = cx.doc_link(&text, number, first_section);
                let second = cx.doc_link(second.as_str(), number, Some(second.as_str()));
                format!("{first} and {second}")
            }
            None => cx.doc_link(&c[0], number, first_section),
        }
    });
    {
        // Bare "§N" refers to the same file, except after "GPL-3.0 " or "RFC NNNN ".
        let haystack = line.clone();
        sub!(BARE_SECTION, |c: &Captures| {
            let m = c.get(0).expect("whole match");
            let before = &haystack[..m.start()];
            let prev = before.chars().next_back();
            let blocked = prev.is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '\x00')
                || before.ends_with("GPL-3.0 ")
                || RFC_BEFORE.is_match(before);
            match docs.sections.get(src).and_then(|s| s.get(&c[1])).cloned() {
                Some(anchor) if !blocked => cx.link(&c[0], src, Some(anchor)),
                _ => c[0].to_string(),
            }
        });
    }

    // Requirements, acceptance criteria, questions, assumptions.
    sub!(FR_REF, |c: &Captures| {
        let id = &c[1];
        let prefix = id
            .rsplit_once('-')
            .map(|(p, _)| p)
            .unwrap_or(id)
            .to_lowercase();
        let mut out = cx.link(id, &d01, Some(id.to_lowercase()));
        for extra in FR_EXTRA.captures_iter(&c[2]) {
            let n = &extra[1];
            out.push('/');
            out.push_str(&cx.link(n, &d01, Some(format!("{prefix}-{n}"))));
        }
        out
    });
    sub!(FR_GROUP_REF, |c: &Captures| {
        let anchor = docs.fr_groups.get(&c[1]).cloned();
        cx.link(&c[0], &d01, anchor)
    });
    sub!(NFR_REF, |c: &Captures| {
        let mut parts = vec![cx.link(&format!("NFR-{}", &c[1]), &d03, Some(nfr_anchor(&c[1])))];
        for extra in NFR_EXTRA.captures_iter(&c[2]) {
            parts.push(cx.link(&extra[1], &d03, Some(nfr_anchor(&extra[1]))));
        }
        parts.join("/")
    });
    sub!(AC_REF, |c: &Captures| cx.link(
        &c[0],
        &d01,
        Some(format!("ac-{}", c[1].to_lowercase()))
    ));
    sub!(QUESTION_REF, |c: &Captures| cx.link(
        &c[0],
        &d07,
        Some(format!("q{}", &c[1]))
    ));
    sub!(ASSUMPTION_REF, |c: &Captures| {
        let first = cx.link(&c[2], &d03, Some(c[2].to_lowercase()));
        let second = c
            .get(3)
            .map(|m| {
                format!(
                    "–{}",
                    cx.link(m.as_str(), &d03, Some(m.as_str().to_lowercase()))
                )
            })
            .unwrap_or_default();
        format!("{} {first}{second}", &c[1])
    });

    // Threats: MITRE pages inside the security doc, the security doc's rows elsewhere.
    // ATLAS first, so `T0051` inside `AML.T0051` isn't taken for an ATT&CK technique.
    sub!(ATLAS_REF, |c: &Captures| {
        if src == d06 {
            cx.put(format!("[{}]({})", &c[0], threat_url(&c[0])))
        } else {
            cx.link(&c[0], &d06, Some(threat_anchor(&c[0])))
        }
    });
    sub!(TECHNIQUE_REF, |c: &Captures| {
        if src == d06 {
            cx.put(format!("[{}]({})", &c[0], mitre_technique(&c[0])))
        } else {
            cx.link(&c[0], &d06, Some(threat_anchor(&c[0])))
        }
    });
    sub!(CWE_REF, |c: &Captures| {
        cx.put(format!(
            "[{}](https://cwe.mitre.org/data/definitions/{}.html)",
            &c[0], &c[1]
        ))
    });

    cx.restore(line)
}

/// Rewrite every body line (outside headings and code fences) of one file.
pub fn linkify_text(docs: &Docs, src: &str, text: &str, missing: &mut Vec<String>) -> String {
    let mut fence = false;
    text.split('\n')
        .map(|line| {
            if is_fence(line) {
                fence = !fence;
                line.to_string()
            } else if fence || is_heading(line) {
                line.to_string()
            } else {
                linkify_line(docs, src, line, missing)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run both steps over the whole docs estate, writing changed files.
pub fn fix(docs: &mut Docs) -> io::Result<FixReport> {
    let mut changed = std::collections::BTreeSet::new();
    for f in docs.files.clone() {
        let text = docs.read(&f)?;
        let new = add_anchors(docs, &f, &text);
        if new != text {
            docs.write(&f, &new)?;
            changed.insert(f);
        }
    }
    docs.reindex()?;
    let mut missing = Vec::new();
    for f in docs.files.clone() {
        let text = docs.read(&f)?;
        let new = linkify_text(docs, &f, &text, &mut missing);
        if new != text {
            docs.write(&f, &new)?;
            changed.insert(f);
        }
    }
    Ok(FixReport {
        changed_files: changed.len(),
        missing_anchors: missing,
    })
}
