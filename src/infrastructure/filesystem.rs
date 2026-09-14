//! Local filesystem adapter.

use std::path::Path;

use crate::application::ports::DocumentReader;
use crate::error::Result;

/// Reads a document from the local filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalDocumentReader;

impl DocumentReader for LocalDocumentReader {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        Ok(std::fs::read(path)?)
    }
}
