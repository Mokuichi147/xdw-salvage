//! CCITT Group 4 (T.6/MMR) bitmap decoding.
//!
//! Some older DocuWorks image pages store a one-bit page image as a bare
//! Group 4 bit stream.  The stream has no TIFF/PDF wrapper; the image size is
//! carried by the surrounding XDW page fields.  This module decodes that
//! wrapper-less form into the packed raster used by the renderers.

use crate::domain::rendering::Raster;

const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Decode a wrapper-less CCITT T.6 (Group 4/MMR) stream.
///
/// The stream is MSB-first, white is zero and black is one, as required by
/// the XDW image pages handled here.  T.6 has no per-row EOL; decoding stops
/// after the declared number of rows and any trailing fill/EOFB is ignored.
pub fn decode(data: &[u8], width: u32, height: u32) -> Option<Raster> {
    decode_inner(data, width, height, false)
}

/// Decode a Group 4 stream found in an old full-size XDW preview.
///
/// Those streams use the optional T.6 uncompressed extension and a few
/// writers emit a changing element just beyond the declared right edge.  The
/// ordinary page codec remains strict; this entry point only clamps those
/// edge conditions after the stream has otherwise decoded successfully.
pub fn decode_relaxed(data: &[u8], width: u32, height: u32) -> Option<Raster> {
    decode_inner(data, width, height, true)
}

fn decode_inner(data: &[u8], width: u32, height: u32, relaxed: bool) -> Option<Raster> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let stride = width.div_ceil(8);
    let mut rows = vec![0; stride.checked_mul(height)?];
    let mut reader = BitReader::new(data);
    let mut reference = Vec::new();

    for y in 0..height {
        let changes = decode_row(&mut reader, width, &reference, relaxed)?;
        paint_row(&mut rows[y * stride..(y + 1) * stride], width, &changes)?;
        reference = changes;
    }

    Some(Raster {
        width: width as u32,
        height: height as u32,
        bits: 1,
        palette: vec![(255, 255, 255), (0, 0, 0)],
        rows,
        stencil: None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pass,
    Horizontal,
    Vertical(i32),
    Uncompressed,
}

/// Decode one two-dimensional T.6 row and return its changing elements.
fn decode_row(
    reader: &mut BitReader<'_>,
    width: usize,
    reference: &[usize],
    relaxed: bool,
) -> Option<Vec<usize>> {
    let mut changes = Vec::new();
    let mut a0 = 0usize;
    let mut white = true;

    while a0 < width {
        let mode = read_mode(reader)?;
        match mode {
            Mode::Pass => {
                let (_, b2) = reference_pair(reference, a0, white, width);
                if b2 <= a0 {
                    return None;
                }
                if !relaxed && b2 > width {
                    return None;
                }
                if relaxed && b2 >= width {
                    push_relaxed_change(&mut changes, b2, width)?;
                }
                a0 = b2.min(width);
            }
            Mode::Horizontal => {
                let first = decode_run(reader, !white)?;
                let second = decode_run(reader, white)?;
                let a1 = a0.checked_add(first)?;
                let a2 = a1.checked_add(second)?;
                if a1 < a0 || a2 < a1 {
                    return None;
                }
                if !relaxed && a2 > width {
                    return None;
                }
                if a1 >= width || a2 >= width {
                    if relaxed {
                        push_relaxed_change(&mut changes, a1, width)?;
                        push_relaxed_change(&mut changes, a2, width)?;
                    } else if a1 < width {
                        changes.push(a1);
                    }
                    a0 = width;
                    continue;
                }
                changes.push(a1);
                changes.push(a2);
                a0 = a2;
            }
            Mode::Vertical(delta) => {
                let (b1, _) = reference_pair(reference, a0, white, width);
                let a1 = i64::try_from(b1).ok()?.checked_add(i64::from(delta))?;
                if a1 < i64::try_from(a0).ok()? {
                    return None;
                }
                if !relaxed && a1 > i64::try_from(width).ok()? {
                    return None;
                }
                let a1 = usize::try_from(a1).ok()?;
                if a1 >= width {
                    if relaxed {
                        push_relaxed_change(&mut changes, a1, width)?;
                    }
                    a0 = width;
                    continue;
                }
                changes.push(a1);
                a0 = a1;
                white = !white;
            }
            Mode::Uncompressed => {
                (a0, white) = decode_uncompressed(reader, a0, white, width, &mut changes, relaxed)?;
            }
        }
    }

    if changes.last().is_none_or(|&last| last < width) {
        changes.push(width);
    }
    Some(changes)
}

/// Return b1/b2, the first reference changes of the current colour phase.
///
/// Reference rows begin with white.  Consequently white runs use even
/// changing-element indices and black runs use odd indices.  A missing
/// reference change is the imaginary change at the right edge of the row.
fn reference_pair(reference: &[usize], a0: usize, white: bool, width: usize) -> (usize, usize) {
    let parity = usize::from(!white);
    let edge = width.max(reference.last().copied().unwrap_or(width));
    let mut i = parity;
    while i < reference.len() {
        if a0 == 0 || reference[i] > a0 {
            let b1 = reference[i];
            let b2 = reference.get(i + 1).copied().unwrap_or(edge);
            return (b1, b2);
        }
        i += 2;
    }
    (edge, edge)
}

fn read_mode(reader: &mut BitReader<'_>) -> Option<Mode> {
    let mut code = 0u16;
    for len in 1..=10u8 {
        code = (code << 1) | u16::from(reader.bit()?);
        let mode = match (len, code) {
            (1, 0b1) => Mode::Vertical(0),
            (3, 0b001) => Mode::Horizontal,
            (3, 0b010) => Mode::Vertical(-1),
            (3, 0b011) => Mode::Vertical(1),
            (4, 0b0001) => Mode::Pass,
            (6, 0b000010) => Mode::Vertical(-2),
            (6, 0b000011) => Mode::Vertical(2),
            (7, 0b0000010) => Mode::Vertical(-3),
            (7, 0b0000011) => Mode::Vertical(3),
            // T.6 extension 0000001xxx, with xxx=111, enters the optional
            // uncompressed mode.  The complete code word is 0000001111.
            (10, 0b0000001111) => Mode::Uncompressed,
            _ => continue,
        };
        return Some(mode);
    }
    None
}

/// Read T.6's optional uncompressed-mode image patterns and return to the
/// two-dimensional mode named by its exit tag.
///
/// In uncompressed mode five image zeroes are represented by `000001`: the
/// final one is a stuffing bit.  An exit code starts with six to ten zeroes,
/// then a one and a tag bit.  The tag names the colour of the next compressed
/// run (black=1, white=0).
fn decode_uncompressed(
    reader: &mut BitReader<'_>,
    mut at: usize,
    mut white: bool,
    width: usize,
    changes: &mut Vec<usize>,
    relaxed: bool,
) -> Option<(usize, bool)> {
    if at > width {
        return None;
    }
    loop {
        let mut zeroes = 0usize;
        while !reader.bit()? {
            zeroes = zeroes.checked_add(1)?;
            if zeroes > 10 {
                return None;
            }
        }

        if zeroes >= 6 {
            let next_black = reader.bit()?;
            append_uncompressed_run(
                &mut at,
                &mut white,
                true,
                zeroes - 6,
                width,
                changes,
                relaxed,
            )?;
            let next_white = !next_black;
            if white != next_white && at < width {
                changes.push(at);
            }
            white = next_white;
            return Some((at.min(width), white));
        }

        if zeroes == 5 {
            // The terminal one is only a stuffing bit; the image pattern is
            // five literal white pixels and the next code continues the run.
            append_uncompressed_run(&mut at, &mut white, true, 5, width, changes, relaxed)?;
        } else {
            append_uncompressed_run(&mut at, &mut white, true, zeroes, width, changes, relaxed)?;
            append_uncompressed_run(&mut at, &mut white, false, 1, width, changes, relaxed)?;
        }

        if at >= width && !relaxed {
            return Some((width, white));
        }
    }
}

fn append_uncompressed_run(
    at: &mut usize,
    white: &mut bool,
    run_white: bool,
    count: usize,
    width: usize,
    changes: &mut Vec<usize>,
    relaxed: bool,
) -> Option<()> {
    if *at > width {
        return if relaxed { Some(()) } else { None };
    }
    if !relaxed && count > width - *at {
        return None;
    }
    if *white != run_white && *at < width {
        changes.push(*at);
    }
    *white = run_white;
    *at = (*at).checked_add(count)?.min(width);
    Some(())
}

fn push_relaxed_change(changes: &mut Vec<usize>, position: usize, width: usize) -> Option<()> {
    let limit = width.checked_add(64)?;
    if position > limit {
        return None;
    }
    changes.push(position);
    Some(())
}

fn decode_run(reader: &mut BitReader<'_>, black: bool) -> Option<usize> {
    let mut total = 0usize;
    loop {
        let run = read_run_code(reader, black)?;
        total = total.checked_add(run)?;
        if run < 64 {
            return Some(total);
        }
    }
}

fn read_run_code(reader: &mut BitReader<'_>, black: bool) -> Option<usize> {
    let table = if black { BLACK_CODES } else { WHITE_CODES };
    let start_len: u8 = if black { 2 } else { 4 };
    let end_len: u8 = if black { 13 } else { 12 };
    let mut code = 0u16;
    for len in 1..=end_len {
        code = (code << 1) | u16::from(reader.bit()?);
        if len < start_len {
            continue;
        }
        if let Some(run) = table
            .get(usize::from(len - start_len))
            .and_then(|entries| entries.iter().find(|(bits, _)| *bits == code))
            .map(|(_, run)| *run)
        {
            return Some(run);
        }
    }
    None
}

/// The standard terminating and makeup code words.  Values >= 64 are makeup
/// words and are followed by another word of the same colour.
const BLACK_CODES: &[&[(u16, usize)]] = &[
    &[(0x2, 3), (0x3, 2)],
    &[(0x2, 1), (0x3, 4)],
    &[(0x2, 6), (0x3, 5)],
    &[(0x3, 7)],
    &[(0x4, 9), (0x5, 8)],
    &[(0x4, 10), (0x5, 11), (0x7, 12)],
    &[(0x4, 13), (0x7, 14)],
    &[(0x18, 15)],
    &[(0x17, 16), (0x18, 17), (0x37, 0), (0x8, 18), (0xf, 64)],
    &[
        (0x17, 24),
        (0x18, 25),
        (0x28, 23),
        (0x37, 22),
        (0x67, 19),
        (0x68, 20),
        (0x6c, 21),
        (0x8, 1792),
        (0xc, 1856),
        (0xd, 1920),
    ],
    &[
        (0x12, 1984),
        (0x13, 2048),
        (0x14, 2112),
        (0x15, 2176),
        (0x16, 2240),
        (0x17, 2304),
        (0x1c, 2368),
        (0x1d, 2432),
        (0x1e, 2496),
        (0x1f, 2560),
        (0x24, 52),
        (0x27, 55),
        (0x28, 56),
        (0x2b, 59),
        (0x2c, 60),
        (0x33, 320),
        (0x34, 384),
        (0x35, 448),
        (0x37, 53),
        (0x38, 54),
        (0x52, 50),
        (0x53, 51),
        (0x54, 44),
        (0x55, 45),
        (0x56, 46),
        (0x57, 47),
        (0x58, 57),
        (0x59, 58),
        (0x5a, 61),
        (0x5b, 256),
        (0x64, 48),
        (0x65, 49),
        (0x66, 62),
        (0x67, 63),
        (0x68, 30),
        (0x69, 31),
        (0x6a, 32),
        (0x6b, 33),
        (0x6c, 40),
        (0x6d, 41),
        (0xc8, 128),
        (0xc9, 192),
        (0xca, 26),
        (0xcb, 27),
        (0xcc, 28),
        (0xcd, 29),
        (0xd2, 34),
        (0xd3, 35),
        (0xd4, 36),
        (0xd5, 37),
        (0xd6, 38),
        (0xd7, 39),
        (0xda, 42),
        (0xdb, 43),
    ],
    &[
        (0x4a, 640),
        (0x4b, 704),
        (0x4c, 768),
        (0x4d, 832),
        (0x52, 1280),
        (0x53, 1344),
        (0x54, 1408),
        (0x55, 1472),
        (0x5a, 1536),
        (0x5b, 1600),
        (0x64, 1664),
        (0x65, 1728),
        (0x6c, 512),
        (0x6d, 576),
        (0x72, 896),
        (0x73, 960),
        (0x74, 1024),
        (0x75, 1088),
        (0x76, 1152),
        (0x77, 1216),
    ],
];

const WHITE_CODES: &[&[(u16, usize)]] = &[
    &[(0x7, 2), (0x8, 3), (0xb, 4), (0xc, 5), (0xe, 6), (0xf, 7)],
    &[
        (0x12, 128),
        (0x13, 8),
        (0x14, 9),
        (0x1b, 64),
        (0x7, 10),
        (0x8, 11),
    ],
    &[
        (0x17, 192),
        (0x18, 1664),
        (0x2a, 16),
        (0x2b, 17),
        (0x3, 13),
        (0x34, 14),
        (0x35, 15),
        (0x7, 1),
        (0x8, 12),
    ],
    &[
        (0x13, 26),
        (0x17, 21),
        (0x18, 28),
        (0x24, 27),
        (0x27, 18),
        (0x28, 24),
        (0x2b, 25),
        (0x3, 22),
        (0x37, 256),
        (0x4, 23),
        (0x8, 20),
        (0xc, 19),
    ],
    &[
        (0x12, 33),
        (0x13, 34),
        (0x14, 35),
        (0x15, 36),
        (0x16, 37),
        (0x17, 38),
        (0x1a, 31),
        (0x1b, 32),
        (0x2, 29),
        (0x24, 53),
        (0x25, 54),
        (0x28, 39),
        (0x29, 40),
        (0x2a, 41),
        (0x2b, 42),
        (0x2c, 43),
        (0x2d, 44),
        (0x3, 30),
        (0x32, 61),
        (0x33, 62),
        (0x34, 63),
        (0x35, 0),
        (0x36, 320),
        (0x37, 384),
        (0x4, 45),
        (0x4a, 59),
        (0x4b, 60),
        (0x5, 46),
        (0x52, 49),
        (0x53, 50),
        (0x54, 51),
        (0x55, 52),
        (0x58, 55),
        (0x59, 56),
        (0x5a, 57),
        (0x5b, 58),
        (0x64, 448),
        (0x65, 512),
        (0x67, 640),
        (0x68, 576),
        (0xa, 47),
        (0xb, 48),
    ],
    &[
        (0x98, 1472),
        (0x99, 1536),
        (0x9a, 1600),
        (0x9b, 1728),
        (0xcc, 704),
        (0xcd, 768),
        (0xd2, 832),
        (0xd3, 896),
        (0xd4, 960),
        (0xd5, 1024),
        (0xd6, 1088),
        (0xd7, 1152),
        (0xd8, 1216),
        (0xd9, 1280),
        (0xda, 1344),
        (0xdb, 1408),
    ],
    &[],
    &[(0x8, 1792), (0xc, 1856), (0xd, 1920)],
    &[
        (0x12, 1984),
        (0x13, 2048),
        (0x14, 2112),
        (0x15, 2176),
        (0x16, 2240),
        (0x17, 2304),
        (0x1c, 2368),
        (0x1d, 2432),
        (0x1e, 2496),
        (0x1f, 2560),
    ],
];

fn paint_row(row: &mut [u8], width: usize, changes: &[usize]) -> Option<()> {
    let mut start = 0usize;
    let mut white = true;
    for &end in changes.iter().chain(std::iter::once(&width)) {
        if end < start {
            return None;
        }
        if end > width {
            if !white {
                paint_black(row, start, width);
            }
            return Some(());
        }
        if !white {
            paint_black(row, start, end);
        }
        start = end;
        white = !white;
    }
    Some(())
}

fn paint_black(row: &mut [u8], mut start: usize, end: usize) {
    while start < end && !start.is_multiple_of(8) {
        row[start / 8] |= 1 << (7 - start % 8);
        start += 1;
    }
    while start + 8 <= end {
        row[start / 8] = 0xFF;
        start += 8;
    }
    while start < end {
        row[start / 8] |= 1 << (7 - start % 8);
        start += 1;
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn bit(&mut self) -> Option<bool> {
        let byte = *self.data.get(self.at / 8)?;
        let bit = byte & (0x80 >> (self.at % 8)) != 0;
        self.at += 1;
        Some(bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = 0u8;
        for (i, bit) in s.bytes().filter(|b| *b == b'0' || *b == b'1').enumerate() {
            byte = (byte << 1) | u8::from(bit == b'1');
            if i % 8 == 7 {
                out.push(byte);
                byte = 0;
            }
        }
        let rem = s.bytes().filter(|b| *b == b'0' || *b == b'1').count() % 8;
        if rem != 0 {
            out.push(byte << (8 - rem));
        }
        out
    }

    #[test]
    fn decodes_all_white_rows_against_an_imaginary_reference() {
        let raster = decode(&[0xFF], 8, 8).expect("all-white Group 4");
        assert_eq!(raster.rows, vec![0; 8]);
    }

    #[test]
    fn decodes_horizontal_black_run_and_vertical_repeat() {
        // First row: white 2, black 4, white 2 (the final white edge is
        // represented by V(0) against the imaginary change at the width).
        // The second row repeats all three changing elements with V(0).
        let stream = bits("001 0111 011 1 1 1 1");
        let raster = decode(&stream, 8, 2).expect("synthetic Group 4");
        assert_eq!(raster.rows, vec![0x3C, 0x3C]);
    }
}
