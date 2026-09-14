//! Embedded-file discovery adapter.

use crate::application::ports::AttachmentScanner;
use crate::domain::attachment::{Attachment, Kind};

const ZIP_LOCAL: &[u8] = b"PK\x03\x04";
const ZIP_EOCD: &[u8] = b"PK\x05\x06";
const ZIP_CENTRAL: &[u8] = b"PK\x01\x02";
const CFB_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// 文書内に埋め込まれた元ファイルを検出する。
pub fn scan(data: &[u8]) -> Vec<Attachment> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(p) = find(data, ZIP_LOCAL, i) {
        if let Some(a) = carve_zip(data, p) {
            i = a.offset + a.len;
            out.push(a);
        } else {
            i = p + 1;
        }
    }
    let mut i = 0usize;
    while let Some(p) = find(data, CFB_MAGIC, i) {
        if let Some(a) = carve_cfb(data, p) {
            i = a.offset + a.len;
            out.push(a);
        } else {
            i = p + 1;
        }
    }
    let mut i = 0usize;
    while let Some(p) = find(data, b"%PDF-", i) {
        if let Some(a) = carve_pdf(data, p) {
            i = a.offset + a.len;
            out.push(a);
        } else {
            i = p + 1;
        }
    }
    out.sort_by_key(|a| a.offset);
    out
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn rfind(hay: &[u8], needle: &[u8], before: usize) -> Option<usize> {
    let end = before.min(hay.len());
    if end < needle.len() {
        return None;
    }
    hay[..end].windows(needle.len()).rposition(|w| w == needle)
}

fn u16le(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(i)?, *d.get(i + 1)?]))
}

fn u32le(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *d.get(i)?,
        *d.get(i + 1)?,
        *d.get(i + 2)?,
        *d.get(i + 3)?,
    ]))
}

fn carve_zip(data: &[u8], start: usize) -> Option<Attachment> {
    let mut search_from = data.len();
    while let Some(eocd) = rfind(data, ZIP_EOCD, search_from) {
        if eocd < start {
            break;
        }
        let cd_size = u32le(data, eocd + 12)? as usize;
        let cd_offset = u32le(data, eocd + 16)? as usize;
        let comment = u16le(data, eocd + 20)? as usize;
        let end = eocd + 22 + comment;
        let cd_start = start.checked_add(cd_offset);
        if end <= data.len() && cd_start.is_some_and(|c| c.checked_add(cd_size) == Some(eocd)) {
            let part = office_part(data, start + cd_offset, cd_size);
            return Some(Attachment {
                offset: start,
                len: end - start,
                kind: Kind::Zip { part },
                length_is_estimate: false,
            });
        }
        search_from = eocd;
    }
    None
}

fn office_part(data: &[u8], cd_start: usize, cd_size: usize) -> Option<&'static str> {
    let end = cd_start.checked_add(cd_size)?.min(data.len());
    let mut i = cd_start;
    while i + 46 <= end {
        if &data[i..i + 4] != ZIP_CENTRAL {
            break;
        }
        let name_len = u16le(data, i + 28)? as usize;
        let extra = u16le(data, i + 30)? as usize;
        let comment = u16le(data, i + 32)? as usize;
        let name = data.get(i + 46..i + 46 + name_len)?;
        if name.starts_with(b"word/") {
            return Some("docx");
        }
        if name.starts_with(b"xl/") {
            return Some("xlsx");
        }
        if name.starts_with(b"ppt/") {
            return Some("pptx");
        }
        i += 46 + name_len + extra + comment;
    }
    None
}

fn carve_cfb(data: &[u8], start: usize) -> Option<Attachment> {
    if start + 512 > data.len() {
        return None;
    }
    let shift = u16le(data, start + 30)?;
    if !(7..=12).contains(&shift) {
        return None;
    }
    let sector = 1usize << shift;
    let fat_sectors = u32le(data, start + 44)? as usize;
    if fat_sectors == 0 || fat_sectors > 4096 {
        return None;
    }
    let bound = 512 + fat_sectors * (sector / 4) * sector;
    let len = bound.min(data.len() - start);
    Some(Attachment {
        offset: start,
        len,
        kind: Kind::CompoundFile,
        length_is_estimate: true,
    })
}

fn carve_pdf(data: &[u8], start: usize) -> Option<Attachment> {
    let mut end = None;
    let mut i = start;
    while let Some(p) = find(data, b"%%EOF", i) {
        end = Some(p + 5);
        i = p + 5;
    }
    let end = end?;
    Some(Attachment {
        offset: start,
        len: end - start,
        kind: Kind::Pdf,
        length_is_estimate: false,
    })
}

/// Uses the format-independent magic/length scanner for embedded originals.
#[derive(Debug, Clone, Copy, Default)]
pub struct MagicAttachmentScanner;

impl AttachmentScanner for MagicAttachmentScanner {
    fn scan(&self, data: &[u8]) -> Vec<Attachment> {
        scan(data)
    }
}
