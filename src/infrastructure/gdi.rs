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
    Figure, Fill, Image, Metafile, Path, Raster, Rect, Segment, Shape, Source, Text,
};
use crate::infrastructure::dib;

/// Copy the source over the destination.
pub const SRCCOPY: u32 = 0x00CC_0020;
/// Paint the brush where the source is black, leave the rest: the raster
/// operation GDI uses to draw a monochrome bitmap as a coloured stencil.
pub const MASK_PAINT: u32 = 0x00B8_074A;

/// GDI stock objects are selected by an index with the top bit set.
const STOCK: u32 = 0x8000_0000;
const NULL_BRUSH: u32 = 5;
const WHITE_PEN: u32 = 6;
const NULL_PEN: u32 = 8;

const TA_BASELINE: u32 = 24;
const TA_BOTTOM: u32 = 8;

/// A font as the metafile created it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Font {
    pub height: i32,
    pub escapement: i32,
    pub weight: i32,
    pub underline: bool,
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
    pub const OP_BEZIER: u8 = 0x13;
    pub const OP_FILLED_POLYGON: u8 = 0x20;
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
        let mut xs = Vec::with_capacity(chars.len());
        let mut cursor = dx;
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
        } else if rop != SRCCOPY {
            return false;
        }
        crop(&mut raster, src);
        let index = self.page.rasters.len();
        self.page.rasters.push(raster);
        self.place(Source::Inline(index), dst);
        true
    }

    /// Place a picture at a logical rectangle given as x, y, width, height.
    fn place(&mut self, source: Source, (x, y, w, h): (i32, i32, i32, i32)) {
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
        self.place_current((x, y, w, h), (sw, sh))
    }

    /// The 16-bit flavour of `DWc`: logical rectangle, source rectangle and
    /// raster operation, all as 16-bit values. Without a body, the picture
    /// fills the clip rectangle set just before.
    fn place_picture_16(&mut self, body: &[u8]) -> bool {
        if body.len() <= 4 {
            let Some((l, t, r, b)) = self.clip_logical else {
                return false;
            };
            return self.place_current((l, t, r - l, b - t), (0, 0));
        }
        let f = |i: usize| i16_at(body, 4 + i * 2).map(i32::from);
        let (Some(x), Some(y), Some(w), Some(h), Some(sw), Some(sh)) =
            (f(0), f(1), f(2), f(3), f(6), f(7))
        else {
            return false;
        };
        self.place_current((x, y, w, h), (sw, sh))
    }

    /// Draw the current picture at `dst`; `(sw, sh)` is its stored size when
    /// the placement says, or zero when only the order identifies it.
    fn place_current(&mut self, dst: (i32, i32, i32, i32), (sw, sh): (i32, i32)) -> bool {
        if sw < 0 || sh < 0 || dst.2 <= 0 || dst.3 <= 0 {
            return false;
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
        self.place(source, dst);
        true
    }

    /// Geometry comments: a point list in one of three codings, either added
    /// to the path under construction or, for the filled polygon, painted
    /// straight away.
    fn geometry(&mut self, body: &[u8]) -> bool {
        let (op, code) = (body[2], body[3]);
        if !matches!(code, dw::CODE_I16 | dw::CODE_I8 | dw::CODE_NIBBLE) {
            return false;
        }
        let Some(points) = decode_points(body) else {
            return false;
        };
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
            dw::OP_POLYLINE_TO => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                match path.figures.last_mut() {
                    Some(f) => f.segments.extend(pts.iter().map(|p| Segment::Line(*p))),
                    None => {
                        if let Some((first, rest)) = pts.split_first() {
                            path.figures.push(Figure {
                                start: *first,
                                segments: rest.iter().map(|p| Segment::Line(*p)).collect(),
                                closed: false,
                            });
                        }
                    }
                }
                true
            }
            dw::OP_POLYLINE | dw::OP_LINE | dw::OP_POLYGON => {
                let Some(path) = self.path.as_mut() else {
                    return false;
                };
                if let Some((first, rest)) = pts.split_first() {
                    path.figures.push(Figure {
                        start: *first,
                        segments: rest.iter().map(|p| Segment::Line(*p)).collect(),
                        closed: op == dw::OP_POLYGON,
                    });
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
                            .chunks_exact(3)
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
/// The count is followed by the first point as two 16-bit values. What
/// follows depends on the coding byte: further absolute 16-bit points, 8-bit
/// deltas, or one byte per point holding two signed 4-bit deltas where the
/// value -8 means the delta is in the next byte instead (and -128 there means
/// the next two bytes).
pub fn decode_points(body: &[u8]) -> Option<Vec<(i32, i32)>> {
    let code = *body.get(3)?;
    let n = u32_at(body, 4)? as usize;
    if n == 0 || n > 1 << 20 {
        return None;
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
            let escaped = |at: &mut usize, nibble: i32| -> Option<i32> {
                if nibble != -8 {
                    return Some(nibble);
                }
                let b = *body.get(*at)? as i8;
                *at += 1;
                if b != i8::MIN {
                    return Some(i32::from(b));
                }
                let v = i16_at(body, *at)?;
                *at += 2;
                Some(i32::from(v))
            };
            for _ in 1..n {
                let b = *body.get(at)?;
                at += 1;
                let hi = i32::from(((b >> 4) as i8) << 4 >> 4);
                let lo = i32::from(((b & 0x0F) as i8) << 4 >> 4);
                x += escaped(&mut at, hi)?;
                y += escaped(&mut at, lo)?;
                pts.push((x, y));
            }
        }
        _ => return None,
    }
    Some(pts)
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
    }
}
