//! A single self-contained HTML page per document.
//!
//! Everything travels inside the one file: recovered page images as data URLs
//! and any original file found in the container offered for download.  The
//! generated page keeps the document content visually plain; the source name
//! is assigned to the HTML title and used as the base for optional downloads.

use crate::application::ports::{AttachmentScanner, PageDecoder};
use crate::application::recovery;
use crate::domain::output::Language as Lang;
use crate::domain::page::{Page, PageData};
use crate::domain::rendering::{
    self, BlendMode, Fill, Image, Metafile, Raster, RasterOp, Rect, Segment, Shape, Source,
};
use crate::domain::{DisplayPage, Document};
use crate::infrastructure::{png, LzhMetafileDecoder, MagicAttachmentScanner};

/// Settings for [`build`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Expand pages held in the container's own coding and lay out the text
    /// from the metafile inside.
    pub decode: bool,
    /// Language of the text this crate writes into the page.
    pub lang: Lang,
    /// Used as the document title and browser tab name.
    pub title: Option<String>,
    /// Include preview entries. Off by default; they are low resolution copies
    /// of other pages and they are in the vendor coding.
    pub include_previews: bool,
    /// Carry original files found in the container as download links.
    pub embed_originals: bool,
    /// Leave out the pages that could not be recovered, rather than marking
    /// them. Page numbering then no longer matches the original.
    pub skip_missing: bool,
    /// Offer the whole source document for download from the page.
    ///
    /// For an archive where most pages are in the vendor coding, carrying the
    /// source preserves the original bytes alongside the recovered content.
    /// It roughly doubles the output, so it is off by default.
    pub carry_source: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            decode: true,
            lang: Lang::English,
            title: None,
            include_previews: false,
            embed_originals: true,
            skip_missing: false,
            carry_source: false,
        }
    }
}

/// What went into the page.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Characters of page text placed.
    pub glyphs: usize,
    pub embedded: usize,
    pub gaps: usize,
    pub skipped: usize,
    pub attachments: usize,
    /// Pictures shown on sheets that could not themselves be reproduced.
    pub pictures: usize,
}

/// Build the HTML document.
pub fn build(data: &[u8], doc: &Document, opts: &Options) -> (String, Report) {
    let decoder = LzhMetafileDecoder;
    let attachments = MagicAttachmentScanner;
    build_with(data, doc, opts, &decoder, &attachments)
}

/// Build HTML using injected application ports.
pub fn build_with<D, A>(
    data: &[u8],
    doc: &Document,
    opts: &Options,
    decoder: &D,
    attachments: &A,
) -> (String, Report)
where
    D: PageDecoder + ?Sized,
    A: AttachmentScanner + ?Sized,
{
    let mut report = Report::default();
    // One output item per sheet. Thumbnails and the pictures a sheet is made of
    // are not sheets; listing them as pages would misreport the document's
    // length.
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

    // A small text-only sheet immediately following a JPEG is a saved
    // drawing layer in some exports.  Keep the HTML page sequence aligned
    // with the PDF adapter by merging that layer onto its image page.
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

    let title = opts.title.clone().unwrap_or_else(|| "document".into());
    let t = Text::for_lang(opts.lang);

    let mut body = String::new();
    let mut no = 0usize;
    if display_mode {
        for display in &doc.display_pages {
            no += 1;
            if let Some(drawn) =
                draw_display_sheet(display, data, doc, decoder, opts.decode, no, &t, &mut body)
            {
                report.embedded += 1;
                report.glyphs += drawn.glyphs;
                report.pictures += drawn.pictures;
            } else if opts.skip_missing {
                report.skipped += 1;
            } else {
                report.gaps += 1;
                body.push_str(&format!(
                    "<section id=\"p{no}\" class=\"gap\"><p class=\"why\">{}</p></section>\n",
                    esc(t.gap_why)
                ));
            }
        }
    } else {
        for (i, p) in selected.iter().enumerate() {
            if merge_label[i] {
                continue;
            }
            no += 1;
            let decoded = opts
                .decode
                .then(|| recovery::decode_page_for_document(data, p, doc, decoder))
                .flatten();
            // A picture page with a drawing of its own is drawn from that
            // drawing, which places the picture and whatever sits over it.
            let overlaid = opts.decode
                && !p.overlays.is_empty()
                // A missing page body can still have a complete drawing in the
                // properties block.  A decoded page body is handled together
                // with its own drawing below, so do not replace it here.  A
                // JPEG is handled in its own arm so text and vector
                // annotations can stay on top of the image.
                && !p.is_recoverable()
                && decoded.is_none()
                && draw_sheet(p, None, data, doc, decoder, no, &t, &mut body)
                    .map(|drawn| {
                        report.embedded += 1;
                        report.glyphs += drawn.glyphs;
                        report.pictures += drawn.pictures.saturating_sub(1);
                        true
                    })
                    .unwrap_or(false);
            if overlaid {
                continue;
            }
            match p.data {
                PageData::Jpeg { offset, len } if offset + len <= data.len() => {
                    report.embedded += 1;
                    let paper = p.paper.unwrap_or((21000, 29700));
                    let pw = paper.0 as f32 * 72.0 / 2540.0;
                    let ph = paper.1 as f32 * 72.0 / 2540.0;
                    let overlay =
                        if opts.decode && p.overlays.iter().any(|overlay| overlay.area.is_none()) {
                            draw_page_overlays(p, data, doc, decoder, no, &t, pw, ph)
                        } else {
                            HtmlOverlay::default()
                        };
                    if overlay.drawn.pictures > 0 {
                        report.glyphs += overlay.drawn.glyphs;
                        report.pictures += overlay.drawn.pictures.saturating_sub(1);
                        push_sheet_figure(p, no, pw, ph, &overlay.svg, &overlay.spans, &mut body);
                        continue;
                    }

                    let mut svg = overlay.svg;
                    let mut spans = overlay.spans;
                    report.glyphs += overlay.drawn.glyphs;
                    if let Some(label) = merged_labels.get(i + 1).and_then(Option::as_ref) {
                        let place = serial_label_place(label, pw, ph);
                        let drawn = draw_metafile(
                            label,
                            data,
                            &[],
                            place,
                            "l",
                            no,
                            &t,
                            &mut svg,
                            &mut spans,
                        );
                        report.glyphs += drawn.glyphs;
                        report.pictures += drawn.pictures;
                    }
                    let (art, art_count) = artwork(data, doc, p.index, &t);
                    report.pictures += art_count;
                    let has_art = !art.is_empty();
                    let image_style = if has_art {
                        "left:5%;top:0%;width:90%;height:52%;object-fit:contain"
                    } else {
                        "left:0%;top:0%;width:100%;height:100%"
                    };
                    let stacked_art = if has_art {
                        format!("<div class=\"picture-stack\">{art}</div>\n")
                    } else {
                        String::new()
                    };
                    let content = format!(
                        "<img class=\"art\" style=\"{image_style}\" loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n{stacked_art}{svg}{spans}",
                        esc(t.page_alt),
                        b64(&data[offset..offset + len]),
                    );
                    push_sheet_figure(p, no, pw, ph, &content, "", &mut body);
                }
                // The sheet is in the container's own coding. Expand it and draw
                // the metafile's text: the page comes back as real, selectable
                // text rather than a note saying it could not be read. A sheet
                // that does not expand falls through to the arm below, which still
                // shows whatever artwork sits on it.
                PageData::Encoded { .. } | PageData::Preview { .. } if decoded.is_some() => {
                    let m = decoded.as_ref().expect("decoded guard above");
                    report.embedded += 1;
                    if let Some(drawn) =
                        draw_sheet(p, Some(m), data, doc, decoder, no, &t, &mut body)
                    {
                        report.glyphs += drawn.glyphs;
                        report.pictures += drawn.pictures;
                    }
                }
                _ => {
                    if opts.skip_missing {
                        report.skipped += 1;
                        continue;
                    }
                    report.gaps += 1;
                    // The sheet itself could not be expanded, but the pictures on
                    // it are plain JPEG. Show them: a page comes back as its
                    // artwork instead of an empty box.
                    let (mut art, count) = artwork(data, doc, p.index, &t);
                    report.pictures += count;
                    if !art.is_empty() {
                        art = format!("<div class=\"arts\">\n{art}</div>\n");
                    }
                    body.push_str(&format!(
                    "<section id=\"p{no}\" class=\"gap\">\n<p class=\"why\">{}</p>\n{art}</section>\n",
                    esc(t.gap_why),
                ));
                }
            }
        }
    }
    // Empty either because the document holds nothing, or because everything it
    // holds was dropped. A page that just stops without saying so is worse than
    // either, so say so.
    if body.is_empty() {
        body.push_str(&format!("<p class=\"why\">{}</p>\n", esc(t.empty)));
    }

    let mut files = String::new();
    if opts.embed_originals {
        for (n, a) in attachments.scan(data).iter().enumerate() {
            if a.offset + a.len > data.len() {
                continue;
            }
            report.attachments += 1;
            let name = format!("{}-original-{}.{}", stem(&title), n + 1, a.kind.extension());
            files.push_str(&format!(
                "<li><a download=\"{}\" href=\"data:application/octet-stream;base64,{}\">{}</a></li>\n",
                esc(&name),
                b64(&data[a.offset..a.offset + a.len]),
                esc(&name),
            ));
        }
    }

    if opts.carry_source {
        report.attachments += 1;
        let name = format!("{}.xdw", stem(&title));
        files.push_str(&format!(
            "<li><a download=\"{}\" href=\"data:application/octet-stream;base64,{}\">{}</a></li>\n",
            esc(&name),
            b64(data),
            esc(&name),
        ));
    }

    // Attachments are functional content rather than document chrome, so keep
    // them after the recovered pages without adding a heading or file-size
    // summary.
    if !files.is_empty() {
        body.push_str("<ul class=\"files\">\n");
        body.push_str(&files);
        body.push_str("</ul>\n");
    }

    let html = format!(
        "<!doctype html>\n<html lang=\"{lang}\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{title_esc}</title>\n<style>{CSS}</style>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n",
        lang = t.lang_attr,
        title_esc = esc(&title),
    );
    (html, report)
}

const CSS: &str = "\
:root{color-scheme:light dark;--bg:#fff;--fg:#1b1b1a;--dim:#6b6b68}\
@media(prefers-color-scheme:dark){:root{--bg:#16161a;--fg:#e9e9e6;--dim:#9a9a96}}\
*{box-sizing:border-box}\
body{margin:0;padding:0 16px 48px;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,-apple-system,'Segoe UI',sans-serif}\
main{max-width:960px;margin:0 auto}\
figure{margin:0 0 28px}\
.frame{position:relative;width:100%;overflow:hidden;container-type:size}\
.sheet{position:absolute;left:0;top:0;width:100%;background:#fff;color:#000;overflow:hidden;container-type:size}\
.sheet.turn90{width:100cqh;height:100cqw;transform-origin:0 0;transform:translateX(100cqw) rotate(90deg)}\
.sheet.turn180{transform:rotate(180deg)}\
.sheet.turn270{width:100cqh;height:100cqw;transform-origin:0 0;transform:translateY(100cqh) rotate(-90deg)}\
.sheet .art{position:absolute;display:block;object-fit:contain;object-position:center}\
.sheet img.art{background:#fff}\
.sheet .picture-stack{position:absolute;left:4%;top:54%;width:92%;height:42%;display:flex;flex-direction:column;gap:4px;overflow:hidden}\
.sheet .picture-stack .art{position:relative;left:auto;top:auto;width:100%;height:100%;min-height:0;flex:1;margin:0}\
.sheet .picture-stack .bands{height:100%}\
.sheet .picture-stack .bands img{height:100%;object-fit:contain}\
.sheet span{position:absolute;white-space:pre;line-height:1;font-family:\"Hiragino Kaku Gothic ProN\",\"Yu Gothic\",\"Meiryo\",\"Noto Sans JP\",sans-serif}\
.files{margin:28px 0 0;padding-left:18px;font-size:.85rem}\
figure>img{display:block;width:100%;height:auto}\
.gap .why{margin:0 0 28px;color:var(--dim);font-size:.9rem}\
.arts{display:flex;flex-direction:column;gap:6px}\
.art{margin:0;position:relative}\
.bands{display:flex;flex-direction:column;line-height:0;overflow:hidden}\
.art img{display:block;width:100%;height:auto}\
@media print{body{background:#fff}figure{break-inside:avoid}}\
";

/// Every string this crate writes into the page, in one place.
struct Text {
    lang_attr: &'static str,
    gap_why: &'static str,
    page_alt: &'static str,
    art_alt: &'static str,
    empty: &'static str,
}

impl Text {
    fn for_lang(lang: Lang) -> Text {
        match lang {
            Lang::Japanese => Text {
                lang_attr: "ja",
                gap_why: "ページ画像がコンテナ独自の符号化で格納されており、復元できません。",
                page_alt: "復元したページ",
                art_alt: "ページから取り出した画像",
                empty: "この文書から復元できるページはありません。",
            },
            Lang::English => Text {
                lang_attr: "en",
                gap_why:
                    "The page image is stored in the container's own coding and was not recovered.",
                page_alt: "Recovered document page",
                art_alt: "Artwork recovered from this page",
                empty: "No page of this document could be recovered.",
            },
        }
    }
}

fn stem(title: &str) -> String {
    let base = title.rsplit(['/', '\\']).next().unwrap_or(title);
    let base = base.split_once('.').map(|(a, _)| a).unwrap_or(base);
    base.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
fn b64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let (chunks, remainder) = data.as_chunks::<3>();
    for c in chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
        }
    }
    match remainder {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            out.push_str("==");
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// Whether a sheet is small enough to plausibly be a text label attached to a
/// preceding image page.  It uses only the page geometry, never a document
/// name or a caller-provided value.
fn is_text_only_label(page: &Page) -> bool {
    let Some((w, h)) = page.paper else {
        return false;
    };
    w > 0 && h > 0 && w.max(h) <= 3000 && w.min(h) >= 300 && w.saturating_mul(h) <= 9_000_000
}

/// A serial label has one short, printable ASCII text run and no other
/// drawing primitives, which keeps the merge conservative.
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

/// Put a small label near the upper-right of the image it follows.  The
/// label's frame gives its physical size; only its anchor is absent from the
/// saved-over sheet representation.
fn serial_label_place(meta: &Metafile, pw: f32, ph: f32) -> Place {
    let (w, h) = meta.points();
    let right = (pw * 0.10).max(8.0);
    let top = (ph * 0.04).max(4.0);
    Place {
        x: ((pw - right - w).max(0.0)) / pw,
        y: top.min((ph - h).max(0.0)) / ph,
        w: (w / pw).clamp(0.0, 1.0),
        h: (h / ph).clamp(0.0, 1.0),
    }
}

/// Where a drawing lands on the sheet, as fractions of the sheet: left,
/// top, width, height.
#[derive(Debug, Clone, Copy)]
struct Place {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Place {
    const SHEET: Place = Place {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
}

/// Result of drawing one metafile.
///
/// `painted` is deliberately separate from the picture count.  A page can be
/// fully recovered from text, fills, or vector paths without containing any
/// bitmap at all; the PDF adapter uses the same distinction when deciding
/// whether a displayed page is a gap.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HtmlDrawn {
    glyphs: usize,
    pictures: usize,
    painted: bool,
}

impl HtmlDrawn {
    fn add(&mut self, other: HtmlDrawn) {
        self.glyphs += other.glyphs;
        self.pictures += other.pictures;
        self.painted |= other.painted;
    }
}

/// Rendered page overlays kept temporarily so a JPEG can receive annotation
/// artwork without being replaced by an annotation-only layer.
#[derive(Debug, Default)]
struct HtmlOverlay {
    svg: String,
    spans: String,
    drawn: HtmlDrawn,
}

/// Draw one metafile into a sheet: everything but the text goes into one
/// SVG element in the metafile's own device units, so pictures, fills, clip
/// paths and outlines need no conversion; the text goes down as positioned
/// spans over it, so it stays selectable and searchable.
///
/// Returns the characters and artwork that were actually placed.
#[allow(clippy::too_many_arguments)]
fn draw_metafile(
    m: &Metafile,
    data: &[u8],
    stored: &[&Page],
    place: Place,
    tag: &str,
    no: usize,
    t: &Text,
    svg: &mut String,
    spans: &mut String,
) -> HtmlDrawn {
    draw_metafile_with_viewbox(m, data, stored, place, tag, no, t, svg, spans, None)
}

#[allow(clippy::too_many_arguments)]
fn draw_metafile_with_viewbox(
    m: &Metafile,
    data: &[u8],
    stored: &[&Page],
    place: Place,
    tag: &str,
    no: usize,
    t: &Text,
    svg: &mut String,
    spans: &mut String,
    viewbox: Option<Rect>,
) -> HtmlDrawn {
    let (dw, dh) = (m.device.0 as f32, m.device.1 as f32);
    if dw <= 0.0 || dh <= 0.0 {
        return HtmlDrawn::default();
    }
    let (origin_x, origin_y, view_w, view_h) = viewbox
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .map(|r| (r.left, r.top, r.width(), r.height()))
        .unwrap_or((0.0, 0.0, dw, dh));
    let upright_vertical = m.uses_upright_vertical_text();
    // Pair the stored pictures the page names with the ones beside the sheet.
    let calls: Vec<(usize, (u32, u32))> = {
        let mut v: Vec<(usize, (u32, u32))> = m
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
    let picture_of = |ordinal: usize| -> Option<&Page> {
        let at = calls.iter().position(|c| c.0 == ordinal)?;
        paired.get(at).copied().flatten().map(|k| stored[k])
    };

    let mut stored_indices = std::collections::BTreeMap::new();
    for (&(ordinal, _), pick) in calls.iter().zip(paired.iter()) {
        if let Some(index) = *pick {
            stored_indices.insert(ordinal, index);
        }
    }

    // A transparent picture is commonly encoded as three adjacent image
    // operations: source XOR, a one-bit source-and mask, then source XOR
    // again.  PDF turns that into one image with a soft mask.  SVG has the
    // same primitive, so keep the operation as one masked JPEG here too.
    let mut masked_masks: std::collections::BTreeMap<usize, (String, String)> =
        std::collections::BTreeMap::new();
    let mut masked_parts = vec![false; m.images.len()];
    for middle in 1..m.images.len().saturating_sub(1) {
        let before_index = middle - 1;
        let after_index = middle + 1;
        if masked_parts[before_index] || masked_parts[middle] || masked_parts[after_index] {
            continue;
        }
        let before = &m.images[before_index];
        let mask = &m.images[middle];
        let after = &m.images[after_index];
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
            || !image_geometry_is_valid(before)
        {
            continue;
        }
        let Some(raster) = m.rasters.get(mask_index) else {
            continue;
        };
        let Some(mask_png) = html_mask_png(raster) else {
            continue;
        };
        let Some(source_page) = picture_of(before_ordinal) else {
            continue;
        };
        let PageData::Jpeg { offset, len } = source_page.data else {
            continue;
        };
        let Some(end) = offset.checked_add(len) else {
            continue;
        };
        if end > data.len() {
            continue;
        }
        let mask_id = format!("p{no}{tag}mask{before_index}");
        let mask_href = format!("data:image/png;base64,{}", b64(&mask_png));
        masked_masks.insert(before_index, (mask_id, mask_href));
        masked_parts[middle] = true;
        masked_parts[after_index] = true;
    }

    svg.push_str(&format!(
        "<svg class=\"art\" style=\"left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%\" viewBox=\"{origin_x} {origin_y} {view_w} {view_h}\" preserveAspectRatio=\"none\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\">\n",
        place.x * 100.0,
        place.y * 100.0,
        place.w * 100.0,
        place.h * 100.0
    ));
    if !m.paths.is_empty() {
        svg.push_str("<defs>");
        for (i, path) in m.paths.iter().enumerate() {
            svg.push_str(&format!(
                "<clipPath id=\"p{no}{tag}c{i}\"><path d=\"{}\"{}/></clipPath>",
                path_data(path),
                if path.even_odd {
                    " clip-rule=\"evenodd\""
                } else {
                    ""
                }
            ));
        }
        svg.push_str("</defs>\n");
    }
    if !masked_masks.is_empty() {
        svg.push_str("<defs>");
        for (index, (mask_id, mask_href)) in &masked_masks {
            let image = &m.images[*index];
            svg.push_str(&format!(
                "<mask id=\"{mask_id}\" maskUnits=\"userSpaceOnUse\" maskContentUnits=\"userSpaceOnUse\" mask-type=\"luminance\" x=\"0\" y=\"0\" width=\"{dw}\" height=\"{dh}\"><image x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" preserveAspectRatio=\"none\" href=\"{mask_href}\" xlink:href=\"{mask_href}\"/></mask>",
                image.left,
                image.top,
                image.width(),
                image.height(),
            ));
        }
        svg.push_str("</defs>\n");
    }
    // A clip rectangle becomes an SVG group with a clip path of its own.
    let mut open_rect: Option<Rect> = None;
    let mut rect_clips = 0usize;
    let mut set_rect = |svg: &mut String, want: Option<Rect>, open: &mut Option<Rect>| {
        if *open == want {
            return;
        }
        if open.is_some() {
            svg.push_str("</g>\n");
        }
        *open = None;
        if let Some(r) = want {
            rect_clips += 1;
            svg.push_str(&format!(
                "<clipPath id=\"p{no}{tag}r{rect_clips}\"><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\"/></clipPath><g clip-path=\"url(#p{no}{tag}r{rect_clips})\">\n",
                r.left,
                r.top,
                r.width(),
                r.height()
            ));
            *open = want;
        }
    };
    let clip_attr = |clip_path: Option<usize>| match clip_path {
        Some(i) => format!(" clip-path=\"url(#p{no}{tag}c{i})\""),
        None => String::new(),
    };

    enum Item<'a> {
        Fill(&'a Fill),
        Image(usize, &'a Image),
        Shape(&'a Shape),
    }
    let mut items: Vec<(usize, Item)> = Vec::new();
    items.extend(m.fills.iter().map(|f| (f.order, Item::Fill(f))));
    items.extend(
        m.images
            .iter()
            .enumerate()
            .map(|(index, i)| (i.order, Item::Image(index, i))),
    );
    items.extend(m.shapes.iter().map(|s| (s.order, Item::Shape(s))));
    items.sort_by_key(|(o, _)| *o);

    let mut drawn = HtmlDrawn::default();
    let mut pngs: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    for (_, item) in items {
        match item {
            Item::Fill(f) => {
                let width = f.right - f.left;
                let height = f.bottom - f.top;
                if !(f.left.is_finite()
                    && f.top.is_finite()
                    && width.is_finite()
                    && height.is_finite()
                    && width > 0.0
                    && height > 0.0)
                {
                    continue;
                }
                drawn.painted = true;
                set_rect(svg, f.clip, &mut open_rect);
                svg.push_str(&format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{}\"{}{} />\n",
                    f.left,
                    f.top,
                    width,
                    height,
                    hex(f.rgb),
                    blend_attr(f.blend),
                    clip_attr(f.clip_path)
                ));
            }
            Item::Image(index, img) => {
                if masked_parts[index]
                    || (img.raster_op == RasterOp::SourceInvert
                        && !masked_masks.contains_key(&index))
                    || !image_geometry_is_valid(img)
                {
                    continue;
                }
                let href = match img.source {
                    Source::Stored { ordinal, .. } => {
                        let Some(pic) = picture_of(ordinal) else {
                            continue;
                        };
                        let PageData::Jpeg { offset, len } = pic.data else {
                            continue;
                        };
                        if offset + len > data.len() {
                            continue;
                        }
                        format!(
                            "data:image/jpeg;base64,{}",
                            b64(&data[offset..offset + len])
                        )
                    }
                    Source::Inline(i) => {
                        let Some(r) = m.rasters.get(i) else { continue };
                        pngs.entry(i)
                            .or_insert_with(|| {
                                format!("data:image/png;base64,{}", b64(&png::encode(r)))
                            })
                            .clone()
                    }
                };
                let mask_attr = masked_masks
                    .get(&index)
                    .map(|(mask_id, _)| format!(" mask=\"url(#{mask_id})\""))
                    .unwrap_or_default();
                set_rect(svg, img.clip, &mut open_rect);
                // A hair of overlap, so bands that abut show no seam.
                svg.push_str(&format!(
                    "<image x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" preserveAspectRatio=\"none\"{}{mask_attr} href=\"{href}\" xlink:href=\"{href}\"><title>{}</title></image>\n",
                    img.left,
                    img.top,
                    img.width() + 0.5,
                    img.height() + 0.5,
                    clip_attr(img.clip_path),
                    esc(t.art_alt)
                ));
                drawn.pictures += 1;
                drawn.painted = true;
            }
            Item::Shape(sh) => {
                if sh.path.is_empty() {
                    continue;
                }
                drawn.painted = true;
                set_rect(svg, sh.clip, &mut open_rect);
                let fill = match sh.fill {
                    Some(rgb) => hex(rgb),
                    None => "none".into(),
                };
                let stroke = match sh.stroke {
                    Some((rgb, w)) => {
                        format!(
                            " stroke=\"{}\" stroke-width=\"{:.2}\"",
                            hex(rgb),
                            w.max(0.5)
                        )
                    }
                    None => String::new(),
                };
                svg.push_str(&format!(
                    "<path d=\"{}\" fill=\"{fill}\"{}{}{}/>\n",
                    path_data(&sh.path),
                    if sh.path.even_odd {
                        " fill-rule=\"evenodd\""
                    } else {
                        ""
                    },
                    blend_attr(sh.blend),
                    stroke
                ));
            }
        }
    }
    set_rect(svg, None, &mut open_rect);
    svg.push_str("</svg>\n");

    let mut placed = 0usize;
    for run in &m.text {
        if run.chars.iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let size = run.size;
        let text_y = if upright_vertical {
            run.y + size * 0.8
        } else {
            run.y
        };
        let top = place.y + (text_y - size - origin_y) / view_h * place.h;
        let (r, g, b) = run.rgb;
        let colour = if (r, g, b) == (0, 0, 0) {
            String::new()
        } else {
            format!("color:#{r:02x}{g:02x}{b:02x};")
        };
        let weight = if run.bold { "font-weight:bold;" } else { "" };
        let line = if run.underline {
            "text-decoration:underline;"
        } else {
            ""
        };
        // Turned text is rotated about its own start, as the metafile means it.
        let turn = if run.escapement != 0 && !upright_vertical {
            format!(
                "transform:rotate({:.1}deg);transform-origin:0 100%;",
                -(run.escapement as f32) / 10.0
            )
        } else {
            String::new()
        };
        for (i, c) in run.chars.iter().enumerate() {
            if c.is_whitespace() {
                continue;
            }
            let left = (place.x
                + (run.xs.get(i).copied().unwrap_or(0.0) - origin_x) / view_w * place.w)
                * 100.0;
            let top = top * 100.0;
            if !left.is_finite() || !top.is_finite() {
                continue;
            }
            spans.push_str(&format!(
                "<span style=\"left:{left:.3}%;top:{top:.3}%;font-size:{:.3}cqh;{colour}{weight}{line}{turn}\">{}</span>",
                size / view_h * place.h * 100.0,
                esc(&c.to_string())
            ));
            placed += 1;
        }
        spans.push('\n');
    }
    drawn.glyphs = placed;
    drawn.painted |= placed > 0;
    drawn
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn blend_attr(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "",
        BlendMode::Multiply => " style=\"mix-blend-mode:multiply\"",
    }
}

fn image_geometry_is_valid(image: &Image) -> bool {
    let width = image.width();
    let height = image.height();
    image.left.is_finite()
        && image.top.is_finite()
        && image.right.is_finite()
        && image.bottom.is_finite()
        && width.is_finite()
        && height.is_finite()
        && width > 0.0
        && height > 0.0
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

/// Convert a one-bit source-and raster into a luminance mask for SVG.
///
/// In the GDI operation zero bits are the opaque foreground.  SVG luminance
/// masks have the opposite useful convention for black/white pixels, so make
/// zero white and one black explicitly instead of relying on the source
/// palette's order.
fn html_mask_png(mask: &Raster) -> Option<Vec<u8>> {
    if mask.bits != 1 || mask.width == 0 || mask.height == 0 {
        return None;
    }
    let stride = mask.stride();
    let required = stride.checked_mul(mask.height as usize)?;
    if mask.rows.len() < required {
        return None;
    }
    let width = usize::try_from(mask.width).ok()?;
    let height = usize::try_from(mask.height).ok()?;
    let mut rows = Vec::with_capacity(width.checked_mul(height)?);
    for y in 0..height {
        let row = &mask.rows[y * stride..(y + 1) * stride];
        for x in 0..width {
            let bit = (row[x / 8] >> (7 - x % 8)) & 1;
            rows.push(if bit == 0 { 0 } else { 1 });
        }
    }
    Some(png::encode(&Raster {
        width: mask.width,
        height: mask.height,
        bits: 8,
        palette: vec![(255, 255, 255), (0, 0, 0)],
        rows,
        stencil: None,
    }))
}

/// An SVG path string for a path in device units.
fn path_data(p: &rendering::Path) -> String {
    let mut d = String::new();
    for f in &p.figures {
        d.push_str(&format!("M{:.1} {:.1}", f.start.0, f.start.1));
        for s in &f.segments {
            match s {
                Segment::Line((x, y)) => d.push_str(&format!("L{x:.1} {y:.1}")),
                Segment::Curve(a, b, c) => d.push_str(&format!(
                    "C{:.1} {:.1} {:.1} {:.1} {:.1} {:.1}",
                    a.0, a.1, b.0, b.1, c.0, c.1
                )),
            }
        }
        if f.closed {
            d.push('Z');
        }
    }
    d
}

/// One sheet with its drawings and overlays, as a figure.
///
/// `main` is the sheet's own metafile, if it expanded. A picture page whose
/// overlay places its pictures is drawn from the overlay alone.
#[allow(clippy::too_many_arguments)]
fn draw_sheet<D: PageDecoder + ?Sized>(
    p: &Page,
    main: Option<&Metafile>,
    data: &[u8],
    doc: &Document,
    decoder: &D,
    no: usize,
    t: &Text,
    body: &mut String,
) -> Option<HtmlDrawn> {
    const PT: f32 = 72.0 / 2540.0;
    let paper = p.paper.unwrap_or((21000, 29700));
    let (pw, ph) = (paper.0 as f32 * PT, paper.1 as f32 * PT);
    let mut svg = String::new();
    let mut spans = String::new();
    let mut drawn = HtmlDrawn::default();
    if let Some(m) = main {
        let stored: Vec<&Page> = doc.pictures_on(p.index).collect();
        drawn.add(draw_metafile(
            m,
            data,
            &stored,
            Place::SHEET,
            "m",
            no,
            t,
            &mut svg,
            &mut spans,
        ));
    }
    let overlays = draw_page_overlays(p, data, doc, decoder, no, t, pw, ph);
    svg.push_str(&overlays.svg);
    spans.push_str(&overlays.spans);
    drawn.add(overlays.drawn);
    // A properties-only page can contain selectable text or vector shapes but
    // no picture.  Do not discard it merely because the picture count is zero.
    if main.is_none() && !drawn.painted {
        return None;
    }
    push_sheet_figure(p, no, pw, ph, &svg, &spans, body);
    Some(drawn)
}

/// Append one sheet-shaped figure using the same rotation and proportions for
/// full pages and overlay-only pages.
fn push_sheet_figure(
    p: &Page,
    no: usize,
    pw: f32,
    ph: f32,
    svg: &str,
    spans: &str,
    body: &mut String,
) {
    let turned = p.rotation % 180 == 90;
    let (shown_w, shown_h) = if turned { (ph, pw) } else { (pw, ph) };
    body.push_str(&format!(
        "<figure id=\"p{no}\">\n<div class=\"frame\" style=\"aspect-ratio:{:.4}\">\n<div class=\"sheet{}\" style=\"aspect-ratio:{:.4}\">\n{svg}{spans}</div>\n</div>\n</figure>\n",
        shown_w / shown_h,
        match p.rotation % 360 {
            90 => " turn90",
            180 => " turn180",
            270 => " turn270",
            _ => "",
        },
        pw / ph,
    ));
}

/// Decode and draw the normal page overlays into temporary layer strings.
/// Keeping the layer separate is important for JPEG pages: an annotation-only
/// overlay must be composited above the JPEG, while an overlay that contains a
/// page picture replaces the JPEG as the authoritative page drawing.
#[allow(clippy::too_many_arguments)]
fn draw_page_overlays<D: PageDecoder + ?Sized>(
    p: &Page,
    data: &[u8],
    doc: &Document,
    decoder: &D,
    no: usize,
    t: &Text,
    pw: f32,
    ph: f32,
) -> HtmlOverlay {
    const PT: f32 = 72.0 / 2540.0;
    let paper = p.paper.unwrap_or((21000, 29700));
    let mut layer = HtmlOverlay::default();
    let mut group: Vec<&Page> = doc.pictures_on(p.index).collect();
    if p.is_recoverable() {
        group.push(p);
        group.sort_by_key(|q| q.index);
    }
    for (n, overlay) in p.overlays.iter().enumerate() {
        let Some(m) = decoder.decode_overlay(overlay, paper) else {
            continue;
        };
        let mut place = match overlay.area {
            None => Place::SHEET,
            Some((x, y, w, h)) => Place {
                x: x as f32 * PT / pw,
                y: y as f32 * PT / ph,
                w: w as f32 * PT / pw,
                h: h as f32 * PT / ph,
            },
        };
        // Text-only WMFs carry point-sized text in their own device
        // coordinate system.  The properties rectangle is the anchor, not a
        // scale box; keep the same physical sizing used by the PDF adapter.
        if is_point_sized_text(overlay, &m) {
            place = fit_text_place(place, &m, pw, ph);
        }
        // The properties rectangle controls placement only.  A full-page
        // overlay may still refer to the pictures stored beside its anchor;
        // the source uses exactly that form for its background and panel.
        let uses_stored = m
            .images
            .iter()
            .any(|image| matches!(image.source, Source::Stored { .. }));
        let stored: &[&Page] = if overlay.area.is_none() || uses_stored {
            &group
        } else {
            &[]
        };
        layer.drawn.add(draw_metafile(
            &m,
            data,
            stored,
            place,
            &format!("o{n}"),
            no,
            t,
            &mut layer.svg,
            &mut layer.spans,
        ));
    }
    layer
}

/// Draw one logical displayed page whose properties record places several
/// page-table bodies on a single sheet.
#[allow(clippy::too_many_arguments)]
fn draw_display_sheet<D: PageDecoder + ?Sized>(
    display: &DisplayPage,
    data: &[u8],
    doc: &Document,
    decoder: &D,
    decode: bool,
    no: usize,
    t: &Text,
    body: &mut String,
) -> Option<HtmlDrawn> {
    const PT: f32 = 72.0 / 2540.0;
    let paper = display.paper.unwrap_or((21000, 29700));
    let (pw, ph) = (paper.0 as f32 * PT, paper.1 as f32 * PT);
    if pw <= 0.0 || ph <= 0.0 {
        return None;
    }
    let mut svg = String::new();
    let mut spans = String::new();
    let mut drawn = HtmlDrawn::default();
    let mut recovered = false;
    // When an overlay refers to the page-table pictures by stored ordinal, it
    // is the authoritative composition of the displayed sheet. Drawing the
    // display member JPEG underneath it as a full-sheet image leaves that
    // JPEG's opaque background visible around the overlay's bounds.
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
        let place = member
            .area
            .map(|area| display_area_place(area, pw, ph))
            .unwrap_or(Place::SHEET);
        match page.data {
            PageData::Jpeg { offset, len } if offset + len <= data.len() => {
                if overlay_uses_stored && member.area.is_none() {
                    continue;
                }
                svg.push_str(&format!(
                    "<img class=\"art\" style=\"left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%\" loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n",
                    place.x * 100.0,
                    place.y * 100.0,
                    place.w * 100.0,
                    place.h * 100.0,
                    esc(t.page_alt),
                    b64(&data[offset..offset + len]),
                ));
                // This is the displayed page body itself, not artwork
                // salvaged from an unrecovered sheet.  The PDF report counts
                // only additional metafile/overlay pictures here.
                drawn.painted = true;
                recovered = true;
            }
            PageData::Encoded { .. } | PageData::Preview { .. } if decode => {
                let Some(meta) = recovery::decode_page_for_document(data, page, doc, decoder)
                else {
                    continue;
                };
                let stored: Vec<&Page> = doc.pictures_on(page.index).collect();
                let member_drawn = draw_metafile_with_viewbox(
                    &meta,
                    data,
                    &stored,
                    place,
                    &format!("d{n}"),
                    no,
                    t,
                    &mut svg,
                    &mut spans,
                    member_viewbox(member.area, &meta),
                );
                drawn.add(member_drawn);
                recovered |= member_drawn.painted;
            }
            _ => {}
        }
    }

    if let Some(member) = display.members.first() {
        if let Some(anchor) = doc.pages.get(member.page_index) {
            let mut group: Vec<&Page> = doc.pictures_on(anchor.index).collect();
            if anchor.is_recoverable() {
                group.push(anchor);
                group.sort_by_key(|page| page.index);
            }
            for (n, overlay) in display.overlays.iter().enumerate() {
                let Some(meta) = decoder.decode_overlay(overlay, paper) else {
                    continue;
                };
                let mut place = overlay
                    .area
                    .map(|area| display_area_place(area, pw, ph))
                    .unwrap_or(Place::SHEET);
                if is_point_sized_text(overlay, &meta) {
                    place = fit_text_place(place, &meta, pw, ph);
                }
                // The properties rectangle controls placement only.  A
                // full-page overlay may still refer to the pictures stored
                // beside its anchor; the source uses exactly that form.
                let uses_stored = meta
                    .images
                    .iter()
                    .any(|image| matches!(image.source, Source::Stored { .. }));
                let stored: &[&Page] = if overlay.area.is_none() || uses_stored {
                    &group
                } else {
                    &[]
                };
                let overlay_drawn = draw_metafile(
                    &meta,
                    data,
                    stored,
                    place,
                    &format!("do{n}"),
                    no,
                    t,
                    &mut svg,
                    &mut spans,
                );
                drawn.add(overlay_drawn);
                recovered |= overlay_drawn.painted;
            }
        }
    }

    // The properties stream can explicitly retain a blank logical page even
    // when the page table has no body to attach to it.
    if display.members.is_empty() && display.overlays.is_empty() {
        recovered = true;
    }

    if !recovered {
        return None;
    }
    // `DisplayPage::paper` already describes the final displayed sheet.  A
    // rotated member is turned inside the sheet by `.turn90`/`.turn270`; do
    // not swap the outer frame as well, or an A3 landscape sheet becomes a
    // portrait page with large empty margins.  Keep the displayed paper
    // proportions for the outer frame.
    let (shown_w, shown_h) = (pw, ph);
    body.push_str(&format!(
        "<figure id=\"p{no}\">\n<div class=\"frame\" style=\"aspect-ratio:{:.4}\">\n<div class=\"sheet{}\" style=\"aspect-ratio:{:.4}\">\n{svg}{spans}</div>\n</div>\n</figure>\n",
        shown_w / shown_h,
        match display.rotation % 360 {
            90 => " turn90",
            180 => " turn180",
            270 => " turn270",
            _ => "",
        },
        pw / ph,
    ));
    Some(drawn)
}

/// Use the occupied drawing box for a composed vector member whose EMF device
/// extent is clearly much larger than the artwork it contains.
fn member_viewbox(area: Option<(u32, u32, u32, u32)>, meta: &Metafile) -> Option<Rect> {
    let _ = area?;
    let bounds = meta.content_bounds()?;
    let (dw, dh) = (
        meta.device.0.unsigned_abs() as f32,
        meta.device.1.unsigned_abs() as f32,
    );
    if dw <= 0.0 || dh <= 0.0 || bounds.width() >= dw * 0.5 || bounds.height() >= dh * 0.5 {
        return None;
    }
    Some(bounds)
}

/// Convert a properties rectangle into CSS fractions of the displayed paper.
fn display_area_place((x, y, w, h): (u32, u32, u32, u32), pw: f32, ph: f32) -> Place {
    const PT: f32 = 72.0 / 2540.0;
    Place {
        x: x as f32 * PT / pw,
        y: y as f32 * PT / ph,
        w: w as f32 * PT / pw,
        h: h as f32 * PT / ph,
    }
}

/// Text-only annotation records use their rectangle as an anchor rather than
/// as a scale box.  Artwork-bearing records must keep the normal fit-to-box
/// behavior.
fn is_point_sized_text(overlay: &crate::domain::page::Overlay, meta: &Metafile) -> bool {
    overlay.area.is_some()
        && !meta.text.is_empty()
        && meta.images.is_empty()
        && meta.rasters.is_empty()
        && meta.fills.is_empty()
        && meta.shapes.is_empty()
}

/// Fit text-only annotation contents to their properties rectangle.  A few
/// exporters store a square metafile frame for a single-line annotation, so
/// fitting that frame would make the text much too small.
fn fit_text_place(mut place: Place, meta: &Metafile, pw: f32, ph: f32) -> Place {
    let (ux, uy) = meta.units_per_point();
    let Some((right, top, bottom)) = text_bounds(meta) else {
        return place;
    };
    let text_w = right / ux;
    let text_h = (bottom - top) / uy;
    let area_w = place.w * pw;
    let area_h = place.h * ph;
    if !(text_w > 0.0 && text_h > 0.0 && area_w > 0.0 && area_h > 0.0) {
        return place;
    }
    let fit = (area_w / text_w).min(area_h / text_h);
    let (mw, mh) = meta.points();
    if fit.is_finite() && fit > 0.0 && mw > 0.0 && mh > 0.0 {
        place.w = mw * fit / pw;
        place.h = mh * fit / ph;
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

/// The plain-JPEG artwork that belongs to one sheet, as figures.
fn artwork(data: &[u8], doc: &Document, index: usize, t: &Text) -> (String, usize) {
    let mut art = String::new();
    let mut count = 0usize;
    for run in doc.picture_runs(index) {
        let bands: Vec<(usize, usize)> = run
            .iter()
            .filter_map(|pic| match pic.data {
                PageData::Jpeg { offset, len } if offset + len <= data.len() => Some((offset, len)),
                _ => None,
            })
            .collect();
        if bands.is_empty() {
            continue;
        }
        count += bands.len();
        art.push_str("<figure class=\"art\"><div class=\"bands\">\n");
        for (offset, len) in &bands {
            art.push_str(&format!(
                "<img loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n",
                esc(t.art_alt),
                b64(&data[*offset..*offset + *len]),
            ));
        }
        art.push_str("</div></figure>\n");
    }
    (art, count)
}

#[cfg(test)]
mod tests {
    use super::{b64, build_with, Options, CSS};
    use crate::application::ports::PageDecoder;
    use crate::domain::page::{Overlay, Page, PageData, Role};
    use crate::domain::rendering::{
        Figure, Image, Metafile, Path, Raster, Segment, Shape, Source, Text,
    };
    use crate::domain::{DisplayMember, DisplayPage, Document};

    #[derive(Debug)]
    struct PropertiesOnlyText;

    impl PageDecoder for PropertiesOnlyText {
        fn decode(&self, _data: &[u8], _page: &Page) -> Option<Metafile> {
            None
        }

        fn decode_overlay(&self, _overlay: &Overlay, _paper: (u32, u32)) -> Option<Metafile> {
            Some(Metafile {
                device: (100, 100),
                frame_mm100: (21000, 29700),
                text: vec![Text {
                    xs: vec![10.0],
                    y: 20.0,
                    chars: vec!['A'],
                    font_kind: crate::domain::rendering::FontKind::Japanese,
                    size: 10.0,
                    escapement: 0,
                    rgb: (0, 0, 0),
                    order: 0,
                    bold: false,
                    underline: false,
                }],
                ..Metafile::default()
            })
        }
    }

    #[derive(Debug)]
    struct VectorDisplay;

    impl PageDecoder for VectorDisplay {
        fn decode(&self, _data: &[u8], _page: &Page) -> Option<Metafile> {
            Some(Metafile {
                device: (100, 100),
                frame_mm100: (21000, 29700),
                shapes: vec![Shape {
                    path: Path {
                        figures: vec![Figure {
                            start: (10.0, 10.0),
                            segments: vec![
                                Segment::Line((90.0, 10.0)),
                                Segment::Line((90.0, 90.0)),
                                Segment::Line((10.0, 90.0)),
                            ],
                            closed: true,
                        }],
                        even_odd: false,
                    },
                    fill: Some((255, 0, 0)),
                    stroke: None,
                    blend: crate::domain::rendering::BlendMode::Normal,
                    order: 0,
                    clip: None,
                }],
                ..Metafile::default()
            })
        }
    }

    #[derive(Debug)]
    struct MaskedPicture;

    impl PageDecoder for MaskedPicture {
        fn decode(&self, _data: &[u8], _page: &Page) -> Option<Metafile> {
            None
        }

        fn decode_overlay(&self, _overlay: &Overlay, _paper: (u32, u32)) -> Option<Metafile> {
            Some(Metafile {
                device: (100, 100),
                frame_mm100: (21000, 29700),
                images: vec![
                    Image {
                        left: 0.0,
                        top: 0.0,
                        right: 100.0,
                        bottom: 100.0,
                        src: (2, 1),
                        source: Source::Stored {
                            ordinal: 0,
                            px: (2, 1),
                        },
                        raster_op: crate::domain::rendering::RasterOp::SourceInvert,
                        order: 0,
                        clip: None,
                        clip_path: None,
                    },
                    Image {
                        left: 0.0,
                        top: 0.0,
                        right: 100.0,
                        bottom: 100.0,
                        src: (2, 1),
                        source: Source::Inline(0),
                        raster_op: crate::domain::rendering::RasterOp::And,
                        order: 1,
                        clip: None,
                        clip_path: None,
                    },
                    Image {
                        left: 0.0,
                        top: 0.0,
                        right: 100.0,
                        bottom: 100.0,
                        src: (2, 1),
                        source: Source::Stored {
                            ordinal: 1,
                            px: (2, 1),
                        },
                        raster_op: crate::domain::rendering::RasterOp::SourceInvert,
                        order: 2,
                        clip: None,
                        clip_path: None,
                    },
                ],
                rasters: vec![Raster {
                    width: 2,
                    height: 1,
                    bits: 1,
                    palette: vec![(0, 0, 0), (255, 255, 255)],
                    rows: vec![0b1000_0000],
                    stencil: None,
                }],
                ..Metafile::default()
            })
        }
    }

    fn page(data: PageData, overlays: Vec<Overlay>) -> Page {
        Page {
            index: 0,
            role: Role::Sheet,
            belongs_to: None,
            offset: 0,
            checksum: None,
            paper: Some((21000, 29700)),
            pixels: Some((2, 1)),
            rotation: 0,
            overlays,
            data,
            unknown_fields: Vec::new(),
        }
    }

    fn document_with_page(page: Page) -> Document {
        Document {
            generation: 7,
            guard: [0; 4],
            trailer_tag: 0x65,
            trailer_at: 0,
            declared_entries: 1,
            pages: vec![page],
            display_pages: Vec::new(),
            properties: None,
            properties_len: None,
            image_derived: Vec::new(),
            checksum: None,
            generations_present: 1,
            unknown_tags: Vec::new(),
            rebuilt: None,
        }
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64(&[0xFF, 0xFF, 0xFF]), "////");
        assert_eq!(b64(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn svg_layers_are_transparent_but_bitmap_layers_keep_a_white_backdrop() {
        assert!(CSS.contains(
            ".sheet .art{position:absolute;display:block;object-fit:contain;object-position:center}"
        ));
        assert!(CSS.contains(".sheet img.art{background:#fff}"));
        assert!(!CSS.contains(
            ".sheet .art{position:absolute;display:block;object-fit:contain;object-position:center;background:#fff}"
        ));
    }

    #[test]
    fn properties_only_text_is_not_reported_as_a_gap() {
        let document = Document {
            generation: 7,
            guard: [0; 4],
            trailer_tag: 0x65,
            trailer_at: 0,
            declared_entries: 1,
            pages: vec![Page {
                index: 0,
                role: Role::Sheet,
                belongs_to: None,
                offset: 0,
                checksum: None,
                paper: Some((21000, 29700)),
                pixels: None,
                rotation: 0,
                overlays: vec![Overlay {
                    kind: 1,
                    expanded: 1,
                    coded: Vec::new(),
                    pixels: None,
                    area: None,
                }],
                data: PageData::Bare { offset: 0, len: 0 },
                unknown_fields: Vec::new(),
            }],
            display_pages: Vec::new(),
            properties: None,
            properties_len: None,
            image_derived: Vec::new(),
            checksum: None,
            generations_present: 1,
            unknown_tags: Vec::new(),
            rebuilt: None,
        };
        let options = Options {
            embed_originals: false,
            ..Options::default()
        };
        let (html, report) = build_with(
            &[],
            &document,
            &options,
            &PropertiesOnlyText,
            &crate::infrastructure::MagicAttachmentScanner,
        );
        assert_eq!(report.embedded, 1);
        assert_eq!(report.gaps, 0);
        assert!(html.contains(">A</span>"));
        assert!(html.contains("<title>document</title>"));
        assert!(!html.contains("<h1>"));
        assert!(!html.contains("class=\"page\""));
        assert!(!html.contains("<figcaption>"));
        assert!(!html.contains("pages recovered"));
    }

    #[test]
    fn vector_only_display_page_is_recovered_like_pdf() {
        let page = page(
            PageData::Encoded {
                offset: 0,
                len: 0,
                kind_code: 4,
                aux_len: None,
                method: None,
                colour: None,
            },
            Vec::new(),
        );
        let mut document = document_with_page(page);
        document.display_pages = vec![DisplayPage {
            paper: Some((21000, 29700)),
            rotation: 0,
            overlays: Vec::new(),
            members: vec![DisplayMember {
                page_index: 0,
                area: None,
            }],
        }];
        let options = Options {
            embed_originals: false,
            ..Options::default()
        };
        let (html, report) = build_with(
            &[],
            &document,
            &options,
            &VectorDisplay,
            &crate::infrastructure::MagicAttachmentScanner,
        );
        assert_eq!(report.embedded, 1);
        assert_eq!(report.gaps, 0);
        assert!(html.contains("<path"));
    }

    #[test]
    fn jpeg_keeps_annotation_text_above_the_image() {
        let overlay = Overlay {
            kind: 1,
            expanded: 1,
            coded: Vec::new(),
            pixels: None,
            area: None,
        };
        let document =
            document_with_page(page(PageData::Jpeg { offset: 0, len: 3 }, vec![overlay]));
        let options = Options {
            embed_originals: false,
            ..Options::default()
        };
        let (html, report) = build_with(
            &[1, 2, 3],
            &document,
            &options,
            &PropertiesOnlyText,
            &crate::infrastructure::MagicAttachmentScanner,
        );
        assert_eq!(report.embedded, 1);
        assert_eq!(report.glyphs, 1);
        assert!(html.contains("data:image/jpeg;base64,AQID"));
        assert!(html.contains(">A</span>"));
    }

    #[test]
    fn stored_overlay_replaces_full_sheet_display_jpeg() {
        let overlay = Overlay {
            kind: 1,
            expanded: 1,
            coded: Vec::new(),
            pixels: None,
            area: None,
        };
        let page = page(PageData::Jpeg { offset: 0, len: 3 }, Vec::new());
        let mut document = document_with_page(page);
        document.display_pages = vec![DisplayPage {
            paper: Some((21000, 29700)),
            rotation: 0,
            overlays: vec![overlay],
            members: vec![DisplayMember {
                page_index: 0,
                area: None,
            }],
        }];
        let options = Options {
            embed_originals: false,
            ..Options::default()
        };
        let (html, report) = build_with(
            &[1, 2, 3],
            &document,
            &options,
            &MaskedPicture,
            &crate::infrastructure::MagicAttachmentScanner,
        );
        assert_eq!(report.embedded, 1);
        // The JPEG is emitted only through the stored-picture overlay. A
        // second full-sheet copy would recreate the opaque edge background.
        assert_eq!(html.matches("<img class=\"art\"").count(), 0);
    }

    #[test]
    fn masked_picture_sequence_becomes_one_svg_image() {
        let overlay = Overlay {
            kind: 1,
            expanded: 1,
            coded: Vec::new(),
            pixels: None,
            area: None,
        };
        let document =
            document_with_page(page(PageData::Jpeg { offset: 0, len: 3 }, vec![overlay]));
        let options = Options {
            embed_originals: false,
            ..Options::default()
        };
        let (html, report) = build_with(
            &[1, 2, 3],
            &document,
            &options,
            &MaskedPicture,
            &crate::infrastructure::MagicAttachmentScanner,
        );
        assert_eq!(report.embedded, 1);
        // The one image is the page's own JPEG, so it is not counted as
        // additional artwork in the report.
        assert_eq!(report.pictures, 0);
        assert!(html.contains("mask=\"url(#p1o0mask0)\""));
        assert_eq!(html.matches("data:image/jpeg;base64,AQID").count(), 2);
    }
}
