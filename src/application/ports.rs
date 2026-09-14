//! Dependency-inversion ports used by the application layer.

use std::path::Path;

use crate::domain::attachment::Attachment;
use crate::domain::page::Page;
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
}
