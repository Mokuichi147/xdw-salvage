//! XDWページテーブルを表すドメインモデル。

/// このプロジェクトで確認済みの圧縮方式コード。
const KNOWN_COMPRESSION: u64 = 1;

/// ページデータの保存形態。
#[derive(Debug, Clone, PartialEq)]
pub enum PageData {
    /// ページ要素のヘッダーまたはkind 5のネスト本体に続くJPEGストリーム。
    /// バイト単位で復元できる。
    Jpeg { offset: usize, len: usize },
    /// DIBヘッダーとパレットが平文で、その後に独自圧縮された画素データが続くプレビュー画像。
    Preview {
        offset: usize,
        len: usize,
        /// データ先頭から圧縮サブヘッダーまでのオフセット。
        pixels_at: usize,
        bpp: u16,
        palette_colours: u32,
        stored: u32,
        expanded: u32,
        rows: u32,
    },
    /// ページメタデータを伴う独自圧縮データ。対応コーデックなしでは復元できない。
    Encoded {
        offset: usize,
        len: usize,
        kind_code: u64,
        /// 保存長より大きく、中間表現のサイズと考えられる値。
        aux_len: Option<u64>,
        method: Option<u64>,
        colour: Option<u64>,
    },
    /// 名前付きエントリのテーブル。平文で保存される。
    Fields {
        offset: usize,
        len: usize,
        records: usize,
    },
    /// ネスト構造もJPEGヘッダーも持たない圧縮データ。
    Bare { offset: usize, len: usize },
}

/// ページの上に重ねて描かれる図形データ。ページ自身の重ね描きか、注釈。
///
/// 文書プロパティブロックの中に、ページ本体と同じ符号化で格納されている。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// 本体と同じ保存形態コード。
    pub kind: u64,
    /// 展開後の長さ。
    pub expanded: usize,
    /// 独自圧縮されたままのデータ。
    pub coded: Vec<u8>,
    /// 注釈の位置と大きさ（x, y, 幅, 高さ。100分の1ミリメートル単位）。
    /// `None` はページ全体に重なる。
    pub area: Option<(u32, u32, u32, u32)>,
}

/// ページテーブル内のエントリ種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 文書の本文ページ。用紙サイズを持つ。
    Sheet,
    /// 直前の本文ページのサムネイル。本文ではない。
    Thumbnail,
    /// 直前の本文ページに配置された画像。
    Picture,
    /// 平文の名前付きデータテーブル。
    Data,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Sheet => "sheet",
            Role::Thumbnail => "thumbnail",
            Role::Picture => "picture",
            Role::Data => "data",
        }
    }
}

/// ページテーブルの1エントリ。
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub index: usize,
    pub role: Role,
    /// 画像またはサムネイルが属する本文ページのエントリ番号。
    pub belongs_to: Option<usize>,
    /// ファイル内のページ要素のオフセット。
    pub offset: usize,
    pub checksum: Option<u32>,
    /// 100分の1ミリメートル単位の用紙サイズ。
    pub paper: Option<(u32, u32)>,
    /// ページが記録しているピクセルサイズ。
    pub pixels: Option<(u32, u32)>,
    /// 表示時に時計回りに加える回転角（度）。文書プロパティに由来する。
    pub rotation: u16,
    /// ページに重ねて描かれる図形。文書プロパティに由来する。
    pub overlays: Vec<Overlay>,
    pub data: PageData,
    /// 未解釈のフィールドタグ。
    pub unknown_fields: Vec<u8>,
}

impl Page {
    /// このエントリ自身の画像を、コンテナのコーデックなしで書き出せるか。
    pub fn is_recoverable(&self) -> bool {
        matches!(self.data, PageData::Jpeg { .. })
    }

    /// 本文ではなく、別の本文ページのサムネイルか。
    pub fn is_preview(&self) -> bool {
        self.role == Role::Thumbnail
    }

    /// 文書の本文ページか。
    pub fn is_sheet(&self) -> bool {
        self.role == Role::Sheet
    }

    /// 記録された用紙サイズをPDFポイントへ変換する。
    pub fn paper_points(&self) -> Option<(f32, f32)> {
        self.paper
            .map(|(w, h)| (w as f32 * 0.72 / 25.4, h as f32 * 0.72 / 25.4))
    }

    /// 回転後に見える向きの用紙サイズをPDFポイントへ変換する。
    pub fn shown_points(&self) -> Option<(f32, f32)> {
        self.paper_points().map(|(w, h)| {
            if self.rotation % 180 == 90 {
                (h, w)
            } else {
                (w, h)
            }
        })
    }

    /// レポート用の保存形態名。
    pub fn kind_name(&self) -> &'static str {
        match self.data {
            PageData::Jpeg { .. } => "jpeg",
            PageData::Preview { .. } => "preview",
            PageData::Encoded { .. } => "encoded",
            PageData::Fields { .. } => "fields",
            PageData::Bare { .. } => "bare",
        }
    }

    /// レポート用の1行説明。
    pub fn describe(&self) -> String {
        let mut line = self.describe_data();
        if !self.rotation.is_multiple_of(360) {
            line.push_str(&format!("  shown turned {}°", self.rotation % 360));
        }
        let own = self.overlays.iter().filter(|o| o.area.is_none()).count();
        let notes = self.overlays.len() - own;
        if own > 0 {
            line.push_str("  +drawing");
        }
        if notes > 0 {
            line.push_str(&format!("  +{notes} annotation(s)"));
        }
        line
    }

    fn describe_data(&self) -> String {
        match &self.data {
            PageData::Jpeg { len, .. } => {
                let (w, h) = self.pixels.unwrap_or((0, 0));
                format!("JPEG    {w}x{h}px  {len} B  (recoverable)")
            }
            PageData::Preview {
                bpp,
                palette_colours,
                stored,
                expanded,
                ..
            } => {
                let (w, h) = self.pixels.unwrap_or((0, 0));
                format!(
                    "preview {w}x{h}px {bpp}bpp {palette_colours} colours  {stored} -> {expanded} B"
                )
            }
            PageData::Encoded {
                len,
                kind_code,
                aux_len,
                method,
                ..
            } => {
                let (w, h) = self.pixels.unwrap_or((0, 0));
                let paper = self
                    .paper
                    .map(|(a, b)| format!("{:.1}x{:.1}mm", a as f32 / 100.0, b as f32 / 100.0))
                    .unwrap_or_else(|| "?".into());
                let ratio = aux_len
                    .map(|a| format!(" ratio {:.1}x", a as f32 / (*len).max(1) as f32))
                    .unwrap_or_default();
                let comp = method
                    .map(|m| {
                        if m == KNOWN_COMPRESSION {
                            format!(" compression {m}")
                        } else {
                            format!(
                                " compression {m} (not the one seen in any sample; may be a standard codec)"
                            )
                        }
                    })
                    .unwrap_or_default();
                format!("encoded {w}x{h}px paper {paper}  {len} B  kind {kind_code}{ratio}{comp}")
            }
            PageData::Fields { len, records, .. } => {
                format!("fields  {len} B  {records} field(s) in the clear")
            }
            PageData::Bare { len, .. } => format!("bare    {len} B  (no metadata)"),
        }
    }
}
