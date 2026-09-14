//! アプリケーション層が具体アダプターなしでテストできることを確認する。

use std::path::Path;

use xdw_salvage::application::{
    AttachmentScanner, DocumentParser, DocumentReader, PageDecoder, SalvageService,
};
use xdw_salvage::domain::attachment::Attachment;
use xdw_salvage::domain::rendering::Metafile;
use xdw_salvage::domain::{Document, Page};
use xdw_salvage::Error;

#[derive(Debug, Clone, Copy)]
struct StubReader;

impl DocumentReader for StubReader {
    fn read(&self, _path: &Path) -> Result<Vec<u8>, Error> {
        Ok(vec![0xAA, 0xBB])
    }
}

#[derive(Debug, Clone, Copy)]
struct StubParser;

impl DocumentParser for StubParser {
    fn parse(&self, _data: &[u8]) -> Result<Document, Error> {
        Ok(empty_document())
    }
}

#[derive(Debug, Clone, Copy)]
struct StubAttachments;

impl AttachmentScanner for StubAttachments {
    fn scan(&self, _data: &[u8]) -> Vec<Attachment> {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct StubDecoder;

impl PageDecoder for StubDecoder {
    fn decode(&self, _data: &[u8], _page: &Page) -> Option<Metafile> {
        None
    }
}

fn empty_document() -> Document {
    Document {
        generation: 10,
        guard: [0; 4],
        trailer_tag: 0x68,
        trailer_at: 0,
        declared_entries: 0,
        pages: Vec::new(),
        properties: None,
        properties_len: None,
        image_derived: Vec::new(),
        checksum: None,
        generations_present: 1,
        unknown_tags: Vec::new(),
        rebuilt: None,
    }
}

#[test]
fn service_coordinates_injected_ports_without_filesystem_or_codec_calls() {
    let service = SalvageService::new(StubReader, StubParser, StubAttachments, StubDecoder);
    let asset = service.open(Path::new("virtual.xdw")).expect("stub input");

    assert_eq!(asset.data, vec![0xAA, 0xBB]);
    assert_eq!(asset.document.generation, 10);
    let analysis = service.analyze(&asset);
    assert_eq!(analysis.coverage.sheets, 0);
    assert!(analysis.attachments.is_empty());
    assert_eq!(analysis.verdict.as_str(), "NONE");
}
