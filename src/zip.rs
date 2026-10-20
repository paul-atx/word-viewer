//! Pulls one member out of a zip archive.

use crate::inflate::inflate;

fn u16at(b: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?) as usize)
}

fn u32at(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?) as usize)
}

/// Extract `want` from the archive `z`, refusing anything larger than `max` bytes.
pub fn extract(z: &[u8], want: &str, max: usize) -> Option<Vec<u8>> {
    // End-of-central-directory record: within the last 22 + 65535 (max comment) bytes.
    let last = z.len().checked_sub(22)?;
    let e = (last.saturating_sub(0xFFFF)..=last).rev().find(|&e| u32at(z, e) == Some(0x06054b50))?;
    let mut c = u32at(z, e + 16)?;
    for _ in 0..u16at(z, e + 10)? {
        if u32at(z, c)? != 0x02014b50 {
            return None;
        }
        let (nl, xl, cl) = (u16at(z, c + 28)?, u16at(z, c + 30)?, u16at(z, c + 32)?);
        let name = z.get(c + 46..c + 46 + nl)?;
        if name == want.as_bytes() {
            if u16at(z, c + 8)? & 1 != 0 {
                return None; // encrypted
            }
            let (meth, csz, usz, l) = (u16at(z, c + 10)?, u32at(z, c + 20)?, u32at(z, c + 24)?, u32at(z, c + 42)?);
            if u32at(z, l)? != 0x04034b50 || usz > max {
                return None;
            }
            let d = l + 30 + u16at(z, l + 26)? + u16at(z, l + 28)?;
            let data = z.get(d..d.checked_add(csz)?)?;
            return match meth {
                0 if csz == usz => Some(data.to_vec()),
                8 => inflate(data, usz),
                _ => None,
            };
        }
        c += 46 + nl + xl + cl;
    }
    None
}
