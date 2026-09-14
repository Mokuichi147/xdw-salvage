//! A single self-contained HTML page per document.
//!
//! Everything travels inside the one file: recovered page images as data URLs,
//! any original file found in the container offered for download, and a card in
//! place of every page whose image is in the vendor's coding, so the reading
//! order still matches the original document.

use crate::application::ports::{AttachmentScanner, PageDecoder};
use crate::application::recovery;
use crate::domain::output::Language as Lang;
use crate::domain::page::{Page, PageData};
use crate::domain::rendering::Metafile;
use crate::domain::Document;
use crate::infrastructure::{LzhMetafileDecoder, MagicAttachmentScanner};

/// Settings for [`build`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Expand pages held in the container's own coding and lay out the text
    /// from the metafile inside.
    pub decode: bool,
    /// Language of the text this crate writes into the page.
    pub lang: Lang,
    /// Shown as the document heading and in the browser tab.
    pub title: Option<String>,
    /// Include preview entries. Off by default; they are low resolution copies
    /// of other pages and they are in the vendor coding, so they arrive as
    /// cards rather than pictures.
    pub include_previews: bool,
    /// Carry original files found in the container as download links.
    pub embed_originals: bool,
    /// Leave out the pages that could not be recovered, rather than marking
    /// them. Page numbering then no longer matches the original.
    pub skip_missing: bool,
    /// Offer the whole source document for download from the page.
    ///
    /// For an archive where most pages are in the vendor coding, a page of
    /// empty cards is not a migration on its own. With the source carried
    /// inside it, the file is at least a strict superset of the one it came
    /// from. It roughly doubles the output, so it is off by default.
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
    // One card per sheet. Thumbnails and the pictures a sheet is made of are
    // not sheets; listing them as pages would misreport the document's length.
    let selected: Vec<&Page> = doc
        .pages
        .iter()
        .filter(|p| p.is_sheet() || (opts.include_previews && p.is_preview()))
        .collect();

    let title = opts.title.clone().unwrap_or_else(|| "document".into());
    let t = Text::for_lang(opts.lang);

    let mut body = String::new();
    let mut gaps: Vec<usize> = Vec::new();
    for (i, p) in selected.iter().enumerate() {
        let no = i + 1;
        let decoded = opts
            .decode
            .then(|| recovery::decode_page(data, p, decoder))
            .flatten();
        match p.data {
            PageData::Jpeg { offset, len } if offset + len <= data.len() => {
                report.embedded += 1;
                let (w, h) = p.pixels.unwrap_or((0, 0));
                body.push_str(&format!(
                    "<figure class=\"page\" id=\"p{no}\">\n<img loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n<figcaption>{} &middot; {w}&times;{h}px</figcaption>\n</figure>\n",
                    esc(&t.page_alt(no)),
                    b64(&data[offset..offset + len]),
                    esc(&t.page_label(no)),
                ));
            }
            // The sheet is in the container's own coding. Expand it and draw
            // the metafile's text: the page comes back as real, selectable
            // text rather than a note saying it could not be read. A sheet
            // that does not expand falls through to the arm below, which still
            // shows whatever artwork sits on it.
            PageData::Encoded { .. } if decoded.is_some() => {
                let m = decoded.as_ref().expect("decoded guard above");
                report.embedded += 1;
                let (placed, drawn) = draw_text(m, data, doc, p.index, no, &t, &mut body);
                report.glyphs += placed;
                report.pictures += drawn;
            }
            _ => {
                if opts.skip_missing {
                    report.skipped += 1;
                    continue;
                }
                report.gaps += 1;
                gaps.push(no);
                let size = p
                    .paper
                    .map(|(w, h)| {
                        format!("{:.0}&times;{:.0} mm", w as f32 / 100.0, h as f32 / 100.0)
                    })
                    .unwrap_or_else(|| "&mdash;".into());
                // The sheet itself could not be expanded, but the pictures on
                // it are plain JPEG. Show them: a page comes back as its
                // artwork instead of an empty box.
                let (mut art, count) = artwork(data, doc, p.index, no, &t);
                let images = usize::from(!art.is_empty());
                report.pictures += count;
                if images > 0 {
                    art = format!(
                        "<p class=\"why art-note\">{}</p>\n<div class=\"arts\">\n{art}</div>\n",
                        esc(&t.art_note(images))
                    );
                }
                body.push_str(&format!(
                    "<section class=\"page gap\" id=\"p{no}\">\n<p class=\"no\">{}</p>\n<p class=\"why\">{}</p>\n<p class=\"meta\">{size} &middot; {}</p>\n{art}</section>\n",
                    esc(&t.page_label(no)),
                    esc(t.gap_why),
                    esc(p.kind_name()),
                ));
            }
        }
    }
    // Empty either because the document holds nothing, or because everything it
    // holds was dropped. A page that just stops without saying so is worse than
    // either, so say so.
    if body.is_empty() {
        body.push_str(&format!(
            "<section class=\"page gap\"><p class=\"why\">{}</p></section>\n",
            esc(t.empty)
        ));
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
                "<li><a download=\"{}\" href=\"data:application/octet-stream;base64,{}\">{}</a> <span class=\"meta\">{} &middot; {} B</span></li>\n",
                esc(&name),
                b64(&data[a.offset..a.offset + a.len]),
                esc(&name),
                esc(a.kind.label()),
                a.len,
            ));
        }
    }

    if opts.carry_source {
        report.attachments += 1;
        let name = format!("{}.xdw", stem(&title));
        files.push_str(&format!(
            "<li><a download=\"{}\" href=\"data:application/octet-stream;base64,{}\">{}</a> <span class=\"meta\">{} &middot; {} B</span></li>\n",
            esc(&name),
            b64(data),
            esc(&name),
            esc(t.source),
            data.len(),
        ));
    }

    let mut head_note = format!(
        "{} &middot; {}",
        t.count(report.embedded, selected.len()),
        esc(&title)
    );
    if report.gaps > 0 {
        head_note.push_str(&format!(" &middot; {}", t.gap_count(report.gaps)));
    }

    let mut index = String::new();
    if !gaps.is_empty() {
        index.push_str(&format!("<p class=\"gaps\"><b>{}</b> ", esc(t.gap_list)));
        for (k, n) in gaps.iter().enumerate() {
            if k == 40 {
                index.push_str(&format!("&hellip; (+{})", gaps.len() - 40));
                break;
            }
            index.push_str(&format!("<a href=\"#p{n}\">{n}</a> "));
        }
        index.push_str("</p>\n");
    }
    if !files.is_empty() {
        index.push_str(&format!(
            "<p class=\"gaps\"><b>{}</b></p>\n<ul class=\"files\">\n{files}</ul>\n",
            esc(t.originals)
        ));
    }

    let html = format!(
        "<!doctype html>\n<html lang=\"{lang}\">\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{title_esc}</title>\n<style>{CSS}</style>\n<header>\n<h1>{title_esc}</h1>\n<p class=\"meta\">{head_note}</p>\n{index}</header>\n<main>\n{body}</main>\n<footer><p class=\"meta\">{footer}</p></footer>\n</html>\n",
        lang = t.lang_attr,
        title_esc = esc(&title),
        footer = esc(t.footer),
    );
    (html, report)
}

const CSS: &str = "\
:root{color-scheme:light dark;--bg:#f7f7f5;--fg:#1b1b1a;--dim:#6b6b68;--line:#d8d8d4;--card:#fff}\
@media(prefers-color-scheme:dark){:root{--bg:#16161a;--fg:#e9e9e6;--dim:#9a9a96;--line:#33333a;--card:#1e1e23}}\
*{box-sizing:border-box}\
body{margin:0;padding:0 16px 48px;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,-apple-system,'Segoe UI',sans-serif}\
header,main,footer{max-width:960px;margin:0 auto}\
header{padding:28px 0 16px;border-bottom:1px solid var(--line)}\
h1{margin:0 0 6px;font-size:1.35rem;word-break:break-all}\
.meta{color:var(--dim);font-size:.85rem;margin:4px 0}\
.sheet{position:relative;width:100%;background:#fff;overflow:hidden;container-type:size}\
.sheet .pic{position:absolute;object-fit:fill}\
.sheet i{position:absolute;display:block}\
.sheet span{position:absolute;white-space:pre;line-height:1;font-family:\"Hiragino Kaku Gothic ProN\",\"Yu Gothic\",\"Meiryo\",\"Noto Sans JP\",sans-serif}\
.gaps{margin:10px 0 0;font-size:.85rem}\
.gaps a{display:inline-block;padding:0 4px;color:inherit}\
.files{margin:6px 0 0;padding-left:18px;font-size:.85rem}\
main{padding-top:24px}\
.page{margin:0 0 28px;background:var(--card);border:1px solid var(--line);border-radius:6px;padding:12px}\
.page img{display:block;width:100%;height:auto;border-radius:3px}\
figcaption{color:var(--dim);font-size:.8rem;padding-top:8px}\
.gap{border-style:dashed;text-align:center;padding:28px 12px}\
.arts{display:flex;flex-direction:column;gap:6px;margin-top:14px}\
.art{margin:0;position:relative}\
.bands{display:flex;flex-direction:column;line-height:0;border-radius:3px;overflow:hidden}\
.art img{display:block;width:100%;height:auto}\
.art figcaption{color:var(--dim);font-size:.68rem;text-align:right;padding-top:2px;opacity:.65}\
.art-note{margin-top:12px!important;font-size:.8rem}\
.gap .no{margin:0 0 8px;font-weight:600}\
.gap .why{margin:0;color:var(--dim);font-size:.9rem}\
footer{padding-top:20px;border-top:1px solid var(--line)}\
@media print{body{background:#fff}.page{break-inside:avoid;border:none;padding:0}}\
";

/// Every string this crate writes into the page, in one place.
struct Text {
    lang_attr: &'static str,
    gap_why: &'static str,
    art_one: &'static str,
    bands_one: &'static str,
    gap_list: &'static str,
    originals: &'static str,
    source: &'static str,
    empty: &'static str,
    footer: &'static str,
    ja: bool,
}

impl Text {
    fn for_lang(lang: Lang) -> Text {
        match lang {
            Lang::Japanese => Text {
                lang_attr: "ja",
                gap_why: "ページ画像がコンテナ独自の符号化で格納されており、復元できません。",
                art_one: "このページから取り出せた画像",
                bands_one: "帯を連結",
                gap_list: "復元できなかったページ:",
                originals: "このページに入っているファイル",
                source: "変換元",
                empty: "この文書から復元できるページはありません。",
                footer: "xdw-salvage が生成。FUJIFILM Business Innovation とは無関係の非公式ツールです。",
                ja: true,
            },
            Lang::English => Text {
                lang_attr: "en",
                gap_why: "The page image is stored in the container's own coding and was not recovered.",
                art_one: "Artwork recovered from this page",
                bands_one: "bands joined",
                gap_list: "Pages not recovered:",
                originals: "Files carried in this page",
                source: "source document",
                empty: "No page of this document could be recovered.",
                footer: "Produced by xdw-salvage, an unofficial tool, not affiliated with FUJIFILM Business Innovation.",
                ja: false,
            },
        }
    }

    fn page_label(&self, n: usize) -> String {
        if self.ja {
            format!("{n} ページ")
        } else {
            format!("Page {n}")
        }
    }

    fn page_alt(&self, n: usize) -> String {
        self.page_label(n)
    }

    fn art_alt(&self, n: usize) -> String {
        if self.ja {
            format!("{n} ページから取り出した画像")
        } else {
            format!("Artwork from page {n}")
        }
    }

    fn bands(&self, n: usize) -> String {
        if self.ja {
            format!("{} {n} 枚", self.bands_one)
        } else {
            format!("{n} {}", self.bands_one)
        }
    }

    /// The pictures are shown in container order, not where they sat on the
    /// page: their positions live inside the coding this crate does not read.
    fn art_note(&self, n: usize) -> String {
        if self.ja {
            format!("{} — {n} 点（元の配置・向きではありません）", self.art_one)
        } else {
            format!(
                "{} — {n} item(s), not in their original positions",
                self.art_one
            )
        }
    }

    fn count(&self, got: usize, total: usize) -> String {
        if self.ja {
            format!("{total} ページ中 {got} ページを復元")
        } else {
            format!("{got} of {total} pages recovered")
        }
    }

    fn gap_count(&self, n: usize) -> String {
        if self.ja {
            format!("{n} ページは未復元")
        } else {
            format!("{n} not recovered")
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
    let mut chunks = data.chunks_exact(3);
    for c in &mut chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
        }
    }
    match chunks.remainder() {
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

/// Lay a metafile's page out as absolutely positioned elements.
///
/// The page keeps its real proportions and everything keeps the position the
/// metafile recorded, so the result reads like the page rather than like a list
/// of the words on it. Sizes are in units of the sheet, which means the page
/// stays right at any width.
///
/// Returns the characters placed and the pictures drawn.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    m: &Metafile,
    data: &[u8],
    doc: &Document,
    sheet: usize,
    no: usize,
    t: &Text,
    body: &mut String,
) -> (usize, usize) {
    let (ux, uy) = m.units_per_point();
    let (pw, ph) = m.points();
    if !(ux.is_finite() && uy.is_finite()) || ux <= 0.0 || uy <= 0.0 || pw <= 0.0 || ph <= 0.0 {
        return (0, 0);
    }
    body.push_str(&format!(
        "<figure class=\"page\" id=\"p{no}\">\n<div class=\"sheet\" style=\"aspect-ratio:{:.4}\">\n",
        pw / ph
    ));

    // Pictures go down first: the metafile draws them under everything else.
    let pictures: Vec<&Page> = doc.pictures_on(sheet).collect();
    let mut drawn = 0usize;
    for img in &m.images {
        let Some(pic) = pictures.iter().find(|p| p.pixels == Some(img.src)) else {
            continue;
        };
        let PageData::Jpeg { offset, len } = pic.data else {
            continue;
        };
        if offset + len > data.len() {
            continue;
        }
        body.push_str(&format!(
            "<img class=\"pic\" alt=\"{}\" style=\"left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%\" src=\"data:image/jpeg;base64,{}\">\n",
            esc(&t.art_alt(no)),
            img.left / ux / pw * 100.0,
            img.top / uy / ph * 100.0,
            img.width() / ux / pw * 100.0,
            img.height() / uy / ph * 100.0,
            b64(&data[offset..offset + len]),
        ));
        drawn += 1;
    }

    // Rules and blocks of colour.
    for f in &m.fills {
        if f.clipped {
            continue;
        }
        body.push_str(&format!(
            "<i style=\"left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%;background:#{:02x}{:02x}{:02x}\"></i>",
            f.left / ux / pw * 100.0,
            f.top / uy / ph * 100.0,
            (f.right - f.left) / ux / pw * 100.0,
            (f.bottom - f.top) / uy / ph * 100.0,
            f.rgb.0,
            f.rgb.1,
            f.rgb.2
        ));
    }

    let mut placed = 0usize;
    for run in &m.text {
        if run.chars.iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let size = run.size / uy;
        let top = (run.y / uy - size) / ph * 100.0;
        let (r, g, b) = run.rgb;
        let colour = if (r, g, b) == (0, 0, 0) {
            String::new()
        } else {
            format!("color:#{r:02x}{g:02x}{b:02x};")
        };
        // Turned text is rotated about its own start, as the metafile means it.
        let turn = if run.escapement != 0 {
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
            let left = run.xs.get(i).copied().unwrap_or(0.0) / ux / pw * 100.0;
            if !left.is_finite() || !top.is_finite() {
                continue;
            }
            body.push_str(&format!(
                "<span style=\"left:{left:.3}%;top:{top:.3}%;font-size:{:.3}cqh;{colour}{turn}\">{}</span>",
                size / ph * 100.0,
                esc(&c.to_string())
            ));
            placed += 1;
        }
        body.push('\n');
    }
    body.push_str(&format!(
        "</div>\n<figcaption>{}</figcaption>\n</figure>\n",
        esc(&t.page_label(no))
    ));
    (placed, drawn)
}

/// The plain-JPEG artwork that belongs to one sheet, as figures.
fn artwork(data: &[u8], doc: &Document, index: usize, no: usize, t: &Text) -> (String, usize) {
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
        let w = run[0].pixels.map(|(w, _)| w).unwrap_or(0);
        let h: u32 = run
            .iter()
            .map(|pic| pic.pixels.map(|(_, h)| h).unwrap_or(0))
            .sum();
        art.push_str("<figure class=\"art\"><div class=\"bands\">\n");
        for (offset, len) in &bands {
            art.push_str(&format!(
                "<img loading=\"lazy\" alt=\"{}\" src=\"data:image/jpeg;base64,{}\">\n",
                esc(&t.art_alt(no)),
                b64(&data[*offset..*offset + *len]),
            ));
        }
        art.push_str(&format!(
            "</div><figcaption>{w}&times;{h}px{}</figcaption></figure>\n",
            if bands.len() > 1 {
                format!(" &middot; {}", t.bands(bands.len()))
            } else {
                String::new()
            }
        ));
    }
    (art, count)
}

#[cfg(test)]
mod tests {
    use super::b64;

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
}
