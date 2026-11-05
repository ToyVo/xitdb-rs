use crate::error::Result;
use crate::tag::Tag;

/// A 9-byte slot: tag byte (low 7 bits tag, high bit `full`) + BE `u64` value.
pub const SLOT_LEN: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub value: i64,
    pub tag: Tag,
    pub full: bool,
}

impl Slot {
    #[must_use]
    pub const fn empty(&self) -> bool {
        matches!(self.tag, Tag::None) && !self.full
    }

    #[must_use]
    pub const fn to_bytes(self) -> [u8; SLOT_LEN] {
        let tag = if self.full {
            self.tag.as_u8() | 0x80
        } else {
            self.tag.as_u8()
        };
        let [v0, v1, v2, v3, v4, v5, v6, v7] = self.value.to_be_bytes();
        [tag, v0, v1, v2, v3, v4, v5, v6, v7]
    }

    #[must_use]
    pub const fn from_bytes(b: [u8; SLOT_LEN]) -> Self {
        let [t, v0, v1, v2, v3, v4, v5, v6, v7] = b;
        // Unknown tag bytes become the `Unknown` sentinel so `validate`
        // rejects them instead of misreading corruption as data.
        Self {
            value: i64::from_be_bytes([v0, v1, v2, v3, v4, v5, v6, v7]),
            tag: Tag::from_u8(t & 0x7F),
            full: t & 0x80 != 0,
        }
    }

    /// Rejects unknown tags from corrupt data.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::Error::UnexpectedTag`] for the `Unknown` sentinel.
    pub const fn validate(&self) -> Result<()> {
        self.tag.validate()
    }
}

#[cfg(test)]
mod tests {
    use crate::slot::Slot;
    use crate::tag::Tag;

    #[test]
    fn slot_nine_byte_round_trip_with_full_high_bit() {
        let s = Slot {
            value: 0x01_02_03_04_05_06_07_08,
            tag: Tag::Bytes,
            full: true,
        };
        let b = s.to_bytes();
        assert_eq!(b.len(), 9);
        assert_eq!(b[0], 0x06 | 0x80);
        assert_eq!(&b[1..], &0x01_02_03_04_05_06_07_08u64.to_be_bytes());
        let back = Slot::from_bytes(b);
        assert_eq!(back.value, s.value);
        assert_eq!(back.tag, Tag::Bytes);
        assert!(back.full);
    }

    #[test]
    fn slot_empty_only_when_none_and_not_full() {
        assert!(
            Slot {
                value: 0,
                tag: Tag::None,
                full: false
            }
            .empty()
        );
        assert!(
            !Slot {
                value: 0,
                tag: Tag::None,
                full: true
            }
            .empty()
        );
        assert!(
            !Slot {
                value: 0,
                tag: Tag::Uint,
                full: false
            }
            .empty()
        );
    }
}
