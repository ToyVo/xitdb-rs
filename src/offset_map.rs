use crate::core::Core;
use crate::cursor::{ReadWrite, WriteCursor, Writeable};
use crate::error::Result;
use crate::hasher::Hasher;
use crate::hashmap::HashMap;

/// Maps source offsets to target offsets during compaction.
///
/// Mappings must remain available until reset so shared references and
/// cycles point at the same copy. Each map needs exclusive use during one
/// compaction.
pub trait OffsetMap {
    /// Clears all mappings.
    ///
    /// # Errors
    ///
    /// Propagates backend failures.
    fn reset(&mut self) -> Result<()>;
    /// Looks up the target for `src`, if mapped.
    ///
    /// # Errors
    ///
    /// Propagates backend failures.
    fn get(&mut self, src: u64) -> Result<Option<u64>>;
    /// Records that `src` was copied to `dst`.
    ///
    /// # Errors
    ///
    /// Propagates backend failures.
    fn put(&mut self, src: u64, dst: u64) -> Result<()>;
}

/// In-memory offsets. Grows with the number of live objects copied, so
/// huge databases should use [`FileOffsetMap`] instead.
pub struct MemoryOffsetMap {
    map: std::collections::HashMap<u64, u64>,
}

impl MemoryOffsetMap {
    /// Empty in-memory offset map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: std::collections::HashMap::new(),
        }
    }
}

impl Default for MemoryOffsetMap {
    fn default() -> Self {
        Self::new()
    }
}

impl OffsetMap for MemoryOffsetMap {
    fn reset(&mut self) -> Result<()> {
        self.map.clear();
        Ok(())
    }

    fn get(&mut self, src: u64) -> Result<Option<u64>> {
        Ok(self.map.get(&src).copied())
    }

    fn put(&mut self, src: u64, dst: u64) -> Result<()> {
        self.map.insert(src, dst);
        Ok(())
    }
}

/// Offsets in a hash map inside a separate scratch database.
///
/// Typically a temporary file, without retaining offsets in memory. Create
/// it with the cursor a scratch `run_context` provides, and run the
/// compaction inside that same transaction so the offsets are not each
/// committed separately. The scratch database's hash size must be at
/// least 8 bytes.
pub struct FileOffsetMap<'db, C: Core, H: Hasher> {
    map: HashMap<'db, C, H, ReadWrite>,
}

impl<'db, C: Core, H: Hasher> FileOffsetMap<'db, C, H> {
    /// Opens the map on the scratch cursor's hash map.
    ///
    /// # Errors
    ///
    /// Propagates hash-map initialization failures.
    pub fn new(cursor: &WriteCursor<'db, C, H>) -> Result<Self> {
        Ok(FileOffsetMap {
            map: HashMap::new_write(cursor)?,
        })
    }

    /// Encodes the offset directly rather than hashing it. The key is
    /// zero-padded to the hash size with the offset big-endian in the
    /// last 8 bytes.
    /// Encodes `offset` as a hash-sized key.
    ///
    /// # Errors
    ///
    /// Fails when the hash size is under 8 bytes.
    fn key(&self, offset: u64) -> Result<Vec<u8>> {
        let size = self.map.hash_size();
        let start = size
            .checked_sub(8)
            .ok_or(crate::error::Error::InvalidHashSize)?;
        let mut key = vec![0u8; size];
        key.get_mut(start..)
            .ok_or(crate::error::Error::Corrupted)?
            .copy_from_slice(&offset.to_be_bytes());
        Ok(key)
    }
}

impl<C: Core, H: Hasher> OffsetMap for FileOffsetMap<'_, C, H> {
    fn reset(&mut self) -> Result<()> {
        let wc = self.map.write_cursor().clone();
        wc.write(Writeable::None)?;
        self.map = HashMap::new_write(&wc)?;
        Ok(())
    }

    fn get(&mut self, src: u64) -> Result<Option<u64>> {
        match self.map.get_by_hash(&self.key(src)?)? {
            None => Ok(None),
            Some(c) => Ok(Some(c.read_uint()?)),
        }
    }

    fn put(&mut self, src: u64, dst: u64) -> Result<()> {
        self.map
            .put_by_hash(&self.key(src)?, Writeable::Uint(dst))?;
        Ok(())
    }
}
