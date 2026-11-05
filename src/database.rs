use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use crate::codec;
use crate::core::Core;
use crate::cursor::{GetTarget, SlotPointer, SortedMapTarget, WriteCursor};
use crate::error::{Error, Result};
use crate::hasher::Hasher;
use crate::header::{HEADER_LEN, Header, MAGIC, VERSION};
use crate::slot::{SLOT_LEN, Slot};
use crate::tag::Tag;

// Byte sizes duplicated as `u64` literals: `as` casts (even in `const`
// position) are denied, so the two spellings are pinned equal by
// `const_sizes_agree` below instead of converting.
pub const ROOT_HEADER_START: u64 = 20; // HEADER_LEN
pub const ROOT_HEADER_LEN: u64 = 17; // SLOT_LEN + 8
pub const ROOT_HEADER_END: u64 = ROOT_HEADER_START + ROOT_HEADER_LEN;
pub const ROOT_TRAILER_LEN: u64 = 25; // SLOT_LEN + 16

pub const BIT_COUNT: u64 = 4;
pub const SLOT_COUNT: u64 = 1 << BIT_COUNT;
pub const MASK: u64 = SLOT_COUNT - 1;
pub const INDEX_BLOCK_LEN: u64 = crate::codec::INDEX_BLOCK_LEN_U64;
pub const MAX_BRANCH: u8 = 16;
const ROOT_FILE_SIZE_OFF: u64 = 29; // ROOT_HEADER_START + SLOT_LEN
const MIN_COMMITTED: u64 = 62; // ROOT_HEADER_END + ROOT_TRAILER_LEN

#[derive(Debug, Clone, Copy)]
pub struct InitOpts {
    pub hash_id: Option<u32>,
    pub file_id: Option<u64>,
    pub fsync: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaState {
    pub committed_size: u64,
    pub checksum: u64,
}

struct TxState {
    // Positions compared by later tasks (freeze/copy-on-write paths).
    #[allow(dead_code)]
    root_pos: u64,
    #[allow(dead_code)]
    scratch_pos: u64,
    frozen_at: Option<u64>,
}

pub struct Database<C: Core, H: Hasher> {
    core: RefCell<C>,
    #[allow(dead_code)]
    hasher: H,
    header: Cell<Header>,
    tx: RefCell<Option<TxState>>,
    tx_start: RefCell<Option<u64>>,
    epoch: Cell<u64>,
}

/// CRC-64/XZ (reflected ECMA table), matching Go's `crc64.Checksum`
/// over file id + trailer position + root slot + commit id, all BE.
fn crc64_table() -> &'static [u64; 256] {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u64; 256];
        let mut i = 0u64;
        for slot in &mut t {
            let mut crc = i;
            for _ in 0..8 {
                if crc & 1 == 1 {
                    crc = (crc >> 1) ^ 0xC96C_5795_D787_0F42;
                } else {
                    crc >>= 1;
                }
            }
            *slot = crc;
            // 0..256 by construction; wrapping documents the bound.
            i = i.wrapping_add(1);
        }
        t
    })
}

/// CRC-64/XZ over raw bytes (reflected ECMA table, init/xor all-ones).
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on internal index failure (unreachable:
/// the index derives from a byte).
fn crc64_xz(data: &[u8]) -> Result<u64> {
    let table = crc64_table();
    let mut crc = !0u64;
    for b in data {
        let [lo, ..] = crc.to_le_bytes();
        let entry = table
            .get(usize::from(lo ^ b))
            .copied()
            .ok_or(Error::Corrupted)?;
        crc = entry ^ (crc >> 8);
    }
    Ok(!crc)
}

/// Computes a trailer checksum.
///
/// # Errors
///
/// Propagates checksum failures (unreachable in practice).
fn trailer_checksum(file_id: u64, pos: u64, root: &[u8; SLOT_LEN], commit_id: u64) -> Result<u64> {
    let mut buf = Vec::with_capacity(33);
    buf.extend_from_slice(&file_id.to_be_bytes());
    buf.extend_from_slice(&pos.to_be_bytes());
    buf.extend_from_slice(root);
    buf.extend_from_slice(&commit_id.to_be_bytes());
    crc64_xz(&buf)
}

fn random_u64() -> Result<u64> {
    let mut b = [0u8; 8];
    crate::random::fill(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

/// Truncates back to committed bytes if the transaction did not commit.
/// Runs on unwind like Go's `defer`, so panics also roll back.
struct TxGuard<'a, C: Core, H: Hasher> {
    db: &'a Database<C, H>,
    done: bool,
}

impl<C: Core, H: Hasher> Drop for TxGuard<'_, C, H> {
    fn drop(&mut self) {
        if !self.done {
            *self.db.tx.borrow_mut() = None;
            *self.db.tx_start.borrow_mut() = None;
            let _ = self.db.truncate();
        }
    }
}

/// Nibble `offset` of a big-endian hash (LSB-first, like Go's big.Int).
///
/// # Errors
///
/// Returns [`Error::Corrupted`] when the offset runs past the hash.
fn hash_nibble(hash: &[u8], offset: u64) -> Result<u8> {
    let bit = offset.checked_mul(BIT_COUNT).ok_or(Error::Corrupted)?;
    let back = usize::try_from(bit / 8).map_err(|_| Error::Corrupted)?;
    let idx = hash
        .len()
        .checked_sub(1)
        .and_then(|l| l.checked_sub(back))
        .ok_or(Error::Corrupted)?;
    let byte = *hash.get(idx).ok_or(Error::Corrupted)?;
    let shift = u32::try_from(bit % 8).map_err(|_| Error::Corrupted)?;
    Ok((byte >> shift) & 0xF)
}

impl<C: Core, H: Hasher> Database<C, H> {
    /// Opens or creates a database on `core`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDatabase`], [`Error::InvalidVersion`], or
    /// [`Error::InvalidHashSize`] for corrupt or foreign headers,
    /// [`Error::Truncated`] for short files; propagates I/O and
    /// randomness failures.
    pub fn open(mut core: C, hasher: H, opts: InitOpts) -> Result<Self> {
        core.seek_to(0)?;
        let len = core.committed_len()?;
        if len == 0 {
            let file_id = match opts.file_id {
                Some(id) => id,
                None => random_u64()?,
            };
            let header = Header {
                file_id,
                hash_id: opts.hash_id.unwrap_or_else(|| hasher.id()),
                hash_size: u16::try_from(hasher.digest_len())
                    .map_err(|_| Error::InvalidHashSize)?,
                version: VERSION,
                magic: MAGIC,
            };
            core.seek_to(0)?;
            core.write(&header.to_bytes())?;
            // Root header caching an empty root, then the first commit
            // (id 0), so the file ends with a trailer like any database.
            core.write(
                &Slot {
                    value: 0,
                    tag: Tag::None,
                    full: false,
                }
                .to_bytes(),
            )?;
            core.write(&0u64.to_be_bytes())?;
            let db = Self {
                core: RefCell::new(core),
                hasher,
                header: Cell::new(header),
                tx: RefCell::new(None),
                tx_start: RefCell::new(None),
                epoch: Cell::new(0),
            };
            db.commit_root(
                Slot {
                    value: 0,
                    tag: Tag::None,
                    full: false,
                },
                0,
            )?;
            if opts.fsync {
                db.core.borrow_mut().sync()?;
            }
            return Ok(db);
        }
        core.seek_to(0)?;
        let mut hb = [0u8; HEADER_LEN];
        core.read(&mut hb)?;
        let header = Header::from_bytes(hb);
        header.validate(&hasher)?;
        let db = Self {
            core: RefCell::new(core),
            hasher,
            header: Cell::new(header),
            tx: RefCell::new(None),
            tx_start: RefCell::new(None),
            epoch: Cell::new(0),
        };
        // Rejects truncated/corrupt files up front.
        db.committed_size()?;
        Ok(db)
    }

    pub fn into_core(self) -> C {
        self.core.into_inner()
    }

    pub fn root_cursor(&self) -> WriteCursor<'_, C, H> {
        let root = self.read_root_slot().unwrap_or(Slot {
            value: 0,
            tag: Tag::None,
            full: false,
        });
        WriteCursor {
            db: self,
            slot_ptr: Cell::new(SlotPointer {
                position: Some(ROOT_HEADER_START),
                slot: root,
            }),
            frozen_at: Cell::new(None),
            epoch: Cell::new(None),
            _mode: std::marker::PhantomData,
        }
    }

    /// Runs `f` with a write cursor to the root in a single transaction,
    /// discarding everything it wrote if it returns an error.
    /// Runs `f` with a write cursor to the root in a single transaction,
    /// discarding everything it wrote if it returns an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NestedTopLevelWrite`] inside another transaction;
    /// propagates `f`'s error after rolling back, plus offset and I/O
    /// failures.
    pub fn run_context(&self, f: impl FnOnce(&WriteCursor<'_, C, H>) -> Result<()>) -> Result<()> {
        self.transact_root(
            |c| {
                f(c)?;
                Ok(c.slot_ptr.get())
            },
            0,
        )
        .map(|_| ())
    }

    /// Root-started transactions shared by `run_context` and root
    /// `write_path`: copies the root to scratch, runs `f` on it, and
    /// commits (or truncates on error). A result at the scratch copy is
    /// remapped to a root cursor holding the new root.
    pub(crate) fn transact_root(
        &self,
        f: impl FnOnce(&WriteCursor<'_, C, H>) -> Result<SlotPointer>,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if self.tx.borrow().is_some() {
            return Err(Error::NestedTopLevelWrite);
        }
        // Discard data left by an unfinished transaction after a crash.
        self.truncate()?;
        let scratch = self.core.borrow_mut().committed_len()?;
        let epoch = self.epoch.get().checked_add(1).ok_or(Error::Corrupted)?;
        self.epoch.set(epoch);
        *self.tx_start.borrow_mut() = Some(scratch);
        *self.tx.borrow_mut() = Some(TxState {
            root_pos: scratch,
            scratch_pos: scratch,
            frozen_at: None,
        });
        let mut guard = TxGuard {
            db: self,
            done: false,
        };

        let root = self.read_root_slot()?;
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(scratch)?;
            core.write(&root.to_bytes())?;
        }
        // Every collection expects its init part to copy its header or
        // root block into the transaction before anything writes to it.
        // The root has no enclosing init part, so run one here — this is
        // what keeps committed headers immutable across transactions.
        let scratch_ptr = crate::cursor::init_root_copy(
            self,
            SlotPointer {
                position: Some(scratch),
                slot: root,
            },
            frozen,
        )?;
        let cursor = WriteCursor {
            db: self,
            slot_ptr: Cell::new(scratch_ptr),
            frozen_at: Cell::new(None),
            epoch: Cell::new(Some(epoch)),
            _mode: std::marker::PhantomData,
        };
        let result = match f(&cursor) {
            Err(e) => {
                *self.tx.borrow_mut() = None;
                *self.tx_start.borrow_mut() = None;
                self.epoch
                    .set(epoch.checked_add(1).ok_or(Error::Corrupted)?);
                guard.done = true;
                self.truncate()?;
                return Err(e);
            }
            Ok(sp) => sp,
        };
        // The scratch copy now holds the new root; commit it.
        let next_root = {
            let mut core = self.core.borrow_mut();
            core.seek_to(scratch)?;
            let mut b = [0u8; SLOT_LEN];
            core.read(&mut b)?;
            Slot::from_bytes(b)
        };
        self.commit_root(next_root, random_u64()?)?;
        *self.tx.borrow_mut() = None;
        *self.tx_start.borrow_mut() = None;
        self.epoch
            .set(epoch.checked_add(1).ok_or(Error::Corrupted)?);
        guard.done = true;
        // A path ending at the root returns a root cursor for later txs.
        if result.position == Some(scratch) {
            return Ok(SlotPointer {
                position: Some(ROOT_HEADER_START),
                slot: next_root,
            });
        }
        Ok(result)
    }

    /// Freezes later writes in the active transaction to copy-on-write.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ExpectedTxStart`] outside a transaction; propagates
    /// I/O failures.
    pub fn freeze(&self) -> Result<()> {
        if self.tx.borrow().is_none() || self.tx_start.borrow().is_none() {
            return Err(Error::ExpectedTxStart);
        }
        let len = self.core.borrow_mut().committed_len()?;
        *self.tx_start.borrow_mut() = Some(len);
        if let Some(tx) = self.tx.borrow_mut().as_mut() {
            tx.frozen_at = Some(len);
        }
        Ok(())
    }

    /// Committed byte count (the root header's file size).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDatabase`] or [`Error::Truncated`] for
    /// corrupt files; propagates I/O failures.
    pub fn committed_size(&self) -> Result<u64> {
        Ok(self.read_root_header()?.0)
    }

    pub(crate) const fn core(&self) -> &RefCell<C> {
        &self.core
    }

    pub(crate) fn in_tx(&self) -> bool {
        self.tx.borrow().is_some()
    }

    pub(crate) fn current_epoch(&self) -> u64 {
        self.epoch.get()
    }

    /// The active transaction's freeze point, if any.
    pub(crate) fn tx_frozen(&self) -> Option<u64> {
        self.tx.borrow().as_ref().and_then(|tx| tx.frozen_at)
    }

    /// Rejects writes into frozen data; the transaction's root copy
    /// stays writable.
    pub(crate) fn check_frozen_slot(&self, ptr: SlotPointer) -> Result<()> {
        if let Some(tx) = self.tx.borrow().as_ref() {
            if let (Some(f), Some(p)) = (tx.frozen_at, ptr.position) {
                if p < f && Some(p) != Some(tx.root_pos) {
                    return Err(Error::FrozenSlot);
                }
            }
        }
        Ok(())
    }

    /// File position of a key-value entry's key slot (`None` for key-only
    /// bytes entries, which have no separate slot).
    pub(crate) fn kv_key_position(&self, entry: Slot) -> Option<u64> {
        match entry.tag {
            Tag::KVPair => {
                let base = u64::try_from(entry.value).ok()?;
                base.checked_add(u64::from(self.header().hash_size))
            }
            Tag::SortedKVPair => u64::try_from(entry.value).ok(),
            _ => None,
        }
    }

    /// File position of a key-value entry's value slot.
    pub(crate) fn kv_value_position(&self, entry: Slot) -> Option<u64> {
        self.kv_key_position(entry)
            .and_then(|k| k.checked_add(codec::SLOT_LEN_U64))
    }

    /// Decodes a `KVPair`/`SortedKVPair` entry (or a key-only bytes
    /// entry) into its key slot, value slot, and hash.
    pub(crate) fn read_kv_pair(&self, entry: Slot) -> Result<(Slot, Slot, Vec<u8>)> {
        match entry.tag {
            Tag::Bytes | Tag::ShortBytes => Ok((
                entry,
                Slot {
                    value: 0,
                    tag: Tag::None,
                    full: false,
                },
                vec![0u8; usize::from(self.header().hash_size)],
            )),
            Tag::KVPair | Tag::SortedKVPair => {
                let mut core = self.core.borrow_mut();
                core.seek_to(codec::u64_from_i64(entry.value)?)?;
                // Sorted pairs omit the hash; plain pairs store it first.
                let mut hash = vec![0u8; usize::from(self.header().hash_size)];
                if entry.tag == Tag::KVPair {
                    core.read(&mut hash)?;
                }
                let mut key_bytes = [0u8; SLOT_LEN];
                core.read(&mut key_bytes)?;
                let key = Slot::from_bytes(key_bytes);
                key.validate()?;
                let mut value_bytes = [0u8; SLOT_LEN];
                core.read(&mut value_bytes)?;
                let value = Slot::from_bytes(value_bytes);
                value.validate()?;
                Ok((key, value, hash))
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    #[allow(dead_code)]
    pub(crate) const fn hasher(&self) -> &H {
        &self.hasher
    }

    pub(crate) fn header(&self) -> Header {
        self.header.get()
    }

    pub(crate) fn read_root_slot(&self) -> Result<Slot> {
        Ok(self.read_root_header()?.1)
    }

    /// File size from the root header plus the root slot from the
    /// trailer it points to. Rejects truncated/corrupt files.
    pub(crate) fn read_root_header(&self) -> Result<(u64, Slot)> {
        let mut core = self.core.borrow_mut();
        core.seek_to(ROOT_FILE_SIZE_OFF)?;
        let mut b = [0u8; 8];
        core.read(&mut b)?;
        let file_size = u64::from_be_bytes(b);
        if file_size < MIN_COMMITTED {
            return Err(Error::InvalidDatabase);
        }
        let len = core.committed_len()?;
        if len < file_size {
            return Err(Error::Truncated);
        }
        drop(core);
        let (root, _, _) = self.read_root_trailer(file_size)?;
        Ok((file_size, root))
    }

    fn read_root_trailer(&self, end: u64) -> Result<(Slot, u64, u64)> {
        if end < ROOT_TRAILER_LEN {
            return Err(Error::InvalidDatabase);
        }
        let pos = codec::sub_u64(end, ROOT_TRAILER_LEN)?;
        let mut core = self.core.borrow_mut();
        core.seek_to(pos)?;
        let mut b = [0u8; SLOT_LEN + 16];
        core.read(&mut b)?;
        drop(core);
        let mut root_bytes = [0u8; SLOT_LEN];
        root_bytes.copy_from_slice(codec::sub(&b, 0, SLOT_LEN)?);
        let commit_id = codec::u64_be(&b, SLOT_LEN)?;
        let checksum_off = codec::usize_from_u64(codec::add(codec::SLOT_LEN_U64, 8)?)?;
        let checksum = codec::u64_be(&b, checksum_off)?;
        // Checksum over the raw bytes (not re-serialized) so a corrupt tag
        // byte surfaces as a checksum mismatch, exactly like Go.
        if checksum != trailer_checksum(self.header().file_id, pos, &root_bytes, commit_id)? {
            return Err(Error::InvalidDatabase);
        }
        let root = Slot::from_bytes(root_bytes);
        root.validate()?;
        Ok((root, commit_id, checksum))
    }

    fn write_root_header(&self, root: Slot) -> Result<()> {
        let mut core = self.core.borrow_mut();
        let file_size = core.committed_len()?;
        core.seek_to(ROOT_HEADER_START)?;
        core.write(&root.to_bytes())?;
        core.write(&file_size.to_be_bytes())?;
        Ok(())
    }

    /// Appends a trailer for `root` and points the root header at it.
    /// Callers must sync afterwards to make the header durable.
    fn commit_root(&self, root: Slot, commit_id: u64) -> Result<()> {
        let trailer_pos = self.core.borrow_mut().committed_len()?;
        let checksum = trailer_checksum(
            self.header().file_id,
            trailer_pos,
            &root.to_bytes(),
            commit_id,
        )?;
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(trailer_pos)?;
            core.write(&root.to_bytes())?;
            core.write(&commit_id.to_be_bytes())?;
            core.write(&checksum.to_be_bytes())?;
            // Flushing before the header update is what completes the
            // transaction; without it a crash could persist the header
            // before the data it points to.
            core.sync()?;
        }
        self.write_root_header(root)
    }

    pub(crate) fn truncate(&self) -> Result<()> {
        let committed = self.committed_size()?;
        let frozen = self.tx.borrow().as_ref().and_then(|tx| tx.frozen_at);
        if let Some(f) = frozen {
            if f > committed {
                // Retain frozen bytes without publishing the failed
                // transaction: cut past them, then re-commit the
                // current root so they end with a trailer.
                let file_len = self.core.borrow_mut().committed_len()?;
                if file_len > f {
                    self.core.borrow_mut().truncate(f)?;
                }
                let root = self.read_root_slot()?;
                self.commit_root(root, random_u64()?)?;
                self.core.borrow_mut().sync()?;
                return Ok(());
            }
        }
        let file_len = self.core.borrow_mut().committed_len()?;
        if file_len > committed {
            self.core.borrow_mut().truncate(committed)?;
        }
        Ok(())
    }
}

/// Hit from descending a hash trie: the resolved slot plus whether the
/// key was freshly created (drives counted-map increments).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MapHit {
    pub ptr: SlotPointer,
    pub is_empty: bool,
}

/// Hash-trie (HAMT) and array-trie storage engine. Mirrors the reference
/// impls tier for tier; `for_write` selects read vs mutating descent.
impl<C: Core, H: Hasher> Database<C, H> {
    pub(crate) fn read_slot_at(&self, pos: u64) -> Result<Slot> {
        let mut core = self.core.borrow_mut();
        core.seek_to(pos)?;
        let mut b = [0u8; SLOT_LEN];
        core.read(&mut b)?;
        Ok(Slot::from_bytes(b))
    }

    pub(crate) fn write_slot_at(&self, pos: u64, slot: Slot) -> Result<()> {
        let mut core = self.core.borrow_mut();
        core.seek_to(pos)?;
        core.write(&slot.to_bytes())?;
        Ok(())
    }

    fn max_hash_tiers(&self) -> u64 {
        // `hash_size` is a validated `u16`; doubling cannot overflow.
        u64::from(self.header().hash_size).saturating_mul(2)
    }

    pub(crate) fn check_hash(&self, hash: &[u8]) -> Result<()> {
        if hash.len() != usize::from(self.header().hash_size) {
            return Err(Error::InvalidHashSize);
        }
        Ok(())
    }

    /// Whether file data predates this transaction (or the cursor's freeze
    /// point) and must be copied before mutation.
    pub(crate) fn is_old(&self, ptr: u64, frozen: u64) -> Result<bool> {
        (*self.tx_start.borrow())
            .map(|start| ptr < start.max(frozen))
            .ok_or(Error::ExpectedTxStart)
    }

    /// Copies a 16-slot index block to the end unless it was made in this
    /// transaction.
    pub(crate) fn copy_index_block_if_old(&self, ptr: u64, frozen: u64) -> Result<u64> {
        if !self.is_old(ptr, frozen)? {
            return Ok(ptr);
        }
        let mut block = vec![0u8; codec::usize_from_u64(INDEX_BLOCK_LEN)?];
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(ptr)?;
            core.read(&mut block)?;
        }
        let mut core = self.core.borrow_mut();
        let next = core.committed_len()?;
        core.seek_to(next)?;
        core.write(&block)?;
        Ok(next)
    }

    /// After appending at the transaction's scratch copy, the appended
    /// slot becomes the transaction's root so it stays writable past a
    /// freeze.
    pub(crate) fn note_append_root(&self, array_pos: Option<u64>, result_pos: Option<u64>) {
        if let Some(tx) = self.tx.borrow_mut().as_mut() {
            if array_pos == Some(tx.scratch_pos) {
                if let Some(r) = result_pos {
                    tx.root_pos = r;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Allocates a fresh key/value triple for a missing hash-bucket slot.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupted`] on offset overflow; propagates I/O errors.
    fn map_slot_create(&self, slot_pos: u64, hash: &[u8], target: &GetTarget) -> Result<MapHit> {
        // Write hash plus empty key/value slots, then point at them.
        let hash_pos = self.core.borrow_mut().committed_len()?;
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(hash_pos)?;
            core.write(hash)?;
            let empty = Slot {
                value: 0,
                tag: Tag::None,
                full: false,
            };
            core.write(&empty.to_bytes())?;
            core.write(&empty.to_bytes())?;
        }
        let key_pos = codec::add(hash_pos, u64::from(self.header().hash_size))?;
        let value_pos = codec::add(key_pos, codec::SLOT_LEN_U64)?;
        let next = Slot {
            value: crate::codec::i64_from_u64(hash_pos)?,
            tag: Tag::KVPair,
            full: false,
        };
        self.write_slot_at(slot_pos, next)?;
        let empty = Slot {
            value: 0,
            tag: Tag::None,
            full: false,
        };
        let ptr = match target {
            GetTarget::KeyValue(_) => SlotPointer {
                position: Some(slot_pos),
                slot: next,
            },
            GetTarget::Key(_) => SlotPointer {
                position: Some(key_pos),
                slot: empty,
            },
            GetTarget::Value(_) => SlotPointer {
                position: Some(value_pos),
                slot: empty,
            },
        };
        Ok(MapHit {
            ptr,
            is_empty: true,
        })
    }

    /// Resolves a hash-bucket key/value pair: copy-on-write when old,
    /// splits the tier on hash collision.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyNotFound`] in read mode or on collision miss,
    /// [`Error::KeyOffsetExceeded`] past the last tier, [`Error::Corrupted`]
    /// on offset overflow; propagates I/O errors.
    fn map_slot_kv(
        &self,
        at: SlotPointer,
        hash: &[u8],
        offset: u64,
        for_write: bool,
        target: &GetTarget,
        frozen: u64,
    ) -> Result<MapHit> {
        let slot_pos = at.position.ok_or(Error::Corrupted)?;
        let slot = at.slot;
        let (k, v, h) = self.read_kv_pair(slot)?;
        if h == hash {
            if for_write && self.is_old(codec::u64_from_i64(slot.value)?, frozen)? {
                let hash_pos = self.core.borrow_mut().committed_len()?;
                {
                    let mut core = self.core.borrow_mut();
                    core.seek_to(hash_pos)?;
                    core.write(&h)?;
                    core.write(&k.to_bytes())?;
                    core.write(&v.to_bytes())?;
                }
                let key_pos = codec::add(hash_pos, u64::from(self.header().hash_size))?;
                let value_pos = codec::add(key_pos, codec::SLOT_LEN_U64)?;
                let next = Slot {
                    value: crate::codec::i64_from_u64(hash_pos)?,
                    tag: Tag::KVPair,
                    full: false,
                };
                self.write_slot_at(slot_pos, next)?;
                let ptr = match target {
                    GetTarget::KeyValue(_) => SlotPointer {
                        position: Some(slot_pos),
                        slot: next,
                    },
                    GetTarget::Key(_) => SlotPointer {
                        position: Some(key_pos),
                        slot: k,
                    },
                    GetTarget::Value(_) => SlotPointer {
                        position: Some(value_pos),
                        slot: v,
                    },
                };
                return Ok(MapHit {
                    ptr,
                    is_empty: false,
                });
            }
            let key_pos = codec::add(
                codec::u64_from_i64(slot.value)?,
                u64::from(self.header().hash_size),
            )?;
            let value_pos = codec::add(key_pos, codec::SLOT_LEN_U64)?;
            let ptr = match target {
                GetTarget::KeyValue(_) => SlotPointer {
                    position: Some(slot_pos),
                    slot,
                },
                GetTarget::Key(_) => SlotPointer {
                    position: Some(key_pos),
                    slot: k,
                },
                GetTarget::Value(_) => SlotPointer {
                    position: Some(value_pos),
                    slot: v,
                },
            };
            return Ok(MapHit {
                ptr,
                is_empty: false,
            });
        }
        // Hash collision with a different key: split the tier.
        if !for_write {
            return Err(Error::KeyNotFound);
        }
        if codec::add(offset, 1)? >= self.max_hash_tiers() {
            return Err(Error::KeyOffsetExceeded);
        }
        let next_i = u64::from(hash_nibble(&h, codec::add(offset, 1)?)?);
        let block = self.core.borrow_mut().committed_len()?;
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(block)?;
            core.write(&vec![0u8; codec::usize_from_u64(INDEX_BLOCK_LEN)?])?;
            core.seek_to(codec::add(block, codec::mul(codec::SLOT_LEN_U64, next_i)?)?)?;
            core.write(&slot.to_bytes())?;
        }
        let res = self.read_map_slot(
            block,
            hash,
            codec::add(offset, 1)?,
            for_write,
            target,
            frozen,
        )?;
        self.write_slot_at(
            slot_pos,
            Slot {
                value: crate::codec::i64_from_u64(block)?,
                tag: Tag::Index,
                full: false,
            },
        )?;
        Ok(res)
    }

    pub(crate) fn read_map_slot(
        &self,
        index_pos: u64,
        hash: &[u8],
        offset: u64,
        for_write: bool,
        target: &GetTarget,
        frozen: u64,
    ) -> Result<MapHit> {
        if offset > self.max_hash_tiers() {
            return Err(Error::KeyOffsetExceeded);
        }
        let i = u64::from(hash_nibble(hash, offset)?);
        let slot_pos = codec::add(index_pos, codec::mul(codec::SLOT_LEN_U64, i)?)?;
        let slot = self.read_slot_at(slot_pos)?;
        match slot.tag {
            Tag::None => {
                if !for_write {
                    return Err(Error::KeyNotFound);
                }
                self.map_slot_create(slot_pos, hash, target)
            }
            Tag::Index => {
                let mut next = codec::u64_from_i64(slot.value)?;
                if for_write {
                    let copied = self.copy_index_block_if_old(next, frozen)?;
                    if copied != next {
                        next = copied;
                        self.write_slot_at(
                            slot_pos,
                            Slot {
                                value: crate::codec::i64_from_u64(next)?,
                                tag: Tag::Index,
                                full: false,
                            },
                        )?;
                    }
                }
                self.read_map_slot(
                    next,
                    hash,
                    codec::add(offset, 1)?,
                    for_write,
                    target,
                    frozen,
                )
            }
            Tag::KVPair => self.map_slot_kv(
                SlotPointer {
                    position: Some(slot_pos),
                    slot,
                },
                hash,
                offset,
                for_write,
                target,
                frozen,
            ),
            _ => Err(Error::UnexpectedTag),
        }
    }

    pub(crate) fn remove_map_slot(
        &self,
        index_pos: u64,
        hash: &[u8],
        offset: u64,
        frozen: u64,
    ) -> Result<Slot> {
        if offset > self.max_hash_tiers() {
            return Err(Error::KeyOffsetExceeded);
        }
        let mut block = [Slot {
            value: 0,
            tag: Tag::None,
            full: false,
        }; BTREE_SLOT_COUNT];
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(index_pos)?;
            for s in &mut block {
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                *s = Slot::from_bytes(b);
            }
        }
        let i = usize::from(hash_nibble(hash, offset)?);
        let here = block.get(i).copied().ok_or(Error::Corrupted)?;
        let slot_pos = codec::add(
            index_pos,
            codec::mul(codec::SLOT_LEN_U64, codec::u64_from_usize(i)?)?,
        )?;
        let next = match here.tag {
            Tag::None => return Err(Error::KeyNotFound),
            Tag::Index => self.remove_map_slot(
                codec::u64_from_i64(here.value)?,
                hash,
                codec::add(offset, 1)?,
                frozen,
            )?,
            Tag::KVPair => {
                let (_, _, h) = self.read_kv_pair(here)?;
                if h == hash {
                    Slot {
                        value: 0,
                        tag: Tag::None,
                        full: false,
                    }
                } else {
                    return Err(Error::KeyNotFound);
                }
            }
            _ => return Err(Error::UnexpectedTag),
        };
        // At the root tier the block is rewritten in place.
        if offset == 0 {
            self.write_slot_at(slot_pos, next)?;
            return Ok(Slot {
                value: crate::codec::i64_from_u64(index_pos)?,
                tag: Tag::Index,
                full: false,
            });
        }
        // Collapse a tier left with a single pair (or none).
        if let Some(slot) = block.get_mut(i) {
            *slot = next;
        } else {
            return Err(Error::Corrupted);
        }
        let mut used: usize = 0;
        let mut single = Slot {
            value: 0,
            tag: Tag::None,
            full: false,
        };
        for s in &block {
            if s.tag == Tag::None {
                continue;
            }
            used = used.checked_add(1).ok_or(Error::Corrupted)?;
            single = *s;
        }
        if used <= 1 {
            match single.tag {
                Tag::None | Tag::KVPair => return Ok(single),
                _ => {}
            }
        }
        let at = self.copy_index_block_if_old(index_pos, frozen)?;
        let at_i = codec::add(
            at,
            codec::mul(codec::SLOT_LEN_U64, codec::u64_from_usize(i)?)?,
        )?;
        self.write_slot_at(at_i, next)?;
        Ok(Slot {
            value: crate::codec::i64_from_u64(at)?,
            tag: Tag::Index,
            full: false,
        })
    }

    pub(crate) fn read_arraylist_slot(
        &self,
        index_pos: u64,
        key: i64,
        shift: u8,
        for_write: bool,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if shift >= MAX_BRANCH {
            return Err(Error::MaxShiftExceeded);
        }
        let shift_bits = codec::mul(u64::from(shift), BIT_COUNT)?;
        // `key` is a non-negative rank here; the mask keeps 4 bits.
        let i = codec::u64_from_i64((key >> shift_bits) & 15)?;
        let slot_pos = codec::add(index_pos, codec::mul(codec::SLOT_LEN_U64, i)?)?;
        let slot = self.read_slot_at(slot_pos)?;
        if shift == 0 {
            return Ok(SlotPointer {
                position: Some(slot_pos),
                slot,
            });
        }
        match slot.tag {
            Tag::None => {
                if !for_write {
                    return Err(Error::KeyNotFound);
                }
                let next = self.core.borrow_mut().committed_len()?;
                {
                    let mut core = self.core.borrow_mut();
                    core.seek_to(next)?;
                    core.write(&vec![0u8; codec::usize_from_u64(INDEX_BLOCK_LEN)?])?;
                }
                self.write_slot_at(
                    slot_pos,
                    Slot {
                        value: crate::codec::i64_from_u64(next)?,
                        tag: Tag::Index,
                        full: false,
                    },
                )?;
                self.read_arraylist_slot(
                    next,
                    key,
                    shift.checked_sub(1).ok_or(Error::Corrupted)?,
                    for_write,
                    frozen,
                )
            }
            Tag::Index => {
                let mut next = codec::u64_from_i64(slot.value)?;
                if for_write {
                    let copied = self.copy_index_block_if_old(next, frozen)?;
                    if copied != next {
                        next = copied;
                        self.write_slot_at(
                            slot_pos,
                            Slot {
                                value: crate::codec::i64_from_u64(next)?,
                                tag: Tag::Index,
                                full: false,
                            },
                        )?;
                    }
                }
                self.read_arraylist_slot(
                    next,
                    key,
                    shift.checked_sub(1).ok_or(Error::Corrupted)?,
                    for_write,
                    frozen,
                )
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// Appends one slot: descends to the key-`size` slot (growing tiers on
    /// overflow), clears it, and returns the grown header plus its pointer.
    pub(crate) fn read_arraylist_append(
        &self,
        size: u64,
        ptr: u64,
        frozen: u64,
    ) -> Result<((u64, u64), SlotPointer)> {
        let prev_shift = array_shift(size, true)?;
        let next_shift = array_shift(size, false)?;
        let mut index_pos = ptr;
        if prev_shift != next_shift {
            // Root overflow: the old root becomes child zero of a new one.
            let fresh = self.core.borrow_mut().committed_len()?;
            {
                let mut core = self.core.borrow_mut();
                core.seek_to(fresh)?;
                core.write(&vec![0u8; codec::usize_from_u64(INDEX_BLOCK_LEN)?])?;
                core.seek_to(fresh)?;
                core.write(
                    &Slot {
                        value: crate::codec::i64_from_u64(index_pos)?,
                        tag: Tag::Index,
                        full: false,
                    }
                    .to_bytes(),
                )?;
            }
            index_pos = fresh;
        }
        let sp = self.read_arraylist_slot(
            index_pos,
            codec::i64_from_u64(size)?,
            next_shift,
            true,
            frozen,
        )?;
        let pos = sp.position.ok_or(Error::CursorNotWriteable)?;
        // Clear values left by a rollback or slice.
        self.write_slot_at(
            pos,
            Slot {
                value: 0,
                tag: Tag::None,
                full: false,
            },
        )?;
        Ok((
            (codec::add(size, 1)?, index_pos),
            SlotPointer {
                position: Some(pos),
                slot: Slot {
                    value: 0,
                    tag: Tag::None,
                    full: false,
                },
            },
        ))
    }

    /// Shrinks an array header to `new_size`, descending to the tier that
    /// holds it. Mirrors Go's `readArrayListSlice`.
    pub(crate) fn read_arraylist_slice(
        &self,
        size: u64,
        ptr: u64,
        new_size: i64,
        frozen: u64,
    ) -> Result<(u64, u64)> {
        if new_size < 0 || codec::u64_from_i64(new_size)? > size {
            return Err(Error::KeyNotFound);
        }
        let new_size = codec::u64_from_i64(new_size)?;
        // Next-tier shift counts from `new_size - 1` (not `new_size`):
        // shrinking 17 elements to exactly 16 drops a tier.
        let next = if new_size == 0 {
            0
        } else {
            log16_floor(new_size.checked_sub(1).ok_or(Error::Corrupted)?)?
        };
        if array_shift(size, true)? == next {
            return Ok((new_size, ptr));
        }
        let mut shift = array_shift(size, true)?;
        let mut index_pos = ptr;
        while shift > next {
            let slot = self.read_slot_at(index_pos)?;
            shift = shift.checked_sub(1).ok_or(Error::Corrupted)?;
            index_pos = codec::u64_from_i64(slot.value)?;
        }
        // The new root may still belong to a past moment; copy it now.
        let index_pos = self.copy_index_block_if_old(index_pos, frozen)?;
        Ok((new_size, index_pos))
    }
}

/// Tier shift for `size` elements. `prev` selects the `size - 1` variant
/// used to detect root overflow on append. Integer base-16 floor log:
/// identical to Go's float expression for every input, without float
///-boundary risk at exact powers of 16.
fn array_shift(size: u64, prev: bool) -> Result<u8> {
    let probe = if prev {
        if size == 0 {
            return Ok(0);
        }
        size.checked_sub(1).ok_or(Error::Corrupted)?
    } else {
        size
    };
    log16_floor(probe)
}

/// floor(log16(n)): the tier shift for the last key `n`.
///
/// # Errors
///
/// Returns [`Error::Corrupted`] on internal counter overflow (unreachable:
/// at most 16 tiers exist).
pub(crate) fn log16_floor(n: u64) -> Result<u8> {
    let mut shift: u8 = 0;
    let mut x = n;
    while x >= SLOT_COUNT {
        x /= SLOT_COUNT;
        shift = shift.checked_add(1).ok_or(Error::Corrupted)?;
    }
    Ok(shift)
}

pub(crate) const BTREE_SLOT_COUNT: usize = 16;
pub(crate) const BTREE_SPLIT_COUNT: usize = 8;
pub(crate) const BTREE_NODE_HEADER: usize = 2;
// `usize` block sizes via const arithmetic (exempt from the lint, unlike
// runtime arithmetic); the `u64` spellings live in `codec` for offsets.
pub(crate) const BTREE_LEAF_BLOCK: usize = BTREE_NODE_HEADER + SLOT_LEN * BTREE_SLOT_COUNT;
pub(crate) const BTREE_BRANCH_BLOCK: usize = BTREE_NODE_HEADER + (SLOT_LEN + 8) * BTREE_SLOT_COUNT;
pub(crate) const SORTED_LEAF_BLOCK: usize = BTREE_LEAF_BLOCK;
pub(crate) const SORTED_BRANCH_BLOCK: usize =
    BTREE_NODE_HEADER + (SLOT_LEN * 2 + 8) * BTREE_SLOT_COUNT;

pub(crate) const BTREE_KIND_LEAF: u8 = 0;
pub(crate) const BTREE_KIND_BRANCH: u8 = 1;

pub(crate) const fn empty_slot() -> Slot {
    Slot {
        value: 0,
        tag: Tag::None,
        full: false,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BTreeNode {
    pub kind: u8,
    pub num: usize,
    pub values: [Slot; BTREE_SLOT_COUNT],
    pub children: [Slot; BTREE_SLOT_COUNT],
    pub counts: [i64; BTREE_SLOT_COUNT],
}

impl BTreeNode {
    /// Blank node of `num` entries (content filled by the caller).
    const fn sized(kind: u8, num: usize) -> Self {
        Self {
            kind,
            num,
            values: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        }
    }

    fn subtree_count(&self) -> Result<i64> {
        if self.kind == BTREE_KIND_LEAF {
            return codec::i64_from_usize(self.num);
        }
        let mut total = 0i64;
        for count in self.counts.iter().take(self.num) {
            total = codec::iadd(total, *count)?;
        }
        Ok(total)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BTreeNodeRef {
    pub node_ptr: u64,
    pub count: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BTreeInsertResult {
    pub node_ptr: u64,
    pub count: i64,
    pub value_pos: u64,
    pub split: Option<BTreeNodeRef>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BTreeWriteSlot {
    pub node_ptr: u64,
    pub value_pos: u64,
    pub slot: Slot,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BTreeJoinResult {
    pub node_ptr: u64,
    pub count: i64,
    pub split: Option<BTreeNodeRef>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BTreeSplitResult {
    pub left: u64,
    pub right: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SortedNode {
    pub kind: u8,
    pub num: usize,
    pub entries: [Slot; BTREE_SLOT_COUNT],
    pub children: [Slot; BTREE_SLOT_COUNT],
    pub separators: [Slot; BTREE_SLOT_COUNT],
    pub counts: [i64; BTREE_SLOT_COUNT],
}

/// Borrowed branch-node contents for the split helper.
struct BranchParts<'a> {
    children: &'a [Slot],
    separators: &'a [Slot],
    counts: &'a [i64],
}

impl SortedNode {
    /// Blank node of `num` entries (content filled by the caller).
    const fn sized(kind: u8, num: usize) -> Self {
        Self {
            kind,
            num,
            entries: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            separators: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        }
    }

    fn subtree_count(&self) -> Result<i64> {
        if self.kind == BTREE_KIND_LEAF {
            return codec::i64_from_usize(self.num);
        }
        let mut total = 0i64;
        for count in self.counts.iter().take(self.num) {
            total = codec::iadd(total, *count)?;
        }
        Ok(total)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SortedSplit {
    pub node_ptr: u64,
    pub count: i64,
    pub separator: Slot,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SortedInsertResult {
    pub node_ptr: u64,
    pub count: i64,
    pub entry: Slot,
    pub added: bool,
    pub split: Option<SortedSplit>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SortedRemoveResult {
    pub node_ptr: u64,
    pub found: bool,
}

/// Count-augmented B-tree + B+tree storage engine. Ports Go's
/// `btree*`/`sorted*` functions tier for tier; every mutation
/// path-copies, sharing untouched subtrees by pointer.
impl<C: Core, H: Hasher> Database<C, H> {
    pub(crate) fn read_btree_node(&self, ptr: u64) -> Result<BTreeNode> {
        let mut core = self.core.borrow_mut();
        core.seek_to(ptr)?;
        let mut head = [0u8; BTREE_NODE_HEADER];
        core.read(&mut head)?;
        let [kind, num] = head;
        if kind > BTREE_KIND_BRANCH {
            return Err(Error::InvalidBTreeNodeKind);
        }
        if usize::from(num) > BTREE_SLOT_COUNT {
            return Err(Error::InvalidBTreeNode);
        }
        let mut node = BTreeNode {
            kind,
            num: usize::from(num),
            values: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        };
        if kind == BTREE_KIND_LEAF {
            for slot in &mut node.values {
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                *slot = Slot::from_bytes(b);
            }
        } else {
            for slot in &mut node.children {
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                *slot = Slot::from_bytes(b);
            }
            for count in &mut node.counts {
                let mut b = [0u8; 8];
                core.read(&mut b)?;
                *count = i64::from_be_bytes(b);
            }
        }
        Ok(node)
    }

    fn write_btree_node_at(&self, node: &BTreeNode, ptr: u64) -> Result<()> {
        let size = if node.kind == BTREE_KIND_LEAF {
            BTREE_LEAF_BLOCK
        } else {
            BTREE_BRANCH_BLOCK
        };
        let num = u8::try_from(node.num).map_err(|_| Error::Corrupted)?;
        let mut buf = Vec::with_capacity(size);
        buf.extend_from_slice(&[node.kind, num]);
        if node.kind == BTREE_KIND_LEAF {
            for slot in &node.values {
                buf.extend_from_slice(&slot.to_bytes());
            }
        } else {
            for slot in &node.children {
                buf.extend_from_slice(&slot.to_bytes());
            }
            for count in &node.counts {
                buf.extend_from_slice(&count.to_be_bytes());
            }
        }
        let mut core = self.core.borrow_mut();
        core.seek_to(ptr)?;
        core.write(&buf)?;
        Ok(())
    }

    pub(crate) fn write_btree_node(&self, node: &BTreeNode) -> Result<u64> {
        let ptr = self.core.borrow_mut().committed_len()?;
        self.write_btree_node_at(node, ptr)?;
        Ok(ptr)
    }

    /// A node is safe to mutate in place when created in this transaction
    /// past the freeze point. Outside a transaction nothing is reusable.
    pub(crate) fn btree_reusable(&self, ptr: u64, frozen: u64) -> bool {
        self.is_old(ptr, frozen).is_ok_and(|old| !old)
    }

    pub(crate) fn btree_write_node(
        &self,
        node: &BTreeNode,
        old_ptr: u64,
        frozen: u64,
    ) -> Result<u64> {
        if self.btree_reusable(old_ptr, frozen) {
            self.write_btree_node_at(node, old_ptr)?;
            return Ok(old_ptr);
        }
        self.write_btree_node(node)
    }

    pub(crate) fn btree_new_root(&self) -> Result<u64> {
        self.write_btree_node(&BTreeNode {
            kind: BTREE_KIND_LEAF,
            num: 0,
            values: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        })
    }

    /// Descends to the slot at `rank`, decoding only counts and slots.
    /// Linked lists and sorted maps share it: identical leaves, branches
    /// starting with children and ending with counts.
    pub(crate) fn descend_by_rank(
        &self,
        root_ptr: u64,
        rank: i64,
        branch_block: usize,
    ) -> Result<SlotPointer> {
        let counts_pos = branch_block
            .checked_sub(BTREE_SLOT_COUNT.checked_mul(8).ok_or(Error::Corrupted)?)
            .ok_or(Error::Corrupted)?;
        let length = self.core.borrow_mut().committed_len()?;
        let mut node_ptr = root_ptr;
        let mut rem = rank;
        loop {
            let size = codec::usize_from_u64(
                codec::u64_from_usize(branch_block)?.min(codec::sub_u64(length, node_ptr)?),
            )?;
            if size < BTREE_LEAF_BLOCK {
                return Err(Error::EndOfStream);
            }
            let mut block = vec![0u8; branch_block];
            {
                let mut core = self.core.borrow_mut();
                core.seek_to(node_ptr)?;
                // A leaf may sit at the end of the file: unread tail stays
                // zeroed, exactly like the reference implementation.
                core.read(codec::sub_mut(&mut block, 0, size)?)?;
            }
            let kind = block.first().copied().ok_or(Error::Corrupted)?;
            let num_byte = block.get(1).copied().ok_or(Error::Corrupted)?;
            if kind > BTREE_KIND_BRANCH {
                return Err(Error::InvalidBTreeNodeKind);
            }
            let num = usize::from(num_byte);
            if num > BTREE_SLOT_COUNT {
                return Err(Error::InvalidBTreeNode);
            }
            if kind == BTREE_KIND_BRANCH && size < branch_block {
                return Err(Error::EndOfStream);
            }
            let mut i: usize = if kind == BTREE_KIND_LEAF {
                if rem >= codec::i64_from_usize(num)? {
                    return Err(Error::InvalidBTreeNode);
                }
                codec::usize_from_i64(rem)?
            } else {
                0
            };
            if kind != BTREE_KIND_LEAF {
                loop {
                    if i.checked_add(1).ok_or(Error::Corrupted)? >= num {
                        break;
                    }
                    let count_at = codec::add(
                        codec::u64_from_usize(counts_pos)?,
                        codec::mul(codec::u64_from_usize(i)?, 8)?,
                    )?;
                    let count = codec::i64_be(&block, codec::usize_from_u64(count_at)?)?;
                    if rem < count {
                        break;
                    }
                    rem = codec::isub(rem, count)?;
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
            }
            let at = codec::add(
                codec::NODE_HEADER_U64,
                codec::mul(codec::u64_from_usize(i)?, codec::SLOT_LEN_U64)?,
            )?;
            let mut b = [0u8; SLOT_LEN];
            b.copy_from_slice(codec::sub(&block, codec::usize_from_u64(at)?, SLOT_LEN)?);
            let slot = Slot::from_bytes(b);
            slot.validate()?;
            if kind == BTREE_KIND_LEAF {
                return Ok(SlotPointer {
                    position: Some(codec::add(node_ptr, at)?),
                    slot,
                });
            }
            node_ptr = codec::u64_from_i64(slot.value)?;
        }
    }

    /// Inserts a placeholder at `rank`, path-copying. The caller fills the
    /// value at `value_pos`.
    /// Inserts a placeholder into a leaf node, splitting on overflow.
    ///
    /// # Errors
    ///
    /// Propagates allocation, offset, and I/O failures.
    fn btree_insert_leaf(
        &self,
        node_ptr: u64,
        node: &BTreeNode,
        rank: i64,
        frozen: u64,
    ) -> Result<BTreeInsertResult> {
        let r = codec::usize_from_i64(rank)?;
        if r > node.num {
            return Err(Error::Corrupted);
        }
        // Placeholder spliced in at `r`: a full None slot still
        // counts as an element if the caller never fills it.
        let mut vals: Vec<Slot> = node.values.get(..r).ok_or(Error::Corrupted)?.to_vec();
        vals.push(Slot {
            value: 0,
            tag: Tag::None,
            full: true,
        });
        vals.extend(
            node.values
                .get(r..node.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        let total = vals.len();
        if total <= BTREE_SLOT_COUNT {
            let mut leaf = BTreeNode::sized(BTREE_KIND_LEAF, total);
            codec::copy_slots(leaf.values.get_mut(..total).ok_or(Error::Corrupted)?, &vals)?;
            let ptr = self.btree_write_node(&leaf, node_ptr, frozen)?;
            let value_pos = codec::add(
                ptr,
                codec::add(
                    codec::NODE_HEADER_U64,
                    codec::mul(codec::u64_from_usize(r)?, codec::SLOT_LEN_U64)?,
                )?,
            )?;
            return Ok(BTreeInsertResult {
                node_ptr: ptr,
                count: codec::i64_from_usize(total)?,
                value_pos,
                split: None,
            });
        }
        let left_n = BTREE_SPLIT_COUNT;
        let right_n = total.checked_sub(left_n).ok_or(Error::Corrupted)?;
        let mut left = BTreeNode::sized(BTREE_KIND_LEAF, left_n);
        codec::copy_slots(
            left.values.get_mut(..left_n).ok_or(Error::Corrupted)?,
            vals.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        let mut right = BTreeNode::sized(BTREE_KIND_LEAF, right_n);
        codec::copy_slots(
            right.values.get_mut(..right_n).ok_or(Error::Corrupted)?,
            vals.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        let left_ptr = self.btree_write_node(&left, node_ptr, frozen)?;
        let right_ptr = self.write_btree_node(&right)?;
        let at = |base: u64, at_rank: usize| -> Result<u64> {
            codec::add(
                base,
                codec::add(
                    codec::NODE_HEADER_U64,
                    codec::mul(codec::u64_from_usize(at_rank)?, codec::SLOT_LEN_U64)?,
                )?,
            )
        };
        let value_pos = if r < left_n {
            at(left_ptr, r)?
        } else {
            at(right_ptr, r.checked_sub(left_n).ok_or(Error::Corrupted)?)?
        };
        Ok(BTreeInsertResult {
            node_ptr: left_ptr,
            count: codec::i64_from_usize(left_n)?,
            value_pos,
            split: Some(BTreeNodeRef {
                node_ptr: right_ptr,
                count: codec::i64_from_usize(right_n)?,
            }),
        })
    }

    /// Inserts a placeholder into a branch node, splitting on overflow.
    ///
    /// # Errors
    ///
    /// Propagates allocation, offset, and I/O failures.
    fn btree_insert_branch(
        &self,
        node_ptr: u64,
        node: &BTreeNode,
        rank: i64,
        frozen: u64,
    ) -> Result<BTreeInsertResult> {
        let mut i: usize = 0;
        let mut rem = rank;
        loop {
            if i.checked_add(1).ok_or(Error::Corrupted)? >= node.num {
                break;
            }
            let count = node.counts.get(i).copied().ok_or(Error::Corrupted)?;
            if rem <= count {
                break;
            }
            rem = codec::isub(rem, count)?;
            i = i.checked_add(1).ok_or(Error::Corrupted)?;
        }
        let child =
            self.btree_insert(codec::child_ptr(node.children.as_slice(), i)?, rem, frozen)?;
        let mut children: Vec<Slot> = node
            .children
            .get(..node.num)
            .ok_or(Error::Corrupted)?
            .to_vec();
        let mut counts: Vec<i64> = node
            .counts
            .get(..node.num)
            .ok_or(Error::Corrupted)?
            .to_vec();
        *children.get_mut(i).ok_or(Error::Corrupted)? = Slot {
            value: codec::i64_from_u64(child.node_ptr)?,
            tag: Tag::Index,
            full: false,
        };
        *counts.get_mut(i).ok_or(Error::Corrupted)? = child.count;
        if let Some(split) = child.split {
            let at = i.checked_add(1).ok_or(Error::Corrupted)?;
            if at > children.len() {
                return Err(Error::Corrupted);
            }
            children.insert(
                at,
                Slot {
                    value: codec::i64_from_u64(split.node_ptr)?,
                    tag: Tag::Index,
                    full: false,
                },
            );
            counts.insert(at, split.count);
        }
        let total = children.len();
        if total <= BTREE_SLOT_COUNT {
            let mut branch = BTreeNode::sized(BTREE_KIND_BRANCH, total);
            codec::copy_slots(
                branch.children.get_mut(..total).ok_or(Error::Corrupted)?,
                children.get(..total).ok_or(Error::Corrupted)?,
            )?;
            codec::copy_counts(
                branch.counts.get_mut(..total).ok_or(Error::Corrupted)?,
                counts.get(..total).ok_or(Error::Corrupted)?,
            )?;
            let ptr = self.btree_write_node(&branch, node_ptr, frozen)?;
            return Ok(BTreeInsertResult {
                node_ptr: ptr,
                count: branch.subtree_count()?,
                value_pos: child.value_pos,
                split: None,
            });
        }
        let left_n = BTREE_SPLIT_COUNT;
        let right_n = total.checked_sub(left_n).ok_or(Error::Corrupted)?;
        let mut left = BTreeNode::sized(BTREE_KIND_BRANCH, left_n);
        codec::copy_slots(
            left.children.get_mut(..left_n).ok_or(Error::Corrupted)?,
            children.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_counts(
            left.counts.get_mut(..left_n).ok_or(Error::Corrupted)?,
            counts.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        let mut right = BTreeNode::sized(BTREE_KIND_BRANCH, right_n);
        codec::copy_slots(
            right.children.get_mut(..right_n).ok_or(Error::Corrupted)?,
            children.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_counts(
            right.counts.get_mut(..right_n).ok_or(Error::Corrupted)?,
            counts.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        let left_ptr = self.btree_write_node(&left, node_ptr, frozen)?;
        let right_ptr = self.write_btree_node(&right)?;
        let right_count = right.subtree_count()?;
        Ok(BTreeInsertResult {
            node_ptr: left_ptr,
            count: left.subtree_count()?,
            value_pos: child.value_pos,
            split: Some(BTreeNodeRef {
                node_ptr: right_ptr,
                count: right_count,
            }),
        })
    }

    pub(crate) fn btree_insert(
        &self,
        node_ptr: u64,
        rank: i64,
        frozen: u64,
    ) -> Result<BTreeInsertResult> {
        let node = self.read_btree_node(node_ptr)?;
        match node.kind {
            BTREE_KIND_LEAF => self.btree_insert_leaf(node_ptr, &node, rank, frozen),
            BTREE_KIND_BRANCH => self.btree_insert_branch(node_ptr, &node, rank, frozen),
            _ => Err(Error::InvalidBTreeNodeKind),
        }
    }

    pub(crate) fn btree_grow_root(&self, result: BTreeInsertResult) -> Result<u64> {
        self.btree_make_root(BTreeJoinResult {
            node_ptr: result.node_ptr,
            count: result.count,
            split: result.split,
        })
    }

    /// Descends to the value slot at `rank` for writing, path-copying only
    /// nodes from past transactions.
    pub(crate) fn btree_get_for_write(
        &self,
        node_ptr: u64,
        rank: i64,
        frozen: u64,
    ) -> Result<BTreeWriteSlot> {
        let node = self.read_btree_node(node_ptr)?;
        if node.kind == BTREE_KIND_LEAF {
            let new_ptr = if self.btree_reusable(node_ptr, frozen) {
                node_ptr
            } else {
                self.write_btree_node(&node)?
            };
            let rank_idx = codec::usize_from_i64(rank)?;
            return Ok(BTreeWriteSlot {
                node_ptr: new_ptr,
                value_pos: codec::add(
                    new_ptr,
                    codec::add(
                        codec::NODE_HEADER_U64,
                        codec::mul(codec::u64_from_usize(rank_idx)?, codec::SLOT_LEN_U64)?,
                    )?,
                )?,
                slot: codec::slot_at_arr(&node.values, rank_idx)?,
            });
        }
        let mut i: usize = 0;
        let mut rem = rank;
        loop {
            if i.checked_add(1).ok_or(Error::Corrupted)? >= node.num {
                break;
            }
            let count = node.counts.get(i).copied().ok_or(Error::Corrupted)?;
            if rem < count {
                break;
            }
            rem = codec::isub(rem, count)?;
            i = i.checked_add(1).ok_or(Error::Corrupted)?;
        }
        let child_ptr =
            codec::u64_from_i64(node.children.get(i).copied().ok_or(Error::Corrupted)?.value)?;
        let child = self.btree_get_for_write(child_ptr, rem, frozen)?;
        if child.node_ptr == child_ptr {
            return Ok(BTreeWriteSlot {
                node_ptr,
                value_pos: child.value_pos,
                slot: child.slot,
            });
        }
        let mut node = node;
        codec::set_slot_at(
            &mut node.children,
            i,
            Slot {
                value: codec::i64_from_u64(child.node_ptr)?,
                tag: Tag::Index,
                full: false,
            },
        )?;
        let new_ptr = self.btree_write_node(&node, node_ptr, frozen)?;
        Ok(BTreeWriteSlot {
            node_ptr: new_ptr,
            value_pos: child.value_pos,
            slot: child.slot,
        })
    }

    /// Removes the element at `rank`, returning the node left behind.
    pub(crate) fn btree_remove(&self, node_ptr: u64, rank: i64, frozen: u64) -> Result<BTreeNode> {
        let _ = frozen;
        let mut node = self.read_btree_node(node_ptr)?;
        match node.kind {
            BTREE_KIND_LEAF => {
                let r = codec::usize_from_i64(rank)?;
                if r >= node.num {
                    return Err(Error::Corrupted);
                }
                let mut vals: Vec<Slot> = node
                    .values
                    .get(..node.num)
                    .ok_or(Error::Corrupted)?
                    .to_vec();
                vals.remove(r);
                while vals.len() < BTREE_SLOT_COUNT {
                    vals.push(empty_slot());
                }
                node.values = vals.try_into().map_err(|_: Vec<Slot>| Error::Corrupted)?;
                node.num = node.num.checked_sub(1).ok_or(Error::Corrupted)?;
                Ok(node)
            }
            BTREE_KIND_BRANCH => {
                let mut i: usize = 0;
                let mut rem = rank;
                loop {
                    if i.checked_add(1).ok_or(Error::Corrupted)? >= node.num {
                        break;
                    }
                    let count = node.counts.get(i).copied().ok_or(Error::Corrupted)?;
                    if rem < count {
                        break;
                    }
                    rem = codec::isub(rem, count)?;
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
                let child_ptr = codec::u64_from_i64(
                    node.children.get(i).copied().ok_or(Error::Corrupted)?.value,
                )?;
                let mut child = self.btree_remove(child_ptr, rem, frozen)?;
                if child.num == 0 {
                    btree_drop_child(&mut node, i)?;
                    return Ok(node);
                }
                if child.num < BTREE_SPLIT_COUNT && node.num > 1 {
                    let first = if i.checked_add(1).ok_or(Error::Corrupted)? >= node.num {
                        i.checked_sub(1).ok_or(Error::Corrupted)?
                    } else {
                        i
                    };
                    let neighbor_i = first;
                    let neighbor = self.read_btree_node(codec::u64_from_i64(
                        codec::slot_at_arr(&node.children, neighbor_i)?.value,
                    )?)?;
                    if codec::add(
                        codec::u64_from_usize(child.num)?,
                        codec::u64_from_usize(neighbor.num)?,
                    )? <= codec::u64_from_usize(BTREE_SLOT_COUNT)?
                    {
                        child = if first == i {
                            btree_concat_nodes(&child, &neighbor)?
                        } else {
                            btree_concat_nodes(&neighbor, &child)?
                        };
                        btree_drop_child(&mut node, first.checked_add(1).ok_or(Error::Corrupted)?)?;
                        i = first;
                    }
                }
                let ptr = self.btree_write_node(&child, child_ptr, frozen)?;
                codec::set_slot_at(
                    &mut node.children,
                    i,
                    Slot {
                        value: crate::codec::i64_from_u64(ptr)?,
                        tag: Tag::Index,
                        full: false,
                    },
                )?;
                codec::set_count_at(&mut node.counts, i, child.subtree_count()?)?;
                Ok(node)
            }
            _ => Err(Error::InvalidBTreeNodeKind),
        }
    }

    fn btree_height(&self, mut ptr: u64) -> Result<usize> {
        let mut height = 0;
        loop {
            let node = self.read_btree_node(ptr)?;
            if node.kind == BTREE_KIND_LEAF {
                return Ok(height);
            }
            height = height.checked_add(1).ok_or(Error::Corrupted)?;
            ptr = codec::child_ptr(node.children.as_slice(), 0)?;
        }
    }

    fn btree_make_root(&self, result: BTreeJoinResult) -> Result<u64> {
        if let Some(split) = result.split {
            let mut root = BTreeNode {
                kind: BTREE_KIND_BRANCH,
                num: 2,
                values: [empty_slot(); BTREE_SLOT_COUNT],
                children: [empty_slot(); BTREE_SLOT_COUNT],
                counts: [0; BTREE_SLOT_COUNT],
            };
            codec::set_slot_at(
                &mut root.children,
                0,
                Slot {
                    value: codec::i64_from_u64(result.node_ptr)?,
                    tag: Tag::Index,
                    full: false,
                },
            )?;
            codec::set_slot_at(
                &mut root.children,
                1,
                Slot {
                    value: codec::i64_from_u64(split.node_ptr)?,
                    tag: Tag::Index,
                    full: false,
                },
            )?;
            codec::set_count_at(&mut root.counts, 0, result.count)?;
            codec::set_count_at(&mut root.counts, 1, split.count)?;
            return self.write_btree_node(&root);
        }
        Ok(result.node_ptr)
    }

    fn btree_assemble_leaf(&self, vals: &[Slot]) -> Result<BTreeJoinResult> {
        let total = vals.len();
        if total <= BTREE_SLOT_COUNT {
            let mut leaf = BTreeNode::sized(BTREE_KIND_LEAF, total);
            codec::copy_slots(leaf.values.get_mut(..total).ok_or(Error::Corrupted)?, vals)?;
            let ptr = self.write_btree_node(&leaf)?;
            return Ok(BTreeJoinResult {
                node_ptr: ptr,
                count: codec::i64_from_usize(total)?,
                split: None,
            });
        }
        let left_n = total / 2;
        let mut left = BTreeNode::sized(BTREE_KIND_LEAF, left_n);
        codec::copy_slots(
            left.values.get_mut(..left_n).ok_or(Error::Corrupted)?,
            vals.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        let mut right = BTreeNode {
            kind: BTREE_KIND_LEAF,
            num: total.checked_sub(left_n).ok_or(Error::Corrupted)?,
            values: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        };
        codec::copy_slots(
            right.values.get_mut(..right.num).ok_or(Error::Corrupted)?,
            vals.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        let left_ptr = self.write_btree_node(&left)?;
        let right_ptr = self.write_btree_node(&right)?;
        Ok(BTreeJoinResult {
            node_ptr: left_ptr,
            count: codec::i64_from_usize(left_n)?,
            split: Some(BTreeNodeRef {
                node_ptr: right_ptr,
                count: codec::i64_from_usize(right.num)?,
            }),
        })
    }

    fn btree_assemble_branch(&self, children: &[Slot], counts: &[i64]) -> Result<BTreeJoinResult> {
        let total = children.len();
        if total <= BTREE_SLOT_COUNT {
            let mut branch = BTreeNode::sized(BTREE_KIND_BRANCH, total);
            codec::copy_slots(
                branch.children.get_mut(..total).ok_or(Error::Corrupted)?,
                children.get(..total).ok_or(Error::Corrupted)?,
            )?;
            codec::copy_counts(
                branch.counts.get_mut(..total).ok_or(Error::Corrupted)?,
                counts.get(..total).ok_or(Error::Corrupted)?,
            )?;
            let ptr = self.write_btree_node(&branch)?;
            return Ok(BTreeJoinResult {
                node_ptr: ptr,
                count: branch.subtree_count()?,
                split: None,
            });
        }
        let left_n = total / 2;
        let mut left = BTreeNode::sized(BTREE_KIND_BRANCH, left_n);
        codec::copy_slots(
            left.children.get_mut(..left_n).ok_or(Error::Corrupted)?,
            children.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_counts(
            left.counts.get_mut(..left_n).ok_or(Error::Corrupted)?,
            counts.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        let mut right = BTreeNode {
            kind: BTREE_KIND_BRANCH,
            num: total.checked_sub(left_n).ok_or(Error::Corrupted)?,
            values: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        };
        codec::copy_slots(
            right
                .children
                .get_mut(..right.num)
                .ok_or(Error::Corrupted)?,
            children.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_counts(
            right.counts.get_mut(..right.num).ok_or(Error::Corrupted)?,
            counts.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        let left_ptr = self.write_btree_node(&left)?;
        let right_ptr = self.write_btree_node(&right)?;
        let right_count = right.subtree_count()?;
        Ok(BTreeJoinResult {
            node_ptr: left_ptr,
            count: left.subtree_count()?,
            split: Some(BTreeNodeRef {
                node_ptr: right_ptr,
                count: right_count,
            }),
        })
    }

    fn btree_merge_nodes(&self, a: &BTreeNode, b: &BTreeNode) -> Result<BTreeJoinResult> {
        if a.kind == BTREE_KIND_LEAF {
            let mut vals = Vec::new();
            vals.extend(
                a.values
                    .get(..a.num)
                    .ok_or(Error::Corrupted)?
                    .iter()
                    .copied(),
            );
            vals.extend(
                b.values
                    .get(..b.num)
                    .ok_or(Error::Corrupted)?
                    .iter()
                    .copied(),
            );
            return self.btree_assemble_leaf(&vals);
        }
        let mut children = Vec::new();
        let mut counts = Vec::new();
        children.extend(
            a.children
                .get(..a.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        children.extend(
            b.children
                .get(..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        counts.extend(
            a.counts
                .get(..a.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        counts.extend(
            b.counts
                .get(..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        self.btree_assemble_branch(&children, &counts)
    }

    fn btree_join_right(
        &self,
        a_ptr: u64,
        ha: usize,
        b_ptr: u64,
        hb: usize,
    ) -> Result<BTreeJoinResult> {
        let a = self.read_btree_node(a_ptr)?;
        let last = a.num.checked_sub(1).ok_or(Error::Corrupted)?;
        let sub = if ha.checked_sub(1).ok_or(Error::Corrupted)? == hb {
            let last_child =
                self.read_btree_node(codec::child_ptr(a.children.as_slice(), last)?)?;
            let b_node = self.read_btree_node(b_ptr)?;
            self.btree_merge_nodes(&last_child, &b_node)?
        } else {
            self.btree_join_right(
                codec::child_ptr(a.children.as_slice(), last)?,
                ha.checked_sub(1).ok_or(Error::Corrupted)?,
                b_ptr,
                hb,
            )?
        };
        let mut children = a.children.get(..a.num).ok_or(Error::Corrupted)?.to_vec();
        let mut counts = a.counts.get(..a.num).ok_or(Error::Corrupted)?.to_vec();
        codec::set_slot_at(
            &mut children,
            last,
            Slot {
                value: codec::i64_from_u64(sub.node_ptr)?,
                tag: Tag::Index,
                full: false,
            },
        )?;
        codec::set_count_at(&mut counts, last, sub.count)?;
        if let Some(split) = sub.split {
            children.push(Slot {
                value: codec::i64_from_u64(split.node_ptr)?,
                tag: Tag::Index,
                full: false,
            });
            counts.push(split.count);
        }
        self.btree_assemble_branch(&children, &counts)
    }

    fn btree_join_left(
        &self,
        a_ptr: u64,
        ha: usize,
        b_ptr: u64,
        hb: usize,
    ) -> Result<BTreeJoinResult> {
        let b = self.read_btree_node(b_ptr)?;
        let sub = if hb.checked_sub(1).ok_or(Error::Corrupted)? == ha {
            let a_node = self.read_btree_node(a_ptr)?;
            let first_child = self.read_btree_node(codec::child_ptr(b.children.as_slice(), 0)?)?;
            self.btree_merge_nodes(&a_node, &first_child)?
        } else {
            self.btree_join_left(
                a_ptr,
                ha,
                codec::child_ptr(b.children.as_slice(), 0)?,
                hb.checked_sub(1).ok_or(Error::Corrupted)?,
            )?
        };
        let mut children = vec![Slot {
            value: codec::i64_from_u64(sub.node_ptr)?,
            tag: Tag::Index,
            full: false,
        }];
        let mut counts = vec![sub.count];
        if let Some(split) = sub.split {
            children.push(Slot {
                value: codec::i64_from_u64(split.node_ptr)?,
                tag: Tag::Index,
                full: false,
            });
            counts.push(split.count);
        }
        children.extend(
            b.children
                .get(1..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        counts.extend(
            b.counts
                .get(1..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        self.btree_assemble_branch(&children, &counts)
    }

    pub(crate) fn btree_join(&self, root_a: u64, root_b: u64) -> Result<u64> {
        let ha = self.btree_height(root_a)?;
        let hb = self.btree_height(root_b)?;
        let result = match ha.cmp(&hb) {
            std::cmp::Ordering::Equal => {
                let a = self.read_btree_node(root_a)?;
                let b = self.read_btree_node(root_b)?;
                self.btree_merge_nodes(&a, &b)?
            }
            std::cmp::Ordering::Greater => self.btree_join_right(root_a, ha, root_b, hb)?,
            std::cmp::Ordering::Less => self.btree_join_left(root_a, ha, root_b, hb)?,
        };
        self.btree_make_root(result)
    }

    fn btree_subtree(
        &self,
        children: &[Slot],
        counts: &[i64],
        start: usize,
        length: usize,
    ) -> Result<u64> {
        if length == 0 {
            return self.btree_new_root();
        }
        if length == 1 {
            return codec::child_ptr(children, start);
        }
        let res = self.btree_assemble_branch(
            children
                .get(start..start.checked_add(length).ok_or(Error::Corrupted)?)
                .ok_or(Error::Corrupted)?,
            counts
                .get(start..start.checked_add(length).ok_or(Error::Corrupted)?)
                .ok_or(Error::Corrupted)?,
        )?;
        Ok(res.node_ptr)
    }

    pub(crate) fn btree_split(&self, root_ptr: u64, rank: i64) -> Result<BTreeSplitResult> {
        let node = self.read_btree_node(root_ptr)?;
        if node.kind == BTREE_KIND_LEAF {
            let r = codec::usize_from_i64(rank)?;
            if r > node.num {
                return Err(Error::Corrupted);
            }
            let mut left = BTreeNode::sized(BTREE_KIND_LEAF, r);
            codec::copy_slots(
                left.values.get_mut(..r).ok_or(Error::Corrupted)?,
                node.values.get(..r).ok_or(Error::Corrupted)?,
            )?;
            let mut right = BTreeNode {
                kind: BTREE_KIND_LEAF,
                num: node.num.checked_sub(r).ok_or(Error::Corrupted)?,
                values: [empty_slot(); BTREE_SLOT_COUNT],
                children: [empty_slot(); BTREE_SLOT_COUNT],
                counts: [0; BTREE_SLOT_COUNT],
            };
            codec::copy_slots(
                right.values.get_mut(..right.num).ok_or(Error::Corrupted)?,
                node.values.get(r..node.num).ok_or(Error::Corrupted)?,
            )?;
            let left_ptr = self.write_btree_node(&left)?;
            let right_ptr = self.write_btree_node(&right)?;
            return Ok(BTreeSplitResult {
                left: left_ptr,
                right: right_ptr,
            });
        }
        let mut i: usize = 0;
        let mut rem = rank;
        loop {
            if i.checked_add(1).ok_or(Error::Corrupted)? >= node.num {
                break;
            }
            let count = node.counts.get(i).copied().ok_or(Error::Corrupted)?;
            if rem <= count {
                break;
            }
            rem = codec::isub(rem, count)?;
            i = i.checked_add(1).ok_or(Error::Corrupted)?;
        }
        let child = self.btree_split(codec::child_ptr(node.children.as_slice(), i)?, rem)?;
        let left_sub = self.btree_subtree(&node.children, &node.counts, 0, i)?;
        let right_sub = self.btree_subtree(
            &node.children,
            &node.counts,
            i.checked_add(1).ok_or(Error::Corrupted)?,
            node.num
                .checked_sub(i.checked_add(1).ok_or(Error::Corrupted)?)
                .ok_or(Error::Corrupted)?,
        )?;
        let joined_left = self.btree_join(left_sub, child.left)?;
        let joined_right = self.btree_join(child.right, right_sub)?;
        Ok(BTreeSplitResult {
            left: joined_left,
            right: joined_right,
        })
    }

    pub(crate) fn read_sorted_node(&self, ptr: u64) -> Result<SortedNode> {
        let mut core = self.core.borrow_mut();
        core.seek_to(ptr)?;
        let mut head = [0u8; BTREE_NODE_HEADER];
        core.read(&mut head)?;
        let [kind, num] = head;
        if kind > BTREE_KIND_BRANCH {
            return Err(Error::InvalidBTreeNodeKind);
        }
        if usize::from(num) > BTREE_SLOT_COUNT {
            return Err(Error::InvalidBTreeNode);
        }
        let mut node = SortedNode {
            kind,
            num: usize::from(num),
            entries: [empty_slot(); BTREE_SLOT_COUNT],
            children: [empty_slot(); BTREE_SLOT_COUNT],
            separators: [empty_slot(); BTREE_SLOT_COUNT],
            counts: [0; BTREE_SLOT_COUNT],
        };
        if kind == BTREE_KIND_LEAF {
            for slot in &mut node.entries {
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                *slot = Slot::from_bytes(b);
            }
        } else {
            for slot in &mut node.children {
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                *slot = Slot::from_bytes(b);
            }
            for slot in &mut node.separators {
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                *slot = Slot::from_bytes(b);
            }
            for count in &mut node.counts {
                let mut b = [0u8; 8];
                core.read(&mut b)?;
                *count = i64::from_be_bytes(b);
            }
        }
        Ok(node)
    }

    fn write_sorted_node_at(&self, node: &SortedNode, ptr: u64) -> Result<()> {
        let size = if node.kind == BTREE_KIND_LEAF {
            SORTED_LEAF_BLOCK
        } else {
            SORTED_BRANCH_BLOCK
        };
        let num = u8::try_from(node.num).map_err(|_| Error::Corrupted)?;
        let mut buf = Vec::with_capacity(size);
        buf.extend_from_slice(&[node.kind, num]);
        if node.kind == BTREE_KIND_LEAF {
            for slot in &node.entries {
                buf.extend_from_slice(&slot.to_bytes());
            }
        } else {
            for slot in &node.children {
                buf.extend_from_slice(&slot.to_bytes());
            }
            for slot in &node.separators {
                buf.extend_from_slice(&slot.to_bytes());
            }
            for count in &node.counts {
                buf.extend_from_slice(&count.to_be_bytes());
            }
        }
        let mut core = self.core.borrow_mut();
        core.seek_to(ptr)?;
        core.write(&buf)?;
        Ok(())
    }

    pub(crate) fn write_sorted_node(&self, node: &SortedNode) -> Result<u64> {
        let ptr = self.core.borrow_mut().committed_len()?;
        self.write_sorted_node_at(node, ptr)?;
        Ok(ptr)
    }

    fn sorted_write_node(&self, node: &SortedNode, old_ptr: u64, frozen: u64) -> Result<u64> {
        if self.btree_reusable(old_ptr, frozen) {
            self.write_sorted_node_at(node, old_ptr)?;
            return Ok(old_ptr);
        }
        self.write_sorted_node(node)
    }

    /// Lexicographic comparison of a stored byte key against `target`.
    pub(crate) fn compare_key(&self, key_slot: Slot, target: &[u8]) -> Result<std::cmp::Ordering> {
        match key_slot.tag {
            Tag::ShortBytes => {
                let bytes = codec::u64_from_i64(key_slot.value)?.to_be_bytes();
                let total = if key_slot.full { 6 } else { 8 };
                let mut len = total;
                for (i, b) in bytes.iter().enumerate().take(total) {
                    if *b == 0 {
                        len = i;
                        break;
                    }
                }
                Ok(codec::sub(&bytes, 0, len)?.cmp(target))
            }
            Tag::Bytes => {
                let mut core = self.core.borrow_mut();
                core.seek_to(codec::u64_from_i64(key_slot.value)?)?;
                let mut size_bytes = [0u8; 8];
                core.read(&mut size_bytes)?;
                let size = codec::usize_from_u64(u64::from_be_bytes(size_bytes))?;
                let mut key = vec![0u8; size];
                core.read(&mut key)?;
                Ok(key.as_slice().cmp(target))
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    pub(crate) fn sorted_branch_search(&self, node: &SortedNode, key: &[u8]) -> Result<usize> {
        let mut lo = 1;
        let mut hi = node.num;
        while lo < hi {
            let mid = lo.midpoint(hi);
            if self.compare_key(codec::slot_at_arr(&node.separators, mid)?, key)?
                == std::cmp::Ordering::Greater
            {
                hi = mid;
            } else {
                lo = mid.checked_add(1).ok_or(Error::Corrupted)?;
            }
        }
        lo.checked_sub(1).ok_or(Error::Corrupted)
    }

    pub(crate) fn sorted_leaf_search(
        &self,
        node: &SortedNode,
        key: &[u8],
    ) -> Result<(usize, bool)> {
        let mut lo = 0;
        let mut hi = node.num;
        while lo < hi {
            let mid = lo.midpoint(hi);
            let kv = self.read_kv_pair(codec::slot_at_arr(&node.entries, mid)?)?;
            match self.compare_key(kv.0, key)? {
                std::cmp::Ordering::Equal => return Ok((mid, true)),
                std::cmp::Ordering::Less => lo = mid.checked_add(1).ok_or(Error::Corrupted)?,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok((lo, false))
    }

    pub(crate) fn sorted_get(&self, root_ptr: u64, key: &[u8]) -> Result<Option<Slot>> {
        let mut node_ptr = root_ptr;
        loop {
            let node = self.read_sorted_node(node_ptr)?;
            if node.kind == BTREE_KIND_LEAF {
                let (idx, found) = self.sorted_leaf_search(&node, key)?;
                if !found {
                    return Ok(None);
                }
                return Ok(Some(codec::slot_at_arr(&node.entries, idx)?));
            }
            let i = self.sorted_branch_search(&node, key)?;
            node_ptr = codec::child_ptr(node.children.as_slice(), i)?;
        }
    }

    pub(crate) fn sorted_rank(&self, root_ptr: u64, key: &[u8]) -> Result<i64> {
        let mut node_ptr = root_ptr;
        let mut rank = 0i64;
        loop {
            let node = self.read_sorted_node(node_ptr)?;
            if node.kind == BTREE_KIND_LEAF {
                let (idx, _) = self.sorted_leaf_search(&node, key)?;
                return codec::iadd(rank, codec::i64_from_usize(idx)?);
            }
            let i = self.sorted_branch_search(&node, key)?;
            for c in node.counts.get(..i).ok_or(Error::Corrupted)? {
                rank = codec::iadd(rank, *c)?;
            }
            node_ptr = codec::child_ptr(node.children.as_slice(), i)?;
        }
    }

    /// Writes a byte key as inline short-bytes (≤8 bytes, no interior
    /// zero) or an external bytes slot.
    pub(crate) fn write_key(&self, key: &[u8]) -> Result<Slot> {
        if key.len() <= 8 && !key.contains(&0) {
            let mut value = [0u8; 8];
            value
                .get_mut(..key.len())
                .ok_or(Error::Corrupted)?
                .copy_from_slice(key);
            return Ok(Slot {
                value: codec::i64_from_u64(u64::from_be_bytes(value))?,
                tag: Tag::ShortBytes,
                full: false,
            });
        }
        let pos = self.core.borrow_mut().committed_len()?;
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(pos)?;
            core.write(&codec::u64_from_usize(key.len())?.to_be_bytes())?;
            core.write(key)?;
        }
        Ok(Slot {
            value: crate::codec::i64_from_u64(pos)?,
            tag: Tag::Bytes,
            full: false,
        })
    }

    /// Wraps a key slot in a key-value pair with an empty value. Navigation
    /// is by key bytes, so the pair carries no hash.
    pub(crate) fn write_sorted_pair(&self, key_slot: Slot) -> Result<Slot> {
        self.encode_kv_pair(
            Tag::SortedKVPair,
            &vec![0u8; usize::from(self.header().hash_size)],
            key_slot,
            empty_slot(),
        )
    }

    /// Writes a `KVPair`/`SortedKVPair` entry at the end, returning its slot.
    /// Sorted pairs leave out the hash.
    pub(crate) fn encode_kv_pair(
        &self,
        tag: Tag,
        hash: &[u8],
        key: Slot,
        value: Slot,
    ) -> Result<Slot> {
        let pos = self.core.borrow_mut().committed_len()?;
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(pos)?;
            if tag == Tag::KVPair {
                core.write(hash)?;
            }
            core.write(&key.to_bytes())?;
            core.write(&value.to_bytes())?;
        }
        Ok(Slot {
            value: crate::codec::i64_from_u64(pos)?,
            tag,
            full: false,
        })
    }

    /// Inserts or replaces a key in a leaf node, splitting on overflow.
    ///
    /// # Errors
    ///
    /// Propagates allocation, offset, and I/O failures.
    fn sorted_put_leaf(
        &self,
        node_ptr: u64,
        node: &SortedNode,
        key: &[u8],
        key_only: bool,
        frozen: u64,
    ) -> Result<SortedInsertResult> {
        let (idx, found) = self.sorted_leaf_search(node, key)?;
        if found {
            if key_only {
                return Ok(SortedInsertResult {
                    node_ptr,
                    count: codec::i64_from_usize(node.num)?,
                    entry: codec::slot_at_arr(&node.entries, idx)?,
                    added: false,
                    split: None,
                });
            }
            let mut leaf = *node;
            let mut entry = codec::slot_at_arr(&node.entries, idx)?;
            if matches!(entry.tag, Tag::Bytes | Tag::ShortBytes) {
                entry = self.write_sorted_pair(entry)?;
            } else if !self.btree_reusable(codec::u64_from_i64(entry.value)?, frozen) {
                let (k, v, h) = self.read_kv_pair(entry)?;
                entry = self.encode_kv_pair(entry.tag, &h, k, v)?;
            }
            codec::set_slot_at(&mut leaf.entries, idx, entry)?;
            let ptr = self.sorted_write_node(&leaf, node_ptr, frozen)?;
            return Ok(SortedInsertResult {
                node_ptr: ptr,
                count: codec::i64_from_usize(node.num)?,
                entry,
                added: false,
                split: None,
            });
        }
        let mut entry = self.write_key(key)?;
        if !key_only {
            entry = self.write_sorted_pair(entry)?;
        }
        let mut entries: Vec<Slot> = node.entries.get(..idx).ok_or(Error::Corrupted)?.to_vec();
        entries.push(entry);
        entries.extend(
            node.entries
                .get(idx..node.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        let total = entries.len();
        if total <= BTREE_SLOT_COUNT {
            let mut leaf = SortedNode::sized(BTREE_KIND_LEAF, total);
            codec::copy_slots(
                leaf.entries.get_mut(..total).ok_or(Error::Corrupted)?,
                &entries,
            )?;
            let ptr = self.sorted_write_node(&leaf, node_ptr, frozen)?;
            return Ok(SortedInsertResult {
                node_ptr: ptr,
                count: codec::i64_from_usize(total)?,
                entry,
                added: true,
                split: None,
            });
        }
        self.sorted_split_leaf(node_ptr, &entries, entry, total, frozen)
    }

    /// Splits an overfull leaf's entries into two leaves.
    ///
    /// # Errors
    ///
    /// Propagates allocation, offset, and I/O failures.
    fn sorted_split_leaf(
        &self,
        node_ptr: u64,
        entries: &[Slot],
        entry: Slot,
        total: usize,
        frozen: u64,
    ) -> Result<SortedInsertResult> {
        let left_n = BTREE_SPLIT_COUNT;
        let right_n = total.checked_sub(left_n).ok_or(Error::Corrupted)?;
        let mut left = SortedNode::sized(BTREE_KIND_LEAF, left_n);
        codec::copy_slots(
            left.entries.get_mut(..left_n).ok_or(Error::Corrupted)?,
            entries.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        let mut right = SortedNode::sized(BTREE_KIND_LEAF, right_n);
        codec::copy_slots(
            right.entries.get_mut(..right_n).ok_or(Error::Corrupted)?,
            entries.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        let (sep_key, _, _) =
            self.read_kv_pair(entries.get(left_n).copied().ok_or(Error::Corrupted)?)?;
        let left_ptr = self.sorted_write_node(&left, node_ptr, frozen)?;
        let right_ptr = self.write_sorted_node(&right)?;
        Ok(SortedInsertResult {
            node_ptr: left_ptr,
            count: codec::i64_from_usize(left_n)?,
            entry,
            added: true,
            split: Some(SortedSplit {
                node_ptr: right_ptr,
                count: codec::i64_from_usize(right_n)?,
                separator: sep_key,
            }),
        })
    }

    /// Inserts or replaces a key in a branch node, splitting on overflow.
    ///
    /// # Errors
    ///
    /// Propagates allocation, offset, and I/O failures.
    /// Assembles two sorted branch halves after an overflow split.
    ///
    /// # Errors
    ///
    /// Propagates allocation, offset, and I/O failures.
    fn sorted_split_branch_vecs(
        &self,
        node_ptr: u64,
        parts: &BranchParts<'_>,
        entry: Slot,
        added: bool,
        frozen: u64,
    ) -> Result<SortedInsertResult> {
        let total = parts.children.len();
        let left_n = BTREE_SPLIT_COUNT;
        let right_n = total.checked_sub(left_n).ok_or(Error::Corrupted)?;
        let mut left = SortedNode::sized(BTREE_KIND_BRANCH, left_n);
        codec::copy_slots(
            left.children.get_mut(..left_n).ok_or(Error::Corrupted)?,
            parts.children.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_slots(
            left.separators.get_mut(..left_n).ok_or(Error::Corrupted)?,
            parts.separators.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_counts(
            left.counts.get_mut(..left_n).ok_or(Error::Corrupted)?,
            parts.counts.get(..left_n).ok_or(Error::Corrupted)?,
        )?;
        let mut right = SortedNode::sized(BTREE_KIND_BRANCH, right_n);
        codec::copy_slots(
            right.children.get_mut(..right_n).ok_or(Error::Corrupted)?,
            parts.children.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        codec::copy_slots(
            right
                .separators
                .get_mut(..right_n)
                .ok_or(Error::Corrupted)?,
            parts
                .separators
                .get(left_n..total)
                .ok_or(Error::Corrupted)?,
        )?;
        codec::copy_counts(
            right.counts.get_mut(..right_n).ok_or(Error::Corrupted)?,
            parts.counts.get(left_n..total).ok_or(Error::Corrupted)?,
        )?;
        let separator = parts
            .separators
            .get(left_n)
            .copied()
            .ok_or(Error::Corrupted)?;
        let left_ptr = self.sorted_write_node(&left, node_ptr, frozen)?;
        let right_ptr = self.write_sorted_node(&right)?;
        let right_count = right.subtree_count()?;
        Ok(SortedInsertResult {
            node_ptr: left_ptr,
            count: left.subtree_count()?,
            entry,
            added,
            split: Some(SortedSplit {
                node_ptr: right_ptr,
                count: right_count,
                separator,
            }),
        })
    }

    fn sorted_put_branch(
        &self,
        node_ptr: u64,
        node: &SortedNode,
        key: &[u8],
        key_only: bool,
        frozen: u64,
    ) -> Result<SortedInsertResult> {
        let i = self.sorted_branch_search(node, key)?;
        let child = self.sorted_put(
            codec::child_ptr(node.children.as_slice(), i)?,
            key,
            key_only,
            frozen,
        )?;
        if !child.added && child.node_ptr == codec::child_ptr(node.children.as_slice(), i)? {
            return Ok(SortedInsertResult {
                node_ptr,
                count: node.subtree_count()?,
                entry: child.entry,
                added: false,
                split: None,
            });
        }
        let mut children: Vec<Slot> = node
            .children
            .get(..node.num)
            .ok_or(Error::Corrupted)?
            .to_vec();
        let mut separators: Vec<Slot> = node
            .separators
            .get(..node.num)
            .ok_or(Error::Corrupted)?
            .to_vec();
        let mut counts: Vec<i64> = node
            .counts
            .get(..node.num)
            .ok_or(Error::Corrupted)?
            .to_vec();
        *children.get_mut(i).ok_or(Error::Corrupted)? = Slot {
            value: codec::i64_from_u64(child.node_ptr)?,
            tag: Tag::Index,
            full: false,
        };
        *counts.get_mut(i).ok_or(Error::Corrupted)? = child.count;
        if let Some(split) = child.split {
            let at = i.checked_add(1).ok_or(Error::Corrupted)?;
            if at > children.len() {
                return Err(Error::Corrupted);
            }
            children.insert(
                at,
                Slot {
                    value: codec::i64_from_u64(split.node_ptr)?,
                    tag: Tag::Index,
                    full: false,
                },
            );
            separators.insert(at, split.separator);
            counts.insert(at, split.count);
        }
        let total = children.len();
        if total <= BTREE_SLOT_COUNT {
            let mut branch = SortedNode::sized(BTREE_KIND_BRANCH, total);
            codec::copy_slots(
                branch.children.get_mut(..total).ok_or(Error::Corrupted)?,
                children.get(..total).ok_or(Error::Corrupted)?,
            )?;
            codec::copy_slots(
                branch.separators.get_mut(..total).ok_or(Error::Corrupted)?,
                separators.get(..total).ok_or(Error::Corrupted)?,
            )?;
            codec::copy_counts(
                branch.counts.get_mut(..total).ok_or(Error::Corrupted)?,
                counts.get(..total).ok_or(Error::Corrupted)?,
            )?;
            let ptr = self.sorted_write_node(&branch, node_ptr, frozen)?;
            return Ok(SortedInsertResult {
                node_ptr: ptr,
                count: branch.subtree_count()?,
                entry: child.entry,
                added: child.added,
                split: None,
            });
        }
        self.sorted_split_branch_vecs(
            node_ptr,
            &BranchParts {
                children: &children,
                separators: &separators,
                counts: &counts,
            },
            child.entry,
            child.added,
            frozen,
        )
    }

    pub(crate) fn sorted_put(
        &self,
        node_ptr: u64,
        key: &[u8],
        key_only: bool,
        frozen: u64,
    ) -> Result<SortedInsertResult> {
        let node = self.read_sorted_node(node_ptr)?;
        match node.kind {
            BTREE_KIND_LEAF => self.sorted_put_leaf(node_ptr, &node, key, key_only, frozen),
            BTREE_KIND_BRANCH => self.sorted_put_branch(node_ptr, &node, key, key_only, frozen),
            _ => Err(Error::InvalidBTreeNodeKind),
        }
    }

    pub(crate) fn sorted_remove(
        &self,
        node_ptr: u64,
        key: &[u8],
        frozen: u64,
    ) -> Result<SortedRemoveResult> {
        let node = self.read_sorted_node(node_ptr)?;
        match node.kind {
            BTREE_KIND_LEAF => {
                let (idx, found) = self.sorted_leaf_search(&node, key)?;
                if !found {
                    return Ok(SortedRemoveResult {
                        node_ptr,
                        found: false,
                    });
                }
                let mut leaf = SortedNode {
                    kind: BTREE_KIND_LEAF,
                    num: node.num.checked_sub(1).ok_or(Error::Corrupted)?,
                    entries: [empty_slot(); BTREE_SLOT_COUNT],
                    children: [empty_slot(); BTREE_SLOT_COUNT],
                    separators: [empty_slot(); BTREE_SLOT_COUNT],
                    counts: [0; BTREE_SLOT_COUNT],
                };
                codec::copy_slots(
                    leaf.entries.get_mut(..idx).ok_or(Error::Corrupted)?,
                    node.entries.get(..idx).ok_or(Error::Corrupted)?,
                )?;
                codec::copy_slots(
                    leaf.entries
                        .get_mut(idx..leaf.num)
                        .ok_or(Error::Corrupted)?,
                    node.entries
                        .get(idx.checked_add(1).ok_or(Error::Corrupted)?..node.num)
                        .ok_or(Error::Corrupted)?,
                )?;
                let ptr = self.sorted_write_node(&leaf, node_ptr, frozen)?;
                Ok(SortedRemoveResult {
                    node_ptr: ptr,
                    found: true,
                })
            }
            BTREE_KIND_BRANCH => {
                let i = self.sorted_branch_search(&node, key)?;
                let child = self.sorted_remove(
                    codec::child_ptr(node.children.as_slice(), i)?,
                    key,
                    frozen,
                )?;
                if !child.found {
                    return Ok(SortedRemoveResult {
                        node_ptr,
                        found: false,
                    });
                }
                let mut branch = node;
                codec::set_slot_at(
                    &mut branch.children,
                    i,
                    Slot {
                        value: codec::i64_from_u64(child.node_ptr)?,
                        tag: Tag::Index,
                        full: false,
                    },
                )?;
                let prev = codec::count_at(&branch.counts, i)?;
                codec::set_count_at(
                    &mut branch.counts,
                    i,
                    prev.checked_sub(1).ok_or(Error::Corrupted)?,
                )?;
                let ptr = self.sorted_write_node(&branch, node_ptr, frozen)?;
                Ok(SortedRemoveResult {
                    node_ptr: ptr,
                    found: true,
                })
            }
            _ => Err(Error::InvalidBTreeNodeKind),
        }
    }

    pub(crate) fn sorted_grow_root(&self, result: SortedInsertResult) -> Result<u64> {
        if let Some(split) = result.split {
            let mut root = SortedNode {
                kind: BTREE_KIND_BRANCH,
                num: 2,
                entries: [empty_slot(); BTREE_SLOT_COUNT],
                children: [empty_slot(); BTREE_SLOT_COUNT],
                separators: [empty_slot(); BTREE_SLOT_COUNT],
                counts: [0; BTREE_SLOT_COUNT],
            };
            codec::set_slot_at(
                &mut root.children,
                0,
                Slot {
                    value: codec::i64_from_u64(result.node_ptr)?,
                    tag: Tag::Index,
                    full: false,
                },
            )?;
            codec::set_slot_at(
                &mut root.children,
                1,
                Slot {
                    value: codec::i64_from_u64(split.node_ptr)?,
                    tag: Tag::Index,
                    full: false,
                },
            )?;
            root.separators[1] = split.separator;
            codec::set_count_at(&mut root.counts, 0, result.count)?;
            codec::set_count_at(&mut root.counts, 1, split.count)?;
            return self.write_sorted_node(&root);
        }
        Ok(result.node_ptr)
    }

    /// Turns a located entry into the slot for the requested target. Only
    /// the value is writeable; key and entry pointers are immutable.
    pub(crate) fn sorted_target_slot(
        &self,
        entry: Slot,
        target: &crate::cursor::SortedMapTarget,
    ) -> Result<SlotPointer> {
        match target {
            SortedMapTarget::KeyValue(_) => Ok(SlotPointer {
                position: None,
                slot: entry,
            }),
            SortedMapTarget::Key(_) => {
                let (k, _, _) = self.read_kv_pair(entry)?;
                Ok(SlotPointer {
                    position: None,
                    slot: k,
                })
            }
            SortedMapTarget::Value(_) => {
                let (_, v, _) = self.read_kv_pair(entry)?;
                Ok(SlotPointer {
                    position: self.kv_value_position(entry),
                    slot: v,
                })
            }
        }
    }

    /// Advances the transaction start past shared subtrees. Concat joins
    /// shared nodes, so later in-place writes must copy them instead.
    pub(crate) fn set_tx_start(&self, value: Option<u64>) {
        *self.tx_start.borrow_mut() = value;
    }
}

fn btree_drop_child(branch: &mut BTreeNode, i: usize) -> Result<()> {
    let mut children: Vec<Slot> = branch
        .children
        .get(..branch.num)
        .ok_or(Error::Corrupted)?
        .to_vec();
    let mut counts: Vec<i64> = branch
        .counts
        .get(..branch.num)
        .ok_or(Error::Corrupted)?
        .to_vec();
    if i >= children.len() {
        return Err(Error::Corrupted);
    }
    children.remove(i);
    counts.remove(i);
    branch.num = branch.num.checked_sub(1).ok_or(Error::Corrupted)?;
    for (index, slot) in branch.children.iter_mut().enumerate() {
        *slot = children.get(index).copied().unwrap_or_else(empty_slot);
    }
    for (index, count) in branch.counts.iter_mut().enumerate() {
        *count = counts.get(index).copied().unwrap_or_default();
    }
    Ok(())
}

fn btree_concat_nodes(a: &BTreeNode, b: &BTreeNode) -> Result<BTreeNode> {
    let mut node = *a;
    let total = node.num.checked_add(b.num).ok_or(Error::Corrupted)?;
    if total > BTREE_SLOT_COUNT {
        return Err(Error::Corrupted);
    }
    if a.kind == BTREE_KIND_LEAF {
        let mut merged: Vec<Slot> = a.values.get(..a.num).ok_or(Error::Corrupted)?.to_vec();
        merged.extend(
            b.values
                .get(..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        for (index, slot) in node.values.iter_mut().enumerate() {
            *slot = merged.get(index).copied().unwrap_or_else(empty_slot);
        }
    } else {
        let mut merged_children: Vec<Slot> =
            a.children.get(..a.num).ok_or(Error::Corrupted)?.to_vec();
        merged_children.extend(
            b.children
                .get(..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        let mut merged_counts: Vec<i64> = a.counts.get(..a.num).ok_or(Error::Corrupted)?.to_vec();
        merged_counts.extend(
            b.counts
                .get(..b.num)
                .ok_or(Error::Corrupted)?
                .iter()
                .copied(),
        );
        for (index, slot) in node.children.iter_mut().enumerate() {
            *slot = merged_children
                .get(index)
                .copied()
                .unwrap_or_else(empty_slot);
        }
        for (index, count) in node.counts.iter_mut().enumerate() {
            *count = merged_counts.get(index).copied().unwrap_or_default();
        }
    }
    node.num = total;
    Ok(node)
}

impl<C: Core, H: Hasher> Database<C, H> {
    /// This database's replica state: what a replica sends its source so
    /// the source knows which bytes to send back.
    /// This database's replica state for [`Database::replication_start`].
    ///
    /// # Errors
    ///
    /// Propagates commit-read and I/O failures.
    pub fn replica_state(&self) -> Result<ReplicaState> {
        let committed = self.committed_size()?;
        let (_, _, checksum) = self.read_root_trailer(committed)?;
        Ok(ReplicaState {
            committed_size: committed,
            checksum,
        })
    }

    /// Where to start sending committed bytes to a replica in `state`:
    /// its committed size if its bytes prefix this database, else 0 for a
    /// full replacement.
    /// Byte offset at which to start sending committed bytes to a replica.
    ///
    /// # Errors
    ///
    /// Propagates commit-read and I/O failures (unrecognized replicas
    /// yield `Ok(0)`, a full copy, instead of an error).
    pub fn replication_start(&self, state: ReplicaState) -> Result<u64> {
        let committed = self.committed_size()?;
        if state.committed_size > committed {
            return Ok(0);
        }
        let checksum = match self.read_root_trailer(state.committed_size) {
            Ok((_, _, checksum)) => checksum,
            Err(Error::InvalidDatabase | Error::UnexpectedTag) => return Ok(0),
            Err(e) => return Err(e),
        };
        if checksum == state.checksum {
            return Ok(state.committed_size);
        }
        Ok(0)
    }

    /// Writes committed bytes `[start, end)` to `w`. With `start == 0` the
    /// bytes form a complete database (fresh root header included).
    /// Streams committed bytes `[start, end)` to `w` for replication.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRange`] for out-of-range spans; propagates
    /// commit-read and I/O failures.
    pub fn read_committed(&self, start: u64, end: u64, w: &mut dyn std::io::Write) -> Result<()> {
        let committed = self.committed_size()?;
        if start > end || end > committed {
            return Err(Error::InvalidRange);
        }
        if start > 0 && start < ROOT_HEADER_END {
            return Err(Error::InvalidRange);
        }
        if start == 0 {
            let (root, _, _) = self.read_root_trailer(end)?;
            self.stream_bytes(w, 0, ROOT_HEADER_START)?;
            let mut root_header = vec![0u8; codec::usize_from_u64(ROOT_HEADER_LEN)?];
            codec::sub_mut(&mut root_header, 0, SLOT_LEN)?.copy_from_slice(&root.to_bytes());
            codec::sub_mut(&mut root_header, SLOT_LEN, 8)?.copy_from_slice(&end.to_be_bytes());
            w.write_all(&root_header)?;
            return self.stream_bytes(w, ROOT_HEADER_END, codec::sub_u64(end, ROOT_HEADER_END)?);
        }
        self.stream_bytes(w, start, codec::sub_u64(end, start)?)
    }

    fn stream_bytes(&self, w: &mut dyn std::io::Write, start: u64, len: u64) -> Result<()> {
        let mut core = self.core.borrow_mut();
        core.seek_to(start)?;
        let mut remaining = len;
        let mut buf = vec![0u8; 4096];
        while remaining > 0 {
            let chunk = codec::usize_from_u64(remaining.min(4096))?;
            core.read(codec::sub_mut(&mut buf, 0, chunk)?)?;
            w.write_all(codec::sub(&buf, 0, chunk)?)?;
            remaining = codec::sub_u64(remaining, codec::u64_from_usize(chunk)?)?;
        }
        Ok(())
    }

    /// Writes `len` bytes from `r` at `start` and commits them. With
    /// `start == 0` this database is replaced; otherwise `start` must be
    /// its committed size. Only mutate a replica this way.
    /// Applies replicated bytes from [`Database::read_committed`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRange`] for a non-appending offset,
    /// [`Error::Truncated`] for short streams, [`Error::InvalidDatabase`]
    /// for a bad replacement image; propagates I/O failures.
    pub fn apply_replicated(&self, start: u64, r: &mut dyn std::io::Read, len: u64) -> Result<()> {
        if start == 0 {
            self.core.borrow_mut().truncate(0)?;
        } else {
            let committed = self.committed_size()?;
            if start != committed {
                return Err(Error::InvalidRange);
            }
            // Discard anything past the committed bytes, such as the
            // remains of an interrupted sync.
            self.truncate()?;
        }
        {
            let mut core = self.core.borrow_mut();
            core.seek_to(start)?;
            let mut buf = vec![0u8; 4096];
            let mut remaining = len;
            while remaining > 0 {
                let chunk = codec::usize_from_u64(remaining.min(4096))?;
                core_read_exact(r, codec::sub_mut(&mut buf, 0, chunk)?)?;
                core.write(codec::sub(&buf, 0, chunk)?)?;
                remaining = codec::sub_u64(remaining, codec::u64_from_usize(chunk)?)?;
            }
        }
        if start == 0 {
            self.core.borrow_mut().sync()?;
            let mut hb = [0u8; HEADER_LEN];
            {
                let mut core = self.core.borrow_mut();
                core.seek_to(0)?;
                core.read(&mut hb)?;
            }
            let header = Header::from_bytes(hb);
            header.validate(&self.hasher)?;
            self.header.set(header);
            self.committed_size()?;
        } else if len > 0 {
            let (root, _, _) = self.read_root_trailer(codec::add(start, len)?)?;
            self.core.borrow_mut().sync()?;
            self.write_root_header(root)?;
            self.core.borrow_mut().sync()?;
        }
        Ok(())
    }

    /// Rebuilds the live data (reachable from the latest root) into a
    /// new or empty `target`, clearing history. The target must be a new
    /// or empty database, else `TargetNotEmpty` (including compacting a
    /// database into its own file).
    /// Rebuilds live data into a new or empty target, clearing history.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TargetNotEmpty`] for a non-empty target; propagates
    /// remap, commit, offset-map, and I/O failures.
    pub fn compact<D: Core>(
        &self,
        target_core: D,
        hasher: H,
        opts: InitOpts,
        offsets: &mut impl crate::offset_map::OffsetMap,
    ) -> Result<Database<D, H>> {
        offsets.reset()?;
        let target = Database::open(target_core, hasher, opts)?;
        if !target.read_root_slot()?.empty() {
            return Err(Error::TargetNotEmpty);
        }
        let source_root = self.read_root_slot()?;
        // An empty root compacts to a new database (an explicit null is
        // not empty, so it remaps below).
        if source_root.empty() {
            return Ok(target);
        }
        // The target is written as ordinary transactions; the remap must
        // run inside one because starting it truncates the target.
        let hash_size = self.header().hash_size;
        let mut compactor = Compactor {
            source: self,
            target_core: target.core(),
            hash_size,
            offsets,
        };
        if source_root.tag == Tag::ArrayList {
            // A history root's latest moment becomes the only moment.
            let last = self
                .root_cursor()
                .read_path_slot(&[crate::cursor::PathPart::ArrayListGet { index: -1 }])?;
            if !last.empty() {
                target.root_cursor().write_path(&[
                    crate::cursor::PathPart::ArrayListInit,
                    crate::cursor::PathPart::ArrayListAppend,
                    crate::cursor::PathPart::Context(crate::cursor::ContextPart::new(move |c| {
                        let remapped = compactor.remap_slot(last)?;
                        c.write(crate::cursor::Writeable::Slot(remapped))?;
                        Ok(())
                    })),
                ])?;
            }
        } else {
            target.run_context(|c| {
                let remapped = compactor.remap_slot(source_root)?;
                c.write(crate::cursor::Writeable::Slot(remapped))?;
                Ok(())
            })?;
        }
        Ok(target)
    }
}

fn core_read_exact(r: &mut dyn std::io::Read, buf: &mut [u8]) -> Result<()> {
    let total = buf.len();
    let mut at = 0;
    while at < total {
        match r.read(codec::sub_mut(
            buf,
            at,
            total.checked_sub(at).ok_or(Error::Corrupted)?,
        )?) {
            Ok(0) => return Err(Error::Truncated),
            Ok(n) => {
                at = at.checked_add(n).ok_or(Error::Corrupted)?;
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(())
}

/// Copies live objects from source to target, recording each offset so
/// shared references and cycles point at the same copy.
pub(crate) struct Compactor<'a, C: Core, D: Core, H: Hasher> {
    source: &'a Database<C, H>,
    target_core: &'a std::cell::RefCell<D>,
    hash_size: u16,
    offsets: &'a mut (dyn crate::offset_map::OffsetMap + 'a),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjKind {
    Index,
    ArrayList,
    HashMap,
    Counted,
    KVPair,
    SortedKVPair,
}

impl<C: Core, D: Core, H: Hasher> Compactor<'_, C, D, H> {
    fn reserve(&self, size: u64) -> Result<u64> {
        let mut target = self.target_core.borrow_mut();
        let at = target.committed_len()?;
        target.seek_to(at)?;
        target.write(&vec![0u8; codec::usize_from_u64(size)?])?;
        Ok(at)
    }

    fn visit(&mut self, src: u64, size: u64, kind: ObjKind) -> Result<u64> {
        if let Some(dst) = self.offsets.get(src)? {
            return Ok(dst);
        }
        let dst = self.reserve(size)?;
        self.offsets.put(src, dst)?;
        self.populate(src, dst, kind)?;
        Ok(dst)
    }

    fn remap_slot(&mut self, slot: Slot) -> Result<Slot> {
        let dst = match slot.tag {
            Tag::None | Tag::Uint | Tag::Int | Tag::Float | Tag::ShortBytes => {
                return Ok(slot);
            }
            Tag::Bytes => self.remap_bytes(&slot)?,
            Tag::Index => self.visit(
                codec::u64_from_i64(slot.value)?,
                INDEX_BLOCK_LEN,
                ObjKind::Index,
            )?,
            Tag::ArrayList => self.visit(
                codec::u64_from_i64(slot.value)?,
                ARRAY_LIST_HEADER_LEN,
                ObjKind::ArrayList,
            )?,
            Tag::LinkedArrayList => self.remap_btree_root(codec::u64_from_i64(slot.value)?)?,
            Tag::HashMap | Tag::HashSet => self.visit(
                codec::u64_from_i64(slot.value)?,
                INDEX_BLOCK_LEN,
                ObjKind::HashMap,
            )?,
            Tag::CountedHashMap | Tag::CountedHashSet => self.visit(
                codec::u64_from_i64(slot.value)?,
                INDEX_BLOCK_LEN + 8,
                ObjKind::Counted,
            )?,
            Tag::KVPair => self.visit(
                codec::u64_from_i64(slot.value)?,
                codec::add(u64::from(self.hash_size), 18)?,
                ObjKind::KVPair,
            )?,
            Tag::SortedKVPair => {
                self.visit(codec::u64_from_i64(slot.value)?, 18, ObjKind::SortedKVPair)?
            }
            Tag::SortedMap | Tag::SortedSet => {
                self.remap_sorted_root(codec::u64_from_i64(slot.value)?)?
            }
            Tag::Unknown => return Err(Error::UnexpectedTag),
        };
        Ok(Slot {
            value: crate::codec::i64_from_u64(dst)?,
            tag: slot.tag,
            full: slot.full,
        })
    }

    fn remap_bytes(&mut self, slot: &Slot) -> Result<u64> {
        if let Some(dst) = self.offsets.get(codec::u64_from_i64(slot.value)?)? {
            return Ok(dst);
        }
        let mut source = self.source.core.borrow_mut();
        source.seek_to(codec::u64_from_i64(slot.value)?)?;
        let mut size_bytes = [0u8; 8];
        source.read(&mut size_bytes)?;
        let size = u64::from_be_bytes(size_bytes);
        let format = u64::from(slot.full);
        let total = codec::add(size, codec::mul(format, 2)?)?;
        let mut target = self.target_core.borrow_mut();
        let dst = target.committed_len()?;
        target.seek_to(dst)?;
        target.write(&size.to_be_bytes())?;
        let mut remaining = total;
        let mut buf = vec![0u8; 4096];
        while remaining > 0 {
            let chunk = codec::usize_from_u64(remaining.min(4096))?;
            source.read(codec::sub_mut(&mut buf, 0, chunk)?)?;
            target.write(codec::sub(&buf, 0, chunk)?)?;
            remaining = codec::sub_u64(remaining, codec::u64_from_usize(chunk)?)?;
        }
        drop(source);
        drop(target);
        self.offsets.put(codec::u64_from_i64(slot.value)?, dst)?;
        Ok(dst)
    }

    fn populate(&mut self, src: u64, dst: u64, kind: ObjKind) -> Result<()> {
        match kind {
            ObjKind::Index | ObjKind::HashMap | ObjKind::Counted => {
                self.populate_slots(src, dst, INDEX_BLOCK_LEN, kind)
            }
            ObjKind::ArrayList => self.populate_arraylist(src, dst),
            ObjKind::KVPair => self.populate_kv(src, dst, Tag::KVPair),
            ObjKind::SortedKVPair => self.populate_kv(src, dst, Tag::SortedKVPair),
        }
    }

    fn remap_btree_root(&mut self, at: u64) -> Result<u64> {
        let bytes = self.read_source(at, BTREE_HEADER_LEN)?;
        let (size, root) = codec::header_pair(&bytes)?;
        let mapped = self.remap_btree_node(root)?;
        let dst = self.reserve(BTREE_HEADER_LEN)?;
        self.write_target(dst, &codec::header_bytes(size, mapped))?;
        Ok(dst)
    }

    fn remap_sorted_root(&mut self, at: u64) -> Result<u64> {
        let bytes = self.read_source(at, BTREE_HEADER_LEN)?;
        let (size, root) = codec::header_pair(&bytes)?;
        let mapped = self.remap_sorted_node(root)?;
        let dst = self.reserve(BTREE_HEADER_LEN)?;
        self.write_target(dst, &codec::header_bytes(size, mapped))?;
        Ok(dst)
    }

    fn populate_btree_node(&mut self, src: u64, dst: u64, kind: u8, num: u8) -> Result<()> {
        let body_at = codec::add(src, codec::NODE_HEADER_U64)?;
        if kind == BTREE_KIND_LEAF {
            let bytes = self.read_source(body_at, INDEX_BLOCK_LEN)?;
            let mut out = vec![kind, num];
            for chunk in bytes.chunks_exact(SLOT_LEN) {
                let mut b = [0u8; SLOT_LEN];
                b.copy_from_slice(chunk);
                out.extend_from_slice(&self.remap_slot(Slot::from_bytes(b))?.to_bytes());
            }
            return self.write_target(dst, &out);
        }
        let bytes = self.read_source(body_at, INDEX_BLOCK_LEN)?;
        let mut out = vec![kind, num];
        for chunk in bytes.chunks_exact(SLOT_LEN) {
            let mut b = [0u8; SLOT_LEN];
            b.copy_from_slice(chunk);
            let child = Slot::from_bytes(b);
            child.validate()?;
            let remapped = if child.tag == Tag::Index {
                let at = self.remap_btree_node(codec::u64_from_i64(child.value)?)?;
                Slot {
                    value: codec::i64_from_u64(at)?,
                    tag: Tag::Index,
                    full: child.full,
                }
            } else {
                child
            };
            out.extend_from_slice(&remapped.to_bytes());
        }
        // Counts follow the children verbatim (16 big-endian `i64`).
        let counts_at = codec::add(body_at, INDEX_BLOCK_LEN)?;
        let counts = self.read_source(counts_at, 128)?;
        out.extend_from_slice(&counts);
        self.write_target(dst, &out)
    }

    fn populate_sorted_node(&mut self, src: u64, dst: u64, kind: u8, num: u8) -> Result<()> {
        let body_at = codec::add(src, codec::NODE_HEADER_U64)?;
        if kind == BTREE_KIND_LEAF {
            let bytes = self.read_source(body_at, INDEX_BLOCK_LEN)?;
            let mut out = vec![kind, num];
            for chunk in bytes.chunks_exact(SLOT_LEN) {
                let mut b = [0u8; SLOT_LEN];
                b.copy_from_slice(chunk);
                out.extend_from_slice(&self.remap_slot(Slot::from_bytes(b))?.to_bytes());
            }
            return self.write_target(dst, &out);
        }
        let bytes = self.read_source(body_at, INDEX_BLOCK_LEN)?;
        let mut out = vec![kind, num];
        for chunk in bytes.chunks_exact(SLOT_LEN) {
            let mut b = [0u8; SLOT_LEN];
            b.copy_from_slice(chunk);
            let child = Slot::from_bytes(b);
            child.validate()?;
            let remapped = if child.tag == Tag::Index {
                let at = self.remap_sorted_node(codec::u64_from_i64(child.value)?)?;
                Slot {
                    value: codec::i64_from_u64(at)?,
                    tag: Tag::Index,
                    full: child.full,
                }
            } else {
                child
            };
            out.extend_from_slice(&remapped.to_bytes());
        }
        let sep_at = codec::add(body_at, INDEX_BLOCK_LEN)?;
        let seps = self.read_source(sep_at, INDEX_BLOCK_LEN)?;
        for chunk in seps.chunks_exact(SLOT_LEN) {
            let mut b = [0u8; SLOT_LEN];
            b.copy_from_slice(chunk);
            out.extend_from_slice(&self.remap_slot(Slot::from_bytes(b))?.to_bytes());
        }
        let counts_at = codec::add(sep_at, INDEX_BLOCK_LEN)?;
        out.extend_from_slice(&self.read_source(counts_at, 128)?);
        self.write_target(dst, &out)
    }

    fn read_source(&self, pos: u64, len: u64) -> Result<Vec<u8>> {
        let mut source = self.source.core.borrow_mut();
        source.seek_to(pos)?;
        let mut buf = vec![0u8; codec::usize_from_u64(len)?];
        source.read(&mut buf)?;
        Ok(buf)
    }

    fn write_target(&self, pos: u64, bytes: &[u8]) -> Result<()> {
        let mut target = self.target_core.borrow_mut();
        target.seek_to(pos)?;
        target.write(bytes)?;
        Ok(())
    }

    fn populate_slots(&mut self, src: u64, dst: u64, len: u64, kind: ObjKind) -> Result<()> {
        let bytes = self.read_source(src, len)?;
        let total = codec::usize_from_u64(len)?;
        let body: &[u8] = if kind == ObjKind::Counted {
            // The leading count travels verbatim (8 bytes).
            codec::sub(&bytes, 8, total.checked_sub(8).ok_or(Error::Corrupted)?)?
        } else {
            &bytes
        };
        let mut out = Vec::with_capacity(total);
        if kind == ObjKind::Counted {
            out.extend_from_slice(codec::sub(&bytes, 0, 8)?);
        }
        for chunk in body.chunks_exact(SLOT_LEN) {
            let mut b = [0u8; SLOT_LEN];
            b.copy_from_slice(chunk);
            out.extend_from_slice(&self.remap_slot(Slot::from_bytes(b))?.to_bytes());
        }
        self.write_target(dst, &out)
    }

    fn populate_arraylist(&mut self, src: u64, dst: u64) -> Result<()> {
        let bytes = self.read_source(src, ARRAY_LIST_HEADER_LEN)?;
        let (size, at) = codec::header_pair(&bytes)?;
        let shift = if size > codec::BSLOT_COUNT_U64 {
            log16_floor(codec::sub_u64(size, 1)?)?
        } else {
            0
        };
        let index = self.remap_arraylist_index(at, size, shift)?;
        self.write_target(dst, &codec::header_bytes(size, index))
    }

    fn remap_arraylist_index(&mut self, src: u64, size: u64, shift: u8) -> Result<u64> {
        // 16^shift children per block; saturates for corrupt shifts.
        let child_size = u32::from(shift)
            .checked_mul(4)
            .and_then(|bits| 1u64.checked_shl(bits))
            .unwrap_or(u64::MAX);
        // Full blocks use the normal cache; partial blocks may be shared by
        // lists of different sizes, so copy them separately and leave the
        // slots beyond the size empty.
        if child_size.checked_mul(SLOT_COUNT).ok_or(Error::Corrupted)? == size {
            let hit = self.remap_slot(Slot {
                value: crate::codec::i64_from_u64(src)?,
                tag: Tag::Index,
                full: false,
            })?;
            return codec::u64_from_i64(hit.value);
        }
        let dst = self.reserve(INDEX_BLOCK_LEN)?;
        let bytes = self.read_source(src, INDEX_BLOCK_LEN)?;
        let mut out = vec![0u8; codec::usize_from_u64(INDEX_BLOCK_LEN)?];
        let mut remaining = size;
        let mut i: usize = 0;
        while i < BTREE_SLOT_COUNT && remaining > 0 {
            let at = codec::mul(codec::u64_from_usize(i)?, codec::SLOT_LEN_U64)?;
            let mut b = [0u8; SLOT_LEN];
            b.copy_from_slice(codec::sub(&bytes, codec::usize_from_u64(at)?, SLOT_LEN)?);
            let child = Slot::from_bytes(b);
            let count = remaining.min(child_size);
            let remapped = if shift == 0 {
                self.remap_slot(child)?
            } else {
                if child.tag != Tag::Index {
                    return Err(Error::UnexpectedTag);
                }
                let at = self.remap_arraylist_index(
                    codec::u64_from_i64(child.value)?,
                    count,
                    shift.checked_sub(1).ok_or(Error::Corrupted)?,
                )?;
                Slot {
                    value: codec::i64_from_u64(at)?,
                    ..child
                }
            };
            codec::sub_mut(&mut out, codec::usize_from_u64(at)?, SLOT_LEN)?
                .copy_from_slice(&remapped.to_bytes());
            remaining = codec::sub_u64(remaining, count)?;
            i = i.checked_add(1).ok_or(Error::Corrupted)?;
        }
        self.write_target(dst, &out)?;
        Ok(dst)
    }

    fn remap_btree_node(&mut self, at: u64) -> Result<u64> {
        if let Some(dst) = self.offsets.get(at)? {
            return Ok(dst);
        }
        let head = self.read_source(at, codec::NODE_HEADER_U64)?;
        let kind = head.first().copied().ok_or(Error::Corrupted)?;
        let num = head.get(1).copied().ok_or(Error::Corrupted)?;
        if kind > BTREE_KIND_BRANCH {
            return Err(Error::InvalidBTreeNodeKind);
        }
        if usize::from(num) > BTREE_SLOT_COUNT {
            return Err(Error::InvalidBTreeNode);
        }
        let size = if kind == BTREE_KIND_LEAF {
            codec::u64_from_usize(BTREE_LEAF_BLOCK)?
        } else {
            codec::u64_from_usize(BTREE_BRANCH_BLOCK)?
        };
        let dst = self.reserve(size)?;
        self.offsets.put(at, dst)?;
        self.populate_btree_node(at, dst, kind, num)?;
        Ok(dst)
    }

    fn remap_sorted_node(&mut self, at: u64) -> Result<u64> {
        if let Some(dst) = self.offsets.get(at)? {
            return Ok(dst);
        }
        let head = self.read_source(at, codec::NODE_HEADER_U64)?;
        let kind = head.first().copied().ok_or(Error::Corrupted)?;
        let num = head.get(1).copied().ok_or(Error::Corrupted)?;
        if kind > BTREE_KIND_BRANCH {
            return Err(Error::InvalidBTreeNodeKind);
        }
        if usize::from(num) > BTREE_SLOT_COUNT {
            return Err(Error::InvalidBTreeNode);
        }
        let size = if kind == BTREE_KIND_LEAF {
            codec::u64_from_usize(SORTED_LEAF_BLOCK)?
        } else {
            codec::u64_from_usize(SORTED_BRANCH_BLOCK)?
        };
        let dst = self.reserve(size)?;
        self.offsets.put(at, dst)?;
        self.populate_sorted_node(at, dst, kind, num)?;
        Ok(dst)
    }

    fn populate_kv(&mut self, src: u64, dst: u64, tag: Tag) -> Result<()> {
        // Decode with the source layout, remap both slots, re-encode.
        let hash_len = if tag == Tag::KVPair {
            usize::from(self.hash_size)
        } else {
            0
        };
        let total = codec::add(codec::u64_from_usize(hash_len)?, 18)?;
        let bytes = self.read_source(src, total)?;
        let (hash, rest) = bytes.split_at_checked(hash_len).ok_or(Error::Corrupted)?;
        let mut key_bytes = [0u8; SLOT_LEN];
        key_bytes.copy_from_slice(codec::sub(rest, 0, SLOT_LEN)?);
        let mut value_bytes = [0u8; SLOT_LEN];
        value_bytes.copy_from_slice(codec::sub(rest, SLOT_LEN, SLOT_LEN)?);
        let key = self.remap_slot(Slot::from_bytes(key_bytes))?;
        let value = self.remap_slot(Slot::from_bytes(value_bytes))?;
        let mut out = Vec::with_capacity(codec::usize_from_u64(total)?);
        if tag == Tag::KVPair {
            out.extend_from_slice(hash);
        }
        out.extend_from_slice(&key.to_bytes());
        out.extend_from_slice(&value.to_bytes());
        self.write_target(dst, &out)
    }
}

const ARRAY_LIST_HEADER_LEN: u64 = 16;
const BTREE_HEADER_LEN: u64 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_sizes_agree() {
        // Pins the duplicated `u64` size literals to the `usize` ones.
        use crate::codec;
        let slot = u64::try_from(SLOT_LEN).unwrap();
        assert_eq!(ROOT_HEADER_START, u64::try_from(HEADER_LEN).unwrap());
        assert_eq!(ROOT_HEADER_LEN, codec::add(slot, 8).unwrap());
        assert_eq!(ROOT_TRAILER_LEN, codec::add(slot, 16).unwrap());
        assert_eq!(INDEX_BLOCK_LEN, codec::mul(slot, SLOT_COUNT).unwrap());
        assert_eq!(
            ROOT_FILE_SIZE_OFF,
            codec::add(ROOT_HEADER_START, slot).unwrap()
        );
        assert_eq!(
            MIN_COMMITTED,
            codec::add(ROOT_HEADER_END, ROOT_TRAILER_LEN).unwrap()
        );
        assert_eq!(crate::codec::SLOT_LEN_U64, slot);
        assert_eq!(
            crate::codec::INDEX_BLOCK_LEN_U64,
            codec::mul(slot, SLOT_COUNT).unwrap()
        );
        assert_eq!(crate::codec::NODE_HEADER_U64, 2);
        assert_eq!(crate::codec::BSLOT_COUNT_U64, SLOT_COUNT);
    }

    #[test]
    fn crc_table_matches_go_ecma() {
        assert_eq!(crc64_table()[1], 0xb32e_4cbe_03a7_5f6f);
    }

    #[test]
    fn crc_is_xz_check_value() {
        // The standard CRC-64/XZ check value (mirrors Go's checksum test).
        assert_eq!(crc64_xz(b"123456789").unwrap(), 0x995d_c9bb_df19_39fa);
    }

    #[test]
    fn trailer_checksum_matches_go_vectors() {
        let mut root = [0u8; SLOT_LEN];
        assert_eq!(
            trailer_checksum(1, 37, &root, 0).unwrap(),
            0xd95e_2cd5_40a1_ea38
        );
        root[0] = 8 | 0x80;
        root[1..].copy_from_slice(&u64::MAX.wrapping_sub(4).to_be_bytes()); // -5 as u64
        assert_eq!(
            trailer_checksum(0x0102_0304_0506_0708, 62, &root, 0x1122_3344_5566_7788).unwrap(),
            0x9944_bcf9_f971_1e77
        );
    }

    #[test]
    fn default_file_ids_differ() {
        let a = Database::open(
            crate::core::MemoryCore::new(),
            crate::hasher::Sha1Hasher,
            crate::database::InitOpts {
                hash_id: None,
                file_id: None,
                fsync: false,
            },
        )
        .unwrap();
        let b = Database::open(
            crate::core::MemoryCore::new(),
            crate::hasher::Sha1Hasher,
            crate::database::InitOpts {
                hash_id: None,
                file_id: None,
                fsync: false,
            },
        )
        .unwrap();
        assert_ne!(a.header().file_id, b.header().file_id);
    }

    #[test]
    fn commit_ids_differ_across_commits() {
        let db = Database::open(
            crate::core::MemoryCore::new(),
            crate::hasher::Sha1Hasher,
            crate::database::InitOpts {
                hash_id: None,
                file_id: Some(1),
                fsync: false,
            },
        )
        .unwrap();
        db.run_context(|_| Ok(())).unwrap();
        let (_, first, _) = db.read_root_trailer(db.committed_size().unwrap()).unwrap();
        db.run_context(|_| Ok(())).unwrap();
        let (_, second, _) = db.read_root_trailer(db.committed_size().unwrap()).unwrap();
        assert_ne!(first, second);
    }
}
