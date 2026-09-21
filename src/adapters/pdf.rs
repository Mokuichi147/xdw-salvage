//! A small PDF writer, just enough to wrap recovered page images.
//!
//! JPEG streams go in untouched as `DCTDecode` images, so a page that comes out
//! of this is the same bytes that went in. Nothing is re-encoded.

use std::collections::BTreeMap;

use crate::application::ports::{AttachmentScanner, PageDecoder};
use crate::application::recovery;
pub use crate::domain::output::Language as Lang;
use crate::domain::page::{Overlay, Page, PageData};
use crate::domain::rendering::{
    self, Fill, FontKind, Image, Metafile, Raster, RasterOp, Rect, Segment, Shape, Source, Text,
};
use crate::domain::{DisplayPage, Document};
use crate::infrastructure::{deflate, jpeg, ttf, LzhMetafileDecoder, MagicAttachmentScanner};

type FontIds = (
    usize,
    Option<usize>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
);

/// How to treat pages whose image cannot be recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Leave them out. Page numbering in the PDF will not match the original.
    Skip,
    /// Emit a blank page of the right size carrying a short note, so the page
    /// numbering still lines up with the original document.
    Placeholder,
}

/// Settings for [`build`].
#[derive(Debug, Clone)]
pub struct Options {
    pub missing: Missing,
    /// Expand pages held in the container's own coding and redraw them from the
    /// metafile inside. On by default: it is the difference between a page of
    /// searchable text and an apology.
    pub decode: bool,
    /// A TrueType font supplied by the caller for the recovered page text.
    ///
    /// Without one the PDF names the `MS-Mincho` Japanese system face and
    /// relies on the reader having a matching face, which keeps the output
    /// small. With one, only the glyphs used in the document are embedded, so
    /// the caller's font license must permit embedding and redistribution in
    /// the resulting PDF.
    pub font: Option<std::sync::Arc<ttf::Font>>,
    /// Include preview entries as pages. Off by default: they are low
    /// resolution copies of other pages, not content.
    ///
    /// Preview images are in the vendor coding, so turning this on adds
    /// placeholders rather than pictures. It exists for inspecting a document's
    /// entry list, not for producing a readable PDF.
    pub include_previews: bool,
    /// Put every page on one paper size, in points, instead of the size each
    /// page declares.
    ///
    /// A document can mix page sizes wildly: a screen capture at its own pixel
    /// density lands on a 249 x 202 mm page while the text pages beside it are
    /// A4. Faithful, but awkward to read or print. With a size set here, images
    /// are centred and scaled down to fit; they are never scaled up, so a small
    /// image stays small rather than turning into a blur.
    pub paper: Option<(f32, f32)>,
    /// Carry any original file found inside the document as a PDF attachment,
    /// so the source travels with the conversion instead of beside it.
    pub embed_originals: bool,
    /// Language for the placeholder notes.
    pub lang: Lang,
    /// Document title. The source file name is a sensible choice.
    pub title: Option<String>,
    /// Add bookmarks pointing at every page that could not be recovered, so an
    /// auditor can jump straight to the gaps instead of scrolling a long file.
    pub bookmark_gaps: bool,
    /// Attach the whole source document to the PDF.
    ///
    /// For an archive where most pages are in the vendor coding, a PDF of
    /// placeholders is not a migration on its own. With the source carried
    /// inside it, the PDF is at least a strict superset of the file it came
    /// from: readable where anything could be read, and reversible everywhere
    /// else. It roughly doubles the output, so it is off by default.
    pub carry_source: bool,
}

/// A4 in points on the 600 dpi page grid used by Windows print output.
pub const A4: (f32, f32) = (595.32, 841.92);
/// US Letter in points.
pub const LETTER: (f32, f32) = (612.0, 792.0);

/// Standard paper sizes which an image-derived XDW frame may miss by a few
/// hundredths of a millimetre.  The orientation is part of each entry.
const STANDARD_PAPERS_MM100: &[(u32, u32)] = &[
    (10_500, 14_800),
    (14_800, 10_500),
    (14_800, 21_000),
    (21_000, 14_800),
    (18_200, 25_700),
    (25_700, 18_200),
    (21_000, 29_700),
    (29_700, 21_000),
    (21_590, 27_940),
    (27_940, 21_590),
    (25_700, 36_400),
    (36_400, 25_700),
    (29_700, 42_000),
    (42_000, 29_700),
];
const STANDARD_PAPER_TOLERANCE_MM100: u32 = 100;

/// Convert an XDW paper size to the PDF page grid used by the reference
/// printouts.  Windows print drivers describe a page in 600-dpi device units,
/// so using the mathematically exact millimetre conversion leaves a visible
/// metadata difference such as 595.28 versus 595.32 points. Scanned JPEG
/// bounds can be as much as about a millimetre short of a standard sheet
/// because the stored frame follows the 300-dpi image extent rather than the
/// printable paper edge. Recognise only that narrow case and keep arbitrary
/// paper sizes unchanged.
fn pdf_paper_points(paper: Option<(u32, u32)>) -> Option<(f32, f32)> {
    paper.map(|(w, h)| {
        let (w, h) = STANDARD_PAPERS_MM100
            .iter()
            .copied()
            .find(|(sw, sh)| {
                w.abs_diff(*sw) <= STANDARD_PAPER_TOLERANCE_MM100
                    && h.abs_diff(*sh) <= STANDARD_PAPER_TOLERANCE_MM100
            })
            .unwrap_or((w, h));
        let to_points = |mm100: u32| {
            let device = (mm100 as f64 * 600.0 / 2540.0).round();
            (device * 72.0 / 600.0) as f32
        };
        (to_points(w), to_points(h))
    })
}

impl Default for Options {
    fn default() -> Self {
        Options {
            decode: true,
            font: None,
            missing: Missing::Placeholder,
            include_previews: false,
            paper: None,
            embed_originals: true,
            lang: Lang::English,
            title: None,
            bookmark_gaps: true,
            carry_source: false,
        }
    }
}

/// What ended up in the PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Report {
    pub embedded: usize,
    pub placeholders: usize,
    /// Pages redrawn from the metafile the container's own coding holds.
    pub drawn: usize,
    /// Characters placed from those metafiles.
    pub glyphs: usize,
    pub skipped: usize,
    /// Original files attached to the PDF.
    pub attachments: usize,
    /// Bookmarks pointing at unrecoverable pages.
    pub bookmarks: usize,
    /// Pictures recovered off sheets that could not themselves be reproduced.
    pub pictures_placed: usize,
}

/// The margin these layouts share.
fn margin_of(pw: f32, ph: f32) -> f32 {
    (pw.min(ph) * 0.06).clamp(2.0, 28.0)
}

/// Whether a sheet is small enough to plausibly be a text label attached to a
/// preceding image page.  The decision is based only on the page's declared
/// geometry and drawing contents; it does not depend on a document name or on
/// the text carried by the page.
fn is_text_only_label(page: &Page) -> bool {
    let Some((w, h)) = page.paper else {
        return false;
    };
    w > 0 && h > 0 && w.max(h) <= 3000 && w.min(h) >= 300 && w.saturating_mul(h) <= 9_000_000
}

/// A serial label has only one short, printable ASCII text layer.  Requiring
/// no other drawing primitives keeps ordinary small forms from being merged
/// accidentally with a preceding JPEG.
fn serial_label(meta: &Metafile) -> bool {
    let chars: Vec<char> = meta
        .text
        .iter()
        .flat_map(|t| t.chars.iter().copied())
        .collect();
    !chars.is_empty()
        && chars.len() <= 64
        && meta.text.len() == 1
        && meta.images.is_empty()
        && meta.rasters.is_empty()
        && meta.fills.is_empty()
        && meta.shapes.is_empty()
        && chars
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | ':' | ' '))
}

/// Place a small label near the upper-right corner of its image page.  The
/// label's own frame supplies its physical text size; only its anchor is lost
/// when the vendor stores it as a separate saved-over sheet.
fn serial_label_place(meta: &Metafile, pw: f32, ph: f32) -> Option<Place> {
    let (w, h) = meta.points();
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let right = (pw * 0.10).max(8.0);
    let top = (ph * 0.04).max(4.0);
    Some(Place {
        x: (pw - right - w).max(0.0),
        top: top.min((ph - h).max(0.0)),
        w,
        h,
        ph,
    })
}

/// Lay the artwork belonging to one sheet onto the lower part of that sheet.
///
/// Their true positions live inside the sheet's own coding, so this makes no
/// claim to be the original layout: it is a contact sheet of what was salvaged
/// off the page. Bands of one picture are butted together first, so a drawing
/// the driver cut into hundreds of slivers comes back as a picture.
fn stack_pictures(
    w: &mut Writer,
    data: &[u8],
    runs: &[Vec<&Page>],
    pw: f32,
    ph: f32,
    content: &mut String,
    xobjects: &mut String,
) -> usize {
    /// One band: where its bytes are and how big it is.
    struct Band {
        offset: usize,
        len: usize,
        px: (u32, u32),
        comps: u8,
    }

    let read = |p: &Page| -> Option<Band> {
        match p.data {
            PageData::Jpeg { offset, len } if offset + len <= data.len() => {
                let info = jpeg::info(&data[offset..offset + len]);
                let px = info.map(|i| (i.width, i.height)).or(p.pixels)?;
                if px.0 == 0 || px.1 == 0 {
                    return None;
                }
                Some(Band {
                    offset,
                    len,
                    px,
                    comps: info.map(|i| i.components).unwrap_or(3),
                })
            }
            _ => None,
        }
    };

    let blocks: Vec<Vec<Band>> = runs
        .iter()
        .map(|r| r.iter().filter_map(|p| read(p)).collect::<Vec<_>>())
        .filter(|b: &Vec<Band>| !b.is_empty())
        .collect();
    if blocks.is_empty() {
        return 0;
    }

    let margin = margin_of(pw, ph);
    let area_w = (pw - 2.0 * margin).max(1.0);

    // If the widest block spans the page at a sensible print resolution, the
    // blocks are page-scale bands and their true size on the sheet is known:
    // only where they sit vertically is lost. Drawing them at that size beats
    // blowing them up to fill the sheet. Checked against a real page: a
    // recovered band measured 12.6% of the page height and landed on the
    // banner it came from.
    let widest_px = blocks
        .iter()
        .map(|b| b.iter().map(|x| x.px.0).max().unwrap_or(1))
        .max()
        .unwrap_or(1) as f32;
    let dpi = widest_px / (pw / 72.0);
    if (150.0..=1200.0).contains(&dpi) {
        let scale = pw / widest_px;
        let total: f32 = blocks
            .iter()
            .map(|b| b.iter().map(|x| x.px.1 as f32).sum::<f32>() * scale)
            .sum::<f32>()
            + margin * 0.3 * (blocks.len() - 1) as f32;
        if total <= ph * 0.88 {
            let mut y = ph * 0.92;
            let mut drawn = 0usize;
            let mut n = 0usize;
            for block in &blocks {
                let bw = block.iter().map(|b| b.px.0).max().unwrap_or(1) as f32 * scale;
                let bh: f32 = block.iter().map(|b| b.px.1 as f32).sum::<f32>() * scale;
                y -= bh;
                let x = (pw - bw) / 2.0;
                content.push_str(&format!("q\n1 0 0 1 {x:.2} {y:.2} cm\n"));
                let mut yy = bh;
                for band in block {
                    let w_pt = band.px.0 as f32 * scale;
                    let h_pt = band.px.1 as f32 * scale;
                    yy -= h_pt;
                    let space = match band.comps {
                        1 => "/DeviceGray",
                        4 => "/DeviceCMYK",
                        _ => "/DeviceRGB",
                    };
                    let img = w.add_stream(
                        format!(
                            "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                             /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode >>",
                            band.px.0, band.px.1
                        ),
                        &data[band.offset..band.offset + band.len],
                    );
                    xobjects.push_str(&format!("/Ar{n} {img} 0 R "));
                    content.push_str(&format!(
                        "q {:.3} 0 0 {:.3} 0 {yy:.3} cm /Ar{n} Do Q\n",
                        w_pt,
                        h_pt + 0.05
                    ));
                    n += 1;
                    drawn += 1;
                }
                content.push_str("Q\n");
                y -= margin * 0.3;
            }
            return drawn;
        }
    }
    // The note keeps the top of the sheet; the artwork gets the rest.
    let area_h = (ph * 0.82 - margin).max(1.0);
    let gutter = (pw.min(ph) * 0.01).clamp(1.0, 5.0);

    // Turning a tall block on its side wins a great deal of room. Decide once
    // for the whole sheet rather than block by block: a reader can turn a page
    // round, but not half of one.
    let size = |block: &Vec<Band>| {
        (
            block.iter().map(|b| b.px.0).max().unwrap_or(1) as f32,
            block.iter().map(|b| b.px.1 as f32).sum::<f32>(),
        )
    };
    let page_upright = area_h >= area_w;
    let turned = blocks
        .iter()
        .filter(|b| {
            let (bw, bh) = size(b);
            (bh > bw) != page_upright
        })
        .count();
    let rotate = turned * 2 > blocks.len();

    // Lay the blocks out in justified rows, the way a picture gallery does, so
    // the sheet is filled instead of leaving a grid of mostly empty cells.
    // Their true positions are inside the coding this crate does not read, so
    // no layout here is the original one; this one is merely legible.
    let shown: Vec<(f32, f32)> = blocks
        .iter()
        .map(|b| {
            let (bw, bh) = size(b);
            if rotate {
                (bh, bw)
            } else {
                (bw, bh)
            }
        })
        .collect();
    let target = area_h / (blocks.len() as f32).sqrt().ceil().max(1.0);
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut width_at_target = 0.0f32;
    for (i, (bw, bh)) in shown.iter().enumerate() {
        let w_at = bw * target / bh.max(1.0);
        if !row.is_empty() && width_at_target + w_at > area_w {
            rows.push(std::mem::take(&mut row));
            width_at_target = 0.0;
        }
        width_at_target += w_at;
        row.push(i);
    }
    if !row.is_empty() {
        rows.push(row);
    }

    // Give each row the height that makes it span the sheet exactly.
    let mut heights: Vec<f32> = Vec::with_capacity(rows.len());
    for r in &rows {
        let sum_ratio: f32 = r
            .iter()
            .map(|&i| shown[i].0 / shown[i].1.max(1.0))
            .sum::<f32>()
            .max(0.001);
        let inner = (area_w - gutter * (r.len() - 1) as f32).max(1.0);
        heights.push(inner / sum_ratio);
    }
    let total: f32 = heights.iter().sum::<f32>() + gutter * (rows.len() - 1) as f32;
    let squeeze = if total > area_h { area_h / total } else { 1.0 };

    let mut drawn = 0usize;
    let mut n = 0usize;
    let mut y_top = margin + area_h;
    for (r, row_h) in rows.iter().zip(heights.iter()) {
        let row_h = row_h * squeeze;
        let mut x = margin;
        for &i in r {
            let block = &blocks[i];
            let (sw, sh) = shown[i];
            let dh = row_h;
            let dw = sw * dh / sh.max(1.0);
            let scale = if rotate { dh / sw } else { dw / sw };
            let cx = x;
            let cy = y_top - dh;
            x += dw + gutter;
            // Place the block, then walk the bands down it with nothing between.
            content.push_str("q\n");
            if rotate {
                // A quarter turn about the block's lower left corner.
                content.push_str(&format!("0 1 -1 0 {:.2} {:.2} cm\n", cx + dw, cy));
            } else {
                content.push_str(&format!("1 0 0 1 {cx:.2} {cy:.2} cm\n"));
            }
            let mut y = block.iter().map(|b| b.px.1 as f32).sum::<f32>() * scale;
            for band in block {
                let w_pt = band.px.0 as f32 * scale;
                let h_pt = band.px.1 as f32 * scale;
                y -= h_pt;
                let space = match band.comps {
                    1 => "/DeviceGray",
                    4 => "/DeviceCMYK",
                    _ => "/DeviceRGB",
                };
                let img = w.add_stream(
                    format!(
                        "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                         /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode >>",
                        band.px.0, band.px.1
                    ),
                    &data[band.offset..band.offset + band.len],
                );
                xobjects.push_str(&format!("/Ar{n} {img} 0 R "));
                // A hair of overlap, so rounding never shows a seam.
                content.push_str(&format!(
                    "q {:.3} 0 0 {:.3} 0 {y:.3} cm /Ar{n} Do Q\n",
                    w_pt,
                    h_pt + 0.05
                ));
                n += 1;
                drawn += 1;
            }
            content.push_str("Q\n");
        }
        y_top -= row_h + gutter;
    }
    drawn
}

/// Build a PDF from whatever the document yields.
///
/// Returns the file bytes and a count of what went in. An empty document still
/// produces a valid one-page PDF so downstream tools have something to open.
pub fn build(data: &[u8], doc: &Document, opts: Options) -> (Vec<u8>, Report) {
    let decoder = LzhMetafileDecoder;
    let attachments = MagicAttachmentScanner;
    build_with(data, doc, opts, &decoder, &attachments)
}

/// Build a PDF using injected application ports.
pub fn build_with<D, A>(
    data: &[u8],
    doc: &Document,
    opts: Options,
    decoder: &D,
    attachments: &A,
) -> (Vec<u8>, Report)
where
    D: PageDecoder + ?Sized,
    A: AttachmentScanner + ?Sized,
{
    let mut w = Writer::new();
    let pages_id = w.reserve();
    let mut font_id: Option<FontIds> = None;
    // One embedded font serves every page, but which glyphs it must carry is
    // only known once every page has been drawn, so its id is reserved now and
    // its body written at the end.
    let embedded = opts.font.as_ref().map(|_| w.reserve());
    let mut used_glyphs: BTreeMap<u16, char> = BTreeMap::new();
    let mut glyph_widths: BTreeMap<u16, u16> = BTreeMap::new();
    // The PDF text operands use the remapped IDs from the subset. Validate the
    // font once before drawing so a font that the subsetter cannot read falls
    // back to full-font embedding without invalidating those IDs.
    let mut glyph_remapper = opts.font.as_ref().and_then(|font| {
        let remapper = subsetter::GlyphRemapper::new();
        subsetter::subset(&font.data, 0, &remapper)
            .ok()
            .map(|_| remapper)
    });
    let mut kids: Vec<usize> = Vec::new();
    // (object id, original page number) for each page that is only a marker.
    let mut gaps: Vec<(usize, usize)> = Vec::new();
    let mut report = Report::default();

    // One PDF page per sheet. The container's entry table also holds thumbnails
    // and the pictures a sheet is made of; putting those on sheets of their own
    // would turn a one page pamphlet into a four page document and make the
    // page numbering meaningless.
    let display_mode = !doc.display_pages.is_empty();
    let selected: Vec<&Page> = if display_mode {
        doc.display_pages
            .iter()
            .filter_map(|display| {
                display
                    .members
                    .first()
                    .and_then(|member| doc.pages.get(member.page_index))
            })
            .collect()
    } else {
        doc.pages
            .iter()
            .filter(|p| p.is_sheet() || (opts.include_previews && p.is_preview()))
            .collect()
    };

    // Some saved-over documents keep a small, text-only serial-number sheet
    // immediately after the JPEG it annotates.  It is a drawing layer, not a
    // second page: decode it once here, then paint it onto the preceding JPEG
    // and omit it from the output page sequence.
    let mut merged_labels: Vec<Option<Metafile>> = vec![None; selected.len()];
    let mut merge_label = vec![false; selected.len()];
    if opts.decode && !display_mode {
        for i in 1..selected.len() {
            let previous = selected[i - 1];
            let label = selected[i];
            if !matches!(previous.data, PageData::Jpeg { .. })
                || !label.is_sheet()
                || !is_text_only_label(label)
            {
                continue;
            }
            let Some(meta) = recovery::decode_page_for_document(data, label, doc, decoder) else {
                continue;
            };
            if serial_label(&meta) {
                merged_labels[i] = Some(meta);
                merge_label[i] = true;
            }
        }
    }

    let mut page_no = 0usize;
    if display_mode {
        for display in &doc.display_pages {
            page_no += 1;
            let rendered = draw_display_page(
                &mut w,
                data,
                doc,
                display,
                decoder,
                opts.paper,
                opts.decode,
                opts.font.as_deref(),
                &mut used_glyphs,
                &mut glyph_widths,
                &mut glyph_remapper,
            );
            let mut content = rendered.content;
            let xobjects = rendered.xobjects;
            if !rendered.recovered {
                if opts.missing == Missing::Skip {
                    report.skipped += 1;
                    continue;
                }
                content.push_str(&placeholder_content(
                    rendered.pw,
                    rendered.ph,
                    page_no,
                    opts.lang,
                ));
            }
            let fonts = if rendered.drawn.glyphs > 0 || !rendered.recovered {
                let font_lang = if rendered.drawn.glyphs > 0 {
                    Lang::Japanese
                } else {
                    opts.lang
                };
                let (latin, cjk, mincho, pmincho, pgothic) =
                    note_fonts(&mut w, &mut font_id, font_lang);
                match (embedded, font_lang) {
                    (Some(id), Lang::Japanese) => format!(
                        "/FA {id} 0 R /FJ {} 0 R /FM {} 0 R /FP {} 0 R /FG {} 0 R",
                        cjk.expect("Japanese font"),
                        mincho.expect("MS Mincho font"),
                        pmincho.expect("MS P Mincho font"),
                        pgothic.expect("MS P Gothic font")
                    ),
                    (Some(id), Lang::English) => format!("/FA {id} 0 R"),
                    (None, _) => font_resources(latin, cjk, mincho, pmincho, pgothic),
                }
            } else {
                String::new()
            };
            let resources = if fonts.is_empty() {
                format!("/XObject << {xobjects} >>")
            } else if xobjects.is_empty() {
                format!("/Font << {fonts} >>")
            } else {
                format!("/Font << {fonts} >> /XObject << {xobjects} >>")
            };
            let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
            let pid = w.add(format!(
                "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {:.2} {:.2}]{rotate} \
                 /Resources << {resources} >> /Contents {cid} 0 R >>",
                rendered.pw,
                rendered.ph,
                rotate = if rendered.rotation % 360 != 0 {
                    format!(" /Rotate {}", rendered.rotation % 360)
                } else {
                    String::new()
                },
            ));
            kids.push(pid);
            report.glyphs += rendered.drawn.glyphs;
            report.pictures_placed += rendered.drawn.pictures;
            if rendered.recovered {
                report.drawn += 1;
            } else {
                gaps.push((pid, page_no));
                report.placeholders += 1;
            }
        }
    } else {
        for (ordinal, p) in selected.iter().enumerate() {
            if merge_label[ordinal] {
                continue;
            }
            page_no += 1;
            // Quarter-turned pages are emitted with their displayed paper
            // dimensions.  A `/Rotate` flag on the original portrait box
            // would rotate an already-landscape metafile a second time and
            // leave the drawing in a small strip of the page.
            let rotate = if p.rotation % 180 == 90 {
                String::new()
            } else if p.rotation % 360 != 0 {
                format!(" /Rotate {}", p.rotation % 360)
            } else {
                String::new()
            };
            let decoded = opts
                .decode
                .then(|| recovery::decode_page_for_document(data, p, doc, decoder))
                .flatten();
            match &p.data {
                PageData::Jpeg { offset, len } => {
                    let stream = &data[*offset..*offset + *len];
                    let info = jpeg::info(stream);
                    let (w_px, h_px) = info
                        .map(|i| (i.width, i.height))
                        .or(p.pixels)
                        .unwrap_or((1, 1));
                    let native_natural = pdf_paper_points(p.paper)
                        .or_else(|| info.map(|i| i.points()))
                        .unwrap_or((w_px as f32, h_px as f32));
                    let natural = if p.rotation % 180 == 90 {
                        (native_natural.1, native_natural.0)
                    } else {
                        native_natural
                    };
                    let (pw, ph) = opts.paper.unwrap_or(natural);

                    // A picture page may carry a drawing of its own that says
                    // where the picture and its companions go and what is
                    // written over them. When it does, that is the page.
                    let mut overlay = String::new();
                    let mut overlay_xobjects = String::new();
                    let over = if opts.decode && p.overlays.iter().any(|o| o.area.is_none()) {
                        draw_overlay_list(
                            &mut w,
                            data,
                            doc,
                            Some(p),
                            p.paper.unwrap_or((21000, 29700)),
                            &p.overlays,
                            decoder,
                            pw,
                            ph,
                            &mut overlay,
                            &mut overlay_xobjects,
                            opts.font.as_deref(),
                            &mut used_glyphs,
                            &mut glyph_widths,
                            &mut glyph_remapper,
                            p.rotation,
                        )
                    } else {
                        Drawn {
                            glyphs: 0,
                            pictures: 0,
                            painted: false,
                        }
                    };
                    if over.pictures > 0 {
                        let fonts = match embedded {
                            Some(id) => {
                                let (_, cjk, mincho, pmincho, pgothic) =
                                    note_fonts(&mut w, &mut font_id, Lang::Japanese);
                                format!(
                                    "/FA {id} 0 R /FJ {} 0 R /FM {} 0 R /FP {} 0 R /FG {} 0 R",
                                    cjk.expect("Japanese font"),
                                    mincho.expect("MS Mincho font"),
                                    pmincho.expect("MS P Mincho font"),
                                    pgothic.expect("MS P Gothic font")
                                )
                            }
                            None => {
                                let (latin, cjk, mincho, pmincho, pgothic) =
                                    note_fonts(&mut w, &mut font_id, Lang::Japanese);
                                font_resources(latin, cjk, mincho, pmincho, pgothic)
                            }
                        };
                        let cid = w.add_stream("<< >>".to_string(), overlay.as_bytes());
                        let pid = w.add(format!(
                        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}]{rotate} \
                         /Resources << /Font << {fonts} >> /XObject << {overlay_xobjects} >> >> \
                         /Contents {cid} 0 R >>"
                    ));
                        kids.push(pid);
                        report.embedded += 1;
                        report.glyphs += over.glyphs;
                        report.pictures_placed += over.pictures.saturating_sub(1);
                        continue;
                    }

                    // The paper frame is authoritative for the PDF page, but it
                    // is not necessarily the same shape as the JPEG.  In
                    // particular, some kind-5 pages carry an A4 frame around a
                    // landscape image.  Size the image from its pixel aspect
                    // ratio and contain it in the page so the pixels are never
                    // stretched.
                    let source_frame = if p.paper.is_some() {
                        contained_size(natural, (w_px as f32, h_px as f32))
                    } else {
                        natural
                    };
                    let image_place = fit_image_place(Place::sheet(pw, ph), source_frame, false);
                    let space = match info.map(|i| i.components).unwrap_or(3) {
                        1 => "/DeviceGray",
                        4 => "/DeviceCMYK",
                        _ => "/DeviceRGB",
                    };
                    let img = w.add_stream(
                        format!(
                            "<< /Type /XObject /Subtype /Image /Width {w_px} /Height {h_px} \
                         /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode >>"
                        ),
                        stream,
                    );
                    // A sheet that is itself a picture can still have pictures of
                    // its own sitting on it. Dropping them because the sheet came
                    // out would be throwing away recovered content, so
                    // the sheet takes the upper part of the page and they follow.
                    let runs = doc.picture_runs(p.index);
                    let mut xobjects = format!("/Im0 {img} 0 R ");
                    let (dw, dh, dx, dy) = if runs.is_empty() {
                        (
                            image_place.w,
                            image_place.h,
                            image_place.x,
                            ph - image_place.top - image_place.h,
                        )
                    } else {
                        let k = (ph * 0.52) / image_place.h.max(0.01);
                        let k = k.min(1.0);
                        (
                            image_place.w * k,
                            image_place.h * k,
                            (pw - image_place.w * k) / 2.0,
                            ph - margin_of(pw, ph) - image_place.h * k,
                        )
                    };
                    let mut content =
                        format!("q {dw:.2} 0 0 {dh:.2} {dx:.2} {dy:.2} cm /Im0 Do Q\n");
                    if !runs.is_empty() {
                        report.pictures_placed += stack_pictures(
                            &mut w,
                            data,
                            &runs,
                            pw,
                            ph * 0.46,
                            &mut content,
                            &mut xobjects,
                        );
                    }
                    // Text-only page overlays belong above the JPEG. Draw them
                    // after the image so they cannot disappear underneath it.
                    let mut drawn_glyphs = 0usize;
                    if over.painted && over.pictures == 0 {
                        content.push_str(&overlay);
                        xobjects.push_str(&overlay_xobjects);
                        drawn_glyphs += over.glyphs;
                    }
                    if let Some(label) = merged_labels.get(ordinal + 1).and_then(Option::as_ref) {
                        if let Some(place) = serial_label_place(label, pw, ph) {
                            let drawn = draw_page(
                                &mut w,
                                data,
                                &[],
                                label,
                                place,
                                "L",
                                &mut content,
                                &mut xobjects,
                                opts.font.as_deref(),
                                &mut used_glyphs,
                                &mut glyph_widths,
                                &mut glyph_remapper,
                            );
                            drawn_glyphs += drawn.glyphs;
                        }
                    }
                    let fonts = if drawn_glyphs > 0 {
                        match embedded {
                            Some(id) => {
                                let (_, cjk, mincho, pmincho, pgothic) =
                                    note_fonts(&mut w, &mut font_id, Lang::Japanese);
                                format!(
                                    "/FA {id} 0 R /FJ {} 0 R /FM {} 0 R /FP {} 0 R /FG {} 0 R",
                                    cjk.expect("Japanese font"),
                                    mincho.expect("MS Mincho font"),
                                    pmincho.expect("MS P Mincho font"),
                                    pgothic.expect("MS P Gothic font")
                                )
                            }
                            None => {
                                let (latin, cjk, mincho, pmincho, pgothic) =
                                    note_fonts(&mut w, &mut font_id, Lang::Japanese);
                                font_resources(latin, cjk, mincho, pmincho, pgothic)
                            }
                        }
                    } else {
                        String::new()
                    };
                    let resources = if fonts.is_empty() {
                        format!("/XObject << {xobjects} >>")
                    } else {
                        format!("/Font << {fonts} >> /XObject << {xobjects} >>")
                    };
                    let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                    let pid = w.add(format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}]{rotate} \
                     /Resources << {resources} >> /Contents {cid} 0 R >>"
                ));
                    kids.push(pid);
                    report.embedded += 1;
                    report.glyphs += drawn_glyphs;
                }
                // The sheet is in the container's own coding. Expand it: what comes
                // out is a metafile, and its text is real text with real positions,
                // so the page can be redrawn rather than apologised for.
                PageData::Encoded { .. } | PageData::Preview { .. } | PageData::Bare { .. }
                    if decoded.is_some() =>
                {
                    let meta = decoded.as_ref().expect("decoded guard above");
                    let (nw, nh) = meta.points();
                    let native_paper = pdf_paper_points(p.paper)
                        .unwrap_or(if nw > 1.0 && nh > 1.0 { (nw, nh) } else { A4 });
                    let shown_paper = if p.rotation % 180 == 90 {
                        (native_paper.1, native_paper.0)
                    } else {
                        native_paper
                    };
                    let (pw, ph) = opts.paper.unwrap_or(shown_paper);
                    let mut content = String::new();
                    let mut xobjects = String::new();
                    let stored: Vec<&Page> = doc.pictures_on(p.index).collect();
                    let target = Place::sheet(pw, ph);
                    let turn = component_rotation(meta, (pw, ph), p.rotation);
                    let (draw_place, matrix) = if turn == 0 {
                        (target, None)
                    } else {
                        oriented_place(target, meta, turn)
                    };
                    let mut main_content = String::new();
                    let mut main_xobjects = String::new();
                    let drawn = draw_page_with_viewbox(
                        &mut w,
                        data,
                        &stored,
                        meta,
                        draw_place,
                        "M",
                        &mut main_content,
                        &mut main_xobjects,
                        opts.font.as_deref(),
                        &mut used_glyphs,
                        &mut glyph_widths,
                        &mut glyph_remapper,
                        None,
                    );
                    if let Some(matrix) = matrix {
                        append_matrix(&mut content, matrix, &main_content);
                    } else {
                        content.push_str(&main_content);
                    }
                    xobjects.push_str(&main_xobjects);
                    let over = draw_overlay_list(
                        &mut w,
                        data,
                        doc,
                        Some(p),
                        p.paper.unwrap_or((21000, 29700)),
                        &p.overlays,
                        decoder,
                        pw,
                        ph,
                        &mut content,
                        &mut xobjects,
                        opts.font.as_deref(),
                        &mut used_glyphs,
                        &mut glyph_widths,
                        &mut glyph_remapper,
                        p.rotation,
                    );
                    let (glyphs, placed) =
                        (drawn.glyphs + over.glyphs, drawn.pictures + over.pictures);
                    let fonts = match embedded {
                        Some(id) => {
                            let (_, cjk, mincho, pmincho, pgothic) =
                                note_fonts(&mut w, &mut font_id, Lang::Japanese);
                            format!(
                                "/FA {id} 0 R /FJ {} 0 R /FM {} 0 R /FP {} 0 R /FG {} 0 R",
                                cjk.expect("Japanese font"),
                                mincho.expect("MS Mincho font"),
                                pmincho.expect("MS P Mincho font"),
                                pgothic.expect("MS P Gothic font")
                            )
                        }
                        None => {
                            let (latin, cjk, mincho, pmincho, pgothic) =
                                note_fonts(&mut w, &mut font_id, Lang::Japanese);
                            font_resources(latin, cjk, mincho, pmincho, pgothic)
                        }
                    };

                    let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                    let pid = w.add(format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}]{rotate} \
                     /Resources << /Font << {} >>{} >> /Contents {cid} 0 R >>",
                    fonts,
                    if xobjects.is_empty() {
                        String::new()
                    } else {
                        format!(" /XObject << {xobjects} >>")
                    }
                ));
                    kids.push(pid);
                    report.drawn += 1;
                    report.glyphs += glyphs;
                    report.pictures_placed += placed;
                }
                _ if opts.missing == Missing::Placeholder => {
                    let (pw, ph) = opts
                        .paper
                        .or_else(|| pdf_paper_points(p.paper))
                        .unwrap_or(A4);
                    // Recovered overlays use the same Japanese CID font as a
                    // decoded page even when the placeholder note language is
                    // English. Reserve all source-face resources before drawing
                    // the overlay; draw_text selects them per recovered run.
                    let overlay_lang = if p.overlays.is_empty() {
                        opts.lang
                    } else {
                        Lang::Japanese
                    };
                    let (latin, cjk, mincho, pmincho, pgothic) =
                        note_fonts(&mut w, &mut font_id, overlay_lang);
                    let mut content = String::new();
                    let mut xobjects = String::new();
                    let over = if opts.decode && !p.overlays.is_empty() {
                        draw_overlay_list(
                            &mut w,
                            data,
                            doc,
                            Some(p),
                            p.paper.unwrap_or((21000, 29700)),
                            &p.overlays,
                            decoder,
                            pw,
                            ph,
                            &mut content,
                            &mut xobjects,
                            opts.font.as_deref(),
                            &mut used_glyphs,
                            &mut glyph_widths,
                            &mut glyph_remapper,
                            p.rotation,
                        )
                    } else {
                        Drawn {
                            glyphs: 0,
                            pictures: 0,
                            painted: false,
                        }
                    };
                    if over.painted {
                        let fonts = match embedded {
                            Some(id) => {
                                let (_, cjk, mincho, pmincho, pgothic) =
                                    note_fonts(&mut w, &mut font_id, Lang::Japanese);
                                format!(
                                    "/FA {id} 0 R /FJ {} 0 R /FM {} 0 R /FP {} 0 R /FG {} 0 R",
                                    cjk.expect("Japanese font"),
                                    mincho.expect("MS Mincho font"),
                                    pmincho.expect("MS P Mincho font"),
                                    pgothic.expect("MS P Gothic font")
                                )
                            }
                            None => font_resources(latin, cjk, mincho, pmincho, pgothic),
                        };
                        let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                        let pid = w.add(format!(
                        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}]{rotate} \
                         /Resources << /Font << {fonts} >>{} >> /Contents {cid} 0 R >>",
                        if xobjects.is_empty() {
                            String::new()
                        } else {
                            format!(" /XObject << {xobjects} >>")
                        }
                    ));
                        kids.push(pid);
                        report.drawn += 1;
                        report.glyphs += over.glyphs;
                        report.pictures_placed += over.pictures;
                        continue;
                    }

                    let mut content = placeholder_content(pw, ph, page_no, opts.lang);

                    // The sheet itself cannot be reproduced, but the pictures it is
                    // made of are plain JPEG. Stack them on the sheet rather than
                    // leaving it blank: a pamphlet page comes back as its artwork,
                    // which is a great deal better than an empty rectangle.
                    xobjects.clear();
                    let runs = doc.picture_runs(p.index);
                    let placed =
                        stack_pictures(&mut w, data, &runs, pw, ph, &mut content, &mut xobjects);
                    report.pictures_placed += placed;

                    let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                    let pid = w.add(format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}]{rotate} \
                     /Resources << /Font << {} >>{} >> /Contents {cid} 0 R >>",
                    font_resources(latin, cjk, mincho, pmincho, pgothic),
                    if xobjects.is_empty() {
                        String::new()
                    } else {
                        format!(" /XObject << {xobjects} >>")
                    }
                ));
                    kids.push(pid);
                    gaps.push((pid, page_no));
                    report.placeholders += 1;
                }
                _ => report.skipped += 1,
            }
        }
    }

    if kids.is_empty() {
        let (latin, cjk, mincho, pmincho, pgothic) = note_fonts(&mut w, &mut font_id, opts.lang);
        let content = placeholder_content(A4.0, A4.1, 0, opts.lang);
        let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
        let pid = w.add(format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {:.2} {:.2}] \
             /Resources << /Font << {} >> >> /Contents {cid} 0 R >>",
            A4.0,
            A4.1,
            font_resources(latin, cjk, mincho, pmincho, pgothic)
        ));
        kids.push(pid);
    }

    if let (Some(id), Some(font)) = (embedded, opts.font.as_deref()) {
        if used_glyphs.is_empty() {
            // Nothing used it; leave a harmless object rather than a dangling id.
            w.set(
                id,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            );
        } else if let Some(remapper) = glyph_remapper.as_ref() {
            for old_gid in remapper.remapped_gids() {
                if let Some(new_gid) = remapper.get(old_gid) {
                    glyph_widths
                        .entry(new_gid)
                        .or_insert_with(|| font.width(old_gid));
                }
            }
            let subset = subsetter::subset(&font.data, 0, remapper)
                .expect("font was validated before PDF drawing");
            embed_font(&mut w, id, font, &subset, &used_glyphs, &glyph_widths);
        } else {
            embed_font(&mut w, id, font, &font.data, &used_glyphs, &glyph_widths);
        }
    }

    let kid_refs: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
    w.set(
        pages_id,
        format!(
            "<< /Type /Pages /Count {} /Kids [{}] >>",
            kids.len(),
            kid_refs.join(" ")
        ),
    );
    // Carry any original file found in the container along with the pages. A
    // reader shows these in its attachment pane; the source document then
    // travels with the conversion rather than beside it.
    let mut names: Vec<String> = Vec::new();
    if opts.embed_originals {
        for (i, a) in attachments.scan(data).iter().enumerate() {
            let payload = a.bytes(data);
            let name = format!("original{}.{}", i + 1, a.kind.extension());
            let stream = w.add_stream(
                format!(
                    "<< /Type /EmbeddedFile /Params << /Size {} >> >>",
                    payload.len()
                ),
                payload,
            );
            let spec = w.add(format!(
                "<< /Type /Filespec /F ({name}) /UF ({name}) \
                 /Desc (original file carried inside the source document) \
                 /EF << /F {stream} 0 R >> >>"
            ));
            names.push(format!("({name}) {spec} 0 R"));
            report.attachments += 1;
        }
    }
    if opts.carry_source {
        let name = source_name(opts.title.as_deref());
        let stream = w.add_stream(
            format!(
                "<< /Type /EmbeddedFile /Params << /Size {} >> >>",
                data.len()
            ),
            data,
        );
        let spec = w.add(format!(
            "<< /Type /Filespec /F ({name}) /UF ({name}) \
             /Desc (the source document this file was made from) \
             /EF << /F {stream} 0 R >> >>"
        ));
        names.push(format!("({name}) {spec} 0 R"));
        report.attachments += 1;
    }

    // Bookmarks for the gaps. A three hundred page conversion with a dozen
    // holes in it is unusable without them.
    let mut outline_ref = String::new();
    if opts.bookmark_gaps && !gaps.is_empty() {
        let root = w.reserve();
        let ids: Vec<usize> = gaps.iter().map(|_| w.reserve()).collect();
        for (n, (&item, &(page_obj, page_no))) in ids.iter().zip(gaps.iter()).enumerate() {
            let mut entry = format!(
                "<< /Title ({}) /Parent {root} 0 R /Dest [{page_obj} 0 R /XYZ null null null]",
                escape(&format!("Page {page_no} not recovered"))
            );
            if n > 0 {
                entry.push_str(&format!(" /Prev {} 0 R", ids[n - 1]));
            }
            if n + 1 < ids.len() {
                entry.push_str(&format!(" /Next {} 0 R", ids[n + 1]));
            }
            entry.push_str(" >>");
            w.set(item, entry);
        }
        w.set(
            root,
            format!(
                "<< /Type /Outlines /Count {} /First {} 0 R /Last {} 0 R >>",
                ids.len(),
                ids[0],
                ids[ids.len() - 1]
            ),
        );
        outline_ref = format!(" /Outlines {root} 0 R");
        report.bookmarks = ids.len();
    }

    let mut catalog = format!("<< /Type /Catalog /Pages {pages_id} 0 R{outline_ref}");
    // A reader honours one page mode. Attachments win over the bookmark list:
    // an original document sitting unnoticed in the pane is the bigger loss,
    // and the bookmarks are still there to be opened.
    if report.attachments > 0 {
        catalog.push_str(" /PageMode /UseAttachments");
    } else if !outline_ref.is_empty() {
        catalog.push_str(" /PageMode /UseOutlines");
    }
    if !names.is_empty() {
        catalog.push_str(&format!(
            " /Names << /EmbeddedFiles << /Names [{}] >> >>",
            names.join(" ")
        ));
    }
    catalog.push_str(" >>");
    let root = w.add(catalog);

    // Record what this file is and how complete it is, so the PDF says for
    // itself how much of the original it carries.
    let mut info = String::from("<< /Producer (xdw-salvage)");
    if let Some(t) = &opts.title {
        info.push_str(&format!(" /Title {}", text_string(t)));
    }
    info.push_str(&format!(
        " /Subject (recovered {} of {} page(s); {} placeholder(s), {} attachment(s)) >>",
        report.embedded + report.drawn,
        selected
            .len()
            .saturating_sub(merge_label.iter().filter(|v| **v).count()),
        report.placeholders,
        report.attachments
    ));
    let info_id = w.add(info);
    (w.finish(root, Some(info_id)), report)
}

/// The fonts a placeholder note may need: always Helvetica, plus a Japanese
/// face when one is asked for.
///
/// The Japanese face is a Windows system font rather than an embedded font,
/// which keeps the file small but leaves the glyphs to the reader. The modern
/// UTF-16 CMap covers the Japanese Unicode mappings used by current PDF
/// readers. Readers without the mapping installed draw nothing at all for
/// them, so a Japanese note is always accompanied by a plain line that every
/// reader can show. A placeholder that renders blank would be worse than a
/// clumsy one.
fn note_fonts(w: &mut Writer, cache: &mut Option<FontIds>, lang: Lang) -> FontIds {
    if let Some(ids) = *cache {
        return ids;
    }
    let latin = w.add(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
         /Encoding /WinAnsiEncoding >>"
            .to_string(),
    );
    let cjk = match lang {
        Lang::English => None,
        Lang::Japanese => {
            let descriptor = w.add(
                "<< /Type /FontDescriptor /FontName /MS-Mincho /Flags 6 \
                 /FontBBox [-1000 -140 1000 859] /ItalicAngle 0 \
                 /Ascent 859 /Descent -140 /CapHeight 679 /StemV 1000 >>"
                    .to_string(),
            );
            let descendant = w.add(format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MS-Mincho \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> \
                 /FontDescriptor {descriptor} 0 R /DW 1000 >>"
            ));
            // Without an embedded font, a reader that lacks the Japanese
            // character collection draws nothing and, worse, copies nothing.
            // A ToUnicode map costs a few hundred bytes and makes the text
            // selectable and searchable even then. The codes are UTF-16 units,
            // so the mapping is the identity.
            let to_unicode = w.add_stream(
                "<< >>".to_string(),
                b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                  /CMapName /Identity-UCS def /CMapType 2 def\n\
                  /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
                  1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                  1 beginbfrange <0000> <FFFF> <0000> endbfrange\n\
                  endcmap CMapName currentdict /CMap defineresource pop end end",
            );
            Some(w.add(format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /MS-Mincho \
                 /Encoding /UniJIS-UTF16-H /DescendantFonts [{descendant} 0 R] \
                 /ToUnicode {to_unicode} 0 R >>"
            )))
        }
    };
    let mincho = match lang {
        Lang::English => None,
        Lang::Japanese => Some(named_japanese_font(w, "MS-Mincho")),
    };
    let pmincho = match lang {
        Lang::English => None,
        Lang::Japanese => Some(named_japanese_font(w, "MS-PMincho")),
    };
    let pgothic = match lang {
        Lang::English => None,
        Lang::Japanese => Some(named_japanese_font(w, "MS-PGothic")),
    };
    *cache = Some((latin, cjk, mincho, pmincho, pgothic));
    (latin, cjk, mincho, pmincho, pgothic)
}

/// A Japanese system face used by a source text run.  It is deliberately
/// separate from the comparison face (`/FJ`): the latter may be HeiseiMin-W3,
/// while ASCII punctuation in a source MS-Mincho run must retain its original
/// font metrics and glyph design.
fn named_japanese_font(w: &mut Writer, name: &str) -> usize {
    let descriptor = w.add(format!(
        "<< /Type /FontDescriptor /FontName /{name} /Flags 6 \
         /ItalicAngle 0 /Ascent 859 /Descent -140 /CapHeight 679 /StemV 1000 >>"
    ));
    let descendant = w.add(format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> \
         /FontDescriptor {descriptor} 0 R /DW 1000 >>"
    ));
    let to_unicode = w.add_stream(
        "<< >>".to_string(),
        b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
          /CMapName /Identity-UCS def /CMapType 2 def\n\
          /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
          1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
          1 beginbfrange <0000> <FFFF> <0000> endbfrange\n\
          endcmap CMapName currentdict /CMap defineresource pop end end",
    );
    w.add(format!(
        "<< /Type /Font /Subtype /Type0 /BaseFont /{name} \
         /Encoding /UniJIS-UTF16-H /DescendantFonts [{descendant} 0 R] \
         /ToUnicode {to_unicode} 0 R >>"
    ))
}

/// Wordings for a placeholder, longest first, so the fullest one that fits at a
/// readable size gets used.
/// A plain ASCII file name for the carried source, safe inside a PDF string.
fn source_name(title: Option<&str>) -> String {
    let base = title.unwrap_or("source.xdw");
    let base = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let mut out: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.trim_matches('_').is_empty() {
        out = "source".into();
    }
    if !out.to_ascii_lowercase().ends_with(".xdw") && !out.to_ascii_lowercase().ends_with(".xbd") {
        out.push_str(".xdw");
    }
    out
}

fn notes(page_no: usize, lang: Lang) -> Vec<String> {
    match (lang, page_no) {
        (Lang::English, 0) => vec!["No page of this document could be recovered.".into()],
        (Lang::English, n) => vec![
            format!("Page {n} could not be recovered: image stored in the container's own coding."),
            format!("Page {n} not recovered (vendor coding)"),
            format!("Page {n} not recovered"),
            format!("p.{n}"),
        ],
        (Lang::Japanese, 0) => vec![
            "この文書からは 1 ページも取り出せませんでした。".into(),
            "取り出せるページがありません".into(),
        ],
        (Lang::Japanese, n) => vec![
            format!("{n} ページ目は取り出せませんでした（独自符号化のため）"),
            format!("{n} ページ目：取り出せません"),
            format!("{n} ページ目"),
            format!("p.{n}"),
        ],
    }
}

/// The font entries a placeholder page needs in its resource dictionary.
fn font_resources(
    latin: usize,
    cjk: Option<usize>,
    mincho: Option<usize>,
    pmincho: Option<usize>,
    pgothic: Option<usize>,
) -> String {
    let mut fonts = format!("/FA {latin} 0 R");
    if let Some(j) = cjk {
        fonts.push_str(&format!(" /FJ {j} 0 R"));
    }
    if let Some(m) = mincho {
        fonts.push_str(&format!(" /FM {m} 0 R"));
    }
    if let Some(p) = pmincho {
        fonts.push_str(&format!(" /FP {p} 0 R"));
    }
    if let Some(g) = pgothic {
        fonts.push_str(&format!(" /FG {g} 0 R"));
    }
    fonts
}

/// Rough width of a string in em units, for fitting text to a page.
///
/// Latin letters average a little over half an em; the Japanese glyphs used
/// here are full width.
fn width_em(s: &str) -> f32 {
    s.chars()
        .map(|c| if (c as u32) < 0x100 { 0.55 } else { 1.0 })
        .sum()
}

/// Write an embedded CID font and return its object id.
///
/// The font is keyed by glyph index, so the text operand carries glyph indices
/// and a ToUnicode map carries the characters back. That combination renders
/// and copies correctly in any reader, with nothing installed.
fn embed_font(
    w: &mut Writer,
    id: usize,
    font: &ttf::Font,
    data: &[u8],
    used: &BTreeMap<u16, char>,
    glyph_widths: &BTreeMap<u16, u16>,
) {
    let compressed = deflate::zlib(data);
    let file = w.add_stream(
        format!("<< /Length1 {} /Filter /FlateDecode >>", data.len()),
        &compressed,
    );
    let descriptor = w.add(format!(
        "<< /Type /FontDescriptor /FontName /{} /Flags 4 /FontBBox [-1000 -1000 2000 2000] \
         /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 \
         /FontFile2 {file} 0 R >>",
        font.name
    ));
    // Widths, run-compressed the way PDF allows.
    let mut widths = String::from("[");
    let mut run: Vec<u16> = Vec::new();
    let mut run_start: Option<u16> = None;
    let flush = |widths: &mut String, start: Option<u16>, run: &mut Vec<u16>| {
        if let Some(st) = start {
            if !run.is_empty() {
                widths.push_str(&format!("{st} ["));
                for (i, v) in run.iter().enumerate() {
                    if i > 0 {
                        widths.push(' ');
                    }
                    widths.push_str(&v.to_string());
                }
                widths.push_str("] ");
            }
        }
        run.clear();
    };
    let mut prev: Option<u16> = None;
    for &gid in used.keys() {
        if prev.is_none_or(|p| gid != p + 1) {
            flush(&mut widths, run_start, &mut run);
            run_start = Some(gid);
        }
        run.push(glyph_widths.get(&gid).copied().unwrap_or(1000));
        prev = Some(gid);
    }
    flush(&mut widths, run_start, &mut run);
    widths.push(']');

    let cid = w.add(format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{} \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
         /FontDescriptor {descriptor} 0 R /DW 1000 /W {widths} /CIDToGIDMap /Identity >>",
        font.name
    ));
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Identity-UCS def /CMapType 2 def\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
    );
    // bfchar sections take at most 100 entries each.
    let entries: Vec<(u16, char)> = used.iter().map(|(g, c)| (*g, *c)).collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (g, c) in chunk {
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            let hex: String = units.iter().map(|u| format!("{u:04X}")).collect();
            cmap.push_str(&format!("<{g:04X}> <{hex}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap CMapName currentdict /CMap defineresource pop end end");
    let to_unicode = w.add_stream("<< >>".to_string(), cmap.as_bytes());
    w.set(
        id,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{} /Encoding /Identity-H \
             /DescendantFonts [{cid} 0 R] /ToUnicode {to_unicode} 0 R >>",
            font.name
        ),
    );
}

/// Everything one sheet draws, in the metafile's own order.
struct Drawn {
    glyphs: usize,
    pictures: usize,
    painted: bool,
}

/// Turn a page's drawing model into PDF content.
///
/// Every primitive goes down in the order the metafile recorded it, so a
/// white block drawn over a picture still hides it and text still sits on
/// top of the rule under it. Metafile y grows downward and PDF y grows
/// upward, so everything is measured from the top of the sheet.
/// Where on the sheet a drawing goes, in points: its left edge, its top
/// edge measured down from the top of the sheet, its size, and the sheet
/// height for turning y the right way up.
#[derive(Debug, Clone, Copy)]
struct Place {
    x: f32,
    top: f32,
    w: f32,
    h: f32,
    ph: f32,
}

impl Place {
    fn sheet(pw: f32, ph: f32) -> Self {
        Place {
            x: 0.0,
            top: 0.0,
            w: pw,
            h: ph,
            ph,
        }
    }
}

/// Largest rectangle with `image`'s aspect ratio that fits inside `frame`.
///
/// A page's paper frame and its recovered JPEG do not always agree.  Keep the
/// paper for the page itself, but use the actual image dimensions for the
/// rectangle that receives the pixels.
fn contained_size(frame: (f32, f32), image: (f32, f32)) -> (f32, f32) {
    if !(frame.0.is_finite()
        && frame.1.is_finite()
        && image.0.is_finite()
        && image.1.is_finite()
        && frame.0 > 0.0
        && frame.1 > 0.0
        && image.0 > 0.0
        && image.1 > 0.0)
    {
        return frame;
    }
    let scale = (frame.0 / image.0).min(frame.1 / image.1);
    (image.0 * scale, image.1 * scale)
}

/// Fit an image frame into a placement rectangle, preserving its aspect
/// ratio.  `allow_upscale` is false for standalone page images so a tiny
/// source is not enlarged merely because the output paper is larger.
fn fit_image_place(place: Place, image: (f32, f32), allow_upscale: bool) -> Place {
    if !(image.0.is_finite()
        && image.1.is_finite()
        && image.0 > 0.0
        && image.1 > 0.0
        && place.w.is_finite()
        && place.h.is_finite()
        && place.w > 0.0
        && place.h > 0.0)
    {
        return place;
    }
    let mut scale = (place.w / image.0).min(place.h / image.1);
    if !allow_upscale {
        scale = scale.min(1.0);
    }
    if !(scale.is_finite() && scale > 0.0) {
        return place;
    }
    let w = image.0 * scale;
    let h = image.1 * scale;
    Place {
        x: place.x + (place.w - w) * 0.5,
        top: place.top + (place.h - h) * 0.5,
        w,
        h,
        ph: place.ph,
    }
}

/// PDFの描画状態に適用する2次元変換。
#[derive(Debug, Clone, Copy)]
struct PdfMatrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

/// 表示ページの回転を、ページ全体の `/Rotate` ではなく描画内容へ適用する。
///
/// XDWの一部のEMFは、プロパティ上の用紙フレームを縦向きで保持したまま、
/// 実際の描画座標を回転後の向きで格納する。ページを先に回転させると、
/// PDFの用紙向きが逆になり、内容も縮小されるため、局所座標系で描いてから
/// 最終ページへ回転配置する。
fn oriented_place(place: Place, meta: &Metafile, rotation: u16) -> (Place, Option<PdfMatrix>) {
    oriented_content_place(place, meta.points(), rotation)
}

/// Apply a displayed-page rotation to an image whose native frame is known in
/// points. JPEG pages do not have a metafile to carry that frame, but they use
/// the same local-coordinate transform as decoded vector/raster pages.
fn oriented_image_place(
    place: Place,
    frame: (f32, f32),
    rotation: u16,
) -> (Place, Option<PdfMatrix>) {
    let rotation = rotation % 360;
    if !(frame.0.is_finite() && frame.1.is_finite() && frame.0 > 0.0 && frame.1 > 0.0) {
        return (place, None);
    }
    let (shown_w, shown_h) = if rotation % 180 == 90 {
        (frame.1, frame.0)
    } else if rotation == 0 || rotation == 180 {
        frame
    } else {
        return (place, None);
    };
    let scale = (place.w / shown_w).min(place.h / shown_h);
    if !(scale.is_finite() && scale > 0.0) {
        return (place, None);
    }
    let local_w = frame.0 * scale;
    let local_h = frame.1 * scale;
    let shown_w = shown_w * scale;
    let shown_h = shown_h * scale;
    let left = place.x + (place.w - shown_w) * 0.5;
    let top = place.top + (place.h - shown_h) * 0.5;
    if rotation == 0 {
        return (
            Place {
                x: left,
                top,
                w: local_w,
                h: local_h,
                ph: place.ph,
            },
            None,
        );
    }

    let local = Place {
        x: 0.0,
        top: 0.0,
        w: local_w,
        h: local_h,
        ph: local_h,
    };
    let top_pdf = place.ph - top;
    let matrix = match rotation {
        // 時計回り90度: (x, y) -> (height - y, x) in top-down coordinates.
        90 => PdfMatrix {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: left,
            f: top_pdf,
        },
        180 => PdfMatrix {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: left + local_w,
            f: top_pdf,
        },
        // 反時計回り90度: (x, y) -> (y, width - x) in top-down coordinates.
        270 => PdfMatrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: left + local_h,
            f: top_pdf - local_w,
        },
        _ => unreachable!("rotation was normalized above"),
    };
    (local, Some(matrix))
}

fn oriented_content_place(
    place: Place,
    (sw, sh): (f32, f32),
    rotation: u16,
) -> (Place, Option<PdfMatrix>) {
    let rotation = rotation % 360;
    if rotation == 0 {
        return (place, None);
    }
    if !(sw.is_finite() && sh.is_finite() && sw > 0.0 && sh > 0.0) {
        return (place, None);
    }
    let (rotated_w, rotated_h) = if rotation % 180 == 90 {
        (sh, sw)
    } else if rotation == 180 {
        (sw, sh)
    } else {
        return (place, None);
    };
    let fit = (place.w / rotated_w).min(place.h / rotated_h);
    if !(fit.is_finite() && fit > 0.0) {
        return (place, None);
    }
    let local_w = sw * fit;
    let local_h = sh * fit;
    let local = Place {
        x: 0.0,
        top: 0.0,
        w: local_w,
        h: local_h,
        ph: local_h,
    };
    let top_pdf = place.ph - place.top;
    let matrix = match rotation {
        // 時計回り90度: (x, y) -> (height - y, x) in top-down coordinates.
        90 => PdfMatrix {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: place.x,
            f: top_pdf,
        },
        180 => PdfMatrix {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: place.x + local_w,
            f: top_pdf,
        },
        // 反時計回り90度: (x, y) -> (y, width - x) in top-down coordinates.
        270 => PdfMatrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: place.x + local_h,
            f: top_pdf - local_w,
        },
        _ => unreachable!("rotation was normalized above"),
    };
    (local, Some(matrix))
}

fn append_matrix(out: &mut String, matrix: PdfMatrix, body: &str) {
    if body.is_empty() {
        return;
    }
    out.push_str(&format!(
        "q {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm\n",
        matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f
    ));
    out.push_str(body);
    out.push_str("Q\n");
}

#[allow(clippy::too_many_arguments)]
fn draw_page(
    w: &mut Writer,
    data: &[u8],
    stored: &[&Page],
    meta: &Metafile,
    place: Place,
    tag: &str,
    out: &mut String,
    xobjects: &mut String,
    font: Option<&ttf::Font>,
    used: &mut BTreeMap<u16, char>,
    glyph_widths: &mut BTreeMap<u16, u16>,
    remapper: &mut Option<subsetter::GlyphRemapper>,
) -> Drawn {
    draw_page_with_viewbox(
        w,
        data,
        stored,
        meta,
        place,
        tag,
        out,
        xobjects,
        font,
        used,
        glyph_widths,
        remapper,
        None,
    )
}

fn should_draw_source_invert(raster_op: RasterOp, masked: bool) -> bool {
    raster_op != RasterOp::SourceInvert || masked
}

#[allow(clippy::too_many_arguments)]
fn draw_page_with_viewbox(
    w: &mut Writer,
    data: &[u8],
    stored: &[&Page],
    meta: &Metafile,
    place: Place,
    tag: &str,
    out: &mut String,
    xobjects: &mut String,
    font: Option<&ttf::Font>,
    used: &mut BTreeMap<u16, char>,
    glyph_widths: &mut BTreeMap<u16, u16>,
    remapper: &mut Option<subsetter::GlyphRemapper>,
    viewbox: Option<Rect>,
) -> Drawn {
    let mut drawn = Drawn {
        glyphs: 0,
        pictures: 0,
        painted: false,
    };
    let (ux, uy) = meta.units_per_point();
    if !(ux.is_finite() && uy.is_finite()) || ux <= 0.0 || uy <= 0.0 {
        return drawn;
    }
    // A page whose paper differs from the metafile's own frame is scaled to
    // fit rather than cropped.
    let (mw, mh) = meta.points();
    let fit = if mw > 1.0 && mh > 1.0 {
        (place.w / mw).min(place.h / mh)
    } else {
        1.0
    };
    let (origin_x, origin_y, scale_x, scale_y) = viewbox
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .map(|r| {
            (
                r.left,
                r.top,
                place.w * ux / r.width(),
                place.h * uy / r.height(),
            )
        })
        .unwrap_or((0.0, 0.0, fit, fit));
    let upright_vertical = meta.uses_upright_vertical_text();
    let px = |x: f32| place.x + (x - origin_x) / ux * scale_x;
    let py = |y: f32| (place.ph - place.top) - (y - origin_y) / uy * scale_y;

    // Stored pictures: pair each ordinal the page calls for with a picture
    // beside the sheet, then embed each picture once.
    let calls: Vec<(usize, (u32, u32))> = {
        let mut v: Vec<(usize, (u32, u32))> = meta
            .images
            .iter()
            .filter_map(|i| match i.source {
                Source::Stored { ordinal, px } => Some((ordinal, px)),
                Source::Inline(_) => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup_by_key(|c| c.0);
        v
    };
    let sizes: Vec<Option<(u32, u32)>> = stored.iter().map(|p| p.pixels).collect();
    let paired = rendering::pair_pictures(&calls, &sizes);
    let mut stored_names: BTreeMap<usize, String> = BTreeMap::new();
    let mut stored_indices: BTreeMap<usize, usize> = BTreeMap::new();
    let mut embedded: BTreeMap<usize, String> = BTreeMap::new();
    for (&(ordinal, _), pick) in calls.iter().zip(paired.iter()) {
        let Some(k) = *pick else { continue };
        stored_indices.insert(ordinal, k);
        if let Some(name) = embedded.get(&k) {
            stored_names.insert(ordinal, name.clone());
            continue;
        }
        let PageData::Jpeg { offset, len } = stored[k].data else {
            continue;
        };
        if offset + len > data.len() {
            continue;
        }
        let info = jpeg::info(&data[offset..offset + len]);
        let (iw, ih) = info
            .map(|i| (i.width, i.height))
            .or(stored[k].pixels)
            .unwrap_or((1, 1));
        let space = match info.map(|i| i.components).unwrap_or(3) {
            1 => "/DeviceGray",
            4 => "/DeviceCMYK",
            _ => "/DeviceRGB",
        };
        let id = w.add_stream(
            format!(
                "<< /Type /XObject /Subtype /Image /Width {iw} /Height {ih} /ColorSpace {space} \
                 /BitsPerComponent 8 /Filter /DCTDecode >>"
            ),
            &data[offset..offset + len],
        );
        let name = format!("{tag}Pc{k}");
        xobjects.push_str(&format!("/{name} {id} 0 R "));
        embedded.insert(k, name.clone());
        stored_names.insert(ordinal, name);
    }
    // Inline bitmaps, each embedded once however often it is placed.
    let mut raster_names: BTreeMap<usize, String> = BTreeMap::new();
    for img in &meta.images {
        let Source::Inline(i) = img.source else {
            continue;
        };
        if raster_names.contains_key(&i) {
            continue;
        }
        let Some(r) = meta.rasters.get(i) else {
            continue;
        };
        let id = raster_object(w, r);
        let name = format!("{tag}Ra{i}");
        xobjects.push_str(&format!("/{name} {id} 0 R "));
        raster_names.insert(i, name);
    }

    // A few metafiles implement a transparent picture with the GDI sequence
    // SRCINVERT -> SRCAND mask -> SRCINVERT.  It is one picture operation,
    // although it appears as three image records in the metafile.  Recognise
    // that operation from its raster operations and matching geometry, then
    // express it as a PDF soft mask.  This keeps the source order intact and
    // does not make text orientation decide the z-order.
    let mut masked_names: BTreeMap<usize, String> = BTreeMap::new();
    let mut masked_parts = vec![false; meta.images.len()];
    for middle in 1..meta.images.len().saturating_sub(1) {
        let before_index = middle - 1;
        let after_index = middle + 1;
        if masked_parts[before_index] || masked_parts[middle] || masked_parts[after_index] {
            continue;
        }
        let before = &meta.images[before_index];
        let mask = &meta.images[middle];
        let after = &meta.images[after_index];
        let (
            Source::Stored {
                ordinal: before_ordinal,
                ..
            },
            Source::Inline(mask_index),
            Source::Stored {
                ordinal: after_ordinal,
                ..
            },
        ) = (before.source, mask.source, after.source)
        else {
            continue;
        };
        if before.raster_op != RasterOp::SourceInvert
            || after.raster_op != RasterOp::SourceInvert
            || mask.raster_op != RasterOp::And
            || before.src != after.src
            || mask.order != before.order.saturating_add(1)
            || after.order != mask.order.saturating_add(1)
            || !same_image_placement(before, mask)
            || !same_image_placement(before, after)
            || stored_indices.get(&before_ordinal) != stored_indices.get(&after_ordinal)
        {
            continue;
        }
        let Some(raster) = meta.rasters.get(mask_index) else {
            continue;
        };
        let Some(&stored_index) = stored_indices.get(&before_ordinal) else {
            continue;
        };
        let Some(name) = masked_jpeg_name(
            w,
            data,
            stored.get(stored_index).copied(),
            raster,
            before.src,
            tag,
            before_index,
            xobjects,
        ) else {
            continue;
        };
        masked_names.insert(before_index, name);
        masked_parts[middle] = true;
        masked_parts[after_index] = true;
    }

    // Merge everything into draw order.
    enum Item<'a> {
        Fill(&'a Fill),
        Image(usize, &'a Image),
        Shape(&'a Shape),
        Text(&'a Text),
    }
    let mut items: Vec<(usize, Item)> = Vec::new();
    items.extend(meta.fills.iter().map(|f| (f.order, Item::Fill(f))));
    items.extend(
        meta.images
            .iter()
            .enumerate()
            .map(|(index, i)| (i.order, Item::Image(index, i))),
    );
    items.extend(meta.shapes.iter().map(|s| (s.order, Item::Shape(s))));
    items.extend(meta.text.iter().map(|t| (t.order, Item::Text(t))));
    items.sort_by_key(|(o, _)| *o);

    // Clipping is expressed with a saved state around a run of primitives
    // that share the same clip, so a gradient of two hundred slivers inside
    // one outline emits the outline once.
    let mut clip_open: Option<(Option<Rect>, Option<usize>)> = None;
    let mut fill_colour: Option<(u8, u8, u8)> = None;
    let set_clip = |out: &mut String,
                    want: (Option<Rect>, Option<usize>),
                    clip_open: &mut Option<(Option<Rect>, Option<usize>)>,
                    fill_colour: &mut Option<(u8, u8, u8)>| {
        if *clip_open == Some(want) {
            return;
        }
        if clip_open.is_some() {
            out.push_str("Q\n");
            *fill_colour = None;
        }
        *clip_open = None;
        if want == (None, None) {
            return;
        }
        out.push_str("q\n");
        if let Some(r) = want.0 {
            out.push_str(&format!(
                "{:.2} {:.2} {:.2} {:.2} re W n\n",
                px(r.left),
                py(r.bottom),
                px(r.right) - px(r.left),
                py(r.top) - py(r.bottom)
            ));
        }
        if let Some(p) = want.1.and_then(|i| meta.paths.get(i)) {
            path_ops(p, px, py, out);
            out.push_str(if p.even_odd { "W* n\n" } else { "W n\n" });
        }
        *clip_open = Some(want);
    };

    for (_, item) in items {
        match item {
            Item::Fill(f) => {
                set_clip(out, (f.clip, f.clip_path), &mut clip_open, &mut fill_colour);
                let (x, y) = (px(f.left), py(f.bottom));
                let (fw, fh) = (px(f.right) - px(f.left), py(f.top) - py(f.bottom));
                if !(x.is_finite() && y.is_finite() && fw > 0.0 && fh > 0.0) {
                    continue;
                }
                drawn.painted = true;
                if fill_colour != Some(f.rgb) {
                    fill_colour = Some(f.rgb);
                    out.push_str(&format!("{} rg\n", colour(f.rgb)));
                }
                out.push_str(&format!("{x:.2} {y:.2} {fw:.2} {fh:.2} re f\n"));
            }
            Item::Image(index, img) => {
                if masked_parts[index] {
                    continue;
                }
                if !should_draw_source_invert(img.raster_op, masked_names.contains_key(&index)) {
                    continue;
                }
                let name = masked_names.get(&index).or_else(|| match img.source {
                    Source::Stored { ordinal, .. } => stored_names.get(&ordinal),
                    Source::Inline(i) => raster_names.get(&i),
                });
                let Some(name) = name else { continue };
                set_clip(
                    out,
                    (img.clip, img.clip_path),
                    &mut clip_open,
                    &mut fill_colour,
                );
                let dw = px(img.right) - px(img.left);
                let dh = py(img.top) - py(img.bottom);
                let (dx, dy) = (px(img.left), py(img.bottom));
                if !(dw.is_finite() && dh.is_finite() && dx.is_finite() && dy.is_finite())
                    || dw <= 0.0
                    || dh <= 0.0
                {
                    continue;
                }
                drawn.painted = true;
                let stencil = match img.source {
                    Source::Inline(i) => meta.rasters.get(i).and_then(|r| r.stencil),
                    Source::Stored { .. } => None,
                };
                if let Some(rgb) = stencil {
                    if fill_colour != Some(rgb) {
                        fill_colour = Some(rgb);
                        out.push_str(&format!("{} rg\n", colour(rgb)));
                    }
                }
                out.push_str(&format!(
                    "q {dw:.2} 0 0 {dh:.2} {dx:.2} {dy:.2} cm /{name} Do Q\n"
                ));
                drawn.pictures += 1;
            }
            Item::Shape(s) => {
                set_clip(out, (s.clip, None), &mut clip_open, &mut fill_colour);
                if s.path.is_empty() {
                    continue;
                }
                drawn.painted = true;
                if let Some(rgb) = s.fill {
                    if fill_colour != Some(rgb) {
                        fill_colour = Some(rgb);
                        out.push_str(&format!("{} rg\n", colour(rgb)));
                    }
                }
                if let Some((rgb, width)) = s.stroke {
                    out.push_str(&format!(
                        "{} RG {:.2} w\n",
                        colour(rgb),
                        (width / uy * scale_y).max(0.2)
                    ));
                }
                path_ops(&s.path, px, py, out);
                out.push_str(
                    match (s.fill.is_some(), s.stroke.is_some(), s.path.even_odd) {
                        (true, true, true) => "B*\n",
                        (true, true, false) => "B\n",
                        (true, false, true) => "f*\n",
                        (true, false, false) => "f\n",
                        (false, true, _) => "S\n",
                        (false, false, _) => "n\n",
                    },
                );
            }
            Item::Text(t) => {
                set_clip(out, (None, None), &mut clip_open, &mut fill_colour);
                let glyphs = draw_text(
                    t,
                    px,
                    py,
                    uy,
                    scale_y,
                    upright_vertical,
                    out,
                    font,
                    used,
                    glyph_widths,
                    remapper,
                    &mut fill_colour,
                );
                drawn.glyphs += glyphs;
                drawn.painted |= glyphs > 0;
            }
        }
    }
    if clip_open.is_some() {
        out.push_str("Q\n");
    }
    out.push_str("0 0 0 rg\n");
    drawn
}

/// The result of drawing one logical displayed page.
struct DisplayRender {
    content: String,
    xobjects: String,
    pw: f32,
    ph: f32,
    rotation: u16,
    drawn: Drawn,
    recovered: bool,
}

/// Draw one logical displayed page, including the page-table bodies placed on
/// it by a DocuMerge properties record.
#[allow(clippy::too_many_arguments)]
fn draw_display_page<D: PageDecoder + ?Sized>(
    w: &mut Writer,
    data: &[u8],
    doc: &Document,
    display: &DisplayPage,
    decoder: &D,
    forced_paper: Option<(f32, f32)>,
    decode: bool,
    font: Option<&ttf::Font>,
    used: &mut BTreeMap<u16, char>,
    glyph_widths: &mut BTreeMap<u16, u16>,
    remapper: &mut Option<subsetter::GlyphRemapper>,
) -> DisplayRender {
    let paper = display.paper.unwrap_or((21000, 29700));
    let natural = pdf_paper_points(Some(paper)).unwrap_or(A4);
    let (pw, ph) = forced_paper.unwrap_or(natural);
    let mut content = String::new();
    let mut xobjects = String::new();
    let mut total = Drawn {
        glyphs: 0,
        pictures: 0,
        painted: false,
    };
    let mut recovered = false;
    // A stored-picture overlay already composes the display member(s). Do not
    // also place the member JPEG as a full-sheet opaque background: that
    // produces black or white bands outside the overlay's actual bounds.
    let overlay_uses_stored = decode
        && display.overlays.iter().any(|overlay| {
            decoder
                .decode_overlay(overlay, paper)
                .map(|meta| {
                    meta.images
                        .iter()
                        .any(|image| matches!(image.source, Source::Stored { .. }))
                })
                .unwrap_or(false)
        });

    for (n, member) in display.members.iter().enumerate() {
        let Some(page) = doc.pages.get(member.page_index) else {
            continue;
        };
        if recovery::preview_replaced_by_text_overlay(page, &display.overlays, paper, decoder) {
            continue;
        }
        let place = member
            .area
            .map(|area| area_place(area, ph))
            .unwrap_or_else(|| Place::sheet(pw, ph));
        match page.data {
            PageData::Jpeg { .. } => {
                if overlay_uses_stored && member.area.is_none() {
                    continue;
                }
                if draw_jpeg_at(
                    w,
                    data,
                    page,
                    place,
                    display.rotation,
                    &format!("D{n}"),
                    &mut content,
                    &mut xobjects,
                ) {
                    recovered = true;
                }
            }
            PageData::Encoded { .. } | PageData::Preview { .. } | PageData::Bare { .. }
                if decode =>
            {
                if let Some(meta) = recovery::decode_page_for_document(data, page, doc, decoder) {
                    if recovery::page_body_replaced_by_vector_overlay(
                        &meta,
                        &display.overlays,
                        paper,
                        decoder,
                    ) {
                        continue;
                    }
                    let stored = doc.pictures_on(page.index).collect::<Vec<_>>();
                    let mut member_content = String::new();
                    let mut member_xobjects = String::new();
                    let viewbox = member_viewbox(member.area, &meta);
                    let turn = component_rotation(&meta, (place.w, place.h), display.rotation);
                    let (draw_place, matrix) = if turn == 0 {
                        (place, None)
                    } else {
                        oriented_place(place, &meta, turn)
                    };
                    let drawn = draw_page_with_viewbox(
                        w,
                        data,
                        &stored,
                        &meta,
                        draw_place,
                        &format!("D{n}"),
                        &mut member_content,
                        &mut member_xobjects,
                        font,
                        used,
                        glyph_widths,
                        remapper,
                        viewbox,
                    );
                    if let Some(matrix) = matrix {
                        append_matrix(&mut content, matrix, &member_content);
                    } else {
                        content.push_str(&member_content);
                    }
                    xobjects.push_str(&member_xobjects);
                    recovered |= drawn.painted;
                    total.glyphs += drawn.glyphs;
                    total.pictures += drawn.pictures;
                    total.painted |= drawn.painted;
                }
            }
            _ => {}
        }
    }

    let anchor = display
        .members
        .first()
        .and_then(|member| doc.pages.get(member.page_index));
    let drawn = draw_overlay_list(
        w,
        data,
        doc,
        anchor,
        paper,
        &display.overlays,
        decoder,
        pw,
        ph,
        &mut content,
        &mut xobjects,
        font,
        used,
        glyph_widths,
        remapper,
        display.rotation,
    );
    recovered |= drawn.painted;
    total.glyphs += drawn.glyphs;
    total.pictures += drawn.pictures;
    total.painted |= drawn.painted;

    if recovery::display_page_is_explicit_blank(display, decoder) {
        recovered = true;
    }

    // Some printer exports preserve explicit blank logical pages in the
    // properties stream while omitting a page-table body for them.  They are
    // still real pages and must survive `--skip-missing` as blank sheets.
    if display.members.is_empty() && display.overlays.is_empty() {
        recovered = true;
    }

    DisplayRender {
        content,
        xobjects,
        pw,
        ph,
        // 90度単位の回転は描画内容へ適用済みなので、MediaBoxの向きを
        // さらに回転させない。未知の角度だけ従来のPDF回転を残す。
        rotation: match display.rotation % 360 {
            0 | 90 | 180 | 270 => 0,
            other => other,
        },
        drawn: total,
        recovered,
    }
}

/// Some composed page bodies are authored in a large screen-device extent but
/// contain only a small vector stamp or label.  Their properties rectangle is
/// the actual displayed object, so use the occupied drawing box as the local
/// viewBox when the metafile clearly has that mismatch.
fn member_viewbox(area: Option<(u32, u32, u32, u32)>, meta: &Metafile) -> Option<Rect> {
    let (_, _, area_w, area_h) = area?;
    let frame_w = meta.frame_mm100.0.unsigned_abs() as f32;
    let frame_h = meta.frame_mm100.1.unsigned_abs() as f32;
    let (dw, dh) = (
        meta.device.0.unsigned_abs() as f32,
        meta.device.1.unsigned_abs() as f32,
    );
    let aspect_mismatch = frame_w > 0.0
        && frame_h > 0.0
        && dw > 0.0
        && dh > 0.0
        && ((frame_w / frame_h) / (dw / dh)).ln().abs() > 0.02;
    // A full-paper member uses the metafile's native coordinate system.  Its
    // artwork may occupy only a small part of the page (for example a footer),
    // but replacing the page viewBox with that artwork box would enlarge its
    // text and vector strokes to fill the whole paper.  Annotation stamp
    // members are the exception: their stored device canvas is often a
    // screen-sized 16:9 canvas inside a square/portrait properties frame.
    if frame_w <= 0.0
        || frame_h <= 0.0
        || (area_w as f32 >= frame_w * 0.9 && area_h as f32 >= frame_h * 0.9 && !aspect_mismatch)
    {
        return None;
    }
    let bounds = meta.content_bounds()?;
    if dw <= 0.0 || dh <= 0.0 || bounds.width() >= dw * 0.5 || bounds.height() >= dh * 0.5 {
        return None;
    }
    Some(bounds)
}

/// Convert a properties rectangle to a PDF placement.
fn area_place((x, y, w, h): (u32, u32, u32, u32), ph: f32) -> Place {
    const PT: f32 = 72.0 / 2540.0;
    Place {
        x: x as f32 * PT,
        top: y as f32 * PT,
        w: w as f32 * PT,
        h: h as f32 * PT,
        ph,
    }
}

/// Embed a JPEG page body into an arbitrary displayed-page rectangle.
#[allow(clippy::too_many_arguments)]
fn draw_jpeg_at(
    w: &mut Writer,
    data: &[u8],
    page: &Page,
    place: Place,
    rotation: u16,
    tag: &str,
    out: &mut String,
    xobjects: &mut String,
) -> bool {
    let PageData::Jpeg { offset, len } = page.data else {
        return false;
    };
    let Some(stream) = data.get(offset..offset.saturating_add(len)) else {
        return false;
    };
    let info = jpeg::info(stream);
    let (width, height) = info
        .map(|i| (i.width, i.height))
        .or(page.pixels)
        .unwrap_or((1, 1));
    let space = match info.map(|i| i.components).unwrap_or(3) {
        1 => "/DeviceGray",
        4 => "/DeviceCMYK",
        _ => "/DeviceRGB",
    };
    let id = w.add_stream(
        format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode >>"
        ),
        stream,
    );
    let name = format!("{tag}Im");
    xobjects.push_str(&format!("/{name} {id} 0 R "));
    let (draw_place, matrix) = oriented_image_place(
        place,
        page.paper_points().unwrap_or((width as f32, height as f32)),
        rotation,
    );
    let bottom = (draw_place.ph - draw_place.top - draw_place.h).max(0.0);
    let body = format!(
        "q {:.2} 0 0 {:.2} {:.2} {:.2} cm /{name} Do Q\n",
        draw_place.w, draw_place.h, draw_place.x, bottom
    );
    if let Some(matrix) = matrix {
        append_matrix(out, matrix, &body);
    } else {
        out.push_str(&body);
    }
    true
}

/// Draw a supplied overlay list over a page.
#[allow(clippy::too_many_arguments)]
fn draw_overlay_list<D: PageDecoder + ?Sized>(
    w: &mut Writer,
    data: &[u8],
    doc: &Document,
    p: Option<&Page>,
    paper: (u32, u32),
    overlays: &[Overlay],
    decoder: &D,
    pw: f32,
    ph: f32,
    out: &mut String,
    xobjects: &mut String,
    font: Option<&ttf::Font>,
    used: &mut BTreeMap<u16, char>,
    glyph_widths: &mut BTreeMap<u16, u16>,
    remapper: &mut Option<subsetter::GlyphRemapper>,
    rotation: u16,
) -> Drawn {
    const PT: f32 = 72.0 / 2540.0;
    let mut total = Drawn {
        glyphs: 0,
        pictures: 0,
        painted: false,
    };
    // A page overlay names every picture of the page in storage order, the
    // sheet's own included when the sheet is itself a picture.
    let mut group: Vec<&Page> = p
        .map(|page| doc.pictures_on(page.index).collect())
        .unwrap_or_default();
    if let Some(page) = p.filter(|page| page.is_recoverable()) {
        group.push(page);
        group.sort_by_key(|q| q.index);
    }
    for (n, overlay) in overlays.iter().enumerate() {
        let Some(meta) = decoder.decode_overlay(overlay, paper) else {
            continue;
        };
        let mut place = match overlay.area {
            None => Place::sheet(pw, ph),
            Some((x, y, aw, ah)) if covers_paper((x, y, aw, ah), paper) => Place::sheet(pw, ph),
            Some((x, y, aw, ah)) => Place {
                x: x as f32 * PT,
                top: y as f32 * PT,
                w: aw as f32 * PT,
                h: ah as f32 * PT,
                ph,
            },
        };
        // Text-only WMFs carry point-sized text in their own coordinate
        // system. Their properties rectangle is the anchor, not a scale box;
        // using it as the WMF frame shrinks a 12-point label to a few points.
        // Keep the anchor and use one PDF point per WMF device unit.
        if is_point_sized_text(overlay, &meta) {
            place = fit_text_place(place, &meta);
        }
        // A properties rectangle describes where the overlay lands, not
        // whether its metafile references the sheet's stored pictures.  The
        // the source record has a full-page rectangle and still calls both the
        // background picture and the sheet picture by stored ordinal.
        let uses_stored = meta
            .images
            .iter()
            .any(|image| matches!(image.source, Source::Stored { .. }));
        let stored: &[&Page] = if overlay.area.is_none() || uses_stored {
            &group
        } else {
            &[]
        };
        let mut layer = String::new();
        let mut layer_xobjects = String::new();
        let component_rotation = component_rotation(&meta, (place.w, place.h), rotation);
        let (draw_place, matrix) = if component_rotation == 0 {
            (place, None)
        } else {
            oriented_place(place, &meta, component_rotation)
        };
        let d = draw_page_with_viewbox(
            w,
            data,
            stored,
            &meta,
            draw_place,
            &format!("O{n}"),
            &mut layer,
            &mut layer_xobjects,
            font,
            used,
            glyph_widths,
            remapper,
            None,
        );
        if let Some(matrix) = matrix {
            append_matrix(out, matrix, &layer);
        } else {
            out.push_str(&layer);
        }
        xobjects.push_str(&layer_xobjects);
        total.glyphs += d.glyphs;
        total.pictures += d.pictures;
        total.painted |= d.painted;
    }
    total
}

/// Some annotation records use their properties rectangle only as an anchor.
/// A text-only metafile is safe to identify this way; records containing
/// artwork must continue to scale to their declared rectangle.
fn is_point_sized_text(overlay: &Overlay, meta: &Metafile) -> bool {
    overlay.area.is_some()
        && !covers_frame(overlay.area, meta.frame_mm100)
        && !meta.text.is_empty()
        && meta.images.is_empty()
        && meta.rasters.is_empty()
        && meta.fills.is_empty()
        && meta.shapes.is_empty()
}

/// A full-sheet text overlay is already expressed in page coordinates.  It is
/// not a point-sized annotation: fitting its occupied text box would enlarge
/// the layer past the page and clip footer text outside the MediaBox.
fn covers_frame(area: Option<(u32, u32, u32, u32)>, frame: (i32, i32)) -> bool {
    let Some((x, y, w, h)) = area else {
        return false;
    };
    let fw = frame.0.unsigned_abs() as f32;
    let fh = frame.1.unsigned_abs() as f32;
    fw > 0.0
        && fh > 0.0
        && (x as f32) <= fw * 0.05
        && (y as f32) <= fh * 0.05
        && (w as f32) >= fw * 0.9
        && (h as f32) >= fh * 0.9
}

/// Apply a quarter-turn only when a decoded member's own frame is not already
/// in the displayed orientation.  Some writers store the frame after the
/// turn, while their properties still retain the original rotation flag.
fn component_rotation(meta: &Metafile, target: (f32, f32), rotation: u16) -> u16 {
    let rotation = rotation % 360;
    if rotation % 180 != 90 {
        return 0;
    }
    let (mw, mh) = meta.points();
    if mw <= 0.0 || mh <= 0.0 {
        return 0;
    }
    ((mw > mh) != (target.0 > target.1))
        .then_some(rotation)
        .unwrap_or(0)
}

fn covers_paper(area: (u32, u32, u32, u32), paper: (u32, u32)) -> bool {
    let (x, y, w, h) = area;
    let (pw, ph) = paper;
    x <= pw / 20 && y <= ph / 20 && w >= pw.saturating_mul(9) / 10 && h >= ph.saturating_mul(9) / 10
}

/// Fit text-only annotation contents to their properties rectangle.  Some
/// exporters give the drawing a square frame even though the actual text is a
/// single line; fitting the frame would either make that line unreadably small
/// or, if the frame is treated as device-sized, absurdly large.
fn fit_text_place(mut place: Place, meta: &Metafile) -> Place {
    let (ux, uy) = meta.units_per_point();
    let Some((right, top, bottom)) = text_bounds(meta) else {
        return place;
    };
    let text_w = right / ux;
    let text_h = (bottom - top) / uy;
    if !(text_w > 0.0 && text_h > 0.0 && place.w > 0.0 && place.h > 0.0) {
        return place;
    }
    let fit = (place.w / text_w).min(place.h / text_h);
    let (mw, mh) = meta.points();
    if fit.is_finite() && fit > 0.0 && mw > 0.0 && mh > 0.0 {
        place.w = mw * fit;
        place.h = mh * fit;
    }
    place
}

/// Approximate the occupied bounds of the text runs in device units.
fn text_bounds(meta: &Metafile) -> Option<(f32, f32, f32)> {
    let mut right = 0.0f32;
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for run in &meta.text {
        for (i, ch) in run.chars.iter().enumerate() {
            let x = run.xs.get(i).copied().unwrap_or(0.0);
            let width = run.size * if (*ch as u32) < 0x100 { 0.55 } else { 1.0 };
            if x.is_finite() && width.is_finite() && run.size.is_finite() && run.y.is_finite() {
                right = right.max(x + width);
                top = top.min(run.y - run.size);
                bottom = bottom.max(run.y);
            }
        }
    }
    (right > 0.0 && top.is_finite() && bottom.is_finite()).then_some((right, top, bottom))
}

fn colour((r, g, b): (u8, u8, u8)) -> String {
    format!(
        "{:.3} {:.3} {:.3}",
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0
    )
}

fn same_image_placement(a: &Image, b: &Image) -> bool {
    let close = |left: f32, right: f32| (left - right).abs() <= 0.01;
    close(a.left, b.left)
        && close(a.top, b.top)
        && close(a.right, b.right)
        && close(a.bottom, b.bottom)
        && a.clip == b.clip
        && a.clip_path == b.clip_path
}

/// Embed one JPEG with a grayscale soft mask derived from a one-bit bitmap.
///
/// The metafile mask is often at device resolution while the JPEG is half that
/// size. Sampling in the mask's coordinate system lets the PDF image and its
/// mask use the same dimensions, as required by PDF readers.
#[allow(clippy::too_many_arguments)]
fn masked_jpeg_name(
    w: &mut Writer,
    data: &[u8],
    page: Option<&Page>,
    mask: &Raster,
    source_size: (u32, u32),
    tag: &str,
    image_index: usize,
    xobjects: &mut String,
) -> Option<String> {
    let page = page?;
    let PageData::Jpeg { offset, len } = page.data else {
        return None;
    };
    let stream = data.get(offset..offset.checked_add(len)?)?;
    let info = jpeg::info(stream);
    let (width, height) = info
        .map(|i| (i.width, i.height))
        .or_else(|| (source_size.0 > 0 && source_size.1 > 0).then_some(source_size))?;
    let alpha = mask_alpha(mask, width, height)?;
    let alpha_body = deflate::zlib(&alpha);
    let mask_id = w.add_stream(
        format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode >>"
        ),
        &alpha_body,
    );
    let space = match info.map(|i| i.components).unwrap_or(3) {
        1 => "/DeviceGray",
        4 => "/DeviceCMYK",
        _ => "/DeviceRGB",
    };
    let image_id = w.add_stream(
        format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode \
             /SMask {mask_id} 0 R >>"
        ),
        stream,
    );
    let name = format!("{tag}Pm{image_index}");
    xobjects.push_str(&format!("/{name} {image_id} 0 R "));
    Some(name)
}

/// Convert the source-and mask's foreground (zero) bits into white alpha.
fn mask_alpha(mask: &Raster, width: u32, height: u32) -> Option<Vec<u8>> {
    if mask.bits != 1 || mask.width == 0 || mask.height == 0 || width == 0 || height == 0 {
        return None;
    }
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let pixels = width.checked_mul(height)?;
    let stride = mask.stride();
    if mask.rows.len() < stride.checked_mul(mask.height as usize)? {
        return None;
    }
    let mut alpha = vec![0u8; pixels];
    for y in 0..height {
        let source_y = (((y as u64 * 2 + 1) * u64::from(mask.height)) / (2 * height as u64))
            .min(u64::from(mask.height - 1)) as usize;
        let row = &mask.rows[source_y * stride..(source_y + 1) * stride];
        for x in 0..width {
            let source_x = (((x as u64 * 2 + 1) * u64::from(mask.width)) / (2 * width as u64))
                .min(u64::from(mask.width - 1)) as usize;
            let bit = (row[source_x / 8] >> (7 - source_x % 8)) & 1;
            alpha[y * width + x] = if bit == 0 { 255 } else { 0 };
        }
    }
    Some(alpha)
}

/// Path construction operators for a path, without the painting operator.
fn path_ops(
    p: &rendering::Path,
    px: impl Fn(f32) -> f32,
    py: impl Fn(f32) -> f32,
    out: &mut String,
) {
    for f in &p.figures {
        out.push_str(&format!("{:.2} {:.2} m\n", px(f.start.0), py(f.start.1)));
        for s in &f.segments {
            match s {
                Segment::Line((x, y)) => out.push_str(&format!("{:.2} {:.2} l\n", px(*x), py(*y))),
                Segment::Curve(a, b, c) => out.push_str(&format!(
                    "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
                    px(a.0),
                    py(a.1),
                    px(b.0),
                    py(b.1),
                    px(c.0),
                    py(c.1)
                )),
            }
        }
        if f.closed {
            out.push_str("h\n");
        }
    }
}

/// An image object for a bitmap carried inside the page.
fn raster_object(w: &mut Writer, r: &Raster) -> usize {
    let body = crate::infrastructure::deflate::zlib(&r.rows);
    if r.stencil.is_some() {
        // A stencil paints the fill colour through its zero bits.  In a PDF
        // image mask the Decode array [0 1] makes raw zero samples the
        // painted foreground; [1 0] would paint the white/background bits and
        // turn a sparse line drawing into an opaque rectangle.
        return w.add_stream(
            format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ImageMask true \
                 /BitsPerComponent 1 /Decode [0 1] /Filter /FlateDecode >>",
                r.width, r.height
            ),
            &body,
        );
    }
    let space = if r.bits == 24 {
        "/DeviceRGB".to_string()
    } else if r.is_bilevel() {
        // Black and white goes straight to a one-bit grey image; a palette
        // starting with white needs the decode array turned round.
        if r.palette[0] == (255, 255, 255) {
            "/DeviceGray /Decode [1 0]".to_string()
        } else {
            "/DeviceGray".to_string()
        }
    } else {
        let mut hex = String::with_capacity(r.palette.len() * 6);
        for (cr, cg, cb) in &r.palette {
            hex.push_str(&format!("{cr:02X}{cg:02X}{cb:02X}"));
        }
        format!(
            "[/Indexed /DeviceRGB {} <{hex}>]",
            r.palette.len().saturating_sub(1)
        )
    };
    let bpc = if r.bits == 24 { 8 } else { r.bits };
    w.add_stream(
        format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {space} \
             /BitsPerComponent {bpc} /Filter /FlateDecode >>",
            r.width, r.height
        ),
        &body,
    )
}

/// One run of text, each character positioned from the spacing the
/// metafile recorded, so the line breaks where the original broke it and
/// no font metric has to be guessed at.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    t: &Text,
    px: impl Fn(f32) -> f32,
    py: impl Fn(f32) -> f32,
    uy: f32,
    fit: f32,
    upright_vertical: bool,
    out: &mut String,
    font: Option<&ttf::Font>,
    used: &mut BTreeMap<u16, char>,
    glyph_widths: &mut BTreeMap<u16, u16>,
    remapper: &mut Option<subsetter::GlyphRemapper>,
    fill_colour: &mut Option<(u8, u8, u8)>,
) -> usize {
    let size = (t.size / uy) * fit;
    if !(size.is_finite() && size > 0.01) {
        return 0;
    }
    if *fill_colour != Some(t.rgb) {
        *fill_colour = Some(t.rgb);
        out.push_str(&format!("{} rg\n", colour(t.rgb)));
    }
    // エスケープメントは反時計回りの10分の1度単位。縦書きでは各字の
    // 縦位置が既に記録されているため、直角回転は文字の回転ではなく
    // 書字方向を表す。
    let ang = if upright_vertical {
        0.0
    } else {
        t.escapement as f32 / 10.0 * std::f32::consts::PI / 180.0
    };
    let (c, s) = (ang.cos(), ang.sin());
    let text_y = if upright_vertical {
        // 縦書きレコードは上端の基準点を使う一方、中間モデルはベースラインを
        // 保持する。方向指定を外す前に、GDIの通常の補正分を加える。
        t.y + t.size * 0.8
    } else {
        t.y
    };
    let base_y = py(text_y);
    let mut placed = 0usize;
    let mut last_x: Option<f32> = None;
    for (i, ch) in t.chars.iter().enumerate() {
        if *ch == '\u{0}' {
            continue;
        }
        let x = px(t.xs.get(i).copied().unwrap_or(0.0));
        if !(x.is_finite() && base_y.is_finite()) {
            continue;
        }
        // Keep the font choice made by the source for the whole run.  An
        // ASCII hyphen in a Japanese-font run is not equivalent to a hyphen in
        // a Verdana run, so character-class routing is intentionally avoided.
        let ascii = ch.is_ascii_graphic() || *ch == ' ';
        let embedded_latin = t.font_kind == FontKind::Latin && ascii;
        let text_font = match (t.font_kind, ascii) {
            (FontKind::Latin, true) => "/FA",
            (FontKind::JapaneseProportional, true) => "/FP",
            (FontKind::JapaneseProportionalGothic, true) => "/FG",
            (FontKind::Japanese, true) => "/FM",
            (_, false) => "/FJ",
        };
        let mut hex = String::from("<");
        match (font, embedded_latin) {
            // With a font of our own, the operand is a remapped glyph index.
            (Some(f), true) => match f.glyph(*ch) {
                Some(old_gid) => {
                    let gid = remapper
                        .as_mut()
                        .map_or(old_gid, |mapper| mapper.remap(old_gid));
                    used.insert(gid, *ch);
                    glyph_widths.insert(gid, f.width(old_gid));
                    hex.push_str(&format!("{gid:04X}"));
                }
                None => continue,
            },
            // The Japanese face maps UTF-16 directly for non-Latin runs.
            (_, false) => {
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    hex.push_str(&format!("{unit:04X}"));
                }
            }
            (None, true) => {
                hex.push_str(&format!("{:02X}", *ch as u32));
            }
        }
        hex.push('>');
        out.push_str(&format!("BT {text_font} {size:.2} Tf\n"));
        if t.bold {
            // Bold without a bold face: stroke the outline a little.
            out.push_str(&format!("2 Tr {} RG {:.2} w\n", colour(t.rgb), size * 0.03));
        }
        out.push_str(&format!(
            "{c:.5} {s:.5} {:.5} {c:.5} {x:.2} {base_y:.2} Tm {hex} Tj\n",
            -s
        ));
        if t.bold {
            out.push_str("0 Tr\n");
        }
        out.push_str("ET\n");
        last_x = Some(x + size * if (*ch as u32) < 0x100 { 0.55 } else { 1.0 });
        placed += 1;
    }
    if t.underline && t.escapement == 0 {
        if let (Some(first), Some(last)) = (t.xs.first(), last_x) {
            let y = base_y - size * 0.12;
            out.push_str(&format!(
                "{} RG {:.2} w {:.2} {y:.2} m {last:.2} {y:.2} l S\n",
                colour(t.rgb),
                (size * 0.06).max(0.2),
                px(*first)
            ));
        }
    }
    placed
}

/// A string as a PDF text-showing operand, quoted or hex as the font needs.
fn show(s: &str, lang: Lang) -> String {
    match lang {
        Lang::English => format!("({})", escape(s)),
        Lang::Japanese => {
            let mut hex = String::from("<");
            for unit in s.encode_utf16() {
                hex.push_str(&format!("{unit:04X}"));
            }
            hex.push('>');
            hex
        }
    }
}

/// Draw a placeholder that stays inside its page, however small the page is.
///
/// A document can carry pages a couple of centimetres tall. Text placed at a
/// fixed offset from the top of an A4 sheet would sit below such a page and
/// never be seen, so the note is shortened and shrunk until it fits, and a
/// border is drawn either way so that even a page too small for legible text
/// still reads as a deliberate blank.
fn placeholder_content(pw: f32, ph: f32, page_no: usize, lang: Lang) -> String {
    let margin = (pw.min(ph) * 0.08).clamp(2.0, 36.0);
    let mut out = format!(
        "0.6 w 0.55 0.58 0.62 RG {:.2} {:.2} {:.2} {:.2} re S\n",
        margin,
        margin,
        (pw - 2.0 * margin).max(0.1),
        (ph - 2.0 * margin).max(0.1)
    );

    let usable = pw - 2.0 * margin;
    let mut baseline = ph - margin;

    // Prefer the fullest wording that still sets at a readable size; only fall
    // back to small type when nothing else fits.
    let place = |lang: Lang, font: &str, out: &mut String, baseline: &mut f32| -> bool {
        for floor in [7.0f32, 3.5] {
            for note in notes(page_no, lang) {
                let size = (usable / width_em(&note)).min(10.0);
                if size < floor {
                    continue;
                }
                let y = if *baseline - margin >= size * 1.4 {
                    *baseline - size * 1.4
                } else {
                    ((ph - size) / 2.0).max(margin * 0.5)
                };
                if y < 0.5 || y + size > ph {
                    continue;
                }
                out.push_str(&format!(
                    "BT {font} {:.2} Tf {:.2} {:.2} Td {} Tj ET\n",
                    size,
                    margin + size * 0.5,
                    y,
                    show(&note, lang)
                ));
                *baseline = y;
                return true;
            }
        }
        false
    };

    if lang == Lang::Japanese {
        place(Lang::Japanese, "/FJ", &mut out, &mut baseline);
    }
    // Always a line every reader can render, whatever fonts it has.
    place(Lang::English, "/FA", &mut out, &mut baseline);
    out
}

/// A complete PDF string object, delimiters included.
///
/// Plain ASCII goes in as a literal string. Anything else becomes a UTF-16BE
/// hex string with the byte order mark readers look for, so a Japanese file
/// name shows up as itself in the title bar instead of a row of question marks.
fn text_string(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return format!("({})", escape(s));
    }
    let mut out = String::from("<FEFF");
    for unit in s.encode_utf16() {
        out.push_str(&format!("{unit:04X}"));
    }
    out.push('>');
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

/// Accumulates indirect objects and lays out the cross-reference table.
struct Writer {
    objects: Vec<Option<Vec<u8>>>,
}

impl Writer {
    fn new() -> Self {
        Writer {
            objects: Vec::new(),
        }
    }

    /// Claim an object number to be filled in later, for forward references.
    fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }

    fn set(&mut self, id: usize, body: String) {
        self.objects[id - 1] = Some(body.into_bytes());
    }

    fn add(&mut self, body: String) -> usize {
        self.objects.push(Some(body.into_bytes()));
        self.objects.len()
    }

    fn add_stream(&mut self, mut dict: String, payload: &[u8]) -> usize {
        // Splice the length into the dictionary, then append the stream body.
        let insert = dict.rfind(">>").unwrap_or(dict.len());
        dict.insert_str(insert, &format!(" /Length {} ", payload.len()));
        let mut body = dict.into_bytes();
        body.extend_from_slice(b"\nstream\n");
        body.extend_from_slice(payload);
        body.extend_from_slice(b"\nendstream");
        self.objects.push(Some(body));
        self.objects.len()
    }

    fn finish(self, root: usize, info: Option<usize>) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets = vec![0usize; self.objects.len()];
        for (i, obj) in self.objects.iter().enumerate() {
            offsets[i] = out.len();
            let body = obj.as_deref().unwrap_or(b"<< >>");
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", self.objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        let info_ref = info.map(|i| format!(" /Info {i} 0 R")).unwrap_or_default();
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {} 0 R{} >>\nstartxref\n{}\n%%EOF\n",
                self.objects.len() + 1,
                root,
                info_ref,
                xref_at
            )
            .as_bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::mask_alpha;
    use super::should_draw_source_invert;
    use super::{
        contained_size, fit_image_place, oriented_image_place, oriented_place, pdf_paper_points,
        Place,
    };
    use crate::domain::rendering::Raster;
    use crate::domain::rendering::{Metafile, RasterOp};

    #[test]
    fn source_and_mask_zero_bits_become_opaque_alpha() {
        let mask = Raster {
            width: 8,
            height: 1,
            bits: 1,
            palette: vec![(0, 0, 0), (255, 255, 255)],
            rows: vec![0b0101_0101],
            stencil: Some((0, 0, 0)),
        };
        assert_eq!(
            mask_alpha(&mask, 8, 1),
            Some(vec![255, 0, 255, 0, 255, 0, 255, 0])
        );
    }

    #[test]
    fn a_device_resolution_mask_is_resized_to_the_jpeg_size() {
        let mask = Raster {
            width: 4,
            height: 2,
            bits: 1,
            palette: vec![(0, 0, 0), (255, 255, 255)],
            rows: vec![0b0011_0000, 0b0011_0000],
            stencil: Some((0, 0, 0)),
        };
        assert_eq!(mask_alpha(&mask, 2, 1), Some(vec![255, 0]));
    }

    #[test]
    fn paper_boxes_match_the_print_grid_and_recover_near_a4_frames() {
        let a4 = pdf_paper_points(Some((21_000, 29_700))).expect("A4");
        assert!((a4.0 - 595.32).abs() < 0.01);
        assert!((a4.1 - 841.92).abs() < 0.01);

        let near_a4 = pdf_paper_points(Some((20_997, 29_692))).expect("near A4");
        assert!((near_a4.0 - 595.32).abs() < 0.01);
        assert!((near_a4.1 - 841.92).abs() < 0.01);

        let image_frame_a4 = pdf_paper_points(Some((20_928, 29_672))).expect("image-frame A4");
        assert!((image_frame_a4.0 - 595.32).abs() < 0.01);
        assert!((image_frame_a4.1 - 841.92).abs() < 0.01);

        let custom = pdf_paper_points(Some((10_000, 14_800))).expect("custom");
        assert!((custom.0 - 283.44).abs() < 0.01);
        assert!((custom.1 - 419.52).abs() < 0.01);
    }

    #[test]
    fn source_invert_is_only_drawn_as_part_of_a_recognised_mask() {
        assert!(!should_draw_source_invert(RasterOp::SourceInvert, false));
        assert!(should_draw_source_invert(RasterOp::SourceInvert, true));
        assert!(should_draw_source_invert(RasterOp::Copy, false));
    }

    #[test]
    fn rotated_display_content_uses_the_swapped_metafile_frame() {
        let meta = Metafile {
            device: (7016, 9921),
            frame_mm100: (29700, 42000),
            ..Default::default()
        };
        let target = Place::sheet(1190.52, 841.92);
        let (local, matrix) = oriented_place(target, &meta, 90);

        assert!((local.w - 841.89).abs() < 0.1);
        assert!((local.h - 1190.55).abs() < 0.1);
        let matrix = matrix.expect("quarter-turn matrix");
        assert_eq!(
            (matrix.a, matrix.b, matrix.c, matrix.d),
            (0.0, -1.0, 1.0, 0.0)
        );
        assert!((matrix.e - 0.0).abs() < 0.01);
        assert!((matrix.f - 841.92).abs() < 0.01);
    }

    #[test]
    fn a_jpeg_with_a_different_aspect_ratio_is_contained_in_its_paper() {
        let paper = (595.32, 841.92);
        let source = contained_size(paper, (700.0, 500.0));
        let placed = fit_image_place(Place::sheet(paper.0, paper.1), source, false);

        assert!((placed.w - 595.32).abs() < 0.01);
        assert!((placed.h - 425.23).abs() < 0.01);
        assert!((placed.w / placed.h - 1.4).abs() < 0.001);
        assert!((placed.top - 208.35).abs() < 0.02);
    }

    #[test]
    fn a_composed_jpeg_is_contained_in_its_destination_area() {
        let target = Place {
            x: 10.0,
            top: 20.0,
            w: 100.0,
            h: 200.0,
            ph: 300.0,
        };
        let (placed, matrix) = oriented_image_place(target, (200.0, 100.0), 0);

        assert!(matrix.is_none());
        assert!((placed.x - 10.0).abs() < 0.01);
        assert!((placed.top - 95.0).abs() < 0.01);
        assert!((placed.w - 100.0).abs() < 0.01);
        assert!((placed.h - 50.0).abs() < 0.01);
    }

    #[test]
    fn a_full_paper_member_keeps_its_native_text_scale() {
        let meta = Metafile {
            device: (4961, 7016),
            frame_mm100: (21000, 29700),
            fills: vec![crate::domain::rendering::Fill {
                left: 100.0,
                top: 100.0,
                right: 200.0,
                bottom: 200.0,
                rgb: (0, 0, 0),
                blend: crate::domain::rendering::BlendMode::Normal,
                order: 0,
                clip: None,
                clip_path: None,
            }],
            ..Default::default()
        };
        assert!(super::member_viewbox(Some((0, 0, 21000, 29700)), &meta).is_none());
        assert!(super::member_viewbox(Some((0, 0, 10000, 14000)), &meta).is_some());
        let mut stamp = meta.clone();
        stamp.device = (2560, 1440);
        stamp.frame_mm100 = (1909, 1909);
        assert!(super::member_viewbox(Some((0, 0, 1909, 1909)), &stamp).is_some());
    }
}
