//! アプリケーション層が具体アダプターなしでテストできることを確認する。

use std::path::Path;

use xdw_salvage::application::{
    display_page_is_recoverable, page_is_recoverable, AttachmentScanner, DocumentParser,
    DocumentReader, PageDecoder, SalvageService,
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
        display_pages: Vec::new(),
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

#[derive(Debug, Clone, Copy)]
struct DrawingDecoder;

impl PageDecoder for DrawingDecoder {
    fn decode(&self, _data: &[u8], page: &Page) -> Option<Metafile> {
        if matches!(page.data, xdw_salvage::domain::PageData::Bare { .. }) {
            Some(Metafile {
                text: vec![xdw_salvage::domain::rendering::Text {
                    xs: vec![0.0],
                    y: 1.0,
                    chars: vec!['x'],
                    font_kind: Default::default(),
                    size: 1.0,
                    escapement: 0,
                    rgb: (0, 0, 0),
                    order: 1,
                    bold: false,
                    underline: false,
                }],
                ..Default::default()
            })
        } else {
            None
        }
    }
}

#[test]
fn recovery_manifest_can_use_the_page_decoder_for_non_structural_pages() {
    let page = Page {
        index: 0,
        role: xdw_salvage::domain::Role::Sheet,
        belongs_to: None,
        offset: 0,
        checksum: None,
        paper: Some((21000, 29700)),
        pixels: None,
        rotation: 0,
        overlays: Vec::new(),
        data: xdw_salvage::domain::PageData::Bare { offset: 0, len: 0 },
        unknown_fields: Vec::new(),
    };
    let mut doc = empty_document();
    doc.pages.push(page.clone());

    assert!(page_is_recoverable(&[], &page, &doc, &DrawingDecoder));
}

#[test]
fn an_explicit_blank_display_page_is_recoverable() {
    let doc = empty_document();
    let display = xdw_salvage::domain::DisplayPage {
        paper: Some((21000, 29700)),
        rotation: 0,
        overlays: Vec::new(),
        members: Vec::new(),
    };

    assert!(display_page_is_recoverable(
        &[],
        &display,
        &doc,
        &DrawingDecoder
    ));
}
