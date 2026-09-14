//! Embedded source-file entities.

/// What kind of source file was found in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A ZIP container. `part` names the office format when known.
    Zip {
        part: Option<&'static str>,
    },
    /// A compound file, as used by pre-2007 Office formats.
    CompoundFile,
    Pdf,
}

impl Kind {
    pub fn extension(&self) -> &'static str {
        match self {
            Kind::Zip {
                part: Some(extension),
            } => extension,
            Kind::Zip { part: None } => "zip",
            Kind::CompoundFile => "cfb",
            Kind::Pdf => "pdf",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Kind::Zip { part: Some("docx") } => "Word document",
            Kind::Zip { part: Some("xlsx") } => "Excel workbook",
            Kind::Zip { part: Some("pptx") } => "PowerPoint deck",
            Kind::Zip { .. } => "zip container",
            Kind::CompoundFile => "compound file (legacy Office)",
            Kind::Pdf => "PDF",
        }
    }
}

/// A payload found inside the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub offset: usize,
    pub len: usize,
    pub kind: Kind,
    /// True when the length came from a header field and may include slack.
    pub length_is_estimate: bool,
}

impl Attachment {
    pub fn bytes<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.offset..self.offset + self.len]
    }
}
