//! Format-neutral drawing data produced by a page decoder.
//!
//! The EMF adapter translates a decoded metafile into these values.  PDF and
//! HTML renderers consume this model instead of depending on the EMF parser,
//! which keeps output concerns replaceable.

use std::collections::HashMap;

/// One run of characters, already positioned.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    /// Left edge of each character, in logical units.
    pub xs: Vec<f32>,
    /// Baseline of the run, in logical units, y increasing downward.
    pub y: f32,
    /// The characters, one per entry in `xs`.
    pub chars: Vec<char>,
    /// Character height in logical units, always positive.
    pub size: f32,
    /// Tenths of a degree counter-clockwise; 0 for ordinary horizontal text.
    pub escapement: i32,
    /// Colour as red, green, blue.
    pub rgb: (u8, u8, u8),
    /// Position in the source's draw order.
    pub order: usize,
}

/// One picture placed on the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Image {
    /// Destination rectangle in logical units, y increasing downward.
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    /// Size of the stored picture this draws, in pixels.
    pub src: (u32, u32),
    /// Position in the source's draw order.
    pub order: usize,
}

impl Image {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
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
    /// The source carries a shape mask for this fill, so the rectangle is only
    /// a bounding box and must not be rendered as a solid rectangle.
    pub clipped: bool,
}

/// A decoded page reduced to the drawing primitives supported by the
/// application output adapters.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Metafile {
    /// Page size in logical units, from the source's device size.
    pub device: (i32, i32),
    /// Page size in hundredths of a millimetre, from the source frame.
    pub frame_mm100: (i32, i32),
    /// Text runs, in source draw order.
    pub text: Vec<Text>,
    /// Pictures, in source draw order.
    pub images: Vec<Image>,
    /// Filled rectangles: rules, borders and blocks of colour.
    pub fills: Vec<Fill>,
    /// Shape masks declared in private comments.
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

    /// Whether anything at all was recovered from this page.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.images.is_empty() && self.fills.is_empty()
    }

    /// The sizes of the stored pictures this page draws, each listed once.
    pub fn image_sizes(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for image in &self.images {
            if !out.contains(&image.src) {
                out.push(image.src);
            }
        }
        out
    }
}
