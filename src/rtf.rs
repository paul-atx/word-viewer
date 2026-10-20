//! Bounded RTF output buffer.

/// Output cap; rendering stops once it is reached.
const MAX_RTF: usize = 32 << 20;

pub struct Rtf {
    buf: String,
    pub full: bool,
}

impl Rtf {
    pub fn new() -> Rtf {
        let mut r = Rtf { buf: String::new(), full: false };
        r.put("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Segoe UI;}}\\fs22\n");
        r
    }

    /// Append trusted RTF markup (never document content).
    pub fn put(&mut self, s: &str) {
        if self.full || self.buf.len() + s.len() > MAX_RTF {
            self.full = true;
            return;
        }
        self.buf.push_str(s);
    }

    /// Append one character of document text, escaped.
    pub fn ch(&mut self, c: u32) {
        match c {
            0x5C | 0x7B | 0x7D => {
                // \ { } are RTF syntax; escape them
                let s = [b'\\', c as u8];
                self.put(std::str::from_utf8(&s).unwrap_or(""));
            }
            0x20..=0x7E => {
                let s = [c as u8];
                self.put(std::str::from_utf8(&s).unwrap_or(""));
            }
            0..=0x1F => {}
            0x7F..=0xFFFF => self.put(&format!("\\u{}?", c as u16 as i16)),
            0x10000..=0x10FFFF => {
                let c = c - 0x10000;
                let hi = (0xD800 + (c >> 10)) as u16 as i16;
                let lo = (0xDC00 + (c & 0x3FF)) as u16 as i16;
                self.put(&format!("\\u{}?\\u{}?", hi, lo));
            }
            _ => self.put("\\u-3?"), // U+FFFD
        }
    }

    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}
