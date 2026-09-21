//! PNG encoding for the bitmaps a page carries, so an HTML page can show
//! them as data URLs.

use crate::domain::rendering::Raster;
use crate::infrastructure::deflate;

/// Encode a raster as a PNG file. Indexed rasters keep their palette and bit
/// depth; a stencil is expanded to RGBA so its transparency is explicit even
/// when the PNG is used as an image inside SVG.
pub fn encode(r: &Raster) -> Vec<u8> {
    if r.stencil.is_some() {
        return encode_stencil_rgba(r);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let (depth, colour_type) = if r.bits == 24 {
        (8u8, 2u8)
    } else {
        (r.bits, 3u8)
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&r.width.to_be_bytes());
    ihdr.extend_from_slice(&r.height.to_be_bytes());
    ihdr.extend_from_slice(&[depth, colour_type, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    if colour_type == 3 {
        let mut plte = Vec::new();
        match r.stencil {
            Some((cr, cg, cb)) => {
                plte.extend_from_slice(&[cr, cg, cb, 255, 255, 255]);
            }
            None => {
                for (cr, cg, cb) in &r.palette {
                    plte.extend_from_slice(&[*cr, *cg, *cb]);
                }
            }
        }
        chunk(&mut out, b"PLTE", &plte);
        if r.stencil.is_some() {
            chunk(&mut out, b"tRNS", &[255, 0]);
        }
    }

    // Every row takes a filter byte; none is the simplest and the zlib layer
    // does the compressing.
    let stride = r.stride();
    let mut raw = Vec::with_capacity(r.rows.len() + r.height as usize);
    for row in r.rows.chunks(stride.max(1)) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    chunk(&mut out, b"IDAT", &deflate::zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn encode_stencil_rgba(r: &Raster) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&r.width.to_be_bytes());
    ihdr.extend_from_slice(&r.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    let stride = r.stride();
    let Some(colour) = r.stencil else {
        return out;
    };
    let width = r.width as usize;
    let height = r.height as usize;
    let mut raw = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4) + height);
    for y in 0..height {
        raw.push(0);
        let row = r
            .rows
            .get(y.saturating_mul(stride)..y.saturating_mul(stride).saturating_add(stride));
        for x in 0..width {
            let bit = row
                .and_then(|row| row.get(x / 8))
                .map(|byte| (byte >> (7 - x % 8)) & 1)
                .unwrap_or(1);
            let alpha = if bit == 0 { 255 } else { 0 };
            raw.extend_from_slice(&[colour.0, colour.1, colour.2, alpha]);
        }
    }
    chunk(&mut out, b"IDAT", &deflate::zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let crc = deflate::crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_indexed_raster_becomes_a_well_formed_png() {
        let r = Raster {
            width: 2,
            height: 1,
            bits: 8,
            palette: vec![(1, 2, 3), (4, 5, 6)],
            rows: vec![0, 1],
            stencil: None,
        };
        let png = encode(&r);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
        assert_eq!(&png[24..29], &[8, 3, 0, 0, 0]);
        assert!(png.windows(4).any(|w| w == b"PLTE"));
        assert!(!png.windows(4).any(|w| w == b"tRNS"));
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn a_stencil_becomes_explicit_rgba() {
        let r = Raster {
            width: 8,
            height: 1,
            bits: 1,
            palette: vec![(0, 0, 0), (255, 255, 255)],
            rows: vec![0b1010_1010],
            stencil: Some((255, 0, 0)),
        };
        let png = encode(&r);
        assert_eq!(&png[24..29], &[8, 6, 0, 0, 0]);
        assert!(!png.windows(4).any(|w| w == b"PLTE"));
        assert!(!png.windows(4).any(|w| w == b"tRNS"));
    }
}
