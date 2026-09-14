//! XDWページ要素をドメインモデルへ変換するTLVアダプター。

use crate::domain::page::{Page, PageData, Role};
use crate::error::Result;
use crate::infrastructure::tlv::{self, Tlv};

// ページ要素内のフィールドタグ。
const F_CHECKSUM: u8 = 0x81;
const F_BODY: u8 = 0x82;

// ネストされたページ本体内のフィールドタグ。
const B_KIND: u8 = 0x80;
const B_AUX: u8 = 0x81;
const B_PAPER_W: u8 = 0x84;
const B_PAPER_H: u8 = 0x85;
const B_DATA: u8 = 0x86;
const B_STORED_LEN: u8 = 0x89;
const B_METHOD: u8 = 0x8A;
const B_COLOUR: u8 = 0x8D;
const B_PIXEL_W: u8 = 0x90;
const B_PIXEL_H: u8 = 0x91;

// プレビュー画像で確認されている本体種別コード。
const KIND_PREVIEW: u64 = 7;

/// `offset` のタグから始まるページ要素を読み取る。
pub fn read(data: &[u8], index: usize, offset: usize) -> Result<Page> {
    let elem = tlv::read_one(data, offset)?;
    let fields = tlv::read_window(data, elem.value, elem.len)?;

    let checksum = tlv::find_uint(&fields, data, F_CHECKSUM).map(|v| v as u32);
    let body = match tlv::find(&fields, F_BODY) {
        Some(b) => b,
        None => {
            return Ok(Page {
                index,
                role: Role::Sheet,
                belongs_to: None,
                offset,
                checksum,
                paper: None,
                pixels: None,
                data: PageData::Bare {
                    offset: elem.value,
                    len: elem.len,
                },
                unknown_fields: unknown(&fields, &[F_CHECKSUM, F_BODY]),
            })
        }
    };

    let mut unknown_fields = unknown(&fields, &[F_CHECKSUM, F_BODY]);
    let raw = body.bytes(data);

    // 形状1: kindフィールドから始まるネストされた本体。
    if raw.first() == Some(&B_KIND) && tlv::window_is_nested(data, body.value, body.len) {
        let f = tlv::read_window(data, body.value, body.len)?;
        unknown_fields.extend(unknown(
            &f,
            &[
                B_KIND,
                B_AUX,
                B_PAPER_W,
                B_PAPER_H,
                B_DATA,
                B_STORED_LEN,
                B_METHOD,
                B_COLOUR,
                B_PIXEL_W,
                B_PIXEL_H,
            ],
        ));
        let paper = match (
            tlv::find_uint(&f, data, B_PAPER_W),
            tlv::find_uint(&f, data, B_PAPER_H),
        ) {
            (Some(w), Some(h)) => Some((w as u32, h as u32)),
            _ => None,
        };
        let mut pixels = match (
            tlv::find_uint(&f, data, B_PIXEL_W),
            tlv::find_uint(&f, data, B_PIXEL_H),
        ) {
            (Some(w), Some(h)) => Some((w as u32, h as u32)),
            _ => None,
        };
        let kind_code = tlv::find_uint(&f, data, B_KIND).unwrap_or(0);
        let aux_len = tlv::find_uint(&f, data, B_AUX);
        let img = tlv::find(&f, B_DATA);

        let page_data = match (kind_code, img) {
            (KIND_PREVIEW, Some(img)) => {
                let d = img.bytes(data);
                let pixels_at = aux_len.unwrap_or(0) as usize;
                let (w, h, bpp, colours) = bitmap_header(d).unwrap_or((0, 0, 0, 0));
                if w > 0 {
                    pixels = Some((w, h));
                }
                let (stored, expanded, rows) = sub_header(d, pixels_at).unwrap_or((0, 0, 0));
                PageData::Preview {
                    offset: img.value,
                    len: img.len,
                    pixels_at,
                    bpp,
                    palette_colours: colours,
                    stored,
                    expanded,
                    rows,
                }
            }
            (_, Some(img)) => PageData::Encoded {
                offset: img.value,
                len: img.len,
                kind_code,
                aux_len,
                method: tlv::find_uint(&f, data, B_METHOD),
                colour: tlv::find_uint(&f, data, B_COLOUR),
            },
            (_, None) => PageData::Bare {
                offset: body.value,
                len: body.len,
            },
        };
        return Ok(Page {
            index,
            role: Role::Sheet,
            belongs_to: None,
            offset,
            checksum,
            paper,
            pixels,
            data: page_data,
            unknown_fields,
        });
    }

    // 形状2: 本体長を繰り返す16オクテットヘッダーとJPEGストリーム。
    if raw.len() > 18 {
        let declared = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
        if declared == body.len && raw[16] == 0xFF && raw[17] == 0xD8 {
            let w = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
            let h = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
            return Ok(Page {
                index,
                role: Role::Sheet,
                belongs_to: None,
                offset,
                checksum,
                paper: None,
                pixels: Some((w, h)),
                data: PageData::Jpeg {
                    offset: body.value + 16,
                    len: body.len - 16,
                },
                unknown_fields,
            });
        }
    }

    // 形状3: 平文の長さ付きフィールド列。
    if let Some(records) = field_table(raw) {
        return Ok(Page {
            index,
            role: Role::Data,
            belongs_to: None,
            offset,
            checksum,
            paper: None,
            pixels: None,
            data: PageData::Fields {
                offset: body.value,
                len: body.len,
                records,
            },
            unknown_fields,
        });
    }

    // 形状4: その他はメタデータを持たない圧縮データ。
    Ok(Page {
        index,
        role: Role::Sheet,
        belongs_to: None,
        offset,
        checksum,
        paper: None,
        pixels: None,
        data: PageData::Bare {
            offset: body.value,
            len: body.len,
        },
        unknown_fields,
    })
}

/// 本体が平文の `[length][value]` フィールド列かどうかを判定する。
fn field_table(raw: &[u8]) -> Option<usize> {
    if raw.len() < 8 {
        return None;
    }
    let mut i = 0usize;
    let mut records = 0usize;
    let mut names = 0usize;
    while i < raw.len() {
        let n = raw[i] as usize;
        if n == 0 || i + 1 + n > raw.len() {
            return None;
        }
        let value = &raw[i + 1..i + 1 + n];
        if n >= 3 && value[n - 1] == 0 && value[..n - 1].iter().all(|c| (0x20..0x7F).contains(c)) {
            names += 1;
        }
        records += 1;
        i += 1 + n;
    }
    (names * 40 >= records).then_some(records)
}

/// フィールドテーブル中の名前を出現順に返す。
pub fn field_names(data: &[u8], offset: usize, len: usize) -> Vec<String> {
    let Some(raw) = data.get(offset..offset + len) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < raw.len() {
        let n = raw[i] as usize;
        if n == 0 || i + 1 + n > raw.len() {
            break;
        }
        let value = &raw[i + 1..i + 1 + n];
        if n >= 3 && value[n - 1] == 0 && value[..n - 1].iter().all(|c| (0x20..0x7F).contains(c)) {
            if let Ok(s) = std::str::from_utf8(&value[..n - 1]) {
                let s = s.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        i += 1 + n;
    }
    out
}

fn unknown(items: &[Tlv], known: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = items
        .iter()
        .map(|t| t.tag)
        .filter(|t| !known.contains(t))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// プレビューpayloadの先頭にあるビットマップヘッダーを読む。
fn bitmap_header(d: &[u8]) -> Option<(u32, u32, u16, u32)> {
    if d.len() < 40 {
        return None;
    }
    let u32le = |i: usize| u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);
    let header_size = u32le(0);
    if header_size != 40 {
        return None;
    }
    let w = u32le(4);
    let h = (u32le(8) as i32).unsigned_abs();
    let bpp = u16::from_le_bytes([d[14], d[15]]);
    let colours = u32le(32);
    Some((w, h, bpp, colours))
}

/// パレット末尾にあるサブヘッダーから格納長・展開長・行数を読む。
fn sub_header(d: &[u8], at: usize) -> Option<(u32, u32, u32)> {
    if at + 16 > d.len() {
        return None;
    }
    let u32le = |i: usize| u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);
    Some((u32le(at + 4), u32le(at + 8), u32le(at + 12)))
}
