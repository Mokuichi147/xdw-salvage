//! Recovery-oriented use cases.

use crate::application::ports::PageDecoder;
use crate::domain::coverage::Coverage;
use crate::domain::page::Page;
use crate::domain::rendering::Metafile;
use crate::domain::Document;

/// Decode one page through the supplied port.
///
/// Empty drawing models are treated as a failed recovery.  This is the same
/// business rule used by both the PDF and HTML adapters, so they cannot drift
/// apart and report different page counts for the same source.
pub fn decode_page<D: PageDecoder + ?Sized>(
    data: &[u8],
    page: &Page,
    decoder: &D,
) -> Option<Metafile> {
    decoder.decode(data, page).filter(|page| !page.is_empty())
}

/// Calculate coverage using the structural facts in `Document` and the
/// injected page decoder for pages held in a coded representation.
pub fn coverage<D: PageDecoder + ?Sized>(
    data: &[u8],
    document: &Document,
    decoder: &D,
) -> Coverage {
    let mut result = document.coverage();
    let mut decoded_sheets = 0usize;
    let mut blank_sheets = 0usize;
    for page in document.sheets() {
        if page.is_recoverable() {
            continue;
        }
        // Decode once per sheet. Besides avoiding duplicate work, this keeps
        // the use case deterministic for a stateful custom decoder.
        if decode_page(data, page, decoder).is_some() {
            decoded_sheets += 1;
        } else if !document.pictures_on(page.index).any(|p| p.is_recoverable()) {
            blank_sheets += 1;
        }
    }
    result.sheets_recovered += decoded_sheets;
    result.sheets_blank = blank_sheets;
    result
}

/// A reusable view that gives output adapters the same recovery decision and
/// decoded drawing model.
#[derive(Debug)]
pub struct RecoveryView<'a, D: PageDecoder + ?Sized> {
    data: &'a [u8],
    document: &'a Document,
    decoder: &'a D,
}

impl<'a, D: PageDecoder + ?Sized> RecoveryView<'a, D> {
    pub fn new(data: &'a [u8], document: &'a Document, decoder: &'a D) -> Self {
        Self {
            data,
            document,
            decoder,
        }
    }

    pub fn document(&self) -> &Document {
        self.document
    }

    pub fn decode(&self, page: &Page) -> Option<Metafile> {
        decode_page(self.data, page, self.decoder)
    }

    pub fn coverage(&self) -> Coverage {
        coverage(self.data, self.document, self.decoder)
    }
}
