//! Windows device-independent bitmaps, as metafiles carry them, turned into
//! the domain's packed raster.
//!
//! Handles the headers and pixel formats actually seen inside page
//! metafiles: `BITMAPINFOHEADER` with 1, 4, 8, 16, 24 or 32 bits per pixel,
//! uncompressed or run-length coded (`BI_RLE4`, `BI_RLE8`).

use crate::domain::rendering::Raster;

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;

/// The largest bitmap this will expand, in pixels. A page at 600 dpi is
/// about 35 million; anything past that is a corrupt header, not a picture.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

/// The fields of a bitmap header this crate uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// Rows run bottom to top, the usual way.
    pub bottom_up: bool,
    pub bits: u16,
    pub compression: u32,
    /// Offset of the colour table from the start of the header.
    pub palette_at: usize,
    pub palette_entries: usize,
    /// Bytes the header and colour table take together.
    pub info_len: usize,
}

/// Read a `BITMAPINFOHEADER` (or a later version of it).
pub fn header(info: &[u8]) -> Option<Header> {
    let size = u32_at(info, 0)? as usize;
    if !(40..=256).contains(&size) {
        return None;
    }
    let width = u32_at(info, 4)? as i32;
    let height = u32_at(info, 8)? as i32;
    let bits = u16_at(info, 14)?;
    let compression = u32_at(info, 16)?;
    let clr_used = u32_at(info, 32)? as usize;
    if width <= 0 || height == 0 || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }
    let palette_entries = if bits <= 8 {
        let max = 1usize << bits;
        if clr_used == 0 {
            max
        } else {
            clr_used.min(max)
        }
    } else if compression == BI_BITFIELDS && size == 40 {
        // Three colour masks follow the header in place of a palette.
        3
    } else {
        0
    };
    Some(Header {
        width: width as u32,
        height: height.unsigned_abs(),
        bottom_up: height > 0,
        bits,
        compression,
        palette_at: size,
        palette_entries,
        info_len: size + palette_entries * 4,
    })
}

/// Turn a bitmap header plus its pixel data into a packed raster.
///
/// `info` starts at the `BITMAPINFOHEADER`; `bits` is the pixel data.
pub fn decode(info: &[u8], bits: &[u8]) -> Option<Raster> {
    let h = header(info)?;
    if u64::from(h.width) * u64::from(h.height) > MAX_PIXELS {
        return None;
    }
    let palette: Vec<(u8, u8, u8)> = if h.bits <= 8 {
        let mut p: Vec<(u8, u8, u8)> = (0..h.palette_entries)
            .filter_map(|i| {
                let at = h.palette_at + i * 4;
                let e = info.get(at..at + 4)?;
                Some((e[2], e[1], e[0]))
            })
            .collect();
        // A short table leaves the higher indices undefined; black is what a
        // zeroed table would have held.
        p.resize(1usize << h.bits, (0, 0, 0));
        p
    } else {
        Vec::new()
    };

    let (w, ht) = (h.width as usize, h.height as usize);
    match (h.compression, h.bits) {
        (BI_RGB, 1 | 4 | 8) => {
            let src_stride = (w * h.bits as usize).div_ceil(32) * 4;
            let dst_stride = (w * h.bits as usize).div_ceil(8);
            let mut rows = vec![0u8; dst_stride * ht];
            for y in 0..ht {
                let src_y = if h.bottom_up { ht - 1 - y } else { y };
                let Some(src) = bits.get(src_y * src_stride..src_y * src_stride + dst_stride)
                else {
                    break;
                };
                rows[y * dst_stride..(y + 1) * dst_stride].copy_from_slice(src);
            }
            Some(Raster {
                width: h.width,
                height: h.height,
                bits: h.bits as u8,
                palette,
                rows,
                stencil: None,
            })
        }
        (BI_RGB | BI_BITFIELDS, 16 | 24 | 32) => {
            let bpp = h.bits as usize / 8;
            let src_stride = (w * bpp).div_ceil(4) * 4;
            let mut rows = vec![0u8; w * 3 * ht];
            let masks = if h.compression == BI_BITFIELDS {
                let m = |i: usize| u32_at(info, h.palette_at + i * 4).unwrap_or(0);
                Some((m(0), m(1), m(2)))
            } else {
                None
            };
            for y in 0..ht {
                let src_y = if h.bottom_up { ht - 1 - y } else { y };
                let Some(src) = bits.get(src_y * src_stride..src_y * src_stride + w * bpp) else {
                    break;
                };
                let dst = &mut rows[y * w * 3..(y + 1) * w * 3];
                for x in 0..w {
                    let px = &src[x * bpp..(x + 1) * bpp];
                    let (r, g, b) = match (h.bits, masks) {
                        (16, m) => {
                            let v = u16::from_le_bytes([px[0], px[1]]) as u32;
                            let (mr, mg, mb) = m.unwrap_or((0x7C00, 0x03E0, 0x001F));
                            (channel(v, mr), channel(v, mg), channel(v, mb))
                        }
                        (32, Some((mr, mg, mb))) => {
                            let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                            (channel(v, mr), channel(v, mg), channel(v, mb))
                        }
                        _ => (px[2], px[1], px[0]),
                    };
                    dst[x * 3] = r;
                    dst[x * 3 + 1] = g;
                    dst[x * 3 + 2] = b;
                }
            }
            Some(Raster {
                width: h.width,
                height: h.height,
                bits: 24,
                palette,
                rows,
                stencil: None,
            })
        }
        (BI_RLE8, 8) | (BI_RLE4, 4) => {
            let indices = run_length(bits, w, ht, h.bits == 4)?;
            // Pack the expanded indices, flipping to top-down.
            let dst_stride = (w * h.bits as usize).div_ceil(8);
            let mut rows = vec![0u8; dst_stride * ht];
            for y in 0..ht {
                let src_y = if h.bottom_up { ht - 1 - y } else { y };
                let src = &indices[src_y * w..(src_y + 1) * w];
                let dst = &mut rows[y * dst_stride..(y + 1) * dst_stride];
                if h.bits == 8 {
                    dst.copy_from_slice(src);
                } else {
                    for (x, &v) in src.iter().enumerate() {
                        dst[x / 2] |= if x % 2 == 0 { v << 4 } else { v & 0x0F };
                    }
                }
            }
            Some(Raster {
                width: h.width,
                height: h.height,
                bits: h.bits as u8,
                palette,
                rows,
                stencil: None,
            })
        }
        _ => None,
    }
}

/// Scale one masked channel to eight bits.
fn channel(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let width = (mask >> shift).count_ones();
    let raw = (v & mask) >> shift;
    if width >= 8 {
        (raw >> (width - 8)) as u8
    } else {
        // Replicate the top bits into the low ones, as GDI does.
        ((raw << (8 - width)) | (raw >> (2 * width).saturating_sub(8))) as u8
    }
}

/// Expand `BI_RLE8` / `BI_RLE4` data into one index per pixel, rows in the
/// order stored (bottom-up).
///
/// Pixels the stream never writes stay at index 0, which is what a zeroed
/// GDI surface would show.
fn run_length(src: &[u8], w: usize, h: usize, nibbles: bool) -> Option<Vec<u8>> {
    let mut out = vec![0u8; w * h];
    let (mut x, mut y) = (0usize, 0usize);
    let mut i = 0usize;
    let mut put = |x: &mut usize, y: usize, v: u8| {
        if *x < w && y < h {
            out[y * w + *x] = v;
        }
        *x += 1;
    };
    while i + 1 < src.len() {
        let count = src[i] as usize;
        let value = src[i + 1];
        i += 2;
        if count > 0 {
            for k in 0..count {
                let v = if nibbles {
                    if k % 2 == 0 {
                        value >> 4
                    } else {
                        value & 0x0F
                    }
                } else {
                    value
                };
                put(&mut x, y, v);
            }
            continue;
        }
        match value {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                let dx = *src.get(i)? as usize;
                let dy = *src.get(i + 1)? as usize;
                i += 2;
                x += dx;
                y += dy;
            }
            n => {
                let n = n as usize;
                let bytes = if nibbles { n.div_ceil(2) } else { n };
                let run = src.get(i..i + bytes)?;
                for k in 0..n {
                    let v = if nibbles {
                        if k % 2 == 0 {
                            run[k / 2] >> 4
                        } else {
                            run[k / 2] & 0x0F
                        }
                    } else {
                        run[k]
                    };
                    put(&mut x, y, v);
                }
                // Each absolute run is padded to a 16-bit boundary.
                i += bytes.div_ceil(2) * 2;
            }
        }
        if y >= h {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        width: i32,
        height: i32,
        bits: u16,
        compression: u32,
        palette: &[(u8, u8, u8)],
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&width.to_le_bytes());
        v.extend_from_slice(&height.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(&compression.to_le_bytes());
        v.extend_from_slice(&[0u8; 12]);
        v.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        for (r, g, b) in palette {
            v.extend_from_slice(&[*b, *g, *r, 0]);
        }
        v
    }

    #[test]
    fn a_bottom_up_one_bit_image_comes_out_top_down_and_unpadded() {
        let i = info(3, 2, 1, BI_RGB, &[(0, 0, 0), (255, 255, 255)]);
        // Two rows, each padded to four bytes; the stored first row is the bottom.
        let bits = [0b1010_0000, 0, 0, 0, 0b0100_0000, 0, 0, 0];
        let r = decode(&i, &bits).expect("decodes");
        assert_eq!((r.width, r.height, r.bits), (3, 2, 1));
        assert_eq!(r.rows, vec![0b0100_0000, 0b1010_0000]);
        assert!(r.is_bilevel());
    }

    #[test]
    fn a_short_palette_is_padded_rather_than_indexed_past_its_end() {
        let i = info(1, 1, 1, BI_RGB, &[(0, 0, 0)]);
        let r = decode(&i, &[0x80, 0, 0, 0]).expect("decodes");
        assert_eq!(r.palette.len(), 2);
    }

    #[test]
    fn twenty_four_bit_pixels_are_reordered_to_red_green_blue() {
        let i = info(1, 1, 24, BI_RGB, &[]);
        let r = decode(&i, &[10, 20, 30, 0]).expect("decodes");
        assert_eq!(r.rows, vec![30, 20, 10]);
        assert_eq!(r.bits, 24);
    }

    #[test]
    fn run_length_eight_expands_runs_escapes_and_deltas() {
        let i = info(
            5,
            2,
            8,
            BI_RLE8,
            &[(0, 0, 0), (1, 1, 1), (2, 2, 2), (3, 3, 3)],
        );
        // Bottom row: 2 x index 1, then an absolute run of three index 2
        // (padded to a word), end of line. Top row: a delta skips two pixels,
        // then 3 x index 3, end of bitmap.
        let bits = [2, 1, 0, 3, 2, 2, 2, 0, 0, 0, 0, 2, 2, 0, 3, 3, 0, 1];
        let r = decode(&i, &bits).expect("decodes");
        assert_eq!(r.rows, vec![0, 0, 3, 3, 3, 1, 1, 2, 2, 2]);
    }

    #[test]
    fn run_length_four_alternates_nibbles() {
        let i = info(4, 1, 4, BI_RLE4, &[(0, 0, 0); 16]);
        let bits = [4, 0x12, 0, 1];
        let r = decode(&i, &bits).expect("decodes");
        assert_eq!(r.rows, vec![0x12, 0x12]);
    }

    #[test]
    fn a_header_with_an_absurd_size_is_refused() {
        let i = info(100_000, 100_000, 8, BI_RGB, &[]);
        assert!(decode(&i, &[]).is_none());
    }
}
