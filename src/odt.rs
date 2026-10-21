//! OpenDocument text (.odt, from LibreOffice or Google Docs) as paragraphs.

use crate::markup::{attr, events, Ev};

/// Elements whose text isn't part of the body: comments, footnotes, deleted
/// text from tracked changes, frame titles.
const SKIP: [&str; 6] = ["office:annotation", "text:note", "text:tracked-changes", "svg:title", "svg:desc", "text:sequence-decls"];

pub fn paragraphs(content_xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut skip = 0usize;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let p = cur.split_whitespace().collect::<Vec<_>>().join(" ");
        if !p.is_empty() {
            out.push(p);
        }
        cur.clear();
    };
    events(content_xml, |ev| match ev {
        Ev::Start(name, attrs, self_closing) => {
            if SKIP.contains(&name.as_str()) {
                skip += usize::from(!self_closing);
            } else if skip > 0 {
            } else if name == "text:p" || name == "text:h" {
                flush(&mut cur, &mut out);
            } else if name == "text:s" {
                let n = attr(attrs, "text:c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1);
                cur.push_str(&" ".repeat(n.min(64)));
            } else if name == "text:tab" || name == "text:line-break" {
                cur.push(' ');
            }
        }
        Ev::End(name) => {
            if SKIP.contains(&name.as_str()) {
                skip = skip.saturating_sub(1);
            } else if skip == 0 && (name == "text:p" || name == "text:h") {
                flush(&mut cur, &mut out);
            }
        }
        Ev::Text(t) => {
            if skip == 0 {
                cur.push_str(&t);
            }
        }
    });
    flush(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn body_only() {
        let xml = r#"<office:document-content><office:body><office:text><text:sequence-decls><text:sequence-decl text:name="Table"/></text:sequence-decls>
<text:h text:outline-level="1">Chapter One</text:h><text:p>It was<text:s/>a <text:span text:style-name="T1">dark</text:span> night<text:note><text:note-body><text:p>A footnote.</text:p></text:note-body></text:note>.</text:p>
<text:p>Second<text:tab/>para<office:annotation><text:p>comment</text:p></office:annotation>.</text:p></office:text></office:body></office:document-content>"#;
        assert_eq!(super::paragraphs(xml), ["Chapter One", "It was a dark night.", "Second para."]);
    }
}
