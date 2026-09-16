//! Coded-page recovery adapter.

use crate::application::ports::PageDecoder;
use crate::domain::page::{Overlay, Page, PageData};
use crate::domain::rendering::{Image, Metafile, Raster, RasterOp, Source};
use crate::domain::Document;
use crate::infrastructure::{ccitt, dib, emf, lzh, wmf};

const KIND_BITMAP: u64 = 7;
const KIND_GROUP4: u64 = 9;

/// Decodes the XDW vendor stream and translates the metafile inside, in
/// either of its two flavours, into the domain drawing model.
#[derive(Debug, Clone, Copy, Default)]
pub struct LzhMetafileDecoder;

impl PageDecoder for LzhMetafileDecoder {
    fn decode(&self, data: &[u8], page: &Page) -> Option<Metafile> {
        if matches!(page.data, PageData::Preview { .. }) {
            return preview_metafile(data, page);
        }
        let PageData::Encoded {
            offset,
            len,
            kind_code,
            aux_len,
            ..
        } = page.data
        else {
            return None;
        };
        let end = offset.checked_add(len)?;
        let coded = data.get(offset..end)?;
        if kind_code == KIND_GROUP4 {
            let (width, height) = page.pixels?;
            let raster = ccitt::decode(coded, width, height)?;
            return Some(raster_metafile(
                raster,
                page.paper.unwrap_or((21000, 29700)),
            ));
        }
        let expanded = usize::try_from(aux_len?).ok()?;
        let raw = lzh::decode(coded, expanded).ok()?;
        let paper = page.paper.unwrap_or((21000, 29700));
        metafile(&raw, paper).filter(|metafile| !metafile.is_empty())
    }

    fn decode_with_document(
        &self,
        data: &[u8],
        page: &Page,
        document: &Document,
    ) -> Option<Metafile> {
        let high = self.decode(data, page).filter(|meta| !meta.is_empty());
        if page.is_full_size_preview() {
            let thumbnail = document.pages.iter().find(|candidate| {
                candidate.role == crate::domain::page::Role::Thumbnail
                    && candidate.belongs_to == Some(page.index)
                    && matches!(candidate.data, PageData::Preview { .. })
            });
            if let Some(thumbnail) = thumbnail {
                if let Some(base) = self.decode(data, thumbnail).filter(|meta| !meta.is_empty()) {
                    return Some(combine_full_preview(page, base, high));
                }
            }
        }
        high
    }

    fn decode_overlay(&self, overlay: &Overlay, paper: (u32, u32)) -> Option<Metafile> {
        if overlay.kind == KIND_GROUP4 {
            let (width, height) = overlay.pixels?;
            let raster = ccitt::decode(&overlay.coded, width, height)?;
            let frame = overlay.area.map(|(_, _, w, h)| (w, h)).unwrap_or(paper);
            return Some(raster_metafile(raster, frame));
        }
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

fn raster_metafile(raster: Raster, paper: (u32, u32)) -> Metafile {
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
    meta
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

/// Decode a page-table preview.  Methods 0 and 1 are the ordinary raw and LZH
/// DIB forms.  Method 5 is an older large-preview form: after LZH expansion it
/// contains DIB rows with the normal four-byte row padding, but only the rows
/// declared in the preview sub-header.
fn preview_metafile(data: &[u8], page: &Page) -> Option<Metafile> {
    let PageData::Preview {
        offset,
        len,
        pixels_at,
        stored,
        expanded,
        rows,
        method,
        ..
    } = page.data
    else {
        return None;
    };
    let image = data.get(offset..offset.checked_add(len)?)?;
    let info = image.get(..pixels_at)?;
    let bits_at = pixels_at.checked_add(16)?;
    let coded = image.get(bits_at..bits_at.checked_add(stored as usize)?)?;
    let bits = match method {
        0 => coded.get(..expanded as usize)?.to_vec(),
        1 | 5 => lzh::decode(coded, expanded as usize).ok()?,
        _ => return None,
    };
    let raster = if method == 5 {
        method5_raster(info, &bits, rows)?
    } else {
        dib::decode(info, &bits)?
    };
    Some(raster_metafile(
        raster,
        page.paper.unwrap_or((21000, 29700)),
    ))
}

fn method5_raster(info: &[u8], bits: &[u8], rows: u32) -> Option<Raster> {
    let header = dib::header(info)?;
    if header.bits != 1 || header.compression != 0 || rows == 0 || rows > header.height {
        return None;
    }
    let width = usize::try_from(header.width).ok()?;
    let rows = usize::try_from(rows).ok()?;
    let source_stride = (width * usize::from(header.bits)).div_ceil(32) * 4;
    let expected = source_stride.checked_mul(rows)?;
    if bits.len() != expected {
        return None;
    }
    // The method-5 rows are written top-down even though their DIB header is
    // the ordinary bottom-up header.  Ask the existing DIB decoder for a
    // temporary top-down height so it also supplies the original palette.
    let mut short_info = info.to_vec();
    let short_height = -(i32::try_from(rows).ok()?);
    short_info
        .get_mut(8..12)?
        .copy_from_slice(&short_height.to_le_bytes());
    dib::decode(&short_info, bits)
}

/// Make a complete page from the full-page thumbnail, then paint the
/// high-resolution method-5 band over its matching upper edge when present.
fn combine_full_preview(page: &Page, base: Metafile, high: Option<Metafile>) -> Metafile {
    let paper = page.paper.unwrap_or((21000, 29700));
    let (width, height) = page.pixels.unwrap_or((0, 0));
    let Some(base_raster) = base.rasters.into_iter().next() else {
        return high.unwrap_or_default();
    };
    let mut result = raster_metafile(base_raster, paper);
    result.device = (width as i32, height as i32);
    result.frame_mm100 = (paper.0 as i32, paper.1 as i32);
    result.images[0].right = width as f32;
    result.images[0].bottom = height as f32;
    if let Some(high_raster) = high.and_then(|m| m.rasters.into_iter().next()) {
        let high_height = high_raster.height;
        if high_raster.width == width && high_height < height {
            let index = result.rasters.len();
            result.rasters.push(high_raster);
            result.images.push(Image {
                left: 0.0,
                top: 0.0,
                right: width as f32,
                bottom: high_height as f32,
                src: (width, high_height),
                source: Source::Inline(index),
                raster_op: RasterOp::Copy,
                order: 1,
                clip: None,
                clip_path: None,
            });
        }
    }
    result
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
