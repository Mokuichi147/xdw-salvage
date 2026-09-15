//! 文書プロパティブロック（展開後）からページごとの表示属性を読む。
//!
//! The block is a flat run of `0x62` records. Each carries a level byte
//! (tag `0x80`): 2 opens a page, 3 gives a child dimension, 4 is the page's
//! own record or, later in the run, an annotation on it. Tags are BER style:
//! a first byte whose low five bits are all set is followed by continuation
//! bytes. Values are lists of length-prefixed big-endian integers.

use crate::domain::page::Overlay;
use crate::infrastructure::tlv;

const RECORD: u8 = 0x62;
const LEVEL: &[u8] = &[0x80];
const PAPER: &[u8] = &[0x85];
const DRAWING: &[u8] = &[0x87];
const ROTATION: &[u8] = &[0x9F, 0x3D];
const CHILD_SIZE: &[u8] = &[0x9F, 0x8F, 0x51];
const CHILD_POSITION: &[u8] = &[0x9F, 0x34];

const LEVEL_PAGE: u64 = 2;
const LEVEL_CHILD: u64 = 3;
const LEVEL_CONTENT: u64 = 4;

// Fields of a drawing element, laid out like a page body.
const D_KIND: u8 = 0x80;
const D_EXPANDED: u8 = 0x81;
const D_DATA: u8 = 0x86;

/// What the properties say about how one page is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageInfo {
    /// Paper as displayed, width and height in hundredths of a millimetre.
    pub paper: Option<(u32, u32)>,
    /// Clockwise rotation applied when the page is shown, in degrees.
    pub rotation: u16,
    /// Drawings laid over the page: the page's own overlay first, then any
    /// annotations, each with its position.
    pub overlays: Vec<Overlay>,
}

/// Per-page display attributes, in page order.
pub fn pages(block: &[u8]) -> Vec<PageInfo> {
    let mut out: Vec<PageInfo> = Vec::new();
    // Whether the next level-4 record is the current page's own.
    let mut expecting_content = false;
    // The size and position the last child record announced, for the
    // annotation record that follows it.
    let mut child: Option<(u32, u32, u32, u32)> = None;
    for (tag, value) in elements(block) {
        if tag != [RECORD] {
            continue;
        }
        let fields: Vec<(Vec<u8>, &[u8])> = elements(value).collect();
        let field = |want: &[u8]| fields.iter().find(|(t, _)| t == want).map(|(_, v)| *v);
        let level = field(LEVEL).and_then(uint);
        match level {
            Some(LEVEL_PAGE) => {
                out.push(PageInfo {
                    paper: field(PAPER).and_then(pair),
                    rotation: 0,
                    overlays: Vec::new(),
                });
                expecting_content = true;
                child = None;
            }
            Some(LEVEL_CHILD) => {
                child = match (
                    field(CHILD_POSITION).and_then(pair),
                    field(CHILD_SIZE).and_then(pair),
                ) {
                    (Some((x, y)), Some((w, h))) => Some((x, y, w, h)),
                    _ => None,
                };
            }
            Some(LEVEL_CONTENT) => {
                let Some(page) = out.last_mut() else { continue };
                // A level-3 child describes the box of the next drawing,
                // including the first drawing after a page record.  Most
                // documents use that pattern for a page's small placed
                // object (for example a preview/QR image); treating the
                // first one as page-sized loses its physical placement.
                let area = child.take();
                if expecting_content {
                    expecting_content = false;
                    if let Some(r) = field(ROTATION).and_then(|v| ints(v).first().copied()) {
                        page.rotation = (r % 360) as u16;
                    }
                    if page.paper.is_none() {
                        page.paper = field(PAPER).and_then(pair);
                    }
                    if let Some(o) = field(DRAWING).and_then(|v| drawing(v, area)) {
                        page.overlays.push(o);
                    }
                } else if let Some(o) = field(DRAWING).and_then(|v| drawing(v, area)) {
                    // An annotation: its drawing sits in the box the child
                    // record before it announced.
                    page.overlays.push(o);
                }
            }
            _ => {}
        }
    }
    out
}

/// A drawing element: kind, expanded length and coded bytes.
fn drawing(v: &[u8], area: Option<(u32, u32, u32, u32)>) -> Option<Overlay> {
    let fields = tlv::read_window(v, 0, v.len()).ok()?;
    let kind = tlv::find_uint(&fields, v, D_KIND)?;
    let expanded = tlv::find_uint(&fields, v, D_EXPANDED)?;
    let data = tlv::find(&fields, D_DATA)?;
    if expanded == 0 || expanded > 1 << 28 {
        return None;
    }
    Some(Overlay {
        kind,
        expanded: expanded as usize,
        coded: data.bytes(v).to_vec(),
        area,
    })
}

/// Walk the tag/length/value elements of `d`, with BER-style long tags.
fn elements(d: &[u8]) -> impl Iterator<Item = (Vec<u8>, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let first = *d.get(at)?;
        let mut tag = vec![first];
        let mut i = at + 1;
        if first & 0x1F == 0x1F {
            loop {
                let b = *d.get(i)?;
                tag.push(b);
                i += 1;
                if b & 0x80 == 0 || tag.len() > 4 {
                    break;
                }
            }
        }
        let (len, value) = tlv::read_len(d, i).ok()?;
        let end = value.checked_add(len)?;
        let v = d.get(value..end)?;
        at = end;
        Some((tag, v))
    })
}

/// The integers of a length-prefixed list.
fn ints(v: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < v.len() {
        let n = v[i] as usize;
        if n == 0 || n > 8 || i + 1 + n > v.len() {
            break;
        }
        out.push(
            v[i + 1..i + 1 + n]
                .iter()
                .fold(0u64, |a, &b| (a << 8) | u64::from(b)),
        );
        i += 1 + n;
    }
    out
}

fn uint(v: &[u8]) -> Option<u64> {
    if v.is_empty() || v.len() > 8 {
        return None;
    }
    Some(v.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b)))
}

fn pair(v: &[u8]) -> Option<(u32, u32)> {
    let list = ints(v);
    match list.as_slice() {
        [w, h, ..] if *w > 0 && *h > 0 && *w < 1 << 32 && *h < 1 << 32 => {
            Some((*w as u32, *h as u32))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &[&[u8]]) -> Vec<u8> {
        let body: Vec<u8> = fields.concat();
        let mut v = vec![RECORD, body.len() as u8];
        v.extend_from_slice(&body);
        v
    }

    #[test]
    fn a_page_record_yields_its_paper_and_the_content_record_its_rotation() {
        let mut block = record(&[&[0x80, 1, 0]]);
        block.extend(record(&[
            &[0x80, 1, 2],
            &[0x85, 7, 0x03, 0x00, 0xA4, 0x10, 0x02, 0x74, 0x04],
        ]));
        block.extend(record(&[&[0x80, 1, 3]]));
        block.extend(record(&[&[0x80, 1, 4], &[0x9F, 0x3D, 2, 1, 90]]));
        // An annotation record at level 4 must not disturb the page's values.
        block.extend(record(&[&[0x80, 1, 4], &[0x9F, 0x3D, 2, 1, 180]]));
        block.extend(record(&[
            &[0x80, 1, 2],
            &[0x85, 6, 2, 0x52, 0x08, 2, 0x74, 0x04],
        ]));
        block.extend(record(&[&[0x80, 1, 4]]));
        assert_eq!(
            pages(&block),
            vec![
                PageInfo {
                    paper: Some((42000, 29700)),
                    rotation: 90,
                    overlays: Vec::new(),
                },
                PageInfo {
                    paper: Some((21000, 29700)),
                    rotation: 0,
                    overlays: Vec::new(),
                }
            ]
        );
    }

    #[test]
    fn a_drawing_on_the_page_record_and_on_an_annotation_are_both_kept() {
        // A drawing element: kind 1, expanded 10, three coded bytes.
        let element: &[u8] = &[0x80, 1, 1, 0x81, 1, 10, 0x86, 3, 7, 8, 9];
        let mut with_drawing = vec![0x87, element.len() as u8];
        with_drawing.extend_from_slice(element);
        let mut block = record(&[&[0x80, 1, 2]]);
        // The child before the page's first content drawing is its placement
        // rectangle too, not only an annotation rectangle.
        block.extend(record(&[
            &[0x80, 1, 3],
            &[0x9F, 0x34, 6, 2, 0x03, 0xE8, 2, 0x07, 0xD0],
            &[0x9F, 0x8F, 0x51, 6, 2, 0x01, 0x2C, 2, 0x01, 0x90],
        ]));
        block.extend(record(&[&[0x80, 1, 4], &with_drawing]));
        // The next child again supplies the position and size of the
        // annotation.
        block.extend(record(&[
            &[0x80, 1, 3],
            &[0x9F, 0x34, 6, 2, 0x03, 0xE8, 2, 0x07, 0xD0],
            &[0x9F, 0x8F, 0x51, 6, 2, 0x01, 0x2C, 2, 0x01, 0x90],
        ]));
        block.extend(record(&[&[0x80, 1, 4], &with_drawing]));
        let pages = pages(&block);
        assert_eq!(pages.len(), 1);
        let overlays = &pages[0].overlays;
        assert_eq!(overlays.len(), 2);
        assert_eq!(overlays[0].area, Some((1000, 2000, 300, 400)));
        assert_eq!(overlays[0].coded, vec![7, 8, 9]);
        assert_eq!(overlays[0].expanded, 10);
        assert_eq!(overlays[1].area, Some((1000, 2000, 300, 400)));
    }

    #[test]
    fn rubbish_yields_nothing_rather_than_a_panic() {
        assert!(pages(&[0x62, 0x85, 0xFF, 0xFF, 0x00]).is_empty());
        assert!(pages(&[0x9F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).is_empty());
    }
}
