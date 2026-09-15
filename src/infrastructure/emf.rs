//! 復号後のWindows拡張メタファイルをドメインモデルへ変換するパーサー。

use crate::domain::rendering::Metafile;
use crate::infrastructure::gdi::{i32_at, rgb, u32_at, Canvas, Font, Object};

const REC_HEAD: usize = 8;

const EMR_HEADER: u32 = 1;
const EMR_MOVETOEX: u32 = 27;
const EMR_LINETO: u32 = 54;
const EMR_POLYGON: u32 = 3;
const EMR_POLYLINE: u32 = 4;
const EMR_SETWINDOWEXTEX: u32 = 9;
const EMR_SETWINDOWORGEX: u32 = 10;
const EMR_SETVIEWPORTEXTEX: u32 = 11;
const EMR_SETVIEWPORTORGEX: u32 = 12;
const EMR_EOF: u32 = 14;
const EMR_SETPOLYFILLMODE: u32 = 19;
const EMR_SETTEXTALIGN: u32 = 22;
const EMR_SETTEXTCOLOR: u32 = 24;
const EMR_INTERSECTCLIPRECT: u32 = 30;
const EMR_SELECTOBJECT: u32 = 37;
const EMR_CREATEPEN: u32 = 38;
const EMR_CREATEBRUSHINDIRECT: u32 = 39;
const EMR_DELETEOBJECT: u32 = 40;
const EMR_ELLIPSE: u32 = 42;
const EMR_RECTANGLE: u32 = 43;
const EMR_GDICOMMENT: u32 = 70;
const EMR_BITBLT: u32 = 76;
const EMR_STRETCHDIBITS: u32 = 81;
const EMR_EXTCREATEFONTINDIRECTW: u32 = 82;
const EMR_EXTTEXTOUTA: u32 = 83;
const EMR_EXTTEXTOUTW: u32 = 84;
const EMR_POLYGON16: u32 = 86;
const EMR_POLYLINE16: u32 = 87;
const EMR_EXTCREATEPEN: u32 = 95;

const PATCOPY: u32 = 0x00F0_0021;
const BS_NULL: u32 = 1;
const PS_NULL: u32 = 5;

/// EMFを読み取り、ドメインの描画モデルへ変換する。
pub fn read(d: &[u8]) -> Option<Metafile> {
    if d.get(40..44)? != b" EMF" {
        return None;
    }
    let mut c = Canvas::new(
        (i32_at(d, 72)?, i32_at(d, 76)?),
        (
            i32_at(d, 32)?.saturating_sub(i32_at(d, 24)?),
            i32_at(d, 36)?.saturating_sub(i32_at(d, 28)?),
        ),
    );
    c.page.records = u32_at(d, 52)?;

    let mut at = 0usize;
    let mut guard = d.len() / REC_HEAD + 2;
    let mut moved: Option<(i32, i32)> = Some((0, 0));
    while at + REC_HEAD <= d.len() && guard > 0 {
        guard -= 1;
        let kind = u32_at(d, at)?;
        let size = u32_at(d, at + 4)? as usize;
        if size < REC_HEAD || at + size > d.len() {
            break;
        }
        let r = &d[at..at + size];
        match kind {
            EMR_HEADER | EMR_EOF => {}
            EMR_SETWINDOWORGEX => {
                if let (Some(x), Some(y)) = (i32_at(r, 8), i32_at(r, 12)) {
                    c.set_window_org(x, y);
                }
            }
            EMR_SETVIEWPORTORGEX => {
                if let (Some(x), Some(y)) = (i32_at(r, 8), i32_at(r, 12)) {
                    c.set_viewport_org(x, y);
                }
            }
            EMR_SETWINDOWEXTEX => {
                if let (Some(x), Some(y)) = (i32_at(r, 8), i32_at(r, 12)) {
                    c.set_window_ext(x, y);
                }
            }
            EMR_SETVIEWPORTEXTEX => {
                if let (Some(x), Some(y)) = (i32_at(r, 8), i32_at(r, 12)) {
                    c.set_viewport_ext(x, y);
                }
            }
            EMR_SETTEXTALIGN => c.set_text_align(u32_at(r, 8).unwrap_or(0)),
            EMR_SETTEXTCOLOR => c.set_text_colour(u32_at(r, 8).unwrap_or(0)),
            EMR_SETPOLYFILLMODE => c.set_poly_fill_mode(u32_at(r, 8).unwrap_or(1)),
            EMR_INTERSECTCLIPRECT => {
                if let (Some(l), Some(t), Some(rt), Some(b)) =
                    (i32_at(r, 8), i32_at(r, 12), i32_at(r, 16), i32_at(r, 20))
                {
                    c.set_clip_rect(l, t, rt, b);
                }
            }
            EMR_MOVETOEX => {
                if let (Some(x), Some(y)) = (i32_at(r, 8), i32_at(r, 12)) {
                    moved = Some((x, y));
                }
            }
            EMR_LINETO => {
                if let (Some((from_x, from_y)), Some(x), Some(y)) =
                    (moved, i32_at(r, 8), i32_at(r, 12))
                {
                    c.polygon(&[(from_x, from_y), (x, y)], false);
                    moved = Some((x, y));
                }
            }
            EMR_EXTCREATEFONTINDIRECTW => {
                if let (Some(h), Some(height), Some(esc), Some(weight)) =
                    (u32_at(r, 8), i32_at(r, 12), i32_at(r, 20), i32_at(r, 28))
                {
                    c.create(
                        h,
                        Object::Font(Font {
                            height,
                            escapement: esc,
                            weight,
                            underline: r.get(33).is_some_and(|&u| u != 0),
                        }),
                    );
                }
            }
            EMR_CREATEBRUSHINDIRECT => {
                if let (Some(h), Some(style), Some(colour)) =
                    (u32_at(r, 8), u32_at(r, 12), u32_at(r, 16))
                {
                    c.create(h, Object::Brush((style != BS_NULL).then(|| rgb(colour))));
                }
            }
            EMR_CREATEPEN => {
                if let (Some(h), Some(style), Some(width), Some(colour)) =
                    (u32_at(r, 8), u32_at(r, 12), i32_at(r, 16), u32_at(r, 24))
                {
                    c.create(h, Object::Pen(pen(style, width, colour, &c)));
                }
            }
            EMR_EXTCREATEPEN => {
                // ihPen, offBmi, cbBmi, offBits, cbBits, then the pen itself:
                // style, width, brush style, colour, hatch.
                if let (Some(h), Some(style), Some(width), Some(colour)) =
                    (u32_at(r, 8), u32_at(r, 28), i32_at(r, 32), u32_at(r, 40))
                {
                    c.create(h, Object::Pen(pen(style, width, colour, &c)));
                }
            }
            EMR_SELECTOBJECT => {
                if let Some(h) = u32_at(r, 8) {
                    c.select(h);
                }
            }
            EMR_DELETEOBJECT => {
                if let Some(h) = u32_at(r, 8) {
                    c.delete(h);
                }
            }
            EMR_BITBLT => {
                if u32_at(r, 40) == Some(PATCOPY) {
                    if let (Some(x), Some(y), Some(cx), Some(cy)) =
                        (i32_at(r, 24), i32_at(r, 28), i32_at(r, 32), i32_at(r, 36))
                    {
                        c.pat_fill(x, y, cx, cy);
                    }
                } else {
                    c.skip(kind);
                }
            }
            EMR_STRETCHDIBITS => {
                if !stretch_dibits(r, &mut c) {
                    c.skip(kind);
                }
            }
            EMR_RECTANGLE => {
                if let (Some(l), Some(t), Some(rt), Some(b)) =
                    (i32_at(r, 8), i32_at(r, 12), i32_at(r, 16), i32_at(r, 20))
                {
                    c.polygon(&[(l, t), (rt, t), (rt, b), (l, b)], true);
                }
            }
            EMR_ELLIPSE => {
                if let (Some(l), Some(t), Some(rt), Some(b)) =
                    (i32_at(r, 8), i32_at(r, 12), i32_at(r, 16), i32_at(r, 20))
                {
                    c.ellipse(l, t, rt, b);
                }
            }
            EMR_POLYGON | EMR_POLYLINE => {
                if let Some(pts) = points32(r) {
                    c.polygon(&pts, kind == EMR_POLYGON);
                }
            }
            EMR_POLYGON16 | EMR_POLYLINE16 => {
                if let Some(pts) = points16(r) {
                    c.polygon(&pts, kind == EMR_POLYGON16);
                }
            }
            EMR_EXTTEXTOUTA | EMR_EXTTEXTOUTW => {
                if !text_out(r, kind == EMR_EXTTEXTOUTW, &mut c) {
                    c.skip(kind);
                }
            }
            EMR_GDICOMMENT => {
                let len = u32_at(r, 8).unwrap_or(0) as usize;
                let body = r.get(12..12 + len.min(r.len().saturating_sub(12)));
                if !body.is_some_and(|b| c.comment(b)) {
                    c.skip(kind);
                }
            }
            other => c.skip(other),
        }
        at += size;
    }
    Some(c.finish())
}

/// A pen from its style, logical width and colour.
fn pen(style: u32, width: i32, colour: u32, c: &Canvas) -> Option<((u8, u8, u8), f32)> {
    if style & 0xF == PS_NULL {
        return None;
    }
    // Width 0 is the thinnest line the device can draw.
    Some((rgb(colour), c.device_len(width.max(1) as f32)))
}

/// Points of a 32-bit polygon record: bounds, count, then the points.
fn points32(r: &[u8]) -> Option<Vec<(i32, i32)>> {
    let n = u32_at(r, 24)? as usize;
    if n > 1 << 20 {
        return None;
    }
    (0..n)
        .map(|i| Some((i32_at(r, 28 + i * 8)?, i32_at(r, 32 + i * 8)?)))
        .collect()
}

/// Points of a 16-bit polygon record.
fn points16(r: &[u8]) -> Option<Vec<(i32, i32)>> {
    let n = u32_at(r, 24)? as usize;
    if n > 1 << 20 {
        return None;
    }
    (0..n)
        .map(|i| {
            Some((
                i32::from(crate::infrastructure::gdi::i16_at(r, 28 + i * 4)?),
                i32::from(crate::infrastructure::gdi::i16_at(r, 30 + i * 4)?),
            ))
        })
        .collect()
}

fn stretch_dibits(r: &[u8], c: &mut Canvas) -> bool {
    let f = |i: usize| i32_at(r, 24 + i * 4);
    let (Some(xd), Some(yd), Some(xs), Some(ys), Some(cxs), Some(cys)) =
        (f(0), f(1), f(2), f(3), f(4), f(5))
    else {
        return false;
    };
    let (Some(off_bmi), Some(cb_bmi), Some(off_bits), Some(cb_bits)) = (
        u32_at(r, 48).map(|v| v as usize),
        u32_at(r, 52).map(|v| v as usize),
        u32_at(r, 56).map(|v| v as usize),
        u32_at(r, 60).map(|v| v as usize),
    ) else {
        return false;
    };
    let (Some(rop), Some(cxd), Some(cyd)) = (u32_at(r, 68), i32_at(r, 72), i32_at(r, 76)) else {
        return false;
    };
    let (Some(info), Some(bits)) = (
        r.get(off_bmi..off_bmi.saturating_add(cb_bmi)),
        r.get(off_bits..off_bits.saturating_add(cb_bits)),
    ) else {
        return false;
    };
    c.stretch_dib(info, bits, (xd, yd, cxd, cyd), (xs, ys, cxs, cys), rop)
}

fn text_out(r: &[u8], wide: bool, c: &mut Canvas) -> bool {
    let (Some(x), Some(y), Some(n), Some(off_string), Some(off_dx)) = (
        i32_at(r, 36),
        i32_at(r, 40),
        u32_at(r, 44).map(|v| v as usize),
        u32_at(r, 48).map(|v| v as usize),
        u32_at(r, 72).map(|v| v as usize),
    ) else {
        return false;
    };
    if n == 0 || n > 1 << 16 {
        return n == 0;
    }
    let width = if wide { 2 } else { 1 };
    let Some(bytes) = off_string
        .checked_add(n * width)
        .and_then(|end| r.get(off_string..end))
    else {
        return false;
    };
    let chars: Vec<char> = if wide {
        let (pairs, _) = bytes.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units).chars().collect()
    } else {
        crate::infrastructure::cp932::decode(bytes)
    };
    let dx: Option<&[u8]> = off_dx.checked_add(n * 4).and_then(|end| r.get(off_dx..end));
    let mut advances = Vec::with_capacity(chars.len());
    if wide {
        for i in 0..chars.len() {
            advances.push(dx.and_then(|d| i32_at(d, i * 4)).unwrap_or(0) as f32);
        }
    } else {
        // The advance table has one entry per byte; a double-byte character
        // takes the sum of its two.
        let mut i = 0usize;
        for ch in &chars {
            let taken = crate::infrastructure::cp932::byte_len(*ch);
            let mut step = 0f32;
            for k in 0..taken {
                step += dx.and_then(|d| i32_at(d, (i + k) * 4)).unwrap_or(0) as f32;
            }
            i += taken;
            advances.push(step);
        }
    }
    c.text(x, y, chars, &advances);
    true
}
