//! word/document.xml -> RTF.

use crate::rtf::Rtf;

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| from + i)
}

/// Value of attribute `key` (e.g. `w:val`) within a single tag.
fn attr<'a>(tag: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut k = key.to_vec();
    k.extend_from_slice(b"=\"");
    let start = find(tag, 0, &k)? + k.len();
    let len = tag[start..].iter().position(|&c| c == b'"')?;
    Some(&tag[start..start + len])
}

/// A toggle like <w:b/> is on unless w:val says otherwise.
fn on(tag: &[u8]) -> bool {
    match attr(tag, b"w:val") {
        Some(v) => !(v.starts_with(b"0") || v == b"false" || v == b"none"),
        None => true,
    }
}

fn text(r: &mut Rtf, raw: &[u8]) {
    let s = String::from_utf8_lossy(raw);
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        if c == '&' {
            if let Some(semi) = s.as_bytes()[i..].iter().take(12).position(|&b| b == b';') {
                let ent = &s[i + 1..i + semi];
                let v = match ent {
                    "amp" => Some('&' as u32),
                    "lt" => Some('<' as u32),
                    "gt" => Some('>' as u32),
                    "quot" => Some('"' as u32),
                    "apos" => Some('\'' as u32),
                    _ => match ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => ent.strip_prefix('#').and_then(|d| d.parse().ok()),
                    },
                };
                if let Some(v) = v {
                    r.ch(v);
                    for _ in 0..semi {
                        it.next();
                    }
                    continue;
                }
            }
        }
        r.ch(c as u32);
    }
}

pub fn render(x: &[u8]) -> String {
    const HEAD: [&str; 5] = ["", "\\b\\fs40 ", "\\b\\fs32 ", "\\b\\fs28 ", "\\b\\fs52 "];
    let mut r = Rtf::new();
    let (mut bold, mut ital, mut under, mut head) = (false, false, false, 0);
    let (mut bullet, mut intbl, mut skip, mut inppr, mut cell) = (false, false, false, false, 0);
    let mut p = 0;
    while !r.full {
        let Some(lt) = find(x, p, b"<") else { break };
        let Some(gt) = find(x, lt, b">") else { break };
        let tag = &x[lt + 1..gt];
        let close = tag.first() == Some(&b'/');
        let selfclose = tag.last() == Some(&b'/');
        let body = if close { &tag[1..] } else { tag };
        let nl = body.iter().position(|&c| c == b'/' || c.is_ascii_whitespace()).unwrap_or(body.len());
        let name = &body[..nl];
        p = gt + 1;
        match name {
            b"mc:Fallback" => skip = !close, // duplicate of the mc:Choice content
            _ if skip => {}
            b"w:p" if close => r.put(if intbl { "  " } else { "\\par\n" }),
            b"w:p" => {
                (bold, ital, under, head, bullet) = (false, false, false, 0, false);
                if !intbl {
                    r.put("\\pard\\sa160 ");
                }
            }
            b"w:pStyle" if !close => {
                if let Some(v) = attr(tag, b"w:val") {
                    if v.starts_with(b"Title") {
                        head = 4;
                    } else if let Some(rest) = v.strip_prefix(b"Heading") {
                        head = match rest.first() {
                            Some(&d @ b'1'..=b'3') => (d - b'0') as usize,
                            _ => 3,
                        };
                    }
                }
            }
            b"w:numPr" if !close => bullet = true,
            b"w:pPr" => {
                inppr = !close && !selfclose; // its <w:tab> children are tab stops, not tabs
                if close && bullet {
                    r.put("\\li360\\fi-240 \\bullet\\tab ");
                }
            }
            b"w:ind" if !close => {
                if let Some(v) = attr(tag, b"w:firstLine") {
                    let fi: i64 = std::str::from_utf8(v).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                    r.put(&format!("\\fi{} ", fi.clamp(-14400, 14400)));
                }
            }
            b"w:r" if !close => (bold, ital, under) = (false, false, false),
            b"w:b" if !close => bold = on(tag),
            b"w:i" if !close => ital = on(tag),
            b"w:u" if !close => under = on(tag),
            b"w:tab" if !close && !inppr => r.put("\\tab "),
            b"w:br" | b"w:cr" if !close => r.put("\\line "),
            b"w:tbl" => {
                intbl = !close;
                if close {
                    r.put("\\par\n");
                }
            }
            b"w:tr" if close => r.put("\\par\n"),
            b"w:tr" => {
                r.put("\\pard\\sa60 ");
                cell = 0;
            }
            b"w:tc" if !close => {
                if cell > 0 {
                    r.put("\\tab\\u9474?  ");
                }
                cell += 1;
            }
            b"w:t" if !close && !selfclose => {
                let Some(te) = find(x, p, b"</w:t>") else { break };
                r.put("{");
                r.put(HEAD[head]);
                if bold {
                    r.put("\\b ");
                }
                if ital {
                    r.put("\\i ");
                }
                if under {
                    r.put("\\ul ");
                }
                text(&mut r, &x[p..te]);
                r.put("}");
                p = te + 6;
            }
            _ => {}
        }
    }
    r.finish()
}
