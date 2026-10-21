//! Plain text (.txt) and Markdown (.md): encoding detection and paragraphs.

/// Windows-1252's characters for bytes 0x80-0x9F (0 where undefined).
const CP1252: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0,
    0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

/// Decode text in whatever encoding Windows tools save: UTF-8 (with or without
/// a byte-order mark), UTF-16 (with a mark, or Notepad-style without), or the
/// old Windows "ANSI" code page 1252.
pub fn decode(data: &[u8]) -> String {
    let utf16 = |bytes: &[u8], le: bool| {
        let units = bytes.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
        char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
    };
    if let Some(rest) = data.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = data.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, true);
    }
    if let Some(rest) = data.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, false);
    }
    // UTF-16 without a mark (zero bytes are valid UTF-8, so check this first):
    // English text has a zero in every other byte
    let sample = &data[..data.len().min(4096)];
    let zeros_odd = sample.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
    let zeros_even = sample.iter().step_by(2).filter(|&&b| b == 0).count();
    if sample.len() >= 4 && zeros_odd * 3 > sample.len() / 2 {
        return utf16(data, true);
    }
    if sample.len() >= 4 && zeros_even * 3 > sample.len() / 2 {
        return utf16(data, false);
    }
    if let Ok(s) = std::str::from_utf8(data) {
        return s.to_string();
    }
    data.iter()
        .map(|&b| match b {
            0x80..=0x9F if CP1252[b as usize - 0x80] != 0 => char::from_u32(CP1252[b as usize - 0x80] as u32).unwrap_or('\u{FFFD}'),
            _ => b as char,
        })
        .collect()
}

/// Paragraphs of a text file. Blank lines separate paragraphs; when they do and
/// paragraphs span several lines (hard-wrapped text such as Project Gutenberg's),
/// the lines are joined. Otherwise every line is a paragraph.
pub fn paragraphs(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let blocks: Vec<&str> = text.split("\n\n").map(str::trim).filter(|b| !b.is_empty()).collect();
    let multi_line = blocks.iter().filter(|b| b.contains('\n')).count();
    let wrapped = blocks.len() >= 2 && multi_line * 2 >= blocks.len();
    let mut out = Vec::new();
    for block in blocks {
        let lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
        if wrapped {
            let mut p = String::new();
            for line in lines {
                // a word broken across lines ("some-" / "thing") joins without a space
                if p.ends_with('-') && line.starts_with(|c: char| c.is_lowercase()) {
                    p.pop();
                } else if !p.is_empty() {
                    p.push(' ');
                }
                p.push_str(line);
            }
            out.push(p);
        } else {
            out.extend(lines.map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    out
}

/// Markdown read as text: heading marks, emphasis, list bullets, links and
/// images are reduced to their words.
pub fn markdown_paragraphs(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for line in text.replace("\r\n", "\n").lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
            lines.push(String::new());
            continue;
        }
        if in_code {
            continue; // code isn't for reading aloud
        }
        let t = t.trim_start_matches('#').trim_start_matches('>').trim_start();
        let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")).unwrap_or(t);
        if t.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | ' ')) && !t.is_empty() {
            lines.push(String::new()); // a horizontal rule or heading underline
            continue;
        }
        lines.push(strip_inline(t));
    }
    paragraphs(&lines.join("\n"))
}

/// Remove inline Markdown: ![alt](src) -> "", [text](url) -> text, *, _, `.
fn strip_inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(n) = rest.find(['[', '*', '_', '`', '!']) {
        out.push_str(&rest[..n]);
        rest = &rest[n..];
        let image = rest.starts_with("![");
        if rest.starts_with('[') || image {
            let open = usize::from(image) + 1;
            if let Some(close) = rest[open..].find("](") {
                let text = &rest[open..open + close];
                if let Some(end) = rest[open + close..].find(')') {
                    if !image {
                        out.push_str(text);
                    }
                    rest = &rest[open + close + end + 1..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        if c == '!' || c == '[' {
            out.push(c);
        } else if c == '_' && out.ends_with(|ch: char| ch.is_alphanumeric()) {
            out.push('_'); // snake_case words keep their underscores
        }
        rest = &rest[c.len_utf8()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        assert_eq!(decode(b"\xEF\xBB\xBFcaf\xC3\xA9"), "café");
        assert_eq!(decode(b"\xFF\xFEh\0i\0"), "hi");
        assert_eq!(decode(b"h\0e\0l\0l\0o\0"), "hello");
        assert_eq!(decode(b"\x93quoted\x94 caf\xE9"), "“quoted” café");
    }

    #[test]
    fn wrapped_and_unwrapped() {
        let gutenberg = "It was the best of times,\nit was the worst of times.\n\nIt was the age of wis-\ndom, it was\nthe age of foolishness.";
        assert_eq!(paragraphs(gutenberg), ["It was the best of times, it was the worst of times.", "It was the age of wisdom, it was the age of foolishness."]);
        assert_eq!(paragraphs("One line.\nAnother line.\r\nThird."), ["One line.", "Another line.", "Third."]);
    }

    #[test]
    fn markdown() {
        let md = "# Chapter One\n\nIt was *dark* and [stormy](http://x).\n![map](m.png)\n\n```\ncode()\n```\n\n- a list item";
        assert_eq!(markdown_paragraphs(md), ["Chapter One", "It was dark and stormy.", "a list item"]);
    }
}
