use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    InvalidDatabase,
    InvalidVersion,
    UnexpectedTag,
    Corrupted,
    ExpiredTransaction,
    NestedTopLevelWrite,
    ExpectedTxStart,
    TargetNotEmpty,
    InvalidHashSize,
    Truncated,
    ChecksumMismatch,
    EndOfStream,
    WriteNotAllowed,
    CursorNotWriteable,
    FrozenSlot,
    FrozenBytes,
    StreamTooLong,
    InvalidOffset,
    UnexpectedWriterPosition,
    KeyNotFound,
    ExpectedUnsignedLong,
    KeyOffsetExceeded,
    MaxShiftExceeded,
    PathPartMustBeAtEnd,
    InvalidBTreeNode,
    InvalidBTreeNodeKind,
    InvalidRange,
    Io(io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDatabase => write!(f, "invalid database"),
            Self::InvalidVersion => write!(f, "invalid version"),
            Self::UnexpectedTag => write!(f, "unexpected tag"),
            Self::Corrupted => write!(f, "corrupted database"),
            Self::ExpiredTransaction => write!(f, "writer belongs to an expired transaction"),
            Self::NestedTopLevelWrite => {
                write!(f, "nested top-level writes are not allowed")
            }
            Self::ExpectedTxStart => write!(f, "expected tx start"),
            Self::TargetNotEmpty => write!(f, "target not empty"),
            Self::InvalidHashSize => write!(f, "invalid hash size"),
            Self::Truncated => write!(f, "truncated database"),
            Self::ChecksumMismatch => write!(f, "checksum mismatch"),
            Self::EndOfStream => write!(f, "end of stream"),
            Self::InvalidBTreeNode => write!(f, "invalid btree node"),
            Self::InvalidBTreeNodeKind => write!(f, "invalid btree node kind"),
            Self::InvalidRange => write!(f, "invalid range"),
            Self::WriteNotAllowed => write!(f, "write not allowed"),
            Self::CursorNotWriteable => write!(f, "cursor not writeable"),
            Self::FrozenSlot => write!(
                f,
                "writer points into frozen data; reacquire it from the transaction root"
            ),
            Self::FrozenBytes => write!(f, "byte writer points into frozen data"),
            Self::StreamTooLong => write!(f, "stream too long"),
            Self::InvalidOffset => write!(f, "invalid offset"),
            Self::UnexpectedWriterPosition => write!(f, "unexpected writer position"),
            Self::KeyNotFound => write!(f, "key not found"),
            Self::ExpectedUnsignedLong => write!(f, "expected unsigned long"),
            Self::KeyOffsetExceeded => write!(f, "key offset exceeded"),
            Self::MaxShiftExceeded => write!(f, "max shift exceeded"),
            Self::PathPartMustBeAtEnd => write!(f, "path part must be at end"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts_to_error_io() {
        let e = Error::from(io::Error::other("boom"));
        assert!(matches!(e, Error::Io(_)));
    }
}
