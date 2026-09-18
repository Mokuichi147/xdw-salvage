//! The drawing state shared by the EMF and WMF readers.
//!
//! Both metafile flavours drive the same GDI model: a window origin that maps
//! logical to device coordinates, a table of fonts, pens and brushes selected
//! into the device context, a clip rectangle, and a path under construction.
//! On top of that the maker adds private comments of its own, which are the
//! same in both flavours: picture placement, clip rectangles, and path
//! geometry in a compact point coding. The canvas here interprets all of it
//! and produces the domain's [`Metafile`].

use std::collections::HashMap;

use crate::domain::rendering::{
    Figure, Fill, FontKind, Image, Metafile, Path, Raster, RasterOp, Rect, Segment, Shape, Source,
    Text,
};
use crate::infrastructure::dib;

/// Copy the source over the destination.
pub const SRCCOPY: u32 = 0x00CC_0020;
/// Paint the brush where the source is black, leave the rest: the raster
/// operation GDI uses to draw a monochrome bitmap as a coloured stencil.
pub const MASK_PAINT: u32 = 0x00B8_074A;
/// Keep the destination where the source bitmap is white and clear it where
/// the source is black.  DocuWorks uses this for monochrome masks embedded in
/// an EMF rather than for ordinary opaque pictures.
pub const SRCAND: u32 = 0x0088_00C6;

/// GDI stock objects are selected by an index with the top bit set.
const STOCK: u32 = 0x8000_0000;
const NULL_BRUSH: u32 = 5;
const WHITE_PEN: u32 = 6;
const NULL_PEN: u32 = 8;

const TA_BASELINE: u32 = 24;
const TA_BOTTOM: u32 = 8;
const TA_RIGHT: u32 = 2;
const TA_CENTER: u32 = 6;

/// A font as the metafile created it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Font {
    pub height: i32,
    pub escapement: i32,
    pub weight: i32,
    pub underline: bool,
    /// The font family selected by the source for this font object.
    pub kind: FontKind,
}

/// What a selectable object is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Object {
    Font(Font),
    /// A brush; `None` paints nothing.
    Brush(Option<(u8, u8, u8)>),
    /// A pen with colour and width; `None` draws nothing.
    Pen(Option<((u8, u8, u8), f32)>),
    /// Something the maker created that this crate does not interpret. It
    /// still takes a slot, so later objects keep their numbers.
    Opaque,
}

/// The private comment vocabulary, shared by both metafile flavours.
mod dw {
    pub const INLINE_PICTURE: &[u8] = b"DWa\0";
    pub const NEXT_PICTURE: &[u8] = b"DWb\0";
    pub const PLACE_PICTURE: &[u8] = b"DWc\0";
    pub const CLIP_RECT: &[u8] = b"DW06";
    pub const END_CLIP_PATH: &[u8] = b"DW01";
    pub const BEGIN_PATH: &[u8] = b"DW02";
    pub const CLIP_PATH: &[u8] = b"DW03";
    pub const FILL_PATH: &[u8] = b"DW04";
    pub const STROKE_PATH: &[u8] = b"DW05";
    /// The 16-bit flavour's picture markers.
    pub const NEXT_PICTURE_16: &[u8] = b"DW\x02\x01";
    pub const PLACE_PICTURE_16: &[u8] = b"DW\x02\x02";
    // Geometry: the third byte says what the points are, the fourth how they
    // are coded.
    pub const OP_POLYLINE: u8 = 0x00;
    pub const OP_LINE: u8 = 0x01;
    pub const OP_POLYLINE_TO: u8 = 0x02;
    pub const OP_POLYGON: u8 = 0x03;
    /// Extended path segments emitted by the DocuWorks printer driver.  The
    /// first point starts a figure and the following points are cubic
    /// control/control/end triples.
    pub const OP_EXTENDED_CONTINUE: u8 = 0x10;
    pub const OP_EXTENDED_START: u8 = 0x11;
    pub const OP_EXTENDED_APPEND: u8 = 0x12;
    pub const OP_BEZIER: u8 = 0x13;
    /// A direct two-point segment used by the printer driver's annotation
    /// layer, outside a begin/end path pair.
    pub const OP_LINE_DIRECT: u8 = 0x40;
    pub const OP_FILLED_POLYGON: u8 = 0x20;
    /// Absolute 32-bit point coordinates used by EMF private Bezier paths.
    pub const CODE_I32: u8 = 0x00;
    pub const CODE_I16: u8 = 0x20;
    pub const CODE_I8: u8 = 0x40;
    pub const CODE_NIBBLE: u8 = 0x80;
}

/// Accumulates drawing state and output while a metafile is walked.
#[derive(Debug)]
pub struct Canvas {
    pub page: Metafile,
    order: usize,
    // Logical to device mapping: device = (logical - window_org) * scale + viewport_org.
    window_org: (i32, i32),
    viewport_org: (i32, i32),
    window_ext: Option<(i32, i32)>,
    viewport_ext: Option<(i32, i32)>,
    objects: HashMap<u32, Object>,
    font: Font,
    brush: Option<(u8, u8, u8)>,
    pen: Option<((u8, u8, u8), f32)>,
    align: u32,
    text_rgb: (u8, u8, u8),
    even_odd: bool,
    /// Clip rectangle in device units, if narrower than the page.
    clip: Option<Rect>,
    /// The last clip rectangle asked for, in logical units, whether or not
    /// it narrowed anything. The 16-bit flavour places pictures into it.
    clip_logical: Option<(i32, i32, i32, i32)>,
    /// Index into `page.paths` of the clip path in force.
    clip_path: Option<usize>,
    /// A path being built between the maker's begin and end comments.
    path: Option<Path>,
    /// Which stored picture the next placement draws.
    picture: Option<Source>,
    /// How many stored pictures have been called for.
    pictures_called: usize,
    /// Current GDI position used by MoveToEx/LineTo.
    moved: Option<(i32, i32)>,
    /// Device-context snapshots made by SaveDC.  DocuWorks uses nested saves
    /// around annotation clips, so a later RestoreDC must also restore the
    /// clip and mapping before the annotation outline is painted.
    saved_states: Vec<SavedState>,
}

#[derive(Clone, Copy, Debug)]
struct SavedState {
    window_org: (i32, i32),
    viewport_org: (i32, i32),
    window_ext: Option<(i32, i32)>,
    viewport_ext: Option<(i32, i32)>,
    font: Font,
    brush: Option<(u8, u8, u8)>,
    pen: Option<((u8, u8, u8), f32)>,
    align: u32,
    text_rgb: (u8, u8, u8),
    even_odd: bool,
    clip: Option<Rect>,
    clip_logical: Option<(i32, i32, i32, i32)>,
    clip_path: Option<usize>,
    moved: Option<(i32, i32)>,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            page: Metafile::default(),
            order: 0,
            window_org: (0, 0),
            viewport_org: (0, 0),
            window_ext: None,
            viewport_ext: None,
            objects: HashMap::new(),
            font: Font::default(),
            // GDI starts with a white brush and a black one-pixel pen.
            brush: Some((255, 255, 255)),
            pen: Some(((0, 0, 0), 1.0)),
            align: 0,
            text_rgb: (0, 0, 0),
            even_odd: true,
            clip: None,
            clip_logical: None,
            clip_path: None,
            path: None,
            picture: None,
            pictures_called: 0,
            moved: Some((0, 0)),
            saved_states: Vec::new(),
        }
    }
}

pub fn rgb(c: u32) -> (u8, u8, u8) {
    (c as u8, (c >> 8) as u8, (c >> 16) as u8)
}

impl Canvas {
    pub fn new(device: (i32, i32), frame_mm100: (i32, i32)) -> Self {
        Canvas {
            page: Metafile {
                device,
                frame_mm100,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Count a record and return its place in the draw order.
    pub fn advance(&mut self) -> usize {
        self.order += 1;
        self.order
    }

    pub fn skip(&mut self, kind: u32) {
        *self.page.skipped.entry(kind).or_insert(0) += 1;
    }

    /// Save the state that affects subsequent GDI drawing.
    pub fn save_state(&mut self) {
        self.saved_states.push(SavedState {
            window_org: self.window_org,
            viewport_org: self.viewport_org,
            window_ext: self.window_ext,
            viewport_ext: self.viewport_ext,
            font: self.font,
            brush: self.brush,
            pen: self.pen,
            align: self.align,
            text_rgb: self.text_rgb,
            even_odd: self.even_odd,
            clip: self.clip,
            clip_logical: self.clip_logical,
            clip_path: self.clip_path,
            moved: self.moved,
        });
    }

    /// Restore a saved state.  Negative levels are relative to the newest
    /// snapshot (`-1` is the common EMF form); positive levels are the
    /// one-based SaveDC return values used by the Win32 API.
    pub fn restore_state(&mut self, level: i32) {
        let Some(index) = (if level < 0 {
            self.saved_states.len() as i32 + level
        } else {
            level - 1
        })
        .try_into()
        .ok()
        .filter(|&index: &usize| index < self.saved_states.len()) else {
            return;
        };
        let state = self.saved_states[index];
        self.window_org = state.window_org;
        self.viewport_org = state.viewport_org;
        self.window_ext = state.window_ext;
        self.viewport_ext = state.viewport_ext;
        self.font = state.font;
        self.brush = state.brush;
        self.pen = state.pen;
        self.align = state.align;
        self.text_rgb = state.text_rgb;
        self.even_odd = state.even_odd;
        self.clip = state.clip;
        self.clip_logical = state.clip_logical;
        self.clip_path = state.clip_path;
        self.moved = state.moved;
        // RestoreDC discards the restored level and every newer level.
        self.saved_states.truncate(index);
    }

    pub fn finish(self) -> Metafile {
        self.page
    }

    // ----- coordinate mapping -----

    pub fn set_window_org(&mut self, x: i32, y: i32) {
        self.window_org = (x, y);
    }

    pub fn set_viewport_org(&mut self, x: i32, y: i32) {
        self.viewport_org = (x, y);
    }

    pub fn set_window_ext(&mut self, x: i32, y: i32) {
        if x != 0 && y != 0 {
            self.window_ext = Some((x, y));
        }
    }

    pub fn set_viewport_ext(&mut self, x: i32, y: i32) {
        if x != 0 && y != 0 {
            self.viewport_ext = Some((x, y));
        }
    }

    /// Logical units map one to one onto device pixels unless both extents
    /// were given, as in the text mapping mode every page here uses.
    fn scale(&self) -> (f32, f32) {
        match (self.window_ext, self.viewport_ext) {
            (Some(w), Some(v)) => (v.0 as f32 / w.0 as f32, v.1 as f32 / w.1 as f32),
            _ => (1.0, 1.0),
        }
    }

    /// A logical point in device units.
    pub fn device(&self, x: i32, y: i32) -> (f32, f32) {
        let (sx, sy) = self.scale();
        (
            (x - self.window_org.0) as f32 * sx + self.viewport_org.0 as f32,
            (y - self.window_org.1) as f32 * sy + self.viewport_org.1 as f32,
        )
    }

    /// A logical distance in device units.
    pub fn device_len(&self, v: f32) -> f32 {
        v * self.scale().1.abs()
    }

    /// Set the current GDI position.
    pub fn move_to(&mut self, x: i32, y: i32) {
        self.moved = Some((x, y));
    }

    /// Draw a line from the current GDI position and advance it.
    pub fn line_to(&mut self, x: i32, y: i32) {
        if let Some((from_x, from_y)) = self.moved {
            self.polygon(&[(from_x, from_y), (x, y)], false);
        }
        self.moved = Some((x, y));
    }

    // ----- objects -----

    pub fn create(&mut self, handle: u32, object: Object) {
        self.objects.insert(handle, object);
    }

    pub fn delete(&mut self, handle: u32) {
        self.objects.remove(&handle);
    }

    pub fn select(&mut self, handle: u32) {
        if handle & STOCK != 0 {
            match handle & !STOCK {
                0 => self.brush = Some((255, 255, 255)),
                1 => self.brush = Some((192, 192, 192)),
                2 => self.brush = Some((128, 128, 128)),
                3 => self.brush = Some((64, 64, 64)),
                4 => self.brush = Some((0, 0, 0)),
                NULL_BRUSH => self.brush = None,
                WHITE_PEN => self.pen = Some(((255, 255, 255), 1.0)),
                7 => self.pen = Some(((0, 0, 0), 1.0)),
                NULL_PEN => self.pen = None,
                _ => {}
            }
            return;
        }
        match self.objects.get(&handle) {
            Some(Object::Font(f)) => self.font = *f,
            Some(Object::Brush(b)) => self.brush = *b,
            Some(Object::Pen(p)) => self.pen = *p,
            Some(Object::Opaque) | None => {}
        }
    }

    /// The selected font's height in logical units.
    pub fn font_height(&self) -> f32 {
        self.font.height.unsigned_abs() as f32
    }

    pub fn set_text_align(&mut self, align: u32) {
        self.align = align;
    }

    pub fn set_text_colour(&mut self, colour: u32) {
        self.text_rgb = rgb(colour);
    }

    pub fn set_poly_fill_mode(&mut self, mode: u32) {
        // 1 is ALTERNATE (even-odd), 2 is WINDING.
        self.even_odd = mode != 2;
    }

    /// Restrict drawing to a logical rectangle.
    pub fn set_clip_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        self.clip_logical = Some((left, top, right, bottom));
        let (l, t) = self.device(left, top);
        let (r, b) = self.device(right, bottom);
        let rect = Rect {
            left: l.min(r),
            top: t.min(b),
            right: l.max(r),
            bottom: t.max(b),
        };
        let (pw, ph) = (self.page.device.0 as f32, self.page.device.1 as f32);
        let whole_page =
            rect.left <= 0.0 && rect.top <= 0.0 && rect.right >= pw && rect.bottom >= ph;
        self.clip = if whole_page || pw <= 0.0 {
            None
        } else {
            Some(rect)
        };
        // A new rectangle replaces any path the maker had clipped to.
        self.clip_path = None;
    }

    // ----- drawing -----

    /// Text at a logical reference point, with per-character advances in
    /// logical units.
    pub fn text(&mut self, x: i32, y: i32, chars: Vec<char>, advances: &[f32]) {
        if chars.is_empty() {
            return;
        }
        let order = self.advance();
        let (dx, dy) = self.device(x, y);
        let (sx, sy) = self.scale();
        let total_advance: f32 = (0..chars.len())
            .map(|i| advances.get(i).copied().unwrap_or(0.0) * sx)
            .sum();
        let start_x = match self.align & TA_CENTER {
            TA_RIGHT => dx - total_advance,
            TA_CENTER => dx - total_advance * 0.5,
            _ => dx,
        };
        let mut xs = Vec::with_capacity(chars.len());
        let mut cursor = start_x;
        for i in 0..chars.len() {
            xs.push(cursor);
            cursor += advances.get(i).copied().unwrap_or(0.0) * sx;
        }
        let size = if self.font.height != 0 {
            self.font.height.unsigned_abs() as f32 * sy.abs()
        } else if xs.len() > 1 {
            (xs[1] - xs[0]).abs().max(1.0)
        } else {
            1.0
        };
        let baseline = if self.align & TA_BASELINE == TA_BASELINE {
            dy
        } else if self.align & TA_BOTTOM == TA_BOTTOM {
            dy - size * 0.2
        } else {
            dy + size * 0.8
        };
        self.page.text.push(Text {
            xs,
            y: baseline,
            chars,
            font_kind: self.font.kind,
            size,
            escapement: self.font.escapement,
            rgb: self.text_rgb,
            order,
            bold: self.font.weight >= 600,
            underline: self.font.underline,
        });
    }

    /// A rectangle painted with the current brush, from logical corner and size.
    pub fn pat_fill(&mut self, x: i32, y: i32, cx: i32, cy: i32) {
        let order = self.advance();
        let Some(rgb) = self.brush else {
            return;
        };
        let (l, t) = self.device(x, y);
        let (r, b) = self.device(x.saturating_add(cx), y.saturating_add(cy));
        if r <= l || b <= t {
            return;
        }
        if self.clip_path.is_some() {
            self.page.shape_masks += 1;
        }
        self.page.fills.push(Fill {
            left: l,
            top: t,
            right: r,
            bottom: b,
            rgb,
            order,
            clip: self.clip,
            clip_path: self.clip_path,
        });
    }

    /// A bitmap carried in the record, drawn at a logical rectangle.
    ///
    /// `src` is the part of the bitmap to draw, as x, y, width, height;
    /// `rop` is the raster operation, which decides whether the bitmap is a
    /// picture or a stencil for the brush.
    pub fn stretch_dib(
        &mut self,
        info: &[u8],
        bits: &[u8],
        dst: (i32, i32, i32, i32),
        src: (i32, i32, i32, i32),
        rop: u32,
    ) -> bool {
        let Some(mut raster) = dib::decode(info, bits) else {
            return false;
        };
        if rop == MASK_PAINT {
            if raster.bits != 1 {
                return false;
            }
            let Some(colour) = self.brush else {
                // Nothing to paint with: the stencil leaves the page alone.
                self.advance();
                return true;
            };
            raster.stencil = Some(colour);
        } else if rop == SRCAND {
            if raster.bits == 1 {
                // モノクロマスクでは0ビットが塗られ、1ビットが元の画素を残す。
                // 出力アダプターが扱う極性へ正規化する。
                normalize_and_mask(&mut raster);
            }
            // 一部のプリンタードライバーは1ビットマスクではなくパレットDIBに
            // SRCANDを使う。中立モデルでは宛先画素とのANDを再現できないが、対象
            // レコードは白紙上の描画なので、画像を捨てず通常のラスタとして残す。
        } else if rop != SRCCOPY {
            return false;
        }
        crop(&mut raster, src);
        let raster_op = RasterOp::from_code(rop);
        let index = self.page.rasters.len();
        self.page.rasters.push(raster);
        self.place(Source::Inline(index), dst, raster_op);
        true
    }

    /// Place a picture at a logical rectangle given as x, y, width, height.
    fn place(&mut self, source: Source, (x, y, w, h): (i32, i32, i32, i32), raster_op: RasterOp) {
        let order = self.advance();
        let (l, t) = self.device(x, y);
        let (r, b) = self.device(x.saturating_add(w), y.saturating_add(h));
        if r <= l || b <= t {
            return;
        }
        let px = match source {
            Source::Stored { px, .. } => px,
            Source::Inline(i) => self
                .page
                .rasters
                .get(i)
                .map(|r| (r.width, r.height))
                .unwrap_or((0, 0)),
        };
        if self.clip_path.is_some() {
            self.page.shape_masks += 1;
        }
        self.page.images.push(Image {
            left: l,
            top: t,
            right: r,
            bottom: b,
            src: px,
            source,
            raster_op,
            order,
            clip: self.clip,
            clip_path: self.clip_path,
        });
    }

    /// A polygon or polyline in logical units, painted with the current pen
    /// and brush.
    pub fn polygon(&mut self, points: &[(i32, i32)], closed: bool) {
        let order = self.advance();
        if points.len() < 2 {
            return;
        }
        let pts: Vec<(f32, f32)> = points.iter().map(|&(x, y)| self.device(x, y)).collect();
        let figure = Figure {
            start: pts[0],
            segments: pts[1..].iter().map(|p| Segment::Line(*p)).collect(),
            closed,
        };
        let path = Path {
            figures: vec![figure],
            even_odd: self.even_odd,
        };
        self.paint(path, closed, true, order);
    }

    /// Paint several polygons from one GDI record as one path.  Multi-polygon
    /// records are used for outlined glyphs; keeping their contours together
    /// preserves the selected even-odd fill rule and the holes in each glyph.
    pub fn polygons(&mut self, polygons: &[Vec<(i32, i32)>], closed: bool) {
        let order = self.advance();
        let figures: Vec<Figure> = polygons
            .iter()
            .filter(|points| points.len() >= 2)
            .map(|points| {
                let pts: Vec<(f32, f32)> = points.iter().map(|&(x, y)| self.device(x, y)).collect();
                Figure {
                    start: pts[0],
                    segments: pts[1..].iter().map(|p| Segment::Line(*p)).collect(),
                    closed,
                }
            })
            .collect();
        if figures.is_empty() {
            return;
        }
        self.paint(
            Path {
                figures,
                even_odd: self.even_odd,
            },
            closed,
            true,
            order,
        );
    }

    /// Paint an axis-aligned ellipse using four cubic Bézier segments.
    ///
    /// GDI exposes ellipses as a primitive while the neutral model only needs
    /// paths.  The control-point approximation is exact enough for PDF/HTML
    /// output at ordinary page resolution and preserves both the selected
    /// brush and pen.
    pub fn ellipse(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        let order = self.advance();
        let (l, t) = self.device(left.min(right), top.min(bottom));
        let (r, b) = self.device(left.max(right), top.max(bottom));
        let left = l.min(r);
        let right = l.max(r);
        let top = t.min(b);
        let bottom = t.max(b);
        if right <= left || bottom <= top {
            return;
        }
        let cx = (left + right) * 0.5;
        let cy = (top + bottom) * 0.5;
        let rx = (right - left) * 0.5;
        let ry = (bottom - top) * 0.5;
        let k = 0.552_284_8;
        let path = Path {
            figures: vec![Figure {
                start: (right, cy),
                segments: vec![
                    Segment::Curve((right, cy + k * ry), (cx + k * rx, bottom), (cx, bottom)),
                    Segment::Curve((cx - k * rx, bottom), (left, cy + k * ry), (left, cy)),
                    Segment::Curve((left, cy - k * ry), (cx - k * rx, top), (cx, top)),
                    Segment::Curve((cx + k * rx, top), (right, cy - k * ry), (right, cy)),
                ],
                closed: true,
            }],
            even_odd: self.even_odd,
        };
        self.paint(path, true, true, order);
    }

    /// GDIのRoundRectに相当する、角を楕円で丸めた矩形を描画する。
    /// GDIが渡す幅・高さは角の楕円全体の寸法なので、半分を半径にする。
    pub fn round_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        ellipse_width: i32,
        ellipse_height: i32,
    ) {
        let order = self.advance();
        let (a, b) = self.device(left, top);
        let (c, d) = self.device(right, bottom);
        let (left, right) = (a.min(c), a.max(c));
        let (top, bottom) = (b.min(d), b.max(d));
        if right <= left || bottom <= top {
            return;
        }
        let (sx, sy) = self.scale();
        let rx = (ellipse_width.unsigned_abs() as f32 * sx.abs() * 0.5).min((right - left) * 0.5);
        let ry = (ellipse_height.unsigned_abs() as f32 * sy.abs() * 0.5).min((bottom - top) * 0.5);
        let k = 0.552_284_8;
        let path = Path {
            figures: vec![Figure {
                start: (left + rx, top),
                segments: vec![
                    Segment::Line((right - rx, top)),
                    Segment::Curve(
                        (right - rx + k * rx, top),
                        (right, top + ry - k * ry),
                        (right, top + ry),
                    ),
                    Segment::Line((right, bottom - ry)),
                    Segment::Curve(
                        (right, bottom - ry + k * ry),
                        (right - rx + k * rx, bottom),
                        (right - rx, bottom),
                    ),
                    Segment::Line((left + rx, bottom)),
                    Segment::Curve(
                        (left + rx - k * rx, bottom),
                        (left, bottom - ry + k * ry),
                        (left, bottom - ry),
                    ),
                    Segment::Line((left, top + ry)),
                    Segment::Curve(
                        (left, top + ry - k * ry),
                        (left + rx - k * rx, top),
                        (left + rx, top),
                    ),
                ],
                closed: true,
            }],
            even_odd: self.even_odd,
        };
        self.paint(path, true, true, order);
    }

    fn paint(&mut self, path: Path, fill: bool, stroke: bool, order: usize) {
        let fill = if fill { self.brush } else { None };
        let stroke = if stroke { self.pen } else { None };
        if fill.is_none() && stroke.is_none() {
            return;
        }
        self.page.shapes.push(Shape {
            path,
            fill,
            stroke,
            order,
            clip: self.clip,
        });
    }

    // ----- the maker's private comments -----

    /// Interpret one private comment. Returns whether it was understood.
    pub fn comment(&mut self, body: &[u8]) -> bool {
        let Some(tag) = body.get(..4) else {
            return false;
        };
        if &tag[..2] != b"DW" {
            return false;
        }
        match tag {
            dw::INLINE_PICTURE => self.inline_picture(body),
            dw::NEXT_PICTURE | dw::NEXT_PICTURE_16 => {
                self.next_picture();
                true
            }
            dw::PLACE_PICTURE => self.place_picture(body),
            dw::PLACE_PICTURE_16 => self.place_picture_16(body),
            dw::CLIP_RECT => {
                let f = |i: usize| i32_at(body, 4 + i * 4);
                if let (Some(l), Some(t), Some(r), Some(b)) = (f(0), f(1), f(2), f(3)) {
                    self.set_clip_rect(l, t, r, b);
                }
                true
            }
            dw::BEGIN_PATH => {
                self.path = Some(Path {
                    figures: Vec::new(),
                    even_odd: self.even_odd,
                });
                true
            }
            dw::CLIP_PATH => {
                if let Some(path) = self.path.take() {
                    self.clip_path = Some(self.intern(path));
                }
                true
            }
            dw::END_CLIP_PATH => true,
            dw::FILL_PATH | dw::STROKE_PATH => {
                let order = self.advance();
                if let Some(mut path) = self.path.take() {
                    path.even_odd = self.even_odd;
                    let fill = tag == dw::FILL_PATH;
                    self.paint(path, fill, !fill, order);
                }
                true
            }
            _ => self.geometry(body),
        }
    }

    fn next_picture(&mut self) {
        self.picture = Some(Source::Stored {
            ordinal: self.pictures_called,
            px: (0, 0),
        });
        self.pictures_called += 1;
    }

    /// `DWa`: a bitmap defined in the comment for later placements.
    fn inline_picture(&mut self, body: &[u8]) -> bool {
        // Offsets count from the start of the comment record: eight bytes of
        // record header plus four of comment length precede `body`.
        const RECORD_PREFIX: usize = 12;
        let f = |i: usize| u32_at(body, 4 + i * 4).map(|v| v as usize);
        let (Some(off_bmi), Some(cb_bmi), Some(off_bits), Some(cb_bits)) = (f(0), f(1), f(2), f(3))
        else {
            return false;
        };
        let at = |off: usize, len: usize| {
            let start = off.checked_sub(RECORD_PREFIX)?;
            body.get(start..start.checked_add(len)?)
        };
        let (Some(info), Some(bits)) = (at(off_bmi, cb_bmi), at(off_bits, cb_bits)) else {
            return false;
        };
        let Some(raster) = dib::decode(info, bits) else {
            return false;
        };
        let index = self.page.rasters.len();
        self.page.rasters.push(raster);
        self.picture = Some(Source::Inline(index));
        true
    }

    /// `DWc`: draw the current picture. The body carries the device
    /// rectangle, the logical origin, the source rectangle, the raster
    /// operation and the logical size.
    fn place_picture(&mut self, body: &[u8]) -> bool {
        let f = |i: usize| i32_at(body, i * 4);
        let (Some(x), Some(y), Some(sw), Some(sh), Some(w), Some(h)) =
            (f(5), f(6), f(9), f(10), f(13), f(14))
        else {
            return false;
        };
        let raster_op = u32_at(body, 48)
            .map(RasterOp::from_code)
            .unwrap_or(RasterOp::Copy);
        self.place_current((x, y, w, h), (sw, sh), raster_op)
    }

    /// The 16-bit flavour of `DWc`: logical rectangle, source rectangle and
    /// raster operation, all as 16-bit values. Without a body, the picture
    /// fills the clip rectangle set just before.
    fn place_picture_16(&mut self, body: &[u8]) -> bool {
        if body.len() <= 4 {
            let Some((l, t, r, b)) = self.clip_logical else {
                return false;
            };
            return self.place_current((l, t, r - l, b - t), (0, 0), RasterOp::Copy);
        }
        let f = |i: usize| i16_at(body, 4 + i * 2).map(i32::from);
        let (Some(x), Some(y), Some(w), Some(h), Some(sw), Some(sh)) =
            (f(0), f(1), f(2), f(3), f(6), f(7))
        else {
            return false;
        };
        self.place_current((x, y, w, h), (sw, sh), RasterOp::Copy)
    }

    /// Draw the current picture at `dst`; `(sw, sh)` is its stored size when
    /// the placement says, or zero when only the order identifies it.
    fn place_current(
        &mut self,
        dst: (i32, i32, i32, i32),
        (sw, sh): (i32, i32),
        raster_op: RasterOp,
    ) -> bool {
        if sw < 0 || sh < 0 || dst.2 <= 0 || dst.3 <= 0 {
            return false;
        }
        if raster_op == RasterOp::And {
            if let Some(Source::Inline(index)) = self.picture {
                let Some(raster) = self.page.rasters.get_mut(index) else {
                    return false;
                };
                if raster.bits != 1 {
                    return false;
                }
                normalize_and_mask(raster);
            }
        }
        let source = match self.picture {
            Some(Source::Stored { ordinal, .. }) => Source::Stored {
                ordinal,
                px: (sw as u32, sh as u32),
            },
            Some(inline @ Source::Inline(_)) => inline,
            // A placement with no picture named first still counts for the
            // next stored one, as the first placement of a page does.
            None => {
                self.next_picture();
                Source::Stored {
                    ordinal: self.pictures_called - 1,
                    px: (sw as u32, sh as u32),
                }
            }
        };
        self.picture = Some(source);
        self.place(source, dst, raster_op);
        true
    }

    /// Geometry comments: a point list in one of three codings, either added
    /// to the current path figure or, for the filled polygon, painted straight
    /// away.  The private `POLYLINE` and `POLYLINE_TO` records continue the
    /// current figure; `LINE` starts a new one.
    fn geometry(&mut self, body: &[u8]) -> bool {
        let (op, code) = (body[2], body[3]);
        if !matches!(
            code,
            dw::CODE_I32 | dw::CODE_I16 | dw::CODE_I8 | dw::CODE_NIBBLE
        ) {
            return false;
        }
        let Some(points) = decode_points(body) else {
            return false;
        };
        if op == dw::OP_LINE_DIRECT {
            self.polygon(&points, false);
            return true;
        }
        let pts: Vec<(f32, f32)> = points.iter().map(|&(x, y)| self.device(x, y)).collect();
        match op {
            dw::OP_FILLED_POLYGON => {
                let order = self.advance();
                if pts.len() < 2 {
                    return true;
                }
                let path = Path {
                    figures: vec![Figure {
                        start: pts[0],
                        segments: pts[1..].iter().map(|p| Segment::Line(*p)).collect(),
                        closed: true,
                    }],
                    even_odd: self.even_odd,
                };
                self.paint(path, true, true, order);
                true
            }
            dw::OP_POLYLINE | dw::OP_POLYLINE_TO => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                if let Some(figure) = path.figures.last_mut() {
                    figure
                        .segments
                        .extend(pts.iter().map(|p| Segment::Line(*p)));
                } else if let Some((first, rest)) = pts.split_first() {
                    path.figures.push(Figure {
                        start: *first,
                        segments: rest.iter().map(|p| Segment::Line(*p)).collect(),
                        closed: false,
                    });
                }
                true
            }
            dw::OP_LINE => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                if let Some((first, rest)) = pts.split_first() {
                    path.figures.push(Figure {
                        start: *first,
                        segments: rest.iter().map(|p| Segment::Line(*p)).collect(),
                        closed: false,
                    });
                }
                true
            }
            dw::OP_POLYGON => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                if let Some((first, rest)) = pts.split_first() {
                    path.figures.push(Figure {
                        start: *first,
                        segments: rest.iter().map(|p| Segment::Line(*p)).collect(),
                        closed: true,
                    });
                }
                true
            }
            dw::OP_EXTENDED_START | dw::OP_EXTENDED_CONTINUE | dw::OP_EXTENDED_APPEND => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                let Some((first, rest)) = pts.split_first() else {
                    return true;
                };
                let curves = |points: &[(f32, f32)]| {
                    points
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|c| Segment::Curve(c[0], c[1], c[2]))
                        .collect::<Vec<_>>()
                };
                if op == dw::OP_EXTENDED_START || path.figures.is_empty() {
                    path.figures.push(Figure {
                        start: *first,
                        segments: curves(rest),
                        closed: false,
                    });
                } else if let Some(figure) = path.figures.last_mut() {
                    // A continuation consists only of control/control/end
                    // triples; its first point is not a repeated move-to.
                    figure.segments.extend(curves(&pts));
                }
                true
            }
            dw::OP_BEZIER => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                if let Some((first, rest)) = pts.split_first() {
                    path.figures.push(Figure {
                        start: *first,
                        segments: rest
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .map(|c| Segment::Curve(c[0], c[1], c[2]))
                            .collect(),
                        closed: true,
                    });
                }
                true
            }
            _ => false,
        }
    }

    /// Store a clip path, reusing the last one when it is the same. Gradient
    /// fills clip every sliver to the same outline, so this keeps the page
    /// from carrying hundreds of copies.
    fn intern(&mut self, path: Path) -> usize {
        if let Some(last) = self.page.paths.last() {
            if *last == path {
                return self.page.paths.len() - 1;
            }
        }
        self.page.paths.push(path);
        self.page.paths.len() - 1
    }
}

/// Normalize a monochrome SRCAND bitmap to the common foreground-bit form.
fn normalize_and_mask(raster: &mut Raster) {
    if raster.stencil.is_some() {
        return;
    }
    if raster.palette.get(1) == Some(&(0, 0, 0)) {
        for byte in &mut raster.rows {
            *byte = !*byte;
        }
    }
    raster.stencil = Some((0, 0, 0));
}

/// Keep only the `src` part (x, y, width, height) of a raster.
///
/// Only whole rows are cut; a horizontal crop would mean re-packing every
/// row of sub-byte pixels, and no page has asked for one.
fn crop(raster: &mut Raster, (sx, sy, sw, sh): (i32, i32, i32, i32)) {
    if sx != 0 || sw <= 0 || sh <= 0 || sw as u32 != raster.width {
        return;
    }
    let (sy, sh) = (sy.max(0) as usize, sh as usize);
    let stride = raster.stride();
    let end = (sy + sh).min(raster.height as usize);
    if sy >= end || (sy == 0 && end == raster.height as usize) {
        return;
    }
    raster.rows = raster.rows[sy * stride..end * stride].to_vec();
    raster.height = (end - sy) as u32;
}

/// Decode a geometry comment's point list.
///
/// The count is followed by absolute 32-bit points for the private EMF Bezier
/// coding, or by the first point as two 16-bit values for the compact codings.
/// The latter continue with absolute 16-bit points, 8-bit deltas, or one byte
/// per point holding two signed 4-bit deltas where the value -8 means the
/// delta is in the next byte instead (and -128 there means the next two
/// bytes).
pub fn decode_points(body: &[u8]) -> Option<Vec<(i32, i32)>> {
    let code = *body.get(3)?;
    let n = u32_at(body, 4)? as usize;
    if n == 0 || n > 1 << 20 {
        return None;
    }
    if code == dw::CODE_I32 {
        let mut x = i32_at(body, 8)?;
        let mut y = i32_at(body, 12)?;
        let mut pts = Vec::with_capacity(n);
        pts.push((x, y));
        let mut at = 16usize;
        for _ in 1..n {
            x = i32_at(body, at)?;
            y = i32_at(body, at + 4)?;
            at += 8;
            pts.push((x, y));
        }
        return Some(pts);
    }
    let mut x = i32::from(i16_at(body, 8)?);
    let mut y = i32::from(i16_at(body, 10)?);
    let mut pts = Vec::with_capacity(n);
    pts.push((x, y));
    let mut at = 12usize;
    match code {
        dw::CODE_I16 => {
            for _ in 1..n {
                x = i32::from(i16_at(body, at)?);
                y = i32::from(i16_at(body, at + 2)?);
                at += 4;
                pts.push((x, y));
            }
        }
        dw::CODE_I8 => {
            for _ in 1..n {
                x += i32::from(*body.get(at)? as i8);
                y += i32::from(*body.get(at + 1)? as i8);
                at += 2;
                pts.push((x, y));
            }
        }
        dw::CODE_NIBBLE => {
            let payload = body.get(12..)?;
            let (decoded, consumed) = decode_nibbles(payload, n, (x, y), false)?;
            if consumed == payload.len() {
                return Some(decoded);
            }
            let (decoded, consumed) = decode_nibbles(payload, n, (x, y), true)?;
            if consumed != payload.len() {
                return None;
            }
            return Some(decoded);
        }
        _ => return None,
    }
    Some(pts)
}

/// Decode the compact point payload.  A `-8` nibble introduces a signed
/// byte; some writers additionally use `-128` as a marker for a following
/// 16-bit value.  The byte form is tried first because it is unambiguous when
/// it consumes the complete record, while the wider form remains available
/// for records whose declared length requires it.
fn decode_nibbles(
    payload: &[u8],
    n: usize,
    first: (i32, i32),
    extend_i16: bool,
) -> Option<(Vec<(i32, i32)>, usize)> {
    let mut x = first.0;
    let mut y = first.1;
    let mut pts = Vec::with_capacity(n);
    pts.push((x, y));
    let mut at = 0usize;
    let escaped = |at: &mut usize, nibble: i32| -> Option<i32> {
        if nibble != -8 {
            return Some(nibble);
        }
        let b = *payload.get(*at)? as i8;
        *at += 1;
        if extend_i16 && b == i8::MIN {
            let v = i16_at(payload, *at)?;
            *at += 2;
            Some(i32::from(v))
        } else {
            Some(i32::from(b))
        }
    };
    for _ in 1..n {
        let b = *payload.get(at)?;
        at += 1;
        let hi = i32::from(((b >> 4) as i8) << 4 >> 4);
        let lo = i32::from(((b & 0x0F) as i8) << 4 >> 4);
        x += escaped(&mut at, hi)?;
        y += escaped(&mut at, lo)?;
        pts.push((x, y));
    }
    Some((pts, at))
}

pub fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn i32_at(d: &[u8], at: usize) -> Option<i32> {
    u32_at(d, at).map(|v| v as i32)
}

pub fn i16_at(d: &[u8], at: usize) -> Option<i16> {
    d.get(at..at + 2).map(|b| i16::from_le_bytes([b[0], b[1]]))
}

pub fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(op: u8, code: u8, n: u32, first: (i16, i16), rest: &[u8]) -> Vec<u8> {
        let mut b = b"DW".to_vec();
        b.push(op);
        b.push(code);
        b.extend_from_slice(&n.to_le_bytes());
        b.extend_from_slice(&first.0.to_le_bytes());
        b.extend_from_slice(&first.1.to_le_bytes());
        b.extend_from_slice(rest);
        b
    }

    #[test]
    fn sixteen_bit_points_are_absolute() {
        let b = geometry(0x20, 0x20, 2, (10, 20), &[30, 0, 40, 0]);
        assert_eq!(decode_points(&b), Some(vec![(10, 20), (30, 40)]));
    }

    #[test]
    fn thirty_two_bit_points_are_absolute() {
        let mut b = b"DW\x13\0".to_vec();
        b.extend_from_slice(&4u32.to_le_bytes());
        for (x, y) in [(10i32, 20i32), (30, 40), (50, 60), (70, 80)] {
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
        }
        assert_eq!(
            decode_points(&b),
            Some(vec![(10, 20), (30, 40), (50, 60), (70, 80)])
        );
    }

    #[test]
    fn eight_bit_points_are_deltas() {
        let b = geometry(0x20, 0x40, 3, (10, 20), &[2, 0xFF, 0x80, 1]);
        assert_eq!(
            decode_points(&b),
            Some(vec![(10, 20), (12, 19), (-116, 20)])
        );
    }

    #[test]
    fn nibble_points_escape_to_wider_deltas() {
        // (-3, 1), then dx escapes to -42 with dy 2, then both escape, dy to
        // a 16-bit value.
        let rest = [0xD1, 0x82, 0xD6, 0x88, 0x05, 0x80, 0xE8, 0x03];
        let b = geometry(0x02, 0x80, 4, (100, 100), &rest);
        assert_eq!(
            decode_points(&b),
            Some(vec![(100, 100), (97, 101), (55, 103), (60, 1103)])
        );
    }

    #[test]
    fn nibble_points_keep_a_signed_128_escape_in_the_byte_form() {
        let b = geometry(0x02, 0x80, 3, (0, 0), &[0x08, 0x80, 0x11]);
        assert_eq!(decode_points(&b), Some(vec![(0, 0), (0, -128), (1, -127)]));
    }

    #[test]
    fn a_short_list_is_refused_rather_than_read_past_the_end() {
        let b = geometry(0x20, 0x20, 3, (10, 20), &[30, 0, 40, 0]);
        assert_eq!(decode_points(&b), None);
    }

    #[test]
    fn the_window_origin_shifts_logical_points_onto_the_device() {
        let mut c = Canvas::new((4961, 7016), (21000, 29700));
        c.set_window_org(-109, -109);
        assert_eq!(c.device(0, 0), (109.0, 109.0));
        assert_eq!(c.device(600, 4876), (709.0, 4985.0));
    }

    #[test]
    fn centered_text_uses_the_reference_point_as_its_center() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.set_text_align(TA_CENTER);
        c.text(100, 200, vec!['a', 'b'], &[20.0, 20.0]);
        assert_eq!(c.page.text[0].xs, vec![80.0, 100.0]);
    }

    #[test]
    fn multi_polygon_keeps_contours_in_one_shape() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.polygons(
            &[
                vec![(10, 10), (40, 10), (40, 40)],
                vec![(60, 60), (90, 60), (90, 90)],
            ],
            true,
        );
        assert_eq!(c.page.shapes.len(), 1);
        assert_eq!(c.page.shapes[0].path.figures.len(), 2);
        assert!(c.page.shapes[0].path.figures.iter().all(|f| f.closed));
    }

    #[test]
    fn a_clip_rectangle_covering_the_page_is_no_clip_at_all() {
        let mut c = Canvas::new((4961, 7016), (21000, 29700));
        c.set_window_org(-109, -109);
        c.set_clip_rect(-109, -109, 4961, 7016);
        assert!(c.clip.is_none());
        c.set_clip_rect(100, 100, 200, 300);
        assert_eq!(
            c.clip,
            Some(Rect {
                left: 209.0,
                top: 209.0,
                right: 309.0,
                bottom: 409.0
            })
        );
    }

    #[test]
    fn restore_state_reinstates_the_clip_and_mapping() {
        let mut c = Canvas::new((1000, 1000), (1000, 1000));
        c.set_window_org(10, 20);
        c.save_state();
        c.set_window_org(100, 200);
        c.set_clip_rect(120, 220, 180, 280);
        assert!(c.clip.is_some());

        c.restore_state(-1);

        assert_eq!(c.window_org, (10, 20));
        assert_eq!(c.clip, None);
    }

    #[test]
    fn restoring_a_positive_level_discards_newer_snapshots() {
        let mut c = Canvas::new((100, 100), (100, 100));
        c.set_window_org(1, 2);
        c.save_state();
        c.set_window_org(3, 4);
        c.save_state();
        c.set_window_org(5, 6);

        c.restore_state(1);

        assert_eq!(c.window_org, (1, 2));
        c.set_window_org(7, 8);
        c.save_state();
        c.set_window_org(9, 10);
        c.restore_state(-1);
        assert_eq!(c.window_org, (7, 8));
    }

    #[test]
    fn the_next_picture_marker_hands_out_ordinals_in_order() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        let mut place = b"DWc\0".to_vec();
        let fields: [i32; 14] = [0, 0, 0, 0, 10, 10, 0, 0, 8, 4, 0, 0x00CC_0020, 20, 10];
        for v in fields {
            place.extend_from_slice(&v.to_le_bytes());
        }
        assert!(c.comment(b"DWb\0"));
        assert!(c.comment(&place));
        assert!(c.comment(&place));
        assert!(c.comment(b"DWb\0"));
        assert!(c.comment(&place));
        let ordinals: Vec<Source> = c.page.images.iter().map(|i| i.source).collect();
        assert_eq!(
            ordinals,
            vec![
                Source::Stored {
                    ordinal: 0,
                    px: (8, 4)
                },
                Source::Stored {
                    ordinal: 0,
                    px: (8, 4)
                },
                Source::Stored {
                    ordinal: 1,
                    px: (8, 4)
                }
            ]
        );
        let i = c.page.images[0];
        assert_eq!((i.left, i.top, i.right, i.bottom), (10.0, 10.0, 30.0, 20.0));
        assert_eq!(i.raster_op, RasterOp::Copy);
    }

    #[test]
    fn a_clip_path_is_stored_once_and_shared_by_the_fills_inside_it() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.create(1, Object::Brush(Some((1, 2, 3))));
        c.select(1);
        let square = geometry(
            0x03,
            0x20,
            4,
            (0, 0),
            &[50, 0, 0, 0, 50, 0, 50, 0, 0, 0, 50, 0],
        );
        for _ in 0..3 {
            assert!(c.comment(b"DW02"));
            assert!(c.comment(&square));
            assert!(c.comment(b"DW03"));
            c.pat_fill(0, 10, 100, 5);
        }
        assert_eq!(c.page.paths.len(), 1);
        assert_eq!(c.page.fills.len(), 3);
        assert!(c.page.fills.iter().all(|f| f.clip_path == Some(0)));
        assert_eq!(c.page.shape_masks, 3);
        // A clip rectangle lifts the path clip.
        c.comment(&{
            let mut b = b"DW06".to_vec();
            for v in [0i32, 0, 100, 100] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b
        });
        c.pat_fill(0, 0, 10, 10);
        assert_eq!(c.page.fills[3].clip_path, None);
    }

    #[test]
    fn a_polyline_continues_the_current_private_path_figure() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.select(STOCK | NULL_BRUSH);
        let first = geometry(dw::OP_LINE, dw::CODE_I16, 2, (10, 20), &[30, 0, 40, 0]);
        let continuation = geometry(dw::OP_POLYLINE, dw::CODE_I16, 2, (50, 60), &[70, 0, 80, 0]);

        assert!(c.comment(dw::BEGIN_PATH));
        assert!(c.comment(&first));
        assert!(c.comment(&continuation));
        assert!(c.comment(dw::STROKE_PATH));

        assert_eq!(c.page.shapes.len(), 1);
        assert_eq!(c.page.shapes[0].path.figures.len(), 1);
        assert_eq!(
            c.page.shapes[0].path.figures[0].segments,
            vec![
                Segment::Line((30.0, 40.0)),
                Segment::Line((50.0, 60.0)),
                Segment::Line((70.0, 80.0)),
            ]
        );
    }

    #[test]
    fn a_stroked_path_takes_the_pen_and_a_filled_one_the_brush() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.create(1, Object::Pen(Some(((9, 9, 9), 6.0))));
        c.select(1);
        c.select(STOCK | NULL_BRUSH);
        let square = geometry(
            0x03,
            0x20,
            4,
            (0, 0),
            &[50, 0, 0, 0, 50, 0, 50, 0, 0, 0, 50, 0],
        );
        c.comment(b"DW02");
        c.comment(&square);
        c.comment(b"DW05");
        c.create(2, Object::Brush(Some((1, 2, 3))));
        c.select(2);
        c.comment(b"DW02");
        c.comment(&square);
        c.comment(b"DW04");
        assert_eq!(c.page.shapes.len(), 2);
        assert_eq!(c.page.shapes[0].stroke, Some(((9, 9, 9), 6.0)));
        assert_eq!(c.page.shapes[0].fill, None);
        assert_eq!(c.page.shapes[1].fill, Some((1, 2, 3)));
        assert_eq!(c.page.shapes[1].stroke, None);
    }

    #[test]
    fn a_polygon_comment_paints_at_once_with_pen_and_brush() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.create(1, Object::Brush(Some((1, 2, 3))));
        c.select(1);
        c.select(STOCK | NULL_PEN);
        let quad = geometry(
            0x20,
            0x20,
            4,
            (0, 0),
            &[50, 0, 0, 0, 50, 0, 50, 0, 0, 0, 50, 0],
        );
        assert!(c.comment(&quad));
        assert_eq!(c.page.shapes.len(), 1);
        assert_eq!(c.page.shapes[0].fill, Some((1, 2, 3)));
        assert_eq!(c.page.shapes[0].stroke, None);
        assert_eq!(c.page.shapes[0].path.figures[0].segments.len(), 3);
    }

    #[test]
    fn a_direct_line_comment_is_painted_without_a_path() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.select(STOCK | NULL_BRUSH);
        let line = geometry(0x40, dw::CODE_I16, 2, (10, 20), &[30, 0, 40, 0]);
        assert!(c.comment(&line));
        assert_eq!(c.page.shapes.len(), 1);
        assert_eq!(c.page.shapes[0].path.figures[0].start, (10.0, 20.0));
        assert_eq!(
            c.page.shapes[0].path.figures[0].segments,
            vec![Segment::Line((30.0, 40.0))]
        );
    }

    #[test]
    fn extended_path_comments_keep_cubic_segments_together() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.select(STOCK | NULL_BRUSH);
        let start = geometry(
            0x11,
            dw::CODE_I16,
            4,
            (0, 0),
            &[10, 0, 0, 0, 20, 0, 10, 0, 30, 0, 20, 0],
        );
        let continuation = geometry(
            0x10,
            dw::CODE_I16,
            3,
            (40, 30),
            &[50, 0, 40, 0, 60, 0, 50, 0],
        );
        assert!(c.comment(b"DW02"));
        assert!(c.comment(&start));
        assert!(c.comment(&continuation));
        assert!(c.comment(b"DW05"));
        assert_eq!(c.page.shapes.len(), 1);
        assert_eq!(c.page.shapes[0].path.figures[0].segments.len(), 2);
        assert!(matches!(
            c.page.shapes[0].path.figures[0].segments[0],
            Segment::Curve((10.0, 0.0), (20.0, 10.0), (30.0, 20.0))
        ));
        assert!(matches!(
            c.page.shapes[0].path.figures[0].segments[1],
            Segment::Curve((40.0, 30.0), (50.0, 40.0), (60.0, 50.0))
        ));
    }

    #[test]
    fn an_ellipse_becomes_a_closed_four_curve_path() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        c.ellipse(10, 20, 50, 80);
        assert_eq!(c.page.shapes.len(), 1);
        let figure = &c.page.shapes[0].path.figures[0];
        assert!(figure.closed);
        assert_eq!(figure.segments.len(), 4);
        assert!(figure
            .segments
            .iter()
            .all(|s| matches!(s, Segment::Curve(..))));
    }

    #[test]
    fn srcand_turns_a_black_palette_entry_into_a_black_stencil() {
        let mut info = Vec::new();
        info.extend_from_slice(&40u32.to_le_bytes());
        info.extend_from_slice(&1i32.to_le_bytes());
        info.extend_from_slice(&1i32.to_le_bytes());
        info.extend_from_slice(&1u16.to_le_bytes());
        info.extend_from_slice(&1u16.to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        info.extend_from_slice(&[0u8; 12]);
        info.extend_from_slice(&2u32.to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        // DIB palette entries are B, G, R, reserved: white then black.
        info.extend_from_slice(&[255, 255, 255, 0, 0, 0, 0, 0]);

        let mut c = Canvas::new((1, 1), (100, 100));
        assert!(c.stretch_dib(&info, &[0x80, 0, 0, 0], (0, 0, 1, 1), (0, 0, 1, 1), SRCAND));
        assert_eq!(c.page.rasters[0].stencil, Some((0, 0, 0)));
        assert_eq!(c.page.rasters[0].rows, vec![0x7f]);
        assert_eq!(c.page.images[0].raster_op, RasterOp::And);
    }

    #[test]
    fn an_inline_picture_is_decoded_and_placed_by_the_next_placement() {
        let mut c = Canvas::new((100, 100), (1000, 1000));
        // A 1x1 one-bit bitmap: 40-byte header, two palette entries, 4 bytes.
        let mut body = b"DWa\0".to_vec();
        let off_bmi = 12 + 4 + 16;
        body.extend_from_slice(&(off_bmi as u32).to_le_bytes());
        body.extend_from_slice(&48u32.to_le_bytes());
        body.extend_from_slice(&((off_bmi + 48) as u32).to_le_bytes());
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&40u32.to_le_bytes());
        body.extend_from_slice(&1i32.to_le_bytes());
        body.extend_from_slice(&1i32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&[0u8; 24]);
        body.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
        body.extend_from_slice(&[0x80, 0, 0, 0]);
        assert!(c.comment(&body));
        let mut place = b"DWc\0".to_vec();
        let fields: [i32; 14] = [0, 0, 0, 0, 5, 5, 0, 0, 1, 1, 0, 0x00CC_0020, 10, 10];
        for v in fields {
            place.extend_from_slice(&v.to_le_bytes());
        }
        assert!(c.comment(&place));
        assert_eq!(c.page.rasters.len(), 1);
        assert_eq!(c.page.images[0].source, Source::Inline(0));
        assert_eq!(c.page.images[0].src, (1, 1));
        assert_eq!(c.page.images[0].raster_op, RasterOp::Copy);
    }
}
