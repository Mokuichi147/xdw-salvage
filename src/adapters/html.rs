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
    self, Fill, Image, Metafile, RasterOp, Rect, Segment, Shape, Source,
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
            if let Some((g, d)) =
                draw_display_sheet(display, data, doc, decoder, opts.decode, no, &t, &mut body)
            {
                report.embedded += 1;
                report.glyphs += g;
                report.pictures += d;
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
            // with its own drawing below, so do not replace it here.  A JPEG
            // needs a page-level overlay; annotation-only drawings must stay
            // on the normal image path.
            && ((!p.is_recoverable() && decoded.is_none())
                || (p.is_recoverable() && p.overlays.iter().any(|o| o.area.is_none())))
            && draw_sheet(p, None, data, doc, decoder, no, &t, &mut body)
                .map(|(g, d)| {
                    report.embedded += 1;
                    report.glyphs += g;
                    report.pictures += d.saturating_sub(1);
                })
                .is_some();
            if overlaid {
                continue;
            }
            match p.data {
                PageData::Jpeg { offset, len } if offset + len <= data.len() => {
                    report.embedded += 1;
                    if let Some(label) = merged_labels.get(i + 1).and_then(Option::as_ref) {
                        let paper = p.paper.unwrap_or((21000, 29700));
                        let pw = paper.0 as f32 * 72.0 / 2540.0;
                        let ph = paper.1 as f32 * 72.0 / 2540.0;
                        let mut svg = String::new();
                        let mut spans = String::new();
                        let place = serial_label_place(label, pw, ph);
                        let (g, d) = draw_metafile(
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
                        report.glyphs += g;
                        report.pictures += d;
                        body.push_str(&format!(
                        "<figure id=\"p{no}\">\n<div class=\"frame\" style=\"aspect-ratio:{:.4}\">\n<div class=\"sheet\" style=\"aspect-ratio:{:.4}\">\n<img class=\"art\" style=\"left:0%;top:0%;width:100%;height:100%\" loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n{svg}{spans}</div>\n</div>\n</figure>\n",
                        pw / ph,
                        pw / ph,
                        esc(t.page_alt),
                        b64(&data[offset..offset + len]),
                    ));
                    } else {
                        body.push_str(&format!(
                        "<figure id=\"p{no}\">\n<img loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n</figure>\n",
                        esc(t.page_alt),
                        b64(&data[offset..offset + len]),
                    ));
                    }
                }
                // The sheet is in the container's own coding. Expand it and draw
                // the metafile's text: the page comes back as real, selectable
                // text rather than a note saying it could not be read. A sheet
                // that does not expand falls through to the arm below, which still
                // shows whatever artwork sits on it.
                PageData::Encoded { .. } | PageData::Preview { .. } if decoded.is_some() => {
                    let m = decoded.as_ref().expect("decoded guard above");
                    report.embedded += 1;
                    if let Some((placed, drawn)) =
                        draw_sheet(p, Some(m), data, doc, decoder, no, &t, &mut body)
                    {
                        report.glyphs += placed;
                        report.pictures += drawn;
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
.sheet .art{position:absolute;display:block;object-fit:contain;object-position:center;background:#fff}\
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

/// Draw one metafile into a sheet: everything but the text goes into one
/// SVG element in the metafile's own device units, so pictures, fills, clip
/// paths and outlines need no conversion; the text goes down as positioned
/// spans over it, so it stays selectable and searchable.
///
/// Returns the characters placed and the pictures drawn.
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
) -> (usize, usize) {
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
) -> (usize, usize) {
    let (dw, dh) = (m.device.0 as f32, m.device.1 as f32);
    if dw <= 0.0 || dh <= 0.0 {
        return (0, 0);
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
        Image(&'a Image),
        Shape(&'a Shape),
    }
    let mut items: Vec<(usize, Item)> = Vec::new();
    items.extend(m.fills.iter().map(|f| (f.order, Item::Fill(f))));
    items.extend(m.images.iter().map(|i| (i.order, Item::Image(i))));
    items.extend(m.shapes.iter().map(|s| (s.order, Item::Shape(s))));
    items.sort_by_key(|(o, _)| *o);

    let mut drawn = 0usize;
    let mut pngs: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    for (_, item) in items {
        match item {
            Item::Fill(f) => {
                set_rect(svg, f.clip, &mut open_rect);
                svg.push_str(&format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{}\"{}/>\n",
                    f.left,
                    f.top,
                    f.right - f.left,
                    f.bottom - f.top,
                    hex(f.rgb),
                    clip_attr(f.clip_path)
                ));
            }
            Item::Image(img) => {
                // SRCINVERT is an intermediate XOR pass in the common
                // SRCINVERT -> SRCAND -> SRCINVERT transparent-picture
                // sequence.  SVG cannot express that operation by placing
                // the source bitmap as an ordinary image; doing so can hide
                // text and vector artwork underneath it.  The PDF adapter
                // recognises the complete masked sequence; HTML currently
                // omits the unsupported intermediate pass instead.
                if img.raster_op == RasterOp::SourceInvert {
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
                set_rect(svg, img.clip, &mut open_rect);
                // A hair of overlap, so bands that abut show no seam.
                svg.push_str(&format!(
                    "<image x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" preserveAspectRatio=\"none\"{} xlink:href=\"{href}\"><title>{}</title></image>\n",
                    img.left,
                    img.top,
                    img.width() + 0.5,
                    img.height() + 0.5,
                    clip_attr(img.clip_path),
                    esc(t.art_alt)
                ));
                drawn += 1;
            }
            Item::Shape(sh) => {
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
                    "<path d=\"{}\" fill=\"{fill}\"{}{stroke}/>\n",
                    path_data(&sh.path),
                    if sh.path.even_odd {
                        " fill-rule=\"evenodd\""
                    } else {
                        ""
                    }
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
    (placed, drawn)
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
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
) -> Option<(usize, usize)> {
    const PT: f32 = 72.0 / 2540.0;
    let paper = p.paper.unwrap_or((21000, 29700));
    let (pw, ph) = (paper.0 as f32 * PT, paper.1 as f32 * PT);
    let mut svg = String::new();
    let mut spans = String::new();
    let initial_svg_len = svg.len();
    let initial_spans_len = spans.len();
    let (mut glyphs, mut pictures) = (0usize, 0usize);
    if let Some(m) = main {
        let stored: Vec<&Page> = doc.pictures_on(p.index).collect();
        let (g, d) = draw_metafile(
            m,
            data,
            &stored,
            Place::SHEET,
            "m",
            no,
            t,
            &mut svg,
            &mut spans,
        );
        glyphs += g;
        pictures += d;
    }
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
        let stored: &[&Page] = if overlay.area.is_none() { &group } else { &[] };
        let (g, d) = draw_metafile(
            &m,
            data,
            stored,
            place,
            &format!("o{n}"),
            no,
            t,
            &mut svg,
            &mut spans,
        );
        glyphs += g;
        pictures += d;
    }
    // A properties-only page can contain selectable text or vector shapes but
    // no picture.  Do not discard it merely because the picture count is zero.
    if main.is_none() && svg.len() == initial_svg_len && spans.len() == initial_spans_len {
        return None;
    }
    // The sheet keeps the stored proportions; a page shown turned is turned
    // by the browser around a wrapper of the shown proportions.
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
    Some((glyphs, pictures))
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
) -> Option<(usize, usize)> {
    const PT: f32 = 72.0 / 2540.0;
    let paper = display.paper.unwrap_or((21000, 29700));
    let (pw, ph) = (paper.0 as f32 * PT, paper.1 as f32 * PT);
    if pw <= 0.0 || ph <= 0.0 {
        return None;
    }
    let mut svg = String::new();
    let mut spans = String::new();
    let mut glyphs = 0usize;
    let mut pictures = 0usize;
    let mut recovered = false;

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
                svg.push_str(&format!(
                    "<img class=\"art\" style=\"left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%\" loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n",
                    place.x * 100.0,
                    place.y * 100.0,
                    place.w * 100.0,
                    place.h * 100.0,
                    esc(t.page_alt),
                    b64(&data[offset..offset + len]),
                ));
                pictures += 1;
                recovered = true;
            }
            PageData::Encoded { .. } | PageData::Preview { .. } if decode => {
                let Some(meta) = recovery::decode_page_for_document(data, page, doc, decoder)
                else {
                    continue;
                };
                let stored: Vec<&Page> = doc.pictures_on(page.index).collect();
                let (g, d) = draw_metafile_with_viewbox(
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
                glyphs += g;
                pictures += d;
                recovered |= g > 0 || d > 0;
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
                let stored: &[&Page] = if overlay.area.is_none() { &group } else { &[] };
                let (g, d) = draw_metafile(
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
                glyphs += g;
                pictures += d;
                recovered |= g > 0 || d > 0;
            }
        }
    }

    // The properties stream can explicitly retain a blank logical page even
    // when the page table has no body to attach to it.
    if display.members.is_empty() && display.overlays.is_empty() {
        recovered = true;
    }

    if !recovered && svg.is_empty() && spans.is_empty() {
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
    Some((glyphs, pictures))
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
    use super::{b64, build_with, Options};
    use crate::application::ports::PageDecoder;
    use crate::domain::page::{Overlay, Page, PageData, Role};
    use crate::domain::rendering::{Metafile, Text};
    use crate::domain::Document;

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
}
