//! Text from PDF files, rebuilt into paragraphs.
//!
//! A PDF page is glyphs placed at positions, not paragraphs. This reads the
//! page content streams, maps each font's glyph codes back to Unicode, and
//! rebuilds lines and paragraphs from where the glyphs sit: joining words
//! hyphenated across lines and dropping page numbers and running headers.
//!
//! Hostile-input limits: objects are found by scanning (no trust in the xref
//! table), nesting and object counts are capped, all decompression shares one
//! budget, and every loop is bounded by the input or by those caps.

use crate::inflate::inflate_partial;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

/// Why a PDF couldn't be read.
#[derive(Debug, PartialEq)]
pub enum PdfError {
    /// Not a PDF we can parse.
    Unreadable,
    /// Encrypted (password-protected or with restrictions).
    Encrypted,
    /// Parsed, but no text: most likely scanned pages (images).
    NoText,
}

const MAX_DEPTH: usize = 48;
const MAX_OBJECTS: usize = 500_000;
const MAX_PAGES: usize = 20_000;
const MAX_ARRAY: usize = 200_000;
/// All decompressed data in one document, together.
const DECODE_BUDGET: usize = 256 << 20;
/// Operations interpreted per page (content streams can be huge).
const MAX_OPS: usize = 4_000_000;

// ---- objects ----

#[derive(Clone, Debug)]
enum Obj {
    Null,
    #[allow(dead_code)] // parsed for completeness; no reader needs the value
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
    Name(String),
    Arr(Vec<Obj>),
    Dict(Dict),
    Ref(u32),
    /// A stream: its dictionary and its data's byte range in the file.
    Stream(Dict, usize, usize),
}

type Dict = Vec<(String, Obj)>;

fn get<'a>(d: &'a Dict, key: &str) -> Option<&'a Obj> {
    d.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

impl Obj {
    fn num(&self) -> Option<f64> {
        match self {
            Obj::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }
    fn name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
    fn dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) | Obj::Stream(d, _, _) => Some(d),
            _ => None,
        }
    }
}

// ---- lexer ----

enum Tok {
    Obj(Obj),
    /// A keyword: content-stream operator, `obj`, `stream`, `R`...
    Op(String),
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0C' | b'\0')
}

fn is_delim(c: u8) -> bool {
    is_ws(c) || matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lex<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lex<'a> {
    fn new(b: &'a [u8], i: usize) -> Lex<'a> {
        Lex { b, i }
    }

    fn ws(&mut self) {
        while self.i < self.b.len() {
            if is_ws(self.b[self.i]) {
                self.i += 1;
            } else if self.b[self.i] == b'%' {
                while self.i < self.b.len() && !matches!(self.b[self.i], b'\n' | b'\r') {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn word(&mut self) -> &'a [u8] {
        let s = self.i;
        while self.i < self.b.len() && !is_delim(self.b[self.i]) {
            self.i += 1;
        }
        &self.b[s..self.i]
    }

    /// The next token. Arrays and dictionaries come back whole; `N G R` is a reference.
    fn next(&mut self, depth: usize) -> Option<Tok> {
        self.ws();
        let c = *self.b.get(self.i)?;
        let tok = match c {
            b'/' => {
                self.i += 1;
                let raw = self.word();
                let mut name = Vec::with_capacity(raw.len());
                let mut k = 0;
                while k < raw.len() {
                    if raw[k] == b'#' {
                        if let Some(v) = raw.get(k + 1..k + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                            name.push(v);
                            k += 3;
                            continue;
                        }
                    }
                    name.push(raw[k]);
                    k += 1;
                }
                Tok::Obj(Obj::Name(String::from_utf8_lossy(&name).into_owned()))
            }
            b'(' => Tok::Obj(Obj::Str(self.literal())),
            b'<' if self.b.get(self.i + 1) == Some(&b'<') => {
                self.i += 2;
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut d = Dict::new();
                loop {
                    self.ws();
                    if self.b.get(self.i..self.i + 2) == Some(b">>") {
                        self.i += 2;
                        break;
                    }
                    let Some(Tok::Obj(Obj::Name(k))) = self.next(depth + 1) else { return None };
                    let v = self.value(depth + 1)?;
                    if d.len() < 4096 {
                        d.push((k, v));
                    }
                }
                Tok::Obj(Obj::Dict(d))
            }
            b'<' => {
                self.i += 1;
                let mut out = Vec::new();
                let mut hi: Option<u8> = None;
                while let Some(&h) = self.b.get(self.i) {
                    self.i += 1;
                    if h == b'>' {
                        break;
                    }
                    let Some(v) = (h as char).to_digit(16) else { continue };
                    match hi.take() {
                        Some(x) => out.push(x << 4 | v as u8),
                        None => hi = Some(v as u8),
                    }
                }
                if let Some(x) = hi {
                    out.push(x << 4);
                }
                Tok::Obj(Obj::Str(out))
            }
            b'[' => {
                self.i += 1;
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut a = Vec::new();
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        None => break,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => {}
                    }
                    let start = self.i;
                    match self.value(depth + 1) {
                        Some(v) if a.len() < MAX_ARRAY => a.push(v),
                        Some(_) => {}
                        None => {
                            if self.i == start {
                                self.i += 1; // stray byte: skip it
                            }
                        }
                    }
                }
                Tok::Obj(Obj::Arr(a))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let w = self.word();
                let s = std::str::from_utf8(w).unwrap_or("0");
                Tok::Obj(Obj::Num(s.parse().unwrap_or_else(|_| s.trim_start_matches(['+', '-']).parse().unwrap_or(0.0))))
            }
            b')' | b'>' | b']' | b'}' | b'{' => {
                self.i += 1;
                Tok::Op((c as char).to_string())
            }
            _ => {
                let w = self.word();
                if w.is_empty() {
                    self.i += 1;
                    return Some(Tok::Op(String::new()));
                }
                match w {
                    b"true" => Tok::Obj(Obj::Bool(true)),
                    b"false" => Tok::Obj(Obj::Bool(false)),
                    b"null" => Tok::Obj(Obj::Null),
                    _ => Tok::Op(String::from_utf8_lossy(&w[..w.len().min(32)]).into_owned()),
                }
            }
        };
        Some(tok)
    }

    /// An object value, folding `N G R` into a reference. The lookahead for
    /// `G R` reads only digits and the letter R: peeking with full parsing would
    /// re-parse nested arrays at every level (exponential time).
    fn value(&mut self, depth: usize) -> Option<Obj> {
        match self.next(depth)? {
            Tok::Obj(Obj::Num(n)) => {
                if n >= 0.0 && n.fract() == 0.0 && n <= u32::MAX as f64 {
                    let save = self.i;
                    self.ws();
                    let g = self.i;
                    while self.b.get(self.i).is_some_and(u8::is_ascii_digit) {
                        self.i += 1;
                    }
                    if self.i > g && self.b.get(self.i).is_none_or(|&c| is_delim(c)) {
                        self.ws();
                        if self.b.get(self.i) == Some(&b'R') && self.b.get(self.i + 1).is_none_or(|&c| is_delim(c)) {
                            self.i += 1;
                            return Some(Obj::Ref(n as u32));
                        }
                    }
                    self.i = save;
                }
                Some(Obj::Num(n))
            }
            Tok::Obj(o) => Some(o),
            Tok::Op(_) => None,
        }
    }

    /// A literal string `( ... )` with escapes and balanced parentheses.
    fn literal(&mut self) -> Vec<u8> {
        self.i += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(&c) = self.b.get(self.i) {
            self.i += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else { break };
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.b.get(self.i) == Some(&b'\n') {
                                self.i += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.b.get(self.i) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }
}

// ---- the document ----

struct Doc<'a> {
    data: &'a [u8],
    objs: BTreeMap<u32, Obj>,
    trailers: Vec<Dict>,
    budget: Cell<usize>,
}

impl<'a> Doc<'a> {
    fn parse(data: &'a [u8]) -> Option<Doc<'a>> {
        if !data[..data.len().min(1024)].windows(5).any(|w| w == b"%PDF-") {
            return None;
        }
        let mut doc = Doc { data, objs: BTreeMap::new(), trailers: Vec::new(), budget: Cell::new(DECODE_BUDGET) };
        doc.scan();
        doc.load_object_streams();
        Some(doc)
    }

    /// Find every `N G obj` in the file; later definitions replace earlier ones,
    /// as incremental updates do.
    fn scan(&mut self) {
        let d = self.data;
        let mut i = 0;
        while i + 3 <= d.len() && self.objs.len() < MAX_OBJECTS {
            let Some(n) = find(&d[i..], b"obj").map(|n| i + n) else { break };
            i = n + 3;
            if d.get(n + 3).is_some_and(|&c| !is_delim(c)) {
                continue;
            }
            // trailing "N G " before the keyword
            let mut k = n;
            while k > 0 && is_ws(d[k - 1]) {
                k -= 1;
            }
            let g_end = k;
            while k > 0 && d[k - 1].is_ascii_digit() {
                k -= 1;
            }
            if k == g_end {
                continue;
            }
            while k > 0 && is_ws(d[k - 1]) {
                k -= 1;
            }
            let n_end = k;
            while k > 0 && d[k - 1].is_ascii_digit() {
                k -= 1;
            }
            if k == n_end || (k > 0 && !is_delim(d[k - 1])) {
                continue;
            }
            let Some(num) = std::str::from_utf8(&d[k..n_end]).ok().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let mut lex = Lex::new(d, n + 3);
            let Some(obj) = lex.value(0) else { continue };
            lex.ws();
            let obj = match obj {
                Obj::Dict(dict) if d.get(lex.i..lex.i + 6) == Some(b"stream") => {
                    let mut s = lex.i + 6;
                    if d.get(s) == Some(&b'\r') {
                        s += 1;
                    }
                    if d.get(s) == Some(&b'\n') {
                        s += 1;
                    }
                    let declared = get(&dict, "Length").and_then(Obj::num).map(|l| l as usize);
                    let end = match declared {
                        Some(len) if s.checked_add(len).is_some_and(|e| {
                            e <= d.len() && {
                                let mut t = Lex::new(d, e);
                                t.ws();
                                d.get(t.i..t.i + 9) == Some(b"endstream")
                            }
                        }) => s + len,
                        _ => find(&d[s.min(d.len())..], b"endstream").map_or(d.len(), |e| s + e),
                    };
                    i = end;
                    Obj::Stream(dict, s, end)
                }
                o => o,
            };
            self.objs.insert(num, obj);
        }
        // trailer dictionaries (classic xref tables)
        let mut i = 0;
        while let Some(t) = find(&d[i..], b"trailer").map(|t| i + t) {
            i = t + 7;
            if let Some(Obj::Dict(dict)) = Lex::new(d, i).value(0) {
                self.trailers.push(dict);
            }
        }
        // cross-reference streams carry the trailer keys themselves
        let xref: Vec<Dict> = self.objs.values().filter_map(|o| match o {
            Obj::Stream(dict, _, _) if get(dict, "Type").and_then(Obj::name) == Some("XRef") => Some(dict.clone()),
            _ => None,
        }).collect();
        self.trailers.extend(xref);
    }

    /// Objects stored compressed inside object streams (PDF 1.5+).
    fn load_object_streams(&mut self) {
        let streams: Vec<Obj> = self.objs.values().filter(|o| matches!(o, Obj::Stream(d, _, _) if get(d, "Type").and_then(Obj::name) == Some("ObjStm"))).cloned().collect();
        for s in streams {
            let Some(dict) = s.dict() else { continue };
            let n = get(dict, "N").and_then(Obj::num).unwrap_or(0.0).clamp(0.0, 100_000.0) as usize;
            let first = get(dict, "First").and_then(Obj::num).unwrap_or(0.0).max(0.0) as usize;
            let Some(data) = self.stream(&s) else { continue };
            let mut lex = Lex::new(&data, 0);
            let mut entries = Vec::new();
            for _ in 0..n {
                match (lex.next(0), lex.next(0)) {
                    (Some(Tok::Obj(Obj::Num(num))), Some(Tok::Obj(Obj::Num(off)))) => entries.push((num as u32, off as usize)),
                    _ => break,
                }
            }
            for (num, off) in entries {
                if self.objs.len() >= MAX_OBJECTS || self.objs.contains_key(&num) {
                    continue;
                }
                if let Some(obj) = first.checked_add(off).and_then(|p| Lex::new(&data, p).value(0)) {
                    self.objs.insert(num, obj);
                }
            }
        }
    }

    /// Follow references.
    fn resolve<'b>(&'b self, mut o: &'b Obj) -> &'b Obj {
        for _ in 0..16 {
            match o {
                Obj::Ref(n) => o = self.objs.get(n).unwrap_or(&Obj::Null),
                _ => return o,
            }
        }
        &Obj::Null
    }

    fn rget<'b>(&'b self, d: &'b Dict, key: &str) -> Option<&'b Obj> {
        get(d, key).map(|o| self.resolve(o))
    }

    /// A stream's decoded data, or None if its filters aren't supported.
    fn stream(&self, o: &Obj) -> Option<Vec<u8>> {
        let Obj::Stream(dict, s, e) = self.resolve(o) else { return None };
        let mut data = self.data.get(*s..*e)?.to_vec();
        let filters: Vec<String> = match self.rget(dict, "Filter") {
            Some(Obj::Name(n)) => vec![n.clone()],
            Some(Obj::Arr(a)) => a.iter().filter_map(|f| self.resolve(f).name().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        for f in filters.iter().take(8) {
            let left = self.budget.get();
            data = match f.as_str() {
                "FlateDecode" | "Fl" => {
                    let body = if data.len() >= 2 && data[0] & 0x0F == 8 && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0 { &data[2..] } else { &data[..] };
                    inflate_partial(body, left)
                }
                "ASCIIHexDecode" | "AHx" => {
                    let Some(Tok::Obj(Obj::Str(v))) = Lex::new(&[b"<".as_slice(), &data, b">"].concat(), 0).next(0) else { return None };
                    v
                }
                "ASCII85Decode" | "A85" => ascii85(&data),
                "RunLengthDecode" | "RL" => run_length(&data, left),
                _ => return None, // images (DCT, JPX, CCITT...) and LZW: not text we can read
            };
            if data.len() > left {
                return None;
            }
            self.budget.set(left - data.len());
        }
        Some(data)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn ascii85(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0;
    let body = d.strip_prefix(b"<~").unwrap_or(d);
    for &c in body {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = c - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |a, &g| a * 85 + g as u64);
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |a, &g| a * 85 + g as u64);
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    out
}

fn run_length(d: &[u8], max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < d.len() && out.len() <= max {
        let n = d[i] as usize;
        i += 1;
        match n {
            128 => break,
            0..=127 => {
                let end = (i + n + 1).min(d.len());
                out.extend_from_slice(&d[i..end]);
                i = end;
            }
            _ => {
                if let Some(&b) = d.get(i) {
                    out.extend(std::iter::repeat_n(b, 257 - n));
                }
                i += 1;
            }
        }
    }
    out
}

// ---- fonts ----

struct Font {
    /// Two-byte character codes (composite fonts).
    wide: bool,
    /// Code -> text, from ToUnicode, encodings and glyph names.
    map: BTreeMap<u32, String>,
    /// Code -> width in thousandths of the font size.
    widths: BTreeMap<u32, f64>,
    default_width: f64,
}

impl Font {
    fn glyphs(&self, s: &[u8]) -> Vec<(u32, &str, f64)> {
        let codes: Vec<u32> = if self.wide {
            s.chunks(2).map(|c| if c.len() == 2 { u32::from(c[0]) << 8 | u32::from(c[1]) } else { u32::from(c[0]) }).collect()
        } else {
            s.iter().map(|&b| u32::from(b)).collect()
        };
        codes.into_iter().map(|c| (c, self.map.get(&c).map_or("", String::as_str), self.widths.get(&c).copied().unwrap_or(self.default_width))).collect()
    }
}

/// The text a simple font's byte stands for in a standard encoding.
fn base_encoding(name: &str, code: u8) -> Option<char> {
    let c = code as u32;
    match name {
        "MacRomanEncoding" if code >= 0x80 => MAC_ROMAN.chars().nth((c - 0x80) as usize),
        "StandardEncoding" => match code {
            0x27 => Some('’'),
            0x60 => Some('‘'),
            0x20..=0x7E => char::from_u32(c),
            _ => STANDARD_HIGH.iter().find(|(k, _)| *k == code).map(|(_, v)| *v),
        },
        "Symbol" => char::from_u32(c).filter(|ch| ch.is_ascii_graphic() || *ch == ' '),
        _ => match code {
            // WinAnsiEncoding (also the default for TrueType fonts)
            0x80..=0x9F => crate::plain::decode(&[code]).chars().next().filter(|&ch| ch != '\u{80}' && !ch.is_control()),
            0x20..=0x7E | 0xA0..=0xFF => char::from_u32(c),
            _ => None,
        },
    }
}

const MAC_ROMAN: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{A0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{F8FF}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";

const STANDARD_HIGH: [(u8, char); 39] = [
    (0xA1, '¡'), (0xA2, '¢'), (0xA3, '£'), (0xA4, '⁄'), (0xA5, '¥'), (0xA6, 'ƒ'), (0xA7, '§'), (0xA8, '¤'), (0xA9, '\''),
    (0xAA, '“'), (0xAB, '«'), (0xAC, '‹'), (0xAD, '›'), (0xAE, 'ﬁ'), (0xAF, 'ﬂ'), (0xB1, '–'), (0xB2, '†'), (0xB3, '‡'),
    (0xB4, '·'), (0xB6, '¶'), (0xB7, '•'), (0xB8, '‚'), (0xB9, '„'), (0xBA, '”'), (0xBB, '»'), (0xBC, '…'), (0xBD, '‰'),
    (0xBF, '¿'), (0xD0, '—'), (0xE1, 'Æ'), (0xE3, 'ª'), (0xE8, 'Ł'), (0xE9, 'Ø'), (0xEA, 'Œ'), (0xEB, 'º'), (0xF1, 'æ'),
    (0xF5, 'ı'), (0xF9, 'ø'), (0xFA, 'œ'),
];

/// Adobe glyph names (the common ones) -> text.
fn glyph_name(n: &str) -> Option<String> {
    if let Some(hex) = n.strip_prefix("uni").filter(|h| h.len() >= 4 && h.len() % 4 == 0) {
        let units: Option<Vec<u16>> = hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).ok().and_then(|h| u16::from_str_radix(h, 16).ok())).collect();
        return units.map(|u| String::from_utf16_lossy(&u));
    }
    if let Some(hex) = n.strip_prefix('u').filter(|h| (4..=6).contains(&h.len())) {
        if let Some(c) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
            return Some(c.to_string());
        }
    }
    let base = n.split('.').next().unwrap_or(n); // "a.sc", "one.oldstyle"
    if base.len() == 1 && base.is_ascii() {
        return Some(base.to_string());
    }
    const DIGITS: [&str; 10] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];
    if let Some(d) = DIGITS.iter().position(|d| *d == base) {
        return Some(d.to_string());
    }
    let s = match base {
        "space" | "nbspace" | "nonbreakingspace" => " ",
        "exclam" => "!", "quotedbl" => "\"", "numbersign" => "#", "dollar" => "$", "percent" => "%", "ampersand" => "&",
        "quotesingle" => "'", "parenleft" => "(", "parenright" => ")", "asterisk" => "*", "plus" => "+", "comma" => ",",
        "hyphen" | "minus" | "hyphenminus" => "-", "period" => ".", "slash" => "/", "colon" => ":", "semicolon" => ";",
        "less" => "<", "equal" => "=", "greater" => ">", "question" => "?", "at" => "@", "bracketleft" => "[",
        "backslash" => "\\", "bracketright" => "]", "asciicircum" => "^", "underscore" => "_", "grave" => "`",
        "braceleft" => "{", "bar" => "|", "braceright" => "}", "asciitilde" => "~",
        "quoteleft" => "‘", "quoteright" => "’", "quotedblleft" => "“", "quotedblright" => "”", "quotesinglbase" => "‚",
        "quotedblbase" => "„", "endash" => "–", "emdash" => "—", "bullet" => "•", "ellipsis" => "…", "dagger" => "†",
        "daggerdbl" => "‡", "perthousand" => "‰", "guilsinglleft" => "‹", "guilsinglright" => "›", "guillemotleft" => "«",
        "guillemotright" => "»", "fi" => "fi", "fl" => "fl", "ff" => "ff", "ffi" => "ffi", "ffl" => "ffl",
        "trademark" => "™", "copyright" => "©", "registered" => "®", "degree" => "°", "section" => "§",
        "paragraph" => "¶", "periodcentered" => "·", "exclamdown" => "¡", "questiondown" => "¿", "cent" => "¢",
        "sterling" => "£", "yen" => "¥", "Euro" | "euro" => "€", "florin" => "ƒ", "multiply" => "×", "divide" => "÷",
        "plusminus" => "±", "onehalf" => "½", "onequarter" => "¼", "threequarters" => "¾", "germandbls" => "ß",
        "ae" => "æ", "AE" => "Æ", "oe" => "œ", "OE" => "Œ", "oslash" => "ø", "Oslash" => "Ø", "dotlessi" => "ı",
        "lslash" => "ł", "Lslash" => "Ł", "eth" => "ð", "Eth" => "Ð", "thorn" => "þ", "Thorn" => "Þ",
        "ordfeminine" => "ª", "ordmasculine" => "º", "mu" => "µ", "currency" => "¤", "brokenbar" => "¦",
        "logicalnot" => "¬", "macron" => "¯", "acute" => "´", "cedilla" => "¸", "dieresis" => "¨",
        _ => return accented(base),
    };
    Some(s.to_string())
}

/// "eacute" -> "é", "Ntilde" -> "Ñ", ...
fn accented(n: &str) -> Option<String> {
    const MARKS: [(&str, char); 8] = [
        ("acute", '\u{301}'), ("grave", '\u{300}'), ("circumflex", '\u{302}'), ("dieresis", '\u{308}'),
        ("tilde", '\u{303}'), ("ring", '\u{30A}'), ("cedilla", '\u{327}'), ("caron", '\u{30C}'),
    ];
    let (mark, comb) = MARKS.iter().find(|(m, _)| n.len() == m.len() + 1 && n.ends_with(m))?;
    let base = n[..n.len() - mark.len()].chars().next().filter(char::is_ascii_alphabetic)?;
    Some(compose(base, *comb))
}

/// Precompose a letter and a combining mark when Latin-1/Latin Extended-A has it.
fn compose(base: char, mark: char) -> String {
    const TABLE: &str = "AÀÁÂÃÄÅ aàáâãäå EÈÉÊ Ë eèéê ë IÌÍÎ Ï iìíî ï OÒÓÔÕÖ oòóôõö UÙÚÛ Ü uùúû ü Y Ý  Ÿ y ý  ÿ N   Ñ n   ñ ";
    let order = ['\u{300}', '\u{301}', '\u{302}', '\u{303}', '\u{308}', '\u{30A}'];
    let special = match (base, mark) {
        ('C', '\u{327}') => Some('Ç'),
        ('c', '\u{327}') => Some('ç'),
        ('S', '\u{30C}') => Some('Š'),
        ('s', '\u{30C}') => Some('š'),
        ('Z', '\u{30C}') => Some('Ž'),
        ('z', '\u{30C}') => Some('ž'),
        _ => None,
    };
    if let Some(c) = special {
        return c.to_string();
    }
    if let Some(k) = order.iter().position(|&m| m == mark) {
        for group in TABLE.split(' ').filter(|g| !g.is_empty()) {
            let chars: Vec<char> = group.chars().collect();
            if chars.first() == Some(&base) {
                if let Some(&c) = chars.get(k + 1).filter(|c| **c != ' ') {
                    return c.to_string();
                }
            }
        }
    }
    format!("{base}{mark}")
}

/// Mappings from a ToUnicode CMap; returns (map, uses two-byte codes).
fn parse_cmap(data: &[u8]) -> (BTreeMap<u32, String>, bool) {
    let mut map = BTreeMap::new();
    let mut wide = false;
    let mut lex = Lex::new(data, 0);
    let code = |s: &[u8]| s.iter().take(4).fold(0u32, |a, &b| a << 8 | u32::from(b));
    let utf16 = |s: &[u8]| {
        let u: Vec<u16> = s.chunks(2).map(|c| if c.len() == 2 { u16::from_be_bytes([c[0], c[1]]) } else { u16::from(c[0]) }).collect();
        String::from_utf16_lossy(&u)
    };
    let mut toks: Vec<Tok> = Vec::new();
    while let Some(t) = lex.next(0) {
        if toks.len() > 2_000_000 {
            break;
        }
        toks.push(t);
    }
    let mut i = 0;
    while i < toks.len() {
        let Tok::Op(op) = &toks[i] else {
            i += 1;
            continue;
        };
        i += 1;
        match op.as_str() {
            "begincodespacerange" => {
                while let Some(Tok::Obj(Obj::Str(s))) = toks.get(i) {
                    wide |= s.len() >= 2;
                    i += 1;
                }
            }
            "beginbfchar" => {
                while let (Some(Tok::Obj(Obj::Str(src))), Some(Tok::Obj(dst))) = (toks.get(i), toks.get(i + 1)) {
                    match dst {
                        Obj::Str(d) => {
                            map.insert(code(src), utf16(d));
                        }
                        Obj::Name(n) => {
                            if let Some(t) = glyph_name(n) {
                                map.insert(code(src), t);
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
            }
            "beginbfrange" => {
                while let (Some(Tok::Obj(Obj::Str(lo))), Some(Tok::Obj(Obj::Str(hi))), Some(Tok::Obj(dst))) = (toks.get(i), toks.get(i + 1), toks.get(i + 2)) {
                    let (lo, hi) = (code(lo), code(hi));
                    let span = hi.saturating_sub(lo).min(65_535);
                    match dst {
                        Obj::Str(d) if !d.is_empty() => {
                            let mut units: Vec<u16> = d.chunks(2).map(|c| if c.len() == 2 { u16::from_be_bytes([c[0], c[1]]) } else { u16::from(c[0]) }).collect();
                            for k in 0..=span {
                                if map.len() > 300_000 {
                                    break;
                                }
                                map.insert(lo + k, String::from_utf16_lossy(&units));
                                if let Some(last) = units.last_mut() {
                                    *last = last.wrapping_add(1);
                                }
                            }
                        }
                        Obj::Arr(a) => {
                            for (k, d) in a.iter().take(span as usize + 1).enumerate() {
                                if let Obj::Str(d) = d {
                                    map.insert(lo + k as u32, utf16(d));
                                }
                            }
                        }
                        _ => {}
                    }
                    i += 3;
                }
            }
            _ => {}
        }
    }
    (map, wide)
}

impl Doc<'_> {
    fn font(&self, fobj: &Obj) -> Font {
        let empty = Dict::new();
        let fd = self.resolve(fobj).dict().unwrap_or(&empty);
        let subtype = self.rget(fd, "Subtype").and_then(Obj::name).unwrap_or("");
        let composite = subtype == "Type0";
        let mut font = Font { wide: composite, map: BTreeMap::new(), widths: BTreeMap::new(), default_width: 500.0 };

        if composite {
            // widths come from the descendant CIDFont's DW and W
            let desc = match self.rget(fd, "DescendantFonts") {
                Some(Obj::Arr(a)) => a.first().map(|d| self.resolve(d)),
                _ => None,
            };
            if let Some(dd) = desc.and_then(Obj::dict) {
                font.default_width = self.rget(dd, "DW").and_then(Obj::num).unwrap_or(1000.0);
                if let Some(Obj::Arr(w)) = self.rget(dd, "W") {
                    let mut k = 0;
                    while k < w.len() && font.widths.len() < 100_000 {
                        let first = self.resolve(&w[k]).num().unwrap_or(0.0).max(0.0) as u32;
                        match (w.get(k + 1).map(|o| self.resolve(o)), w.get(k + 2).map(|o| self.resolve(o))) {
                            (Some(Obj::Arr(ws)), _) => {
                                for (j, x) in ws.iter().enumerate() {
                                    if let Some(x) = self.resolve(x).num() {
                                        font.widths.insert(first + j as u32, x);
                                    }
                                }
                                k += 2;
                            }
                            (Some(Obj::Num(last)), Some(Obj::Num(x))) => {
                                let last = (*last as u32).min(first.saturating_add(65_535));
                                for c in first..=last {
                                    font.widths.insert(c, *x);
                                }
                                k += 3;
                            }
                            _ => break,
                        }
                    }
                }
            }
        } else {
            // simple font: base encoding, Differences, Widths
            let (base, diffs) = match self.rget(fd, "Encoding") {
                Some(Obj::Name(n)) => (n.clone(), None),
                Some(Obj::Dict(e)) => (
                    self.rget(e, "BaseEncoding").and_then(Obj::name).unwrap_or("WinAnsiEncoding").to_string(),
                    self.rget(e, "Differences").cloned(),
                ),
                _ => ((if subtype == "Type1" { "StandardEncoding" } else { "WinAnsiEncoding" }).to_string(), None),
            };
            for code in 0..=255u8 {
                if let Some(c) = base_encoding(&base, code) {
                    font.map.insert(u32::from(code), c.to_string());
                }
            }
            if let Some(Obj::Arr(d)) = diffs {
                let mut code = 0u32;
                for item in &d {
                    match self.resolve(item) {
                        Obj::Num(n) => code = n.max(0.0) as u32,
                        Obj::Name(n) => {
                            match glyph_name(n) {
                                Some(t) => font.map.insert(code, t),
                                None => font.map.remove(&code),
                            };
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
            let first = self.rget(fd, "FirstChar").and_then(Obj::num).unwrap_or(0.0).max(0.0) as u32;
            if let Some(Obj::Arr(ws)) = self.rget(fd, "Widths") {
                for (j, x) in ws.iter().take(256).enumerate() {
                    if let Some(x) = self.resolve(x).num() {
                        font.widths.insert(first + j as u32, x);
                    }
                }
            }
            if let Some(desc) = self.rget(fd, "FontDescriptor").and_then(Obj::dict) {
                if let Some(mw) = self.rget(desc, "MissingWidth").and_then(Obj::num).filter(|w| *w > 0.0) {
                    font.default_width = mw;
                }
            }
        }
        if let Some(cmap) = get(fd, "ToUnicode").and_then(|o| self.stream(o)) {
            let (map, wide) = parse_cmap(&cmap);
            if composite || !wide {
                for (k, v) in map {
                    font.map.insert(k, v);
                }
            }
        }
        font
    }
}

// ---- page content ----

type Mat = [f64; 6];

fn mul(a: &Mat, b: &Mat) -> Mat {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// A run of text drawn at one place.
#[derive(Debug, Clone)]
struct Span {
    x0: f64,
    x1: f64,
    y: f64,
    size: f64,
    text: String,
}

struct Interp<'d, 'a> {
    doc: &'d Doc<'a>,
    fonts: BTreeMap<u32, Font>,
    spans: Vec<Span>,
    ops: usize,
}

impl Interp<'_, '_> {
    fn font_for(&mut self, resources: &Dict, name: &str) -> Option<u32> {
        let fonts = self.doc.rget(resources, "Font").and_then(Obj::dict)?;
        let fobj = get(fonts, name)?;
        // cache by object number; inline fonts get a key past the object range
        let key = match fobj {
            Obj::Ref(n) => *n,
            _ => u32::MAX - self.fonts.len() as u32,
        };
        if !self.fonts.contains_key(&key) {
            let f = self.doc.font(fobj);
            self.fonts.insert(key, f);
        }
        Some(key)
    }

    /// Interpret a content stream, collecting text spans.
    fn run(&mut self, content: &[u8], resources: &Dict, ctm0: Mat, depth: usize) {
        let mut lex = Lex::new(content, 0);
        let mut operands: Vec<Obj> = Vec::new();
        let mut stack: Vec<Mat> = Vec::new();
        let mut ctm = ctm0;
        let (mut tm, mut tlm) = (IDENTITY, IDENTITY);
        let (mut tc, mut tw, mut th, mut tl, mut fs) = (0.0, 0.0, 1.0, 0.0, 0.0);
        let mut font: Option<u32> = None;
        while let Some(tok) = lex.next(0) {
            self.ops += 1;
            if self.ops > MAX_OPS {
                break;
            }
            let op = match tok {
                Tok::Obj(o) => {
                    if operands.len() < 64 {
                        operands.push(o);
                    }
                    continue;
                }
                Tok::Op(op) => op,
            };
            let n = |k: usize| operands.get(k).and_then(Obj::num).unwrap_or(0.0);
            match op.as_str() {
                "q" => {
                    if stack.len() < 64 {
                        stack.push(ctm);
                    }
                }
                "Q" => ctm = stack.pop().unwrap_or(ctm0),
                "cm" if operands.len() >= 6 => ctm = mul(&[n(0), n(1), n(2), n(3), n(4), n(5)], &ctm),
                "BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                "Tf" => {
                    fs = n(1);
                    font = operands.first().and_then(Obj::name).map(str::to_string).and_then(|f| self.font_for(resources, &f));
                }
                "Tc" => tc = n(0),
                "Tw" => tw = n(0),
                "Tz" => th = n(0) / 100.0,
                "TL" => tl = n(0),
                "Td" | "TD" => {
                    if op == "TD" {
                        tl = -n(1);
                    }
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, n(0), n(1)], &tlm);
                    tm = tlm;
                }
                "Tm" if operands.len() >= 6 => {
                    tlm = [n(0), n(1), n(2), n(3), n(4), n(5)];
                    tm = tlm;
                }
                "T*" | "'" | "\"" => {
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -tl], &tlm);
                    tm = tlm;
                    if op == "\"" {
                        tw = n(0);
                        tc = n(1);
                    }
                    if op != "T*" {
                        if let Some(Obj::Str(s)) = operands.last() {
                            let s = s.clone();
                            self.show(&[Obj::Str(s)], font, fs, tc, tw, th, &mut tm, &ctm);
                        }
                    }
                }
                "Tj" => {
                    if let Some(Obj::Str(s)) = operands.first() {
                        let s = s.clone();
                        self.show(&[Obj::Str(s)], font, fs, tc, tw, th, &mut tm, &ctm);
                    }
                }
                "TJ" => {
                    if let Some(Obj::Arr(a)) = operands.first() {
                        let a = a.clone();
                        self.show(&a, font, fs, tc, tw, th, &mut tm, &ctm);
                    }
                }
                "Do" if depth < 4 => {
                    // a form XObject can hold text too
                    let name = operands.first().and_then(Obj::name).map(str::to_string);
                    let xobjs = self.doc.rget(resources, "XObject").and_then(Obj::dict);
                    if let (Some(name), Some(xobjs)) = (name, xobjs) {
                        if let Some(x) = get(xobjs, &name) {
                            let x = self.doc.resolve(x).clone();
                            if let Obj::Stream(d, _, _) = &x {
                                if get(d, "Subtype").and_then(Obj::name) == Some("Form") {
                                    let m = match self.doc.rget(d, "Matrix") {
                                        Some(Obj::Arr(a)) if a.len() == 6 => {
                                            let v: Vec<f64> = a.iter().map(|o| self.doc.resolve(o).num().unwrap_or(0.0)).collect();
                                            [v[0], v[1], v[2], v[3], v[4], v[5]]
                                        }
                                        _ => IDENTITY,
                                    };
                                    let res = self.doc.rget(d, "Resources").and_then(Obj::dict).cloned().unwrap_or_else(|| resources.clone());
                                    if let Some(data) = self.doc.stream(&x) {
                                        self.run(&data, &res, mul(&m, &ctm), depth + 1);
                                    }
                                }
                            }
                        }
                    }
                }
                "BI" => {
                    // inline image: skip its binary data up to "EI"
                    while lex.i < content.len() {
                        if content[lex.i..].starts_with(b"ID") && (lex.i == 0 || is_ws(content[lex.i - 1])) {
                            lex.i += 2;
                            break;
                        }
                        lex.i += 1;
                    }
                    while lex.i + 2 <= content.len() {
                        if &content[lex.i..lex.i + 2] == b"EI" && is_ws(content[lex.i - 1]) && content.get(lex.i + 2).is_none_or(|&c| is_ws(c)) {
                            lex.i += 2;
                            break;
                        }
                        lex.i += 1;
                    }
                }
                _ => {}
            }
            operands.clear();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn show(&mut self, items: &[Obj], font: Option<u32>, fs: f64, tc: f64, tw: f64, th: f64, tm: &mut Mat, ctm: &Mat) {
        let Some(font) = font.and_then(|k| self.fonts.get(&k)) else { return };
        let start = mul(tm, ctm);
        let size = (start[2] * start[2] + start[3] * start[3]).sqrt() * fs.abs();
        let mut text = String::new();
        for item in items {
            match item {
                Obj::Str(s) => {
                    for (code, t, w) in font.glyphs(s) {
                        text.push_str(t);
                        let mut adv = w / 1000.0 * fs + tc;
                        if !font.wide && code == 32 {
                            adv += tw;
                        }
                        let tx = adv * th;
                        tm[4] += tx * tm[0];
                        tm[5] += tx * tm[1];
                    }
                }
                Obj::Num(k) => {
                    let tx = -k / 1000.0 * fs * th;
                    tm[4] += tx * tm[0];
                    tm[5] += tx * tm[1];
                    // a big kerning gap is a word space the PDF didn't spell out
                    if -k > 180.0 && !text.ends_with(' ') && !text.is_empty() {
                        text.push(' ');
                    }
                }
                _ => {}
            }
        }
        let end = mul(tm, ctm);
        if !text.trim().is_empty() && size.is_finite() && size > 0.0 && self.spans.len() < 2_000_000 {
            self.spans.push(Span { x0: start[4], x1: end[4].max(start[4]), y: start[5], size, text });
        }
    }
}

// ---- layout ----

/// Spans -> lines. Lines follow drawing order (which keeps the columns of most
/// multi-column PDFs apart); within a line, pieces are put left to right, since
/// some PDFs draw a line's letters in any order.
fn lines(spans: &[Span]) -> Vec<Span> {
    let mut groups: Vec<Vec<&Span>> = Vec::new();
    for s in spans {
        match groups.last_mut() {
            Some(g) if (s.y - g[0].y).abs() < 0.45 * s.size.max(g[0].size) => g.push(s),
            _ => groups.push(vec![s]),
        }
    }
    let mut out: Vec<Span> = Vec::new();
    for mut g in groups {
        g.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut line = g[0].clone();
        for s in &g[1..] {
            // text drawn twice to fake bold: skip the copy
            if (s.x0 - line.x1).abs() < 1.0 && line.text.ends_with(s.text.as_str()) && s.x1 <= line.x1 + 1.0 {
                continue;
            }
            if s.x0 >= line.x0 && s.x1 <= line.x1 + 0.5 && line.text.contains(s.text.as_str()) && s.text.len() > 2 {
                continue;
            }
            let gap = s.x0 - line.x1;
            if gap > 0.12 * s.size && !line.text.ends_with(' ') && !s.text.starts_with(' ') {
                line.text.push(' ');
            }
            line.text.push_str(&s.text);
            line.x1 = line.x1.max(s.x1);
            line.size = line.size.max(s.size);
        }
        line.text = line.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.text.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Page numbers alone on a line: "12", "Page 3", "3 of 10", "iv".
fn is_page_number(t: &str) -> bool {
    let t = t.trim().to_lowercase();
    let t = t.strip_prefix("page").map_or(t.as_str(), str::trim);
    let parts: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == '/').filter(|p| !p.is_empty() && *p != "of" && *p != "-").collect();
    !parts.is_empty()
        && parts.len() <= 3
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()) || p.chars().all(|c| "ivxlc".contains(c)))
}

/// Lines of one page -> paragraphs.
fn paragraphs_of(lines: &[Span]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    // the page's typical left edge, right edge and line spacing
    let mut lefts: Vec<i64> = lines.iter().map(|l| l.x0.round() as i64).collect();
    lefts.sort_unstable();
    let margin = mode(&lefts).unwrap_or(0) as f64;
    let mut rights: Vec<f64> = lines.iter().map(|l| l.x1).collect();
    rights.sort_by(f64::total_cmp);
    let right = rights[rights.len() * 9 / 10];
    let mut gaps: Vec<f64> = lines.windows(2).map(|w| w[0].y - w[1].y).filter(|g| *g > 0.0).collect();
    gaps.sort_by(f64::total_cmp);
    let spacing = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0);

    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, l) in lines.iter().enumerate() {
        let new_para = i == 0 || {
            let p = &lines[i - 1];
            let gap = p.y - l.y;
            let size_change = (l.size - p.size).abs() > 0.2 * p.size;
            let big_gap = spacing > 0.0 && gap > spacing * 1.45 || gap < -l.size; // or a jump back up (new column)
            // a first-line indent; but if the next line is indented the same way,
            // it's a hanging indent (a list item's continuation), not a new paragraph
            let hanging = lines.get(i + 1).is_some_and(|n| (n.x0 - l.x0).abs() < 0.5 * l.size);
            let indented = l.x0 > margin + 0.8 * l.size && l.x0 > p.x0 + 0.8 * l.size && !hanging;
            let short_prev = p.x1 < right - 4.0 * p.size && p.text.ends_with(['.', '!', '?', ':', '"', '”', '’', ')']);
            size_change || big_gap || indented || short_prev
        };
        if new_para && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if cur.ends_with('-') && l.text.starts_with(|c: char| c.is_lowercase()) {
            cur.pop(); // a word hyphenated across lines
        } else if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&l.text);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn mode(sorted: &[i64]) -> Option<i64> {
    let mut best = (0, None);
    let mut i = 0;
    while i < sorted.len() {
        let j = sorted[i..].iter().position(|&v| v != sorted[i]).map_or(sorted.len(), |k| i + k);
        if j - i > best.0 {
            best = (j - i, Some(sorted[i]));
        }
        i = j;
    }
    best.1
}

pub fn paragraphs(data: &[u8]) -> Result<Vec<String>, PdfError> {
    let doc = Doc::parse(data).ok_or(PdfError::Unreadable)?;
    if doc.trailers.iter().any(|t| get(t, "Encrypt").is_some()) {
        return Err(PdfError::Encrypted);
    }
    // the page tree, from the catalog
    let root = doc.trailers.iter().rev().find_map(|t| get(t, "Root")).map(|r| doc.resolve(r).clone()).or_else(|| {
        doc.objs.values().find(|o| o.dict().is_some_and(|d| get(d, "Type").and_then(Obj::name) == Some("Catalog"))).cloned()
    });
    let mut pages: Vec<(Dict, Dict)> = Vec::new(); // (page, inherited resources)
    if let Some(pages_root) = root.as_ref().and_then(Obj::dict).and_then(|d| get(d, "Pages")) {
        let mut seen = BTreeSet::new();
        walk(&doc, pages_root, &Dict::new(), &mut seen, 0, &mut pages);
    }
    if pages.is_empty() {
        // no usable tree: every page object, in object order
        for o in doc.objs.values() {
            if let Some(d) = o.dict().filter(|d| get(d, "Type").and_then(Obj::name) == Some("Page")) {
                let res = doc.rget(d, "Resources").and_then(Obj::dict).cloned().unwrap_or_default();
                pages.push((d.clone(), res));
                if pages.len() >= MAX_PAGES {
                    break;
                }
            }
        }
    }
    if pages.is_empty() {
        return Err(PdfError::Unreadable);
    }

    let mut interp = Interp { doc: &doc, fonts: BTreeMap::new(), spans: Vec::new(), ops: 0 };
    let mut page_lines: Vec<Vec<Span>> = Vec::new();
    for (page, resources) in &pages {
        interp.spans.clear();
        interp.ops = 0;
        let contents: Vec<&Obj> = match get(page, "Contents").map(|c| doc.resolve(c)) {
            Some(Obj::Arr(a)) => a.iter().collect(),
            Some(c @ Obj::Stream(..)) => vec![c],
            _ => Vec::new(),
        };
        let mut content = Vec::new();
        for c in contents.iter().take(1000) {
            if let Some(d) = doc.stream(c) {
                content.extend_from_slice(&d);
                content.push(b'\n');
            }
        }
        interp.run(&content, resources, IDENTITY, 0);
        page_lines.push(lines(&interp.spans));
    }

    // running headers and footers: the same top or bottom line on many pages
    let norm = |t: &str| t.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    // a page's edge lines: the two highest and two lowest on the page
    let edges = |lines: &[Span]| -> BTreeSet<usize> {
        let mut by_y: Vec<usize> = (0..lines.len()).collect();
        by_y.sort_by(|&a, &b| lines[b].y.total_cmp(&lines[a].y));
        by_y.iter().take(2).chain(by_y.iter().rev().take(2)).copied().collect()
    };
    for lines in &page_lines {
        let texts: BTreeSet<String> = edges(lines).into_iter().map(|i| norm(&lines[i].text)).collect();
        for t in texts {
            *counts.entry(t).or_default() += 1;
        }
    }
    let n_pages = page_lines.len();
    // repeated on 40% of pages, or (with a page number in it, like a section's
    // running head "The Low Road 19") on at least three pages
    let repeated = |t: &str| {
        let n = counts.get(&norm(t)).copied().unwrap_or(0);
        n_pages >= 3 && (n * 5 >= n_pages * 2 || n >= 3 && t.contains(|c: char| c.is_ascii_digit()))
    };

    let mut out: Vec<String> = Vec::new();
    for lines in page_lines {
        let edge = edges(&lines);
        let lines: Vec<Span> = lines
            .into_iter()
            .enumerate()
            .filter(|(i, l)| !(edge.contains(i) && (is_page_number(&l.text) || repeated(&l.text))))
            .map(|(_, l)| l)
            .collect();
        let mut paras = paragraphs_of(&lines).into_iter().peekable();
        // a paragraph that runs over the page break
        if let (Some(last), Some(_)) = (out.last_mut(), paras.peek()) {
            let first = paras.next().unwrap_or_default();
            if !last.ends_with(['.', '!', '?', '"', '”', ':']) && first.starts_with(|c: char| c.is_lowercase()) {
                if last.ends_with('-') {
                    last.pop();
                } else {
                    last.push(' ');
                }
                last.push_str(&first);
            } else {
                out.push(first);
            }
        }
        out.extend(paras);
    }
    // a paragraph that stops mid-sentence and a next one that starts in lowercase
    // are one paragraph split by a column or page break
    let mut joined: Vec<String> = Vec::with_capacity(out.len());
    for p in out {
        match joined.last_mut() {
            Some(last) if !last.ends_with(['.', '!', '?', '"', '”', '’', ':', ')']) && p.starts_with(|c: char| c.is_lowercase()) => {
                if last.ends_with('-') {
                    last.pop();
                } else {
                    last.push(' ');
                }
                last.push_str(&p);
            }
            _ => joined.push(p),
        }
    }
    if joined.is_empty() {
        return Err(PdfError::NoText);
    }
    Ok(joined)
}

fn walk(doc: &Doc, node: &Obj, inherited: &Dict, seen: &mut BTreeSet<u32>, depth: usize, out: &mut Vec<(Dict, Dict)>) {
    if depth > 64 || out.len() >= MAX_PAGES {
        return;
    }
    if let Obj::Ref(n) = node {
        if !seen.insert(*n) {
            return; // a cycle in the page tree
        }
    }
    let Some(d) = doc.resolve(node).dict() else { return };
    let resources = doc.rget(d, "Resources").and_then(Obj::dict).cloned().unwrap_or_else(|| inherited.clone());
    match doc.rget(d, "Kids") {
        Some(Obj::Arr(kids)) => {
            for k in kids {
                walk(doc, k, &resources, seen, depth + 1, out);
            }
        }
        _ => out.push((d.clone(), resources)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-page PDF with WinAnsi Helvetica text.
    fn tiny(content: &str) -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        for (i, o) in objs.iter().enumerate() {
            pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, o));
        }
        pdf.push_str("trailer\n<< /Root 1 0 R /Size 6 >>\n%%EOF\n");
        pdf.into_bytes()
    }

    #[test]
    fn reads_lines_into_paragraphs() {
        let content = "BT /F1 12 Tf 72 700 Td 14 TL (It was a dark and stormy night; the rain fell in tor-) Tj T* (rents, except at occasional intervals.) Tj \
                       0 -40 Td (Page 2 begins a new paragraph \\(really\\).) Tj ET";
        let p = paragraphs(&tiny(content)).unwrap();
        assert_eq!(p, ["It was a dark and stormy night; the rain fell in torrents, except at occasional intervals.", "Page 2 begins a new paragraph (really)."]);
    }

    #[test]
    fn tj_kerning_spaces_and_encrypted() {
        let p = paragraphs(&tiny("BT /F1 12 Tf 72 700 Td [(Hello)-300(world)] TJ ET")).unwrap();
        assert_eq!(p, ["Hello world"]);
        let enc = String::from_utf8(tiny("BT ET")).unwrap().replace("<< /Root", "<< /Encrypt 9 0 R /Root");
        assert_eq!(paragraphs(enc.as_bytes()), Err(PdfError::Encrypted));
        assert_eq!(paragraphs(&tiny("q 1 0 0 1 0 0 cm Q")), Err(PdfError::NoText));
    }

    #[test]
    fn cmap_and_glyph_names() {
        let (m, wide) = parse_cmap(b"1 begincodespacerange <0000> <FFFF> endcodespacerange 2 beginbfchar <0003> <0020> <0011> <00660069> endbfchar 1 beginbfrange <0024> <0026> <0041> endbfrange");
        assert!(wide);
        assert_eq!(m.get(&0x11).map(String::as_str), Some("fi"));
        assert_eq!(m.get(&0x26).map(String::as_str), Some("C"));
        assert_eq!(glyph_name("eacute").as_deref(), Some("é"));
        assert_eq!(glyph_name("uni201C").as_deref(), Some("“"));
        assert_eq!(glyph_name("quoteright").as_deref(), Some("’"));
        assert!(is_page_number("Page 3 of 10") && is_page_number("iv") && !is_page_number("Chapter 3"));
    }
}
