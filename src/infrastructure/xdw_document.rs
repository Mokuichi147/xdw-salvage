//! XDWコンテナをドメインの文書モデルへ変換するパーサー。

use crate::domain::document::{Document, Rebuilt, SUPPORTED_GENERATIONS};
use crate::domain::page::{Page, PageData, Role};
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
    let mut sheets = pages.iter_mut().filter(|p| p.is_sheet());
    for info in &shown {
        let Some(sheet) = sheets.next() else { break };
        sheet.rotation = info.rotation;
        if sheet.paper.is_none() {
            sheet.paper = info.paper;
        }
        sheet.overlays = info.overlays.clone();
    }
    // A few older exports retain the complete display description in the
    // properties block but omit the page-offset field from their trailer. In
    // that case the properties are the only trustworthy page table: preserve
    // each described page as a synthetic sheet so its bitmap/text overlays can
    // still be rendered. No bytes outside the properties block are guessed as
    // page data.
    if pages.is_empty() && offsets.is_none() {
        if shown.is_empty() {
            return Err(Error::MissingField {
                tag: TR_PAGE_OFFSETS,
                in_tag: trailer.tag,
            });
        }
        pages = shown
            .into_iter()
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
                overlays: info.overlays,
                data: PageData::Bare {
                    offset: trailer.start,
                    len: 0,
                },
                unknown_fields: Vec::new(),
            })
            .collect();
    }

    let (generations_present, unknown_tags) = survey(data);

    Ok(Document {
        generation,
        guard,
        trailer_tag: trailer.tag,
        trailer_at: trailer.start,
        declared_entries,
        pages,
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
    for i in 0..pages.len() {
        if matches!(pages[i].data, PageData::Fields { .. }) {
            pages[i].role = Role::Data;
            pages[i].belongs_to = None;
            continue;
        }
        if matches!(pages[i].data, PageData::Preview { .. }) {
            pages[i].role = Role::Thumbnail;
            close_group(pages, &group);
            let owner = group
                .iter()
                .rev()
                .find(|&&k| pages[k].role == Role::Sheet)
                .map(|&k| pages[k].index);
            pages[i].belongs_to = owner;
            group.clear();
        } else {
            group.push(i);
        }
    }
    close_group(pages, &group);
}

/// サムネイル間にある一続きのエントリの役割を確定する。
fn close_group(pages: &mut [Page], group: &[usize]) {
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
