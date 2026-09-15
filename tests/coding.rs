//! The page coding, and what comes out of it.
//!
//! Every fixture here is built byte by byte in the test itself.

use xdw_salvage::domain::rendering;
use xdw_salvage::infrastructure::{cp932, emf, lzh, wmf};

/// Writes bits most significant first, the way the coder reads them.
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    bit: u8,
    acc: u8,
}

impl BitWriter {
    fn put(&mut self, value: u32, width: u32) {
        for i in (0..width).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1) as u8;
            self.bit += 1;
            if self.bit == 8 {
                self.out.push(self.acc);
                self.acc = 0;
                self.bit = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit > 0 {
            self.acc <<= 8 - self.bit;
            self.out.push(self.acc);
        }
        self.out
    }
}

/// A block whose three tables each hold exactly one symbol.
///
/// The format writes a zero count to mean "one symbol, and it costs no bits",
/// which makes a whole block of literals expressible without a Huffman table.
/// It is the smallest complete stream the coder can produce.
fn block_of_one_literal(count: u16, byte: u8) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.put(count as u32, 16); // symbols in this block
    w.put(0, 5); // code-length table: no entries...
    w.put(0, 5); // ...its single symbol
    w.put(0, 9); // main table: no entries...
    w.put(byte as u32, 9); // ...its single symbol, the literal
    w.put(0, 4); // distance table: no entries...
    w.put(0, 4); // ...its single symbol
    w.finish()
}

#[test]
fn a_block_of_literals_expands_to_those_literals() {
    let stream = block_of_one_literal(64, b'A');
    let out = lzh::decode(&stream, 64).expect("decodes");
    assert_eq!(out, vec![b'A'; 64]);
}

#[test]
fn decoding_stops_at_the_length_the_container_declared() {
    // The block says 64 symbols; the container says 10 bytes. The container
    // wins, because it is the container that says how long the page is.
    let stream = block_of_one_literal(64, b'Z');
    assert_eq!(lzh::decode(&stream, 10).expect("decodes").len(), 10);
}

#[test]
fn several_blocks_run_on_from_one_another() {
    // Blocks follow one another in the bit stream with no padding between, so
    // the fixture has to write both into the same writer.
    let mut w = BitWriter::default();
    for (count, byte) in [(4u16, b'a'), (4, b'b')] {
        w.put(count as u32, 16);
        w.put(0, 5);
        w.put(0, 5);
        w.put(0, 9);
        w.put(byte as u32, 9);
        w.put(0, 4);
        w.put(0, 4);
    }
    let out = lzh::decode(&w.finish(), 8).expect("decodes");
    assert_eq!(&out[..], b"aaaabbbb");
}

#[test]
fn a_stream_that_runs_out_is_an_error_not_a_panic() {
    let stream = block_of_one_literal(64, b'A');
    // Ask for more than the block can produce, with nothing after it.
    let err = lzh::decode(&stream, 4096).unwrap_err();
    assert!(
        format!("{err}").contains("before it is complete"),
        "unexpected error: {err}"
    );
}

#[test]
fn an_absurd_expanded_length_is_refused_before_anything_is_allocated() {
    let err = lzh::decode(&[0u8; 8], usize::MAX / 2).unwrap_err();
    assert!(format!("{err}").contains("expand"), "unexpected: {err}");
}

#[test]
fn rubbish_never_panics() {
    for len in 0..64usize {
        for seed in 0..16u8 {
            let junk: Vec<u8> = (0..len)
                .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
                .collect();
            let _ = lzh::decode(&junk, 1024);
        }
    }
}

#[test]
fn a_zero_block_count_is_refused_rather_than_looping() {
    let mut w = BitWriter::default();
    w.put(0, 16);
    let err = lzh::decode(&w.finish(), 16).unwrap_err();
    assert!(format!("{err}").contains("code table"), "unexpected: {err}");
}

// --- the page inside ---------------------------------------------------

/// A metafile with one run of text, built to the published record layout.
fn metafile_with_text(text: &[u8], x: i32, y: i32) -> Vec<u8> {
    let mut d = Vec::new();
    let put = |v: i32, d: &mut Vec<u8>| d.extend_from_slice(&v.to_le_bytes());

    // EMR_HEADER, 176 bytes.
    let header_len = 176usize;
    put(1, &mut d);
    put(header_len as i32, &mut d);
    for v in [0, 0, 4960, 7015] {
        put(v, &mut d); // rclBounds
    }
    for v in [0, 0, 21000, 29700] {
        put(v, &mut d); // rclFrame, hundredths of a millimetre
    }
    d.extend_from_slice(b" EMF");
    put(0x0001_0000, &mut d); // version
    put(0, &mut d); // nBytes, filled in below
    put(2, &mut d); // nRecords
    d.extend_from_slice(&8u16.to_le_bytes()); // nHandles
    d.extend_from_slice(&0u16.to_le_bytes()); // reserved
    put(0, &mut d); // nDescription
    put(0, &mut d); // offDescription
    put(0, &mut d); // nPalEntries
    put(4961, &mut d); // szlDevice
    put(7016, &mut d);
    put(210, &mut d); // szlMillimeters
    put(297, &mut d);
    d.resize(header_len, 0);

    // EMR_EXTTEXTOUTA. Every offset in the record is measured from the start
    // of the record itself.
    let rec = d.len();
    let n = text.len();
    let fixed = 76usize;
    let off_dx = fixed;
    let off_str = off_dx + n * 4;
    let size = off_str + n;
    put(83, &mut d);
    put(size as i32, &mut d);
    for v in [x, y, x + 1000, y + 100] {
        put(v, &mut d); // rclBounds
    }
    put(1, &mut d); // iGraphicsMode
    put(1, &mut d); // exScale
    put(1, &mut d); // eyScale
    put(x, &mut d); // ptlReference
    put(y, &mut d);
    put(n as i32, &mut d); // nChars
    put(off_str as i32, &mut d); // offString
    put(0, &mut d); // fOptions
    for v in [0, 0, 0, 0] {
        put(v, &mut d); // rcl
    }
    put(off_dx as i32, &mut d); // offDx
    for _ in 0..n {
        put(100, &mut d); // one advance per source byte
    }
    d.extend_from_slice(text);
    d.resize(rec + size, 0);

    let total = d.len() as i32;
    d[48..52].copy_from_slice(&total.to_le_bytes());
    d
}

#[test]
fn a_page_is_a_metafile_and_its_text_comes_back() {
    let file = metafile_with_text(b"Hello", 500, 1000);
    let page = emf::read(&file).expect("reads as a metafile");
    assert_eq!(page.device, (4961, 7016));
    assert_eq!(page.frame_mm100, (21000, 29700));
    assert_eq!(page.text.len(), 1);
    assert_eq!(page.text[0].chars.iter().collect::<String>(), "Hello");
    // A4 to within a rounding of a point.
    let (w, h) = page.points();
    assert!(
        (w - 595.3).abs() < 1.0 && (h - 841.9).abs() < 1.0,
        "{w}x{h}"
    );
}

#[test]
fn characters_are_placed_from_the_spacing_the_metafile_recorded() {
    let page = emf::read(&metafile_with_text(b"abc", 200, 400)).expect("reads");
    let t = &page.text[0];
    assert_eq!(t.xs, vec![200.0, 300.0, 400.0]);
}

#[test]
fn a_page_that_is_not_a_metafile_is_reported_as_such() {
    assert!(emf::read(&[0u8; 200]).is_none());
    assert!(emf::read(&[]).is_none());
}

#[test]
fn record_types_that_are_not_drawn_are_counted_rather_than_dropped() {
    let mut file = metafile_with_text(b"x", 0, 0);
    // Append a record this reader does not draw.
    file.extend_from_slice(&1234u32.to_le_bytes()); // an unsupported EMF record
    file.extend_from_slice(&24u32.to_le_bytes());
    file.extend_from_slice(&[0u8; 16]);
    let page = emf::read(&file).expect("reads");
    assert_eq!(
        page.skipped.get(&1234),
        Some(&1),
        "a dropped record went unreported"
    );
}

#[test]
fn a_record_size_that_lies_stops_the_walk_rather_than_running_off() {
    let mut file = metafile_with_text(b"x", 0, 0);
    let rec = 176;
    file[rec + 4..rec + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let page = emf::read(&file).expect("reads");
    assert!(page.text.is_empty());
}

// --- the text's encoding ------------------------------------------------

#[test]
fn page_text_decodes_from_the_windows_japanese_code_page() {
    // Ordinary two-byte characters, an ASCII run, and half-width katakana.
    assert_eq!(
        cp932::decode(b"\x93\xfa\x96\x7b")
            .iter()
            .collect::<String>(),
        "日本"
    );
    assert_eq!(
        cp932::decode(b"Excel2000").iter().collect::<String>(),
        "Excel2000"
    );
    assert_eq!(
        cp932::decode(&[0xb1, 0xb2]).iter().collect::<String>(),
        "ｱｲ"
    );
    assert_eq!(
        cp932::decode(&[0x80]).iter().collect::<String>(),
        "\u{0080}"
    );
    assert_eq!(cp932::byte_len('\u{0080}'), 1);
}

#[test]
fn an_unassigned_byte_pair_does_not_swallow_the_rest_of_the_line() {
    let out: String = cp932::decode(b"\x93\xfa\x81\x20AB").iter().collect();
    assert!(out.starts_with('日'), "{out}");
    assert!(
        out.ends_with("AB"),
        "a bad pair ate the text after it: {out}"
    );
}

#[test]
fn a_lead_byte_at_the_very_end_is_not_a_panic() {
    let _ = cp932::decode(b"abc\x93");
}

// --- what the metafile draws besides text -------------------------------

/// Append a record with a fixed body to a metafile.
fn push_record(d: &mut Vec<u8>, kind: u32, body: &[u8]) {
    let size = (8 + body.len()) as u32;
    d.extend_from_slice(&kind.to_le_bytes());
    d.extend_from_slice(&size.to_le_bytes());
    d.extend_from_slice(body);
}

/// A `BITBLT` whose operation is "fill with the current brush".
fn fill_blt(x: i32, y: i32, cx: i32, cy: i32) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [0i32, 0, 0, 0] {
        b.extend_from_slice(&v.to_le_bytes()); // rclBounds
    }
    for v in [x, y, cx, cy] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&0x00F0_0021u32.to_le_bytes()); // PATCOPY
    b.resize(92, 0);
    b
}

/// A solid brush of one colour.
fn brush(handle: u32, r: u8, g: u8, bl: u8) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&handle.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // BS_SOLID
    b.extend_from_slice(&u32::from_le_bytes([r, g, bl, 0]).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b
}

#[test]
fn a_blit_with_no_source_is_a_filled_rectangle() {
    // Rules, borders and blocks of colour are all drawn this way; a reader that
    // ignores them loses every ruled line on a form.
    let mut f = metafile_with_text(b"x", 0, 0);
    push_record(&mut f, 39, &brush(1, 0x20, 0x40, 0x60));
    push_record(&mut f, 37, &1u32.to_le_bytes());
    push_record(&mut f, 76, &fill_blt(100, 200, 300, 4));
    let page = emf::read(&f).expect("reads");
    assert_eq!(page.fills.len(), 1);
    let fill = page.fills[0];
    assert_eq!(
        (fill.left, fill.top, fill.right, fill.bottom),
        (100.0, 200.0, 400.0, 204.0)
    );
    assert_eq!(fill.rgb, (0x20, 0x40, 0x60));
    assert!(fill.clip_path.is_none());
}

#[test]
fn a_brush_that_paints_nothing_fills_nothing() {
    let mut f = metafile_with_text(b"x", 0, 0);
    let mut hollow = brush(1, 0, 0, 0);
    hollow[4..8].copy_from_slice(&1u32.to_le_bytes()); // BS_NULL
    push_record(&mut f, 39, &hollow);
    push_record(&mut f, 37, &1u32.to_le_bytes());
    push_record(&mut f, 76, &fill_blt(0, 0, 10, 10));
    assert!(emf::read(&f).expect("reads").fills.is_empty());
}

/// A private comment record.
fn comment(body: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&(body.len() as u32).to_le_bytes());
    c.extend_from_slice(body);
    c
}

/// A private geometry comment: a closed square in 16-bit absolute points.
fn square_comment(op: u8, side: i16) -> Vec<u8> {
    let mut b = b"DW".to_vec();
    b.push(op);
    b.push(0x20);
    b.extend_from_slice(&4u32.to_le_bytes());
    for (x, y) in [(0, 0), (side, 0), (side, side), (0, side)] {
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
    }
    b
}

#[test]
fn fills_inside_a_clip_path_carry_the_outline_they_are_cut_by() {
    // Gradient lettering is drawn as slivers clipped to the glyph outlines:
    // begin path, outline, select the path as clip, then a filled rectangle.
    // Painting the rectangles unclipped would cover the page with bars, so
    // each fill names the path and the path is stored once.
    let mut f = metafile_with_text(b"x", 0, 0);
    push_record(&mut f, 39, &brush(1, 1, 2, 3));
    push_record(&mut f, 37, &1u32.to_le_bytes());
    for _ in 0..2 {
        push_record(&mut f, 70, &comment(b"DW02"));
        push_record(&mut f, 70, &comment(&square_comment(0x03, 50)));
        push_record(&mut f, 70, &comment(b"DW03"));
        push_record(&mut f, 76, &fill_blt(0, 10, 100, 5));
    }
    let page = emf::read(&f).expect("reads");
    assert_eq!(page.shape_masks, 2);
    assert_eq!(page.paths.len(), 1, "the same outline was stored twice");
    assert_eq!(page.fills.len(), 2);
    assert!(page.fills.iter().all(|f| f.clip_path == Some(0)));
    assert_eq!(page.paths[0].figures.len(), 1);
    assert_eq!(page.paths[0].figures[0].segments.len(), 3);
}

#[test]
fn a_stroked_path_becomes_an_outlined_shape() {
    let mut f = metafile_with_text(b"x", 0, 0);
    // EXTCREATEPEN: handle, four bitmap fields, then style, width, brush
    // style, colour, hatch.
    let mut pen = Vec::new();
    for v in [2u32, 56, 0, 56, 0, 0x12000, 6, 0, 0x0000_00FF, 0] {
        pen.extend_from_slice(&v.to_le_bytes());
    }
    push_record(&mut f, 95, &pen);
    push_record(&mut f, 37, &2u32.to_le_bytes());
    push_record(&mut f, 37, &0x8000_0005u32.to_le_bytes()); // NULL_BRUSH
    push_record(&mut f, 70, &comment(b"DW02"));
    push_record(&mut f, 70, &comment(&square_comment(0x03, 50)));
    push_record(&mut f, 70, &comment(b"DW05"));
    let page = emf::read(&f).expect("reads");
    assert_eq!(page.shapes.len(), 1);
    assert_eq!(page.shapes[0].stroke, Some(((255, 0, 0), 6.0)));
    assert_eq!(page.shapes[0].fill, None);
}

#[test]
fn the_window_origin_moves_everything_onto_the_printable_area() {
    // Pages are recorded with the origin at minus the margin, so a point at
    // logical (0, 0) sits 109 pixels in from the corner of the device.
    let mut f = metafile_with_text(b"x", 0, 0);
    let mut org = Vec::new();
    org.extend_from_slice(&(-109i32).to_le_bytes());
    org.extend_from_slice(&(-109i32).to_le_bytes());
    // Move the origin record ahead of the text: rebuild the file.
    let text_record = f.split_off(176);
    push_record(&mut f, 10, &org);
    f.extend_from_slice(&text_record);
    let page = emf::read(&f).expect("reads");
    assert_eq!(page.text[0].xs[0], 109.0);
}

#[test]
fn an_embedded_bitmap_is_decoded_and_placed() {
    // STRETCHDIBITS with a 2x2 eight-bit bitmap, drawn at twice its size.
    let mut f = metafile_with_text(b"x", 0, 0);
    let mut r = Vec::new();
    let put = |v: i32, r: &mut Vec<u8>| r.extend_from_slice(&v.to_le_bytes());
    for v in [0, 0, 0, 0] {
        put(v, &mut r); // rclBounds
    }
    for v in [100, 200, 0, 0, 2, 2] {
        put(v, &mut r); // xDest, yDest, xSrc, ySrc, cxSrc, cySrc
    }
    let header_at = 8 + 72; // after the fixed part of the record
    let info_len = 40 + 256 * 4;
    put(header_at, &mut r); // offBmiSrc
    put(info_len, &mut r); // cbBmiSrc
    put(header_at + info_len, &mut r); // offBitsSrc
    put(8, &mut r); // cbBitsSrc: two rows padded to four bytes
    put(0, &mut r); // usage
    put(0x00CC_0020u32 as i32, &mut r); // SRCCOPY
    put(4, &mut r); // cxDest
    put(4, &mut r); // cyDest
                    // BITMAPINFOHEADER
    put(40, &mut r);
    put(2, &mut r);
    put(2, &mut r);
    r.extend_from_slice(&1u16.to_le_bytes());
    r.extend_from_slice(&8u16.to_le_bytes());
    for _ in 0..6 {
        put(0, &mut r);
    }
    for i in 0..256u32 {
        r.extend_from_slice(&[i as u8, 0, 0, 0]);
    }
    r.extend_from_slice(&[1, 2, 0, 0, 3, 4, 0, 0]);
    push_record(&mut f, 81, &r);
    let page = emf::read(&f).expect("reads");
    assert_eq!(page.rasters.len(), 1);
    let raster = &page.rasters[0];
    assert_eq!((raster.width, raster.height, raster.bits), (2, 2, 8));
    // Bottom-up storage comes out top-down.
    assert_eq!(raster.rows, vec![3, 4, 1, 2]);
    assert_eq!(raster.palette[3], (0, 0, 3));
    assert_eq!(page.images.len(), 1);
    let i = page.images[0];
    assert_eq!(i.source, rendering::Source::Inline(0));
    assert_eq!(
        (i.left, i.top, i.right, i.bottom),
        (100.0, 200.0, 104.0, 204.0)
    );
}

#[test]
fn a_bold_underlined_face_marks_its_text() {
    let mut f = metafile_with_text(b"x", 0, 0);
    // EXTCREATEFONTINDIRECTW: handle then LOGFONTW.
    let mut font = Vec::new();
    font.extend_from_slice(&3u32.to_le_bytes());
    for v in [-88i32, 40, 0, 0, 700] {
        font.extend_from_slice(&v.to_le_bytes());
    }
    font.extend_from_slice(&[0, 1, 0, 0]); // italic, underline, strikeout, charset
    font.resize(4 + 92 + 4, 0);
    let text_record = f.split_off(176);
    push_record(&mut f, 82, &font);
    push_record(&mut f, 37, &3u32.to_le_bytes());
    f.extend_from_slice(&text_record);
    let page = emf::read(&f).expect("reads");
    assert!(page.text[0].bold);
    assert!(page.text[0].underline);
    assert_eq!(page.text[0].size, 88.0);
}

#[test]
fn a_picture_placement_says_where_a_stored_image_goes() {
    // The pixels live in the container, not the metafile; the metafile names
    // the picture by its stored size and gives the rectangle: the device
    // corners, then the logical origin, the source rectangle, the raster
    // operation and the logical size.
    let mut f = metafile_with_text(b"x", 0, 0);
    let mut body: Vec<i32> = vec![0; 15];
    body[1] = 400;
    body[2] = 500;
    body[3] = 1400;
    body[4] = 2000;
    body[5] = 400;
    body[6] = 500;
    body[9] = 128;
    body[10] = 64;
    body[12] = 0x0066_0046u32 as i32;
    body[13] = 1000;
    body[14] = 1500;
    let mut comment = Vec::new();
    comment.extend_from_slice(&60u32.to_le_bytes());
    comment.extend_from_slice(b"DWc\0");
    for v in &body[1..] {
        comment.extend_from_slice(&v.to_le_bytes());
    }
    push_record(&mut f, 70, &comment);
    let page = emf::read(&f).expect("reads");
    assert_eq!(page.images.len(), 1);
    let i = page.images[0];
    assert_eq!(
        (i.left, i.top, i.right, i.bottom),
        (400.0, 500.0, 1400.0, 2000.0)
    );
    assert_eq!(i.src, (128, 64));
    assert_eq!(
        i.source,
        rendering::Source::Stored {
            ordinal: 0,
            px: (128, 64)
        }
    );
    assert_eq!(i.raster_op, rendering::RasterOp::SourceInvert);
    assert_eq!(page.image_sizes(), vec![(128, 64)]);
}

// --- the older metafile flavour ------------------------------------------

/// A 16-bit metafile record: size in words, function, then the body.
fn wmf_record(d: &mut Vec<u8>, function: u16, body: &[u8]) {
    let words = (6 + body.len()) / 2;
    d.extend_from_slice(&(words as u32).to_le_bytes());
    d.extend_from_slice(&function.to_le_bytes());
    d.extend_from_slice(body);
}

fn words(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn a_page_in_the_older_flavour_yields_text_fills_and_pictures() {
    let mut d = Vec::new();
    // Header: type 1, nine words, version 0x300, then sizes.
    d.extend_from_slice(&words(&[1, 9, 0x300]));
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&words(&[0]));
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&words(&[0]));
    wmf_record(&mut d, 0x020B, &words(&[-109, -109])); // window origin: y, x
    wmf_record(&mut d, 0x020C, &words(&[7016, 4961])); // window extent: y, x
    wmf_record(&mut d, 0x012E, &words(&[24])); // baseline alignment

    // A font: LOGFONT16 with height -88, weight 700, then the face name.
    let mut font = words(&[-88, 0, 0, 0, 700]);
    font.extend_from_slice(&[0, 0, 0, 0x80, 0, 0, 0, 0]);
    font.extend_from_slice(&[0u8; 32]);
    wmf_record(&mut d, 0x02FB, &font);
    wmf_record(&mut d, 0x012D, &words(&[0]));
    // A blue brush in the next slot, selected, then a rectangle painted with it.
    let mut brush = words(&[0]);
    brush.extend_from_slice(&0x00FF_0000u32.to_le_bytes());
    brush.extend_from_slice(&words(&[0]));
    wmf_record(&mut d, 0x02FC, &brush);
    wmf_record(&mut d, 0x012D, &words(&[1]));
    let mut blt = 0x00F0_0021u32.to_le_bytes().to_vec();
    blt.extend_from_slice(&words(&[10, 20, 300, 400])); // height, width, y, x
    wmf_record(&mut d, 0x061D, &blt);
    // Text: y, x, count, options, string, advances.
    let mut text = words(&[500, 600, 2, 0]);
    text.extend_from_slice(b"Hi");
    text.extend_from_slice(&words(&[40, 40]));
    wmf_record(&mut d, 0x0A32, &text);
    // A picture placed into a clip rectangle: bottom, right, top, left.
    wmf_record(&mut d, 0x0416, &words(&[1000, 2000, 100, 200]));
    let mut mark = words(&[0x000F, 4]);
    mark.extend_from_slice(b"DW\x02\x01");
    wmf_record(&mut d, 0x0626, &mark);
    let mut place = words(&[0x000F, 4]);
    place.extend_from_slice(b"DW\x02\x02");
    wmf_record(&mut d, 0x0626, &place);
    wmf_record(&mut d, 0x0000, &[]);

    let page = wmf::read(&d, (21000, 29700)).expect("reads");
    assert_eq!(page.device, (4961, 7016));
    assert_eq!(page.frame_mm100, (21000, 29700));
    assert_eq!(page.fills.len(), 1);
    assert_eq!(page.fills[0].rgb, (0, 0, 255));
    assert_eq!(
        (page.fills[0].left, page.fills[0].top),
        (509.0, 409.0),
        "the window origin was not applied"
    );
    assert_eq!(page.text.len(), 1);
    assert_eq!(page.text[0].chars.iter().collect::<String>(), "Hi");
    assert!(page.text[0].bold);
    assert_eq!(page.text[0].xs, vec![709.0, 749.0]);
    assert_eq!(page.images.len(), 1);
    let i = page.images[0];
    assert_eq!(
        (i.left, i.top, i.right, i.bottom),
        (309.0, 209.0, 2109.0, 1109.0)
    );
    assert!(matches!(
        i.source,
        rendering::Source::Stored { ordinal: 0, .. }
    ));
}

#[test]
fn bytes_that_are_neither_flavour_are_refused() {
    assert!(wmf::read(&[0u8; 64], (21000, 29700)).is_none());
    assert!(wmf::read(b"\x01\x00\x09\x00", (21000, 29700)).is_none());
}
