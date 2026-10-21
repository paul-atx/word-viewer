//! EPUB e-books: chapters in reading (spine) order, as paragraphs.

use crate::markup::{attr, events, html_paragraphs, Ev};
use crate::zip;

/// Most chapter files read from one book, and their total size.
const MAX_ITEMS: usize = 2000;
const MAX_TOTAL: usize = 64 << 20;

/// Turn an href relative to `base` into a path inside the zip.
fn resolve(base: &str, href: &str) -> String {
    let href = href.split(['#', '?']).next().unwrap_or("");
    let decoded = percent_decode(href);
    let mut parts: Vec<&str> = if decoded.starts_with('/') { Vec::new() } else { base.split('/').filter(|p| !p.is_empty()).collect() };
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The book's text, or None if this isn't a readable EPUB.
pub fn paragraphs(z: &[u8]) -> Option<Vec<String>> {
    let text = |path: &str, max: usize| zip::extract(z, path, max).map(|b| String::from_utf8_lossy(&b).into_owned());
    // META-INF/container.xml names the package file
    let container = text("META-INF/container.xml", 1 << 20)?;
    let mut opf_path = None;
    events(&container, |ev| {
        if let Ev::Start(name, attrs, _) = ev {
            if name.ends_with("rootfile") && opf_path.is_none() {
                opf_path = attr(attrs, "full-path");
            }
        }
    });
    let opf_path = opf_path?;
    let opf = text(&opf_path, 8 << 20)?;
    let base = opf_path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();

    // manifest: id -> href; spine: ids in reading order
    let mut manifest: Vec<(String, String)> = Vec::new();
    let mut spine: Vec<String> = Vec::new();
    events(&opf, |ev| {
        if let Ev::Start(name, attrs, _) = ev {
            let local = name.rsplit(':').next().unwrap_or(&name);
            if local == "item" {
                if let (Some(id), Some(href)) = (attr(attrs, "id"), attr(attrs, "href")) {
                    manifest.push((id, href));
                }
            } else if local == "itemref" && attr(attrs, "linear").as_deref() != Some("no") {
                if let Some(id) = attr(attrs, "idref") {
                    spine.push(id);
                }
            }
        }
    });

    let mut out = Vec::new();
    let mut total = 0;
    for id in spine.iter().take(MAX_ITEMS) {
        let Some((_, href)) = manifest.iter().find(|(m, _)| m == id) else { continue };
        let Some(page) = text(&resolve(&base, href), MAX_TOTAL - total) else { continue };
        total += page.len();
        out.extend(html_paragraphs(&page));
        if total >= MAX_TOTAL {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn paths() {
        assert_eq!(resolve("OEBPS", "Text/ch%201.xhtml#top"), "OEBPS/Text/ch 1.xhtml");
        assert_eq!(resolve("OEBPS/Text", "../Images/a.xhtml"), "OEBPS/Images/a.xhtml");
        assert_eq!(resolve("", "chapter.html"), "chapter.html");
        assert_eq!(resolve("a", "../../../../x"), "x");
    }
}
