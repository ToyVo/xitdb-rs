/// Hashing is caller-provided (like every other impl); the database only
/// stores the hash size + id in its header. Mirrors Go's `Hasher`.
pub trait Hasher {
    fn digest_len(&self) -> usize;
    fn digest(&self, data: &[u8]) -> Vec<u8>;
    fn id(&self) -> u32;
}

#[must_use]
pub const fn bytes_to_id(name: [u8; 4]) -> u32 {
    u32::from_be_bytes(name)
}

#[must_use]
pub const fn id_to_bytes(id: u32) -> [u8; 4] {
    id.to_be_bytes()
}

/// SHA-1 hasher (in-tree implementation, always available).
pub struct Sha1Hasher;

impl Hasher for Sha1Hasher {
    fn digest_len(&self) -> usize {
        20
    }

    fn digest(&self, data: &[u8]) -> Vec<u8> {
        // Length overflow is unreachable on 64-bit targets; zeros rather
        // than panicking on exotic ones.
        crate::sha1::digest(data).unwrap_or([0u8; 20]).to_vec()
    }

    fn id(&self) -> u32 {
        bytes_to_id(*b"sha1")
    }
}

#[cfg(test)]
mod tests {
    use crate::hasher::{Hasher, Sha1Hasher, bytes_to_id, id_to_bytes};

    #[test]
    fn sha1_hasher_len_and_id() {
        let h = Sha1Hasher;
        assert_eq!(h.digest_len(), 20);
        assert_eq!(h.id(), bytes_to_id(*b"sha1"));
    }

    #[test]
    fn sha1_hasher_digest_abc() {
        let h = Sha1Hasher;
        let d = h.digest(b"abc");
        assert_eq!(
            d,
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
            ]
        );
    }

    #[test]
    fn hash_id_bytes_round_trip() {
        assert_eq!(id_to_bytes(bytes_to_id(*b"sha1")), *b"sha1");
    }
}
