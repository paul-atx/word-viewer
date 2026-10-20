//! Plain-text paragraphs from the RTF this crate renders.
//!
//! The renderers only ever emit a small, known RTF subset (groups, a font table,
//! \par, \line, \tab, \bullet, \uN? escapes, formatting words), so this reads
//! exactly that.

/// Split rendered RTF into paragraphs of plain text, dropping empty ones.
pub fn paragraphs(rtf: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut high: Option<u16> = None; // pending UTF-16 high surrogate
    let mut it = rtf.chars().peekable();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let p = cur.split_whitespace().collect::<Vec<_>>().join(" ");
        if !p.is_empty() {
            out.push(p);
        }
        cur.clear();
    };
    while let Some(c) = it.next() {
        match c {
            '{' if it.clone().take(8).collect::<String>() == "\\fonttbl" => {
                let mut depth = 1;
                for c in it.by_ref() {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                }
            }
            '{' | '}' | '\n' | '\r' => {}
            '\\' => {
                let Some(&n) = it.peek() else { break };
                if !n.is_ascii_alphabetic() {
                    it.next();
                    cur.push(n); // \\ \{ \}
                    continue;
                }
                let mut word = String::new();
                while let Some(a) = it.next_if(|a| a.is_ascii_alphabetic()) {
                    word.push(a);
                }
                let mut num = String::new();
                while let Some(d) = it.next_if(|d| d.is_ascii_digit() || *d == '-' && num.is_empty()) {
                    num.push(d);
                }
                it.next_if_eq(&' '); // a control word's delimiting space
                match word.as_str() {
                    "par" => flush(&mut cur, &mut out),
                    "line" | "tab" => cur.push(' '),
                    "u" => {
                        it.next_if_eq(&'?'); // fallback character
                        let u = num.parse::<i32>().unwrap_or(0xFFFD) as i16 as u16;
                        match (high.take(), u) {
                            (_, 0xD800..=0xDBFF) => high = Some(u),
                            (Some(h), 0xDC00..=0xDFFF) => {
                                let cp = 0x10000 + ((h as u32 - 0xD800) << 10) + (u as u32 - 0xDC00);
                                cur.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                            (_, 0x2502) => cur.push(' '), // table cell divider
                            _ => cur.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}')),
                        }
                    }
                    _ => {} // \rtf1, \b, \fs22, \pard, \bullet, ...
                }
            }
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn subset() {
        let rtf = "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Segoe UI;}}\\fs22\n\\pard\\sa160 {\\b\\fs52 Title}\\par\n\
                   \\pard {caf\\u233? \\\\ \\{x\\} a\\tab b\\line c \\u-10179?\\u-8704?}\\par\n\\pard\\sa60 A\\tab\\u9474?  B\\par\n}";
        assert_eq!(super::paragraphs(rtf), ["Title", "café \\ {x} a b c \u{1F600}", "A B"]);
    }
}
