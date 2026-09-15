//! Tests run against containers built inside the test itself.
//!
//! Everything here is synthetic, so the suite carries no third-party file.

use xdw_salvage::adapters::pdf;
use xdw_salvage::application::verification as verify;
use xdw_salvage::domain::{Kind, PageData, Verdict};
use xdw_salvage::infrastructure::xdw_document::parse;
use xdw_salvage::infrastructure::{self, attachments as attach, tlv};

/// Encode one element with the container's length rules.
fn elem(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = value.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n <= 0xFF {
        out.extend_from_slice(&[0x81, n as u8]);
    } else if n <= 0xFFFF {
        out.push(0x82);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x83);
        out.extend_from_slice(&(n as u32).to_be_bytes()[1..]);
    }
    out.extend_from_slice(value);
    out
}

/// A JPEG with a real header and no scan data. Never decoded, only measured.
fn tiny_jpeg(w: u16, h: u16) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
    v.extend_from_slice(b"JFIF\0");
    v.extend_from_slice(&[0x01, 0x01, 0x01]); // version, units = dpi
    v.extend_from_slice(&150u16.to_be_bytes()); // x density
    v.extend_from_slice(&150u16.to_be_bytes()); // y density
    v.extend_from_slice(&[0x00, 0x00]); // no thumbnail
    v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

/// A page whose body is a raw header plus a JPEG stream.
fn jpeg_page(w: u32, h: u32) -> Vec<u8> {
    let stream = tiny_jpeg(w as u16, h as u16);
    let body_len = 16 + stream.len();
    let mut body = Vec::new();
    body.extend_from_slice(&(body_len as u32).to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&w.to_le_bytes());
    body.extend_from_slice(&h.to_le_bytes());
    body.extend_from_slice(&stream);
    assert_eq!(body.len(), body_len);
    let mut page = elem(0x81, &[0xDE, 0xAD, 0xBE, 0xEF]);
    page.extend_from_slice(&elem(0x82, &body));
    elem(0x64, &page)
}

/// A kind-5 page whose nested data field contains a JPEG directly.
fn kind5_jpeg_page(w: u32, h: u32) -> Vec<u8> {
    let stream = tiny_jpeg(w as u16, h as u16);
    let mut body = Vec::new();
    body.extend_from_slice(&elem(0x80, &[5]));
    body.extend_from_slice(&elem(0x84, &21000u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x85, &29700u16.to_be_bytes()));
    // These attributes belong to the kind-5 page form. Their values are not
    // needed to recover the JPEG, but they must not be reported as unknown
    // fields once this page form is recognized.
    for tag in [0x87, 0x88, 0x8B, 0x8C] {
        body.extend_from_slice(&elem(tag, &[0]));
    }
    body.extend_from_slice(&elem(0x8D, &[3]));
    body.extend_from_slice(&elem(0x86, &stream));
    let mut page = elem(0x81, &[0x01, 0x02, 0x03, 0x04]);
    page.extend_from_slice(&elem(0x82, &body));
    elem(0x64, &page)
}

/// A page whose body is nested metadata around opaque image data.
fn encoded_page(payload_len: usize) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&elem(0x80, &[4]));
    body.extend_from_slice(&elem(0x81, &2000u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x84, &21000u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x85, &29700u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x90, &4961u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x91, &7016u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x89, &(payload_len as u16).to_be_bytes()));
    body.extend_from_slice(&elem(0x86, &vec![0x5A; payload_len]));
    let mut page = elem(0x81, &[0x01, 0x02, 0x03, 0x04]);
    page.extend_from_slice(&elem(0x82, &body));
    elem(0x64, &page)
}

/// 表示形状を持たない不透明データのページ型エントリ。
fn bare_page() -> Vec<u8> {
    let mut page = elem(0x81, &[0x09, 0x08, 0x07, 0x06]);
    page.extend_from_slice(&elem(0x82, &[0x99; 32]));
    elem(0x64, &page)
}

/// A preview entry: a bitmap header and palette in the clear, then payload.
fn preview_page() -> Vec<u8> {
    let mut img = Vec::new();
    img.extend_from_slice(&40u32.to_le_bytes()); // header size
    img.extend_from_slice(&104i32.to_le_bytes());
    img.extend_from_slice(&146i32.to_le_bytes());
    img.extend_from_slice(&1u16.to_le_bytes()); // planes
    img.extend_from_slice(&8u16.to_le_bytes()); // bits per pixel
    img.extend_from_slice(&0u32.to_le_bytes()); // compression
    img.extend_from_slice(&15184u32.to_le_bytes());
    img.extend_from_slice(&492i32.to_le_bytes());
    img.extend_from_slice(&492i32.to_le_bytes());
    img.extend_from_slice(&216u32.to_le_bytes()); // palette entries
    img.extend_from_slice(&0u32.to_le_bytes());
    img.extend_from_slice(&[0u8; 864]); // the palette itself
    img.extend_from_slice(&1u32.to_le_bytes()); // method
    img.extend_from_slice(&32u32.to_le_bytes()); // stored
    img.extend_from_slice(&15184u32.to_le_bytes()); // expanded
    img.extend_from_slice(&146u32.to_le_bytes()); // rows
    img.extend_from_slice(&[0x5Au8; 32]);

    let mut body = Vec::new();
    body.extend_from_slice(&elem(0x80, &[7]));
    body.extend_from_slice(&elem(0x81, &904u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x84, &21138u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x85, &29674u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x89, &(img.len() as u16).to_be_bytes()));
    body.extend_from_slice(&elem(0x86, &img));
    let mut page = elem(0x81, &[7, 7, 7, 7]);
    page.extend_from_slice(&elem(0x82, &body));
    elem(0x64, &page)
}

/// Assemble a whole file around the given page elements.
fn container(generation: u8, trailer_tag: u8, pages: Vec<Vec<u8>>, extra: &[u8]) -> Vec<u8> {
    let mut header_fields = elem(0x82, &[generation]);
    header_fields.extend_from_slice(&elem(0x80, &[0x00, 0xC0, 0x13]));
    header_fields.extend_from_slice(&elem(0x83, &[0x01, 0x0D, 0x0A, 0x01]));
    let header = elem(0x60, &header_fields);

    // Lay the pages out to learn their absolute offsets. The document element
    // header is sized first with a guess, then the whole thing is rebuilt.
    for doc_hdr in [2usize, 3, 4, 5] {
        let base = header.len() + doc_hdr;
        let mut body = Vec::new();
        let mut offsets: Vec<u32> = Vec::new();
        for p in &pages {
            offsets.push((base + body.len()) as u32);
            body.extend_from_slice(p);
        }
        body.extend_from_slice(extra);
        let props = elem(0x63, &[0xAB; 8]);
        body.extend_from_slice(&props);

        let mut offset_bytes = Vec::new();
        for o in &offsets {
            offset_bytes.extend_from_slice(&o.to_le_bytes());
        }
        let mut trailer_fields = elem(0x80, &[offsets.len() as u8]);
        trailer_fields.extend_from_slice(&elem(0x81, &offset_bytes));
        // Register every raw-bodied page as image-derived, the way a real
        // container does: the trailer lists (index, checksum) pairs for them.
        let mut reg = Vec::new();
        for (i, p) in pages.iter().enumerate() {
            if p.len() > 24 && p[p.len() - 1] == 0xD9 {
                reg.extend_from_slice(&(i as u32).to_le_bytes());
                reg.extend_from_slice(&[0u8; 4]);
            }
        }
        if !reg.is_empty() {
            trailer_fields.extend_from_slice(&elem(0x8D, &reg));
        }
        trailer_fields.extend_from_slice(&elem(0x83, &32u16.to_be_bytes()));
        trailer_fields.extend_from_slice(&elem(0x84, &8u16.to_be_bytes()));
        trailer_fields.extend_from_slice(&elem(0x85, &[0xAA, 0xBB, 0xCC, 0xDD]));
        let self_len = trailer_fields.len() + 6;
        trailer_fields.extend_from_slice(&elem(0x86, &(self_len as u32).to_le_bytes()));
        assert_eq!(trailer_fields.len(), self_len);
        body.extend_from_slice(&elem(trailer_tag, &trailer_fields));

        let doc_elem = elem(0x61, &body);
        let actual_hdr = doc_elem.len() - body.len();
        if actual_hdr != doc_hdr {
            continue; // the length grew into a wider encoding; try again
        }
        let mut file = header.clone();
        file.extend_from_slice(&doc_elem);
        return file;
    }
    panic!("could not lay out container");
}

#[test]
fn length_encodings_round_trip() {
    for len in [0usize, 1, 0x7F, 0x80, 0xFF, 0x100, 0xFFFF, 0x10000] {
        let bytes = elem(0x64, &vec![7u8; len]);
        let t = tlv::read_one(&bytes, 0).expect("readable");
        assert_eq!(t.tag, 0x64);
        assert_eq!(t.len, len);
        assert_eq!(t.end(), bytes.len());
    }
}

#[test]
fn truncation_is_an_error_not_a_guess() {
    let mut bytes = elem(0x64, &[0u8; 40]);
    bytes.truncate(20);
    assert!(tlv::read_one(&bytes, 0).is_err());
}

#[test]
fn indefinite_length_is_refused() {
    // 0x80 as a length octet would be the indefinite form.
    let bytes = [0x64u8, 0x80, 0x00, 0x00];
    assert!(tlv::read_one(&bytes, 0).is_err());
}

#[test]
fn reads_pages_through_the_trailer() {
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    assert_eq!(doc.generation, 10);
    assert_eq!(doc.trailer_tag, 0x68);
    assert_eq!(doc.pages.len(), 2);
    assert_eq!(doc.guard, [0x01, 0x0D, 0x0A, 0x01]);

    let cov = doc.coverage();
    assert_eq!(cov.sheets, 2);
    assert_eq!(cov.sheets_recovered, 1);
    assert!(!cov.is_complete());

    assert!(doc.pages[0].is_recoverable());
    assert_eq!(doc.pages[0].pixels, Some((600, 800)));
    assert!(!doc.pages[1].is_recoverable());
    assert_eq!(doc.pages[1].paper, Some((21000, 29700)));
    assert_eq!(doc.pages[1].pixels, Some((4961, 7016)));
}

#[test]
fn nested_kind5_jpeg_is_recoverable() {
    let original = tiny_jpeg(600, 800);
    let file = container(10, 0x68, vec![kind5_jpeg_page(600, 800)], &[]);
    let doc = parse(&file).expect("parses");

    assert!(doc.pages[0].is_recoverable());
    assert_eq!(doc.pages[0].paper, Some((21000, 29700)));
    assert_eq!(doc.pages[0].pixels, Some((600, 800)));
    assert!(doc.pages[0].unknown_fields.is_empty());
    let PageData::Jpeg { offset, len } = &doc.pages[0].data else {
        panic!("kind 5 JPEG was not classified as a JPEG page");
    };
    assert_eq!(&file[*offset..*offset + *len], original.as_slice());

    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.embedded, 1);
    assert_eq!(report.placeholders, 0);
    assert!(String::from_utf8_lossy(&bytes).contains("/DCTDecode"));
    assert!(bytes
        .windows(original.len())
        .any(|w| w == original.as_slice()));
}

#[test]
fn older_generation_uses_its_own_trailer_tag() {
    let file = container(7, 0x65, vec![jpeg_page(100, 100)], &[]);
    let doc = parse(&file).expect("parses");
    assert_eq!(doc.generation, 7);
    assert_eq!(doc.trailer_tag, 0x65);
}

#[test]
fn unknown_generation_is_refused_rather_than_guessed() {
    let file = container(12, 0x6A, vec![jpeg_page(10, 10)], &[]);
    match parse(&file) {
        Err(xdw_salvage::Error::UnsupportedGeneration(12)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn unrecognised_fields_are_reported() {
    let mut body = Vec::new();
    body.extend_from_slice(&elem(0x80, &[4]));
    body.extend_from_slice(&elem(0x84, &21000u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x85, &29700u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x86, &[0u8; 16]));
    body.extend_from_slice(&elem(0x9F, b"text layer?")); // not a tag we know
    let mut page = elem(0x81, &[1, 2, 3, 4]);
    page.extend_from_slice(&elem(0x82, &body));
    let file = container(10, 0x68, vec![elem(0x64, &page)], &[]);
    let doc = parse(&file).expect("parses");
    assert_eq!(doc.pages[0].unknown_fields, vec![0x9F]);
}

#[test]
fn pdf_embeds_recoverable_pages_and_marks_the_rest() {
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());

    assert_eq!(report.embedded, 1);
    assert_eq!(report.placeholders, 1);
    assert!(bytes.starts_with(b"%PDF-1.5"));
    assert!(bytes.ends_with(b"%%EOF\n"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/DCTDecode"));
    assert!(text.contains("/Count 2"));
    assert!(text.contains("startxref"));

    // The JPEG must be present unchanged.
    let original = tiny_jpeg(600, 800);
    assert!(
        bytes
            .windows(original.len())
            .any(|w| w == original.as_slice()),
        "page image was altered"
    );
}

#[test]
fn pdf_can_leave_unrecoverable_pages_out() {
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        missing: pdf::Missing::Skip,
        ..pdf::Options::default()
    };
    let (bytes, report) = pdf::build(&file, &doc, opts);
    assert_eq!(report.embedded, 1);
    assert_eq!(report.skipped, 1);
    assert!(String::from_utf8_lossy(&bytes).contains("/Count 1"));
}

/// A stored-only zip holding one entry, built by hand.
fn tiny_zip(name: &str, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let name_b = name.as_bytes();
    out.extend_from_slice(b"PK\x03\x04");
    out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version..time
    out.extend_from_slice(&0u32.to_le_bytes()); // crc, left zero on purpose
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name_b);
    out.extend_from_slice(content);

    let cd_offset = out.len();
    out.extend_from_slice(b"PK\x01\x02");
    out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra
    out.extend_from_slice(&0u16.to_le_bytes()); // comment
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // local header offset
    out.extend_from_slice(name_b);
    let cd_size = out.len() - cd_offset;

    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(cd_size as u32).to_le_bytes());
    out.extend_from_slice(&(cd_offset as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[test]
fn finds_and_names_an_embedded_office_file() {
    let zip = tiny_zip("word/document.xml", b"<w:document/>");
    let file = container(10, 0x68, vec![encoded_page(32)], &elem(0x70, &zip));

    let found = attach::scan(&file);
    assert_eq!(found.len(), 1, "expected exactly one payload");
    assert_eq!(found[0].kind, Kind::Zip { part: Some("docx") });
    assert_eq!(found[0].kind.extension(), "docx");
    assert!(!found[0].length_is_estimate);
    assert_eq!(
        found[0].bytes(&file),
        zip.as_slice(),
        "payload cut inexactly"
    );

    let doc = parse(&file).expect("parses");
    assert_eq!(
        infrastructure::local_service().verdict(&file, &doc),
        Verdict::OriginalFile
    );
}

#[test]
fn a_document_with_no_payload_reports_none() {
    let file = container(10, 0x68, vec![encoded_page(32)], &[]);
    assert!(attach::scan(&file).is_empty());
    let doc = parse(&file).expect("parses");
    assert_eq!(
        infrastructure::local_service().verdict(&file, &doc),
        Verdict::StructureOnly
    );
}

/// A page with a small paper size, to check the placeholder stays on the sheet.
fn small_encoded_page(w_100mm: u16, h_100mm: u16) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&elem(0x80, &[4]));
    body.extend_from_slice(&elem(0x81, &500u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x84, &w_100mm.to_be_bytes()));
    body.extend_from_slice(&elem(0x85, &h_100mm.to_be_bytes()));
    body.extend_from_slice(&elem(0x89, &16u16.to_be_bytes()));
    body.extend_from_slice(&elem(0x86, &[0x33u8; 16]));
    let mut page = elem(0x81, &[9, 9, 9, 9]);
    page.extend_from_slice(&elem(0x82, &body));
    elem(0x64, &page)
}

/// Every `x y Td` pair in the content streams, which are stored uncompressed.
fn text_positions(pdf: &[u8]) -> Vec<(f32, f32)> {
    let text = String::from_utf8_lossy(pdf);
    let mut out = Vec::new();
    for (i, _) in text.match_indices(" Td ") {
        let before: Vec<&str> = text[..i].split_whitespace().rev().take(2).collect();
        if let (Some(y), Some(x)) = (before.first(), before.get(1)) {
            if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                out.push((x, y));
            }
        }
    }
    out
}

/// Every `/MediaBox [0 0 w h]` in the file.
fn media_boxes(pdf: &[u8]) -> Vec<(f32, f32)> {
    let text = String::from_utf8_lossy(pdf);
    let mut out = Vec::new();
    for (i, _) in text.match_indices("/MediaBox [0 0 ") {
        let rest = &text[i + 15..];
        let end = rest.find(']').unwrap_or(0);
        let nums: Vec<f32> = rest[..end]
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() == 2 {
            out.push((nums[0], nums[1]));
        }
    }
    out
}

#[test]
fn placeholder_text_stays_on_a_very_small_page() {
    // 26.4 x 2.0 mm: shorter than the note would be at a fixed offset.
    let file = container(10, 0x68, vec![small_encoded_page(2636, 198)], &[]);
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.placeholders, 1);

    let boxes = media_boxes(&bytes);
    assert_eq!(boxes.len(), 1);
    let (pw, ph) = boxes[0];
    assert!((ph - 5.61).abs() < 0.1, "page height was {ph}");

    for (x, y) in text_positions(&bytes) {
        assert!(
            x >= 0.0 && x <= pw && y >= 0.0 && y <= ph,
            "text placed at ({x}, {y}) is outside a {pw} x {ph} page"
        );
    }
    // A page too small for legible text still gets a visible frame.
    assert!(String::from_utf8_lossy(&bytes).contains(" re S"));
}

#[test]
fn placeholder_note_shrinks_rather_than_overflowing() {
    // 78 x 31 mm, wide enough for a short note but not the long one.
    let file = container(10, 0x68, vec![small_encoded_page(7800, 3100)], &[]);
    let doc = parse(&file).expect("parses");
    let (bytes, _) = pdf::build(&file, &doc, pdf::Options::default());
    let text = String::from_utf8_lossy(&bytes);
    let (pw, ph) = media_boxes(&bytes)[0];
    for (x, y) in text_positions(&bytes) {
        assert!(
            x >= 0.0 && x <= pw && y >= 0.0 && y <= ph,
            "({x}, {y}) off page"
        );
    }
    assert!(
        text.contains("not recovered") || text.contains("could not be recovered"),
        "no note was drawn"
    );
    // The note should be set at a readable size, not shrunk to fit the long one.
    assert!(
        text.contains("/FA 10.00 Tf"),
        "note was shrunk instead of shortened"
    );
}

#[test]
fn uniform_paper_puts_every_page_on_one_sheet() {
    let file = container(
        10,
        0x68,
        vec![jpeg_page(600, 800), small_encoded_page(7800, 3100)],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        paper: Some(pdf::A4),
        ..pdf::Options::default()
    };
    let (bytes, report) = pdf::build(&file, &doc, opts);
    assert_eq!(report.embedded, 1);
    assert_eq!(report.placeholders, 1);
    for (w, h) in media_boxes(&bytes) {
        assert!((w - pdf::A4.0).abs() < 0.01 && (h - pdf::A4.1).abs() < 0.01);
    }
}

#[test]
fn images_are_never_scaled_up_to_fill_the_sheet() {
    // A 100 x 100 pixel image at 72 dpi is 100 x 100 pt; on A4 it must stay so.
    let file = container(10, 0x68, vec![jpeg_page(100, 100)], &[]);
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        paper: Some(pdf::A4),
        ..pdf::Options::default()
    };
    let (bytes, _) = pdf::build(&file, &doc, opts);
    let text = String::from_utf8_lossy(&bytes);
    // 150 dpi in the test JPEG, so 100 px is 48 pt.
    assert!(
        text.contains("q 48.00 0 0 48.00 "),
        "image was rescaled: {text:.0}"
    );
}

#[test]
fn a_picture_after_a_sheet_belongs_to_that_sheet() {
    // How the container writes a page made of artwork: the sheet, then the
    // pictures it is made of, then the sheet's thumbnail. Counting the pictures
    // as pages turns a one page pamphlet into a four page document.
    let file = container(
        10,
        0x68,
        vec![
            encoded_page(64),
            jpeg_page(4733, 281),
            jpeg_page(4733, 281),
            jpeg_page(4733, 281),
            preview_page(),
        ],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();

    assert_eq!(cov.sheets, 1, "a pamphlet page is one page");
    assert_eq!(cov.pictures, 3);
    assert_eq!(cov.thumbnails, 1);
    assert_eq!(cov.sheets_recovered, 0, "the sheet itself is not recovered");
    assert_eq!(cov.pictures_recovered, 3);
    assert_eq!(cov.sheets_with_pictures, 1);
    assert_eq!(doc.pictures_on(0).count(), 3);
    assert_eq!(verify::Expectation::of(&doc).pages, 1);
}

#[test]
fn a_picture_with_no_sheet_before_it_is_a_sheet_of_its_own() {
    // A document made only of imported pictures has no printer-driver sheet to
    // attach them to, and every picture is then a page in its own right.
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();

    assert_eq!(doc.image_derived, vec![0]);
    assert_eq!(cov.sheets, 2);
    assert_eq!(cov.pictures, 0);
    assert_eq!(cov.sheets_recovered, 1);
    assert_eq!(cov.printer_derived(), 1);
}

#[test]
fn a_wholly_printer_derived_document_has_nothing_to_recover() {
    let file = container(10, 0x68, vec![encoded_page(64), encoded_page(80)], &[]);
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();
    assert_eq!(cov.sheets, 2);
    assert_eq!(cov.printer_derived(), 2);
    assert_eq!(cov.sheets_recovered, 0);
    assert_eq!(cov.pictures, 0);
    // Not a failure of the reader: there is nothing here a reader could get.
    assert_eq!(cov.recovery_of_pictures(), None);
}

#[test]
fn a_sheets_artwork_lands_on_that_sheet_not_on_sheets_of_its_own() {
    let file = container(
        10,
        0x68,
        vec![encoded_page(64), jpeg_page(400, 120), jpeg_page(400, 120)],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.placeholders, 1);
    assert_eq!(report.embedded, 0);
    assert_eq!(report.pictures_placed, 2);
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Count 1"),
        "the pamphlet became several pages"
    );
    assert!(text.contains("/Ar0"), "the artwork was dropped");
    assert!(text.contains("/Ar1"));

    let (page, hreport) = xdw_salvage::adapters::html::build(
        &file,
        &doc,
        &xdw_salvage::adapters::html::Options::default(),
    );
    assert_eq!(hreport.gaps, 1);
    assert_eq!(hreport.pictures, 2);
    assert_eq!(page.matches("id=\"p").count(), 1);
}

#[test]
fn artwork_on_a_recovered_sheet_is_kept_not_dropped() {
    // A recovered sheet can still own artwork. Keep both layers and preserve
    // their recorded order; nothing recovered may be dropped.
    let file = container(
        10,
        0x68,
        vec![jpeg_page(900, 860), jpeg_page(200, 240)],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.embedded, 1, "the sheet itself");
    assert_eq!(report.placeholders, 0);
    assert_eq!(report.pictures_placed, 1, "the artwork was dropped");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Count 1"), "the cover became several pages");
    assert!(text.contains("/Im0"), "the sheet is missing");
    assert!(text.contains("/Ar0"), "the artwork is missing");
}

#[test]
fn a_page_with_neither_sheet_nor_artwork_is_counted_as_blank() {
    // Sizing a migration turns on one number: how many pages arrive EMPTY,
    // not how many are merely degraded. A real two-page pamphlet has a cover
    // whose artwork survives and an all-text page where nothing does; the
    // second is the one that has to be counted.
    let file = container(
        10,
        0x68,
        vec![encoded_page(64), jpeg_page(400, 120), encoded_page(64)],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();
    assert_eq!(cov.sheets, 2);
    assert_eq!(cov.sheets_recovered, 0);
    assert_eq!(cov.sheets_with_pictures, 1);
    assert_eq!(
        cov.sheets_blank, 1,
        "the all-text page must be counted as blank, the cover must not"
    );
}

#[test]
fn a_recovered_sheet_is_never_counted_as_blank() {
    let file = container(10, 0x68, vec![jpeg_page(400, 120), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();
    assert_eq!(cov.sheets, 2);
    assert_eq!(cov.sheets_blank, 1);
}

#[test]
fn the_expectation_comes_from_the_container() {
    let file = container(
        10,
        0x68,
        vec![jpeg_page(600, 800), small_encoded_page(21000, 29700)],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);

    assert_eq!(exp.pages, 2, "both entries are content pages");
    assert_eq!(exp.previews, 0);
    assert_eq!(exp.sizes.len(), 2);
    // The encoded page declares A4; the imported picture declares no paper.
    assert_eq!(exp.sizes[0], None);
    let (w, h) = exp.size_mm(1).expect("A4 in millimetres");
    assert!(
        (w - 210.0).abs() < 0.5 && (h - 297.0).abs() < 0.5,
        "{w} x {h}"
    );
}

#[test]
fn a_faithful_page_count_passes() {
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);
    let report = verify::compare(&exp, 2, &[]);
    assert!(report.is_clean());
    assert!(report.pages_all_present());
}

#[test]
fn a_dropped_page_is_reported_as_a_loss() {
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);
    let report = verify::compare(&exp, 1, &[]);

    assert!(!report.pages_all_present());
    assert!(report.findings.iter().any(|f| matches!(
        f,
        verify::Finding::PageCount {
            expected: 2,
            observed: 1
        }
    )));
    assert!(report.findings[0].describe().contains("1 missing"));
}

#[test]
fn extra_pages_are_not_a_loss_but_are_still_reported() {
    let file = container(10, 0x68, vec![encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);
    let report = verify::compare(&exp, 3, &[]);
    assert!(report.pages_all_present(), "nothing went missing");
    assert!(!report.is_clean());
    assert!(report.findings[0].describe().contains("2 extra"));
}

#[test]
fn a_converter_that_emitted_the_thumbnails_is_named() {
    // One content page and one preview entry; a converter that emitted both
    // produces two pages where one was expected.
    let file = container(10, 0x68, vec![encoded_page(64), preview_page()], &[]);
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);
    assert_eq!(exp.pages, 1);
    assert_eq!(exp.previews, 1);

    let report = verify::compare(&exp, 2, &[]);
    assert!(report
        .findings
        .iter()
        .any(|f| matches!(f, verify::Finding::PreviewsConverted { previews: 1 })));
}

#[test]
fn a_resized_page_is_reported_when_sizes_are_supplied() {
    let file = container(10, 0x68, vec![small_encoded_page(21000, 29700)], &[]);
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);
    // US Letter instead of A4.
    let report = verify::compare(&exp, 1, &[(612.0, 792.0)]);
    assert!(report.pages_all_present());
    assert_eq!(report.compared_sizes, 1);
    assert!(report
        .findings
        .iter()
        .any(|f| matches!(f, verify::Finding::PageSize { index: 0, .. })));
}

#[test]
fn turning_a_page_round_is_not_a_size_change() {
    let file = container(10, 0x68, vec![small_encoded_page(29700, 21000)], &[]);
    let doc = parse(&file).expect("parses");
    let exp = verify::Expectation::of(&doc);
    // The container says 297 x 210; the converted file says 210 x 297.
    let report = verify::compare(&exp, 1, &[(595.28, 841.89)]);
    assert!(report.is_clean(), "unexpected: {:?}", report.findings);
}

#[test]
fn an_embedded_original_is_carried_into_the_pdf() {
    let zip = tiny_zip("word/document.xml", b"<w:document/>");
    let file = container(10, 0x68, vec![encoded_page(32)], &elem(0x70, &zip));
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());

    assert_eq!(report.attachments, 1);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/EmbeddedFiles"), "no attachment name tree");
    assert!(text.contains("/Type /Filespec"));
    assert!(text.contains("/Type /EmbeddedFile"));
    assert!(
        text.contains("original1.docx"),
        "attachment not named by kind"
    );
    assert!(
        text.contains("/PageMode /UseAttachments"),
        "reader will not show it"
    );
    // The payload must go in untouched.
    assert!(
        bytes.windows(zip.len()).any(|w| w == zip.as_slice()),
        "the attached file was altered"
    );
}

#[test]
fn attachments_can_be_left_out() {
    let zip = tiny_zip("word/document.xml", b"<w:document/>");
    let file = container(10, 0x68, vec![encoded_page(32)], &elem(0x70, &zip));
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        embed_originals: false,
        ..pdf::Options::default()
    };
    let (bytes, report) = pdf::build(&file, &doc, opts);
    assert_eq!(report.attachments, 0);
    assert!(!String::from_utf8_lossy(&bytes).contains("/EmbeddedFiles"));
}

#[test]
fn the_pdf_records_what_it_carries() {
    let file = container(10, 0x68, vec![jpeg_page(600, 800), encoded_page(64)], &[]);
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        title: Some("title.xdw".into()),
        ..pdf::Options::default()
    };
    let (bytes, _) = pdf::build(&file, &doc, opts);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Producer (xdw-salvage)"));
    assert!(
        text.contains("/Info "),
        "the trailer must point at the info dict"
    );
    assert!(
        text.contains("recovered 1 of 2 page(s)"),
        "the file should say how complete it is"
    );
}

#[test]
fn a_japanese_note_always_comes_with_a_line_any_reader_can_draw() {
    let file = container(10, 0x68, vec![small_encoded_page(21000, 29700)], &[]);
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        lang: pdf::Lang::Japanese,
        ..pdf::Options::default()
    };
    let (bytes, _) = pdf::build(&file, &doc, opts);
    let text = String::from_utf8_lossy(&bytes);

    // Both faces are declared and both are used.
    assert!(text.contains("/Subtype /Type0"), "no Japanese font");
    assert!(text.contains("/UniJIS-UTF16-H"));
    assert!(text.contains("/Subtype /CIDFontType2"));
    assert!(text.contains("/BaseFont /MS-Mincho"));
    assert!(text.contains("/Flags 6"));
    assert!(text.contains("/FontBBox [-1000 -140 1000 859]"));
    assert!(text.contains("/Ascent 859 /Descent -140 /CapHeight 679 /StemV 1000"));
    assert!(text.contains("/Ordering (Japan1) /Supplement 4"));
    assert!(
        !text.contains("/FontFile2"),
        "default output embedded a font"
    );
    assert!(text.contains("/BaseFont /Helvetica"), "no fallback font");
    assert!(text.contains("/FJ ") && text.contains("/FA "));
    // The Japanese line is a hex string; the fallback is a literal one.
    assert!(text.contains("Tf") && text.contains("<") && text.contains("Tj"));
    assert!(
        text.contains("not recovered") || text.contains("could not be recovered"),
        "a reader without Japanese fonts would see an empty page"
    );
}

#[test]
fn english_output_carries_no_japanese_font() {
    let file = container(10, 0x68, vec![encoded_page(32)], &[]);
    let doc = parse(&file).expect("parses");
    let (bytes, _) = pdf::build(&file, &doc, pdf::Options::default());
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("/Subtype /Type0"));
    assert!(text.contains("/BaseFont /Helvetica"));
}

#[test]
fn placeholder_numbering_follows_the_pdf_not_the_container() {
    // A document whose entries alternate content and preview: the second
    // content page is entry 2, but sheet 2 of the PDF. Numbering the note from
    // the entry index would put "page 3" on sheet 2.
    let file = container(
        10,
        0x68,
        vec![
            encoded_page(32),
            preview_page(),
            small_encoded_page(21000, 29700),
            preview_page(),
        ],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.placeholders, 2, "previews are not pages");

    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Page 1"), "first sheet mis-numbered");
    assert!(text.contains("Page 2"), "second sheet should be page 2");
    assert!(
        !text.contains("Page 3"),
        "the entry index leaked into the note"
    );
}

#[test]
fn gaps_are_bookmarked_and_point_at_their_own_page() {
    let file = container(
        10,
        0x68,
        vec![jpeg_page(600, 800), encoded_page(32), encoded_page(48)],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());

    assert_eq!(report.bookmarks, 2, "one bookmark per unrecoverable page");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Type /Outlines"));
    assert!(text.contains("/Count 2"));
    assert!(text.contains("Page 2 not recovered"));
    assert!(text.contains("Page 3 not recovered"));
    // Chained both ways so a reader can walk the list.
    assert!(text.contains("/Next ") && text.contains("/Prev "));
}

#[test]
fn bookmarks_can_be_turned_off() {
    let file = container(10, 0x68, vec![encoded_page(32)], &[]);
    let doc = parse(&file).expect("parses");
    let opts = pdf::Options {
        bookmark_gaps: false,
        ..pdf::Options::default()
    };
    let (bytes, report) = pdf::build(&file, &doc, opts);
    assert_eq!(report.bookmarks, 0);
    assert!(!String::from_utf8_lossy(&bytes).contains("/Type /Outlines"));
}

#[test]
fn a_fully_recovered_document_gets_no_bookmarks() {
    let file = container(10, 0x68, vec![jpeg_page(100, 100)], &[]);
    let doc = parse(&file).expect("parses");
    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.placeholders, 0);
    assert_eq!(report.bookmarks, 0);
    assert!(!String::from_utf8_lossy(&bytes).contains("/Outlines"));
}

// ---------------------------------------------------------------------------
// HTML output
// ---------------------------------------------------------------------------

#[test]
fn html_carries_the_page_bytes_unchanged() {
    let jpeg = tiny_jpeg(300, 200);
    let bytes = container(10, 0x68, vec![jpeg_page(300, 200), encoded_page(64)], &[]);
    let doc = parse(&bytes).unwrap();
    let (page, report) = xdw_salvage::adapters::html::build(
        &bytes,
        &doc,
        &xdw_salvage::adapters::html::Options::default(),
    );

    assert_eq!(report.embedded, 1);
    assert_eq!(report.gaps, 1);

    // The image in the page must be the same bytes the container held.
    let needle = "data:image/jpeg;base64,";
    let at = page.find(needle).expect("no image in the page") + needle.len();
    let end = at + page[at..].find('"').unwrap();
    assert_eq!(decode_b64(&page[at..end]), jpeg, "the image was altered");
}

#[test]
fn html_keeps_page_numbering_when_a_page_is_missing() {
    // Sheet, its thumbnail, an imported picture as a page of its own, its
    // thumbnail, then another printed sheet.
    let bytes = container(
        10,
        0x68,
        vec![
            encoded_page(64),
            preview_page(),
            jpeg_page(300, 200),
            preview_page(),
            encoded_page(64),
        ],
        &[],
    );
    let doc = parse(&bytes).unwrap();
    let (page, _) = xdw_salvage::adapters::html::build(
        &bytes,
        &doc,
        &xdw_salvage::adapters::html::Options::default(),
    );
    // Three anchors, in order, and the picture is the second of them.
    let p1 = page.find("id=\"p1\"").expect("no page 1");
    let p2 = page.find("id=\"p2\"").expect("no page 2");
    let p3 = page.find("id=\"p3\"").expect("no page 3");
    let img = page.find("<img").expect("no image");
    assert!(p1 < p2 && p2 < img && img < p3);
}

#[test]
fn html_can_drop_the_missing_pages_instead() {
    let bytes = container(
        10,
        0x68,
        vec![encoded_page(64), preview_page(), jpeg_page(300, 200)],
        &[],
    );
    let doc = parse(&bytes).unwrap();
    let opts = xdw_salvage::adapters::html::Options {
        skip_missing: true,
        ..xdw_salvage::adapters::html::Options::default()
    };
    let (page, report) = xdw_salvage::adapters::html::build(&bytes, &doc, &opts);
    assert_eq!(report.gaps, 0);
    assert_eq!(report.skipped, 1);
    assert!(!page.contains("class=\"page gap\""));
}

#[test]
fn html_escapes_a_hostile_title() {
    let bytes = container(10, 0x68, vec![jpeg_page(300, 200)], &[]);
    let doc = parse(&bytes).unwrap();
    let opts = xdw_salvage::adapters::html::Options {
        title: Some("<script>alert(1)</script>".into()),
        ..xdw_salvage::adapters::html::Options::default()
    };
    let (page, _) = xdw_salvage::adapters::html::build(&bytes, &doc, &opts);
    assert!(
        !page.contains("<script>alert"),
        "a file name went into the page as markup"
    );
    assert!(page.contains("&lt;script&gt;"));
}

#[test]
fn html_offers_an_embedded_original_for_download() {
    let zip = tiny_zip("word/document.xml", b"<w:document/>");
    let bytes = container(10, 0x68, vec![jpeg_page(300, 200)], &zip);
    let doc = parse(&bytes).unwrap();
    let (page, report) = xdw_salvage::adapters::html::build(
        &bytes,
        &doc,
        &xdw_salvage::adapters::html::Options::default(),
    );
    assert_eq!(report.attachments, 1);
    assert!(page.contains("download=\""));
    assert!(page.contains(".docx"));
}

#[test]
fn html_says_so_when_there_is_nothing_to_show() {
    let bytes = container(10, 0x68, vec![], &[]);
    let doc = parse(&bytes).unwrap();
    let (page, report) = xdw_salvage::adapters::html::build(
        &bytes,
        &doc,
        &xdw_salvage::adapters::html::Options::default(),
    );
    assert_eq!(report.embedded, 0);
    assert!(page.contains("No page of this document could be recovered."));
    assert!(page.trim_end().ends_with("</html>"));
}

#[test]
fn the_source_document_can_travel_inside_the_pdf() {
    let bytes = container(10, 0x68, vec![encoded_page(64), encoded_page(64)], &[]);
    let opts = pdf::Options {
        carry_source: true,
        title: Some("source.xdw".into()),
        ..pdf::Options::default()
    };
    let (out, report) = pdf::build(&bytes, &parse(&bytes).unwrap(), opts);
    assert_eq!(report.attachments, 1);

    // The container's own bytes must be in there, verbatim.
    assert!(
        out.windows(bytes.len()).any(|w| w == bytes),
        "the source was not carried whole"
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("/Type /EmbeddedFile"));
    // The file name is reduced to something a PDF string can hold safely.
    assert!(
        text.contains(".xdw)"),
        "no file name for the carried source"
    );
}

#[test]
fn a_japanese_title_survives_into_the_document_information() {
    let bytes = container(10, 0x68, vec![encoded_page(64)], &[]);
    let opts = pdf::Options {
        title: Some("日本語タイトル".into()),
        ..pdf::Options::default()
    };
    let (out, _) = pdf::build(&bytes, &parse(&bytes).unwrap(), opts);
    let text = String::from_utf8_lossy(&out);
    // UTF-16BE with the byte order mark, not a row of question marks.
    let want: String = "日本語タイトル"
        .encode_utf16()
        .map(|u| format!("{u:04X}"))
        .collect();
    assert!(
        text.contains(&format!("/Title <FEFF{want}>")),
        "the title was flattened"
    );
    assert!(!text.contains("/Title (???"));
}

#[test]
fn the_source_document_can_travel_inside_the_html() {
    let bytes = container(10, 0x68, vec![encoded_page(64)], &[]);
    let doc = parse(&bytes).unwrap();
    let opts = xdw_salvage::adapters::html::Options {
        carry_source: true,
        title: Some("source.xdw".into()),
        ..xdw_salvage::adapters::html::Options::default()
    };
    let (page, report) = xdw_salvage::adapters::html::build(&bytes, &doc, &opts);
    assert_eq!(report.attachments, 1);
    let needle = "base64,";
    let at = page.find(needle).expect("nothing to download") + needle.len();
    let end = at + page[at..].find('"').unwrap();
    assert_eq!(decode_b64(&page[at..end]), bytes, "the source was altered");
}

/// Minimal base64 decoder, for checking what the writer produced.
fn decode_b64(s: &str) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = A.iter().position(|&a| a == c).expect("not base64") as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Container navigation
// ---------------------------------------------------------------------------

#[test]
fn a_stale_outer_length_does_not_hide_the_live_trailer() {
    // One real document has a document element whose declared length stops
    // short of the trailer that is actually in force: the save that appended
    // the trailer never went back to widen the length in front of it. Reading
    // forwards from the start would miss every page. Reading back from the end
    // of the file, which is the only supported route, must not care.
    let mut bytes = container(10, 0x68, vec![jpeg_page(300, 200), encoded_page(64)], &[]);
    let good = parse(&bytes).unwrap();

    // Shrink the document element's length field without moving anything.
    let header = tlv::read_one(&bytes, 0).unwrap();
    let at = header.value + header.len;
    assert_eq!(bytes[at], 0x61, "the fixture changed shape");
    let body = tlv::read_one(&bytes, at).unwrap();
    let width = body.value - at - 1; // length octets, or 0 for the short form
    let shrunk = (body.len - 40) as u64;
    if width == 0 {
        bytes[at + 1] = shrunk as u8;
    } else {
        for (k, slot) in (0..width).rev().zip(at + 2..body.value) {
            bytes[slot] = (shrunk >> (8 * k)) as u8;
        }
    }

    let stale = parse(&bytes).expect("a short outer length must not stop the read");
    assert_eq!(stale.pages.len(), good.pages.len());
    assert_eq!(stale.coverage(), good.coverage());
}

#[test]
fn a_page_imported_from_a_picture_keeps_its_own_sheet() {
    // A run that holds only pictures is a page that was imported rather than
    // printed: the biggest picture is the page, anything else sits on it.
    let file = container(
        10,
        0x68,
        vec![jpeg_page(947, 859), jpeg_page(203, 243), preview_page()],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();
    assert_eq!(cov.sheets, 1);
    assert_eq!(cov.sheets_recovered, 1);
    assert_eq!(cov.pictures, 1);
    assert_eq!(doc.sheets().next().unwrap().pixels, Some((947, 859)));
}

#[test]
fn a_broken_page_table_is_rebuilt_by_scanning() {
    // Two real documents carry a trailer describing a longer file than the one
    // on disk: every offset misses. Refusing them would be the easy answer and
    // the wrong one, because the pages are all still there to be found.
    let mut file = container(
        10,
        0x68,
        vec![encoded_page(64), jpeg_page(300, 200), preview_page()],
        &[],
    );
    let good = parse(&file).expect("parses");
    assert!(good.rebuilt.is_none());

    // Push every offset in the trailer's table past the end of the file.
    let n = file.len();
    let tlen = u32::from_le_bytes([file[n - 4], file[n - 3], file[n - 2], file[n - 1]]) as usize;
    let start = n - tlen;
    let mut patched = false;
    for i in start..n - 6 {
        if file[i] == 0x81 && (file[i + 1] as usize).is_multiple_of(4) && file[i + 1] > 0 {
            let count = file[i + 1] as usize;
            for k in (0..count).step_by(4) {
                let at = i + 2 + k;
                if at + 4 <= n {
                    file[at..at + 4].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
                }
            }
            patched = true;
            break;
        }
    }
    assert!(patched, "could not find the offset table");

    let doc = parse(&file).expect("a broken table must not lose the file");
    assert!(doc.rebuilt.is_some(), "the rebuild was not reported");
    assert_eq!(doc.pages.len(), good.pages.len());
    assert_eq!(doc.coverage(), good.coverage());
}

#[test]
fn bands_of_one_picture_are_joined_rather_than_scattered() {
    // A large picture is not stored whole: the driver cuts it into bands of
    // equal width and writes each as its own entry. One real drawing arrives as
    // 691 slivers, which as separate pictures are unreadable and as joined
    // blocks are ten legible images.
    let file = container(
        10,
        0x68,
        vec![
            encoded_page(64),
            jpeg_page(1136, 26),
            jpeg_page(1136, 26),
            jpeg_page(1136, 26),
            jpeg_page(400, 300), // a different width: a picture of its own
            jpeg_page(1136, 26),
            preview_page(),
        ],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let runs = doc.picture_runs(0);
    assert_eq!(runs.len(), 3, "bands were not grouped");
    assert_eq!(runs[0].len(), 3);
    assert_eq!(runs[1].len(), 1);
    assert_eq!(runs[2].len(), 1);

    let (bytes, report) = pdf::build(&file, &doc, pdf::Options::default());
    assert_eq!(report.pictures_placed, 5, "a band was dropped");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Count 1"));
}

#[test]
fn a_file_whose_extension_lies_is_named_not_just_refused() {
    // An archive sweep turns up .xdw files that are really something else.
    // "no container header" sends whoever reads the log hunting for a fault
    // that is not there.
    for (bytes, want) in [
        (&b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj"[..], "a PDF"),
        (
            &b"\xff\xd8\xff\xe0\x00\x10JFIF\0\x01\x01\x01"[..],
            "a JPEG image",
        ),
        (
            &b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1rest of it"[..],
            "an older Office document (compound file)",
        ),
        (&b"II*\x00\x08\x00\x00\x00more"[..], "a TIFF image"),
    ] {
        match parse(bytes) {
            Err(xdw_salvage::Error::NotAContainer { looks_like }) => {
                assert_eq!(looks_like, want)
            }
            other => panic!("{want}: got {other:?}"),
        }
    }
    // A docx is a zip, and the first entry names it.
    let zip = tiny_zip("word/document.xml", b"<w:document/>");
    match parse(&zip) {
        Err(xdw_salvage::Error::NotAContainer { looks_like }) => {
            assert_eq!(looks_like, "a Word document (.docx)")
        }
        other => panic!("docx: got {other:?}"),
    }
    // Something unrecognisable still gets the plain answer, not a guess.
    assert!(matches!(
        parse(&[0x40, 0x02, 0x00, 0x00]),
        Err(xdw_salvage::Error::NoFileHeader)
    ));
}

/// A body that is a plain run of length-prefixed fields with names in it.
fn field_table(names: &[&str]) -> Vec<u8> {
    let mut body = Vec::new();
    for (i, name) in names.iter().enumerate() {
        // id, two small values, then the null-terminated name
        body.push(2);
        body.extend_from_slice(&(2000u16 + i as u16).to_be_bytes());
        body.extend_from_slice(&[1, 4, 1, 0xFF]);
        body.push(name.len() as u8 + 1);
        body.extend_from_slice(name.as_bytes());
        body.push(0);
    }
    let mut page = elem(0x81, &[9, 9, 9, 9]);
    page.extend_from_slice(&elem(0x82, &body));
    elem(0x64, &page)
}

#[test]
fn a_table_of_names_in_the_clear_is_not_a_page() {
    // Two real documents carry, in the page list, a plain run of
    // length-prefixed fields holding short names. Counting it as a page puts a
    // sheet in the document that is not there, and reporting it as an
    // undecodable stream hides something that is already readable.
    let file = container(
        10,
        0x68,
        vec![
            encoded_page(64),
            preview_page(),
            field_table(&["dpi", "mm", "org", "%100"]),
        ],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();
    assert_eq!(cov.sheets, 1, "the table was counted as a page");
    assert_eq!(cov.pictures, 0);
    assert_eq!(cov.thumbnails, 1);
    assert_eq!(cov.data_tables, 1);
    assert_eq!(verify::Expectation::of(&doc).pages, 1);

    let table = doc
        .pages
        .iter()
        .find(|p| matches!(p.data, PageData::Fields { .. }))
        .expect("no field table found");
    let PageData::Fields {
        offset,
        len,
        records,
    } = table.data
    else {
        unreachable!()
    };
    assert_eq!(records, 4 * 4);
    assert_eq!(
        xdw_salvage::infrastructure::xdw_page::field_names(&file, offset, len),
        vec!["dpi", "mm", "org", "%100"]
    );

    // It does not become a page of the conversion either.
    let (bytes, _) = pdf::build(&file, &doc, pdf::Options::default());
    assert!(String::from_utf8_lossy(&bytes).contains("/Count 1"));
}

#[test]
fn metadata_less_bare_tail_is_not_a_page() {
    let file = container(
        10,
        0x68,
        vec![encoded_page(64), preview_page(), bare_page()],
        &[],
    );
    let doc = parse(&file).expect("parses");
    let cov = doc.coverage();
    assert_eq!(cov.sheets, 1, "不透明な末尾データがページとして数えられた");
    assert_eq!(cov.thumbnails, 1);
    assert_eq!(cov.data_tables, 1);
    assert_eq!(doc.pages[2].role.as_str(), "data");

    let (bytes, _) = pdf::build(&file, &doc, pdf::Options::default());
    assert!(String::from_utf8_lossy(&bytes).contains("/Count 1"));
}

#[test]
fn coded_data_is_not_mistaken_for_a_table_of_names() {
    // The coding will now and then parse as a run of fields by luck. Requiring
    // names keeps it out; nine real annotation blocks are coded, and none of
    // them has any.
    let mut rng = 0x243F_6A88_85A3_08D3u64;
    let mut noise = Vec::new();
    for _ in 0..3000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        noise.push((rng >> 24) as u8);
    }
    let mut page = elem(0x81, &[3, 3, 3, 3]);
    page.extend_from_slice(&elem(0x82, &noise));
    let file = container(10, 0x68, vec![elem(0x64, &page)], &[]);
    let doc = parse(&file).expect("parses");
    assert_eq!(doc.coverage().data_tables, 0);
}
