use std::fmt;

/// Everything that can go wrong while reading a DocuWorks container.
#[derive(Debug)]
pub enum Error {
    /// A length field claims more bytes than the buffer holds.
    Truncated {
        at: usize,
        tag: u8,
    },
    /// A length field uses an encoding this reader refuses (too many length
    /// octets, or the indefinite form, which the format never uses).
    BadLength {
        at: usize,
    },
    /// The file does not start with a container header element.
    NoFileHeader,
    /// The file is not a DocuWorks container but is something this crate
    /// recognises, which is worth saying: an archive being migrated collects
    /// files whose extension lies, and "no container header" sends whoever
    /// reads the log hunting for a fault that is not there.
    NotAContainer {
        looks_like: &'static str,
    },
    /// The trailer could not be located by walking back from the end.
    NoTrailer,
    /// A required field was absent from an element.
    MissingField {
        tag: u8,
        in_tag: u8,
    },
    /// The container format generation is outside what this crate has been
    /// verified against.
    ///
    /// This is also what you get for password-protected or digitally signed
    /// documents: those are written as a later generation. This crate does not
    /// attempt to read them, and deliberately implements no way to do so.
    UnsupportedGeneration(u32),
    /// A coded stream ended before it had produced the length the container
    /// declared for it.
    CodingTruncated {
        at: usize,
    },
    /// A coded stream's Huffman table is not usable.
    CodingBadTable,
    /// A match in a coded stream points before the start of the output.
    CodingBadDistance,
    /// A coded stream declares an expanded length past anything plausible.
    CodingAbsurdLength {
        len: usize,
    },
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CodingTruncated { at } => {
                write!(f, "coded stream ends at bit {at} before it is complete")
            }
            Error::CodingBadTable => write!(f, "coded stream has an unusable code table"),
            Error::CodingBadDistance => {
                write!(f, "coded stream copies from before the start of the page")
            }
            Error::CodingAbsurdLength { len } => {
                write!(f, "coded stream claims to expand to {len} bytes")
            }
            Error::Truncated { at, tag } => {
                write!(
                    f,
                    "element 0x{tag:02x} at offset {at} runs past end of file"
                )
            }
            Error::BadLength { at } => write!(f, "unreadable length field at offset {at}"),
            Error::NoFileHeader => f.write_str("no container header at offset 0"),
            Error::NotAContainer { looks_like } => {
                write!(
                    f,
                    "not a DocuWorks container; the contents are {looks_like}"
                )
            }
            Error::NoTrailer => f.write_str("no trailer found at end of file"),
            Error::MissingField { tag, in_tag } => {
                write!(f, "field 0x{tag:02x} missing from element 0x{in_tag:02x}")
            }
            Error::UnsupportedGeneration(v) => write!(
                f,
                "container generation {v} is not supported (protected or signed \
                 documents use a later generation; this crate does not read them)"
            ),
            Error::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
