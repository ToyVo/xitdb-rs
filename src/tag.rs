use crate::error::{Error, Result};

/// Collection/scalar tag. Discriminant order matches the reference impls;
/// reordering is a breaking change for the file format.
///
/// Non-exhaustive downstream: the format may gain tags, which decode as
/// [`Tag::Unknown`] here.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tag {
    None = 0,
    Index = 1,
    ArrayList = 2,
    LinkedArrayList = 3,
    HashMap = 4,
    KVPair = 5,
    Bytes = 6,
    ShortBytes = 7,
    Uint = 8,
    Int = 9,
    Float = 10,
    HashSet = 11,
    CountedHashMap = 12,
    CountedHashSet = 13,
    SortedMap = 14,
    SortedSet = 15,
    SortedKVPair = 16,
    /// Parse sentinel for tag bytes outside 0..=16. Never written by
    /// valid code; `validate` always rejects it.
    #[doc(hidden)]
    Unknown = 127,
}

impl Tag {
    /// Decodes a tag byte, mapping unknown values to [`Tag::Unknown`].
    #[must_use]
    pub(crate) const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Index,
            2 => Self::ArrayList,
            3 => Self::LinkedArrayList,
            4 => Self::HashMap,
            5 => Self::KVPair,
            6 => Self::Bytes,
            7 => Self::ShortBytes,
            8 => Self::Uint,
            9 => Self::Int,
            10 => Self::Float,
            11 => Self::HashSet,
            12 => Self::CountedHashMap,
            13 => Self::CountedHashSet,
            14 => Self::SortedMap,
            15 => Self::SortedSet,
            16 => Self::SortedKVPair,
            _ => Self::Unknown,
        }
    }

    /// Numeric discriminant (mirrors the `#[repr(u8)]` layout without a cast).
    #[must_use]
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Index => 1,
            Self::ArrayList => 2,
            Self::LinkedArrayList => 3,
            Self::HashMap => 4,
            Self::KVPair => 5,
            Self::Bytes => 6,
            Self::ShortBytes => 7,
            Self::Uint => 8,
            Self::Int => 9,
            Self::Float => 10,
            Self::HashSet => 11,
            Self::CountedHashMap => 12,
            Self::CountedHashSet => 13,
            Self::SortedMap => 14,
            Self::SortedSet => 15,
            Self::SortedKVPair => 16,
            Self::Unknown => 127,
        }
    }

    /// Rejects the [`Tag::Unknown`] sentinel from corrupt data.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedTag`] for [`Tag::Unknown`].
    pub const fn validate(self) -> Result<()> {
        match self {
            Self::Unknown => Err(Error::UnexpectedTag),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::slot::Slot;
    use crate::tag::Tag;

    #[test]
    fn tag_discriminants_match_reference_order() {
        let order = [
            (Tag::None, 0u8),
            (Tag::Index, 1),
            (Tag::ArrayList, 2),
            (Tag::LinkedArrayList, 3),
            (Tag::HashMap, 4),
            (Tag::KVPair, 5),
            (Tag::Bytes, 6),
            (Tag::ShortBytes, 7),
            (Tag::Uint, 8),
            (Tag::Int, 9),
            (Tag::Float, 10),
            (Tag::HashSet, 11),
            (Tag::CountedHashMap, 12),
            (Tag::CountedHashSet, 13),
            (Tag::SortedMap, 14),
            (Tag::SortedSet, 15),
            (Tag::SortedKVPair, 16),
        ];
        for (tag, want) in order {
            assert_eq!(tag.as_u8(), want);
        }
    }

    #[test]
    fn tag_validate_rejects_17() {
        let mut bytes = [0u8; 9];
        bytes[0] = 17;
        let slot = Slot::from_bytes(bytes);
        assert!(slot.tag.validate().is_err());
    }
}
