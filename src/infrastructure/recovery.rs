//! Coded-page recovery adapter.

use crate::application::ports::PageDecoder;
use crate::domain::page::{Overlay, Page, PageData};
use crate::domain::rendering::Metafile;
use crate::infrastructure::{emf, lzh, wmf};

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
        let raw = lzh::decode(&overlay.coded, overlay.expanded).ok()?;
        // An annotation's frame is its own box; a page overlay's is the page.
        let frame = overlay.area.map(|(_, _, w, h)| (w, h)).unwrap_or(paper);
        metafile(&raw, frame).filter(|metafile| !metafile.is_empty())
    }
}

/// Read whichever metafile flavour the expanded bytes hold.
pub fn metafile(raw: &[u8], paper_mm100: (u32, u32)) -> Option<Metafile> {
    if let Some(m) = emf::read(raw) {
        return Some(m);
    }
    wmf::read(raw, (paper_mm100.0 as i32, paper_mm100.1 as i32))
}
