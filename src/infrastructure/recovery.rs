//! Coded-page recovery adapter.

use crate::application::ports::PageDecoder;
use crate::domain::page::{Page, PageData};
use crate::domain::rendering::Metafile;
use crate::infrastructure::{emf, lzh};

/// Decodes the XDW vendor stream and translates its EMF payload into the
/// domain drawing model.
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
        emf::read(&raw).filter(|metafile| !metafile.is_empty())
    }
}
