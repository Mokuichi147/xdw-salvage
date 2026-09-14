//! Pure policies used by the application layer.

use crate::domain::coverage::Coverage;

/// How much of a document can be recovered, as a single verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// An original file is embedded. The document's own source is recoverable.
    OriginalFile,
    /// Every sheet is recoverable as an image or decoded page.
    AllPages,
    /// Some sheets are recoverable, some are not.
    SomePages,
    /// No sheet is recoverable, but artwork off the sheets is.
    PicturesOnly,
    /// Nothing but structure. The sheets need the vendor's codec.
    StructureOnly,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::OriginalFile => "ORIGINAL",
            Verdict::AllPages => "ALL",
            Verdict::SomePages => "SOME",
            Verdict::PicturesOnly => "ART",
            Verdict::StructureOnly => "NONE",
        }
    }

    pub fn explain(&self) -> &'static str {
        match self {
            Verdict::OriginalFile => "carries the source file; extract it directly",
            Verdict::AllPages => "every content page recovers losslessly",
            Verdict::SomePages => "part of the sheets recover; the rest need the vendor codec",
            Verdict::PicturesOnly => "no sheet recovers, but the artwork on them does",
            Verdict::StructureOnly => "sheets are in the vendor codec",
        }
    }
}

/// Classify a document from facts supplied by the outer adapters.
///
/// Attachment discovery and coded-page decoding are deliberately represented
/// as inputs.  This keeps the policy deterministic and testable without making
/// the domain know how either operation is implemented.
pub fn classify(coverage: Coverage, has_original: bool) -> Verdict {
    if has_original {
        return Verdict::OriginalFile;
    }
    if coverage.is_complete() {
        Verdict::AllPages
    } else if coverage.sheets_recovered > 0 {
        Verdict::SomePages
    } else if coverage.pictures_recovered > 0 {
        Verdict::PicturesOnly
    } else {
        Verdict::StructureOnly
    }
}
