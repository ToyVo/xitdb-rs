use crate::error::{Error, Result};
use crate::hasher::Hasher;

pub const HEADER_LEN: usize = 20;
pub const VERSION: u16 = 1;
pub const MAGIC: [u8; 3] = [0x78, 0x69, 0x74]; // 'xit'

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub file_id: u64,
    pub hash_id: u32,
    pub hash_size: u16,
    pub version: u16,
    pub magic: [u8; 3],
}

impl Header {
    #[must_use]
    pub const fn to_bytes(self) -> [u8; HEADER_LEN] {
        let [v0, v1] = self.version.to_be_bytes();
        let [s0, s1] = self.hash_size.to_be_bytes();
        let [h0, h1, h2, h3] = self.hash_id.to_be_bytes();
        let [f0, f1, f2, f3, f4, f5, f6, f7] = self.file_id.to_be_bytes();
        let [m0, m1, m2] = self.magic;
        [
            m0, m1, m2, 0, //
            v0, v1, //
            s0, s1, //
            h0, h1, h2, h3, //
            f0, f1, f2, f3, //
            f4, f5, f6, f7,
        ]
    }

    #[must_use]
    pub const fn from_bytes(b: [u8; HEADER_LEN]) -> Self {
        let [
            m0,
            m1,
            m2,
            _,
            v0,
            v1,
            s0,
            s1,
            h0,
            h1,
            h2,
            h3,
            f0,
            f1,
            f2,
            f3,
            f4,
            f5,
            f6,
            f7,
        ] = b;
        Self {
            file_id: u64::from_be_bytes([f0, f1, f2, f3, f4, f5, f6, f7]),
            hash_id: u32::from_be_bytes([h0, h1, h2, h3]),
            hash_size: u16::from_be_bytes([s0, s1]),
            version: u16::from_be_bytes([v0, v1]),
            magic: [m0, m1, m2],
        }
    }

    /// Validates magic + version + hash size against the hasher this
    /// database was opened with. Called by `Database::open`. The hash id
    /// is recorded, not enforced — like Go, which must open files such as
    /// `test.db` whose id is zero.
    /// Validates the header on open.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDatabase`], [`Error::InvalidVersion`], or
    /// [`Error::InvalidHashSize`] for foreign headers.
    pub fn validate<H: Hasher>(&self, hasher: &H) -> Result<()> {
        if self.magic != MAGIC {
            return Err(Error::InvalidDatabase);
        }
        if self.version != VERSION {
            return Err(Error::InvalidVersion);
        }
        if usize::from(self.hash_size) != hasher.digest_len() {
            return Err(Error::InvalidHashSize);
        }
        // NOTE: hash_id is informational (see above).
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::hasher::Sha1Hasher;
    use crate::header::{Header, MAGIC, VERSION};

    fn sample() -> Header {
        Header {
            file_id: 0x0102_0304_0506_0708,
            hash_id: 0x7368_6131,
            hash_size: 20,
            version: VERSION,
            magic: MAGIC,
        }
    }

    #[test]
    fn header_twenty_byte_round_trip_layout() {
        let b = sample().to_bytes();
        assert_eq!(b.len(), 20);
        assert_eq!(&b[0..3], b"xit");
        assert_eq!(b[3], 0);
        assert_eq!(&b[4..6], &VERSION.to_be_bytes());
        assert_eq!(&b[6..8], &20u16.to_be_bytes());
        assert_eq!(b[8..12], 0x7368_6131u32.to_be_bytes());
        assert_eq!(b[12..20], 0x0102_0304_0506_0708u64.to_be_bytes());
        let back = Header::from_bytes(b);
        assert_eq!(back.file_id, sample().file_id);
        assert_eq!(back.hash_id, sample().hash_id);
        assert_eq!(back.hash_size, sample().hash_size);
    }

    #[test]
    fn header_validate_rejects_bad_magic_and_version() {
        let mut bad_magic = sample();
        bad_magic.magic = [0x62, 0x61, 0x64];
        assert!(bad_magic.validate(&Sha1Hasher).is_err());
        let mut bad_version = sample();
        bad_version.version = VERSION + 1;
        assert!(bad_version.validate(&Sha1Hasher).is_err());
    }

    #[test]
    fn header_validate_rejects_wrong_hash_size() {
        let mut wrong_size = sample();
        wrong_size.hash_size = 99;
        assert!(wrong_size.validate(&Sha1Hasher).is_err());
    }
}
