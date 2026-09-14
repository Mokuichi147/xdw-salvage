//! Concrete adapters for the application ports.

pub mod attachments;
pub mod cp932;
pub mod emf;
pub mod filesystem;
pub mod jpeg;
pub mod lzh;
pub mod recovery;
pub mod tlv;
pub mod ttf;
pub mod xdw;
pub mod xdw_document;
pub mod xdw_page;

use crate::application::SalvageService;

pub use attachments::MagicAttachmentScanner;
pub use filesystem::LocalDocumentReader;
pub use recovery::LzhMetafileDecoder;
pub use xdw::XdwDocumentParser;

/// The default service used by the command-line adapter.
pub type LocalSalvageService = SalvageService<
    LocalDocumentReader,
    XdwDocumentParser,
    MagicAttachmentScanner,
    LzhMetafileDecoder,
>;

pub fn local_service() -> LocalSalvageService {
    SalvageService::new(
        LocalDocumentReader,
        XdwDocumentParser,
        MagicAttachmentScanner,
        LzhMetafileDecoder,
    )
}
