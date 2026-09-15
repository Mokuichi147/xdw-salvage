//! XDWコンテナで使われるタグ・長さ・値要素の読み取り。

use crate::error::{Error, Result};

/// ファイルバッファ内の絶対オフセットを持つ要素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlv {
    pub tag: u8,
    /// タグオクテットのオフセット。
    pub start: usize,
    /// 値の先頭オクテットのオフセット。
    pub value: usize,
    /// 値の長さ。
    pub len: usize,
}

impl Tlv {
    /// 値の直後のオフセット。
    #[inline]
    pub fn end(&self) -> usize {
        self.value + self.len
    }

    /// 元のバッファから要素の値を取り出す。
    #[inline]
    pub fn bytes<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.value..self.end()]
    }

    /// 値をビッグエンディアンの符号なし整数として読む。
    pub fn uint(&self, data: &[u8]) -> Option<u64> {
        if self.len == 0 || self.len > 8 {
            return None;
        }
        let mut v = 0u64;
        for &b in self.bytes(data) {
            v = (v << 8) | u64::from(b);
        }
        Some(v)
    }
}

/// `i` から長さフィールドを読み、長さと値の先頭を返す。
pub fn read_len(data: &[u8], i: usize) -> Result<(usize, usize)> {
    let n = *data.get(i).ok_or(Error::BadLength { at: i })?;
    if n < 0x80 {
        return Ok((usize::from(n), i + 1));
    }
    let k = usize::from(n & 0x7f);
    // 0は不定長形式、5以上は現実的なファイル長を超えるため拒否する。
    if k == 0 || k > 4 || i + 1 + k > data.len() {
        return Err(Error::BadLength { at: i });
    }
    let mut v = 0usize;
    for &b in &data[i + 1..i + 1 + k] {
        v = (v << 8) | usize::from(b);
    }
    Ok((v, i + 1 + k))
}

/// `at` のタグから始まる要素を1つ読む。
pub fn read_one(data: &[u8], at: usize) -> Result<Tlv> {
    let tag = *data.get(at).ok_or(Error::BadLength { at })?;
    let (len, value) = read_len(data, at + 1)?;
    if value.checked_add(len).is_none_or(|e| e > data.len()) {
        return Err(Error::Truncated { at, tag });
    }
    Ok(Tlv {
        tag,
        start: at,
        value,
        len,
    })
}

/// `data[start .. start + len]` に詰め込まれた要素を読む。
pub fn read_window(data: &[u8], start: usize, len: usize) -> Result<Vec<Tlv>> {
    let end = start
        .checked_add(len)
        .filter(|&e| e <= data.len())
        .ok_or(Error::BadLength { at: start })?;
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let t = read_one(data, i)?;
        if t.end() > end {
            return Err(Error::Truncated { at: i, tag: t.tag });
        }
        i = t.end();
        out.push(t);
    }
    Ok(out)
}

/// 指定範囲全体が要素列として解釈できるか判定する。
pub fn window_is_nested(data: &[u8], start: usize, len: usize) -> bool {
    read_window(data, start, len).is_ok_and(|v| !v.is_empty())
}

/// 指定タグを持つ最初の要素を返す。
pub fn find(items: &[Tlv], tag: u8) -> Option<Tlv> {
    items.iter().copied().find(|t| t.tag == tag)
}

/// 指定タグを持つ最初の要素を整数として返す。
pub fn find_uint(items: &[Tlv], data: &[u8], tag: u8) -> Option<u64> {
    find(items, tag).and_then(|t| t.uint(data))
}

/// トレーラーのオフセット表で使われるリトルエンディアンのu32列を読む。
pub fn le_u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}
