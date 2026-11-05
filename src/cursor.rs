use std::cell::{Cell, RefCell};
use std::marker::PhantomData;

use crate::codec;
use crate::core::Core;
use crate::database::{Database, ROOT_HEADER_START, empty_slot};
use crate::error::{Error, Result};
use crate::hasher::Hasher;
use crate::slot::{SLOT_LEN, Slot};
use crate::tag::Tag;

#[derive(Debug, Clone, Copy)]
pub struct ReadOnly;
#[derive(Debug, Clone, Copy)]
pub struct ReadWrite;

/// A slot plus its file position. `position == None` means transient
/// (not yet flushed to the file).
#[derive(Debug, Clone, Copy)]
pub struct SlotPointer {
    pub position: Option<u64>,
    pub slot: Slot,
}

/// Cursor borrowing its database. Typestate `Mode` restricts writes at
/// compile time; runtime tx/freeze rules mirror the reference impls.
pub struct Cursor<'db, C: Core, H: Hasher, Mode> {
    pub(crate) db: &'db Database<C, H>,
    pub(crate) slot_ptr: Cell<SlotPointer>,
    pub(crate) frozen_at: Cell<Option<u64>>,
    pub(crate) epoch: Cell<Option<u64>>,
    pub(crate) _mode: PhantomData<Mode>,
}

impl<C: Core, H: Hasher, Mode> Clone for Cursor<'_, C, H, Mode> {
    fn clone(&self) -> Self {
        Cursor {
            db: self.db,
            slot_ptr: Cell::new(self.slot_ptr.get()),
            frozen_at: Cell::new(self.frozen_at.get()),
            epoch: Cell::new(self.epoch.get()),
            _mode: PhantomData,
        }
    }
}

pub type ReadCursor<'db, C, H> = Cursor<'db, C, H, ReadOnly>;
pub type WriteCursor<'db, C, H> = Cursor<'db, C, H, ReadWrite>;

/// Data that can be written through a cursor. Mirrors Go's `WriteableData`.
#[derive(Debug, Clone)]
pub enum Writeable {
    Slot(Slot),
    Bytes(Vec<u8>),
    BytesObject {
        value: Vec<u8>,
        format_tag: Option<[u8; 2]>,
    },
    Uint(u64),
    Int(i64),
    Float(f64),
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytesObject {
    pub value: Vec<u8>,
    pub format_tag: Option<[u8; 2]>,
}

impl BytesObject {
    fn is_short(&self) -> bool {
        let total = if self.format_tag.is_some() { 6 } else { 8 };
        if self.value.len() > total {
            return false;
        }
        !self.value.contains(&0)
    }
}

pub struct KeyValuePair<'db, C: Core, H: Hasher> {
    pub key: ReadCursor<'db, C, H>,
    pub value: ReadCursor<'db, C, H>,
    pub hash: Vec<u8>,
}

pub struct WriteKeyValuePair<'db, C: Core, H: Hasher> {
    pub key: WriteCursor<'db, C, H>,
    pub value: WriteCursor<'db, C, H>,
    pub hash: Vec<u8>,
}

/// Path steps for `write_path`/`read_path`.
///
/// Reads traverse `Get` steps; writes additionally run
/// `Write`/`Context`/collection steps.
/// `Context` carries a single-use closure (moved on `Clone`, taken on use).
pub enum PathPart<'a, C: Core, H: Hasher> {
    Write(Writeable),
    Context(ContextPart<'a, C, H>),
    ArrayListInit,
    ArrayListGet { index: i64 },
    ArrayListAppend,
    HashMapInit { counted: bool, set: bool },
    HashMapGet { target: GetTarget },
    HashMapRemove { hash: Vec<u8> },
    ArrayListSlice { size: i64 },
    SortedMapInit { set: bool },
    SortedMapGet { target: SortedMapTarget },
    SortedMapGetIndex { index: i64 },
    SortedMapRemove { key: Vec<u8> },
    LinkedArrayListInit,
    LinkedArrayListGet { index: i64 },
    LinkedArrayListAppend,
    LinkedArrayListSlice { offset: i64, size: i64 },
    LinkedArrayListConcat { list: Slot },
    LinkedArrayListInsert { index: i64 },
    LinkedArrayListRemove { index: i64 },
}

/// Single-use transaction callback, shared by `Context` path parts and
/// `append_context`.
pub type ContextFn<'a, C, H> = Box<dyn for<'c> FnOnce(&WriteCursor<'c, C, H>) -> Result<()> + 'a>;

pub struct ContextPart<'a, C: Core, H: Hasher> {
    func: RefCell<Option<ContextFn<'a, C, H>>>,
}

impl<'a, C: Core, H: Hasher> ContextPart<'a, C, H> {
    pub fn new(f: impl for<'c> FnOnce(&WriteCursor<'c, C, H>) -> Result<()> + 'a) -> Self {
        ContextPart {
            func: RefCell::new(Some(Box::new(f))),
        }
    }
}

impl<C: Core, H: Hasher> std::fmt::Debug for ContextPart<'_, C, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Context(..)")
    }
}

impl<C: Core, H: Hasher> std::fmt::Debug for PathPart<'_, C, H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathPart::Write(w) => f.debug_tuple("Write").field(w).finish(),
            PathPart::Context(c) => f.debug_tuple("Context").field(c).finish(),
            PathPart::ArrayListInit => f.write_str("ArrayListInit"),
            PathPart::ArrayListGet { index } => f
                .debug_struct("ArrayListGet")
                .field("index", index)
                .finish(),
            PathPart::ArrayListAppend => f.write_str("ArrayListAppend"),
            PathPart::HashMapInit { counted, set } => f
                .debug_struct("HashMapInit")
                .field("counted", counted)
                .field("set", set)
                .finish(),
            PathPart::HashMapGet { target } => f
                .debug_struct("HashMapGet")
                .field("target", target)
                .finish(),
            PathPart::HashMapRemove { .. } => f.write_str("HashMapRemove(..)"),
            PathPart::ArrayListSlice { size } => f
                .debug_struct("ArrayListSlice")
                .field("size", size)
                .finish(),
            PathPart::SortedMapInit { set } => {
                f.debug_struct("SortedMapInit").field("set", set).finish()
            }
            PathPart::SortedMapGet { target } => f
                .debug_struct("SortedMapGet")
                .field("target", target)
                .finish(),
            PathPart::SortedMapGetIndex { index } => f
                .debug_struct("SortedMapGetIndex")
                .field("index", index)
                .finish(),
            PathPart::SortedMapRemove { .. } => f.write_str("SortedMapRemove(..)"),
            PathPart::LinkedArrayListInit => f.write_str("LinkedArrayListInit"),
            PathPart::LinkedArrayListGet { index } => f
                .debug_struct("LinkedArrayListGet")
                .field("index", index)
                .finish(),
            PathPart::LinkedArrayListAppend => f.write_str("LinkedArrayListAppend"),
            PathPart::LinkedArrayListSlice { offset, size } => f
                .debug_struct("LinkedArrayListSlice")
                .field("offset", offset)
                .field("size", size)
                .finish(),
            PathPart::LinkedArrayListConcat { .. } => f.write_str("LinkedArrayListConcat(..)"),
            PathPart::LinkedArrayListInsert { index } => f
                .debug_struct("LinkedArrayListInsert")
                .field("index", index)
                .finish(),
            PathPart::LinkedArrayListRemove { index } => f
                .debug_struct("LinkedArrayListRemove")
                .field("index", index)
                .finish(),
        }
    }
}

/// Which slot a map lookup resolves to.
#[derive(Debug, Clone)]
pub enum GetTarget {
    KeyValue(Vec<u8>),
    Key(Vec<u8>),
    Value(Vec<u8>),
}

impl GetTarget {
    fn hash(&self) -> &[u8] {
        match self {
            Self::KeyValue(h) | Self::Key(h) | Self::Value(h) => h,
        }
    }
}

/// Which slot a sorted-map lookup resolves to.
#[derive(Debug, Clone)]
pub enum SortedMapTarget {
    KeyValue(Vec<u8>),
    Key(Vec<u8>),
    Value(Vec<u8>),
}

impl SortedMapTarget {
    fn key(&self) -> &[u8] {
        match self {
            Self::KeyValue(k) | Self::Key(k) | Self::Value(k) => k,
        }
    }
}

fn is_root(ptr: &SlotPointer) -> bool {
    ptr.position == Some(ROOT_HEADER_START)
}

/// Decodes inline short-bytes content: up to 8 value bytes (6 + 2-byte
/// format tag when `full`), cut at the first zero.
fn decode_short(slot: &Slot) -> (Vec<u8>, Option<[u8; 2]>) {
    let bytes = crate::codec::bits_u64(slot.value).to_be_bytes();
    let total = if slot.full { 6 } else { 8 };
    let mut len = total;
    for (i, b) in bytes.iter().enumerate().take(total) {
        if *b == 0 {
            len = i;
            break;
        }
    }
    let mut tag = None;
    if slot.full {
        // Six payload bytes plus the two tag bytes always exist here.
        if let (Some(&hi), Some(&lo)) = (bytes.get(6), bytes.get(7)) {
            tag = Some([hi, lo]);
        }
    }
    (bytes.get(..len).map_or_else(Vec::new, <[u8]>::to_vec), tag)
}

impl<'db, C: Core, H: Hasher, Mode> Cursor<'db, C, H, Mode> {
    fn resolved(&self) -> Result<SlotPointer> {
        let ptr = self.slot_ptr.get();
        if is_root(&ptr) {
            // Re-read the root: the snapshot goes stale after the next
            // transaction. The slot in the newest trailer is a fixed
            // snapshot, so one commit is read even if more are made.
            let (file_size, root) = self.db.read_root_header()?;
            return Ok(SlotPointer {
                position: Some(codec::sub_u64(
                    file_size,
                    crate::database::ROOT_TRAILER_LEN,
                )?),
                slot: root,
            });
        }
        Ok(ptr)
    }

    /// Returns the resolved slot this cursor points at.
    ///
    /// # Errors
    ///
    /// Propagates resolution and I/O failures.
    pub fn slot(&self) -> Result<Slot> {
        Ok(self.resolved()?.slot)
    }

    /// Reads the unsigned integer stored at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not a `Uint`, holds a negative value, or I/O fails.
    pub fn read_uint(&self) -> Result<u64> {
        let ptr = self.resolved()?;
        if ptr.slot.tag != Tag::Uint {
            return Err(Error::UnexpectedTag);
        }
        if ptr.slot.value < 0 {
            return Err(Error::ExpectedUnsignedLong);
        }
        codec::u64_from_i64(ptr.slot.value)
    }

    /// Reads the signed integer stored at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not an `Int` or I/O fails.
    pub fn read_int(&self) -> Result<i64> {
        let ptr = self.resolved()?;
        if ptr.slot.tag != Tag::Int {
            return Err(Error::UnexpectedTag);
        }
        Ok(ptr.slot.value)
    }

    /// Reads the floating-point value stored at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not a `Float` or I/O fails.
    pub fn read_float(&self) -> Result<f64> {
        let ptr = self.resolved()?;
        if ptr.slot.tag != Tag::Float {
            return Err(Error::UnexpectedTag);
        }
        Ok(f64::from_bits(codec::u64_from_i64(ptr.slot.value)?))
    }

    fn read_bytes_capped(&self, cap: Option<usize>) -> Result<BytesObject> {
        let ptr = self.resolved()?;
        match ptr.slot.tag {
            Tag::None => Ok(BytesObject {
                value: Vec::new(),
                format_tag: None,
            }),
            Tag::Bytes => {
                let mut core = self.db.core().borrow_mut();
                core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                let mut size_bytes = [0u8; 8];
                core.read(&mut size_bytes)?;
                let size = codec::usize_from_u64(u64::from_be_bytes(size_bytes))?;
                if let Some(cap) = cap {
                    if cap > 0 && size > cap {
                        return Err(Error::StreamTooLong);
                    }
                }
                let mut value = vec![0u8; size];
                core.read(&mut value)?;
                let format_tag = if ptr.slot.full {
                    let mut tag = [0u8; 2];
                    core.read(&mut tag)?;
                    Some(tag)
                } else {
                    None
                };
                Ok(BytesObject { value, format_tag })
            }
            Tag::ShortBytes => {
                let (value, format_tag) = decode_short(&ptr.slot);
                if let Some(cap) = cap {
                    if cap > 0 && value.len() > cap {
                        return Err(Error::StreamTooLong);
                    }
                }
                Ok(BytesObject { value, format_tag })
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// Reads the byte value, bounded by `buf`'s length like Go's `maxSize`
    /// (empty `buf` means unbounded). Copies the value into `buf`.
    /// Reads the byte string at this cursor into `buf`.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not bytes, the value overflows `buf`, or I/O fails.
    pub fn read_bytes(&self, buf: &mut [u8]) -> Result<Vec<u8>> {
        let cap = if buf.is_empty() {
            None
        } else {
            Some(buf.len())
        };
        let obj = self.read_bytes_capped(cap)?;
        if !buf.is_empty() {
            buf.get_mut(..obj.value.len())
                .ok_or(Error::Corrupted)?
                .copy_from_slice(&obj.value);
        }
        Ok(obj.value)
    }

    /// Reads the byte string at this cursor, allocating for it.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not bytes or I/O fails.
    pub fn read_bytes_alloc(&self) -> Result<Vec<u8>> {
        Ok(self.read_bytes_capped(None)?.value)
    }

    /// Reads the byte string plus format tag at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not bytes or I/O fails.
    pub fn read_bytes_object(&self, buf: &mut [u8]) -> Result<BytesObject> {
        let cap = if buf.is_empty() {
            None
        } else {
            Some(buf.len())
        };
        self.read_bytes_capped(cap)
    }

    /// Reads the key/value pair this cursor points at.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not a pair or I/O fails.
    pub fn read_key_value_pair(&self) -> Result<KeyValuePair<'db, C, H>> {
        let ptr = self.resolved()?;
        let (key_slot, value_slot, hash) = self.db.read_kv_pair(ptr.slot)?;
        let mk = |position: Option<u64>, slot: Slot| ReadCursor {
            db: self.db,
            slot_ptr: Cell::new(SlotPointer { position, slot }),
            frozen_at: Cell::new(None),
            epoch: Cell::new(None),
            _mode: PhantomData,
        };
        Ok(KeyValuePair {
            key: mk(self.db.kv_key_position(ptr.slot), key_slot),
            value: mk(self.db.kv_value_position(ptr.slot), value_slot),
            hash,
        })
    }

    /// Counts the elements in the collection at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the slot holds no countable value or I/O fails.
    pub fn count(&self) -> Result<u64> {
        let ptr = self.resolved()?;
        match ptr.slot.tag {
            Tag::None => Ok(0),
            Tag::ArrayList | Tag::LinkedArrayList | Tag::SortedMap | Tag::SortedSet => {
                let mut core = self.db.core().borrow_mut();
                core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                let mut b = [0u8; 16];
                core.read(&mut b)?;
                let size = codec::i64_be(&b, 0)?;
                if size < 0 {
                    return Err(Error::ExpectedUnsignedLong);
                }
                Ok(codec::u64_from_i64(size)?)
            }
            Tag::CountedHashMap | Tag::CountedHashSet => {
                let mut core = self.db.core().borrow_mut();
                core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                let mut b = [0u8; 8];
                core.read(&mut b)?;
                let size = i64::from_be_bytes(b);
                if size < 0 {
                    return Err(Error::ExpectedUnsignedLong);
                }
                Ok(codec::u64_from_i64(size)?)
            }
            Tag::Bytes => {
                let mut core = self.db.core().borrow_mut();
                core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                let mut b = [0u8; 8];
                core.read(&mut b)?;
                Ok(u64::from_be_bytes(b))
            }
            Tag::ShortBytes => {
                let (value, _) = decode_short(&ptr.slot);
                Ok(codec::u64_from_usize(value.len())?)
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// Reads through `path`, returning `None` for missing keys.
    ///
    /// # Errors
    ///
    /// Propagates traversal, corruption, and I/O failures.
    pub fn read_path(&self, path: &[PathPart<'_, C, H>]) -> Result<Option<ReadCursor<'db, C, H>>> {
        let start = self.resolved()?;
        if path.is_empty() {
            if start.slot.empty() {
                return Ok(None);
            }
            return Ok(Some(ReadCursor {
                db: self.db,
                slot_ptr: Cell::new(start),
                frozen_at: Cell::new(None),
                epoch: Cell::new(None),
                _mode: PhantomData,
            }));
        }
        // Reads traverse lookups only; anything else is rejected.
        // A missing key resolves to no cursor, like Go's `ReadPath`.
        let mut ptr = start;
        for part in path {
            let next = match part {
                PathPart::ArrayListGet { index } => {
                    Self::apply_arraylist_get(self.db, ptr, *index, 0, false)
                }
                PathPart::HashMapGet { target } => {
                    Self::apply_hashmap_get(self.db, ptr, target, 0, false)
                }
                PathPart::LinkedArrayListGet { index } => {
                    Self::apply_linked_get(self.db, ptr, *index, 0, false)
                }
                PathPart::SortedMapGet { target } => {
                    Self::apply_sorted_get(self.db, ptr, target, 0, false)
                }
                PathPart::SortedMapGetIndex { index } => {
                    Self::apply_sorted_get_index(self.db, ptr, *index)
                }
                _ => return Err(Error::WriteNotAllowed),
            };
            match next {
                Ok(p) => ptr = p,
                Err(Error::KeyNotFound) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        Ok(Some(ReadCursor {
            db: self.db,
            slot_ptr: Cell::new(ptr),
            frozen_at: Cell::new(None),
            epoch: Cell::new(None),
            _mode: PhantomData,
        }))
    }

    /// Reads the slot at the end of `path`, or an empty slot when the
    /// key is missing (mirrors Go's `ReadPathSlot`).
    /// Reads the slot found through `path`.
    ///
    /// # Errors
    ///
    /// Fails when the path misses or traversal/I/O fails.
    pub fn read_path_slot(&self, path: &[PathPart<'_, C, H>]) -> Result<Slot> {
        match self.read_path(path) {
            Ok(Some(c)) => c.slot(),
            Ok(None) => Ok(Slot {
                value: 0,
                tag: Tag::None,
                full: false,
            }),
            Err(e) => Err(e),
        }
    }

    /// Copies the collection to the end when the transaction froze it,
    /// so in-place writes below never touch committed bytes.
    /// Copies the collection to the end when the transaction froze it,
    /// so writes below never touch committed bytes. Mirrors Go's
    /// `copyCollectionIfFrozen`: only the transaction-wide freeze point
    /// triggers a copy (cursor-local `frozen` only affects descent).
    fn frozen_copy(
        db: &Database<C, H>,
        ptr: SlotPointer,
        init: &PathPart<'_, C, H>,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if let Some(f) = db.tx_frozen() {
            // Signed comparison like Go: corrupt negative offsets sort
            // below any freeze point and get copied, never mutated.
            if ptr.slot.value < codec::i64_from_u64(f)? {
                return match init {
                    PathPart::ArrayListInit => Self::apply_arraylist_init(db, ptr, frozen),
                    PathPart::HashMapInit { counted, set } => {
                        Self::apply_hashmap_init(db, ptr, *counted, *set, frozen)
                    }
                    PathPart::LinkedArrayListInit => Self::apply_linked_init(db, ptr, frozen),
                    PathPart::SortedMapInit { set } => {
                        Self::apply_sorted_init(db, ptr, *set, frozen)
                    }
                    _ => Err(Error::UnexpectedTag),
                };
            }
        }
        Ok(ptr)
    }

    fn read_array_header(db: &Database<C, H>, start: u64) -> Result<(u64, u64)> {
        let mut core = db.core().borrow_mut();
        core.seek_to(start)?;
        let mut b = [0u8; 16];
        core.read(&mut b)?;
        let size = codec::i64_be(&b, 0)?;
        let at = codec::i64_be(&b, 8)?;
        if size < 0 || at < 0 {
            return Err(Error::ExpectedUnsignedLong);
        }
        Ok((codec::u64_from_i64(size)?, codec::u64_from_i64(at)?))
    }

    fn write_array_header(db: &Database<C, H>, start: u64, size: u64, at: u64) -> Result<()> {
        let mut core = db.core().borrow_mut();
        core.seek_to(start)?;
        core.write(&codec::i64_from_u64(size)?.to_be_bytes())?;
        core.write(&codec::i64_from_u64(at)?.to_be_bytes())?;
        Ok(())
    }

    /// Copies an array's header + root block to the end. Returns the new
    /// `(start, block)`.
    fn copy_array_root(db: &Database<C, H>, start: u64) -> Result<(u64, u64)> {
        let (size, at) = Self::read_array_header(db, start)?;
        let mut block = vec![0u8; codec::usize_from_u64(crate::database::INDEX_BLOCK_LEN)?];
        {
            let mut core = db.core().borrow_mut();
            core.seek_to(at)?;
            core.read(&mut block)?;
        }
        let fresh = db.core().borrow_mut().committed_len()?;
        {
            let mut core = db.core().borrow_mut();
            core.seek_to(fresh)?;
            core.write(&codec::i64_from_u64(size)?.to_be_bytes())?;
            core.write(&codec::i64_from_u64(codec::add(fresh, 16)?)?.to_be_bytes())?;
            core.write(&block)?;
        }
        let _ = size;
        Ok((fresh, codec::add(fresh, 16)?))
    }

    fn apply_arraylist_init(
        db: &Database<C, H>,
        ptr: SlotPointer,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let position = ptr.position.ok_or(Error::CursorNotWriteable)?;
        match ptr.slot.tag {
            Tag::None => {
                let start = db.core().borrow_mut().committed_len()?;
                {
                    let mut core = db.core().borrow_mut();
                    core.seek_to(start)?;
                    core.write(&0i64.to_be_bytes())?;
                    core.write(&codec::i64_from_u64(codec::add(start, 16)?)?.to_be_bytes())?;
                    let zeros = vec![0u8; codec::usize_from_u64(crate::database::INDEX_BLOCK_LEN)?];
                    core.write(&zeros)?;
                }
                let slot = Slot {
                    value: codec::i64_from_u64(start)?,
                    tag: Tag::ArrayList,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            Tag::ArrayList => {
                let start = codec::u64_from_i64(ptr.slot.value)?;
                // Copy header + root block unless made in this transaction.
                let new_start = if db.is_old(start, frozen)? {
                    Self::copy_array_root(db, start)?.0
                } else {
                    start
                };
                let slot = Slot {
                    value: codec::i64_from_u64(new_start)?,
                    tag: Tag::ArrayList,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    fn apply_arraylist_get(
        db: &Database<C, H>,
        ptr: SlotPointer,
        index: i64,
        frozen: u64,
        for_write: bool,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag == Tag::None {
            return Err(Error::KeyNotFound);
        }
        if ptr.slot.tag != Tag::ArrayList {
            return Err(Error::UnexpectedTag);
        }
        let mut ptr = ptr;
        if for_write {
            ptr = Self::frozen_copy(db, ptr, &PathPart::ArrayListInit, frozen)?;
        }
        let start = codec::u64_from_i64(ptr.slot.value)?;
        let (size, at) = Self::read_array_header(db, start)?;
        if index >= codec::i64_from_u64(size)?
            || index
                < codec::i64_from_u64(size)?
                    .checked_neg()
                    .ok_or(Error::Corrupted)?
        {
            return Err(Error::KeyNotFound);
        }
        let key = if index < 0 {
            codec::iadd(codec::i64_from_u64(size)?, index)?
        } else {
            index
        };
        let shift = if size <= 16 {
            0
        } else {
            crate::database::log16_floor(codec::sub_u64(size, 1)?)?
        };
        db.read_arraylist_slot(at, key, shift, for_write, frozen)
    }

    fn apply_arraylist_append(
        db: &Database<C, H>,
        ptr: SlotPointer,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::ArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::ArrayListInit, frozen)?;
        let start = codec::u64_from_i64(ptr.slot.value)?;
        let (size, at) = Self::read_array_header(db, start)?;
        let ((new_size, new_at), sp) = db.read_arraylist_append(size, at, frozen)?;
        Self::write_array_header(db, start, new_size, new_at)?;
        db.note_append_root(ptr.position, sp.position);
        Ok(sp)
    }

    fn apply_hashmap_init(
        db: &Database<C, H>,
        ptr: SlotPointer,
        counted: bool,
        set: bool,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let position = ptr.position.ok_or(Error::CursorNotWriteable)?;
        let tag = match (counted, set) {
            (true, true) => Tag::CountedHashSet,
            (true, false) => Tag::CountedHashMap,
            (false, true) => Tag::HashSet,
            (false, false) => Tag::HashMap,
        };
        match ptr.slot.tag {
            Tag::None => {
                let start = db.core().borrow_mut().committed_len()?;
                {
                    let mut core = db.core().borrow_mut();
                    core.seek_to(start)?;
                    if counted {
                        core.write(&0i64.to_be_bytes())?;
                    }
                    let zeros = vec![0u8; codec::usize_from_u64(crate::database::INDEX_BLOCK_LEN)?];
                    core.write(&zeros)?;
                }
                let slot = Slot {
                    value: codec::i64_from_u64(start)?,
                    tag,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            Tag::HashMap | Tag::HashSet | Tag::CountedHashMap | Tag::CountedHashSet => {
                if counted {
                    match ptr.slot.tag {
                        Tag::CountedHashMap | Tag::CountedHashSet => {}
                        _ => return Err(Error::UnexpectedTag),
                    }
                } else {
                    match ptr.slot.tag {
                        Tag::HashMap | Tag::HashSet => {}
                        _ => return Err(Error::UnexpectedTag),
                    }
                }
                let start = codec::u64_from_i64(ptr.slot.value)?;
                let new_start = if db.is_old(start, frozen)? {
                    // Copy the count (if any) plus the root block.
                    let count_bytes = if counted {
                        let mut core = db.core().borrow_mut();
                        core.seek_to(start)?;
                        let mut b = [0u8; 8];
                        core.read(&mut b)?;
                        b.to_vec()
                    } else {
                        Vec::new()
                    };
                    let block_at = if counted {
                        codec::add(start, 8)?
                    } else {
                        start
                    };
                    let mut block =
                        vec![0u8; codec::usize_from_u64(crate::database::INDEX_BLOCK_LEN)?];
                    {
                        let mut core = db.core().borrow_mut();
                        core.seek_to(block_at)?;
                        core.read(&mut block)?;
                    }
                    let fresh = db.core().borrow_mut().committed_len()?;
                    {
                        let mut core = db.core().borrow_mut();
                        core.seek_to(fresh)?;
                        if counted {
                            core.write(&count_bytes)?;
                        }
                        core.write(&block)?;
                    }
                    fresh
                } else {
                    start
                };
                let slot = Slot {
                    value: codec::i64_from_u64(new_start)?,
                    tag,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    fn apply_hashmap_get(
        db: &Database<C, H>,
        ptr: SlotPointer,
        target: &GetTarget,
        frozen: u64,
        for_write: bool,
    ) -> Result<SlotPointer> {
        let counted = match ptr.slot.tag {
            Tag::None => return Err(Error::KeyNotFound),
            Tag::HashMap | Tag::HashSet => false,
            Tag::CountedHashMap | Tag::CountedHashSet => true,
            _ => return Err(Error::UnexpectedTag),
        };
        let mut ptr = ptr;
        if for_write {
            let set = matches!(ptr.slot.tag, Tag::HashSet | Tag::CountedHashSet);
            ptr = Self::frozen_copy(db, ptr, &PathPart::HashMapInit { counted, set }, frozen)?;
        }
        let index_pos = if counted {
            codec::add(codec::u64_from_i64(ptr.slot.value)?, 8)?
        } else {
            codec::u64_from_i64(ptr.slot.value)?
        };
        db.check_hash(target.hash())?;
        let hit = db.read_map_slot(index_pos, target.hash(), 0, for_write, target, frozen)?;
        if for_write && counted && hit.is_empty {
            let mut core = db.core().borrow_mut();
            core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
            let mut b = [0u8; 8];
            core.read(&mut b)?;
            let count = i64::from_be_bytes(b);
            core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
            core.write(&codec::iadd(count, 1)?.to_be_bytes())?;
        }
        Ok(hit.ptr)
    }

    fn apply_hashmap_remove(
        db: &Database<C, H>,
        ptr: SlotPointer,
        hash: &[u8],
        frozen: u64,
    ) -> Result<SlotPointer> {
        let counted = match ptr.slot.tag {
            Tag::None => return Err(Error::KeyNotFound),
            Tag::HashMap | Tag::HashSet => false,
            Tag::CountedHashMap | Tag::CountedHashSet => true,
            _ => return Err(Error::UnexpectedTag),
        };
        let set = matches!(ptr.slot.tag, Tag::HashSet | Tag::CountedHashSet);
        let ptr = Self::frozen_copy(db, ptr, &PathPart::HashMapInit { counted, set }, frozen)?;
        let index_pos = if counted {
            codec::add(codec::u64_from_i64(ptr.slot.value)?, 8)?
        } else {
            codec::u64_from_i64(ptr.slot.value)?
        };
        db.check_hash(hash)?;
        match db.remove_map_slot(index_pos, hash, 0, frozen) {
            Err(Error::KeyNotFound) => Err(Error::KeyNotFound),
            Err(e) => Err(e),
            Ok(_) => {
                if counted {
                    let mut core = db.core().borrow_mut();
                    core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                    let mut b = [0u8; 8];
                    core.read(&mut b)?;
                    let count = i64::from_be_bytes(b);
                    core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                    core.write(&codec::isub(count, 1)?.to_be_bytes())?;
                }
                Ok(ptr)
            }
        }
    }

    /// Iterates the cursor's collection in storage order.
    /// Iterates the collection at this cursor from the start.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not iterable or I/O fails.
    pub fn try_iter(&self) -> Result<Iter<'db, C, H>> {
        let ptr = self.resolved()?;
        Iter::new(self.db, ptr)
    }

    /// Iterates an array list from `index` (negatives count from the end).
    /// Out of range yields an empty iterator.
    /// Iterates the collection at this cursor from `index`.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not iterable or I/O fails.
    pub fn iter_from(&self, index: i64) -> Result<Iter<'db, C, H>> {
        let ptr = self.resolved()?;
        Iter::from_index(self.db, ptr, index)
    }

    /// Iterates a linked list or sorted map/set from rank `index`
    /// (negatives count from the end). Out of range yields empty.
    /// Iterates the sorted collection at this cursor from `index`.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not a sorted collection or I/O fails.
    pub fn iter_from_index(&self, index: i64) -> Result<Iter<'db, C, H>> {
        let ptr = self.resolved()?;
        Iter::from_btree_index(self.db, ptr, index)
    }

    /// Iterates a sorted map/set from the first key >= `key`.
    /// Iterates the sorted collection at this cursor from `key`.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not a sorted collection or I/O fails.
    pub fn iter_from_key(&self, key: &[u8]) -> Result<Iter<'db, C, H>> {
        let ptr = self.resolved()?;
        Iter::from_sorted_key(self.db, ptr, key)
    }

    fn apply_arraylist_slice(
        db: &Database<C, H>,
        ptr: SlotPointer,
        size: i64,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::ArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::ArrayListInit, frozen)?;
        let start = codec::u64_from_i64(ptr.slot.value)?;
        let (old_size, at) = Self::read_array_header(db, start)?;
        let (new_size, new_at) = db.read_arraylist_slice(old_size, at, size, frozen)?;
        Self::write_array_header(db, start, new_size, new_at)?;
        Ok(ptr)
    }

    /// Key/value cursors for in-transaction map writes (mirrors Go's
    /// `WriteKVPairCursor`).
    fn apply_sorted_get_index(
        db: &Database<C, H>,
        ptr: SlotPointer,
        index: i64,
    ) -> Result<SlotPointer> {
        match ptr.slot.tag {
            Tag::None => return Err(Error::KeyNotFound),
            Tag::SortedMap | Tag::SortedSet => {}
            _ => return Err(Error::UnexpectedTag),
        }
        let (at, size, root) = Self::sorted_header(db, ptr)?;
        let _ = at;
        if index >= codec::i64_from_u64(size)?
            || index
                < codec::i64_from_u64(size)?
                    .checked_neg()
                    .ok_or(Error::Corrupted)?
        {
            return Err(Error::KeyNotFound);
        }
        let rank = if index < 0 {
            codec::iadd(codec::i64_from_u64(size)?, index)?
        } else {
            index
        };
        db.descend_by_rank(root, rank, crate::database::SORTED_BRANCH_BLOCK)
    }

    fn read_btree_header(db: &Database<C, H>, at: u64) -> Result<(u64, u64)> {
        let mut core = db.core().borrow_mut();
        core.seek_to(at)?;
        let mut b = [0u8; 16];
        core.read(&mut b)?;
        let size = codec::i64_be(&b, 0)?;
        let root = codec::i64_be(&b, 8)?;
        if size < 0 || root < 0 {
            return Err(Error::ExpectedUnsignedLong);
        }
        Ok((codec::u64_from_i64(size)?, codec::u64_from_i64(root)?))
    }

    fn write_btree_header(db: &Database<C, H>, at: u64, size: u64, root: u64) -> Result<()> {
        let mut core = db.core().borrow_mut();
        core.seek_to(at)?;
        core.write(&codec::i64_from_u64(size)?.to_be_bytes())?;
        core.write(&codec::i64_from_u64(root)?.to_be_bytes())?;
        Ok(())
    }

    fn linked_header(db: &Database<C, H>, ptr: SlotPointer) -> Result<(u64, u64, u64)> {
        if ptr.slot.tag == Tag::None {
            return Err(Error::KeyNotFound);
        }
        if ptr.slot.tag != Tag::LinkedArrayList {
            return Err(Error::UnexpectedTag);
        }
        let (size, root) =
            Cursor::<C, H, ReadOnly>::read_btree_header(db, codec::u64_from_i64(ptr.slot.value)?)?;
        Ok((codec::u64_from_i64(ptr.slot.value)?, size, root))
    }

    fn apply_linked_init(
        db: &Database<C, H>,
        ptr: SlotPointer,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let position = ptr.position.ok_or(Error::CursorNotWriteable)?;
        match ptr.slot.tag {
            Tag::None => {
                let root = db.write_btree_node(&crate::database::BTreeNode {
                    kind: crate::database::BTREE_KIND_LEAF,
                    num: 0,
                    values: [Slot {
                        value: 0,
                        tag: Tag::None,
                        full: false,
                    }; crate::database::BTREE_SLOT_COUNT],
                    children: [Slot {
                        value: 0,
                        tag: Tag::None,
                        full: false,
                    }; crate::database::BTREE_SLOT_COUNT],
                    counts: [0; crate::database::BTREE_SLOT_COUNT],
                })?;
                let at = db.core().borrow_mut().committed_len()?;
                Self::write_btree_header(db, at, 0, root)?;
                let slot = Slot {
                    value: codec::i64_from_u64(at)?,
                    tag: Tag::LinkedArrayList,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            Tag::LinkedArrayList => {
                // Nodes are always appended, so only the header needs copying.
                let mut at = codec::u64_from_i64(ptr.slot.value)?;
                if db.is_old(at, frozen)? {
                    let (size, root) = Self::read_btree_header(db, at)?;
                    let fresh = db.core().borrow_mut().committed_len()?;
                    Self::write_btree_header(db, fresh, size, root)?;
                    at = fresh;
                }
                let slot = Slot {
                    value: codec::i64_from_u64(at)?,
                    tag: Tag::LinkedArrayList,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    fn apply_linked_get(
        db: &Database<C, H>,
        ptr: SlotPointer,
        index: i64,
        frozen: u64,
        for_write: bool,
    ) -> Result<SlotPointer> {
        let mut ptr = ptr;
        if for_write {
            ptr = Self::frozen_copy(db, ptr, &PathPart::LinkedArrayListInit, frozen)?;
        }
        let (at, size, root) = Self::linked_header(db, ptr)?;
        if index >= codec::i64_from_u64(size)?
            || index
                < codec::i64_from_u64(size)?
                    .checked_neg()
                    .ok_or(Error::Corrupted)?
        {
            return Err(Error::KeyNotFound);
        }
        let rank = if index < 0 {
            codec::iadd(codec::i64_from_u64(size)?, index)?
        } else {
            index
        };
        if !for_write {
            return db.descend_by_rank(root, rank, crate::database::BTREE_BRANCH_BLOCK);
        }
        let got = db.btree_get_for_write(root, rank, frozen)?;
        if got.node_ptr != root {
            Self::write_btree_header(db, at, size, got.node_ptr)?;
        }
        Ok(SlotPointer {
            position: Some(got.value_pos),
            slot: got.slot,
        })
    }

    fn apply_linked_append(
        db: &Database<C, H>,
        ptr: SlotPointer,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::LinkedArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::LinkedArrayListInit, frozen)?;
        let (at, size, root) = Self::linked_header(db, ptr)?;
        let ins = db.btree_insert(root, codec::i64_from_u64(size)?, frozen)?;
        let grown = db.btree_grow_root(ins)?;
        Self::write_btree_header(db, at, codec::add(size, 1)?, grown)?;
        Ok(SlotPointer {
            position: Some(ins.value_pos),
            slot: empty_slot(),
        })
    }

    fn apply_linked_slice(
        db: &Database<C, H>,
        ptr: SlotPointer,
        offset: i64,
        size: i64,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::LinkedArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::LinkedArrayListInit, frozen)?;
        let (at, total, root) = Self::linked_header(db, ptr)?;
        // Bounds-checked without overflow (`offset + size` could wrap).
        let total_signed = codec::i64_from_u64(total)?;
        if offset < 0
            || size < 0
            || offset > total_signed
            || size > total_signed.checked_sub(offset).ok_or(Error::Corrupted)?
        {
            return Err(Error::KeyNotFound);
        }
        let after = db.btree_split(root, offset)?.right;
        let sliced = db.btree_split(after, size)?.left;
        Self::write_btree_header(db, at, codec::u64_from_i64(size)?, sliced)?;
        Ok(ptr)
    }

    fn apply_linked_concat(
        db: &Database<C, H>,
        ptr: SlotPointer,
        list: Slot,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::LinkedArrayList || list.tag != Tag::LinkedArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::LinkedArrayListInit, frozen)?;
        let (at, size_a, root_a) = Self::linked_header(db, ptr)?;
        let (_, size_b, root_b) = Self::linked_header(
            db,
            SlotPointer {
                position: None,
                slot: list,
            },
        )?;
        // The join shares subtrees with both operands, so freeze everything
        // created so far: later in-place writes must copy those nodes.
        let tx_start = db.core().borrow_mut().committed_len()?;
        db.set_tx_start(Some(tx_start));
        let joined = db.btree_join(root_a, root_b)?;
        Self::write_btree_header(db, at, codec::add(size_a, size_b)?, joined)?;
        Ok(ptr)
    }

    fn apply_linked_insert(
        db: &Database<C, H>,
        ptr: SlotPointer,
        index: i64,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::LinkedArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::LinkedArrayListInit, frozen)?;
        let (at, size, root) = Self::linked_header(db, ptr)?;
        if index >= codec::i64_from_u64(size)?
            || index
                < codec::i64_from_u64(size)?
                    .checked_neg()
                    .ok_or(Error::Corrupted)?
        {
            return Err(Error::KeyNotFound);
        }
        let rank = if index < 0 {
            codec::iadd(codec::i64_from_u64(size)?, index)?
        } else {
            index
        };
        let ins = db.btree_insert(root, rank, frozen)?;
        let grown = db.btree_grow_root(ins)?;
        Self::write_btree_header(db, at, codec::add(size, 1)?, grown)?;
        Ok(SlotPointer {
            position: Some(ins.value_pos),
            slot: empty_slot(),
        })
    }

    fn apply_linked_remove(
        db: &Database<C, H>,
        ptr: SlotPointer,
        index: i64,
        frozen: u64,
    ) -> Result<SlotPointer> {
        if ptr.slot.tag != Tag::LinkedArrayList {
            return Err(Error::UnexpectedTag);
        }
        let ptr = Self::frozen_copy(db, ptr, &PathPart::LinkedArrayListInit, frozen)?;
        let (at, size, root) = Self::linked_header(db, ptr)?;
        if index >= codec::i64_from_u64(size)?
            || index
                < codec::i64_from_u64(size)?
                    .checked_neg()
                    .ok_or(Error::Corrupted)?
        {
            return Err(Error::KeyNotFound);
        }
        let rank = if index < 0 {
            codec::iadd(codec::i64_from_u64(size)?, index)?
        } else {
            index
        };
        let new_root = if size == 1 {
            db.write_btree_node(&crate::database::BTreeNode {
                kind: crate::database::BTREE_KIND_LEAF,
                num: 0,
                values: [Slot {
                    value: 0,
                    tag: Tag::None,
                    full: false,
                }; crate::database::BTREE_SLOT_COUNT],
                children: [Slot {
                    value: 0,
                    tag: Tag::None,
                    full: false,
                }; crate::database::BTREE_SLOT_COUNT],
                counts: [0; crate::database::BTREE_SLOT_COUNT],
            })?
        } else {
            let mut node = db.btree_remove(root, rank, frozen)?;
            let mut new_root = db.btree_write_node(&node, root, frozen)?;
            // A root left with one child is replaced by it.
            loop {
                if node.kind != crate::database::BTREE_KIND_BRANCH || node.num != 1 {
                    break;
                }
                new_root = codec::child_ptr(&node.children, 0)?;
                node = db.read_btree_node(new_root)?;
            }
            new_root
        };
        Self::write_btree_header(db, at, codec::sub_u64(size, 1)?, new_root)?;
        Ok(ptr)
    }

    fn apply_sorted_init(
        db: &Database<C, H>,
        ptr: SlotPointer,
        set: bool,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let position = ptr.position.ok_or(Error::CursorNotWriteable)?;
        let tag = if set { Tag::SortedSet } else { Tag::SortedMap };
        match ptr.slot.tag {
            Tag::None => {
                let root = db.write_sorted_node(&crate::database::SortedNode {
                    kind: crate::database::BTREE_KIND_LEAF,
                    num: 0,
                    entries: [empty_slot(); crate::database::BTREE_SLOT_COUNT],
                    children: [empty_slot(); crate::database::BTREE_SLOT_COUNT],
                    separators: [empty_slot(); crate::database::BTREE_SLOT_COUNT],
                    counts: [0; crate::database::BTREE_SLOT_COUNT],
                })?;
                let at = db.core().borrow_mut().committed_len()?;
                Self::write_btree_header(db, at, 0, root)?;
                let slot = Slot {
                    value: codec::i64_from_u64(at)?,
                    tag,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            Tag::SortedMap | Tag::SortedSet => {
                let mut at = codec::u64_from_i64(ptr.slot.value)?;
                if db.is_old(at, frozen)? {
                    let (size, root) = Self::read_btree_header(db, at)?;
                    let fresh = db.core().borrow_mut().committed_len()?;
                    Self::write_btree_header(db, fresh, size, root)?;
                    at = fresh;
                }
                let slot = Slot {
                    value: codec::i64_from_u64(at)?,
                    tag,
                    full: false,
                };
                db.write_slot_at(position, slot)?;
                Ok(SlotPointer {
                    position: Some(position),
                    slot,
                })
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    fn apply_sorted_get(
        db: &Database<C, H>,
        ptr: SlotPointer,
        target: &SortedMapTarget,
        frozen: u64,
        for_write: bool,
    ) -> Result<SlotPointer> {
        match ptr.slot.tag {
            Tag::None => return Err(Error::KeyNotFound),
            Tag::SortedMap | Tag::SortedSet => {}
            _ => return Err(Error::UnexpectedTag),
        }
        let mut ptr = ptr;
        if for_write {
            let set = ptr.slot.tag == Tag::SortedSet;
            ptr = Self::frozen_copy(db, ptr, &PathPart::SortedMapInit { set }, frozen)?;
        }
        let (at, _, root) = Self::sorted_header(db, ptr)?;
        if !for_write {
            let entry = db
                .sorted_get(root, target.key())?
                .ok_or(Error::KeyNotFound)?;
            return db.sorted_target_slot(entry, target);
        }
        let key_only = ptr.slot.tag == Tag::SortedSet && matches!(target, SortedMapTarget::Key(_));
        let ins = db.sorted_put(root, target.key(), key_only, frozen)?;
        let grown = db.sorted_grow_root(ins)?;
        let (_, size, _) = Self::sorted_header(db, ptr)?;
        Self::write_btree_header(db, at, codec::add(size, u64::from(ins.added))?, grown)?;
        db.sorted_target_slot(ins.entry, target)
    }

    fn sorted_header(db: &Database<C, H>, ptr: SlotPointer) -> Result<(u64, u64, u64)> {
        if ptr.slot.tag == Tag::None {
            return Err(Error::KeyNotFound);
        }
        if !matches!(ptr.slot.tag, Tag::SortedMap | Tag::SortedSet) {
            return Err(Error::UnexpectedTag);
        }
        let (size, root) =
            Cursor::<C, H, ReadOnly>::read_btree_header(db, codec::u64_from_i64(ptr.slot.value)?)?;
        Ok((codec::u64_from_i64(ptr.slot.value)?, size, root))
    }

    fn apply_sorted_remove(
        db: &Database<C, H>,
        ptr: SlotPointer,
        key: &[u8],
        frozen: u64,
    ) -> Result<SlotPointer> {
        match ptr.slot.tag {
            Tag::None => return Err(Error::KeyNotFound),
            Tag::SortedMap | Tag::SortedSet => {}
            _ => return Err(Error::UnexpectedTag),
        }
        let set = ptr.slot.tag == Tag::SortedSet;
        let ptr = Self::frozen_copy(db, ptr, &PathPart::SortedMapInit { set }, frozen)?;
        let (at, size, root) = Self::sorted_header(db, ptr)?;
        let res = db.sorted_remove(root, key, frozen)?;
        if !res.found {
            return Err(Error::KeyNotFound);
        }
        Self::write_btree_header(db, at, codec::sub_u64(size, 1)?, res.node_ptr)?;
        Ok(ptr)
    }

    /// Opens a streaming reader over the bytes at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the slot is not bytes or I/O fails.
    pub fn reader(&self, buf: &mut [u8]) -> Result<Reader<'db, C, H>> {
        Reader::new(self, buf.len().max(1))
    }
}

/// Advance-or-finish signal for per-family path dispatch.
enum Step {
    /// Continue with the updated pointer.
    Next(SlotPointer),
    /// Finish the path early with the updated pointer.
    Done(SlotPointer),
}
impl<'db, C: Core, H: Hasher> Cursor<'db, C, H, ReadWrite> {
    fn check_writable(&self) -> Result<()> {
        let ptr = self.slot_ptr.get();
        if !self.db.in_tx() {
            if !is_root(&ptr) {
                return Err(Error::ExpectedTxStart);
            }
        } else if !is_root(&ptr) {
            match self.epoch.get() {
                None => return Err(Error::ExpectedTxStart),
                Some(e) if e != self.db.current_epoch() => {
                    return Err(Error::ExpiredTransaction);
                }
                _ => {}
            }
        }
        self.db.check_frozen_slot(ptr)
    }

    /// Writes through `path`, committing a root-started transaction like
    /// Go's `WritePath`. Nested top-level writes are rejected.
    ///
    /// # Errors
    ///
    /// Propagates path-application, transaction, and I/O failures.
    pub fn write_path(&self, path: &[PathPart<'_, C, H>]) -> Result<Self> {
        self.check_writable()?;
        let ptr = self.slot_ptr.get();
        if self.db.in_tx() && is_root(&ptr) && !path.is_empty() {
            return Err(Error::NestedTopLevelWrite);
        }
        if path.is_empty() {
            let resolved = self.resolved()?;
            return Ok(Self {
                db: self.db,
                slot_ptr: Cell::new(resolved),
                frozen_at: self.frozen_at.clone(),
                epoch: self.epoch.clone(),
                _mode: PhantomData,
            });
        }
        if is_root(&ptr) {
            // Initializing an initialized root is free: no transaction,
            // just discard crash leftovers and reuse it.
            if path.len() == 1 {
                if let Some(tag) = path.first().and_then(Self::init_tag) {
                    let root = self.db.read_root_slot()?;
                    if root.tag == tag {
                        self.db.truncate()?;
                        return Ok(Self {
                            db: self.db,
                            slot_ptr: Cell::new(SlotPointer {
                                position: Some(ROOT_HEADER_START),
                                slot: root,
                            }),
                            frozen_at: Cell::new(None),
                            epoch: Cell::new(None),
                            _mode: PhantomData,
                        });
                    }
                }
            }
            let frozen = self.frozen_at.get().unwrap_or(0);
            // `transact_root` wraps this same database, so `self.db` (at
            // `Self`'s lifetime) is the identical object as `c.db`.
            let result = self.db.transact_root(
                |c| Self::apply_parts(self.db, c.slot_ptr.get(), path, frozen, true),
                frozen,
            )?;
            // A remapped root result is a fresh root cursor; anything
            // else belongs to the just-committed transaction's epoch.
            let epoch = if result.position == Some(ROOT_HEADER_START) {
                None
            } else {
                Some(self.db.current_epoch())
            };
            return Ok(Self {
                db: self.db,
                slot_ptr: Cell::new(result),
                frozen_at: self.frozen_at.clone(),
                epoch: Cell::new(epoch),
                _mode: PhantomData,
            });
        }
        // Non-root: reload (an earlier write may have moved the slot),
        // apply, and sync when no transaction is open.
        if self.db.in_tx() {
            if let Some(pos) = ptr.position {
                let mut core = self.db.core().borrow_mut();
                core.seek_to(pos)?;
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                let slot = Slot::from_bytes(b);
                slot.validate()?;
                self.slot_ptr.set(SlotPointer {
                    position: Some(pos),
                    slot,
                });
            }
        }
        let frozen = self.frozen_at.get().unwrap_or(0);
        let result = Self::apply_parts(self.db, self.slot_ptr.get(), path, frozen, true)?;
        if !self.db.in_tx() {
            self.db.core().borrow_mut().sync()?;
        }
        // Re-read: an earlier write may have moved what the slot points to.
        let result = if self.db.in_tx() {
            if let Some(pos) = result.position {
                let mut core = self.db.core().borrow_mut();
                core.seek_to(pos)?;
                let mut b = [0u8; SLOT_LEN];
                core.read(&mut b)?;
                let slot = Slot::from_bytes(b);
                slot.validate()?;
                SlotPointer {
                    position: Some(pos),
                    slot,
                }
            } else {
                result
            }
        } else {
            result
        };
        Ok(Self {
            db: self.db,
            slot_ptr: Cell::new(result),
            frozen_at: self.frozen_at.clone(),
            epoch: Cell::new(Some(self.db.current_epoch())),
            _mode: PhantomData,
        })
    }

    /// The collection tag an init part creates, if any (mirrors Go).
    const fn init_tag(part: &PathPart<'_, C, H>) -> Option<Tag> {
        match part {
            PathPart::ArrayListInit => Some(Tag::ArrayList),
            PathPart::LinkedArrayListInit => Some(Tag::LinkedArrayList),
            PathPart::SortedMapInit { set } => {
                Some(if *set { Tag::SortedSet } else { Tag::SortedMap })
            }
            PathPart::HashMapInit { counted, set } => Some(match (counted, set) {
                (true, true) => Tag::CountedHashSet,
                (true, false) => Tag::CountedHashMap,
                (false, true) => Tag::HashSet,
                (false, false) => Tag::HashMap,
            }),
            _ => None,
        }
    }

    /// Runs a trailing `Context` closure step.
    ///
    /// # Errors
    ///
    /// Propagates closure and cursor failures.
    fn apply_context_part(
        db: &'db Database<C, H>,
        ptr: SlotPointer,
        ctx: &ContextPart<'_, C, H>,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let f = ctx
            .func
            .borrow_mut()
            .take()
            .ok_or(Error::CursorNotWriteable)?;
        let nc = Self {
            db,
            slot_ptr: Cell::new(ptr),
            frozen_at: Cell::new(Some(frozen)),
            epoch: Cell::new(Some(db.current_epoch())),
            _mode: PhantomData,
        };
        f(&nc)?;
        Ok(nc.slot_ptr.get())
    }

    /// Dispatches array-list path steps.
    ///
    /// # Errors
    ///
    /// Propagates collection and I/O failures.
    fn apply_array_part(
        db: &Database<C, H>,
        mut ptr: SlotPointer,
        part: &PathPart<'_, C, H>,
        frozen: u64,
        for_write: bool,
    ) -> Result<Step> {
        match part {
            PathPart::ArrayListInit => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_arraylist_init(db, ptr, frozen)?;
            }
            PathPart::ArrayListGet { index } => {
                ptr = Self::apply_arraylist_get(db, ptr, *index, frozen, for_write)?;
            }
            PathPart::ArrayListAppend => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_arraylist_append(db, ptr, frozen)?;
            }
            PathPart::ArrayListSlice { size } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_arraylist_slice(db, ptr, *size, frozen)?;
            }
            _ => return Err(Error::UnexpectedTag),
        }
        Ok(Step::Next(ptr))
    }

    /// Dispatches hash-map path steps.
    ///
    /// # Errors
    ///
    /// Propagates collection and I/O failures.
    fn apply_map_part(
        db: &Database<C, H>,
        mut ptr: SlotPointer,
        part: &PathPart<'_, C, H>,
        frozen: u64,
        for_write: bool,
    ) -> Result<Step> {
        match part {
            PathPart::HashMapInit { counted, set } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_hashmap_init(db, ptr, *counted, *set, frozen)?;
            }
            PathPart::HashMapGet { target } => {
                ptr = Self::apply_hashmap_get(db, ptr, target, frozen, for_write)?;
            }
            PathPart::HashMapRemove { hash } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                // Removal ends the path, like Go.
                return Ok(Step::Done(Self::apply_hashmap_remove(
                    db, ptr, hash, frozen,
                )?));
            }
            _ => return Err(Error::UnexpectedTag),
        }
        Ok(Step::Next(ptr))
    }

    /// Dispatches sorted-map path steps.
    ///
    /// # Errors
    ///
    /// Propagates collection and I/O failures.
    fn apply_sorted_part(
        db: &Database<C, H>,
        mut ptr: SlotPointer,
        part: &PathPart<'_, C, H>,
        frozen: u64,
        for_write: bool,
    ) -> Result<Step> {
        match part {
            PathPart::SortedMapInit { set } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_sorted_init(db, ptr, *set, frozen)?;
            }
            PathPart::SortedMapGet { target } => {
                ptr = Self::apply_sorted_get(db, ptr, target, frozen, for_write)?;
            }
            PathPart::SortedMapGetIndex { .. } => {
                return Err(Error::WriteNotAllowed);
            }
            PathPart::SortedMapRemove { key } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                return Ok(Step::Done(Self::apply_sorted_remove(db, ptr, key, frozen)?));
            }
            _ => return Err(Error::UnexpectedTag),
        }
        Ok(Step::Next(ptr))
    }

    /// Dispatches linked-list path steps.
    ///
    /// # Errors
    ///
    /// Propagates collection and I/O failures.
    fn apply_linked_part(
        db: &Database<C, H>,
        mut ptr: SlotPointer,
        part: &PathPart<'_, C, H>,
        frozen: u64,
        for_write: bool,
    ) -> Result<Step> {
        match part {
            PathPart::LinkedArrayListInit => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_linked_init(db, ptr, frozen)?;
            }
            PathPart::LinkedArrayListGet { index } => {
                ptr = Self::apply_linked_get(db, ptr, *index, frozen, for_write)?;
            }
            PathPart::LinkedArrayListAppend => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_linked_append(db, ptr, frozen)?;
            }
            PathPart::LinkedArrayListSlice { offset, size } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_linked_slice(db, ptr, *offset, *size, frozen)?;
            }
            PathPart::LinkedArrayListConcat { list } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_linked_concat(db, ptr, *list, frozen)?;
            }
            PathPart::LinkedArrayListInsert { index } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_linked_insert(db, ptr, *index, frozen)?;
            }
            PathPart::LinkedArrayListRemove { index } => {
                if !for_write {
                    return Err(Error::WriteNotAllowed);
                }
                ptr = Self::apply_linked_remove(db, ptr, *index, frozen)?;
            }
            _ => return Err(Error::UnexpectedTag),
        }
        Ok(Step::Next(ptr))
    }
    fn apply_parts(
        db: &'db Database<C, H>,
        mut ptr: SlotPointer,
        path: &[PathPart<'_, C, H>],
        frozen: u64,
        for_write: bool,
    ) -> Result<SlotPointer> {
        let mut i = 0;
        while i < path.len() {
            let part = path.get(i).ok_or(Error::Corrupted)?;
            match part {
                PathPart::Write(data) => {
                    if !for_write {
                        return Err(Error::WriteNotAllowed);
                    }
                    ptr = Self::apply_write(db, ptr, data, frozen)?;
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
                PathPart::Context(ctx) => {
                    if !for_write {
                        return Err(Error::WriteNotAllowed);
                    }
                    if i.checked_add(1).ok_or(Error::Corrupted)? != path.len() {
                        return Err(Error::PathPartMustBeAtEnd);
                    }
                    ptr = Self::apply_context_part(db, ptr, ctx, frozen)?;
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
                part @ (PathPart::ArrayListInit
                | PathPart::ArrayListGet { .. }
                | PathPart::ArrayListAppend
                | PathPart::ArrayListSlice { .. }) => {
                    match Self::apply_array_part(db, ptr, part, frozen, for_write)? {
                        Step::Next(next) => ptr = next,
                        Step::Done(done) => return Ok(done),
                    }
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
                part @ (PathPart::HashMapInit { .. }
                | PathPart::HashMapGet { .. }
                | PathPart::HashMapRemove { .. }) => {
                    match Self::apply_map_part(db, ptr, part, frozen, for_write)? {
                        Step::Next(next) => ptr = next,
                        Step::Done(done) => return Ok(done),
                    }
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
                part @ (PathPart::SortedMapInit { .. }
                | PathPart::SortedMapGet { .. }
                | PathPart::SortedMapGetIndex { .. }
                | PathPart::SortedMapRemove { .. }) => {
                    match Self::apply_sorted_part(db, ptr, part, frozen, for_write)? {
                        Step::Next(next) => ptr = next,
                        Step::Done(done) => return Ok(done),
                    }
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
                part @ (PathPart::LinkedArrayListInit
                | PathPart::LinkedArrayListGet { .. }
                | PathPart::LinkedArrayListAppend
                | PathPart::LinkedArrayListConcat { .. }
                | PathPart::LinkedArrayListInsert { .. }
                | PathPart::LinkedArrayListRemove { .. }
                | PathPart::LinkedArrayListSlice { .. }) => {
                    match Self::apply_linked_part(db, ptr, part, frozen, for_write)? {
                        Step::Next(next) => ptr = next,
                        Step::Done(done) => return Ok(done),
                    }
                    i = i.checked_add(1).ok_or(Error::Corrupted)?;
                }
            }
        }
        Ok(ptr)
    }

    fn apply_write(
        db: &Database<C, H>,
        ptr: SlotPointer,
        data: &Writeable,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let position = ptr.position.ok_or(Error::CursorNotWriteable)?;
        let mut slot = match data {
            Writeable::None => Slot {
                value: 0,
                tag: Tag::None,
                full: false,
            },
            Writeable::Slot(s) => *s,
            Writeable::Uint(v) => Slot {
                value: codec::bits_i64(*v),
                tag: Tag::Uint,
                full: false,
            },
            Writeable::Int(v) => Slot {
                value: *v,
                tag: Tag::Int,
                full: false,
            },
            Writeable::Float(v) => Slot {
                value: codec::bits_i64(v.to_bits()),
                tag: Tag::Float,
                full: false,
            },
            Writeable::Bytes(value) => {
                return Self::apply_bytes(
                    db,
                    ptr,
                    &BytesObject {
                        value: value.clone(),
                        format_tag: None,
                    },
                    frozen,
                );
            }
            Writeable::BytesObject { value, format_tag } => {
                return Self::apply_bytes(
                    db,
                    ptr,
                    &BytesObject {
                        value: value.clone(),
                        format_tag: *format_tag,
                    },
                    frozen,
                );
            }
        };
        if slot.tag == Tag::None {
            slot.full = true;
        }
        {
            let mut core = db.core().borrow_mut();
            core.seek_to(position)?;
            core.write(&slot.to_bytes())?;
        }
        Ok(SlotPointer {
            position: Some(position),
            slot,
        })
    }

    fn apply_bytes(
        db: &Database<C, H>,
        ptr: SlotPointer,
        data: &BytesObject,
        frozen: u64,
    ) -> Result<SlotPointer> {
        let position = ptr.position.ok_or(Error::CursorNotWriteable)?;
        if data.is_short() {
            let mut buf = [0u8; 8];
            buf.get_mut(..data.value.len())
                .ok_or(Error::Corrupted)?
                .copy_from_slice(&data.value);
            if let Some(tag) = data.format_tag {
                buf.get_mut(6..)
                    .ok_or(Error::Corrupted)?
                    .copy_from_slice(&tag);
            }
            let slot = Slot {
                value: codec::i64_from_u64(u64::from_be_bytes(buf))?,
                tag: Tag::ShortBytes,
                full: data.format_tag.is_some(),
            };
            {
                let mut core = db.core().borrow_mut();
                core.seek_to(position)?;
                core.write(&slot.to_bytes())?;
            }
            return Ok(SlotPointer {
                position: Some(position),
                slot,
            });
        }
        // Long byte arrays stream through a writer at the scratch cursor.
        // `finish` persists the parent slot, like Go's `CursorWriter`.
        let temp = WriteCursor {
            db,
            slot_ptr: Cell::new(ptr),
            frozen_at: Cell::new(Some(frozen)),
            epoch: Cell::new(Some(db.current_epoch())),
            _mode: PhantomData,
        };
        let mut writer = Writer::new(&temp)?;
        writer.set_format_tag_opt(data.format_tag);
        writer.write_bytes(&data.value)?;
        let slot = writer.finish()?;
        Ok(SlotPointer {
            position: Some(position),
            slot,
        })
    }

    /// Stages a key/value pair write at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the cursor is not writable or I/O fails.
    pub fn write_key_value_pair(&self) -> Result<WriteKeyValuePair<'db, C, H>> {
        let ptr = self.slot_ptr.get();
        let (key_slot, value_slot, hash) = self.db.read_kv_pair(ptr.slot)?;
        let mk = |position: Option<u64>, slot: Slot| WriteCursor {
            db: self.db,
            slot_ptr: Cell::new(SlotPointer { position, slot }),
            frozen_at: self.frozen_at.clone(),
            epoch: Cell::new(Some(self.db.current_epoch())),
            _mode: PhantomData,
        };
        Ok(WriteKeyValuePair {
            key: mk(self.db.kv_key_position(ptr.slot), key_slot),
            value: mk(self.db.kv_value_position(ptr.slot), value_slot),
            hash,
        })
    }

    /// Writes `data` at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the cursor is not writable or I/O fails.
    pub fn write(&self, data: Writeable) -> Result<()> {
        let result = self.write_path(&[PathPart::Write(data)])?;
        self.slot_ptr.set(result.slot_ptr.get());
        Ok(())
    }

    /// Writes `data` only when this cursor holds nothing.
    ///
    /// # Errors
    ///
    /// Fails when the cursor is not writable or I/O fails.
    pub fn write_if_empty(&self, data: Writeable) -> Result<()> {
        self.check_writable()?;
        if self.resolved()?.slot.empty() {
            self.write(data)?;
        }
        Ok(())
    }

    /// Later writes through this cursor copy what the transaction wrote
    /// so far instead of changing it in place.
    /// Freezes the value at this cursor against later writes.
    ///
    /// # Errors
    ///
    /// Fails when the slot cannot be frozen or I/O fails.
    pub fn freeze(&self) -> Result<()> {
        if !self.db.in_tx() {
            return Err(Error::ExpectedTxStart);
        }
        let len = self.db.core().borrow_mut().committed_len()?;
        self.frozen_at.set(Some(len));
        Ok(())
    }

    /// Opens a streaming writer for the bytes at this cursor.
    ///
    /// # Errors
    ///
    /// Fails when the cursor is not writable or I/O fails.
    pub fn writer<'a>(&'a self, _buf: &mut [u8]) -> Result<Writer<'a, 'db, C, H>> {
        // `_buf` reserves Zig's scratch-buffer convention; bytes stream
        // directly to the store exactly like Go's `Writer()`.
        Writer::new(self)
    }
}

/// Incremental byte reader. `chunk` comes from the caller's scratch
/// length, bounding each core read.
pub struct Reader<'db, C: Core, H: Hasher> {
    db: &'db Database<C, H>,
    kind: ReaderKind,
    rel: u64,
    chunk: usize,
}

enum ReaderKind {
    External { size: u64, start: u64 },
    Inline { data: [u8; 8], len: usize },
}

impl<'db, C: Core, H: Hasher> Reader<'db, C, H> {
    fn new<Mode>(cursor: &Cursor<'db, C, H, Mode>, chunk: usize) -> Result<Self> {
        Self::from_resolved(cursor.db, cursor.resolved()?, chunk)
    }

    fn from_resolved(db: &'db Database<C, H>, ptr: SlotPointer, chunk: usize) -> Result<Self> {
        let kind = match ptr.slot.tag {
            Tag::Bytes => {
                let mut core = db.core().borrow_mut();
                core.seek_to(codec::u64_from_i64(ptr.slot.value)?)?;
                let mut b = [0u8; 8];
                core.read(&mut b)?;
                let size = u64::from_be_bytes(b);
                let start = codec::add(codec::u64_from_i64(ptr.slot.value)?, 8)?;
                ReaderKind::External { size, start }
            }
            Tag::ShortBytes => {
                let (value, _) = decode_short(&ptr.slot);
                let mut data = [0u8; 8];
                data.get_mut(..value.len())
                    .ok_or(Error::Corrupted)?
                    .copy_from_slice(&value);
                ReaderKind::Inline {
                    data,
                    len: value.len(),
                }
            }
            _ => return Err(Error::UnexpectedTag),
        };
        Ok(Self {
            db,
            kind,
            rel: 0,
            chunk,
        })
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        match &self.kind {
            ReaderKind::External { size, .. } => *size,
            ReaderKind::Inline { len, .. } => u64::try_from(*len).unwrap_or(u64::MAX),
        }
    }

    /// Seeks the reader to `pos`.
    ///
    /// # Errors
    ///
    /// Fails when `pos` lies beyond the value or I/O fails.
    pub fn seek_to(&mut self, pos: u64) -> Result<()> {
        if pos > self.size() {
            return Err(Error::InvalidOffset);
        }
        self.rel = pos;
        Ok(())
    }

    /// Reads the remainder of the value into a vector.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    pub fn read_all(&mut self) -> Result<Vec<u8>> {
        let total = self.size();
        let mut out = Vec::with_capacity(codec::usize_from_u64(codec::sub_u64(total, self.rel)?)?);
        match &self.kind {
            ReaderKind::External { start, .. } => {
                let mut core = self.db.core().borrow_mut();
                while self.rel < total {
                    let want = codec::usize_from_u64(
                        total
                            .min(codec::add(self.rel, codec::u64_from_usize(self.chunk)?)?)
                            .checked_sub(self.rel)
                            .ok_or(Error::Corrupted)?,
                    )?;
                    let mut buf = vec![0u8; want];
                    core.seek_to(codec::add(*start, self.rel)?)?;
                    core.read(&mut buf)?;
                    out.extend_from_slice(&buf);
                    self.rel = codec::add(self.rel, codec::u64_from_usize(want)?)?;
                }
            }
            ReaderKind::Inline { data, len } => {
                let start = codec::usize_from_u64(self.rel)?;
                let end = *len;
                if start > end {
                    return Err(Error::Corrupted);
                }
                out.extend_from_slice(data.get(start..end).ok_or(Error::Corrupted)?);
                self.rel = codec::u64_from_usize(*len)?;
            }
        }
        Ok(out)
    }
}

/// Streaming byte writer. Bytes go directly to the store; `finish`
/// backfills the size, appends the format tag, and points the parent
/// slot at the array — exactly like Go's `CursorWriter`.
pub struct Writer<'a, 'db, C: Core, H: Hasher> {
    db: &'db Database<C, H>,
    /// Shared with the parent cursor: `finish` points it at the array,
    /// mirroring Go's `CursorWriter`, which mutates its parent in place.
    /// Borrow ends when the writer is last used.
    slot_cell: &'a Cell<SlotPointer>,
    epoch: Option<u64>,
    parent_pos: Option<u64>,
    size: u64,
    start: u64,
    rel: u64,
    ptr_pos: u64,
    format_tag: Option<[u8; 2]>,
    slot: Slot,
    frozen: u64,
}

impl<'a, 'db, C: Core, H: Hasher> Writer<'a, 'db, C, H> {
    fn new(parent: &'a WriteCursor<'db, C, H>) -> Result<Self> {
        parent.check_writable()?;
        let ptr = parent.slot_ptr.get();
        if is_root(&ptr) {
            return Err(Error::CursorNotWriteable);
        }
        if !parent.db.in_tx() {
            return Err(Error::ExpectedTxStart);
        }
        let frozen = parent.frozen_at.get().unwrap_or(0);
        let mut core = parent.db.core().borrow_mut();
        let ptr_pos = core.committed_len()?;
        core.seek_to(ptr_pos)?;
        core.write(&0u64.to_be_bytes())?;
        let start = codec::add(ptr_pos, 8)?;
        // Provisionally point the parent at the array under construction;
        // `finish` resolves the format tag. Mirrors Go, where the parent
        // slot only resolves at `Finish`, except the in-memory view stays
        // usable throughout.
        parent.slot_ptr.set(SlotPointer {
            position: ptr.position,
            slot: Slot {
                value: codec::i64_from_u64(ptr_pos)?,
                tag: Tag::Bytes,
                full: false,
            },
        });
        Ok(Self {
            db: parent.db,
            slot_cell: &parent.slot_ptr,
            epoch: parent.epoch.get(),
            parent_pos: ptr.position,
            size: 0,
            start,
            rel: 0,
            ptr_pos,
            format_tag: None,
            slot: Slot {
                value: codec::i64_from_u64(ptr_pos)?,
                tag: Tag::Bytes,
                full: false,
            },
            frozen,
        })
    }

    fn check_writable(&self) -> Result<()> {
        if !self.db.in_tx() {
            return Err(Error::ExpectedTxStart);
        }
        match self.epoch {
            None => return Err(Error::ExpectedTxStart),
            Some(e) if e != self.db.current_epoch() => return Err(Error::ExpiredTransaction),
            _ => {}
        }
        let frozen = self.db.tx_frozen();
        if let Some(f) = frozen {
            if (codec::u64_from_i64(self.slot.value)?) < f.max(self.frozen) {
                return Err(Error::FrozenBytes);
            }
        } else if (codec::u64_from_i64(self.slot.value)?) < self.frozen {
            return Err(Error::FrozenBytes);
        }
        Ok(())
    }

    pub const fn set_format_tag(&mut self, tag: [u8; 2]) {
        self.format_tag = Some(tag);
    }

    const fn set_format_tag_opt(&mut self, tag: Option<[u8; 2]>) {
        self.format_tag = tag;
    }

    /// Appends `data` to the value under construction.
    ///
    /// # Errors
    ///
    /// Fails when the writer is exhausted or I/O fails.
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.check_writable()?;
        let new_rel = codec::add(self.rel, codec::u64_from_usize(data.len())?)?;
        if new_rel > self.size {
            let end = self.db.core().borrow_mut().committed_len()?;
            if end != codec::add(self.start, self.size)? {
                return Err(Error::UnexpectedWriterPosition);
            }
        }
        self.db
            .core()
            .borrow_mut()
            .seek_to(codec::add(self.start, self.rel)?)?;
        self.db.core().borrow_mut().write(data)?;
        self.rel = new_rel;
        if self.rel > self.size {
            self.size = self.rel;
        }
        Ok(())
    }

    pub const fn seek_to(&mut self, pos: u64) {
        if pos <= self.size {
            self.rel = pos;
        }
    }

    /// Finishes the write, returning the completed slot.
    ///
    /// # Errors
    ///
    /// Propagates finalization and I/O failures.
    pub fn finish(&mut self) -> Result<Slot> {
        self.check_writable()?;
        if let Some(tag) = self.format_tag {
            self.slot.full = true;
            let mut core = self.db.core().borrow_mut();
            let end = core.committed_len()?;
            if codec::add(self.start, self.size)? != end {
                return Err(Error::UnexpectedWriterPosition);
            }
            core.seek_to(end)?;
            core.write(&tag)?;
        }
        {
            let mut core = self.db.core().borrow_mut();
            core.seek_to(self.ptr_pos)?;
            core.write(&self.size.to_be_bytes())?;
            if let Some(parent_pos) = self.parent_pos {
                // Point the parent slot at the array, like Go's `Finish`.
                // The slot was unresolved while streaming (its bytes were
                // still arriving); it resolves now.
                core.seek_to(parent_pos)?;
                core.write(&self.slot.to_bytes())?;
            }
        }
        if let Some(parent_pos) = self.parent_pos {
            self.slot_cell.set(SlotPointer {
                position: Some(parent_pos),
                slot: self.slot,
            });
        }
        Ok(self.slot)
    }
}

/// Stack-based collection iterator. Yields read-only cursors in storage
/// order; B-tree seeking lands in Task 5.
pub struct Iter<'db, C: Core, H: Hasher> {
    db: &'db Database<C, H>,
    stack: Vec<IterLevel>,
    size: u64,
    index: u64,
    tag: Tag,
}

struct IterLevel {
    position: u64,
    block: [Slot; 16],
    index: usize,
}

fn read_slot_block<C: Core, H: Hasher>(db: &Database<C, H>, pos: u64) -> Result<[Slot; 16]> {
    let mut core = db.core().borrow_mut();
    core.seek_to(pos)?;
    let mut bytes = vec![0u8; codec::usize_from_u64(crate::database::INDEX_BLOCK_LEN)?];
    core.read(&mut bytes)?;
    let mut block = [Slot {
        value: 0,
        tag: Tag::None,
        full: false,
    }; 16];
    for (s, chunk) in block
        .iter_mut()
        .zip(bytes.chunks_exact(codec::usize_from_u64(crate::codec::SLOT_LEN_U64)?))
    {
        let mut b = [0u8; SLOT_LEN];
        b.copy_from_slice(chunk);
        *s = Slot::from_bytes(b);
    }
    Ok(block)
}

impl<'db, C: Core, H: Hasher> Iter<'db, C, H> {
    fn new(db: &'db Database<C, H>, ptr: SlotPointer) -> Result<Self> {
        let mut it = Self {
            db,
            stack: Vec::new(),
            size: 0,
            index: 0,
            tag: ptr.slot.tag,
        };
        match ptr.slot.tag {
            Tag::None => Ok(it),
            Tag::ArrayList => {
                let (size, at) = Cursor::<C, H, ReadOnly>::read_array_header(
                    db,
                    codec::u64_from_i64(ptr.slot.value)?,
                )?;
                it.size = size;
                it.stack.push(IterLevel {
                    position: at,
                    block: read_slot_block(db, at)?,
                    index: 0,
                });
                Ok(it)
            }
            Tag::HashMap | Tag::HashSet => {
                let at = codec::u64_from_i64(ptr.slot.value)?;
                it.stack.push(IterLevel {
                    position: at,
                    block: read_slot_block(db, at)?,
                    index: 0,
                });
                Ok(it)
            }
            Tag::CountedHashMap | Tag::CountedHashSet => {
                let at = codec::add(codec::u64_from_i64(ptr.slot.value)?, 8)?;
                it.stack.push(IterLevel {
                    position: at,
                    block: read_slot_block(db, at)?,
                    index: 0,
                });
                Ok(it)
            }
            Tag::LinkedArrayList | Tag::SortedMap | Tag::SortedSet => {
                // Backed by a B-tree: walk from the root node's value/child
                // slots, skipping its kind+num header.
                let (size, root) = Cursor::<C, H, ReadOnly>::read_btree_header(
                    db,
                    codec::u64_from_i64(ptr.slot.value)?,
                )?;
                it.size = size;
                let block = if ptr.slot.tag == Tag::LinkedArrayList {
                    Self::btree_iter_block(db, root)?
                } else {
                    Self::sorted_iter_block(db, root)?
                };
                it.stack.push(IterLevel {
                    position: codec::add(root, crate::codec::NODE_HEADER_U64)?,
                    block,
                    index: 0,
                });
                Ok(it)
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    fn from_index(db: &'db Database<C, H>, ptr: SlotPointer, index: i64) -> Result<Self> {
        let mut it = Self {
            db,
            stack: Vec::new(),
            size: 0,
            index: 0,
            tag: ptr.slot.tag,
        };
        match ptr.slot.tag {
            Tag::None => Ok(it),
            Tag::ArrayList => {
                let (size, at) = Cursor::<C, H, ReadOnly>::read_array_header(
                    db,
                    codec::u64_from_i64(ptr.slot.value)?,
                )?;
                let size_signed = codec::i64_from_u64(size)?;
                let mut idx = index;
                if idx < 0 {
                    idx = codec::iadd(idx, size_signed)?;
                }
                if idx < 0 || idx >= size_signed {
                    return Ok(it);
                }
                let shift = if size <= 16 {
                    0
                } else {
                    crate::database::log16_floor(codec::sub_u64(size, 1)?)?
                };
                // Descend straight to the tier holding `idx`.
                let mut stack = Vec::new();
                let mut pos = at;
                let mut sh = shift;
                loop {
                    let block = read_slot_block(db, pos)?;
                    let rot = codec::mul(u64::from(sh), 4)?;
                    let i = codec::usize_from_u64((codec::u64_from_i64(idx)? >> rot) & 15)?;
                    stack.push(IterLevel {
                        position: pos,
                        block,
                        index: i,
                    });
                    if sh == 0 {
                        break;
                    }
                    pos = codec::u64_from_i64(codec::slot_at_arr(&block, i)?.value)?;
                    sh = sh.checked_sub(1).ok_or(Error::Corrupted)?;
                }
                it.size = size;
                it.index = codec::u64_from_i64(idx)?;
                it.stack = stack;
                Ok(it)
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// The iterable slots of a B-tree node: values/entries for leaves,
    /// children for branches.
    fn btree_iter_block(db: &'db Database<C, H>, node_ptr: u64) -> Result<[Slot; 16]> {
        let node = db.read_btree_node(node_ptr)?;
        if node.kind == crate::database::BTREE_KIND_LEAF {
            return Ok(node.values);
        }
        Ok(node.children)
    }

    /// Sorted leaves hold entries; same shape as B-tree values.
    fn sorted_iter_block(db: &'db Database<C, H>, node_ptr: u64) -> Result<[Slot; 16]> {
        let node = db.read_sorted_node(node_ptr)?;
        if node.kind == crate::database::BTREE_KIND_LEAF {
            return Ok(node.entries);
        }
        Ok(node.children)
    }

    fn resolve_rank(index: i64, size: u64) -> Option<u64> {
        let size_signed = codec::i64_from_u64(size).ok()?;
        let mut idx = index;
        if idx < 0 {
            idx = codec::iadd(idx, size_signed).ok()?;
        }
        if idx < 0 || idx >= size_signed {
            return None;
        }
        codec::u64_from_i64(idx).ok()
    }

    fn from_btree_index(db: &'db Database<C, H>, ptr: SlotPointer, index: i64) -> Result<Self> {
        let mut it = Self {
            db,
            stack: Vec::new(),
            size: 0,
            index: 0,
            tag: ptr.slot.tag,
        };
        match ptr.slot.tag {
            Tag::None => Ok(it),
            Tag::LinkedArrayList => {
                let (size, root) = Cursor::<C, H, ReadOnly>::read_btree_header(
                    db,
                    codec::u64_from_i64(ptr.slot.value)?,
                )?;
                let Some(idx) = Self::resolve_rank(index, size) else {
                    return Ok(it);
                };
                // Descend the count tree straight to the rank.
                let mut stack = Vec::new();
                let mut node_ptr = root;
                let mut rem = codec::i64_from_u64(idx)?;
                loop {
                    let node = db.read_btree_node(node_ptr)?;
                    let position = codec::add(node_ptr, crate::codec::NODE_HEADER_U64)?;
                    if node.kind == crate::database::BTREE_KIND_LEAF {
                        stack.push(IterLevel {
                            position,
                            block: node.values,
                            index: codec::usize_from_i64(rem)?,
                        });
                        break;
                    }
                    let mut i = 0usize;
                    while i.checked_add(1).ok_or(Error::Corrupted)? < node.num
                        && rem >= codec::count_at(&node.counts, i)?
                    {
                        rem = codec::isub(rem, codec::count_at(&node.counts, i)?)?;
                        i = i.checked_add(1).ok_or(Error::Corrupted)?;
                    }
                    stack.push(IterLevel {
                        position,
                        block: node.children,
                        index: i,
                    });
                    node_ptr = codec::child_ptr(&node.children, i)?;
                }
                it.size = size;
                it.index = idx;
                it.stack = stack;
                Ok(it)
            }
            Tag::SortedMap | Tag::SortedSet => {
                let (size, root) = Cursor::<C, H, ReadOnly>::read_btree_header(
                    db,
                    codec::u64_from_i64(ptr.slot.value)?,
                )?;
                let Some(idx) = Self::resolve_rank(index, size) else {
                    return Ok(it);
                };
                let mut stack = Vec::new();
                let mut node_ptr = root;
                let mut rem = codec::i64_from_u64(idx)?;
                loop {
                    let node = db.read_sorted_node(node_ptr)?;
                    let position = codec::add(node_ptr, crate::codec::NODE_HEADER_U64)?;
                    if node.kind == crate::database::BTREE_KIND_LEAF {
                        stack.push(IterLevel {
                            position,
                            block: node.entries,
                            index: codec::usize_from_i64(rem)?,
                        });
                        break;
                    }
                    let mut i = 0usize;
                    while i.checked_add(1).ok_or(Error::Corrupted)? < node.num
                        && rem >= codec::count_at(&node.counts, i)?
                    {
                        rem = codec::isub(rem, codec::count_at(&node.counts, i)?)?;
                        i = i.checked_add(1).ok_or(Error::Corrupted)?;
                    }
                    stack.push(IterLevel {
                        position,
                        block: node.children,
                        index: i,
                    });
                    node_ptr = codec::child_ptr(&node.children, i)?;
                }
                it.size = size;
                it.index = idx;
                it.stack = stack;
                Ok(it)
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    fn from_sorted_key(db: &'db Database<C, H>, ptr: SlotPointer, key: &[u8]) -> Result<Self> {
        let mut it = Self {
            db,
            stack: Vec::new(),
            size: 0,
            index: 0,
            tag: ptr.slot.tag,
        };
        match ptr.slot.tag {
            Tag::None => return Ok(it),
            Tag::SortedMap | Tag::SortedSet => {}
            _ => return Err(Error::UnexpectedTag),
        }
        let (size, root) =
            Cursor::<C, H, ReadOnly>::read_btree_header(db, codec::u64_from_i64(ptr.slot.value)?)?;
        let mut stack = Vec::new();
        let mut node_ptr = root;
        let mut before = 0u64;
        loop {
            let node = db.read_sorted_node(node_ptr)?;
            let position = codec::add(node_ptr, crate::codec::NODE_HEADER_U64)?;
            if node.kind == crate::database::BTREE_KIND_LEAF {
                let (li, _) = db.sorted_leaf_search(&node, key)?;
                before = codec::add(before, codec::u64_from_usize(li)?)?;
                stack.push(IterLevel {
                    position,
                    block: node.entries,
                    index: li,
                });
                break;
            }
            let i = db.sorted_branch_search(&node, key)?;
            for c in node.counts.get(..i).ok_or(Error::Corrupted)? {
                before = codec::add(before, codec::u64_from_i64(*c)?)?;
            }
            stack.push(IterLevel {
                position,
                block: node.children,
                index: i,
            });
            node_ptr = codec::child_ptr(&node.children, i)?;
        }
        it.size = size;
        it.index = before;
        it.stack = stack;
        Ok(it)
    }

    fn next_internal(&mut self, node_offset: u64) -> Result<Option<ReadCursor<'db, C, H>>> {
        while let Some(level) = self.stack.last_mut() {
            if level.index == level.block.len() {
                self.stack.pop();
                if let Some(prev) = self.stack.last_mut() {
                    prev.index = prev.index.checked_add(1).ok_or(Error::Corrupted)?;
                }
                continue;
            }
            let next = *level.block.get(level.index).ok_or(Error::Corrupted)?;
            if next.tag == Tag::Index {
                let child = codec::add(codec::u64_from_i64(next.value)?, node_offset)?;
                let block = read_slot_block(self.db, child)?;
                self.stack.push(IterLevel {
                    position: child,
                    block,
                    index: 0,
                });
                continue;
            }
            let i = level.index;
            level.index = level.index.checked_add(1).ok_or(Error::Corrupted)?;
            if next.tag == Tag::None && !next.full {
                continue;
            }
            let position = codec::add(
                level.position,
                codec::mul(codec::u64_from_usize(i)?, crate::codec::SLOT_LEN_U64)?,
            )?;
            return Ok(Some(ReadCursor {
                db: self.db,
                slot_ptr: Cell::new(SlotPointer {
                    position: Some(position),
                    slot: next,
                }),
                frozen_at: Cell::new(None),
                epoch: Cell::new(None),
                _mode: PhantomData,
            }));
        }
        Ok(None)
    }
}

impl<'db, C: Core, H: Hasher> Iterator for Iter<'db, C, H> {
    type Item = Result<ReadCursor<'db, C, H>>;

    fn next(&mut self) -> Option<Result<ReadCursor<'db, C, H>>> {
        match self.tag {
            Tag::None => None,
            Tag::ArrayList => {
                while self.index < self.size {
                    let next = self.index.checked_add(1)?;
                    self.index = next;
                    if let Some(outcome) = self.next_internal(0).transpose() {
                        return Some(outcome);
                    }
                }
                None
            }
            Tag::HashMap | Tag::HashSet | Tag::CountedHashMap | Tag::CountedHashSet => {
                match self.next_internal(0) {
                    Ok(Some(c)) => Some(Ok(c)),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                }
            }
            Tag::LinkedArrayList | Tag::SortedMap | Tag::SortedSet => {
                while self.index < self.size {
                    let next = self.index.checked_add(1)?;
                    self.index = next;
                    // B-tree nodes have a kind+num header before their slots.
                    if let Some(outcome) = self
                        .next_internal(
                            codec::u64_from_usize(crate::database::BTREE_NODE_HEADER).ok()?,
                        )
                        .transpose()
                    {
                        return Some(outcome);
                    }
                }
                None
            }
            _ => Some(Err(Error::UnexpectedTag)),
        }
    }
}

/// Runs the collection init matching a root tag over the scratch copy, so
/// committed headers/blocks are never rewritten in place. Mirrors Go's
/// `initPartFor` in the root-transaction branch (scalar roots need no init;
/// B-tree roots arrive in Task 5).
pub(crate) fn init_root_copy<C: Core, H: Hasher>(
    db: &Database<C, H>,
    ptr: SlotPointer,
    frozen: u64,
) -> Result<SlotPointer> {
    match ptr.slot.tag {
        Tag::ArrayList => Cursor::<C, H, ReadWrite>::apply_arraylist_init(db, ptr, frozen),
        Tag::HashMap => {
            Cursor::<C, H, ReadWrite>::apply_hashmap_init(db, ptr, false, false, frozen)
        }
        Tag::HashSet => Cursor::<C, H, ReadWrite>::apply_hashmap_init(db, ptr, false, true, frozen),
        Tag::CountedHashMap => {
            Cursor::<C, H, ReadWrite>::apply_hashmap_init(db, ptr, true, false, frozen)
        }
        Tag::CountedHashSet => {
            Cursor::<C, H, ReadWrite>::apply_hashmap_init(db, ptr, true, true, frozen)
        }
        Tag::LinkedArrayList => Cursor::<C, H, ReadWrite>::apply_linked_init(db, ptr, frozen),
        Tag::SortedMap => Cursor::<C, H, ReadWrite>::apply_sorted_init(db, ptr, false, frozen),
        Tag::SortedSet => Cursor::<C, H, ReadWrite>::apply_sorted_init(db, ptr, true, frozen),
        // `None`, scalars, bytes, and index/kv slots need no init.
        _ => Ok(ptr),
    }
}
