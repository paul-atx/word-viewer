//! Legacy Word 97-2003 (.doc): compound file -> piece table -> RTF (text only).

use crate::rtf::Rtf;

const ENDCHAIN: u32 = 0xFFFF_FFFE;

fn u16at(b: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?) as usize)
}

fn u32at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

fn table(bytes: &[u8]) -> Vec<u32> {
    bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect()
}

/// A parsed OLE compound file.
struct Cfb<'a> {
    z: &'a [u8],
    ssz: usize,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini: Vec<u8>,
    cutoff: usize,
    dir: Vec<u8>,
}

impl<'a> Cfb<'a> {
    fn sector(&self, id: u32) -> Option<&'a [u8]> {
        let off = (id as usize).checked_add(1)?.checked_mul(self.ssz)?;
        self.z.get(off..off.checked_add(self.ssz)?)
    }

    /// Follow a sector chain. With `size`, read exactly that many bytes; otherwise
    /// read to the end of the chain. Never returns more than the file's size.
    fn chain(&self, start: u32, size: Option<usize>, mini: bool) -> Option<Vec<u8>> {
        let (bs, next) = if mini { (64, &self.minifat) } else { (self.ssz, &self.fat) };
        let limit = size.unwrap_or(usize::MAX);
        if size.is_some_and(|s| s > self.z.len()) {
            return None;
        }
        let mut out = Vec::new();
        let mut id = start;
        while out.len() < limit {
            if id == ENDCHAIN && size.is_none() {
                break;
            }
            if out.len() > self.z.len() {
                return None; // a cycle in the chain
            }
            let blk = if mini {
                let o = (id as usize).checked_mul(64)?;
                self.mini.get(o..o + 64)?
            } else {
                self.sector(id)?
            };
            out.extend_from_slice(&blk[..bs.min(limit - out.len())]);
            id = *next.get(id as usize)?;
        }
        Some(out)
    }

    fn open(z: &'a [u8]) -> Option<Cfb<'a>> {
        let shift = u16at(z, 0x1E)?;
        if shift != 9 && shift != 12 {
            return None;
        }
        let ssz = 1usize << shift;
        let nfs = u32at(z, 0x2C)? as usize;
        if nfs > z.len() / ssz {
            return None;
        }
        let mut cf = Cfb { z, ssz, fat: Vec::new(), minifat: Vec::new(), mini: Vec::new(), cutoff: 0, dir: Vec::new() };

        // FAT sector ids: 109 in the header, the rest in the DIFAT chain.
        let mut ids: Vec<u32> = (0..109).map(|i| u32at(z, 0x4C + i * 4)).collect::<Option<_>>()?;
        let mut difat = u32at(z, 0x44)?;
        while ids.len() < nfs && difat < ENDCHAIN {
            let s = cf.sector(difat)?;
            ids.extend(table(&s[..ssz - 4]));
            difat = u32at(s, ssz - 4)?;
            if ids.len() > nfs + ssz {
                return None; // DIFAT cycle
            }
        }
        let mut fat = Vec::with_capacity(nfs * ssz);
        for &id in ids.iter().take(nfs) {
            fat.extend_from_slice(cf.sector(id)?);
        }
        cf.fat = table(&fat);

        cf.dir = cf.chain(u32at(z, 0x30)?, None, false)?;
        let root_start = u32at(&cf.dir, 116)?;
        let mf = u32at(z, 0x3C)?;
        if mf < ENDCHAIN {
            cf.minifat = table(&cf.chain(mf, None, false)?);
            cf.mini = cf.chain(root_start, None, false)?;
        }
        cf.cutoff = u32at(z, 0x38)? as usize;
        Some(cf)
    }

    fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let want: Vec<u16> = name.encode_utf16().collect();
        for d in self.dir.as_chunks::<128>().0 {
            let nl = u16at(d, 64)? / 2;
            if d[66] != 2 || nl != want.len() + 1 {
                continue;
            }
            if (0..want.len()).all(|i| u16at(d, i * 2) == Some(want[i] as usize)) {
                let size = u32at(d, 120)? as usize;
                return self.chain(u32at(d, 116)?, Some(size), size < self.cutoff);
            }
        }
        None
    }
}

// Character formatting, as bit flags.
const BOLD: u8 = 1;
const ITAL: u8 = 2;
const UL: u8 = 4;
const STRIKE: u8 = 8;
/// Deleted with change tracking on: not part of the document as shown.
const DELETED: u8 = 16;
/// Hidden text: Word doesn't show or print it.
const HIDDEN: u8 = 32;
const SHOWN: u8 = BOLD | ITAL | UL | STRIKE;

/// The paragraph properties the viewer uses.
#[derive(Clone, Copy, Default)]
struct Pap {
    /// Index of its style in the style sheet.
    istd: u16,
    /// A list item (numbered or bulleted).
    list: bool,
    /// In a table: a paragraph in a cell, a cell end or a row end.
    in_table: bool,
    /// The mark that ends a table row.
    row_end: bool,
}

/// Formatting runs from FKPs: (first fc, end fc, value), sorted by first fc.
/// Text positions outside every run have default formatting.
struct Runs<T> {
    runs: Vec<(u32, u32, T)>,
    last: usize,
}

impl<T: Copy + Default> Runs<T> {
    fn new(mut runs: Vec<(u32, u32, T)>) -> Runs<T> {
        runs.sort_by_key(|r| r.0);
        Runs { runs, last: 0 }
    }

    /// The value at stream offset `fc`.
    fn at(&mut self, fc: u32) -> T {
        // Text is mostly read in order, so the last run usually still applies.
        if let Some(&(a, b, v)) = self.runs.get(self.last) {
            if a <= fc && fc < b {
                return v;
            }
        }
        let i = self.runs.partition_point(|r| r.0 <= fc);
        match i.checked_sub(1).and_then(|j| Some((j, *self.runs.get(j)?))) {
            Some((j, (_, b, v))) if fc < b => {
                self.last = j;
                v
            }
            _ => T::default(),
        }
    }
}

/// Call `f(sprm, operand)` for each property modifier in a grpprl. Stops at the
/// first one whose size can't be worked out, keeping those before it.
fn sprms(mut g: &[u8], mut f: impl FnMut(u16, &[u8])) {
    while let Some(id) = u16at(g, 0) {
        let rest = &g[2..];
        let len = match id >> 13 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => match id {
                // sprmTDefTable: a 2-byte size, which counts itself minus one
                0xD608 => match u16at(rest, 0) {
                    Some(n) => n + 1,
                    None => return,
                },
                // sprmPChgTabs with over 255 bytes of tab stops: size not stored
                0xC615 if rest.first() == Some(&255) => return,
                _ => match rest.first() {
                    Some(&n) => 1 + n as usize,
                    None => return,
                },
            },
        };
        let Some(op) = rest.get(..len) else { return };
        f(id as u16, op);
        g = &rest[len..];
    }
}

/// Set or clear `bit` by a toggle operand: 1 on, 0 off; 0x81 means the
/// opposite of the style, which is taken as on (styles rarely set these).
fn toggle(flags: &mut u8, bit: u8, op: &[u8]) {
    match op.first() {
        Some(1 | 0x81) => *flags |= bit,
        _ => *flags &= !bit,
    }
}

fn chp(grpprl: &[u8]) -> u8 {
    let mut f = 0;
    sprms(grpprl, |id, op| match id {
        0x0835 => toggle(&mut f, BOLD, op),    // sprmCFBold
        0x0836 => toggle(&mut f, ITAL, op),    // sprmCFItalic
        0x0837 => toggle(&mut f, STRIKE, op),  // sprmCFStrike
        0x0800 => toggle(&mut f, DELETED, op), // sprmCFRMarkDel
        0x083C => toggle(&mut f, HIDDEN, op),  // sprmCFVanish
        0x2A3E => toggle(&mut f, UL, &[(op.first() != Some(&0)) as u8]), // sprmCKul: any underline style
        _ => {}
    });
    f
}

fn pap(istd: u16, grpprl: &[u8]) -> Pap {
    let mut p = Pap { istd, ..Pap::default() };
    sprms(grpprl, |id, op| match id {
        0x460B => p.list = u16at(op, 0).is_some_and(|v| (v as i16) > 0), // sprmPIlfo
        0x2416 => p.in_table = op.first().is_some_and(|&v| v != 0),        // sprmPFInTable
        0x2417 => p.row_end = op.first().is_some_and(|&v| v != 0),         // sprmPFTtp
        0x6649 => p.in_table |= u32at(op, 0).is_some_and(|v| v as i32 > 0), // sprmPItap
        _ => {}
    });
    p
}

/// The 512-byte FKP pages a bin table (PlcBteChpx or PlcBtePapx) points to.
fn fkp_pages<'a>(wd: &'a [u8], plc: &[u8]) -> Vec<&'a [u8]> {
    let n = plc.len().saturating_sub(4) / 8;
    let Some(pns) = plc.get((n + 1) * 4..) else { return Vec::new() };
    // A page can only be listed once per 512 bytes of the stream it lives in.
    (0..n.min(wd.len() / 512))
        .filter_map(|i| {
            let o = (u32at(pns, i * 4)? & 0x3F_FFFF) as usize * 512;
            wd.get(o..o + 512)
        })
        .collect()
}

/// Character formatting runs (non-default only), from the CHPX FKPs.
fn chpx_runs(wd: &[u8], plc: &[u8]) -> Vec<(u32, u32, u8)> {
    let mut runs = Vec::new();
    for page in fkp_pages(wd, plc) {
        let crun = page[511] as usize;
        if crun > 0x65 {
            continue;
        }
        for i in 0..crun {
            let (Some(a), Some(b), Some(&rgb)) = (u32at(page, i * 4), u32at(page, i * 4 + 4), page.get((crun + 1) * 4 + i)) else { continue };
            let o = rgb as usize * 2;
            let f = match page.get(o) {
                Some(&cb) if o > 0 => page.get(o + 1..o + 1 + cb as usize).map_or(0, chp),
                _ => 0,
            };
            if f != 0 && a < b {
                runs.push((a, b, f));
            }
        }
    }
    runs
}

/// Paragraph property runs, from the PAPX FKPs.
fn papx_runs(wd: &[u8], plc: &[u8]) -> Vec<(u32, u32, Pap)> {
    let mut runs = Vec::new();
    for page in fkp_pages(wd, plc) {
        let crun = page[511] as usize;
        if crun > 0x1D {
            continue;
        }
        for i in 0..crun {
            let (Some(a), Some(b), Some(&bx)) = (u32at(page, i * 4), u32at(page, i * 4 + 4), page.get((crun + 1) * 4 + i * 13)) else { continue };
            let o = bx as usize * 2;
            // PapxInFkp: cb, then 2*cb-1 bytes; or 0, then cb', then 2*cb' bytes
            let grp = match (page.get(o), page.get(o + 1)) {
                (Some(&cb), _) if cb != 0 && o > 0 => page.get(o + 1..o + 2 * cb as usize),
                (Some(0), Some(&cb)) if o > 0 => page.get(o + 2..o + 2 + 2 * cb as usize),
                _ => None,
            };
            if let Some(g) = grp.filter(|_| a < b) {
                if let Some(istd) = u16at(g, 0) {
                    runs.push((a, b, pap(istd as u16, &g[2..])));
                }
            }
        }
    }
    runs
}

/// Each style's built-in identifier (sti), indexed by style number (istd).
fn style_ids(stsh: &[u8]) -> Vec<u16> {
    let (Some(cb), Some(cstd)) = (u16at(stsh, 0), u16at(stsh, 2)) else { return Vec::new() };
    let mut at = 2 + cb;
    let mut out = Vec::new();
    for _ in 0..cstd {
        let Some(n) = u16at(stsh, at) else { break };
        // an empty slot (n == 0) is no style
        out.push(if n >= 2 { u16at(stsh, at + 2).map_or(0xFFF, |v| v as u16 & 0xFFF) } else { 0xFFF });
        at += 2 + n;
    }
    out
}

/// Heading level by built-in style: 1 to 3 (4 to 9 show as 3), or 4 for Title.
fn heading(sti: u16) -> usize {
    match sti {
        1 => 1,
        2 => 2,
        3..=9 => 3,
        62 => 4,
        _ => 0,
    }
}

/// Above this, a paragraph or table row is written out before its end is seen
/// (losing its style), so pathological paragraphs can't hold unbounded memory.
const FLUSH: usize = 1 << 20;

pub fn render(z: &[u8]) -> Option<String> {
    const CP1252: [u16; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0,
        0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
    const HEAD: [&str; 5] = ["", "\\b\\fs40 ", "\\b\\fs32 ", "\\b\\fs28 ", "\\b\\fs52 "];
    let cf = Cfb::open(z)?;
    let wd = cf.stream("WordDocument")?;
    // Word 97 and later only; Word 6/95 use a different layout.
    if wd.len() < 0x200 || u16at(&wd, 0)? != 0xA5EC || u16at(&wd, 2)? < 0xC0 {
        return None;
    }
    let mut r = Rtf::new();
    let flags = u16at(&wd, 0x0A)?;
    if flags & 0x0100 != 0 {
        r.put("This document is password-protected.");
        return Some(r.finish());
    }
    let tbl = cf.stream(if flags & 0x0200 != 0 { "1Table" } else { "0Table" })?;

    // FIB: skip the fibRgW and fibRgLw arrays to reach ccpText and the
    // (fc, lcb) pairs locating structures in the table stream.
    let lw = 34 + u16at(&wd, 32)? * 2;
    // Every character takes at least a byte, so this also bounds the work.
    let ccp = (u32at(&wd, lw + 2 + 12)? as usize).min(wd.len());
    let pairs = lw + 2 + u16at(&wd, lw)? * 4;
    let npairs = u16at(&wd, pairs)?;
    let pair = |i: usize| -> Option<&[u8]> {
        if i >= npairs {
            return None;
        }
        let fc = u32at(&wd, pairs + 2 + i * 8)? as usize;
        tbl.get(fc..fc.checked_add(u32at(&wd, pairs + 2 + i * 8 + 4)? as usize)?)
    };
    let clx = pair(33)?; // fcClx
    // Formatting is optional: if it can't be read, the text still shows.
    let stis = pair(1).map(style_ids).unwrap_or_default(); // fcStshf
    let mut chps = Runs::new(pair(12).map(|p| chpx_runs(&wd, p)).unwrap_or_default()); // fcPlcfBteChpx
    let mut paps = Runs::new(pair(13).map(|p| papx_runs(&wd, p)).unwrap_or_default()); // fcPlcfBtePapx
    let has_paps = !paps.runs.is_empty();

    let mut at = 0;
    while clx.get(at) == Some(&1) {
        at += 3 + u16at(clx, at + 1)?; // skip Prc entries
    }
    if clx.get(at) != Some(&2) {
        return None;
    }
    let pn = u32at(clx, at + 1)? as usize;
    let plc = clx.get(at + 5..(at + 5).checked_add(pn)?)?;
    let npc = pn.checked_sub(4)? / 12;
    let (cps, pcds) = plc.split_at((npc + 1) * 4); // CP array, then piece descriptors

    let mut fields: Vec<bool> = Vec::new(); // per open field: showing its result yet?
    let mut hidden = 0; // open fields still in their code part
    // A paragraph's style is stored with its mark, at its end, so each paragraph
    // (or table cell) collects in `para` until then; a table row in `row`.
    let (mut para, mut row) = (Rtf::fragment(), Rtf::fragment());
    let mut fmt = 0; // formatting of the open run group in `para`, 0 for none
    let mut para_started = false; // part of `para` already written out (see FLUSH)
    let (mut in_row, mut row_started, mut cell_done, mut prev_cell) = (false, false, false, false);
    let mut prev = 0;
    for i in 0..npc {
        let (a, b) = (u32at(cps, i * 4)? as usize, (u32at(cps, i * 4 + 4)? as usize).min(ccp));
        if a >= ccp || a < prev || b < a {
            break; // CPs must ascend, so no text is read twice
        }
        prev = b;
        let fc = u32at(pcds, i * 8 + 2)?;
        let packed = fc & 0x4000_0000 != 0;
        let (off, w) = if packed { (((fc & 0x3FFF_FFFF) / 2) as usize, 1) } else { (fc as usize, 2) };
        for k in 0..b - a {
            if r.full {
                break;
            }
            let at = off + k * w;
            let ch = if packed {
                let Some(&c) = wd.get(at) else { break };
                match c {
                    0x80..=0x9F if CP1252[c as usize - 0x80] != 0 => CP1252[c as usize - 0x80] as u32,
                    _ => c as u32,
                }
            } else {
                let Some(c) = u16at(&wd, at) else { break };
                c as u32
            };
            match ch {
                // fields: hide the code (0x13..0x14), show the result (0x14..0x15)
                0x13 => {
                    if fields.len() < 64 {
                        fields.push(false);
                        hidden += 1;
                    }
                }
                0x14 => {
                    if fields.last() == Some(&false) {
                        *fields.last_mut()? = true;
                        hidden -= 1;
                    }
                }
                0x15 => {
                    if fields.pop() == Some(false) {
                        hidden -= 1;
                    }
                }
                _ if hidden > 0 => {}
                _ => {
                    let fc = at as u32; // at < wd.len() <= MAX_FILE
                    let f = chps.at(fc);
                    if f & (DELETED | HIDDEN) != 0 {
                        continue;
                    }
                    if !matches!(ch, 0x0D | 0x0C | 0x07) {
                        prev_cell = false;
                        if f & SHOWN != fmt {
                            if fmt != 0 {
                                para.put("}");
                            }
                            if f & SHOWN != 0 {
                                para.put("{");
                                for (bit, word) in [(BOLD, "\\b "), (ITAL, "\\i "), (UL, "\\ul "), (STRIKE, "\\strike ")] {
                                    if f & bit != 0 {
                                        para.put(word);
                                    }
                                }
                            }
                            fmt = f & SHOWN;
                        }
                        match ch {
                            0x0B => para.put("\\line "),
                            0x09 => para.put("\\tab "),
                            0x1E => para.ch('-' as u32),
                            _ => para.ch(ch),
                        }
                        if para.size() > FLUSH {
                            if !para_started {
                                r.put("\\pard\\sa160 ");
                            }
                            r.put(&para.take());
                            para_started = true;
                        }
                        continue;
                    }

                    // A paragraph, cell or row ends here.
                    if fmt != 0 {
                        para.put("}");
                        fmt = 0;
                    }
                    let p = paps.at(fc);
                    let into_row = |row: &mut Rtf, para: &mut Rtf, cell_done: &mut bool| {
                        if std::mem::take(cell_done) {
                            row.put("\\tab\\u9474?  ");
                        }
                        row.put(&para.take());
                    };
                    if ch == 0x07 {
                        // Without paragraph properties, a second cell mark in a
                        // row is taken as the row end (wrong after an empty cell).
                        let row_end = if has_paps && (p.in_table || p.row_end) { p.row_end } else { prev_cell };
                        if row_end {
                            if !row_started {
                                r.put("\\pard\\sa60 ");
                            }
                            r.put(&row.take());
                            r.put("\\par\n");
                            para.take();
                            (in_row, row_started, cell_done, prev_cell) = (false, false, false, false);
                        } else {
                            into_row(&mut row, &mut para, &mut cell_done);
                            (in_row, cell_done, prev_cell) = (true, true, true);
                        }
                    } else if has_paps && p.in_table {
                        // one of several paragraphs in a cell
                        into_row(&mut row, &mut para, &mut cell_done);
                        row.put("\\line ");
                        in_row = true;
                    } else {
                        if in_row {
                            // a table that ended without a row mark
                            if !row_started {
                                r.put("\\pard\\sa60 ");
                            }
                            r.put(&row.take());
                            r.put("\\par\n");
                            (in_row, row_started, cell_done) = (false, false, false);
                        }
                        let sti = stis.get(p.istd as usize).copied().unwrap_or(0xFFF);
                        let head = if para_started { 0 } else { heading(sti) };
                        if !para_started {
                            r.put("\\pard\\sa160 ");
                            // a list item, or a List Bullet / List Number style;
                            // headings are often in a list too, for outline numbering
                            if head == 0 && (p.list || matches!(sti, 48 | 49 | 54..=61)) {
                                r.put("\\li360\\fi-240 \\bullet\\tab ");
                            }
                        }
                        if head > 0 {
                            r.put("{");
                            r.put(HEAD[head]);
                        }
                        r.put(&para.take());
                        if head > 0 {
                            r.put("}");
                        }
                        r.put("\\par\n");
                        para_started = false;
                    }
                    if row.size() > FLUSH {
                        if !row_started {
                            r.put("\\pard\\sa60 ");
                        }
                        r.put(&row.take());
                        row_started = true;
                    }
                }
            }
        }
    }
    // Text after the last mark (normally none).
    if fmt != 0 {
        para.put("}");
    }
    r.put(&row.take());
    r.put(&para.take());
    Some(r.finish())
}
