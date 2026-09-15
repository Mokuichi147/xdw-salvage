//! 復号後のWindowsメタファイル（16ビット形式）をドメインモデルへ変換するパーサー。
//!
//! Pages of one storage kind expand to this older flavour rather than EMF.
//! Records are counted in 16-bit words, coordinates are 16-bit, and objects
//! are addressed by slot number rather than handle.

use crate::domain::rendering::Metafile;
use crate::infrastructure::gdi::{i16_at, rgb, u16_at, u32_at, Canvas, Font, Object};

const HEADER_WORDS: usize = 9;

const META_SETBKMODE: u16 = 0x0102;
const META_SETPOLYFILLMODE: u16 = 0x0106;
const META_SETTEXTALIGN: u16 = 0x012E;
const META_SETTEXTCOLOR: u16 = 0x0209;
const META_SETWINDOWORG: u16 = 0x020B;
const META_SETWINDOWEXT: u16 = 0x020C;
const META_SETVIEWPORTORG: u16 = 0x020D;
const META_SETVIEWPORTEXT: u16 = 0x020E;
const META_LINETO: u16 = 0x0213;
const META_MOVETO: u16 = 0x0214;
const META_INTERSECTCLIPRECT: u16 = 0x0416;
const META_POLYGON: u16 = 0x0324;
const META_POLYLINE: u16 = 0x0325;
const META_RECTANGLE: u16 = 0x041B;
const META_ELLIPSE: u16 = 0x0418;
const META_ESCAPE: u16 = 0x0626;
const META_PATBLT: u16 = 0x061D;
const META_TEXTOUT: u16 = 0x0521;
const META_EXTTEXTOUT: u16 = 0x0A32;
const META_STRETCHDIB: u16 = 0x0F43;
const META_CREATEPENINDIRECT: u16 = 0x02FA;
const META_CREATEFONTINDIRECT: u16 = 0x02FB;
const META_CREATEBRUSHINDIRECT: u16 = 0x02FC;
const META_SELECTOBJECT: u16 = 0x012D;
const META_DELETEOBJECT: u16 = 0x01F0;
const META_EOF: u16 = 0x0000;
/// A record of the maker's own that creates an object of some kind.
const META_DW_CREATE: u16 = 0x06FF;

const MFCOMMENT: u16 = 0x000F;
const PATCOPY: u32 = 0x00F0_0021;
const BS_NULL: u16 = 1;
const PS_NULL: u16 = 5;

/// Whether the bytes start with a metafile header of this flavour.
pub fn looks_like(d: &[u8]) -> bool {
    d.len() >= HEADER_WORDS * 2
        && u16_at(d, 0) == Some(1)
        && u16_at(d, 2) == Some(HEADER_WORDS as u16)
        && matches!(u16_at(d, 4), Some(0x0100) | Some(0x0300))
}

/// WMFを読み取り、ドメインの描画モデルへ変換する。
///
/// The file carries no page size of its own, so the window extent is taken
/// as the device size and `paper_mm100` as the frame.
pub fn read(d: &[u8], paper_mm100: (i32, i32)) -> Option<Metafile> {
    if !looks_like(d) {
        return None;
    }
    let mut c = Canvas::new((0, 0), paper_mm100);
    c.page.records = u16_at(d, 14).map(u32::from).unwrap_or(0);
    // Object slots: a create takes the lowest free one.
    let mut slots: Vec<bool> = Vec::new();
    let mut moved: (i32, i32) = (0, 0);

    let mut at = HEADER_WORDS * 2;
    let mut guard = d.len() / 6 + 2;
    while at + 6 <= d.len() && guard > 0 {
        guard -= 1;
        let size = u32_at(d, at)? as usize * 2;
        let kind = u16_at(d, at + 4)?;
        if size < 6 || at + size > d.len() {
            break;
        }
        let r = &d[at..at + size];
        let p = |i: usize| i16_at(r, 6 + i * 2).map(i32::from);
        match kind {
            META_EOF => break,
            META_SETWINDOWORG => {
                if let (Some(y), Some(x)) = (p(0), p(1)) {
                    c.set_window_org(x, y);
                }
            }
            META_SETWINDOWEXT => {
                if let (Some(y), Some(x)) = (p(0), p(1)) {
                    c.set_window_ext(x, y);
                    if c.page.device == (0, 0) {
                        c.page.device = (x.abs(), y.abs());
                    }
                }
            }
            META_SETVIEWPORTORG => {
                if let (Some(y), Some(x)) = (p(0), p(1)) {
                    c.set_viewport_org(x, y);
                }
            }
            META_SETVIEWPORTEXT => {
                if let (Some(y), Some(x)) = (p(0), p(1)) {
                    c.set_viewport_ext(x, y);
                }
            }
            META_SETTEXTALIGN => c.set_text_align(u16_at(r, 6).map(u32::from).unwrap_or(0)),
            META_SETTEXTCOLOR => c.set_text_colour(u32_at(r, 6).unwrap_or(0)),
            META_SETPOLYFILLMODE => c.set_poly_fill_mode(u16_at(r, 6).map(u32::from).unwrap_or(1)),
            META_SETBKMODE => {}
            META_INTERSECTCLIPRECT => {
                if let (Some(b), Some(rt), Some(t), Some(l)) = (p(0), p(1), p(2), p(3)) {
                    c.set_clip_rect(l, t, rt, b);
                }
            }
            META_CREATEFONTINDIRECT => {
                // LOGFONT16: height, width, escapement, orientation, weight,
                // italic, underline, strikeout, charset, ... face name.
                if let (Some(height), Some(esc), Some(weight)) = (p(0), p(2), p(4)) {
                    create(
                        &mut slots,
                        &mut c,
                        Object::Font(Font {
                            height,
                            escapement: esc,
                            weight,
                            underline: r.get(6 + 11).is_some_and(|&u| u != 0),
                        }),
                    );
                }
            }
            META_CREATEBRUSHINDIRECT => {
                if let (Some(style), Some(colour)) = (u16_at(r, 6), u32_at(r, 8)) {
                    create(
                        &mut slots,
                        &mut c,
                        Object::Brush((style != BS_NULL).then(|| rgb(colour))),
                    );
                }
            }
            META_CREATEPENINDIRECT => {
                // style, width (x, y), colour.
                if let (Some(style), Some(width), Some(colour)) =
                    (u16_at(r, 6), p(1), u32_at(r, 12))
                {
                    let pen = (style & 0xF != PS_NULL)
                        .then(|| (rgb(colour), c.device_len(width.max(1) as f32)));
                    create(&mut slots, &mut c, Object::Pen(pen));
                }
            }
            META_DW_CREATE => create(&mut slots, &mut c, Object::Opaque),
            META_SELECTOBJECT => {
                if let Some(slot) = u16_at(r, 6) {
                    c.select(u32::from(slot));
                }
            }
            META_DELETEOBJECT => {
                if let Some(slot) = u16_at(r, 6) {
                    c.delete(u32::from(slot));
                    if let Some(used) = slots.get_mut(usize::from(slot)) {
                        *used = false;
                    }
                }
            }
            META_PATBLT => {
                // rop, height, width, y, x.
                if u32_at(r, 6) == Some(PATCOPY) {
                    if let (Some(h), Some(w), Some(y), Some(x)) = (p(2), p(3), p(4), p(5)) {
                        c.pat_fill(x, y, w, h);
                    }
                } else {
                    c.skip(u32::from(kind));
                }
            }
            META_RECTANGLE => {
                if let (Some(b), Some(rt), Some(t), Some(l)) = (p(0), p(1), p(2), p(3)) {
                    c.polygon(&[(l, t), (rt, t), (rt, b), (l, b)], true);
                }
            }
            META_ELLIPSE => {
                if let (Some(b), Some(rt), Some(t), Some(l)) = (p(0), p(1), p(2), p(3)) {
                    c.ellipse(l, t, rt, b);
                }
            }
            META_POLYGON | META_POLYLINE => {
                if let Some(n) = u16_at(r, 6).map(usize::from) {
                    let pts: Option<Vec<(i32, i32)>> = (0..n)
                        .map(|i| Some((p(1 + i * 2)?, p(2 + i * 2)?)))
                        .collect();
                    if let Some(pts) = pts {
                        c.polygon(&pts, kind == META_POLYGON);
                    }
                }
            }
            META_MOVETO => {
                if let (Some(y), Some(x)) = (p(0), p(1)) {
                    moved = (x, y);
                }
            }
            META_LINETO => {
                if let (Some(y), Some(x)) = (p(0), p(1)) {
                    c.polygon(&[moved, (x, y)], false);
                    moved = (x, y);
                }
            }
            META_STRETCHDIB => {
                if !stretch_dib(r, &mut c) {
                    c.skip(u32::from(kind));
                }
            }
            META_EXTTEXTOUT => {
                if !ext_text_out(r, &mut c) {
                    c.skip(u32::from(kind));
                }
            }
            META_TEXTOUT => {
                if !text_out(r, &mut c) {
                    c.skip(u32::from(kind));
                }
            }
            META_ESCAPE => {
                let understood = match (u16_at(r, 6), u16_at(r, 8)) {
                    (Some(MFCOMMENT), Some(len)) => r
                        .get(10..10 + usize::from(len).min(r.len().saturating_sub(10)))
                        .is_some_and(|body| c.comment(body)),
                    _ => false,
                };
                if !understood {
                    c.skip(u32::from(kind));
                }
            }
            other => c.skip(u32::from(other)),
        }
        at += size;
    }
    Some(c.finish())
}

/// Put an object in the lowest free slot, as GDI numbers them.
fn create(slots: &mut Vec<bool>, c: &mut Canvas, object: Object) {
    let slot = slots.iter().position(|used| !used).unwrap_or(slots.len());
    if slot == slots.len() {
        slots.push(true);
    } else {
        slots[slot] = true;
    }
    c.create(slot as u32, object);
}

fn stretch_dib(r: &[u8], c: &mut Canvas) -> bool {
    // rop, usage, then source height, width, y, x and destination height,
    // width, y, x, then the bitmap.
    let p = |i: usize| i16_at(r, 12 + i * 2).map(i32::from);
    let (Some(sh), Some(sw), Some(sy), Some(sx), Some(dh), Some(dw), Some(dy), Some(dx)) =
        (p(0), p(1), p(2), p(3), p(4), p(5), p(6), p(7))
    else {
        return false;
    };
    let Some(info) = r.get(28..) else {
        return false;
    };
    let Some(header) = crate::infrastructure::dib::header(info) else {
        return false;
    };
    let Some(bits) = info.get(header.info_len..) else {
        return false;
    };
    let rop = u32_at(r, 6).unwrap_or(0);
    c.stretch_dib(info, bits, (dx, dy, dw, dh), (sx, sy, sw, sh), rop)
}

fn ext_text_out(r: &[u8], c: &mut Canvas) -> bool {
    const ETO_OPAQUE: u16 = 2;
    const ETO_CLIPPED: u16 = 4;
    let (Some(y), Some(x), Some(n), Some(options)) = (
        i16_at(r, 6).map(i32::from),
        i16_at(r, 8).map(i32::from),
        u16_at(r, 10).map(usize::from),
        u16_at(r, 12),
    ) else {
        return false;
    };
    if n == 0 {
        return true;
    }
    let mut at = 14usize;
    if options & (ETO_OPAQUE | ETO_CLIPPED) != 0 {
        at += 8;
    }
    let Some(bytes) = r.get(at..at + n) else {
        return false;
    };
    let chars = crate::infrastructure::cp932::decode(bytes);
    // The string is padded to a word; the advance table follows if present.
    let dx_at = at + n + (n & 1);
    let dx: Option<&[u8]> = r.get(dx_at..dx_at + n * 2);
    let mut advances = Vec::with_capacity(chars.len());
    let mut i = 0usize;
    for ch in &chars {
        let taken = crate::infrastructure::cp932::byte_len(*ch);
        let mut step = 0f32;
        for k in 0..taken {
            step += dx
                .and_then(|d| i16_at(d, (i + k) * 2))
                .map(f32::from)
                .unwrap_or(0.0);
        }
        i += taken;
        advances.push(step);
    }
    if dx.is_none() {
        // No advances recorded: space the characters by the font height, the
        // best guess available without the face.
        let step = c.font_height().max(1.0);
        advances = chars
            .iter()
            .map(|ch| {
                if crate::infrastructure::cp932::byte_len(*ch) == 1 {
                    step * 0.5
                } else {
                    step
                }
            })
            .collect();
    }
    c.text(x, y, chars, &advances);
    true
}

fn text_out(r: &[u8], c: &mut Canvas) -> bool {
    let Some(n) = u16_at(r, 6).map(usize::from) else {
        return false;
    };
    let Some(bytes) = r.get(8..8 + n) else {
        return false;
    };
    let after = 8 + n + (n & 1);
    let (Some(y), Some(x)) = (
        i16_at(r, after).map(i32::from),
        i16_at(r, after + 2).map(i32::from),
    ) else {
        return false;
    };
    let chars = crate::infrastructure::cp932::decode(bytes);
    let step = c.font_height().max(1.0);
    let advances: Vec<f32> = chars
        .iter()
        .map(|ch| {
            if crate::infrastructure::cp932::byte_len(*ch) == 1 {
                step * 0.5
            } else {
                step
            }
        })
        .collect();
    c.text(x, y, chars, &advances);
    true
}
