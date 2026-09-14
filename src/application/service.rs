//! The document-level application service.

use std::path::Path;

use crate::application::ports::{AttachmentScanner, DocumentParser, DocumentReader, PageDecoder};
use crate::application::recovery;
use crate::domain::attachment::Attachment;
use crate::domain::coverage::Coverage;
use crate::domain::policy::{self, Verdict};
use crate::domain::Document;
use crate::error::Result;

/// Bytes and the parsed document that belong together throughout a use case.
#[derive(Debug, Clone)]
pub struct DocumentAsset {
    pub data: Vec<u8>,
    pub document: Document,
}

/// Facts calculated once for a document and shared by presenters.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub coverage: Coverage,
    pub attachments: Vec<Attachment>,
    pub verdict: Verdict,
}

/// Coordinates document use cases while keeping concrete adapters out of the
/// application layer.
#[derive(Debug, Clone)]
pub struct SalvageService<R, P, A, D> {
    reader: R,
    parser: P,
    attachments: A,
    decoder: D,
}

impl<R, P, A, D> SalvageService<R, P, A, D>
where
    R: DocumentReader,
    P: DocumentParser,
    A: AttachmentScanner,
    D: PageDecoder,
{
    pub fn new(reader: R, parser: P, attachments: A, decoder: D) -> Self {
        Self {
            reader,
            parser,
            attachments,
            decoder,
        }
    }

    /// Open a document through the configured reader and parser.
    pub fn open(&self, path: &Path) -> Result<DocumentAsset> {
        let data = self.reader.read(path)?;
        let document = self.parser.parse(&data)?;
        Ok(DocumentAsset { data, document })
    }

    /// Parse bytes supplied by a caller that already owns the input.
    pub fn parse(&self, data: &[u8]) -> Result<Document> {
        self.parser.parse(data)
    }

    /// Run the analysis use case once and return all facts needed by reports.
    pub fn analyze(&self, asset: &DocumentAsset) -> Analysis {
        let attachments = self.attachments.scan(&asset.data);
        let coverage = recovery::coverage(&asset.data, &asset.document, &self.decoder);
        let verdict = policy::classify(coverage, !attachments.is_empty());
        Analysis {
            coverage,
            attachments,
            verdict,
        }
    }

    pub fn attachments(&self, data: &[u8]) -> Vec<Attachment> {
        self.attachments.scan(data)
    }

    pub fn coverage(&self, data: &[u8], document: &Document) -> Coverage {
        recovery::coverage(data, document, &self.decoder)
    }

    pub fn verdict(&self, data: &[u8], document: &Document) -> Verdict {
        let attachments = self.attachments.scan(data);
        policy::classify(
            recovery::coverage(data, document, &self.decoder),
            !attachments.is_empty(),
        )
    }

    pub fn decoder(&self) -> &D {
        &self.decoder
    }

    pub fn attachment_scanner(&self) -> &A {
        &self.attachments
    }
}
