use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::core::Core;
use crate::cursor::{
    ContextPart, Cursor, GetTarget, Iter, KeyValuePair, PathPart, ReadCursor, WriteCursor,
    Writeable,
};
use crate::error::{Error, Result};
use crate::hasher::Hasher;
use crate::slot::Slot;
use crate::tag::Tag;

pub struct CountedHashMap<'db, C: Core, H: Hasher, Mode> {
    cursor: Cursor<'db, C, H, Mode>,
}

impl<'db, C: Core, H: Hasher, Mode> CountedHashMap<'db, C, H, Mode> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new(cursor: Cursor<'db, C, H, Mode>) -> Result<Self> {
        match cursor.slot()?.tag {
            Tag::None | Tag::CountedHashMap | Tag::CountedHashSet => Ok(CountedHashMap { cursor }),
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

    pub fn read_only(&self) -> CountedHashMap<'db, C, H, crate::cursor::ReadOnly> {
        CountedHashMap {
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
            target: GetTarget::Value(hash.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_slot(&self, key: &str) -> Result<Slot> {
        let hash = self.digest(key.as_bytes());
        self.get_slot_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_slot_by_hash(&self, hash: &[u8]) -> Result<Slot> {
        self.cursor.read_path_slot(&[PathPart::HashMapGet {
            target: GetTarget::Value(hash.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_key_value_pair(&self, key: &str) -> Result<Option<KeyValuePair<'db, C, H>>> {
        let hash = self.digest(key.as_bytes());
        self.get_key_value_pair_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_key_value_pair_by_hash(
        &self,
        hash: &[u8],
    ) -> Result<Option<KeyValuePair<'db, C, H>>> {
        match self.cursor.read_path(&[PathPart::HashMapGet {
            target: GetTarget::KeyValue(hash.to_vec()),
        }])? {
            None => Ok(None),
            Some(c) => Ok(Some(c.read_key_value_pair()?)),
        }
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn try_iter(&self) -> Result<Iter<'db, C, H>> {
        self.cursor.try_iter()
    }
}

pub struct WriteCountedHashMap<'db, C: Core, H: Hasher> {
    read: CountedHashMap<'db, C, H, crate::cursor::ReadWrite>,
    write_cursor: WriteCursor<'db, C, H>,
}

impl<'db, C: Core, H: Hasher> Deref for WriteCountedHashMap<'db, C, H> {
    type Target = CountedHashMap<'db, C, H, crate::cursor::ReadWrite>;
    fn deref(&self) -> &Self::Target {
        &self.read
    }
}

impl<'db, C: Core, H: Hasher> WriteCountedHashMap<'db, C, H> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        let wc = cursor.write_path(&[PathPart::HashMapInit {
            counted: true,
            set: false,
        }])?;
        let read = CountedHashMap::new(wc.clone())?;
        Ok(WriteCountedHashMap {
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

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put(&self, key: &str, data: Writeable) -> Result<()> {
        let hash = self.digest(key.as_bytes());
        self.put_key_value(&hash, key.as_bytes().to_vec(), data)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_by_hash(&self, hash: &[u8], data: Writeable) -> Result<()> {
        self.write_cursor
            .write_path(&[
                PathPart::HashMapGet {
                    target: GetTarget::Value(hash.to_vec()),
                },
                PathPart::Write(data),
            ])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    fn put_key_value(&self, hash: &[u8], key: Vec<u8>, data: Writeable) -> Result<()> {
        let target = GetTarget::KeyValue(hash.to_vec());
        self.write_cursor
            .write_path(&[
                PathPart::HashMapGet { target },
                PathPart::Context(ContextPart::new(move |c| {
                    let kv = c.write_key_value_pair()?;
                    kv.key.write_if_empty(Writeable::Bytes(key))?;
                    kv.value.write(data)?;
                    Ok(())
                })),
            ])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor(&self, key: &str) -> Result<WriteCursor<'db, C, H>> {
        let hash = self.digest(key.as_bytes());
        self.put_key_by_hash(&hash, Writeable::Bytes(key.as_bytes().to_vec()))?;
        self.put_cursor_by_hash(&hash)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor_by_hash(&self, hash: &[u8]) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::HashMapGet {
            target: GetTarget::Value(hash.to_vec()),
        }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_key_by_hash(&self, hash: &[u8], data: Writeable) -> Result<()> {
        let target = GetTarget::Key(hash.to_vec());
        self.write_cursor
            .write_path(&[
                PathPart::HashMapGet { target },
                PathPart::Context(ContextPart::new(move |c| c.write_if_empty(data))),
            ])
            .map(|_| ())
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

pub type ReadCountedHashMap<'db, C, H> = CountedHashMap<'db, C, H, crate::cursor::ReadOnly>;
