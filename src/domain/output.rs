//! Output-independent presentation settings.

/// Language used for human-readable recovery notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    /// Japanese text uses the CJK character collection selected by the output
    /// adapter unless the caller supplies an embeddable font.
    Japanese,
}
