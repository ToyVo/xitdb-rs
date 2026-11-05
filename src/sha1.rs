//! Minimal SHA-1 (FIPS 180-4) backing [`crate::hasher::Sha1Hasher`].
//!
//! Vendored into the crate (public-domain style) so `xitdb` ships zero
//! dependencies. This is a primitives-only port for hashing database keys,
//! not a general-purpose crypto API. Known-answer tests in `hasher.rs` and
//! the trailer-checksum vectors in `database.rs` pin it byte-exact.

use crate::error::{Error, Result};

/// SHA-1 digest.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when the input length overflows `u64`
/// (unreachable on 64-bit targets).
pub fn digest(data: &[u8]) -> Result<[u8; 20]> {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    let bit_len = u64::try_from(data.len())
        .map_err(|_| Error::Corrupted)?
        .checked_mul(8)
        .ok_or(Error::Corrupted)?;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut schedule = [0u32; 80];
        let mut words = schedule.iter_mut();
        for chunk in block.chunks_exact(4).take(16) {
            let mut word_bytes = [0u8; 4];
            word_bytes.copy_from_slice(chunk);
            *words.next().ok_or(Error::Corrupted)? = u32::from_be_bytes(word_bytes);
        }
        for index in 16usize..80 {
            let back = |ago: usize| -> Result<u32> {
                schedule
                    .get(index.checked_sub(ago).ok_or(Error::Corrupted)?)
                    .copied()
                    .ok_or(Error::Corrupted)
            };
            let expanded = (back(3)? ^ back(8)? ^ back(14)? ^ back(16)?).rotate_left(1);
            *schedule.get_mut(index).ok_or(Error::Corrupted)? = expanded;
        }
        let [mut s0, mut s1, mut s2, mut s3, mut s4] = state;
        for (round, &word) in schedule.iter().enumerate() {
            let (mix, round_key) = match round {
                0..20 => ((s1 & s2) | (!s1 & s3), 0x5A82_7999),
                20..40 => (s1 ^ s2 ^ s3, 0x6ED9_EBA1),
                40..60 => ((s1 & s2) | (s1 & s3) | (s2 & s3), 0x8F1B_BCDC),
                _ => (s1 ^ s2 ^ s3, 0xCA62_C1D6),
            };
            let tmp = s0
                .rotate_left(5)
                .wrapping_add(mix)
                .wrapping_add(s4)
                .wrapping_add(round_key)
                .wrapping_add(word);
            s4 = s3;
            s3 = s2;
            s2 = s1.rotate_left(30);
            s1 = s0;
            s0 = tmp;
        }
        let [st0, st1, st2, st3, st4] = state;
        state = [
            st0.wrapping_add(s0),
            st1.wrapping_add(s1),
            st2.wrapping_add(s2),
            st3.wrapping_add(s3),
            st4.wrapping_add(s4),
        ];
    }

    let mut out = Vec::with_capacity(20);
    for limb in state {
        out.extend_from_slice(&limb.to_be_bytes());
    }
    let digest: [u8; 20] = out.try_into().map_err(|_| Error::Corrupted)?;
    Ok(digest)
}
