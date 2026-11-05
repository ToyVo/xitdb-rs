//! Checked small codecs for untrusted file bytes.
//!
//! The storage engine constantly reinterprets integers, indexes buffers,
//! and advances offsets. Plain `as` casts, wrapping arithmetic, and direct
//! indexing would each be a silent-corruption or panic vector on malformed
//! files, so everything here is bounds-checked and every failure maps to
//! [`crate::error::Error::Corrupted`] (I/O errors propagate unchanged).

use crate::error::{Error, Result};
use crate::slot::Slot;

/// `u64` mirrors of byte-size constants (`as` casts are denied, even in
/// `const` position, so the spellings are pinned equal by `sizes_agree`).
pub const SLOT_LEN_U64: u64 = 9;
pub const INDEX_BLOCK_LEN_U64: u64 = 144;
pub const NODE_HEADER_U64: u64 = 2;
pub const BSLOT_COUNT_U64: u64 = 16;

/// Big-endian `u64` at `off`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when fewer than 8 bytes remain.
pub fn u64_be(buf: &[u8], off: usize) -> Result<u64> {
    let bytes = buf
        .get(off..off.checked_add(8).ok_or(Error::Corrupted)?)
        .ok_or(Error::Corrupted)?;
    let mut fixed = [0u8; 8];
    fixed.copy_from_slice(bytes);
    Ok(u64::from_be_bytes(fixed))
}

/// Big-endian `i64` at `off`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when fewer than 8 bytes remain.
pub fn i64_be(buf: &[u8], off: usize) -> Result<i64> {
    let bytes = buf
        .get(off..off.checked_add(8).ok_or(Error::Corrupted)?)
        .ok_or(Error::Corrupted)?;
    let mut fixed = [0u8; 8];
    fixed.copy_from_slice(bytes);
    Ok(i64::from_be_bytes(fixed))
}

/// Borrowed `len` bytes at `off`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds.
pub fn sub(buf: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    let end = off.checked_add(len).ok_or(Error::Corrupted)?;
    buf.get(off..end).ok_or(Error::Corrupted)
}

/// Exact-width `usize` to `u64`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on 128-bit targets (cannot happen on
/// supported ones, but the conversion is checked anyway).
pub fn u64_from_usize(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Corrupted)
}

/// Exact-width `u64` to `usize`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when the value does not fit.
pub fn usize_from_u64(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Corrupted)
}

/// Bit-preserving `u64` to `i64` for on-disk offsets.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when the offset exceeds `i64::MAX`.
pub fn i64_from_u64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Corrupted)
}

/// Bit-preserving `i64` to `u64` for on-disk offsets.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on negative values.
pub fn u64_from_i64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Corrupted)
}

/// Exact-width `usize` to `i64` for counts and ranks.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when the value does not fit.
pub fn i64_from_usize(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Corrupted)
}

/// Exact-width `i64` to `usize` for indexing.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on negative values or overflow.
pub fn usize_from_i64(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Corrupted)
}

/// Indexed slot read with bounds check (B-tree descent).
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds.
pub fn slot_at_arr(slots: &[Slot], i: usize) -> Result<Slot> {
    slots.get(i).copied().ok_or(Error::Corrupted)
}

/// Child pointer at an index slot: bounds-checked read plus
/// sign-checked offset conversion.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds or negative.
pub fn child_ptr(children: &[Slot], i: usize) -> Result<u64> {
    u64_from_i64(slot_at_arr(children, i)?.value)
}

/// Indexed slot write with bounds check.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds.
pub fn set_slot_at(slots: &mut [Slot], i: usize, value: Slot) -> Result<()> {
    slots
        .get_mut(i)
        .map(|slot| *slot = value)
        .ok_or(Error::Corrupted)
}

/// Indexed count read with bounds check.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds.
pub fn count_at(counts: &[i64], i: usize) -> Result<i64> {
    counts.get(i).copied().ok_or(Error::Corrupted)
}

/// Indexed count write with bounds check.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds.
pub fn set_count_at(counts: &mut [i64], i: usize, value: i64) -> Result<()> {
    counts
        .get_mut(i)
        .map(|count| *count = value)
        .ok_or(Error::Corrupted)
}

/// Mutable borrowed `len` bytes at `off`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when out of bounds.
pub fn sub_mut(buf: &mut [u8], off: usize, len: usize) -> Result<&mut [u8]> {
    let end = off.checked_add(len).ok_or(Error::Corrupted)?;
    buf.get_mut(off..end).ok_or(Error::Corrupted)
}

/// Copies equal-length slot runs (node split/merge assembly).
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on length mismatch.
pub fn copy_slots(dst: &mut [Slot], src: &[Slot]) -> Result<()> {
    if dst.len() != src.len() {
        return Err(Error::Corrupted);
    }
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d = *s;
    }
    Ok(())
}

/// Copies equal-length count runs.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on length mismatch.
pub fn copy_counts(dst: &mut [i64], src: &[i64]) -> Result<()> {
    if dst.len() != src.len() {
        return Err(Error::Corrupted);
    }
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d = *s;
    }
    Ok(())
}

/// Encodes a 16-byte collection header (size + root pointer).
#[must_use]
pub fn header_bytes(size: u64, root: u64) -> Vec<u8> {
    let mut header = Vec::with_capacity(16);
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&root.to_be_bytes());
    header
}

/// Decodes a 16-byte collection header.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when fewer than 16 bytes remain.
pub fn header_pair(bytes: &[u8]) -> Result<(u64, u64)> {
    Ok((u64_be(bytes, 0)?, u64_be(bytes, 8)?))
}

/// Bit-preserving `i64` to `u64` for inline value patterns (short-byte
/// payloads, float bits). Unlike [`u64_from_i64`](u64_from_i64), negative
/// values wrap instead of erroring — exactly like Go's conversions.
#[must_use]
pub const fn bits_u64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

/// Bit-preserving `u64` to `i64` for inline value patterns.
///
/// See [`bits_u64`] for why this wraps instead of erroring.
#[must_use]
pub const fn bits_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

/// Checked addition for file offsets.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on overflow.
pub fn add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right).ok_or(Error::Corrupted)
}

/// Checked subtraction for file offsets.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on underflow.
pub fn sub_u64(left: u64, right: u64) -> Result<u64> {
    left.checked_sub(right).ok_or(Error::Corrupted)
}

/// Checked multiplication for block sizes.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on overflow.
pub fn mul(left: u64, right: u64) -> Result<u64> {
    left.checked_mul(right).ok_or(Error::Corrupted)
}

/// Checked addition for counts and ranks.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on overflow.
pub fn iadd(left: i64, right: i64) -> Result<i64> {
    left.checked_add(right).ok_or(Error::Corrupted)
}

/// Checked subtraction for counts and ranks.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on underflow.
pub fn isub(left: i64, right: i64) -> Result<i64> {
    left.checked_sub(right).ok_or(Error::Corrupted)
}
