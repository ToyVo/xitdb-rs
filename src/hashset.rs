use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::core::Core;
use crate::cursor::{
    ContextPart, Cursor, GetTarget, Iter, PathPart, ReadCursor, WriteCursor, Writeable,
};
use crate::error::{Error, Result};
use crate::hasher::Hasher;
use crate::slot::Slot;
use crate::tag::Tag;

pub struct HashSet<'db, C: Core, H: Hasher, Mode> {
    cursor: Cursor<'db, C, H, Mode>,
}

impl<'db, C: Core, H: Hasher, Mode> HashSet<'db, C, H, Mode> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new(cursor: Cursor<'db, C, H, Mode>) -> Result<Self> {
        match cursor.slot()?.tag {
            Tag::None | Tag::HashMap | Tag::HashSet => Ok(HashSet { cursor }),
            _ => Err(Error::UnexpectedTag),
        }
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn slot(&self) -> Result<Slot> {
        self.cursor.slot()
    }

    pub fn read_only(&self) -> HashSet<'db, C, H, crate::cursor::ReadOnly> {
        HashSet {
            cursor: Cursor {
                db: self.cursor.db,
                slot_ptr: Cell::new(self.cursor.slot_ptr.get()),
                frozen_at: Cell::new(self.cursor.frozen_at.get()),
                epoch: Cell::new(None),
                _mode: PhantomData,
            },
        }
    }

    fn digest(&self, key: &[u8]) -> Vec<u8> {
        self.cursor.db.hasher().digest(key)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn contains(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn contains_by_bytes(&self, key: &[u8]) -> Result<bool> {
        Ok(self.get_by_bytes(key)?.is_some())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn contains_by_hash(&self, hash: &[u8]) -> Result<bool> {
        Ok(self.get_by_hash(hash)?.is_some())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get(&self, key: &str) -> Result<Option<ReadCursor<'db, C, H>>> {
        let hash = self.digest(key.as_bytes());
        self.get_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_by_bytes(&self, key: &[u8]) -> Result<Option<ReadCursor<'db, C, H>>> {
        let hash = self.digest(key);
        self.get_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_by_hash(&self, hash: &[u8]) -> Result<Option<ReadCursor<'db, C, H>>> {
        self.cursor.read_path(&[PathPart::HashMapGet {
            target: GetTarget::Key(hash.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn try_iter(&self) -> Result<Iter<'db, C, H>> {
        self.cursor.try_iter()
    }
}

pub struct WriteHashSet<'db, C: Core, H: Hasher> {
    read: HashSet<'db, C, H, crate::cursor::ReadWrite>,
    write_cursor: WriteCursor<'db, C, H>,
}

impl<'db, C: Core, H: Hasher> Deref for WriteHashSet<'db, C, H> {
    type Target = HashSet<'db, C, H, crate::cursor::ReadWrite>;
    fn deref(&self) -> &Self::Target {
        &self.read
    }
}

impl<'db, C: Core, H: Hasher> WriteHashSet<'db, C, H> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        let wc = cursor.write_path(&[PathPart::HashMapInit {
            counted: false,
            set: true,
        }])?;
        let read = HashSet::new(wc.clone())?;
        Ok(WriteHashSet {
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

    fn digest(&self, key: &[u8]) -> Vec<u8> {
        self.read.digest(key)
    }

    /// Sets only the key; the value always stays empty.
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put(&self, key: &str) -> Result<()> {
        let hash = self.digest(key.as_bytes());
        self.put_by_hash(&hash, Writeable::Bytes(key.as_bytes().to_vec()))
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_by_bytes(&self, key: &[u8]) -> Result<()> {
        let hash = self.digest(key);
        self.put_by_hash(&hash, Writeable::Bytes(key.to_vec()))
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_by_hash(&self, hash: &[u8], key: Writeable) -> Result<()> {
        let target = GetTarget::Key(hash.to_vec());
        self.write_cursor
            .write_path(&[
                PathPart::HashMapGet { target },
                PathPart::Context(ContextPart::new(move |c| c.write_if_empty(key))),
            ])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor(&self, key: &str) -> Result<WriteCursor<'db, C, H>> {
        let hash = self.digest(key.as_bytes());
        self.put_by_hash(&hash, Writeable::Bytes(key.as_bytes().to_vec()))?;
        self.put_cursor_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor_by_hash(&self, hash: &[u8]) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::HashMapGet {
            target: GetTarget::Key(hash.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn remove(&self, key: &str) -> Result<bool> {
        let hash = self.digest(key.as_bytes());
        self.remove_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn remove_by_hash(&self, hash: &[u8]) -> Result<bool> {
        match self.op(&[PathPart::HashMapRemove {
            hash: hash.to_vec(),
        }]) {
            Ok(_) => Ok(true),
            Err(Error::KeyNotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

pub type ReadHashSet<'db, C, H> = HashSet<'db, C, H, crate::cursor::ReadOnly>;
