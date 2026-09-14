//! 文書全体を表すドメインモデル。

use super::coverage::Coverage;
use super::page::{Page, Role};

/// このクレートが検証済みのコンテナ世代。
pub const SUPPORTED_GENERATIONS: &[u32] = &[7, 10];

/// XDWコンテナから抽出された文書モデル。
#[derive(Debug, Clone)]
pub struct Document {
    /// ファイルヘッダーの世代番号。
    pub generation: u32,
    /// ファイルヘッダーに保存された4オクテットのガード値。
    pub guard: [u8; 4],
    /// 世代ごとに異なるトレーラー要素のタグ。
    pub trailer_tag: u8,
    /// トレーラー要素のファイル内オフセット。
    pub trailer_at: usize,
    /// トレーラーが宣言するエントリ数。プレビューも含む。
    pub declared_entries: u32,
    pub pages: Vec<Page>,
    /// 文書プロパティブロックのオフセットと長さ。
    pub properties: Option<(usize, usize)>,
    /// プロパティブロックの保存長と展開長。
    pub properties_len: Option<(u32, u32)>,
    /// 画像由来としてトレーラーが登録しているページ番号。
    pub image_derived: Vec<u32>,
    pub checksum: Option<u32>,
    /// ファイル内に存在する文書要素の世代数。
    pub generations_present: usize,
    /// 文書直下で解釈しなかった要素のタグ・位置・長さ。
    pub unknown_tags: Vec<(u8, usize, usize)>,
    /// ページテーブルを追跡できず、ページをスキャンで再構築した場合の理由。
    pub rebuilt: Option<Rebuilt>,
}

impl Document {
    /// 文書の本文ページを順番に返す。
    pub fn sheets(&self) -> impl Iterator<Item = &Page> {
        self.pages.iter().filter(|p| p.is_sheet())
    }

    /// プレビューではない本文エントリを返す。
    pub fn content_pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.iter().filter(|p| !p.is_preview())
    }

    /// コンテナのコーデックなしで書き出せるエントリを返す。
    pub fn recoverable_pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.iter().filter(|p| p.is_recoverable())
    }

    /// `index` の本文ページに配置された画像を返す。
    pub fn pictures_on(&self, index: usize) -> impl Iterator<Item = &Page> {
        self.pages
            .iter()
            .filter(move |p| p.role == Role::Picture && p.belongs_to == Some(index))
    }

    /// 1枚の画像を構成する連続した画像バンドを返す。
    pub fn picture_runs(&self, sheet: usize) -> Vec<Vec<&Page>> {
        let mut runs: Vec<Vec<&Page>> = Vec::new();
        for p in self.pictures_on(sheet) {
            let width = p.pixels.map(|(w, _)| w);
            let joins = runs.last().is_some_and(|r: &Vec<&Page>| {
                r.last().is_some_and(|q| q.pixels.map(|(w, _)| w) == width) && width.is_some()
            });
            if joins {
                runs.last_mut().expect("checked above").push(p);
            } else {
                runs.push(vec![p]);
            }
        }
        runs
    }

    /// 構造情報だけから、復元可能な範囲を集計する。
    pub fn coverage(&self) -> Coverage {
        let sheets = self.sheets().count();
        let pictures = self
            .pages
            .iter()
            .filter(|p| p.role == Role::Picture)
            .count();
        let sheets_recovered = self.sheets().filter(|p| p.is_recoverable()).count();
        let pictures_recovered = self
            .pages
            .iter()
            .filter(|p| p.role == Role::Picture && p.is_recoverable())
            .count();
        Coverage {
            sheets,
            sheets_recovered,
            pictures,
            pictures_recovered,
            thumbnails: self.pages.iter().filter(|p| p.is_preview()).count(),
            data_tables: self.pages.iter().filter(|p| p.role == Role::Data).count(),
            sheets_with_pictures: self
                .sheets()
                .filter(|p| self.pictures_on(p.index).next().is_some())
                .count(),
            sheets_blank: self
                .sheets()
                .filter(|p| {
                    !p.is_recoverable() && !self.pictures_on(p.index).any(|q| q.is_recoverable())
                })
                .count(),
        }
    }
}

/// ページテーブルを再構築した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebuilt {
    /// トレーラーのオフセットがファイル外を指していた。
    OffsetOutsideFile,
    /// オフセットがページ要素ではないデータを指していた。
    OffsetNotAPage,
}

impl Rebuilt {
    pub fn as_str(self) -> &'static str {
        match self {
            Rebuilt::OffsetOutsideFile => "the page table points outside the file",
            Rebuilt::OffsetNotAPage => "the page table points at something that is not a page",
        }
    }
}
