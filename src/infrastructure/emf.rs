//! 復号後のWindows拡張メタファイルをドメインモデルへ変換するパーサー。

use std::collections::HashMap;

use crate::domain::rendering::{Fill, Image, Metafile, Text};

const REC_HEAD: usize = 8;

const EMR_HEADER: u32 = 1;
const EMR_SETTEXTALIGN: u32 = 22;
const EMR_SETTEXTCOLOR: u32 = 24;
const EMR_SELECTOBJECT: u32 = 37;
const EMR_DELETEOBJECT: u32 = 40;
const EMR_EXTCREATEFONTINDIRECTW: u32 = 82;
const EMR_EXTTEXTOUTA: u32 = 83;
const EMR_EXTTEXTOUTW: u32 = 84;
const EMR_GDICOMMENT: u32 = 70;
const EMR_CREATEBRUSHINDIRECT: u32 = 39;
const EMR_BITBLT: u32 = 76;

const PATCOPY: u32 = 0x00F0_0021;
const BS_NULL: u32 = 1;
const DW_SHAPE_MASK: [u32; 2] = [0x8000_5744, 0x8002_5744];
const TA_BASELINE: u32 = 24;
const TA_BOTTOM: u32 = 8;

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn i32_at(d: &[u8], at: usize) -> Option<i32> {
    u32_at(d, at).map(|v| v as i32)
}

#[derive(Clone, Copy, Default)]
struct Font {
    height: i32,
    escapement: i32,
}

/// EMFを読み取り、ドメインの描画モデルへ変換する。
pub fn read(d: &[u8]) -> Option<Metafile> {
    if d.get(40..44)? != b" EMF" {
        return None;
    }
    let mut page = Metafile {
        device: (i32_at(d, 72)?, i32_at(d, 76)?),
        frame_mm100: (
            i32_at(d, 32)?.saturating_sub(i32_at(d, 24)?),
            i32_at(d, 36)?.saturating_sub(i32_at(d, 28)?),
        ),
        records: u32_at(d, 52)?,
        ..Default::default()
    };

    let mut fonts: HashMap<u32, Font> = HashMap::new();
    let mut brushes: HashMap<u32, Option<(u8, u8, u8)>> = HashMap::new();
    let mut brush: Option<(u8, u8, u8)> = None;
    let mut masked = false;
    let mut current = Font::default();
    let mut align: u32 = 0;
    let mut rgb = (0u8, 0u8, 0u8);

    let mut at = 0usize;
    let mut order = 0usize;
    let mut guard = d.len() / REC_HEAD + 2;
    while at + REC_HEAD <= d.len() && guard > 0 {
        guard -= 1;
        order += 1;
        let kind = u32_at(d, at)?;
        let size = u32_at(d, at + 4)? as usize;
        if size < REC_HEAD || at + size > d.len() {
            break;
        }
        let r = &d[at..at + size];
        match kind {
            EMR_HEADER => {}
            EMR_SETTEXTALIGN => align = u32_at(r, 8).unwrap_or(0),
            EMR_SETTEXTCOLOR => {
                let c = u32_at(r, 8).unwrap_or(0);
                rgb = (c as u8, (c >> 8) as u8, (c >> 16) as u8);
            }
            EMR_EXTCREATEFONTINDIRECTW => {
                if let (Some(h), Some(height), Some(esc)) =
                    (u32_at(r, 8), i32_at(r, 12), i32_at(r, 20))
                {
                    fonts.insert(
                        h,
                        Font {
                            height,
                            escapement: esc,
                        },
                    );
                }
            }
            EMR_CREATEBRUSHINDIRECT => {
                if let (Some(h), Some(style), Some(c)) =
                    (u32_at(r, 8), u32_at(r, 12), u32_at(r, 16))
                {
                    brushes.insert(
                        h,
                        (style != BS_NULL).then_some((c as u8, (c >> 8) as u8, (c >> 16) as u8)),
                    );
                }
            }
            EMR_SELECTOBJECT => {
                if let Some(h) = u32_at(r, 8) {
                    if let Some(f) = fonts.get(&h) {
                        current = *f;
                    }
                    if let Some(b) = brushes.get(&h) {
                        brush = *b;
                    }
                }
            }
            EMR_DELETEOBJECT => {
                if let Some(h) = u32_at(r, 8) {
                    fonts.remove(&h);
                    brushes.remove(&h);
                }
            }
            EMR_BITBLT => match fill_rect(r, brush, order, masked) {
                Some(f) => page.fills.push(f),
                None => *page.skipped.entry(kind).or_insert(0) += 1,
            },
            EMR_EXTTEXTOUTA | EMR_EXTTEXTOUTW => {
                if let Some(t) = text_out(r, kind == EMR_EXTTEXTOUTW, &current, align, rgb, order) {
                    if !t.chars.is_empty() {
                        page.text.push(t);
                    }
                }
            }
            EMR_GDICOMMENT => {
                if is_shape_mask(r) {
                    page.shape_masks += 1;
                    masked = true;
                }
                match picture_placement(r, order) {
                    Some(img) => page.images.push(img),
                    None => *page.skipped.entry(kind).or_insert(0) += 1,
                }
            }
            other => *page.skipped.entry(other).or_insert(0) += 1,
        }
        at += size;
    }
    Some(page)
}

fn text_out(
    r: &[u8],
    wide: bool,
    font: &Font,
    align: u32,
    rgb: (u8, u8, u8),
    order: usize,
) -> Option<Text> {
    let x = i32_at(r, 36)? as f32;
    let y = i32_at(r, 40)? as f32;
    let n = u32_at(r, 44)? as usize;
    let off_string = u32_at(r, 48)? as usize;
    let off_dx = u32_at(r, 72)? as usize;
    if n == 0 || n > 1 << 16 {
        return None;
    }

    let width = if wide { 2 } else { 1 };
    let bytes = r.get(off_string..off_string.checked_add(n * width)?)?;
    let chars: Vec<char> = if wide {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units).chars().collect()
    } else {
        crate::infrastructure::cp932::decode(bytes)
    };

    let mut xs = Vec::with_capacity(chars.len());
    let mut cursor = x;
    let dx: Option<&[u8]> = r.get(off_dx..off_dx + n * 4);
    if wide {
        for i in 0..chars.len() {
            xs.push(cursor);
            let step = dx.and_then(|d| i32_at(d, i * 4)).unwrap_or(0) as f32;
            cursor += step;
        }
    } else {
        let mut i = 0usize;
        for ch in &chars {
            xs.push(cursor);
            let taken = ch.len_utf8().min(2);
            let taken = if *ch as u32 > 0x7F { 2 } else { taken.min(1) };
            let mut step = 0f32;
            for k in 0..taken {
                step += dx.and_then(|d| i32_at(d, (i + k) * 4)).unwrap_or(0) as f32;
            }
            i += taken;
            cursor += step;
        }
    }

    let size = if font.height != 0 {
        font.height.unsigned_abs() as f32
    } else if xs.len() > 1 {
        (xs[1] - xs[0]).abs().max(1.0)
    } else {
        1.0
    };

    let baseline = if align & TA_BASELINE == TA_BASELINE {
        y
    } else if align & TA_BOTTOM == TA_BOTTOM {
        y - size * 0.2
    } else {
        y + size * 0.8
    };

    Some(Text {
        xs,
        y: baseline,
        chars,
        size,
        escapement: font.escapement,
        rgb,
        order,
    })
}

fn is_shape_mask(r: &[u8]) -> bool {
    let Some(len) = u32_at(r, 8) else {
        return false;
    };
    let Some(body) = r.get(12..12 + (len as usize).min(r.len().saturating_sub(12))) else {
        return false;
    };
    u32_at(body, 0).is_some_and(|k| DW_SHAPE_MASK.contains(&k))
}

fn picture_placement(r: &[u8], order: usize) -> Option<Image> {
    const NEEDED: usize = 60;
    const SRCCOPY: u32 = 0x00CC_0020;
    let len = u32_at(r, 8)? as usize;
    let body = r.get(12..12 + len.min(r.len().saturating_sub(12)))?;
    if body.len() < NEEDED || body.get(..3)? != b"DWc" {
        return None;
    }
    let f = |i: usize| i32_at(body, i * 4);
    if f(12)? as u32 != SRCCOPY {
        return None;
    }
    let (w, h) = (f(9)?, f(10)?);
    if w <= 0 || h <= 0 {
        return None;
    }
    let (left, top, right, bottom) = (f(1)?, f(2)?, f(3)?, f(4)?);
    if right <= left || bottom <= top {
        return None;
    }
    Some(Image {
        left: left as f32,
        top: top as f32,
        right: right as f32,
        bottom: bottom as f32,
        src: (w as u32, h as u32),
        order,
    })
}

fn fill_rect(r: &[u8], brush: Option<(u8, u8, u8)>, order: usize, clipped: bool) -> Option<Fill> {
    if u32_at(r, 40)? != PATCOPY {
        return None;
    }
    let rgb = brush?;
    let (x, y, cx, cy) = (
        i32_at(r, 24)?,
        i32_at(r, 28)?,
        i32_at(r, 32)?,
        i32_at(r, 36)?,
    );
    if cx <= 0 || cy <= 0 {
        return None;
    }
    Some(Fill {
        left: x as f32,
        top: y as f32,
        right: (x + cx) as f32,
        bottom: (y + cy) as f32,
        rgb,
        order,
        clipped,
    })
}
