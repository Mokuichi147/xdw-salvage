//! The parser is pointed at whole archives of files nobody has inspected.
//!
//! A migration that panics halfway through is worse than one that reports a
//! file it cannot read, so the rule here is simple: for any input at all,
//! parsing either succeeds or returns an error, and never panics, hangs or
//! allocates without bound. These tests take valid containers apart in every
//! way a damaged file might be damaged and check that rule holds.

use xdw_salvage::adapters::pdf;
use xdw_salvage::application::verification as verify;
use xdw_salvage::infrastructure::{attachments as attach, xdw_document::parse};

/// A small deterministic generator, so a failure can be reproduced from the
/// seed printed in the panic message.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

fn elem(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = value.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n <= 0xFF {
        out.extend_from_slice(&[0x81, n as u8]);
    } else {
        out.push(0x82);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    }
    out.extend_from_slice(value);
    out
}

/// A small but complete container: one page with metadata, one raw page, a
/// properties block and a trailer.
fn fixture() -> Vec<u8> {
    let mut body = Vec::new();
    let mut meta = elem(0x80, &[4]);
    meta.extend_from_slice(&elem(0x81, &2000u16.to_be_bytes()));
    meta.extend_from_slice(&elem(0x84, &21000u16.to_be_bytes()));
    meta.extend_from_slice(&elem(0x85, &29700u16.to_be_bytes()));
    meta.extend_from_slice(&elem(0x89, &32u16.to_be_bytes()));
    meta.extend_from_slice(&elem(0x86, &[0x5A; 32]));
    let page_a = elem(
        0x64,
        &[elem(0x81, &[1, 2, 3, 4]), elem(0x82, &meta)].concat(),
    );

    let mut raw = Vec::new();
    raw.extend_from_slice(&0u32.to_le_bytes()); // fixed up below
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&8u32.to_le_bytes());
    raw.extend_from_slice(&8u32.to_le_bytes());
    raw.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xD9]);
    let len = raw.len() as u32;
    raw[..4].copy_from_slice(&len.to_le_bytes());
    let page_b = elem(
        0x64,
        &[elem(0x81, &[5, 6, 7, 8]), elem(0x82, &raw)].concat(),
    );

    let header = elem(
        0x60,
        &[
            elem(0x82, &[10]),
            elem(0x80, &[0x00, 0xC0, 0x13]),
            elem(0x83, &[0x01, 0x0D, 0x0A, 0x01]),
        ]
        .concat(),
    );

    for doc_hdr in [2usize, 3, 4] {
        let base = header.len() + doc_hdr;
        body.clear();
        let mut offsets = Vec::new();
        offsets.push(base as u32);
        body.extend_from_slice(&page_a);
        offsets.push((base + body.len()) as u32);
        body.extend_from_slice(&page_b);
        body.extend_from_slice(&elem(0x63, &[0xAB; 8]));

        let mut tr = elem(0x80, &[offsets.len() as u8]);
        tr.extend_from_slice(&elem(
            0x81,
            &offsets
                .iter()
                .flat_map(|o| o.to_le_bytes())
                .collect::<Vec<u8>>(),
        ));
        tr.extend_from_slice(&elem(0x83, &32u16.to_be_bytes()));
        tr.extend_from_slice(&elem(0x84, &8u16.to_be_bytes()));
        tr.extend_from_slice(&elem(0x85, &[0xAA, 0xBB, 0xCC, 0xDD]));
        let self_len = tr.len() + 6;
        tr.extend_from_slice(&elem(0x86, &(self_len as u32).to_le_bytes()));
        body.extend_from_slice(&elem(0x68, &tr));

        let doc = elem(0x61, &body);
        if doc.len() - body.len() == doc_hdr {
            return [header, doc].concat();
        }
    }
    unreachable!("could not lay out the fixture container")
}

/// Parse, and if that works use the result the way the tools do. Anything that
/// panics fails the test; an error is a perfectly good outcome.
fn exercise(bytes: &[u8]) {
    let Ok(doc) = parse(bytes) else {
        return;
    };
    let _ = doc.coverage();
    let _ = doc.content_pages().count();
    for p in &doc.pages {
        let _ = p.describe();
        let _ = p.paper_points();
        let _ = p.kind_name();
    }
    let _ = verify::Expectation::of(&doc);
    let _ = attach::scan(bytes);
    let (pdf_bytes, _) = pdf::build(bytes, &doc, pdf::Options::default());
    assert!(pdf_bytes.starts_with(b"%PDF"), "produced a broken PDF");
}

#[test]
fn the_fixture_itself_is_sound() {
    let good = fixture();
    let doc = parse(&good).expect("the fixture must parse");
    assert_eq!(doc.pages.len(), 2);
    exercise(&good);
}

#[test]
fn single_byte_corruption_never_panics() {
    let good = fixture();
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for _ in 0..4000 {
        let mut bad = good.clone();
        let at = rng.below(bad.len());
        bad[at] ^= 1 << rng.below(8);
        exercise(&bad);
    }
}

#[test]
fn truncation_at_every_length_never_panics() {
    let good = fixture();
    for cut in 0..good.len() {
        exercise(&good[..cut]);
    }
}

#[test]
fn wholesale_corruption_never_panics() {
    let good = fixture();
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..2000 {
        let mut bad = good.clone();
        for _ in 0..rng.below(24) + 1 {
            let at = rng.below(bad.len());
            bad[at] = (rng.next() & 0xFF) as u8;
        }
        exercise(&bad);
    }
}

#[test]
fn random_bytes_are_rejected_not_crashed_on() {
    let mut rng = Rng(0x0BAD_F00D_1234_5678);
    for len in [0usize, 1, 2, 7, 8, 16, 64, 513, 4096] {
        for _ in 0..200 {
            let junk: Vec<u8> = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
            exercise(&junk);
        }
    }
}

#[test]
fn a_length_field_claiming_the_moon_is_refused() {
    // A four-octet length of 0xFFFFFFFF on a tiny file.
    let mut bytes = vec![0x60, 0x84, 0xFF, 0xFF, 0xFF, 0xFF];
    bytes.extend_from_slice(&[0u8; 16]);
    assert!(parse(&bytes).is_err());
    exercise(&bytes);
}

#[test]
fn page_offsets_pointing_outside_the_file_are_refused() {
    let mut good = fixture();
    // The offset table sits in the trailer; push every entry past the end.
    let n = good.len();
    let tlen = u32::from_le_bytes([good[n - 4], good[n - 3], good[n - 2], good[n - 1]]) as usize;
    let start = n - tlen;
    for i in start..n - 6 {
        if good[i] == 0x81 {
            let count = good[i + 1] as usize;
            for k in 0..count {
                let at = i + 2 + k;
                if at + 4 <= n {
                    good[at..at + 4].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
                }
            }
            break;
        }
    }
    exercise(&good);
}

#[test]
fn an_offset_table_of_absurd_size_does_not_allocate_the_world() {
    // Claim a hundred thousand pages in a file that is a few hundred bytes.
    let mut table = Vec::new();
    for i in 0..100_000u32 {
        table.extend_from_slice(&i.to_le_bytes());
    }
    let mut tr = elem(0x80, &[0xFF]);
    tr.extend_from_slice(&elem(0x81, &table));
    let self_len = tr.len() + 6;
    tr.extend_from_slice(&elem(0x86, &(self_len as u32).to_le_bytes()));
    let header = elem(
        0x60,
        &[elem(0x82, &[10]), elem(0x83, &[0x01, 0x0D, 0x0A, 0x01])].concat(),
    );
    let body = elem(0x68, &tr);
    let bytes = [header, elem(0x61, &body)].concat();
    exercise(&bytes);
}
