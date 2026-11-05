use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::codec;
use crate::core::Core;
use crate::cursor::{
    Cursor, Iter, KeyValuePair, PathPart, ReadCursor, SortedMapTarget, WriteCursor, Writeable,
};
use crate::error::{Error, Result};
use crate::hasher::Hasher;
use crate::slot::Slot;
use crate::tag::Tag;

pub struct SortedMap<'db, C: Core, H: Hasher, Mode> {
    cursor: Cursor<'db, C, H, Mode>,
}

impl<'db, C: Core, H: Hasher, Mode> SortedMap<'db, C, H, Mode> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new(cursor: Cursor<'db, C, H, Mode>) -> Result<Self> {
        match cursor.slot()?.tag {
            Tag::None | Tag::SortedMap | Tag::SortedSet => Ok(SortedMap { cursor }),
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn slot(&self) -> Result<Slot> {
        self.cursor.slot()
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn count(&self) -> Result<u64> {
        self.cursor.count()
    }

    pub fn read_only(&self) -> SortedMap<'db, C, H, crate::cursor::ReadOnly> {
        SortedMap {
            cursor: Cursor {
                db: self.cursor.db,
                slot_ptr: Cell::new(self.cursor.slot_ptr.get()),
                frozen_at: Cell::new(self.cursor.frozen_at.get()),
                epoch: Cell::new(None),
                _mode: PhantomData,
            },
        }
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get(&self, key: &str) -> Result<Option<ReadCursor<'db, C, H>>> {
        self.get_by_bytes(key.as_bytes())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_by_bytes(&self, key: &[u8]) -> Result<Option<ReadCursor<'db, C, H>>> {
        self.cursor.read_path(&[PathPart::SortedMapGet {
            target: SortedMapTarget::Value(key.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_slot(&self, key: &str) -> Result<Slot> {
        self.get_slot_by_bytes(key.as_bytes())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_slot_by_bytes(&self, key: &[u8]) -> Result<Slot> {
        self.cursor.read_path_slot(&[PathPart::SortedMapGet {
            target: SortedMapTarget::Value(key.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_key_value_pair(&self, key: &str) -> Result<Option<KeyValuePair<'db, C, H>>> {
        self.get_key_value_pair_by_bytes(key.as_bytes())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_key_value_pair_by_bytes(
        &self,
        key: &[u8],
    ) -> Result<Option<KeyValuePair<'db, C, H>>> {
        match self.cursor.read_path(&[PathPart::SortedMapGet {
            target: SortedMapTarget::KeyValue(key.to_vec()),
        }])? {
            None => Ok(None),
            Some(c) => Ok(Some(c.read_key_value_pair()?)),
        }
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_index_key_value_pair(&self, index: i64) -> Result<Option<KeyValuePair<'db, C, H>>> {
        match self
            .cursor
            .read_path(&[PathPart::SortedMapGetIndex { index }])?
        {
            None => Ok(None),
            Some(c) => Ok(Some(c.read_key_value_pair()?)),
        }
    }

    /// Number of keys strictly less than `key`.
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn rank(&self, key: &str) -> Result<i64> {
        self.rank_by_bytes(key.as_bytes())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn rank_by_bytes(&self, key: &[u8]) -> Result<i64> {
        let root = self.sorted_root()?;
        self.cursor.db.sorted_rank(root, key)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    fn sorted_root(&self) -> Result<u64> {
        let slot = self.cursor.slot()?;
        match slot.tag {
            Tag::None => Err(Error::KeyNotFound),
            Tag::SortedMap | Tag::SortedSet => {
                let (size, root) = self.header_root()?;
                let _ = size;
                Ok(root)
            }
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    /// Reads the collection header (size, root) for this cursor.
    ///
    /// # Errors
    ///
    /// Fails on negative header values or I/O failures.
    fn header_root(&self) -> Result<(u64, u64)> {
        let slot = self.cursor.slot()?;
        let mut core = self.cursor.db.core().borrow_mut();
        core.seek_to(codec::u64_from_i64(slot.value)?)?;
        let mut b = [0u8; 16];
        core.read(&mut b)?;
        let size = codec::i64_be(&b, 0)?;
        let root = codec::i64_be(&b, 8)?;
        if size < 0 || root < 0 {
            return Err(Error::ExpectedUnsignedLong);
        }
        Ok((codec::u64_from_i64(size)?, codec::u64_from_i64(root)?))
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn try_iter(&self) -> Result<Iter<'db, C, H>> {
        self.cursor.try_iter()
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn iter_from(&self, key: &[u8]) -> Result<Iter<'db, C, H>> {
        self.cursor.iter_from_key(key)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn iter_from_index(&self, index: i64) -> Result<Iter<'db, C, H>> {
        self.cursor.iter_from_index(index)
    }
}

pub struct WriteSortedMap<'db, C: Core, H: Hasher> {
    read: SortedMap<'db, C, H, crate::cursor::ReadWrite>,
    write_cursor: WriteCursor<'db, C, H>,
}

impl<'db, C: Core, H: Hasher> Deref for WriteSortedMap<'db, C, H> {
    type Target = SortedMap<'db, C, H, crate::cursor::ReadWrite>;
    fn deref(&self) -> &Self::Target {
        &self.read
    }
}

impl<'db, C: Core, H: Hasher> WriteSortedMap<'db, C, H> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        let wc = cursor.write_path(&[PathPart::SortedMapInit { set: false }])?;
        let read = SortedMap::new(wc.clone())?;
        Ok(WriteSortedMap {
            read,
            write_cursor: wc,
        })
    }

    /// Runs a write path, then refreshes both halves from their stable
    /// file position. Mirrors Go, where the read/write halves share one
    /// `SlotPointer` refreshed by `reloadSlot` on every write: file positions
    /// never move, so a re-read observes earlier writes in the same
    /// transaction (including frozen copies).
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    fn op(&self, parts: &[PathPart<'_, C, H>]) -> Result<WriteCursor<'db, C, H>> {
        let c = self.write_cursor.write_path(parts)?;
        if let Some(pos) = self.write_cursor.slot_ptr.get().position {
            let slot = self.write_cursor.db.read_slot_at(pos)?;
            slot.validate()?;
            let sp = crate::cursor::SlotPointer {
                position: Some(pos),
                slot,
            };
            self.write_cursor.slot_ptr.set(sp);
            self.read.cursor.slot_ptr.set(sp);
        }
        Ok(c)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put(&self, key: &str, data: Writeable) -> Result<()> {
        self.put_by_bytes(key.as_bytes(), data)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_by_bytes(&self, key: &[u8], data: Writeable) -> Result<()> {
        self.write_cursor
            .write_path(&[
                PathPart::SortedMapGet {
                    target: SortedMapTarget::Value(key.to_vec()),
                },
                PathPart::Write(data),
            ])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor(&self, key: &str) -> Result<WriteCursor<'db, C, H>> {
        self.put_cursor_by_bytes(key.as_bytes())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor_by_bytes(&self, key: &[u8]) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::SortedMapGet {
            target: SortedMapTarget::Value(key.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn remove(&self, key: &str) -> Result<bool> {
        self.remove_by_bytes(key.as_bytes())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn remove_by_bytes(&self, key: &[u8]) -> Result<bool> {
        match self
            .write_cursor
            .write_path(&[PathPart::SortedMapRemove { key: key.to_vec() }])
        {
            Ok(_) => Ok(true),
            Err(Error::KeyNotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

pub type ReadSortedMap<'db, C, H> = SortedMap<'db, C, H, crate::cursor::ReadOnly>;
