// SPDX-License-Identifier: AGPL-3.0-or-later
//! Converting an extractor's cleaned HTML into `Article` blocks.

use rtm_article::{Block, clean_text};
use scraper::{ElementRef, Html, Node};

/// Elements whose content is never narrated.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "math", "iframe", "object", "button", "form",
    "input", "select", "textarea", "nav", "aside", "footer",
];

/// Whether a `<sup>` is a footnote or reference marker ("1", "23", "*", "†", "[4]"),
/// which would otherwise be read aloud as part of the preceding word.
fn is_reference_marker(text: &str) -> bool {
    let t = text.trim().trim_start_matches('[').trim_end_matches(']');
    !t.is_empty()
        && t.len() <= 6
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '*' | '†' | '‡' | '§' | ',' | '-'))
}

/// The text of an element, as it should be read: line breaks become spaces, reference
/// markers and skipped elements are dropped.
fn inline_text(el: ElementRef<'_>) -> String {
    let mut out = String::new();
    collect_inline(el, &mut out);
    clean_text(&out)
}

fn collect_inline(el: ElementRef<'_>, out: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) => {
                let Some(child) = ElementRef::wrap(child) else {
                    continue;
                };
                let name = e.name();
                if SKIP.contains(&name) {
                    continue;
                }
                if name == "sup" && is_reference_marker(&child.text().collect::<String>()) {
                    continue;
                }
                if name == "br" {
                    out.push(' ');
                    continue;
                }
                // Block-level children inside an inline context still need separating.
                if matches!(name, "p" | "div" | "li" | "td" | "th") {
                    out.push(' ');
                }
                collect_inline(child, out);
            }
            _ => {}
        }
    }
}

fn list_items(el: ElementRef<'_>) -> Vec<String> {
    el.children()
        .filter_map(ElementRef::wrap)
        .filter(|c| c.value().name() == "li")
        .map(inline_text)
        .filter(|t| !t.is_empty())
        .collect()
}

fn table(el: ElementRef<'_>) -> Block {
    let mut caption = None;
    let mut rows = Vec::new();
    for node in el.descendants().filter_map(ElementRef::wrap) {
        match node.value().name() {
            "caption" => caption = Some(inline_text(node)),
            "tr" => rows.push(
                node.children()
                    .filter_map(ElementRef::wrap)
                    .filter(|c| matches!(c.value().name(), "td" | "th"))
                    .map(inline_text)
                    .collect(),
            ),
            _ => {}
        }
    }
    Block::Table { caption, rows }
}

fn figure(el: ElementRef<'_>) -> Block {
    let mut alt = None;
    let mut caption = None;
    for node in el.descendants().filter_map(ElementRef::wrap) {
        match node.value().name() {
            "img" => {
                alt = node
                    .value()
                    .attr("alt")
                    .map(clean_text)
                    .filter(|a| !a.is_empty())
            }
            "figcaption" => caption = Some(inline_text(node)),
            _ => {}
        }
    }
    Block::Figure { alt, caption }
}

/// Convert the children of `el` into blocks, appending to `out`. Loose inline content
/// between block elements becomes its own paragraph.
fn walk(el: ElementRef<'_>, out: &mut Vec<Block>) {
    let mut loose = String::new();
    let flush = |loose: &mut String, out: &mut Vec<Block>| {
        let text = clean_text(loose);
        if !text.is_empty() {
            out.push(Block::Paragraph { text });
        }
        loose.clear();
    };
    for child in el.children() {
        match child.value() {
            Node::Text(t) => loose.push_str(t),
            Node::Element(e) => {
                let Some(child) = ElementRef::wrap(child) else {
                    continue;
                };
                let name = e.name();
                if SKIP.contains(&name) {
                    continue;
                }
                let block = match name {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(Block::Heading {
                        level: name[1..].parse().unwrap_or(2),
                        text: inline_text(child),
                    }),
                    "p" => Some(Block::Paragraph {
                        text: inline_text(child),
                    }),
                    "ul" | "ol" => Some(Block::List {
                        ordered: name == "ol",
                        items: list_items(child),
                    }),
                    "blockquote" => {
                        let mut inner = Vec::new();
                        walk(child, &mut inner);
                        Some(Block::Quote { blocks: inner })
                    }
                    "pre" => Some(Block::Code {
                        text: child.text().collect(),
                    }),
                    "table" => Some(table(child)),
                    "figure" | "img" => Some(if name == "img" {
                        Block::Figure {
                            alt: child
                                .value()
                                .attr("alt")
                                .map(clean_text)
                                .filter(|a| !a.is_empty()),
                            caption: None,
                        }
                    } else {
                        figure(child)
                    }),
                    "hr" => Some(Block::Separator),
                    // Containers: look inside.
                    "div" | "section" | "article" | "main" | "header" | "body" | "html" | "dl"
                    | "details" => {
                        flush(&mut loose, out);
                        walk(child, out);
                        None
                    }
                    // Anything else is inline content.
                    _ => {
                        if !(name == "sup"
                            && is_reference_marker(&child.text().collect::<String>()))
                        {
                            if name == "br" {
                                loose.push(' ');
                            } else {
                                collect_inline(child, &mut loose);
                            }
                        }
                        None
                    }
                };
                if let Some(block) = block {
                    flush(&mut loose, out);
                    out.push(block);
                }
            }
            _ => {}
        }
    }
    flush(&mut loose, out);
}

/// Blocks from an HTML fragment.
pub fn from_html(fragment: &str) -> Vec<Block> {
    let doc = Html::parse_fragment(fragment);
    let mut out = Vec::new();
    walk(doc.root_element(), &mut out);
    out
}

/// Drop a leading heading that repeats the title: the narrator reads the title first.
pub fn drop_repeated_title(blocks: &mut Vec<Block>, title: &str) {
    let normal = |s: &str| clean_text(s).to_lowercase();
    if let Some(Block::Heading { text, .. }) = blocks.first()
        && normal(text) == normal(title)
    {
        blocks.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(t: &str) -> Block {
        Block::Paragraph { text: t.into() }
    }

    #[test]
    fn structure_is_kept() {
        let blocks = from_html(
            "<h2>Head</h2><p>One <em>two</em><br>three.</p><ul><li>a</li><li> b </li></ul>\
             <blockquote><p>Quoted.</p></blockquote><ol><li>x</li></ol><hr><pre>code  here</pre>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 2,
                    text: "Head".into()
                },
                para("One two three."),
                Block::List {
                    ordered: false,
                    items: vec!["a".into(), "b".into()]
                },
                Block::Quote {
                    blocks: vec![para("Quoted.")]
                },
                Block::List {
                    ordered: true,
                    items: vec!["x".into()]
                },
                Block::Separator,
                Block::Code {
                    text: "code  here".into()
                },
            ]
        );
    }

    #[test]
    fn reference_markers_are_dropped_but_real_superscripts_kept() {
        let blocks = from_html(
            "<p>In 1914.<sup>23</sup> Then<sup>[4]</sup> more<sup>*</sup>.</p><p>E = mc<sup>2</sup> and x<sup>n</sup></p>",
        );
        // "2" in mc² is indistinguishable from a marker here; markers win, which costs
        // little in narration ("mc", then the rest).
        assert_eq!(blocks[0], para("In 1914. Then more."));
        assert_eq!(blocks[1], para("E = mc and xn"));
    }

    #[test]
    fn loose_text_and_containers() {
        let blocks = from_html("Intro text<div><p>Inside</p>tail</div><script>bad()</script>");
        assert_eq!(
            blocks,
            vec![para("Intro text"), para("Inside"), para("tail")]
        );
    }

    #[test]
    fn tables_and_figures() {
        let blocks = from_html(
            "<figure><img src=x alt=\"A lock\"><figcaption>Lock 7</figcaption></figure>\
             <table><caption>Levels</caption><tr><th>Lock</th><th>Rise</th></tr><tr><td>7</td><td>2 m</td></tr></table>",
        );
        assert_eq!(
            blocks[0],
            Block::Figure {
                alt: Some("A lock".into()),
                caption: Some("Lock 7".into())
            }
        );
        assert_eq!(
            blocks[1],
            Block::Table {
                caption: Some("Levels".into()),
                rows: vec![
                    vec!["Lock".into(), "Rise".into()],
                    vec!["7".into(), "2 m".into()]
                ]
            }
        );
    }

    #[test]
    fn repeated_title_heading_is_dropped() {
        let mut blocks = from_html("<h1>The  Title</h1><p>Body</p>");
        drop_repeated_title(&mut blocks, "the title");
        assert_eq!(blocks, vec![para("Body")]);
        let mut blocks = from_html("<h1>Different</h1>");
        drop_repeated_title(&mut blocks, "The Title");
        assert_eq!(blocks.len(), 1);
    }
}
