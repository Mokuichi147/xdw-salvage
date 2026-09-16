//! Dependency-inversion ports used by the application layer.

use std::path::Path;

use crate::domain::attachment::Attachment;
use crate::domain::page::{Overlay, Page};
use crate::domain::rendering::Metafile;
use crate::domain::Document;
use crate::error::Result;

/// Reads the bytes of a document from an external source.
pub trait DocumentReader {
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
}

/// Turns document bytes into the domain document model.
pub trait DocumentParser {
    fn parse(&self, data: &[u8]) -> Result<Document>;
}

/// Finds source files carried by a document.
pub trait AttachmentScanner {
    fn scan(&self, data: &[u8]) -> Vec<Attachment>;
}

/// Recovers a drawing model from a coded page, when the adapter understands
/// that page's storage format.
pub trait PageDecoder {
    fn decode(&self, data: &[u8], page: &Page) -> Option<Metafile>;

    /// Recover a page when its neighbouring entries are needed to complete
    /// an old storage form.  Decoders that do not need document context use
    /// the ordinary page method unchanged.
    fn decode_with_document(
        &self,
        data: &[u8],
        page: &Page,
        _document: &Document,
    ) -> Option<Metafile> {
        self.decode(data, page)
    }

    /// Recover the drawing laid over a page, when the adapter understands
    /// its storage format. `paper` is the page's paper in hundredths of a
    /// millimetre, for overlays that cover the whole page.
    fn decode_overlay(&self, _overlay: &Overlay, _paper: (u32, u32)) -> Option<Metafile> {
        None
    }
}
