//! A small PDF writer, just enough to wrap recovered page images.
//!
//! JPEG streams go in untouched as `DCTDecode` images, so a page that comes out
//! of this is the same bytes that went in. Nothing is re-encoded.

use std::collections::BTreeMap;

use crate::application::ports::{AttachmentScanner, PageDecoder};
use crate::application::recovery;
pub use crate::domain::output::Language as Lang;
use crate::domain::page::{Page, PageData};
use crate::domain::rendering::Metafile;
use crate::domain::Document;
use crate::infrastructure::{jpeg, ttf, LzhMetafileDecoder, MagicAttachmentScanner};

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
    /// A TrueType font to embed for the recovered page text.
    ///
    /// Without one the PDF names a standard Japanese face and relies on the
    /// reader having it, which is how Japanese PDFs have always been written
    /// but leaves nothing to render on a machine without those fonts. With one,
    /// the document carries its own text and reads the same everywhere.
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

/// A4 in points.
pub const A4: (f32, f32) = (595.28, 841.89);
/// US Letter in points.
pub const LETTER: (f32, f32) = (612.0, 792.0);

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
    let mut font_id: Option<(usize, Option<usize>)> = None;
    // One embedded font serves every page, but which glyphs it must carry is
    // only known once every page has been drawn, so its id is reserved now and
    // its body written at the end.
    let embedded = opts.font.as_ref().map(|_| w.reserve());
    let mut used_glyphs: BTreeMap<u16, char> = BTreeMap::new();
    let mut kids: Vec<usize> = Vec::new();
    // (object id, original page number) for each page that is only a marker.
    let mut gaps: Vec<(usize, usize)> = Vec::new();
    let mut report = Report::default();

    // One PDF page per sheet. The container's entry table also holds thumbnails
    // and the pictures a sheet is made of; putting those on sheets of their own
    // would turn a one page pamphlet into a four page document and make the
    // page numbering meaningless.
    let selected: Vec<&Page> = doc
        .pages
        .iter()
        .filter(|p| p.is_sheet() || (opts.include_previews && p.is_preview()))
        .collect();

    for (ordinal, p) in selected.iter().enumerate() {
        let page_no = ordinal + 1;
        let decoded = opts
            .decode
            .then(|| recovery::decode_page(data, p, decoder))
            .flatten();
        match &p.data {
            PageData::Jpeg { offset, len } => {
                let stream = &data[*offset..*offset + *len];
                let info = jpeg::info(stream);
                let (w_px, h_px) = info
                    .map(|i| (i.width, i.height))
                    .or(p.pixels)
                    .unwrap_or((1, 1));
                let natural = p
                    .paper_points()
                    .or_else(|| info.map(|i| i.points()))
                    .unwrap_or((w_px as f32, h_px as f32));
                let (pw, ph) = opts.paper.unwrap_or(natural);
                // Contain-fit, never enlarging: an image smaller than the sheet
                // keeps its own size rather than being blown up.
                let scale = (pw / natural.0).min(ph / natural.1).min(1.0);
                let (dw, dh) = (natural.0 * scale, natural.1 * scale);
                let dy = (ph - dh) / 2.0;
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
                // its own sitting on it: one real cover is an aerial photograph
                // with a panel of samples over it. Dropping them because the
                // sheet came out would be throwing away recovered content, so
                // the sheet takes the upper part of the page and they follow.
                let runs = doc.picture_runs(p.index);
                let mut xobjects = format!("/Im0 {img} 0 R ");
                let (dw, dh, dy) = if runs.is_empty() {
                    (dw, dh, dy)
                } else {
                    let k = (ph * 0.52) / dh.max(0.01);
                    let k = k.min(1.0);
                    (dw * k, dh * k, ph - margin_of(pw, ph) - dh * k)
                };
                let dx = (pw - dw) / 2.0;
                let mut content = format!("q {dw:.2} 0 0 {dh:.2} {dx:.2} {dy:.2} cm /Im0 Do Q\n");
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
                let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                let pid = w.add(format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] \
                     /Resources << /XObject << {xobjects} >> >> /Contents {cid} 0 R >>"
                ));
                kids.push(pid);
                report.embedded += 1;
            }
            // The sheet is in the container's own coding. Expand it: what comes
            // out is a metafile, and its text is real text with real positions,
            // so the page can be redrawn rather than apologised for.
            PageData::Encoded { .. } if decoded.is_some() => {
                let meta = decoded.as_ref().expect("decoded guard above");
                let (nw, nh) = meta.points();
                let (pw, ph) = opts
                    .paper
                    .or_else(|| p.paper_points())
                    .unwrap_or(if nw > 1.0 && nh > 1.0 { (nw, nh) } else { A4 });
                let mut content = String::new();
                // Pictures first: the metafile draws them under the text.
                let mut xobjects = String::new();
                let placed = place_pictures(
                    &mut w,
                    data,
                    doc,
                    p.index,
                    meta,
                    pw,
                    ph,
                    &mut content,
                    &mut xobjects,
                );
                let glyphs = draw_metafile(
                    meta,
                    pw,
                    ph,
                    &mut content,
                    opts.font.as_deref(),
                    &mut used_glyphs,
                );
                let fonts = match embedded {
                    Some(id) => format!("/FJ {id} 0 R"),
                    None => {
                        let (latin, cjk) = note_fonts(&mut w, &mut font_id, Lang::Japanese);
                        font_resources(latin, cjk)
                    }
                };

                let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                let pid = w.add(format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] \
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
                let (pw, ph) = opts.paper.or_else(|| p.paper_points()).unwrap_or(A4);
                let (latin, cjk) = note_fonts(&mut w, &mut font_id, opts.lang);
                let mut content = placeholder_content(pw, ph, page_no, opts.lang);

                // The sheet itself cannot be reproduced, but the pictures it is
                // made of are plain JPEG. Stack them on the sheet rather than
                // leaving it blank: a pamphlet page comes back as its artwork,
                // which is a great deal better than an empty rectangle.
                let mut xobjects = String::new();
                let runs = doc.picture_runs(p.index);
                let placed =
                    stack_pictures(&mut w, data, &runs, pw, ph, &mut content, &mut xobjects);
                report.pictures_placed += placed;

                let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
                let pid = w.add(format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] \
                     /Resources << /Font << {} >>{} >> /Contents {cid} 0 R >>",
                    font_resources(latin, cjk),
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

    if kids.is_empty() {
        let (latin, cjk) = note_fonts(&mut w, &mut font_id, opts.lang);
        let content = placeholder_content(A4.0, A4.1, 0, opts.lang);
        let cid = w.add_stream("<< >>".to_string(), content.as_bytes());
        let pid = w.add(format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 595.28 841.89] \
             /Resources << /Font << {} >> >> /Contents {cid} 0 R >>",
            font_resources(latin, cjk)
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
        } else {
            embed_font(&mut w, id, font, &used_glyphs);
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
        report.embedded,
        report.embedded + report.placeholders + report.skipped,
        report.placeholders,
        report.attachments
    ));
    let info_id = w.add(info);
    (w.finish(root, Some(info_id)), report)
}

/// The fonts a placeholder note may need: always Helvetica, plus a Japanese
/// face when one is asked for.
///
/// The Japanese face is a character collection rather than an embedded font,
/// which keeps the file small but leaves the glyphs to the reader. Readers
/// without the mapping installed draw nothing at all for them, so a Japanese
/// note is always accompanied by a plain line that every reader can show. A
/// placeholder that renders blank would be worse than a clumsy one.
fn note_fonts(
    w: &mut Writer,
    cache: &mut Option<(usize, Option<usize>)>,
    lang: Lang,
) -> (usize, Option<usize>) {
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
            let descendant = w.add(
                "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /KozMinPro-Regular-Acro \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 6 >> \
                 /DW 1000 >>"
                    .to_string(),
            );
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
                "<< /Type /Font /Subtype /Type0 /BaseFont /KozMinPro-Regular-Acro \
                 /Encoding /UniJIS-UCS2-H /DescendantFonts [{descendant} 0 R] \
                 /ToUnicode {to_unicode} 0 R >>"
            )))
        }
    };
    *cache = Some((latin, cjk));
    (latin, cjk)
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
fn font_resources(latin: usize, cjk: Option<usize>) -> String {
    match cjk {
        Some(j) => format!("/FA {latin} 0 R /FJ {j} 0 R"),
        None => format!("/FA {latin} 0 R"),
    }
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

/// Draw a sheet's pictures where the metafile says they go.
///
/// The metafile names a picture by the size of the stored image, and the
/// container stores a sheet's pictures in the order the metafile first calls
/// for them, so the two line up without guessing. A picture the metafile never
/// mentions is drawn nowhere rather than invented a place for.
#[allow(clippy::too_many_arguments)]
fn place_pictures(
    w: &mut Writer,
    data: &[u8],
    doc: &Document,
    sheet: usize,
    meta: &Metafile,
    pw: f32,
    ph: f32,
    content: &mut String,
    xobjects: &mut String,
) -> usize {
    let (ux, uy) = meta.units_per_point();
    if !(ux.is_finite() && uy.is_finite()) || ux <= 0.0 || uy <= 0.0 || meta.images.is_empty() {
        return 0;
    }
    let (mw, mh) = meta.points();
    let fit = if mw > 1.0 && mh > 1.0 {
        (pw / mw).min(ph / mh)
    } else {
        1.0
    };

    // Pair each size the metafile asks for with a stored picture, in order.
    let pictures: Vec<&Page> = doc.pictures_on(sheet).collect();
    let mut ids: Vec<(u32, u32, usize)> = Vec::new();
    let mut taken = vec![false; pictures.len()];
    for want in meta.image_sizes() {
        let pick = pictures
            .iter()
            .position(|p| {
                !taken[pictures
                    .iter()
                    .position(|q| std::ptr::eq(*q, *p))
                    .unwrap_or(0)]
                    && p.pixels == Some(want)
            })
            .or_else(|| pictures.iter().position(|p| p.pixels == Some(want)));
        if let Some(k) = pick {
            let PageData::Jpeg { offset, len } = pictures[k].data else {
                continue;
            };
            if offset + len > data.len() {
                continue;
            }
            taken[k] = true;
            let info = jpeg::info(&data[offset..offset + len]);
            let space = match info.map(|i| i.components).unwrap_or(3) {
                1 => "/DeviceGray",
                4 => "/DeviceCMYK",
                _ => "/DeviceRGB",
            };
            let id = w.add_stream(
                format!(
                    "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {space} \
                     /BitsPerComponent 8 /Filter /DCTDecode >>",
                    want.0, want.1
                ),
                &data[offset..offset + len],
            );
            let name = ids.len();
            xobjects.push_str(&format!("/Pc{name} {id} 0 R "));
            ids.push((want.0, want.1, name));
        }
    }

    let mut drawn = 0usize;
    for img in &meta.images {
        let Some(&(_, _, name)) = ids.iter().find(|(a, b, _)| (*a, *b) == img.src) else {
            continue;
        };
        let dw = img.width() / ux * fit;
        let dh = img.height() / uy * fit;
        let dx = img.left / ux * fit;
        // Metafile y grows downward; PDF y grows upward.
        let dy = ph - img.bottom / uy * fit;
        if !(dw.is_finite() && dh.is_finite() && dx.is_finite() && dy.is_finite())
            || dw <= 0.0
            || dh <= 0.0
        {
            continue;
        }
        content.push_str(&format!(
            "q {dw:.2} 0 0 {dh:.2} {dx:.2} {dy:.2} cm /Pc{name} Do Q\n"
        ));
        drawn += 1;
    }
    drawn
}

/// Write an embedded CID font and return its object id.
///
/// The font is keyed by glyph index, so the text operand carries glyph indices
/// and a ToUnicode map carries the characters back. That combination renders
/// and copies correctly in any reader, with nothing installed.
fn embed_font(w: &mut Writer, id: usize, font: &ttf::Font, used: &BTreeMap<u16, char>) {
    let file = w.add_stream(format!("<< /Length1 {} >>", font.data.len()), &font.data);
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
        if prev.map_or(true, |p| gid != p + 1) {
            flush(&mut widths, run_start, &mut run);
            run_start = Some(gid);
        }
        run.push(font.width(gid));
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

/// Draw a page's metafile text onto the sheet, returning how many characters
/// were placed.
///
/// Every character is positioned individually from the spacing the metafile
/// recorded, so the line breaks where the original broke it and no font metric
/// has to be guessed at. Metafile y grows downward and PDF y grows upward, so
/// each baseline is measured from the top of the sheet.
fn draw_metafile(
    meta: &Metafile,
    pw: f32,
    ph: f32,
    out: &mut String,
    font: Option<&ttf::Font>,
    used: &mut BTreeMap<u16, char>,
) -> usize {
    let (ux, uy) = meta.units_per_point();
    if !(ux.is_finite() && uy.is_finite()) || ux <= 0.0 || uy <= 0.0 {
        return 0;
    }
    // A page whose paper differs from the metafile's own frame is scaled to fit
    // rather than cropped.
    let (mw, mh) = meta.points();
    let fit = if mw > 1.0 && mh > 1.0 {
        (pw / mw).min(ph / mh)
    } else {
        1.0
    };
    let mut placed = 0usize;
    let mut colour: Option<(u8, u8, u8)> = None;
    // Rules and blocks of colour go down in the metafile's own order, before
    // the text that sits on them.
    let mut fill: Option<(u8, u8, u8)> = None;
    for f in &meta.fills {
        if f.clipped {
            continue;
        }
        let (x, y) = (f.left / ux * fit, ph - f.bottom / uy * fit);
        let (fw, fh) = ((f.right - f.left) / ux * fit, (f.bottom - f.top) / uy * fit);
        if !(x.is_finite() && y.is_finite() && fw.is_finite() && fh.is_finite())
            || fw <= 0.0
            || fh <= 0.0
        {
            continue;
        }
        if fill != Some(f.rgb) {
            fill = Some(f.rgb);
            let (r, g, b) = f.rgb;
            out.push_str(&format!(
                "{:.3} {:.3} {:.3} rg\n",
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0
            ));
        }
        out.push_str(&format!("{x:.2} {y:.2} {fw:.2} {fh:.2} re f\n"));
    }
    if fill.is_some() {
        out.push_str("0 0 0 rg\n");
    }
    for t in &meta.text {
        if colour != Some(t.rgb) {
            colour = Some(t.rgb);
            let (r, g, b) = t.rgb;
            out.push_str(&format!(
                "{:.3} {:.3} {:.3} rg\n",
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0
            ));
        }
        let size = (t.size / uy) * fit;
        if !(size.is_finite() && size > 0.01) {
            continue;
        }
        // Escapement is tenths of a degree, counter-clockwise.
        let ang = t.escapement as f32 / 10.0 * std::f32::consts::PI / 180.0;
        let (c, s) = (ang.cos(), ang.sin());
        let base_y = ph - (t.y / uy) * fit;
        out.push_str(&format!("BT /FJ {size:.2} Tf\n"));
        for (i, ch) in t.chars.iter().enumerate() {
            if *ch == '\u{0}' {
                continue;
            }
            let x = (t.xs.get(i).copied().unwrap_or(0.0) / ux) * fit;
            if !(x.is_finite() && base_y.is_finite()) {
                continue;
            }
            let mut hex = String::from("<");
            match font {
                // With a font of our own, the operand is a glyph index.
                Some(f) => match f.glyph(*ch) {
                    Some(gid) => {
                        used.insert(gid, *ch);
                        hex.push_str(&format!("{gid:04X}"));
                    }
                    None => continue,
                },
                // Otherwise the reader's own Japanese face maps UTF-16 directly.
                None => {
                    let mut buf = [0u16; 2];
                    for unit in ch.encode_utf16(&mut buf) {
                        hex.push_str(&format!("{unit:04X}"));
                    }
                }
            }
            hex.push('>');
            out.push_str(&format!(
                "{c:.5} {s:.5} {:.5} {c:.5} {x:.2} {base_y:.2} Tm {hex} Tj\n",
                -s
            ));
            placed += 1;
        }
        out.push_str("ET\n");
    }
    if colour.is_some() {
        out.push_str("0 0 0 rg\n");
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
        out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
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
