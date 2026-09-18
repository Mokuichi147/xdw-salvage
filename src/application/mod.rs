//! Application use cases and ports.
//!
//! This layer coordinates the work needed by the CLI and output adapters. It
//! depends on abstractions for reading, parsing, decoding and attachment
//! discovery; concrete XDW and filesystem implementations live in
//! [`crate::infrastructure`].

pub mod ports;
pub mod recovery;
pub mod service;
pub mod verification;

pub use ports::{AttachmentScanner, DocumentParser, DocumentReader, PageDecoder};
pub use recovery::{
    coverage, decode_overlay, decode_page, display_page_is_recoverable, page_is_recoverable,
};
pub use service::{Analysis, DocumentAsset, SalvageService};
pub use verification::{compare, Expectation, Finding, Report};
