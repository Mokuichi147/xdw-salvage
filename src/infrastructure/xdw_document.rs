//! XDWコンテナをドメインの文書モデルへ変換するパーサー。

use crate::domain::document::{
    DisplayMember, DisplayPage, Document, Rebuilt, SUPPORTED_GENERATIONS,
};
use crate::domain::page::{Page, PageData, PagePlacement, Role};
use crate::error::{Error, Result};
use crate::infrastructure::tlv::{self, Tlv};

const T_HEADER: u8 = 0x60;
const T_BODY: u8 = 0x61;
const T_PROPERTIES: u8 = 0x63;
const T_PAGE: u8 = 0x64;

const H_GENERATION: u8 = 0x82;
const H_GUARD: u8 = 0x83;

const TR_PAGE_COUNT: u8 = 0x80;
const TR_PAGE_OFFSETS: u8 = 0x81;
const TR_PROPS_EXPANDED: u8 = 0x83;
const TR_PROPS_STORED: u8 = 0x84;
const TR_CHECKSUM: u8 = 0x85;
const TR_SELF_LEN: u8 = 0x86;
const TR_SECURITY: u8 = 0x88;
const TR_IMAGE_PAGES: u8 = 0x8D;

/// XDWファイル全体を解析する。
pub fn parse(data: &[u8]) -> Result<Document> {
    let header = match tlv::read_one(data, 0) {
        Ok(h) if h.tag == T_HEADER => h,
        other => {
            // 既知形式なら、単に「コンテナではない」より具体的に報告する。
            if let Some(looks_like) = sniff(data) {
                return Err(Error::NotAContainer { looks_like });
            }
            other?;
            return Err(Error::NoFileHeader);
        }
    };
    let hf = tlv::read_window(data, header.value, header.len)?;
    let generation = tlv::find_uint(&hf, data, H_GENERATION).ok_or(Error::MissingField {
        tag: H_GENERATION,
        in_tag: T_HEADER,
    })? as u32;
    if !SUPPORTED_GENERATIONS.contains(&generation) {
        return Err(Error::UnsupportedGeneration(generation));
    }
    let mut guard = [0u8; 4];
    if let Some(g) = tlv::find(&hf, H_GUARD) {
        let b = g.bytes(data);
        for (i, v) in b.iter().take(4).enumerate() {
            guard[i] = *v;
        }
    }

    let (trailer, fields) = read_trailer(data)?;
    if generation == 11 && is_protected(&fields, data) {
        return Err(Error::ProtectedDocument);
    }
    let declared_entries = tlv::find_uint(&fields, data, TR_PAGE_COUNT).unwrap_or(0) as u32;
    let offsets = tlv::find(&fields, TR_PAGE_OFFSETS).map(|t| tlv::le_u32s(t.bytes(data)));

    // ページテーブルが壊れている場合はページ要素を直接スキャンして再構築する。
    let mut rebuilt = None;
    let mut pages = Vec::with_capacity(offsets.as_ref().map_or(0, Vec::len));
    if let Some(offsets) = offsets.as_ref() {
        for (i, &off) in offsets.iter().enumerate() {
            let off = off as usize;
            if off >= data.len() {
                rebuilt = Some(Rebuilt::OffsetOutsideFile);
                break;
            }
            match crate::infrastructure::xdw_page::read(data, i, off) {
                Ok(p) => pages.push(p),
                Err(_) => {
                    rebuilt = Some(Rebuilt::OffsetNotAPage);
                    break;
                }
            }
        }
        if rebuilt.is_some() {
            pages.clear();
            for (i, off) in scan_for_pages(data).into_iter().enumerate() {
                if let Ok(p) = crate::infrastructure::xdw_page::read(data, i, off) {
                    pages.push(p);
                }
            }
        }
    }
    assign_roles(&mut pages);

    let stored = tlv::find_uint(&fields, data, TR_PROPS_STORED).map(|v| v as u32);
    let expanded = tlv::find_uint(&fields, data, TR_PROPS_EXPANDED).map(|v| v as u32);
    let properties = stored.and_then(|s| locate_properties(data, trailer.start, s as usize));
    let image_derived = tlv::find(&fields, TR_IMAGE_PAGES)
        .map(|t| {
            tlv::le_u32s(t.bytes(data))
                .chunks(2)
                .filter_map(|c| c.first().copied())
                .collect()
        })
        .unwrap_or_default();

    // The properties block says how each page is shown: its paper as
    // displayed and any rotation. A page element carries neither for a
    // picture page, and the rotation for no page at all.
    let shown = if let (Some((at, stored)), Some(expanded)) = (properties, expanded) {
        if let Some(coded) = data.get(at..at + stored) {
            if let Ok(block) = crate::infrastructure::lzh::decode(coded, expanded as usize) {
                crate::infrastructure::xdw_properties::pages(&block)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    // A few exports retain the complete display description in the properties
    // block but omit or invalidate the page-offset field from their trailer.
    // In that case the properties are the only trustworthy page table:
    // preserve each described page as a synthetic sheet so its bitmap/text
    // overlays can still be rendered. No bytes outside the properties block
    // are guessed as page data.
    if pages.is_empty() {
        if shown.is_empty() {
            if offsets.is_none() {
                return Err(Error::MissingField {
                    tag: TR_PAGE_OFFSETS,
                    in_tag: trailer.tag,
                });
            }
        } else {
            pages = shown
                .iter()
                .enumerate()
                .map(|(index, info)| Page {
                    index,
                    role: Role::Sheet,
                    belongs_to: None,
                    offset: trailer.start,
                    checksum: None,
                    paper: info.paper,
                    pixels: None,
                    rotation: info.rotation,
                    overlays: info.overlays.clone(),
                    data: PageData::Bare {
                        offset: trailer.start,
                        len: 0,
                    },
                    unknown_fields: Vec::new(),
                })
                .collect();
        }
    }

    let display_pages = apply_display_layout(&mut pages, &shown);
    // Some saved-over documents leave a paper-sized, body-less page in the
    // page table for a stamp or another properties-only annotation.  It is
    // not a logical page of its own: the following recoverable body is the
    // page on which the annotation was placed.  Only apply this conservative
    // repair when the properties hierarchy was not usable, because a trusted
    // composed display page already carries its annotations explicitly.
    if display_pages.is_empty() {
        merge_orphan_overlay_sheets(&mut pages);
    }

    let (generations_present, unknown_tags) = survey(data);

    Ok(Document {
        generation,
        guard,
        trailer_tag: trailer.tag,
        trailer_at: trailer.start,
        declared_entries,
        pages,
        display_pages,
        properties,
        properties_len: match (stored, expanded) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        },
        image_derived,
        checksum: tlv::find_uint(&fields, data, TR_CHECKSUM).map(|v| v as u32),
        generations_present,
        unknown_tags,
        rebuilt,
    })
}

/// Move a properties-only annotation sheet onto the following page body.
///
/// A few old writers materialise an annotation as a `Bare` sheet between two
/// ordinary page-table entries.  Emitting that entry as a page produces a
/// stamp on an otherwise blank sheet and shifts every later page number.  The
/// body-less entry is safe to merge only when its paper and rotation agree
/// with the next recoverable sheet and it has no stored pictures of its own.
fn merge_orphan_overlay_sheets(pages: &mut [Page]) {
    let sheets: Vec<usize> = pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| page.is_sheet().then_some(index))
        .collect();
    for pair in sheets.windows(2) {
        let [orphan, target] = *pair else { continue };
        let is_orphan = matches!(pages[orphan].data, PageData::Bare { .. })
            && !pages[orphan].overlays.is_empty()
            && pages[orphan]
                .paper
                .zip(pages[target].paper)
                .is_some_and(|((ow, oh), (tw, th))| {
                    let close = |a: u32, b: u32| a.abs_diff(b) <= 100;
                    (close(ow, tw) && close(oh, th)) || (close(ow, th) && close(oh, tw))
                })
            && pages[orphan].rotation % 360 == pages[target].rotation % 360
            && !pages
                .iter()
                .any(|page| page.role == Role::Picture && page.belongs_to == Some(orphan));
        if !is_orphan || !pages[target].is_recoverable() {
            continue;
        }
        let overlays = std::mem::take(&mut pages[orphan].overlays);
        pages[target].overlays.extend(overlays);
        pages[orphan].role = Role::Data;
        pages[orphan].belongs_to = None;
    }
}

/// Apply the display hierarchy from the properties block to the page table.
///
/// A level-2 properties record is a logical displayed page.  Its level-3/4
/// children without a drawing field are references to page-table bodies; the
/// other level-4 children are inline annotation drawings.  The page table may
/// also contain old bodies left by a saved-over document, so references are
/// matched by their native frame when possible rather than blindly taking the
/// first entries.
fn apply_display_layout(
    pages: &mut [Page],
    shown: &[crate::infrastructure::xdw_properties::PageInfo],
) -> Vec<DisplayPage> {
    if shown.is_empty() {
        return Vec::new();
    }
    let sheet_indices: Vec<usize> = pages
        .iter()
        .enumerate()
        .filter_map(|(i, page)| page.is_sheet().then_some(i))
        .collect();
    let placements = shown
        .iter()
        .map(|info| info.placements.len())
        .sum::<usize>();
    let single_placement_pages = shown
        .iter()
        .filter(|info| info.placements.len() == 1)
        .count();
    let meaningful_sheets = sheet_indices
        .iter()
        .filter(|&&index| {
            !matches!(pages[index].data, PageData::Bare { .. })
                || pages[index].paper.is_some()
                || pages[index].pixels.is_some()
        })
        .count();
    // A saved-over file can retain opaque, old page bodies in the table.  A
    // single current properties page with one reference is still trustworthy
    // in that shape.  Conversely, when the properties contain more logical
    // pages than current page bodies, do not invent a repeated page layout.
    let trustworthy = shown.len() == sheet_indices.len()
        || (shown.len() == 1 && placements == sheet_indices.len())
        || (shown.len() == 1 && placements > 0 && meaningful_sheets <= placements)
        // A QR/split-print export can retain explicit blank logical pages and
        // place one current body on each of the remaining pages.  The page
        // table alone cannot express those blanks, but this one-to-one
        // placement pattern is unambiguous and matches the source order.
        || (placements == sheet_indices.len() && single_placement_pages == placements);
    if !trustworthy {
        apply_page_attributes(pages, shown);
        return Vec::new();
    }
    let mut used = vec![false; pages.len()];
    let mut result = Vec::with_capacity(shown.len());
    let mut fallback = 0usize;

    for info in shown {
        let mut members = Vec::new();
        for placement in &info.placements {
            let picked = pick_sheet(pages, &sheet_indices, &used, placement, fallback);
            let Some(index) = picked else { continue };
            used[index] = true;
            fallback = sheet_indices
                .iter()
                .position(|&candidate| candidate == index)
                .map_or(fallback, |position| position + 1);
            members.push(DisplayMember {
                page_index: index,
                area: Some(placement.area),
            });
        }
        // A normal one-sheet document can omit the child placement while its
        // properties still describe rotation and paper.  Keep that page in
        // the display model instead of making the layout appear empty.
        if members.is_empty() && shown.len() == sheet_indices.len() {
            if let Some(&index) = sheet_indices
                .iter()
                .skip(fallback)
                .find(|&&candidate| !used[candidate])
                .or_else(|| sheet_indices.iter().find(|&&candidate| !used[candidate]))
            {
                used[index] = true;
                members.push(DisplayMember {
                    page_index: index,
                    area: None,
                });
            }
        }

        if let Some(anchor) = members.first().map(|member| member.page_index) {
            let page = &mut pages[anchor];
            page.rotation = info.rotation;
            // Keep the page body's native frame on `Page`; the properties
            // paper belongs to `DisplayPage` and can be much larger for a
            // DocuMerge composition.
            page.overlays = info.overlays.clone();
        }

        // Saved-over documents can leave one trailing, attribute-less
        // properties record behind.  It has neither a canvas nor a body and
        // must not become a phantom blank output page.  Explicit blank pages
        // retain their paper field and therefore remain in the display model.
        if members.is_empty() && info.paper.is_none() && info.overlays.is_empty() {
            continue;
        }

        result.push(DisplayPage {
            paper: resolved_display_paper(info.paper, info.rotation, pages, &members),
            rotation: info.rotation,
            overlays: info.overlays.clone(),
            members,
        });
    }
    result
}

/// 表示属性の用紙寸法は、保存形式や出力元によって整数ミリ単位へ丸められる
/// ことがある。本文ページが1枚だけの通常ページなら、そのページ本体の寸法を
/// 優先してPDF/HTMLの用紙サイズを保つ。複数本体を合成する表示ページでは、
/// 親の用紙が本来のキャンバスなので、プロパティの寸法をそのまま使う。
fn resolved_display_paper(
    declared: Option<(u32, u32)>,
    rotation: u16,
    pages: &[Page],
    members: &[DisplayMember],
) -> Option<(u32, u32)> {
    if members.len() != 1 {
        return declared;
    }
    let native = pages
        .get(members[0].page_index)
        .and_then(|page| page.paper)
        .map(|(w, h)| if rotation % 180 == 90 { (h, w) } else { (w, h) });
    let Some(native) = native else {
        return declared;
    };
    let close = |a: u32, b: u32| a.abs_diff(b) <= 100;
    match declared {
        Some((w, h)) if close(w, native.0) && close(h, native.1) => Some(native),
        Some(_) => declared,
        None => Some(native),
    }
}

/// Preserve the historical one-to-one properties mapping when a container's
/// saved-over hierarchy is too ambiguous to compose safely.
fn apply_page_attributes(
    pages: &mut [Page],
    shown: &[crate::infrastructure::xdw_properties::PageInfo],
) {
    let mut sheets = pages.iter_mut().filter(|page| page.is_sheet());
    for info in shown {
        let Some(page) = sheets.next() else { break };
        page.rotation = info.rotation;
        if info.paper.is_some() {
            page.paper = info.paper;
        }
        page.overlays = info.overlays.clone();
    }
}

/// Pick the not-yet-used page body that best matches a properties child.
fn pick_sheet(
    pages: &[Page],
    sheet_indices: &[usize],
    used: &[bool],
    placement: &PagePlacement,
    fallback: usize,
) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for &index in sheet_indices {
        if used[index] {
            continue;
        }
        let score = geometry_score(&pages[index], placement.frame);
        if best.is_none_or(|(old, _)| score < old) {
            best = Some((score, index));
        }
    }
    best.map(|(_, index)| index).or_else(|| {
        sheet_indices
            .iter()
            .skip(fallback)
            .copied()
            .find(|&index| !used[index])
    })
}

/// Compare a page body's declared geometry with a properties child frame.
fn geometry_score(page: &Page, frame: Option<(u32, u32)>) -> f32 {
    let Some((fw, fh)) = frame else { return 0.0 };
    if fw == 0 || fh == 0 {
        return 100.0;
    }
    let dims = page.paper.or(page.pixels);
    let Some((pw, ph)) = dims else { return 100.0 };
    let ratio_error = |a: f32, b: f32| ((a / b).ln()).abs();
    let normal = ratio_error(pw as f32, fw as f32) + ratio_error(ph as f32, fh as f32);
    let turned = ratio_error(pw as f32, fh as f32) + ratio_error(ph as f32, fw as f32);
    normal.min(turned)
}

/// XDWコンテナでないファイルの既知形式を判定する。
pub fn sniff(data: &[u8]) -> Option<&'static str> {
    let starts = |m: &[u8]| data.len() >= m.len() && &data[..m.len()] == m;
    if starts(b"%PDF-") {
        return Some("a PDF");
    }
    if starts(b"PK\x03\x04") {
        // Office形式は最初のエントリ名に形式を含む。
        let head = &data[..data.len().min(512)];
        let find = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
        if find(b"word/") {
            return Some("a Word document (.docx)");
        }
        if find(b"xl/") {
            return Some("an Excel workbook (.xlsx)");
        }
        if find(b"ppt/") {
            return Some("a PowerPoint file (.pptx)");
        }
        return Some("a zip archive");
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Some("an older Office document (compound file)");
    }
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Some("a JPEG image");
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some("a PNG image");
    }
    if starts(b"II*\x00") || starts(b"MM\x00*") {
        return Some("a TIFF image");
    }
    if starts(b"{\rtf") {
        return Some("an RTF document");
    }
    if starts(b"<!DOCTYPE") || starts(b"<html") || starts(b"<?xml") {
        return Some("markup, not a document container");
    }
    None
}

/// ページテーブルが使えないコンテナからページ要素を直接探す。
fn scan_for_pages(data: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 < data.len() {
        if data[i] == T_PAGE {
            if let Ok((len, value)) = tlv::read_len(data, i + 1) {
                let fits = value + len <= data.len() && len > 10;
                let shaped = value + 7 <= data.len()
                    && data[value] == 0x81
                    && data[value + 1] == 0x04
                    && data[value + 6] == 0x82;
                if fits && shaped {
                    out.push(i);
                    i = value + len;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// サムネイルを区切りとして、ページテーブル内エントリの役割を確定する。
fn assign_roles(pages: &mut [Page]) {
    let mut group: Vec<usize> = Vec::new();
    let mut after_thumbnail = false;
    for i in 0..pages.len() {
        if matches!(pages[i].data, PageData::Fields { .. }) {
            pages[i].role = Role::Data;
            pages[i].belongs_to = None;
            after_thumbnail = false;
            continue;
        }
        if pages[i].is_full_size_preview() {
            // Some old image documents put a page-sized, method-5 preview
            // between the preceding thumbnail and the next one.  It is the
            // page body, not another thumbnail.  Close the preceding group
            // before starting it so the following small preview attaches to
            // this page.
            close_group(pages, &group, false);
            pages[i].role = Role::Sheet;
            pages[i].belongs_to = None;
            group.clear();
            // Keep the body in the current group until its following
            // thumbnail is seen.  Unlike an encoded body, this entry arrived
            // on the preview branch itself, so clearing the group here would
            // make the thumbnail owner impossible to discover.
            group.push(i);
            after_thumbnail = false;
        } else if matches!(pages[i].data, PageData::Preview { .. }) {
            // A small preview is normally the thumbnail belonging to the
            // group immediately before it.  Some writers, however, omit the
            // coded page body and leave only a standalone preview.  Decide
            // which case this is after closing the preceding group; otherwise
            // the standalone image is silently discarded as an orphaned
            // thumbnail.
            close_group(pages, &group, false);
            let owner = group
                .iter()
                .rev()
                .find(|&&k| pages[k].role == Role::Sheet)
                .map(|&k| pages[k].index);
            if let Some(owner) = owner {
                pages[i].role = Role::Thumbnail;
                pages[i].belongs_to = Some(owner);
                after_thumbnail = true;
            } else {
                // Do not put this page in `group`: two consecutive standalone
                // previews must become two pages, not one page plus a
                // thumbnail attached to the other.
                pages[i].role = Role::Sheet;
                pages[i].belongs_to = None;
                after_thumbnail = false;
            }
            group.clear();
        } else {
            group.push(i);
        }
    }
    close_group(pages, &group, after_thumbnail);
}

/// サムネイル間にある一続きのエントリの役割を確定する。
fn close_group(pages: &mut [Page], group: &[usize], after_thumbnail: bool) {
    if group.is_empty() {
        return;
    }
    let is_picture = |p: &Page| matches!(p.data, PageData::Jpeg { .. } | PageData::Bare { .. });
    let group: Vec<usize> = group
        .iter()
        .copied()
        .filter(|&i| pages[i].role != Role::Data)
        .collect();
    let group = &group[..];
    if group.is_empty() {
        return;
    }
    // 用紙・画素のどちらの形状情報もないBareエントリは、復元対象ページでは
    // なくコンテナ内の不透明データである。特にプレビュー後の末尾エントリは
    // 埋め込み文書やアプリケーション状態を持つことがあり、ページテーブルに
    // 列挙されているだけで余分な本文ページにしてはならない。
    let opaque_data = after_thumbnail
        && group.iter().all(|&i| {
            matches!(pages[i].data, PageData::Bare { .. })
                && pages[i].paper.is_none()
                && pages[i].pixels.is_none()
        });
    if opaque_data {
        for &i in group {
            pages[i].role = Role::Data;
            pages[i].belongs_to = None;
        }
        return;
    }

    // A nested kind-5 JPEG carries its paper size in the page body.  It is a
    // complete image page, not one of the paperless JPEG entries that a
    // printer driver stores as artwork beside a coded sheet.  When several
    // such pages are consecutive (some writers omit the thumbnail between
    // them), treating the largest one as the sheet would append every later
    // page to the first and would also make the artwork contact-sheet layout
    // rotate it to fit.  Honour the explicit page geometry as a boundary and
    // leave only paperless images to the artwork heuristic below.
    let explicit_image_sheets: Vec<usize> = group
        .iter()
        .copied()
        .filter(|&i| matches!(pages[i].data, PageData::Jpeg { .. }) && pages[i].paper.is_some())
        .collect();
    if !explicit_image_sheets.is_empty() {
        let mut current: Option<usize> = None;
        for &i in group {
            let is_sheet = explicit_image_sheets.contains(&i) || !is_picture(&pages[i]);
            if is_sheet {
                pages[i].role = Role::Sheet;
                pages[i].belongs_to = None;
                current = Some(pages[i].index);
            } else {
                pages[i].role = Role::Picture;
                pages[i].belongs_to = current;
            }
        }
        let owner = explicit_image_sheets
            .first()
            .copied()
            .map(|i| pages[i].index);
        for &i in group {
            if pages[i].role == Role::Picture && pages[i].belongs_to.is_none() {
                pages[i].belongs_to = owner;
            }
        }
        return;
    }

    // コンテナは本文とその上の画像を順に書く。本文より前の画像は、本文に
    // 配置された画像ではなく、画像からインポートされた本文として扱う。
    let first_coded = group.iter().position(|&i| !is_picture(&pages[i]));

    // コード化された本文がない場合は、最大の画像を本文と推定する。
    let implied_sheet = if first_coded.is_some() {
        None
    } else {
        group
            .iter()
            .copied()
            .max_by_key(|&i| {
                let (w, h) = pages[i].pixels.unwrap_or((0, 0));
                (w as u64) * (h as u64)
            })
            .or_else(|| group.first().copied())
    };

    let mut current: Option<usize> = None;
    for (pos, &i) in group.iter().enumerate() {
        let before_the_sheet = first_coded.is_some_and(|f| pos < f);
        let sheet = !is_picture(&pages[i]) || implied_sheet == Some(i) || before_the_sheet;
        if sheet {
            pages[i].role = Role::Sheet;
            pages[i].belongs_to = None;
            current = Some(pages[i].index);
        } else {
            pages[i].role = Role::Picture;
            pages[i].belongs_to = current;
        }
    }
    let owner = group
        .iter()
        .find(|&&i| pages[i].role == Role::Sheet)
        .map(|&i| pages[i].index);
    for &i in group {
        if pages[i].role == Role::Picture && pages[i].belongs_to.is_none() {
            pages[i].belongs_to = owner;
        }
    }
}

/// ファイル末尾からトレーラー要素を探す。
fn read_trailer(data: &[u8]) -> Result<(Tlv, Vec<Tlv>)> {
    let n = data.len();
    if n < 8 || data[n - 6] != TR_SELF_LEN || data[n - 5] != 0x04 {
        return Err(Error::NoTrailer);
    }
    let len = u32::from_le_bytes([data[n - 4], data[n - 3], data[n - 2], data[n - 1]]) as usize;
    // 長さヘッダーは2〜5オクテットなので、末尾に正確に着地するものを探す。
    for hdr in 2..=5usize {
        let Some(start) = n.checked_sub(len + hdr) else {
            continue;
        };
        let Ok((l, value)) = tlv::read_len(data, start + 1) else {
            continue;
        };
        if l == len && value == start + hdr {
            let t = Tlv {
                tag: data[start],
                start,
                value,
                len,
            };
            let fields = tlv::read_window(data, value, len)?;
            return Ok((t, fields));
        }
    }
    Err(Error::NoTrailer)
}

/// トレーラー直前にあるプロパティブロックを探す。
fn locate_properties(data: &[u8], trailer_at: usize, stored: usize) -> Option<(usize, usize)> {
    for hdr in 2..=5usize {
        let start = trailer_at.checked_sub(stored + hdr)?;
        if data.get(start) != Some(&T_PROPERTIES) {
            continue;
        }
        if let Ok((l, value)) = tlv::read_len(data, start + 1) {
            if l == stored && value == start + hdr {
                return Some((value, stored));
            }
        }
    }
    None
}

/// 世代11の保護コンテナに付くセキュリティ記述を判定する。
///
/// The security payload is deliberately not interpreted: the clear marker is
/// enough to distinguish an authorized, ordinary generation-11 container from
/// a document whose pages are encrypted or signature-bound.
fn is_protected(fields: &[Tlv], data: &[u8]) -> bool {
    tlv::find(fields, TR_SECURITY).is_some_and(|t| t.bytes(data).starts_with(b"SECU"))
}

/// 文書世代数と未解釈要素を集計する。
fn survey(data: &[u8]) -> (usize, Vec<(u8, usize, usize)>) {
    let mut generations = 0usize;
    let mut unknown = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let Ok(t) = tlv::read_one(data, i) else { break };
        if t.tag == T_BODY {
            generations += 1;
            if let Ok(items) = tlv::read_window(data, t.value, t.len) {
                for it in items {
                    let known =
                        matches!(it.tag, T_PAGE | T_PROPERTIES) || it.tag == 0x65 || it.tag == 0x68;
                    if !known {
                        unknown.push((it.tag, it.start, it.len));
                    }
                }
            }
        }
        i = t.end();
    }
    (generations.max(1), unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(index: usize, paper: (u32, u32)) -> Page {
        Page {
            index,
            role: Role::Sheet,
            belongs_to: None,
            offset: 0,
            checksum: None,
            paper: Some(paper),
            pixels: None,
            rotation: 0,
            overlays: Vec::new(),
            data: PageData::Bare { offset: 0, len: 0 },
            unknown_fields: Vec::new(),
        }
    }

    fn preview(index: usize) -> Page {
        Page {
            index,
            role: Role::Data,
            belongs_to: None,
            offset: 0,
            checksum: None,
            paper: None,
            pixels: Some((160, 120)),
            rotation: 0,
            overlays: Vec::new(),
            data: PageData::Preview {
                offset: 0,
                len: 0,
                pixels_at: 0,
                bpp: 1,
                palette_colours: 2,
                stored: 1,
                expanded: 1,
                rows: 1,
                method: 1,
            },
            unknown_fields: Vec::new(),
        }
    }

    fn annotation() -> crate::domain::page::Overlay {
        crate::domain::page::Overlay {
            kind: 1,
            expanded: 0,
            coded: Vec::new(),
            pixels: None,
            area: Some((100, 200, 300, 400)),
        }
    }

    #[test]
    fn standalone_previews_are_kept_as_pages() {
        let mut pages = vec![preview(0), preview(1)];

        assign_roles(&mut pages);

        assert!(pages.iter().all(|page| page.role == Role::Sheet));
        assert!(pages.iter().all(|page| page.belongs_to.is_none()));
    }

    #[test]
    fn preview_after_a_body_is_a_thumbnail_but_next_orphan_is_a_page() {
        let mut pages = vec![sheet(0, (21000, 29700)), preview(1), preview(2)];

        assign_roles(&mut pages);

        assert_eq!(pages[0].role, Role::Sheet);
        assert_eq!(pages[1].role, Role::Thumbnail);
        assert_eq!(pages[1].belongs_to, Some(0));
        assert_eq!(pages[2].role, Role::Sheet);
        assert_eq!(pages[2].belongs_to, None);
    }

    #[test]
    fn properties_only_sheet_is_merged_into_the_following_body() {
        let mut pages = vec![sheet(0, (21000, 29700)), sheet(1, (21000, 29700))];
        pages[0].overlays.push(annotation());
        pages[1].data = PageData::Encoded {
            offset: 0,
            len: 1,
            kind_code: 9,
            aux_len: None,
            method: None,
            colour: None,
        };

        merge_orphan_overlay_sheets(&mut pages);

        assert!(pages[0].overlays.is_empty());
        assert_eq!(pages[0].role, Role::Data);
        assert_eq!(pages[1].overlays, vec![annotation()]);
    }

    #[test]
    fn rounded_display_paper_uses_the_native_single_sheet_size() {
        let pages = vec![sheet(0, (21590, 27940))];
        let members = vec![DisplayMember {
            page_index: 0,
            area: None,
        }];
        assert_eq!(
            resolved_display_paper(Some((21600, 27900)), 0, &pages, &members),
            Some((21590, 27940))
        );
    }

    #[test]
    fn rotated_display_paper_follows_the_native_orientation() {
        let pages = vec![sheet(0, (29700, 42000))];
        let members = vec![DisplayMember {
            page_index: 0,
            area: None,
        }];
        assert_eq!(
            resolved_display_paper(Some((42000, 29700)), 90, &pages, &members),
            Some((42000, 29700))
        );
    }

    #[test]
    fn composed_display_paper_keeps_the_parent_canvas() {
        let pages = vec![sheet(0, (10000, 10000)), sheet(1, (10000, 10000))];
        let members = vec![
            DisplayMember {
                page_index: 0,
                area: None,
            },
            DisplayMember {
                page_index: 1,
                area: None,
            },
        ];
        assert_eq!(
            resolved_display_paper(Some((42000, 29700)), 0, &pages, &members),
            Some((42000, 29700))
        );
    }
}
