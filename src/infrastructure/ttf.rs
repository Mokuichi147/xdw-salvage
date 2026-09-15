//! PDFへフォントを埋め込むための最小限のTrueType解析。

use std::collections::HashMap;

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// 埋め込みに必要な範囲だけを解析したTrueTypeフォント。
pub struct Font {
    /// フォントの元データ。PDFへは使用グリフのサブセットだけを格納する。
    pub data: Vec<u8>,
    /// 1emあたりのデザイン単位数。
    pub units_per_em: u16,
    cmap: HashMap<u32, u16>,
    advances: Vec<u16>,
    /// PDFに書き込めるPostScript名。
    pub name: String,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Font({}, {} glyph(s), {} upem)",
            self.name,
            self.advances.len(),
            self.units_per_em
        )
    }
}

impl Font {
    /// フォントファイルを読み込む。
    pub fn parse(data: Vec<u8>, fallback_name: &str) -> Option<Font> {
        let tag = u32_at(&data, 0)?;
        if tag != 0x0001_0000 && tag != 0x7472_7565 {
            return None;
        }
        let count = u16_at(&data, 4)? as usize;
        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::new();
        for i in 0..count.min(512) {
            let rec = 12 + i * 16;
            let name: [u8; 4] = data.get(rec..rec + 4)?.try_into().ok()?;
            let off = u32_at(&data, rec + 8)? as usize;
            let len = u32_at(&data, rec + 12)? as usize;
            if off <= data.len() {
                tables.insert(name, (off, len.min(data.len() - off)));
            }
        }
        let table = |n: &[u8; 4]| tables.get(n).map(|&(o, l)| &data[o..o + l]);

        let head = table(b"head")?;
        let units_per_em = u16_at(head, 18)?.max(1);
        let long_loca = u16_at(head, 50)? == 1;
        let _ = long_loca;

        let hhea = table(b"hhea")?;
        let long_metrics = u16_at(hhea, 34)? as usize;
        let hmtx = table(b"hmtx")?;
        let maxp = table(b"maxp")?;
        let glyphs = u16_at(maxp, 4)? as usize;

        let mut advances = Vec::with_capacity(glyphs);
        let mut last = 0u16;
        for g in 0..glyphs {
            if g < long_metrics {
                last = u16_at(hmtx, g * 4).unwrap_or(last);
            }
            advances.push(last);
        }

        let cmap = read_cmap(table(b"cmap")?).unwrap_or_default();
        if cmap.is_empty() {
            return None;
        }

        let name = sanitise(fallback_name);
        Some(Font {
            data,
            units_per_em,
            cmap,
            advances,
            name,
        })
    }

    /// 文字を描くグリフ番号。
    pub fn glyph(&self, c: char) -> Option<u16> {
        self.cmap.get(&(c as u32)).copied()
    }

    /// グリフ幅をPDFの1/1000 em単位で返す。
    pub fn width(&self, gid: u16) -> u16 {
        let raw = self.advances.get(gid as usize).copied().unwrap_or(0) as u32;
        ((raw * 1000) / self.units_per_em as u32).min(u16::MAX as u32) as u16
    }
}

fn read_cmap(d: &[u8]) -> Option<HashMap<u32, u16>> {
    let n = u16_at(d, 2)? as usize;
    let (mut best, mut best_score) = (None, -1i32);
    for i in 0..n.min(64) {
        let rec = 4 + i * 8;
        let platform = u16_at(d, rec)?;
        let encoding = u16_at(d, rec + 2)?;
        let off = u32_at(d, rec + 4)? as usize;
        let score = match (platform, encoding) {
            (3, 10) => 4,
            (0, 4) | (0, 6) => 3,
            (3, 1) => 2,
            (0, _) => 1,
            _ => -1,
        };
        if score > best_score && off < d.len() {
            best_score = score;
            best = Some(off);
        }
    }
    let off = best?;
    let sub = d.get(off..)?;
    let mut map = HashMap::new();
    match u16_at(sub, 0)? {
        4 => {
            let segs = u16_at(sub, 6)? as usize / 2;
            let ends = 14;
            let starts = ends + segs * 2 + 2;
            let deltas = starts + segs * 2;
            let ranges = deltas + segs * 2;
            for s in 0..segs {
                let end = u16_at(sub, ends + s * 2)?;
                let start = u16_at(sub, starts + s * 2)?;
                let delta = u16_at(sub, deltas + s * 2)?;
                let range_off = u16_at(sub, ranges + s * 2)?;
                if start > end {
                    continue;
                }
                for c in start..=end {
                    if c == 0xFFFF {
                        continue;
                    }
                    let gid = if range_off == 0 {
                        c.wrapping_add(delta)
                    } else {
                        let at = ranges + s * 2 + range_off as usize + (c - start) as usize * 2;
                        match u16_at(sub, at) {
                            Some(0) | None => continue,
                            Some(g) => g.wrapping_add(delta),
                        }
                    };
                    if gid != 0 {
                        map.insert(c as u32, gid);
                    }
                }
            }
        }
        12 => {
            let groups = u32_at(sub, 12)? as usize;
            for g in 0..groups.min(200_000) {
                let rec = 16 + g * 12;
                let start = u32_at(sub, rec)?;
                let end = u32_at(sub, rec + 4)?;
                let gid = u32_at(sub, rec + 8)?;
                if start > end || end - start > 0x10_000 {
                    continue;
                }
                for c in start..=end {
                    let id = gid + (c - start);
                    if id != 0 && id <= u16::MAX as u32 {
                        map.insert(c, id as u16);
                    }
                }
            }
        }
        _ => return None,
    }
    Some(map)
}

fn sanitise(s: &str) -> String {
    let base = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let base = base.split('.').next().unwrap_or(base);
    let out: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if out.is_empty() {
        "EmbeddedFont".into()
    } else {
        out
    }
}
