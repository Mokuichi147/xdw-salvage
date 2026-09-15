//! Coded-page recovery adapter.

use crate::application::ports::PageDecoder;
use crate::domain::page::{Overlay, Page, PageData};
use crate::domain::rendering::{Image, Metafile, RasterOp, Source};
use crate::infrastructure::{dib, emf, lzh, wmf};

const KIND_BITMAP: u64 = 7;

/// Decodes the XDW vendor stream and translates the metafile inside, in
/// either of its two flavours, into the domain drawing model.
#[derive(Debug, Clone, Copy, Default)]
pub struct LzhMetafileDecoder;

impl PageDecoder for LzhMetafileDecoder {
    fn decode(&self, data: &[u8], page: &Page) -> Option<Metafile> {
        let PageData::Encoded {
            offset,
            len,
            aux_len: Some(expanded),
            ..
        } = page.data
        else {
            return None;
        };
        let end = offset.checked_add(len)?;
        let expanded = usize::try_from(expanded).ok()?;
        let coded = data.get(offset..end)?;
        let raw = lzh::decode(coded, expanded).ok()?;
        let paper = page.paper.unwrap_or((21000, 29700));
        metafile(&raw, paper).filter(|metafile| !metafile.is_empty())
    }

    fn decode_overlay(&self, overlay: &Overlay, paper: (u32, u32)) -> Option<Metafile> {
        // A kind-7 drawing is a preview-style DIB.  Its 0x81 field is the
        // offset of the bitmap sub-header, not the expanded LZH length used
        // by ordinary metafile drawings.
        if overlay.kind == KIND_BITMAP {
            let frame = overlay.area.map(|(_, _, w, h)| (w, h)).unwrap_or(paper);
            return bitmap_overlay(overlay, frame);
        }
        let raw = lzh::decode(&overlay.coded, overlay.expanded).ok()?;
        // An annotation's frame is its own box; a page overlay's is the page.
        let frame = overlay.area.map(|(_, _, w, h)| (w, h)).unwrap_or(paper);
        metafile(&raw, frame).filter(|metafile| !metafile.is_empty())
    }
}

/// Decode a preview-style bitmap stored in a document-properties drawing.
fn bitmap_overlay(overlay: &Overlay, paper: (u32, u32)) -> Option<Metafile> {
    let info = &overlay.coded;
    let header = dib::header(info)?;
    let sub_at = overlay.expanded;
    if sub_at < header.info_len {
        return None;
    }
    let method = u32_le(info, sub_at)?;
    let stored = usize::try_from(u32_le(info, sub_at + 4)?).ok()?;
    let expanded = usize::try_from(u32_le(info, sub_at + 8)?).ok()?;
    let bits_at = sub_at.checked_add(16)?;
    let coded = info.get(bits_at..bits_at.checked_add(stored)?)?;
    let bits = match method {
        0 => coded.get(..expanded)?.to_vec(),
        1 => lzh::decode(coded, expanded).ok()?,
        _ => return None,
    };
    let raster = dib::decode(info.get(..sub_at)?, &bits)?;
    let (width, height) = (raster.width, raster.height);
    let mut meta = Metafile {
        device: (width as i32, height as i32),
        frame_mm100: (paper.0 as i32, paper.1 as i32),
        ..Default::default()
    };
    meta.rasters.push(raster);
    meta.images.push(Image {
        left: 0.0,
        top: 0.0,
        right: width as f32,
        bottom: height as f32,
        src: (width, height),
        source: Source::Inline(0),
        raster_op: RasterOp::Copy,
        order: 1,
        clip: None,
        clip_path: None,
    });
    Some(meta)
}

fn u32_le(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Read whichever metafile flavour the expanded bytes hold.
pub fn metafile(raw: &[u8], paper_mm100: (u32, u32)) -> Option<Metafile> {
    if let Some(m) = emf::read(raw) {
        return Some(m);
    }
    wmf::read(raw, (paper_mm100.0 as i32, paper_mm100.1 as i32))
}
