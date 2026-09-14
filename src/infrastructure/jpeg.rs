//! PDF配置用のJPEGヘッダー解析。

/// JPEGストリームが宣言する情報。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub dpi: (f32, f32),
    pub dpi_known: bool,
}

impl JpegInfo {
    /// 画像を原寸配置するPDFポイント数。
    pub fn points(&self) -> (f32, f32) {
        let (dx, dy) = self.dpi;
        (
            self.width as f32 * 72.0 / dx.max(1.0),
            self.height as f32 * 72.0 / dy.max(1.0),
        )
    }
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

/// JPEGのフレームヘッダーとJFIF密度を読む。
pub fn info(buf: &[u8]) -> Option<JpegInfo> {
    if buf.len() < 4 || buf[0] != 0xFF || buf[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    let mut dpi = (72.0f32, 72.0f32);
    let mut dpi_known = false;

    while i + 1 < buf.len() {
        if buf[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = buf[i + 1];
        i += 2;
        match marker {
            0xFF => {
                i -= 1;
                continue;
            }
            0x01 | 0xD0..=0xD7 => continue,
            0xD9 | 0xDA => break,
            _ => {}
        }
        let seg_len = usize::from(be16(buf, i)?);
        if seg_len < 2 || i + seg_len > buf.len() {
            return None;
        }
        let seg = &buf[i + 2..i + seg_len];

        if marker == 0xE0 && seg.len() >= 12 && &seg[..5] == b"JFIF\0" {
            let units = seg[7];
            let x = f32::from(be16(seg, 8)?);
            let y = f32::from(be16(seg, 10)?);
            if x > 0.0 && y > 0.0 {
                match units {
                    1 => {
                        dpi = (x, y);
                        dpi_known = true;
                    }
                    2 => {
                        dpi = (x * 2.54, y * 2.54);
                        dpi_known = true;
                    }
                    _ => {}
                }
            }
        }

        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if seg.len() < 6 {
                return None;
            }
            let height = u32::from(be16(seg, 1)?);
            let width = u32::from(be16(seg, 3)?);
            let components = seg[5];
            if width == 0 || height == 0 || components == 0 {
                return None;
            }
            return Some(JpegInfo {
                width,
                height,
                components,
                dpi,
                dpi_known,
            });
        }
        i += seg_len;
    }
    None
}
