//! A small zlib compressor, enough to put bitmaps into PDF and PNG streams.
//!
//! Emits one deflate block with the fixed Huffman code, LZ77 matches found
//! through a hash chain. Not the tightest possible stream, but a page of
//! flat colour or a bilevel scan shrinks by a large factor, and nothing
//! outside the standard library is needed. The decoder in the tests exists
//! only to prove the encoder round-trips.

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 32;

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn new() -> Self {
        Bits {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    /// Deflate packs bits least-significant first.
    fn put(&mut self, value: u32, width: u32) {
        self.acc |= u64::from(value) << self.n;
        self.n += width;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes go out most-significant bit first.
    fn put_code(&mut self, code: u32, width: u32) {
        let mut rev = 0u32;
        for i in 0..width {
            rev |= ((code >> i) & 1) << (width - 1 - i);
        }
        self.put(rev, width);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// Literal/length symbol in the fixed code.
fn put_symbol(b: &mut Bits, sym: u32) {
    match sym {
        0..=143 => b.put_code(0x30 + sym, 8),
        144..=255 => b.put_code(0x190 + (sym - 144), 9),
        256..=279 => b.put_code(sym - 256, 7),
        _ => b.put_code(0xC0 + (sym - 280), 8),
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn put_length(b: &mut Bits, len: usize) {
    let i = LENGTH_BASE
        .iter()
        .rposition(|&base| usize::from(base) <= len)
        .unwrap_or(0);
    put_symbol(b, 257 + i as u32);
    let extra = LENGTH_EXTRA[i];
    if extra > 0 {
        b.put((len - usize::from(LENGTH_BASE[i])) as u32, u32::from(extra));
    }
}

fn put_distance(b: &mut Bits, dist: usize) {
    let i = DIST_BASE
        .iter()
        .rposition(|&base| usize::from(base) <= dist)
        .unwrap_or(0);
    b.put_code(i as u32, 5);
    let extra = DIST_EXTRA[i];
    if extra > 0 {
        b.put((dist - usize::from(DIST_BASE[i])) as u32, u32::from(extra));
    }
}

fn hash(d: &[u8], at: usize) -> usize {
    let v = (u32::from(d[at]) << 16) | (u32::from(d[at + 1]) << 8) | u32::from(d[at + 2]);
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Compress `data` into a zlib stream.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut b = Bits::new();
    // zlib header: deflate, 32K window, default level, no dictionary.
    b.put(0x78, 8);
    b.put(0x9C, 8);
    // One final block with fixed Huffman codes.
    b.put(1, 1);
    b.put(1, 2);

    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; data.len()];
    let mut i = 0usize;
    while i < data.len() {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH <= data.len() {
            let h = hash(data, i);
            let mut cand = head[h];
            let mut chain = 0usize;
            let limit = (data.len() - i).min(MAX_MATCH);
            while cand != usize::MAX && chain < MAX_CHAIN && i - cand <= WINDOW {
                if data[cand + best_len] == data[i + best_len] {
                    let mut len = 0usize;
                    while len < limit && data[cand + len] == data[i + len] {
                        len += 1;
                    }
                    if len > best_len {
                        best_len = len;
                        best_dist = i - cand;
                        if len == limit {
                            break;
                        }
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
            prev[i] = head[h];
            head[h] = i;
        }
        if best_len >= MIN_MATCH {
            put_length(&mut b, best_len);
            put_distance(&mut b, best_dist);
            // Enter the skipped positions into the chain so later matches can
            // start inside this one.
            for k in 1..best_len {
                let at = i + k;
                if at + MIN_MATCH <= data.len() {
                    let h = hash(data, at);
                    prev[at] = head[h];
                    head[h] = at;
                }
            }
            i += best_len;
        } else {
            put_symbol(&mut b, u32::from(data[i]));
            i += 1;
        }
    }
    put_symbol(&mut b, 256);
    let mut out = b.finish();
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// CRC-32 as PNG uses it.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader for the one kind of stream the encoder writes.
    fn inflate_fixed(z: &[u8]) -> Vec<u8> {
        struct In<'a> {
            d: &'a [u8],
            pos: usize,
        }
        impl In<'_> {
            fn bit(&mut self) -> u32 {
                let b = (self.d[self.pos >> 3] >> (self.pos & 7)) & 1;
                self.pos += 1;
                u32::from(b)
            }
            fn bits(&mut self, n: u32) -> u32 {
                (0..n).fold(0, |acc, i| acc | (self.bit() << i))
            }
            fn code(&mut self, n: u32) -> u32 {
                (0..n).fold(0, |acc, _| (acc << 1) | self.bit())
            }
        }
        let mut r = In { d: &z[2..], pos: 0 };
        assert_eq!(r.bits(1), 1, "final block");
        assert_eq!(r.bits(2), 1, "fixed code");
        let mut out = Vec::new();
        loop {
            // Fixed literal/length code: read 7 bits, extend as needed.
            let mut c = r.code(7);
            let sym = if c <= 23 {
                c + 256
            } else {
                c = (c << 1) | r.bit();
                if c <= 191 {
                    c - 48
                } else if c <= 199 {
                    c - 192 + 280
                } else {
                    ((c << 1) | r.bit()) - 400 + 144
                }
            };
            match sym {
                0..=255 => out.push(sym as u8),
                256 => break,
                _ => {
                    let i = (sym - 257) as usize;
                    let len =
                        usize::from(LENGTH_BASE[i]) + r.bits(u32::from(LENGTH_EXTRA[i])) as usize;
                    let di = r.code(5) as usize;
                    let dist =
                        usize::from(DIST_BASE[di]) + r.bits(u32::from(DIST_EXTRA[di])) as usize;
                    let start = out.len() - dist;
                    for k in 0..len {
                        out.push(out[start + k]);
                    }
                }
            }
        }
        out
    }

    fn round_trip(data: &[u8]) {
        let z = zlib(data);
        assert_eq!(&z[..2], &[0x78, 0x9C]);
        let back = inflate_fixed(&z[..z.len() - 4]);
        assert_eq!(back, data);
        let tail = &z[z.len() - 4..];
        assert_eq!(
            u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]),
            adler32(data)
        );
    }

    #[test]
    fn empty_and_tiny_inputs_round_trip() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"ab");
        round_trip(b"abc");
    }

    #[test]
    fn repeated_bytes_become_matches_and_come_back() {
        let data = vec![0xFFu8; 100_000];
        let z = zlib(&data);
        assert!(
            z.len() < 1000,
            "a flat page should shrink hard: {}",
            z.len()
        );
        round_trip(&data);
    }

    #[test]
    fn mixed_content_round_trips() {
        let mut data = Vec::new();
        for i in 0..20_000u32 {
            data.push((i.wrapping_mul(2654435761) >> 13) as u8);
            if i % 7 == 0 {
                data.extend_from_slice(b"the same phrase again and again");
            }
        }
        round_trip(&data);
    }

    #[test]
    fn matches_across_the_maximum_length_and_far_distances_round_trip() {
        let mut data: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        data.extend_from_slice(&data.clone()[..5000]);
        round_trip(&data);
    }

    #[test]
    fn the_checksums_match_the_standard_test_values() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
