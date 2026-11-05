use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::core::Core;
use crate::cursor::{ContextPart, Cursor, Iter, PathPart, ReadCursor, WriteCursor, Writeable};
use crate::error::{Error, Result};
use crate::hasher::Hasher;
use crate::slot::Slot;
use crate::tag::Tag;

pub struct LinkedArrayList<'db, C: Core, H: Hasher, Mode> {
    cursor: Cursor<'db, C, H, Mode>,
}

impl<'db, C: Core, H: Hasher, Mode> LinkedArrayList<'db, C, H, Mode> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new(cursor: Cursor<'db, C, H, Mode>) -> Result<Self> {
        match cursor.slot()?.tag {
            Tag::None | Tag::LinkedArrayList => Ok(LinkedArrayList { cursor }),
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

    pub fn read_only(&self) -> LinkedArrayList<'db, C, H, crate::cursor::ReadOnly> {
        LinkedArrayList {
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
    pub fn get(&self, index: i64) -> Result<Option<ReadCursor<'db, C, H>>> {
        self.cursor
            .read_path(&[PathPart::LinkedArrayListGet { index }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_slot(&self, index: i64) -> Result<Slot> {
        self.cursor
            .read_path_slot(&[PathPart::LinkedArrayListGet { index }])
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
    pub fn iter_from(&self, index: i64) -> Result<Iter<'db, C, H>> {
        self.cursor.iter_from_index(index)
    }
}

pub struct WriteLinkedArrayList<'db, C: Core, H: Hasher> {
    read: LinkedArrayList<'db, C, H, crate::cursor::ReadWrite>,
    write_cursor: WriteCursor<'db, C, H>,
}

impl<'db, C: Core, H: Hasher> Deref for WriteLinkedArrayList<'db, C, H> {
    type Target = LinkedArrayList<'db, C, H, crate::cursor::ReadWrite>;
    fn deref(&self) -> &Self::Target {
        &self.read
    }
}

impl<'db, C: Core, H: Hasher> WriteLinkedArrayList<'db, C, H> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        let wc = cursor.write_path(&[PathPart::LinkedArrayListInit])?;
        let read = LinkedArrayList::new(wc.clone())?;
        Ok(WriteLinkedArrayList {
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
    pub fn put(&self, index: i64, data: Writeable) -> Result<()> {
        self.op(&[
            PathPart::LinkedArrayListGet { index },
            PathPart::Write(data),
        ])
        .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor(&self, index: i64) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::LinkedArrayListGet { index }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn append(&self, data: Writeable) -> Result<()> {
        self.op(&[PathPart::LinkedArrayListAppend, PathPart::Write(data)])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn append_cursor(&self) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::LinkedArrayListAppend])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn append_context<'a>(
        &self,
        data: Writeable,
        f: impl for<'c> FnOnce(&WriteCursor<'c, C, H>) -> Result<()> + 'a,
    ) -> Result<()> {
        self.op(&[
            PathPart::LinkedArrayListAppend,
            PathPart::Write(data),
            PathPart::Context(ContextPart::new(f)),
        ])
        .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn slice(&self, offset: i64, size: i64) -> Result<()> {
        self.op(&[PathPart::LinkedArrayListSlice { offset, size }])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn concat(&self, list: Slot) -> Result<()> {
        self.op(&[PathPart::LinkedArrayListConcat { list }])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn insert(&self, index: i64, data: Writeable) -> Result<()> {
        self.op(&[
            PathPart::LinkedArrayListInsert { index },
            PathPart::Write(data),
        ])
        .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn insert_cursor(&self, index: i64) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::LinkedArrayListInsert { index }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn remove(&self, index: i64) -> Result<()> {
        self.op(&[PathPart::LinkedArrayListRemove { index }])
            .map(|_| ())
    }
}

pub type ReadLinkedArrayList<'db, C, H> = LinkedArrayList<'db, C, H, crate::cursor::ReadOnly>;
