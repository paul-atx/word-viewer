//! EPUB e-books: chapters in reading (spine) order, as paragraphs.

use crate::markup::{attr, events, html_blocks, Block, Ev};
use std::ops::Range;
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

/// Kinds of file that come before or after the story: cover, title page,
/// copyright, contents, notes, index, ads. Matched against the words of a file's
/// name ("Weir_epub3_cop_r1", "FM_halftitle", "BM_endNotes") and the kinds the
/// book's guide and landmarks give it. An introduction or preface isn't one.
const MATTER: [&str; 50] = [
    "cover", "cvi", "cov", "titlepage", "halftitlepage", "halftitle", "htp", "tp", "title", "imprint", "cop", "copyright",
    "uncopyright", "ded", "dedication", "toc", "contents", "nav", "fm", "frontmatter", "praise", "adc", "ad", "ads", "alsoby",
    "bm", "backmatter", "ack", "acknowledgments", "acknowledgements", "ata", "about", "abouttheauthor", "author", "index",
    "bibliography", "endnotes", "notes", "glossary", "colophon", "credits", "newsletter", "excerpt", "teaser",
    "copyrightpage", "landmarks", "loi", "lot", "rearnotes", "footnotes",
];

/// Whether a file is front or back matter, by its name or its kinds.
fn is_matter(path: &str, kinds: &[&str]) -> bool {
    let stem = path.rsplit('/').next().unwrap_or(path);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s).to_ascii_lowercase();
    let joined: String = stem.chars().filter(char::is_ascii_alphanumeric).collect();
    let words = stem.split(|c: char| !c.is_ascii_alphanumeric()).map(|w| w.trim_end_matches(|c: char| c.is_ascii_digit()));
    let named = words.chain([joined.as_str()]).any(|w| MATTER.contains(&w));
    named || kinds.iter().any(|k| MATTER.contains(&k.replace('-', "").as_str()))
}

/// Whether a file is the author's own way into the book, read even when the
/// book marks its text as starting later.
fn is_opening(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    ["intro", "preface", "foreword", "prologue", "prelude"].iter().any(|w| name.contains(w))
}

/// The paragraphs of the story itself: after the files at the start that are
/// front matter (or where the book says its text begins, if later, but never
/// past an introduction or preface) and before the files at the end that are
/// back matter. `files` is each part's path and kinds; `hint` the first part the
/// book marks as its text.
fn body(files: &[(String, Vec<&str>)], parts: &[usize], total: usize, hint: Option<usize>) -> Range<usize> {
    let mut start = files.iter().take_while(|(p, k)| is_matter(p, k)).count();
    while start < hint.unwrap_or(0) && !is_opening(&files[start].0) {
        start += 1;
    }
    let trail = files.iter().rev().take_while(|(p, k)| is_matter(p, k)).count();
    let end = files.len() - trail;
    // skip at most the first half; never everything
    if start >= end || start > files.len() / 2 {
        return 0..total;
    }
    parts.get(start).copied().unwrap_or(0)..parts.get(end).copied().unwrap_or(total)
}

/// The book's text, or None if this isn't a readable EPUB: its paragraphs (each
/// marked if it's a heading), where each of its files (usually a chapter) starts,
/// and which paragraphs are the story rather than front and back matter.
pub fn blocks(z: &[u8]) -> Option<(Vec<Block>, Vec<usize>, Range<usize>)> {
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

    // manifest: id -> href; spine: ids in reading order; the guide's kinds of file
    let mut manifest: Vec<(String, String)> = Vec::new();
    let mut spine: Vec<String> = Vec::new();
    let mut kinds: Vec<(String, String)> = Vec::new(); // (path, kind)
    let mut nav = None;
    events(&opf, |ev| {
        if let Ev::Start(name, attrs, _) = ev {
            let local = name.rsplit(':').next().unwrap_or(&name);
            if local == "item" {
                if let (Some(id), Some(href)) = (attr(attrs, "id"), attr(attrs, "href")) {
                    if attr(attrs, "properties").is_some_and(|p| p.split_whitespace().any(|p| p == "nav")) {
                        nav = Some(resolve(&base, &href));
                    }
                    manifest.push((id, href));
                }
            } else if local == "reference" {
                if let (Some(kind), Some(href)) = (attr(attrs, "type"), attr(attrs, "href")) {
                    kinds.push((resolve(&base, &href), kind.to_ascii_lowercase()));
                }
            } else if local == "itemref" && attr(attrs, "linear").as_deref() != Some("no") {
                if let Some(id) = attr(attrs, "idref") {
                    spine.push(id);
                }
            }
        }
    });

    // EPUB 3 landmarks (in the navigation document) say the same as the guide
    if let Some(nav_path) = nav {
        if let Some(doc) = text(&nav_path, 4 << 20) {
            let dir = nav_path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            let mut in_landmarks = false;
            events(&doc, |ev| match ev {
                Ev::Start(name, attrs, _) if name.ends_with("nav") => {
                    in_landmarks = attr(attrs, "epub:type").is_some_and(|t| t.contains("landmarks"));
                }
                Ev::End(name) if name.ends_with("nav") => in_landmarks = false,
                Ev::Start(name, attrs, _) if in_landmarks && name == "a" => {
                    if let (Some(kind), Some(href)) = (attr(attrs, "epub:type"), attr(attrs, "href")) {
                        kinds.push((resolve(&dir, &href), kind.to_ascii_lowercase()));
                    }
                }
                _ => {}
            });
        }
    }

    let mut out = Vec::new();
    let mut parts = Vec::new();
    let mut files: Vec<(String, Vec<&str>)> = Vec::new();
    let mut hint = None;
    let mut total = 0;
    for id in spine.iter().take(MAX_ITEMS) {
        let Some((_, href)) = manifest.iter().find(|(m, _)| m == id) else { continue };
        let path = resolve(&base, href);
        let Some(page) = text(&path, MAX_TOTAL - total) else { continue };
        total += page.len();
        let page = html_blocks(&page);
        if !page.is_empty() {
            let here: Vec<&str> = kinds.iter().filter(|(p, _)| *p == path).map(|(_, k)| k.as_str()).collect();
            if hint.is_none() && here.iter().any(|k| matches!(*k, "text" | "bodymatter")) {
                hint = Some(parts.len());
            }
            parts.push(out.len());
            files.push((path, here));
        }
        out.extend(page);
        if total >= MAX_TOTAL {
            break;
        }
    }
    let body = body(&files, &parts, out.len(), hint);
    (!out.is_empty()).then_some((out, parts, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_and_back_matter() {
        let f = |names: &[&str]| -> Vec<(String, Vec<&'static str>)> { names.iter().map(|n| (n.to_string(), Vec::new())).collect() };
        // The Martian: cover, title, copyright, dedication, contents, then chapters
        let m = f(&["titlepage.xhtml", "W_epub3_nav_r1.xhtml", "W_epub3_cvi_r1.xhtml", "W_epub3_tp_r1.xhtml", "W_epub3_cop_r1.xhtml",
            "W_epub3_ded_r1.xhtml", "W_epub3_fm_r1.xhtml", "W_epub3_toc_r1.xhtml", "W_epub3_c001_r1.xhtml", "W_epub3_c002_r1.xhtml",
            "W_epub3_c003_r1.xhtml", "W_epub3_c004_r1.xhtml", "W_epub3_c005_r1.xhtml", "W_epub3_c006_r1.xhtml", "W_epub3_c007_r1.xhtml",
            "W_epub3_c008_r1.xhtml", "W_epub3_ack_r1.xhtml", "W_epub3_ata_r1.xhtml"]);
        let parts: Vec<usize> = (0..m.len()).map(|i| i * 10).collect();
        assert_eq!(body(&m, &parts, 180, Some(5)), 80..160);
        // Standard Ebooks: an introduction is part of the book, even though its
        // landmarks say the text starts at chapter 1
        let s = f(&["titlepage.xhtml", "imprint.xhtml", "introduction.xhtml", "chapter-1.xhtml", "chapter-2.xhtml", "colophon.xhtml", "uncopyright.xhtml"]);
        assert_eq!(body(&s, &[0, 2, 4, 30, 60, 90, 95], 99, Some(3)), 4..90);
        // unnamed files: the book's own marker skips the front matter
        let c = f(&["part0000.html", "part0001.html", "part0002.html", "part0003.html"]);
        assert_eq!(body(&c, &[0, 3, 9, 40], 80, Some(2)), 9..80);
        // kinds from the guide; a book with no front matter
        let g = vec![("a.xml".to_string(), vec!["cover"]), ("b.xml".to_string(), vec!["text"]), ("c.xml".to_string(), vec![])];
        assert_eq!(body(&g, &[0, 1, 50], 99, None), 1..99);
        assert_eq!(body(&f(&["ch1.xhtml", "ch2.xhtml"]), &[0, 40], 80, None), 0..80);
        // never everything, and at most the first half skipped
        assert_eq!(body(&f(&["cover.xhtml", "toc.xhtml"]), &[0, 1], 9, None), 0..9);
        assert_eq!(body(&f(&["cover.xhtml", "toc.xhtml", "copyright.xhtml", "ch1.xhtml"]), &[0, 1, 2, 3], 9, None), 0..9);
    }

    #[test]
    fn paths() {
        assert_eq!(resolve("OEBPS", "Text/ch%201.xhtml#top"), "OEBPS/Text/ch 1.xhtml");
        assert_eq!(resolve("OEBPS/Text", "../Images/a.xhtml"), "OEBPS/Images/a.xhtml");
        assert_eq!(resolve("", "chapter.html"), "chapter.html");
        assert_eq!(resolve("a", "../../../../x"), "x");
    }
}
