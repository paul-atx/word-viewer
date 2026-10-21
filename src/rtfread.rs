//! Reading RTF files (from WordPad, Word, etc.) as plain-text paragraphs.
//!
//! Only body text is kept: font and color tables, styles, document info,
//! pictures, embedded objects, headers, footers, footnotes, comments and field
//! instructions are skipped (a field's visible result is kept).

/// Destinations whose contents are never text to read.
const SKIP: [&str; 34] = [
    "fonttbl", "colortbl", "stylesheet", "info", "pict", "object", "objdata", "header", "headerl", "headerr", "headerf",
    "footer", "footerl", "footerr", "footerf", "footnote", "annotation", "atnid", "atnauthor", "fldinst", "themedata",
    "colorschememapping", "latentstyles", "datastore", "xmlnstbl", "listtable", "listoverridetable", "rsidtbl",
    "generator", "pgdsctbl", "mmathPr", "filetbl", "revtbl", "shpinst",
];

/// Deepest group nesting followed; deeper content is ignored.
const MAX_DEPTH: usize = 512;

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    /// Fallback characters to drop after a \uN (set by \ucN).
    uc: u32,
}

pub fn paragraphs(rtf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut stack = vec![Group { skip: false, uc: 1 }];
    let mut pending_skip = 0u32; // fallback chars still to drop after \uN
    let mut star = false; // the group began with \* (ignorable destination)
    let mut at_group_start = false;
    let mut i = 0;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let p = cur.split_whitespace().collect::<Vec<_>>().join(" ");
        if !p.is_empty() {
            out.push(p);
        }
        cur.clear();
    };
    let emit = |cur: &mut String, stack: &[Group], pending: &mut u32, c: char| {
        if *pending > 0 {
            *pending -= 1;
        } else if !stack.last().is_some_and(|g| g.skip) {
            cur.push(c);
        }
    };
    while i < rtf.len() {
        let b = rtf[i];
        i += 1;
        match b {
            b'{' => {
                let top = *stack.last().unwrap_or(&Group { skip: false, uc: 1 });
                if stack.len() < MAX_DEPTH {
                    stack.push(top);
                } else {
                    stack.push(Group { skip: true, uc: top.uc });
                }
                at_group_start = true;
                star = false;
                pending_skip = 0;
                continue;
            }
            b'}' => {
                if stack.len() > 1 {
                    stack.pop();
                }
                at_group_start = false;
                pending_skip = 0;
                continue;
            }
            b'\r' | b'\n' => continue,
            b'\\' => {
                let Some(&n) = rtf.get(i) else { break };
                if !n.is_ascii_alphabetic() {
                    i += 1;
                    match n {
                        b'*' => {
                            star = true;
                            continue; // keeps at_group_start for the destination word
                        }
                        b'\'' => {
                            let hex = rtf.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                            i += 2;
                            if let Some(byte) = hex {
                                let c = crate::plain::decode(&[byte]).chars().next().unwrap_or('\u{FFFD}');
                                emit(&mut cur, &stack, &mut pending_skip, c);
                            }
                        }
                        b'~' => emit(&mut cur, &stack, &mut pending_skip, ' '),
                        b'_' => emit(&mut cur, &stack, &mut pending_skip, '-'),
                        b'-' => {}
                        b'\r' | b'\n' => {
                            if !stack.last().is_some_and(|g| g.skip) {
                                flush(&mut cur, &mut out);
                            }
                        }
                        other => emit(&mut cur, &stack, &mut pending_skip, other as char),
                    }
                    at_group_start = false;
                    continue;
                }
                let start = i;
                while i < rtf.len() && rtf[i].is_ascii_alphabetic() && i - start < 32 {
                    i += 1;
                }
                let word = std::str::from_utf8(&rtf[start..i]).unwrap_or("");
                let num_start = i;
                if i < rtf.len() && rtf[i] == b'-' {
                    i += 1;
                }
                while i < rtf.len() && rtf[i].is_ascii_digit() && i - num_start < 10 {
                    i += 1;
                }
                let param: Option<i64> = std::str::from_utf8(&rtf[num_start..i]).ok().and_then(|s| s.parse().ok());
                if i < rtf.len() && rtf[i] == b' ' {
                    i += 1;
                }
                let first = at_group_start;
                at_group_start = false;
                let top = stack.len() - 1;
                // a destination we don't read, or any unknown \* destination
                if first && (SKIP.contains(&word) || star) && word != "fldrslt" {
                    stack[top].skip = true;
                    continue;
                }
                star = false;
                match word {
                    "par" | "sect" | "page" | "row" => {
                        if !stack[top].skip {
                            flush(&mut cur, &mut out);
                        }
                    }
                    "line" | "tab" | "cell" => emit(&mut cur, &stack, &mut pending_skip, ' '),
                    "emdash" => emit(&mut cur, &stack, &mut pending_skip, '—'),
                    "endash" => emit(&mut cur, &stack, &mut pending_skip, '–'),
                    "bullet" => emit(&mut cur, &stack, &mut pending_skip, '•'),
                    "lquote" => emit(&mut cur, &stack, &mut pending_skip, '‘'),
                    "rquote" => emit(&mut cur, &stack, &mut pending_skip, '’'),
                    "ldblquote" => emit(&mut cur, &stack, &mut pending_skip, '“'),
                    "rdblquote" => emit(&mut cur, &stack, &mut pending_skip, '”'),
                    "uc" => stack[top].uc = param.unwrap_or(1).clamp(0, 8) as u32,
                    "u" => {
                        let v = param.unwrap_or(0xFFFD);
                        let v = if v < 0 { v + 65536 } else { v } as u32;
                        let c = char::from_u32(v).unwrap_or('\u{FFFD}');
                        pending_skip = 0;
                        emit(&mut cur, &stack, &mut pending_skip, c);
                        pending_skip = stack[top].uc;
                    }
                    "bin" => {
                        // raw binary data follows; skip it
                        i = i.saturating_add(param.unwrap_or(0).max(0) as usize).min(rtf.len());
                    }
                    _ => {}
                }
            }
            _ => {
                at_group_start = false;
                if b < 0x80 {
                    emit(&mut cur, &stack, &mut pending_skip, b as char);
                } else {
                    // raw 8-bit text: read as Windows-1252
                    let c = crate::plain::decode(&[b]).chars().next().unwrap_or('\u{FFFD}');
                    emit(&mut cur, &stack, &mut pending_skip, c);
                }
            }
        }
    }
    flush(&mut cur, &mut out);
    // UTF-16 surrogate pairs written as two \u words
    out.into_iter().map(|p| fix_surrogates(&p)).collect()
}

/// `\u` words for characters outside the BMP arrive as two U+FFFD (lone
/// surrogates can't be chars); nothing to recover, so collapse each pair to one.
fn fix_surrogates(p: &str) -> String {
    p.replace("\u{FFFD}\u{FFFD}", "\u{FFFD}")
}

#[cfg(test)]
mod tests {
    use super::paragraphs;

    #[test]
    fn wordpad_style() {
        let rtf = br#"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fnil Calibri;}}{\colortbl ;\red255\green0\blue0;}
{\*\generator Riched20 10.0.19041}\viewkind4\uc1\pard\sa200\sl276\slmult1\f0\fs22\lang9 It was a {\b dark} night.\par
Caf\'e9 \u8212? \ldblquote quoted\rdblquote\par
{\field{\*\fldinst HYPERLINK "http://x"}{\fldrslt link text}}\par
{\pict\wmetafile8\picw10 0102ff}{\header Page header}\par}"#;
        assert_eq!(paragraphs(rtf), ["It was a dark night.", "Café — “quoted”", "link text"]);
    }

    #[test]
    fn unicode_skip_count() {
        assert_eq!(paragraphs(br"{\rtf1\uc2 a\u233 xyb\par}"), ["aéb"]);
    }
}
