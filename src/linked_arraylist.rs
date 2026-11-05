use std::cell::Cell;
use std::marker::PhantomData;

use crate::core::Core;
use crate::cursor::{
    ContextPart, Cursor, Iter, PathPart, ReadCursor, ReadWrite, WriteCursor, Writeable,
};
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

impl<'db, C: Core, H: Hasher> LinkedArrayList<'db, C, H, ReadWrite> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        let wc = cursor.write_path(&[PathPart::LinkedArrayListInit])?;
        LinkedArrayList::new(wc)
    }

    /// Runs a write path, then refreshes this cursor from its stable
    /// file position so later operations start from the collection
    /// (file positions never move within a transaction).
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    fn op(&self, parts: &[PathPart<'_, C, H>]) -> Result<WriteCursor<'db, C, H>> {
        let c = self.cursor.write_path(parts)?;
        if let Some(pos) = self.cursor.slot_ptr.get().position {
            let slot = self.cursor.db.read_slot_at(pos)?;
            slot.validate()?;
            let sp = crate::cursor::SlotPointer {
                position: Some(pos),
                slot,
            };
            self.cursor.slot_ptr.set(sp);
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
