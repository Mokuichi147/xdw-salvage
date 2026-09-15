//! Recovery coverage value object.

/// A count of what is and is not recoverable from a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Coverage {
    /// Sheets of the document. This is its real length.
    pub sheets: usize,
    /// Sheets whose own image could be written out or decoded.
    pub sheets_recovered: usize,
    /// Pictures placed on those sheets.
    pub pictures: usize,
    /// Pictures that could be written out.
    pub pictures_recovered: usize,
    /// Thumbnail entries, which are not content.
    pub thumbnails: usize,
    /// ページではないデータエントリ。
    pub data_tables: usize,
    /// Sheets that carry at least one recoverable picture.
    pub sheets_with_pictures: usize,
    /// Sheets that come out completely empty: the sheet itself is in the
    /// vendor's coding and it carries no recoverable artwork either.
    pub sheets_blank: usize,
}

impl Coverage {
    /// Every sheet can be reproduced.
    pub fn is_complete(&self) -> bool {
        self.sheets > 0 && self.sheets_recovered == self.sheets
    }

    /// Sheets whose image is in the vendor's coding.
    pub fn printer_derived(&self) -> usize {
        self.sheets.saturating_sub(self.sheets_recovered)
    }

    /// Share of the pictures actually written out.
    pub fn recovery_of_pictures(&self) -> Option<f32> {
        (self.pictures > 0).then(|| self.pictures_recovered as f32 / self.pictures as f32)
    }
}
