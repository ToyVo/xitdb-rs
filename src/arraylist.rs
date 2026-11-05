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

pub struct ArrayList<'db, C: Core, H: Hasher, Mode> {
    cursor: Cursor<'db, C, H, Mode>,
}

impl<'db, C: Core, H: Hasher, Mode> ArrayList<'db, C, H, Mode> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new(cursor: Cursor<'db, C, H, Mode>) -> Result<Self> {
        match cursor.slot()?.tag {
            Tag::None | Tag::ArrayList => Ok(ArrayList { cursor }),
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

    pub fn read_only(&self) -> ArrayList<'db, C, H, crate::cursor::ReadOnly> {
        ArrayList {
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
        self.cursor.read_path(&[PathPart::ArrayListGet { index }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn get_slot(&self, index: i64) -> Result<Slot> {
        self.cursor
            .read_path_slot(&[PathPart::ArrayListGet { index }])
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
        self.cursor.iter_from(index)
    }
}

impl<'db, C: Core, H: Hasher> ArrayList<'db, C, H, ReadWrite> {
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        let wc = cursor.write_path(&[PathPart::ArrayListInit])?;
        ArrayList::new(wc)
    }

    /// Runs a write path, then refreshes this cursor from its stable
    /// file position so later operations start from the collection
    /// (file positions never move within a transaction; the returned
    /// cursor points at the new element).
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    fn op(&self, parts: &[PathPart<'_, C, H>]) -> Result<WriteCursor<'db, C, H>> {
        let c = self.cursor.write_path(parts)?;
        if let Some(pos) = self.cursor.slot_ptr.get().position {
            let slot = self.cursor.db.read_slot_at(pos)?;
            slot.validate()?;
            self.cursor.slot_ptr.set(crate::cursor::SlotPointer {
                position: Some(pos),
                slot,
            });
        }
        Ok(c)
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put(&self, index: i64, data: Writeable) -> Result<()> {
        self.cursor
            .write_path(&[PathPart::ArrayListGet { index }, PathPart::Write(data)])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn put_cursor(&self, index: i64) -> Result<WriteCursor<'db, C, H>> {
        self.cursor.write_path(&[PathPart::ArrayListGet { index }])
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn append(&self, data: Writeable) -> Result<()> {
        self.cursor
            .write_path(&[PathPart::ArrayListAppend, PathPart::Write(data)])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn append_cursor(&self) -> Result<WriteCursor<'db, C, H>> {
        self.op(&[PathPart::ArrayListAppend])
    }

    /// Appends `prev` (usually the previous moment's slot) and runs `f`
    /// on the appended cursor in the same transaction.
    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn append_context<'a>(
        &self,
        data: Writeable,
        f: impl for<'c> FnOnce(&WriteCursor<'c, C, H>) -> Result<()> + 'a,
    ) -> Result<()> {
        self.cursor
            .write_path(&[
                PathPart::ArrayListAppend,
                PathPart::Write(data),
                PathPart::Context(ContextPart::new(f)),
            ])
            .map(|_| ())
    }

    /// # Errors
    ///
    /// Propagates cursor and I/O failures.
    pub fn slice(&self, size: i64) -> Result<()> {
        self.cursor
            .write_path(&[PathPart::ArrayListSlice { size }])
            .map(|_| ())
    }
}
