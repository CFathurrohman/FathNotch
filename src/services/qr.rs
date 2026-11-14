//! A small QR code encoder: byte mode, error correction level M, versions 1–40 (a phone link needs ~4).
//! Follows ISO/IEC 18004 (structure after Project Nayuki's reference implementation).

pub struct Qr {
    pub size: usize,
    modules: Vec<bool>,
}

impl Qr {
    /// Dark module at column `x`, row `y`.
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.modules[y * self.size + x]
    }
}

// Level M rows of the standard tables (index = version).
const ECC_PER_BLOCK: [u8; 41] = [
    0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28,
];
const NUM_BLOCKS: [u8; 41] = [
    0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38,
    40, 43, 45, 47, 49,
];
/// Format bits for level M.
const ECL_BITS: u32 = 0;

fn raw_modules(ver: usize) -> usize {
    let mut r = (16 * ver + 128) * ver + 64;
    if ver >= 2 {
        let n = ver / 7 + 2;
        r -= (25 * n - 10) * n - 55;
        if ver >= 7 {
            r -= 36;
        }
    }
    r
}

fn data_codewords(ver: usize) -> usize {
    raw_modules(ver) / 8 - ECC_PER_BLOCK[ver] as usize * NUM_BLOCKS[ver] as usize
}

/// Encodes `data` with the best mask (lowest penalty).
pub fn encode(data: &[u8]) -> Option<Qr> {
    encode_mask(data, None)
}

/// `mask`: force one (0–7), or None to pick the best.
pub fn encode_mask(data: &[u8], mask: Option<u8>) -> Option<Qr> {
    let ver = (1..=40).find(|&v| {
        let cc = if v <= 9 { 8 } else { 16 };
        4 + cc + data.len() * 8 <= data_codewords(v) * 8
    })?;
    // Bit stream: byte mode, length, data, terminator, padding.
    let mut bits: Vec<bool> = Vec::new();
    let push = |val: u32, n: usize, bits: &mut Vec<bool>| {
        for i in (0..n).rev() {
            bits.push((val >> i) & 1 != 0);
        }
    };
    push(4, 4, &mut bits);
    push(data.len() as u32, if ver <= 9 { 8 } else { 16 }, &mut bits);
    for &b in data {
        push(b as u32, 8, &mut bits);
    }
    let cap = data_codewords(ver) * 8;
    let term = (cap - bits.len()).min(4);
    push(0, term, &mut bits);
    let pad = (8 - bits.len() % 8) % 8;
    push(0, pad, &mut bits);
    let mut alt = 0xEC;
    while bits.len() < cap {
        push(alt, 8, &mut bits);
        alt ^= 0xEC ^ 0x11;
    }
    let mut codewords = vec![0u8; bits.len() / 8];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            codewords[i >> 3] |= 1 << (7 - (i & 7));
        }
    }
    let all = add_ecc(&codewords, ver);

    let size = ver * 4 + 17;
    let mut q = Builder { size, modules: vec![false; size * size], func: vec![false; size * size] };
    q.function_patterns(ver);
    q.codewords(&all);
    let mask = match mask {
        Some(m) => m,
        None => {
            let mut best = (u32::MAX, 0u8);
            for m in 0..8u8 {
                q.apply_mask(m);
                q.format_bits(m);
                let p = q.penalty();
                if p < best.0 {
                    best = (p, m);
                }
                q.apply_mask(m); // undo (XOR)
            }
            best.1
        }
    };
    q.apply_mask(mask);
    q.format_bits(mask);
    Some(Qr { size, modules: q.modules })
}

// ---------------------------------------------------------------- error correction

fn gf_mul(x: u8, y: u8) -> u8 {
    let mut z: u32 = 0;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x11D);
        z ^= ((y as u32 >> i) & 1) * x as u32;
    }
    z as u8
}

fn rs_divisor(deg: usize) -> Vec<u8> {
    let mut r = vec![0u8; deg];
    r[deg - 1] = 1;
    let mut root = 1u8;
    for _ in 0..deg {
        for j in 0..deg {
            r[j] = gf_mul(r[j], root);
            if j + 1 < deg {
                r[j] ^= r[j + 1];
            }
        }
        root = gf_mul(root, 2);
    }
    r
}

fn rs_remainder(data: &[u8], div: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; div.len()];
    for &b in data {
        let factor = b ^ r.remove(0);
        r.push(0);
        for (x, &d) in r.iter_mut().zip(div) {
            *x ^= gf_mul(d, factor);
        }
    }
    r
}

fn add_ecc(data: &[u8], ver: usize) -> Vec<u8> {
    let nblocks = NUM_BLOCKS[ver] as usize;
    let ecc_len = ECC_PER_BLOCK[ver] as usize;
    let raw = raw_modules(ver) / 8;
    let nshort = nblocks - raw % nblocks;
    let short_len = raw / nblocks;
    let div = rs_divisor(ecc_len);
    let mut blocks = Vec::new();
    let mut k = 0;
    for i in 0..nblocks {
        let len = short_len - ecc_len + usize::from(i >= nshort);
        let mut dat = data[k..k + len].to_vec();
        k += len;
        let ecc = rs_remainder(&dat, &div);
        if i < nshort {
            dat.push(0);
        }
        dat.extend(ecc);
        blocks.push(dat);
    }
    let mut out = Vec::with_capacity(raw);
    for i in 0..blocks[0].len() {
        for (j, b) in blocks.iter().enumerate() {
            if i != short_len - ecc_len || j >= nshort {
                out.push(b[i]);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- layout

struct Builder {
    size: usize,
    modules: Vec<bool>,
    func: Vec<bool>,
}

impl Builder {
    fn set_func(&mut self, x: usize, y: usize, dark: bool) {
        self.modules[y * self.size + x] = dark;
        self.func[y * self.size + x] = true;
    }

    fn function_patterns(&mut self, ver: usize) {
        let size = self.size;
        for i in 0..size {
            self.set_func(6, i, i % 2 == 0);
            self.set_func(i, 6, i % 2 == 0);
        }
        for (cx, cy) in [(3i32, 3i32), (size as i32 - 4, 3), (3, size as i32 - 4)] {
            for dy in -4..=4i32 {
                for dx in -4..=4i32 {
                    let (x, y) = (cx + dx, cy + dy);
                    if x >= 0 && y >= 0 && (x as usize) < size && (y as usize) < size {
                        let d = dx.abs().max(dy.abs());
                        self.set_func(x as usize, y as usize, d != 2 && d != 4);
                    }
                }
            }
        }
        let pos = align_positions(ver, size);
        let n = pos.len();
        for i in 0..n {
            for j in 0..n {
                if (i == 0 && j == 0) || (i == 0 && j == n - 1) || (i == n - 1 && j == 0) {
                    continue;
                }
                for dy in -2..=2i32 {
                    for dx in -2..=2i32 {
                        let (x, y) = ((pos[i] as i32 + dx) as usize, (pos[j] as i32 + dy) as usize);
                        self.set_func(x, y, dx.abs().max(dy.abs()) != 1);
                    }
                }
            }
        }
        self.format_bits(0); // reserve the areas
        if ver >= 7 {
            let mut rem = ver as u32;
            for _ in 0..12 {
                rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
            }
            let bits = (ver as u32) << 12 | rem;
            for i in 0..18 {
                let bit = (bits >> i) & 1 != 0;
                let a = size - 11 + i % 3;
                let b = i / 3;
                self.set_func(a, b, bit);
                self.set_func(b, a, bit);
            }
        }
    }

    fn format_bits(&mut self, mask: u8) {
        let size = self.size;
        let data = ECL_BITS << 3 | mask as u32;
        let mut rem = data;
        for _ in 0..10 {
            rem = (rem << 1) ^ ((rem >> 9) * 0x537);
        }
        let bits = (data << 10 | rem) ^ 0x5412;
        let bit = |i: usize| (bits >> i) & 1 != 0;
        for i in 0..=5 {
            self.set_func(8, i, bit(i));
        }
        self.set_func(8, 7, bit(6));
        self.set_func(8, 8, bit(7));
        self.set_func(7, 8, bit(8));
        for i in 9..15 {
            self.set_func(14 - i, 8, bit(i));
        }
        for i in 0..8 {
            self.set_func(size - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set_func(8, size - 15 + i, bit(i));
        }
        self.set_func(8, size - 8, true);
    }

    fn codewords(&mut self, data: &[u8]) {
        let size = self.size;
        let mut i = 0usize;
        let mut right = size as i32 - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            for vert in 0..size {
                for j in 0..2 {
                    let x = (right - j) as usize;
                    let upward = (right + 1) & 2 == 0;
                    let y = if upward { size - 1 - vert } else { vert };
                    if !self.func[y * size + x] && i < data.len() * 8 {
                        self.modules[y * size + x] = (data[i >> 3] >> (7 - (i & 7))) & 1 != 0;
                        i += 1;
                    }
                }
            }
            right -= 2;
        }
    }

    fn apply_mask(&mut self, mask: u8) {
        let size = self.size;
        for y in 0..size {
            for x in 0..size {
                let invert = match mask {
                    0 => (x + y) % 2 == 0,
                    1 => y % 2 == 0,
                    2 => x % 3 == 0,
                    3 => (x + y) % 3 == 0,
                    4 => (x / 3 + y / 2) % 2 == 0,
                    5 => x * y % 2 + x * y % 3 == 0,
                    6 => (x * y % 2 + x * y % 3) % 2 == 0,
                    _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
                };
                if invert && !self.func[y * size + x] {
                    self.modules[y * size + x] ^= true;
                }
            }
        }
    }

    /// The standard's four penalty rules.
    fn penalty(&self) -> u32 {
        let n = self.size;
        let m = |x: usize, y: usize| self.modules[y * n + x];
        let mut score = 0u32;
        // Rule 1: runs of 5+ same-colour modules in a row or column.
        for horizontal in [true, false] {
            for a in 0..n {
                let mut run = 1;
                for b in 1..n {
                    let (cur, prev) = if horizontal { (m(b, a), m(b - 1, a)) } else { (m(a, b), m(a, b - 1)) };
                    if cur == prev {
                        run += 1;
                        if run == 5 {
                            score += 3;
                        } else if run > 5 {
                            score += 1;
                        }
                    } else {
                        run = 1;
                    }
                }
            }
        }
        // Rule 2: 2×2 blocks of one colour.
        for y in 0..n - 1 {
            for x in 0..n - 1 {
                let c = m(x, y);
                if c == m(x + 1, y) && c == m(x, y + 1) && c == m(x + 1, y + 1) {
                    score += 3;
                }
            }
        }
        // Rule 3: finder-like 1:1:3:1:1 with four light modules on either side.
        const P1: [bool; 11] = [true, false, true, true, true, false, true, false, false, false, false];
        const P2: [bool; 11] = [false, false, false, false, true, false, true, true, true, false, true];
        for a in 0..n {
            for b in 0..n.saturating_sub(10) {
                for horizontal in [true, false] {
                    let at = |k: usize| if horizontal { m(b + k, a) } else { m(a, b + k) };
                    if (0..11).all(|k| at(k) == P1[k]) {
                        score += 40;
                    }
                    if (0..11).all(|k| at(k) == P2[k]) {
                        score += 40;
                    }
                }
            }
        }
        // Rule 4: dark/light balance.
        let dark = self.modules.iter().filter(|d| **d).count();
        let total = n * n;
        let k = ((dark * 20).abs_diff(total * 10)).div_ceil(total).saturating_sub(1);
        score += k as u32 * 10;
        score
    }
}

fn align_positions(ver: usize, size: usize) -> Vec<usize> {
    if ver == 1 {
        return vec![];
    }
    let n = ver / 7 + 2;
    let step = (ver * 8 + n * 3 + 5) / (n * 4 - 4) * 2;
    let mut r = vec![0; n];
    r[0] = 6;
    let mut pos = size - 7;
    for i in (1..n).rev() {
        r[i] = pos;
        pos -= step;
    }
    r
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dump() {
        let text = std::env::var("QR_TEXT").unwrap_or_else(|_| "http://192.168.1.20:53318/?k=abcdefghjkmn".into());
        for m in 0..8 {
            let q = super::encode_mask(text.as_bytes(), Some(m)).unwrap();
            let mut s = String::new();
            for y in 0..q.size {
                for x in 0..q.size {
                    s.push(if q.get(x, y) { '1' } else { '0' });
                }
                s.push('\n');
            }
            std::fs::write(format!("{}/qr-mask{m}.txt", std::env::var("QR_OUT").unwrap_or(".".into())), s).unwrap();
        }
        let best = super::encode(text.as_bytes()).unwrap();
        println!("size {}", best.size);
    }
}
