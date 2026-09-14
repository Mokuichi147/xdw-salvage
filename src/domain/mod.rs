//! Core business models and policies.
//!
//! The domain layer deliberately knows nothing about files, command-line
//! arguments, PDF syntax, or the XDW/LZH/EMF wire formats.  Parsers and
//! renderers translate their data into the models in this module.

pub mod attachment;
pub mod coverage;
pub mod document;
pub mod output;
pub mod page;
pub mod policy;
pub mod rendering;

pub use attachment::{Attachment, Kind};
pub use coverage::Coverage;
pub use document::{Document, Rebuilt, SUPPORTED_GENERATIONS};
pub use output::Language;
pub use page::{Page, PageData, Role};
pub use policy::Verdict;
pub use rendering::{Fill, Image, Metafile, Text};
