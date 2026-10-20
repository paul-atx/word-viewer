//! Raw DEFLATE decoder (RFC 1951), after zlib's puff.c.

struct Huff {
    count: [u16; 16],
    symbol: [u16; 288],
}

impl Huff {
    fn new(lens: &[u8]) -> Huff {
        let mut h = Huff { count: [0; 16], symbol: [0; 288] };
        for &l in lens {
            h.count[l as usize] += 1;
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + h.count[i];
        }
        for (i, &l) in lens.iter().enumerate() {
            if l != 0 {
                h.symbol[offs[l as usize] as usize] = i as u16;
                offs[l as usize] += 1;
            }
        }
        h.count[0] = 0;
        h
    }
}

struct State<'a> {
    inp: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
    out: Vec<u8>,
    max: usize,
}

type R<T> = Result<T, ()>;

impl State<'_> {
    fn bits(&mut self, need: u32) -> R<u32> {
        let mut v = self.bitbuf as u64;
        while self.bitcnt < need {
            let b = *self.inp.get(self.pos).ok_or(())?;
            self.pos += 1;
            v |= (b as u64) << self.bitcnt;
            self.bitcnt += 8;
        }
        self.bitbuf = (v >> need) as u32;
        self.bitcnt -= need;
        Ok((v & ((1u64 << need) - 1)) as u32)
    }

    fn decode(&mut self, h: &Huff) -> R<usize> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= self.bits(1)? as i32;
            let count = h.count[len] as i32;
            if code - count < first {
                return h.symbol.get((index + code - first) as usize).map(|&s| s as usize).ok_or(());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(())
    }

    fn codes(&mut self, lc: &Huff, dc: &Huff) -> R<()> {
        const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
        const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
        const DBASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
        const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
        loop {
            let sym = self.decode(lc)?;
            if sym < 256 {
                if self.out.len() >= self.max {
                    return Err(());
                }
                self.out.push(sym as u8);
                continue;
            }
            if sym == 256 {
                return Ok(());
            }
            let s = sym - 257;
            let len = *LBASE.get(s).ok_or(())? as usize + self.bits(LEXT[s] as u32)? as usize;
            let ds = self.decode(dc)?;
            let dist = *DBASE.get(ds).ok_or(())? as usize + self.bits(DEXT[ds] as u32)? as usize;
            if dist > self.out.len() || self.out.len() + len > self.max {
                return Err(());
            }
            for _ in 0..len {
                let b = self.out[self.out.len() - dist];
                self.out.push(b);
            }
        }
    }

    fn stored(&mut self) -> R<()> {
        self.bitbuf = 0;
        self.bitcnt = 0;
        let hdr = self.inp.get(self.pos..self.pos + 4).ok_or(())?;
        let n = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
        if u16::from_le_bytes([hdr[2], hdr[3]]) as usize != !n & 0xFFFF {
            return Err(());
        }
        self.pos += 4;
        let data = self.inp.get(self.pos..self.pos + n).ok_or(())?;
        if self.out.len() + n > self.max {
            return Err(());
        }
        self.out.extend_from_slice(data);
        self.pos += n;
        Ok(())
    }

    fn dynamic(&mut self) -> R<()> {
        const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
        let nlen = self.bits(5)? as usize + 257;
        let ndist = self.bits(5)? as usize + 1;
        let ncode = self.bits(4)? as usize + 4;
        if nlen > 286 || ndist > 30 {
            return Err(());
        }
        let mut lens = [0u8; 320];
        for &o in &ORDER[..ncode] {
            lens[o] = self.bits(3)? as u8;
        }
        let lc = Huff::new(&lens[..19]);
        let mut i = 0;
        while i < nlen + ndist {
            let sym = self.decode(&lc)?;
            let (val, rep) = match sym {
                0..=15 => {
                    lens[i] = sym as u8;
                    i += 1;
                    continue;
                }
                16 if i > 0 => (lens[i - 1], 3 + self.bits(2)? as usize),
                17 => (0, 3 + self.bits(3)? as usize),
                18 => (0, 11 + self.bits(7)? as usize),
                _ => return Err(()),
            };
            if i + rep > nlen + ndist {
                return Err(());
            }
            lens[i..i + rep].fill(val);
            i += rep;
        }
        let lc = Huff::new(&lens[..nlen]);
        let dc = Huff::new(&lens[nlen..nlen + ndist]);
        self.codes(&lc, &dc)
    }
}

/// Inflate raw DEFLATE data `inp`, which must decompress to exactly `size` bytes.
pub fn inflate(inp: &[u8], size: usize) -> Option<Vec<u8>> {
    let mut s = State { inp, pos: 0, bitbuf: 0, bitcnt: 0, out: Vec::with_capacity(size), max: size };
    let run = |s: &mut State| -> R<()> {
        loop {
            let last = s.bits(1)?;
            match s.bits(2)? {
                0 => s.stored()?,
                1 => {
                    let mut l = [8u8; 288];
                    l[144..256].fill(9);
                    l[256..280].fill(7);
                    s.codes(&Huff::new(&l), &Huff::new(&[5u8; 30]))?
                }
                2 => s.dynamic()?,
                _ => return Err(()),
            }
            if last == 1 {
                return Ok(());
            }
        }
    };
    run(&mut s).ok()?;
    (s.out.len() == size).then_some(s.out)
}
