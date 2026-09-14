//! XDW container parser adapter.

use crate::application::ports::DocumentParser;
use crate::domain::Document;
use crate::error::Result;
use crate::infrastructure::xdw_document;

/// Adapts the existing, format-specific parser to the application port.
#[derive(Debug, Clone, Copy, Default)]
pub struct XdwDocumentParser;

impl DocumentParser for XdwDocumentParser {
    fn parse(&self, data: &[u8]) -> Result<Document> {
        xdw_document::parse(data)
    }
}
