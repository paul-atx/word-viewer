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

pub fn render(z: &[u8]) -> Option<String> {
    const CP1252: [u16; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0,
        0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
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

    // FIB: skip the fibRgW and fibRgLw arrays to reach ccpText and fcClx/lcbClx.
    let lw = 34 + u16at(&wd, 32)? * 2;
    // Every character takes at least a byte, so this also bounds the work.
    let ccp = (u32at(&wd, lw + 2 + 12)? as usize).min(wd.len());
    let fl = lw + 2 + u16at(&wd, lw)? * 4 + 2 + 33 * 8;
    let clx_at = u32at(&wd, fl)? as usize;
    let clx = tbl.get(clx_at..clx_at.checked_add(u32at(&wd, fl + 4)? as usize)?)?;

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
    let (mut hidden, mut lastcell, mut pending) = (0, false, false);
    let mut prev = 0;
    r.put("\\pard\\sa160 ");
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
                // cell end; a second one in a row is the row end
                0x07 => {
                    pending = !lastcell;
                    if lastcell {
                        r.put("\\par\n\\pard\\sa60 ");
                    }
                }
                _ => {
                    if pending {
                        r.put("\\tab\\u9474?  ");
                        pending = false;
                    }
                    match ch {
                        0x0D | 0x0C => r.put("\\par\n\\pard\\sa160 "),
                        0x0B => r.put("\\line "),
                        0x09 => r.put("\\tab "),
                        0x1E => r.ch('-' as u32),
                        _ => r.ch(ch),
                    }
                }
            }
            lastcell = ch == 0x07 && pending;
        }
    }
    Some(r.finish())
}
