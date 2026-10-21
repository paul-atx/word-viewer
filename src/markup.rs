//! A small, lenient reader for XML and HTML (ODT content, EPUB chapters, web
//! pages): tags and text in document order, entities decoded. It never builds a
//! tree, so deeply nested or malformed markup costs nothing extra.

/// One piece of markup, in document order.
pub enum Ev<'a> {
    /// A start tag: lowercase name (with any namespace prefix), its raw
    /// attributes, and whether it closes itself (`<br/>`).
    Start(String, &'a str, bool),
    End(String),
    Text(String),
}

/// Call `f` for each tag and run of text in `s`.
pub fn events<'a>(s: &'a str, mut f: impl FnMut(Ev<'a>)) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let Some(lt) = s[i..].find('<').map(|n| i + n) else {
            f(Ev::Text(decode(&s[i..])));
            return;
        };
        if lt > i {
            f(Ev::Text(decode(&s[i..lt])));
        }
        let rest = &s[lt..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(b.len(), |n| lt + n + 3);
            continue;
        }
        if let Some(data) = rest.strip_prefix("<![CDATA[") {
            let n = data.find("]]>").unwrap_or(data.len());
            f(Ev::Text(data[..n].to_string()));
            i = lt + 9 + n + 3;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = rest.find('>').map_or(b.len(), |n| lt + n + 1);
            continue;
        }
        // a tag: find its end, skipping '>' inside quoted attribute values
        let mut j = lt + 1;
        let mut quote = 0u8;
        while j < b.len() {
            match b[j] {
                q @ (b'"' | b'\'') if quote == 0 => quote = q,
                q if q == quote => quote = 0,
                b'>' if quote == 0 => break,
                _ => {}
            }
            j += 1;
        }
        let inner = &s[lt + 1..j.min(b.len())];
        i = j + 1;
        let (close, inner) = match inner.strip_prefix('/') {
            Some(x) => (true, x),
            None => (false, inner),
        };
        let name_end = inner.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            f(Ev::Text("<".into())); // a stray '<' in text
            i = lt + 1;
            continue;
        }
        if close {
            f(Ev::End(name));
            continue;
        }
        let self_closing = inner.ends_with('/');
        let attrs = &inner[name_end..inner.len() - usize::from(self_closing)];
        // HTML's raw-text elements: their contents are not markup
        if matches!(name.as_str(), "script" | "style") && !self_closing {
            let end = format!("</{name}");
            let body = &s[i.min(b.len())..];
            let n = body.to_ascii_lowercase().find(&end).unwrap_or(body.len());
            i += n;
            f(Ev::Start(name, attrs, false));
            continue;
        }
        f(Ev::Start(name, attrs, self_closing));
    }
}

/// The value of attribute `key` (e.g. `full-path`, `text:c`) in a tag's raw attributes.
pub fn attr(attrs: &str, key: &str) -> Option<String> {
    let mut rest = attrs;
    while let Some(n) = rest.find(key) {
        let before = rest[..n].chars().last();
        let after = rest[n + key.len()..].trim_start();
        rest = &rest[n + key.len()..];
        if before.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let Some(v) = after.strip_prefix('=') else { continue };
        let v = v.trim_start();
        let q = v.chars().next()?;
        let value = if q == '"' || q == '\'' {
            let body = &v[1..];
            &body[..body.find(q).unwrap_or(body.len())]
        } else {
            &v[..v.find(|c: char| c.is_whitespace()).unwrap_or(v.len())]
        };
        return Some(decode(value));
    }
    None
}

/// Decode character references and the common HTML named entities.
pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(n) = rest.find('&') {
        out.push_str(&rest[..n]);
        rest = &rest[n..];
        let semi = rest.as_bytes().iter().take(12).position(|&c| c == b';');
        let ch = semi.and_then(|e| {
            let ent = &rest[1..e];
            match ent.strip_prefix('#') {
                Some(num) => {
                    let v = match num.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => num.parse().ok(),
                    };
                    v.map(|v| char::from_u32(v).unwrap_or('\u{FFFD}'))
                }
                None => named(ent),
            }
        });
        match (ch, semi) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named(e: &str) -> Option<char> {
    Some(match e {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{A0}',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "eacute" => 'é',
        "egrave" => 'è',
        "ecirc" => 'ê',
        "aacute" => 'á',
        "agrave" => 'à',
        "acirc" => 'â',
        "iacute" => 'í',
        "oacute" => 'ó',
        "uacute" => 'ú',
        "ntilde" => 'ñ',
        "ccedil" => 'ç',
        "uuml" => 'ü',
        "ouml" => 'ö',
        "auml" => 'ä',
        "szlig" => 'ß',
        "shy" => '\u{AD}',
        "thinsp" | "ensp" | "emsp" => ' ',
        _ => return None,
    })
}

/// Paragraphs of readable text from an HTML or XHTML page. Block elements start
/// new paragraphs; scripts, styles, the document head and navigation are skipped.
pub fn html_paragraphs(html: &str) -> Vec<String> {
    const BLOCKS: [&str; 22] = [
        "p", "div", "h1", "h2", "h3", "h4", "h5", "h6", "li", "blockquote", "br", "tr", "section", "article",
        "header", "footer", "pre", "hr", "dt", "dd", "figcaption", "caption",
    ];
    const SKIP: [&str; 9] = ["head", "script", "style", "nav", "svg", "math", "noscript", "template", "title"];
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut skip: Vec<String> = Vec::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let p = cur.split_whitespace().collect::<Vec<_>>().join(" ");
        if !p.is_empty() {
            out.push(p);
        }
        cur.clear();
    };
    events(html, |ev| match ev {
        Ev::Start(name, _, self_closing) => {
            let local = name.rsplit(':').next().unwrap_or(&name).to_string();
            if !self_closing && SKIP.contains(&local.as_str()) {
                skip.push(local);
            } else if skip.is_empty() && BLOCKS.contains(&local.as_str()) {
                flush(&mut cur, &mut out);
            }
        }
        Ev::End(name) => {
            let local = name.rsplit(':').next().unwrap_or(&name);
            if skip.last().is_some_and(|s| s == local) {
                skip.pop();
            } else if skip.is_empty() && BLOCKS.contains(&local) {
                flush(&mut cur, &mut out);
            }
        }
        Ev::Text(t) => {
            if skip.is_empty() {
                cur.push_str(&t);
            }
        }
    });
    flush(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html() {
        let h = "<html><head><title>T</title><style>p{a:b}</style></head><body><h1>Chapter&nbsp;One</h1>\
                 <p>It was a <em>dark</em> night &amp; cold.<br/>Line two.</p><script>if (a<b) x()</script>\
                 <!-- note --><p class=\"x > y\">Caf&#233; &#x2014; end</p><nav><p>skip</p></nav></body></html>";
        assert_eq!(html_paragraphs(h), ["Chapter One", "It was a dark night & cold.", "Line two.", "Café — end"]);
    }

    #[test]
    fn attrs() {
        assert_eq!(attr(r#" full-path="OEBPS/content.opf" media-type='x'"#, "full-path").as_deref(), Some("OEBPS/content.opf"));
        assert_eq!(attr(r#" text:c="3""#, "text:c").as_deref(), Some("3"));
        assert_eq!(attr(r#" data-id="1" id="2""#, "id").as_deref(), Some("2"));
    }
}
