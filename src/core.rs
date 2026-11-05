use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{Error, Result};

/// Storage backend. Mirrors Go's `Core` interface with Rust naming:
/// `Length`→`committed_len`, `SetLength`→`truncate`, `Sync`→`sync`.
///
/// # Errors
///
/// Every method propagates I/O failures; short reads report
/// [`Error::Truncated`] instead.
pub trait Core {
    /// Fills `buf` from the current position.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures; short reads report [`Error::Truncated`].
    fn read(&mut self, buf: &mut [u8]) -> Result<()>;
    /// Writes `buf` at the current position.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn write(&mut self, buf: &[u8]) -> Result<()>;
    /// Returns the committed length.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn committed_len(&mut self) -> Result<u64>;
    /// Moves the position to `pos`.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn seek_to(&mut self, pos: u64) -> Result<()>;
    /// Returns the current position.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn position(&mut self) -> Result<u64>;
    /// Truncates the backend to `len`.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn truncate(&mut self, len: u64) -> Result<()>;
    /// Flushes buffered data.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn flush(&mut self) -> Result<()>;
    /// Persists committed data.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn sync(&mut self) -> Result<()>;
}

fn truncated<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(e)
        }
    })
}

pub struct MemoryCore {
    data: Vec<u8>,
    pos: u64,
}

impl MemoryCore {
    /// Empty in-memory backend at position zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: Vec::new(),
            pos: 0,
        }
    }
}

impl Default for MemoryCore {
    fn default() -> Self {
        Self::new()
    }
}

impl Core for MemoryCore {
    fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        let start = crate::codec::usize_from_u64(self.pos)?;
        let end = start.checked_add(buf.len()).ok_or(Error::Truncated)?;
        if end > self.data.len() {
            return Err(Error::Truncated);
        }
        buf.copy_from_slice(self.data.get(start..end).ok_or(Error::Truncated)?);
        self.pos = crate::codec::add(self.pos, crate::codec::u64_from_usize(buf.len())?)?;
        Ok(())
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        let start = crate::codec::usize_from_u64(self.pos)?;
        let end = start.checked_add(buf.len()).ok_or(Error::Corrupted)?;
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data
            .get_mut(start..end)
            .ok_or(Error::Corrupted)?
            .copy_from_slice(buf);
        self.pos = crate::codec::add(self.pos, crate::codec::u64_from_usize(buf.len())?)?;
        Ok(())
    }

    fn committed_len(&mut self) -> Result<u64> {
        crate::codec::u64_from_usize(self.data.len())
    }

    fn seek_to(&mut self, pos: u64) -> Result<()> {
        self.pos = pos;
        Ok(())
    }

    fn position(&mut self) -> Result<u64> {
        Ok(self.pos)
    }

    fn truncate(&mut self, len: u64) -> Result<()> {
        self.data.truncate(crate::codec::usize_from_u64(len)?);
        self.pos = self.pos.min(len);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        Ok(())
    }
}

pub struct FileCore {
    file: File,
    fsync: bool,
}

impl FileCore {
    /// File backend at the file's current position.
    #[must_use]
    pub const fn new(file: File, fsync: bool) -> Self {
        Self { file, fsync }
    }
}

impl Core for FileCore {
    fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        truncated(self.file.read_exact(buf))
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.file.write_all(buf)?;
        Ok(())
    }

    fn committed_len(&mut self) -> Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn seek_to(&mut self, pos: u64) -> Result<()> {
        self.file.seek(SeekFrom::Start(pos))?;
        Ok(())
    }

    fn position(&mut self) -> Result<u64> {
        Ok(self.file.stream_position()?)
    }

    fn truncate(&mut self, len: u64) -> Result<()> {
        self.file.set_len(len)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        if self.fsync {
            self.file.sync_all()?;
        }
        Ok(())
    }
}

/// File backend with an append buffer. Flushes on commit for performance;
/// callers must not interleave reads with unflushed state except through
/// this type (reads/seeks flush pending appends first).
pub struct BufferedFileCore {
    file: File,
    /// Pending append bytes; valid only at `flush_point..`.
    buf: Vec<u8>,
    flush_point: u64,
    pos: u64,
    fsync: bool,
}

impl BufferedFileCore {
    /// Opens the buffered backend, positioned at the file's end.
    ///
    /// # Errors
    ///
    /// Propagates metadata failures.
    pub fn new(file: File, fsync: bool) -> Result<Self> {
        let flush_point = file.metadata().map_or(0, |m| m.len());
        // Start positioned at the end like a fresh writer; `Database::open`
        // seeks explicitly before reading.
        Ok(Self {
            file,
            buf: Vec::new(),
            flush_point,
            pos: flush_point,
            fsync,
        })
    }

    /// Writes pending appends at the flush point.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    fn flush_buf(&mut self) -> Result<()> {
        if !self.buf.is_empty() {
            self.file.seek(SeekFrom::Start(self.flush_point))?;
            self.file.write_all(&self.buf)?;
            self.flush_point = crate::codec::add(
                self.flush_point,
                crate::codec::u64_from_usize(self.buf.len())?,
            )?;
            self.buf.clear();
        }
        Ok(())
    }
}

impl Core for BufferedFileCore {
    fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        // Serve from pending appends when fully inside them.
        if self.pos >= self.flush_point {
            let off = crate::codec::sub_u64(self.pos, self.flush_point)?;
            let end = crate::codec::add(off, crate::codec::u64_from_usize(buf.len())?)?;
            if end <= crate::codec::u64_from_usize(self.buf.len())? {
                buf.copy_from_slice(crate::codec::sub(
                    &self.buf,
                    crate::codec::usize_from_u64(off)?,
                    buf.len(),
                )?);
                self.pos = crate::codec::add(self.pos, crate::codec::u64_from_usize(buf.len())?)?;
                return Ok(());
            }
        }
        self.flush_buf()?;
        self.file.seek(SeekFrom::Start(self.pos))?;
        truncated(self.file.read_exact(buf))?;
        self.pos = crate::codec::add(self.pos, crate::codec::u64_from_usize(buf.len())?)?;
        Ok(())
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        // Appends extend the buffer; overwrites flush first then go direct.
        let buffered_end = crate::codec::add(
            self.flush_point,
            crate::codec::u64_from_usize(self.buf.len())?,
        )?;
        if self.pos == buffered_end {
            self.buf.extend_from_slice(buf);
            self.pos = crate::codec::add(buffered_end, crate::codec::u64_from_usize(buf.len())?)?;
            return Ok(());
        }
        self.flush_buf()?;
        self.file.seek(SeekFrom::Start(self.pos))?;
        self.file.write_all(buf)?;
        self.pos = crate::codec::add(self.pos, crate::codec::u64_from_usize(buf.len())?)?;
        self.flush_point = self.flush_point.max(self.pos);
        Ok(())
    }

    fn committed_len(&mut self) -> Result<u64> {
        let file_len = self.file.metadata()?.len();
        let buffered = crate::codec::add(
            self.flush_point,
            crate::codec::u64_from_usize(self.buf.len())?,
        )?;
        Ok(file_len.max(buffered))
    }

    fn seek_to(&mut self, pos: u64) -> Result<()> {
        self.pos = pos;
        Ok(())
    }

    fn position(&mut self) -> Result<u64> {
        Ok(self.pos)
    }

    fn truncate(&mut self, len: u64) -> Result<()> {
        self.buf.clear();
        self.file.set_len(len)?;
        self.flush_point = len.min(self.flush_point);
        self.pos = self.pos.min(len);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.flush_buf()?;
        self.file.flush()?;
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        self.flush()?;
        if self.fsync {
            self.file.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{Core, MemoryCore};

    #[test]
    fn memory_core_read_write_seek_len_round_trip() {
        let mut c = MemoryCore::new();
        c.write(b"hello").unwrap();
        assert_eq!(c.committed_len().unwrap(), 5);
        c.seek_to(0).unwrap();
        assert_eq!(c.position().unwrap(), 0);
        let mut buf = [0u8; 5];
        c.read(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        assert_eq!(c.position().unwrap(), 5);
        c.truncate(2).unwrap();
        assert_eq!(c.committed_len().unwrap(), 2);
        c.seek_to(0).unwrap();
        let mut buf2 = [0u8; 2];
        c.read(&mut buf2).unwrap();
        assert_eq!(&buf2, b"he");
    }
}
