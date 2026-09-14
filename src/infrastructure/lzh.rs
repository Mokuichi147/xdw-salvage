//! XDWページの独自符号化を展開するLHA `-lh5-` 互換デコーダー。

use crate::error::{Error, Result};

const THRESHOLD: usize = 3;
const MAXMATCH: usize = 256;
const NC: usize = 255 + MAXMATCH + 2 - THRESHOLD;
const NT: usize = 19;
const TBIT: u32 = 5;
const CBIT: u32 = 9;
const PBIT: u32 = 4;
const DICBIT: usize = 13;
const NP: usize = DICBIT + 1;
const MAX_CODE_LEN: usize = 16;

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    fn bit(&mut self) -> Result<u32> {
        let byte = self
            .data
            .get(self.pos >> 3)
            .ok_or(Error::CodingTruncated { at: self.pos })?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(b as u32)
    }

    fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }
}

#[derive(Default)]
struct Huffman {
    counts: [u16; MAX_CODE_LEN + 1],
    symbols: Vec<u16>,
    only: Option<u16>,
}

impl Huffman {
    fn single(sym: u16) -> Self {
        Huffman {
            only: Some(sym),
            ..Default::default()
        }
    }

    fn build(lengths: &[u8]) -> Result<Self> {
        let mut counts = [0u16; MAX_CODE_LEN + 1];
        for &l in lengths {
            let l = l as usize;
            if l > MAX_CODE_LEN {
                return Err(Error::CodingBadTable);
            }
            counts[l] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0usize; MAX_CODE_LEN + 2];
        for l in 1..=MAX_CODE_LEN {
            offsets[l + 1] = offsets[l] + counts[l] as usize;
        }
        let total = offsets[MAX_CODE_LEN + 1];
        let mut symbols = vec![0u16; total];
        let mut next = offsets;
        for (sym, &l) in lengths.iter().enumerate() {
            if l > 0 {
                let l = l as usize;
                symbols[next[l]] = sym as u16;
                next[l] += 1;
            }
        }
        Ok(Huffman {
            counts,
            symbols,
            only: None,
        })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16> {
        if let Some(s) = self.only {
            return Ok(s);
        }
        let (mut code, mut first, mut index) = (0i32, 0i32, 0usize);
        for len in 1..=MAX_CODE_LEN {
            code |= bits.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                let at = index + (code - first) as usize;
                return self.symbols.get(at).copied().ok_or(Error::CodingBadTable);
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::CodingBadTable)
    }
}

fn read_length_table(
    bits: &mut Bits<'_>,
    n_max: usize,
    n_bits: u32,
    special: usize,
) -> Result<Huffman> {
    let n = bits.bits(n_bits)? as usize;
    if n == 0 {
        return Ok(Huffman::single(bits.bits(n_bits)? as u16));
    }
    if n > n_max {
        return Err(Error::CodingBadTable);
    }
    let mut lengths = vec![0u8; n_max];
    let mut i = 0usize;
    while i < n {
        let mut c = bits.bits(3)?;
        if c == 7 {
            while bits.bit()? == 1 {
                c += 1;
                if c as usize > MAX_CODE_LEN {
                    return Err(Error::CodingBadTable);
                }
            }
        }
        *lengths.get_mut(i).ok_or(Error::CodingBadTable)? = c as u8;
        i += 1;
        if i == special {
            let mut skip = bits.bits(2)?;
            while skip > 0 {
                *lengths.get_mut(i).ok_or(Error::CodingBadTable)? = 0;
                i += 1;
                skip -= 1;
            }
        }
    }
    Huffman::build(&lengths)
}

fn read_main_table(bits: &mut Bits<'_>, lengths_of: &Huffman) -> Result<Huffman> {
    let n = bits.bits(CBIT)? as usize;
    if n == 0 {
        return Ok(Huffman::single(bits.bits(CBIT)? as u16));
    }
    if n > NC {
        return Err(Error::CodingBadTable);
    }
    let mut lengths = vec![0u8; NC];
    let mut i = 0usize;
    while i < n {
        let c = lengths_of.decode(bits)?;
        if c <= 2 {
            let mut run = match c {
                0 => 1u32,
                1 => bits.bits(4)? + 3,
                _ => bits.bits(CBIT)? + 20,
            };
            while run > 0 {
                *lengths.get_mut(i).ok_or(Error::CodingBadTable)? = 0;
                i += 1;
                run -= 1;
            }
        } else {
            *lengths.get_mut(i).ok_or(Error::CodingBadTable)? = (c - 2) as u8;
            i += 1;
        }
    }
    Huffman::build(&lengths)
}

/// 圧縮ストリームをコンテナが宣言した長さまで展開する。
pub fn decode(data: &[u8], expanded: usize) -> Result<Vec<u8>> {
    const CEILING: usize = 256 << 20;
    if expanded > CEILING {
        return Err(Error::CodingAbsurdLength { len: expanded });
    }
    let mut bits = Bits::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(expanded);
    let mut left = 0u32;
    let (mut main, mut dist) = (Huffman::default(), Huffman::default());

    while out.len() < expanded {
        if left == 0 {
            left = bits.bits(16)?;
            if left == 0 {
                return Err(Error::CodingBadTable);
            }
            let lengths_of = read_length_table(&mut bits, NT, TBIT, 3)?;
            main = read_main_table(&mut bits, &lengths_of)?;
            dist = read_length_table(&mut bits, NP, PBIT, usize::MAX)?;
        }
        left -= 1;
        let c = main.decode(&mut bits)? as usize;
        if c < 256 {
            out.push(c as u8);
        } else {
            let len = c - 256 + THRESHOLD;
            let mut d = dist.decode(&mut bits)? as usize;
            if d != 0 {
                if d > DICBIT + 1 {
                    return Err(Error::CodingBadTable);
                }
                d = (1 << (d - 1)) + bits.bits(d as u32 - 1)? as usize;
            }
            let start = out
                .len()
                .checked_sub(d + 1)
                .ok_or(Error::CodingBadDistance)?;
            for k in 0..len {
                if out.len() >= expanded {
                    break;
                }
                let b = out[start + k];
                out.push(b);
            }
        }
    }
    out.truncate(expanded);
    Ok(out)
}
