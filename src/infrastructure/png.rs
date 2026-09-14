//! PNG encoding for the bitmaps a page carries, so an HTML page can show
//! them as data URLs.

use crate::domain::rendering::Raster;
use crate::infrastructure::deflate;

/// Encode a raster as a PNG file. Indexed rasters keep their palette and bit
/// depth; a stencil becomes a two-entry palette with the second transparent.
pub fn encode(r: &Raster) -> Vec<u8> {
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
    fn a_stencil_gets_a_transparent_second_entry() {
        let r = Raster {
            width: 8,
            height: 1,
            bits: 1,
            palette: vec![(0, 0, 0), (255, 255, 255)],
            rows: vec![0b1010_1010],
            stencil: Some((255, 0, 0)),
        };
        let png = encode(&r);
        let at = png.windows(4).position(|w| w == b"PLTE").expect("palette");
        assert_eq!(&png[at + 4..at + 10], &[255, 0, 0, 255, 255, 255]);
        assert!(png.windows(4).any(|w| w == b"tRNS"));
    }
}
