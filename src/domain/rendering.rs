//! Format-neutral drawing data produced by a page decoder.
//!
//! The EMF and WMF adapters translate a decoded metafile into these values.
//! PDF and HTML renderers consume this model instead of depending on the
//! metafile parsers, which keeps output concerns replaceable.
//!
//! All coordinates are device pixels of the page, y increasing downward, with
//! the window origin already applied.

use std::collections::HashMap;

/// The font family selected by the source metafile for a text run.
///
/// Keeping this at run level matters: the same ASCII hyphen can have a
/// different shape and width in a Japanese proportional face than in Verdana.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontKind {
    #[default]
    Japanese,
    JapaneseProportional,
    JapaneseProportionalGothic,
    Latin,
}

/// One run of characters, already positioned.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    /// Left edge of each character, in device units.
    pub xs: Vec<f32>,
    /// Baseline of the run, in device units, y increasing downward.
    pub y: f32,
    /// The characters, one per entry in `xs`.
    pub chars: Vec<char>,
    /// The source font family selected for this run.
    pub font_kind: FontKind,
    /// Character height in device units, always positive.
    pub size: f32,
    /// Tenths of a degree counter-clockwise; 0 for ordinary horizontal text.
    pub escapement: i32,
    /// Colour as red, green, blue.
    pub rgb: (u8, u8, u8),
    /// Position in the source's draw order.
    pub order: usize,
    /// The face asked for a bold weight.
    pub bold: bool,
    /// The face asked for an underline.
    pub underline: bool,
}

/// Where the pixels of a placed picture come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A picture stored beside the sheet in the container, named by the order
    /// in which the sheet calls for its pictures and by its stored size.
    Stored { ordinal: usize, px: (u32, u32) },
    /// A bitmap carried inside the metafile itself: `rasters[index]`.
    Inline(usize),
}

/// Raster operation used by a bitmap placement.
///
/// The operation is part of the drawing semantics, not a hint about page
/// orientation.  In particular, a colour picture bracketed by `SourceInvert`
/// placements and an `And` bitmap is one masked picture operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterOp {
    Copy,
    And,
    SourceInvert,
    MaskPaint,
    Other(u32),
}

impl RasterOp {
    /// Translate the Win32 ROP3 values used by the metafile formats.
    pub fn from_code(code: u32) -> Self {
        match code {
            0x00CC_0020 => RasterOp::Copy,
            0x0088_00C6 => RasterOp::And,
            0x0066_0046 => RasterOp::SourceInvert,
            0x00B8_074A => RasterOp::MaskPaint,
            other => RasterOp::Other(other),
        }
    }
}

/// One picture placed on the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Image {
    /// Destination rectangle in device units, y increasing downward.
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    /// Size of the stored picture this draws, in pixels.
    pub src: (u32, u32),
    /// Which pixels to draw.
    pub source: Source,
    /// The raster operation used for this placement.
    pub raster_op: RasterOp,
    /// Position in the source's draw order.
    pub order: usize,
    /// The clip rectangle in force when the picture was drawn, if narrower
    /// than the page.
    pub clip: Option<Rect>,
    /// A path the picture is clipped to: `paths[index]`.
    pub clip_path: Option<usize>,
}

impl Image {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// An axis-aligned rectangle in device units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// A bitmap carried inside the page itself, rows top-down and packed to the
/// byte with no padding, the way PDF and PNG both want them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// 1, 4 or 8 for palette indices, 24 for packed red, green, blue.
    pub bits: u8,
    /// Colours for index values; empty for 24-bit data.
    pub palette: Vec<(u8, u8, u8)>,
    pub rows: Vec<u8>,
    /// A one-bit stencil rather than a picture: zero bits are painted in this
    /// colour and one bits leave the page as it was.
    pub stencil: Option<(u8, u8, u8)>,
}

impl Raster {
    /// Bytes per row.
    pub fn stride(&self) -> usize {
        (self.width as usize * self.bits as usize).div_ceil(8)
    }

    /// Whether the palette is exactly black and white, which readers show
    /// faster as a one-bit grey image than through a palette.
    pub fn is_bilevel(&self) -> bool {
        self.bits == 1
            && self.palette.len() == 2
            && ((self.palette[0] == (0, 0, 0) && self.palette[1] == (255, 255, 255))
                || (self.palette[0] == (255, 255, 255) && self.palette[1] == (0, 0, 0)))
    }
}

/// A filled rectangle: a rule, a border, or a block of colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fill {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub rgb: (u8, u8, u8),
    /// Position in the source's draw order.
    pub order: usize,
    /// The rectangle the fill was clipped to, if narrower than the page.
    pub clip: Option<Rect>,
    /// A path the fill is clipped to: `paths[index]`. Gradients and textured
    /// lettering are drawn as many thin rectangles clipped to the outline.
    pub clip_path: Option<usize>,
}

/// One piece of a figure: a straight line or a cubic curve to a point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Line((f32, f32)),
    Curve((f32, f32), (f32, f32), (f32, f32)),
}

/// One connected run of a path, starting with a move.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Figure {
    pub start: (f32, f32),
    pub segments: Vec<Segment>,
    /// Whether the outline returns to `start`. Filling closes every figure
    /// regardless, as GDI does.
    pub closed: bool,
}

/// A path in device units, y increasing downward.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Path {
    pub figures: Vec<Figure>,
    /// Fill by the even-odd rule instead of the non-zero winding rule.
    pub even_odd: bool,
}

impl Path {
    pub fn is_empty(&self) -> bool {
        self.figures.is_empty()
    }

    /// Bounding box of every point on the path, as left, top, right, bottom.
    pub fn bounds(&self) -> Option<Rect> {
        let mut r: Option<Rect> = None;
        let mut add = |(x, y): (f32, f32)| {
            r = Some(match r {
                None => Rect {
                    left: x,
                    top: y,
                    right: x,
                    bottom: y,
                },
                Some(b) => Rect {
                    left: b.left.min(x),
                    top: b.top.min(y),
                    right: b.right.max(x),
                    bottom: b.bottom.max(y),
                },
            });
        };
        for f in &self.figures {
            add(f.start);
            for s in &f.segments {
                match s {
                    Segment::Line(p) => add(*p),
                    Segment::Curve(a, b, c) => {
                        add(*a);
                        add(*b);
                        add(*c);
                    }
                }
            }
        }
        r
    }
}

/// A path painted onto the page: filled with a brush and/or outlined with a
/// pen.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub path: Path,
    /// Fill colour, if the brush paints anything.
    pub fill: Option<(u8, u8, u8)>,
    /// Outline colour and width in device units, if the pen draws anything.
    pub stroke: Option<((u8, u8, u8), f32)>,
    /// Position in the source's draw order.
    pub order: usize,
    /// The rectangle the drawing was clipped to, if narrower than the page.
    pub clip: Option<Rect>,
}

/// A decoded page reduced to the drawing primitives supported by the
/// application output adapters.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Metafile {
    /// Page size in device units, from the source's device size.
    pub device: (i32, i32),
    /// Page size in hundredths of a millimetre, from the source frame.
    pub frame_mm100: (i32, i32),
    /// Text runs, in source draw order.
    pub text: Vec<Text>,
    /// Pictures, in source draw order.
    pub images: Vec<Image>,
    /// Bitmaps carried inside the page, referenced by `Source::Inline`.
    pub rasters: Vec<Raster>,
    /// Filled rectangles: rules, borders and blocks of colour.
    pub fills: Vec<Fill>,
    /// Polygons, polylines and curves.
    pub shapes: Vec<Shape>,
    /// Clip paths referenced by fills and pictures, each stored once.
    pub paths: Vec<Path>,
    /// How many drawing operations were clipped to a path.
    pub shape_masks: usize,
    /// Record types read but not drawn, with their counts.
    pub skipped: HashMap<u32, usize>,
    /// Records in the source, as its header declares.
    pub records: u32,
}

impl Metafile {
    /// Logical units per point, for placing the page on paper.
    pub fn units_per_point(&self) -> (f32, f32) {
        let mm100_to_pt = 72.0 / 2540.0;
        let (w, h) = (self.frame_mm100.0 as f32, self.frame_mm100.1 as f32);
        let (dw, dh) = (self.device.0 as f32, self.device.1 as f32);
        if w > 0.0 && h > 0.0 && dw > 0.0 && dh > 0.0 {
            (dw / (w * mm100_to_pt), dh / (h * mm100_to_pt))
        } else {
            (1.0, 1.0)
        }
    }

    /// Paper size in PDF points.
    pub fn points(&self) -> (f32, f32) {
        let k = 72.0 / 2540.0;
        (self.frame_mm100.0 as f32 * k, self.frame_mm100.1 as f32 * k)
    }

    /// 文字レイヤーが正立した縦書き配置かどうか。
    ///
    /// メタファイルによっては、字を1字ずつ連続する縦座標へ配置し、直角の
    /// エスケープメントだけで書字方向を示す。この指定を字ごとに適用すると
    /// 字形そのものが回転してしまう。繰り返すx座標と直角の角度を使い、
    /// 通常の回転ラベルと区別する。
    pub fn uses_upright_vertical_text(&self) -> bool {
        let runs: Vec<&Text> = self
            .text
            .iter()
            .filter(|run| run.chars.iter().any(|c| !c.is_whitespace()))
            .collect();
        if runs.len() < 3
            || runs
                .iter()
                .any(|run| run.chars.len() != 1 || run.xs.len() != 1)
        {
            return false;
        }
        let angle = runs[0].escapement.rem_euclid(3600);
        if !matches!(angle, 900 | 2700)
            || runs
                .iter()
                .any(|run| run.escapement.rem_euclid(3600) != angle)
        {
            return false;
        }

        let mut columns: Vec<f32> = Vec::new();
        let mut repeated_column = false;
        let mut varying_y = false;
        let mut reference_y: Option<f32> = None;
        for run in runs {
            let Some(&x) = run.xs.first() else {
                return false;
            };
            if columns.iter().any(|previous| (x - *previous).abs() <= 1.0) {
                repeated_column = true;
            } else {
                columns.push(x);
            }
            if let Some(previous_y) = reference_y {
                varying_y |= (run.y - previous_y).abs() > 1.0;
            } else {
                reference_y = Some(run.y);
            }
        }
        repeated_column && varying_y
    }

    /// Whether anything at all was recovered from this page.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.images.is_empty()
            && self.fills.is_empty()
            && self.shapes.is_empty()
    }

    /// The sizes of the stored pictures this page draws, each listed once.
    pub fn image_sizes(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for image in &self.images {
            if matches!(image.source, Source::Stored { .. }) && !out.contains(&image.src) {
                out.push(image.src);
            }
        }
        out
    }

    /// How many stored pictures the page calls for.
    pub fn stored_pictures(&self) -> usize {
        self.images
            .iter()
            .filter_map(|i| match i.source {
                Source::Stored { ordinal, .. } => Some(ordinal + 1),
                Source::Inline(_) => None,
            })
            .max()
            .unwrap_or(0)
    }
}

/// Pair the stored pictures a page calls for with the pictures kept beside
/// the sheet.
///
/// `calls` lists each ordinal the page named, with the pixel size it drew at,
/// in ordinal order; `stored` lists the sizes of the pictures beside the
/// sheet in storage order. The container keeps each distinct picture once,
/// so a tile placed thirty times is one stored picture named thirty times:
/// a call takes the first picture of its size not yet taken, and failing
/// that the last one of that size, so repeats resolve to the same bytes.
/// Returns, per call, the index into `stored`.
pub fn pair_pictures(
    calls: &[(usize, (u32, u32))],
    stored: &[Option<(u32, u32)>],
) -> Vec<Option<usize>> {
    // Some printer drivers emit two placement passes for every stored tile:
    // the first pass is the mask and the second paints the colour.  The
    // container de-duplicates the tile bytes, while the metafile retains both
    // calls.  In that unambiguous shape, preserving the repeated pair is more
    // informative than assigning every overflow call to the last tile.
    if !stored.is_empty()
        && calls.len() == stored.len() * 2
        && calls
            .as_chunks::<2>()
            .0
            .iter()
            .all(|pair| pair[0].1 == pair[1].1)
    {
        return (0..calls.len()).map(|i| Some(i / 2)).collect();
    }
    let mut taken = vec![false; stored.len()];
    let mut out = Vec::with_capacity(calls.len());
    for &(_, px) in calls {
        let known = px.0 > 0 && px.1 > 0;
        let fresh = stored
            .iter()
            .enumerate()
            .position(|(i, s)| !taken[i] && (!known || *s == Some(px)));
        let pick = fresh
            .or_else(|| {
                known
                    .then(|| stored.iter().rposition(|s| *s == Some(px)))
                    .flatten()
            })
            .or_else(|| taken.iter().position(|t| !t));
        if let Some(k) = pick {
            taken[k] = true;
        }
        out.push(pick);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(x: f32, y: f32, ch: char, escapement: i32) -> Text {
        Text {
            xs: vec![x],
            y,
            chars: vec![ch],
            font_kind: FontKind::Japanese,
            size: 10.0,
            escapement,
            rgb: (0, 0, 0),
            order: 0,
            bold: false,
            underline: false,
        }
    }

    #[test]
    fn detects_glyphs_already_positioned_in_vertical_columns() {
        let m = Metafile {
            text: vec![
                text(100.0, 10.0, '縦', 2700),
                text(100.0, 20.0, '書', 2700),
                text(80.0, 10.0, 'き', 2700),
            ],
            ..Metafile::default()
        };
        assert!(m.uses_upright_vertical_text());
    }

    #[test]
    fn does_not_treat_a_single_rotated_run_as_vertical_writing() {
        let m = Metafile {
            text: vec![
                text(100.0, 10.0, 'A', 2700),
                text(110.0, 10.0, 'B', 2700),
                text(120.0, 10.0, 'C', 2700),
            ],
            ..Metafile::default()
        };
        assert!(!m.uses_upright_vertical_text());
    }

    #[test]
    fn classifies_the_raster_operations_used_by_picture_masks() {
        assert_eq!(RasterOp::from_code(0x00CC_0020), RasterOp::Copy);
        assert_eq!(RasterOp::from_code(0x0088_00C6), RasterOp::And);
        assert_eq!(RasterOp::from_code(0x0066_0046), RasterOp::SourceInvert);
        assert_eq!(RasterOp::from_code(0x00B8_074A), RasterOp::MaskPaint);
        assert_eq!(
            RasterOp::from_code(0x1234_5678),
            RasterOp::Other(0x1234_5678)
        );
    }

    #[test]
    fn repeated_calls_for_one_size_share_the_one_stored_picture() {
        let calls = [
            (0, (128, 128)),
            (1, (116, 128)),
            (2, (128, 128)),
            (3, (400, 604)),
        ];
        let stored = [Some((128, 128)), Some((116, 128)), Some((400, 604))];
        assert_eq!(
            pair_pictures(&calls, &stored),
            vec![Some(0), Some(1), Some(0), Some(2)]
        );
    }

    #[test]
    fn distinct_pictures_of_one_size_are_handed_out_in_order() {
        let calls = [(0, (10, 10)), (1, (10, 10)), (2, (10, 10))];
        let stored = [Some((10, 10)), Some((10, 10)), Some((10, 10))];
        assert_eq!(
            pair_pictures(&calls, &stored),
            vec![Some(0), Some(1), Some(2)]
        );
    }

    #[test]
    fn a_size_nothing_matches_falls_back_to_the_next_unused_picture() {
        let calls = [(0, (5, 5))];
        let stored = [Some((10, 10))];
        assert_eq!(pair_pictures(&calls, &stored), vec![Some(0)]);
        assert_eq!(pair_pictures(&calls, &[]), vec![None]);
    }

    #[test]
    fn paired_mask_and_colour_passes_share_each_stored_picture() {
        let calls = [(0, (20, 30)), (1, (20, 30)), (2, (40, 50)), (3, (40, 50))];
        let stored = [Some((20, 30)), Some((40, 50))];
        assert_eq!(
            pair_pictures(&calls, &stored),
            vec![Some(0), Some(0), Some(1), Some(1)]
        );
    }
}
