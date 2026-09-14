//! Reads the container structure of DocuWorks (`.xdw`) files and salvages
//! whatever can be recovered without the vendor's software.
//!
//! # What this does
//!
//! A `.xdw` file is a flat sequence of tag/length/value elements, navigated
//! from the end like a PDF. This crate reads that structure and recovers:
//!
//! * **JPEG pages**, copied out byte for byte,
//! * **original files** carried inside the document, such as the `.docx` a page
//!   was printed from, and
//! * a **PDF** assembled from the above.
//!
//! It also supplies the half of an audit that nothing else can. A bulk
//! migration through other software cannot mark its own homework; the page
//! table read out of the container says how many pages there should be, so a
//! page that quietly went missing is caught while the original still exists.
//! Reading the converted PDF back is left to whatever PDF tooling you already
//! use. See [`application::verification`].
//!
//! # What this does not do
//!
//! Pages produced by printing through the vendor's printer driver store their
//! image in a coding of the vendor's own. This crate does not decode it, and
//! reports such pages as unrecoverable rather than guessing. In practice that
//! means office documents converted to `.xdw` often yield their original file
//! but not their page images, while pages imported from images can come out
//! whole.
//!
//! Password-protected and digitally signed documents are refused. They are
//! written as a later container generation, and no facility to read past access
//! control is implemented here or planned.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//!
//! use xdw_salvage::{adapters::pdf, infrastructure};
//!
//! let service = infrastructure::local_service();
//! let asset = service.open(Path::new("scan.xdw"))?;
//! let bytes = &asset.data;
//! let doc = &asset.document;
//! let cov = doc.coverage();
//! println!("{} sheet(s), {} picture(s) on them", cov.sheets, cov.pictures);
//!
//! for a in infrastructure::attachments::scan(bytes) {
//!     std::fs::write(format!("original.{}", a.kind.extension()), a.bytes(bytes))?;
//! }
//!
//! let (file, report) = pdf::build(&bytes, &doc, pdf::Options::default());
//! std::fs::write("scan.pdf", file)?;
//! println!("{} pages embedded", report.embedded);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Provenance
//!
//! An independent implementation, written from the observable behaviour of
//! sample files plus publicly documented interface semantics. It contains no
//! vendor code, headers or binaries, and none were disassembled. DocuWorks is a
//! product of FUJIFILM Business Innovation; this crate is not affiliated with
//! or endorsed by them.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod adapters;
pub mod application;
pub mod domain;
pub mod error;
pub mod infrastructure;

pub use error::{Error, Result};
