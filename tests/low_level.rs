//! Low-level API scenarios ported from xitdb-go's `low_level_test.go`:
//! frozen/expired writers at slot granularity, panic rollback, raw core
//! semantics, header validation, and sized B-tree stress.

use xitdb::arraylist::ArrayList;
use xitdb::core::{Core, MemoryCore};
use xitdb::cursor::{ContextPart, PathPart, Writeable};
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::{Hasher, Sha1Hasher, bytes_to_id};
use xitdb::hashmap::HashMap;
use xitdb::header::{HEADER_LEN, Header};
use xitdb::linked_arraylist::WriteLinkedArrayList;

fn open_mem(file_id: u64) -> Result<Database<MemoryCore, Sha1Hasher>, Error> {
    Database::open(
        MemoryCore::new(),
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(file_id),
            fsync: false,
        },
    )
}

fn mem_db_open(bytes: &[u8]) -> Result<Database<MemoryCore, Sha1Hasher>, Error> {
    let mut core = MemoryCore::new();
    core.write(bytes)?;
    Database::open(
        core,
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(1),
            fsync: false,
        },
    )
}

fn full_bytes(db: &Database<MemoryCore, Sha1Hasher>) -> Result<Vec<u8>, Error> {
    let end = db.committed_size()?;
    let mut buf = Vec::new();
    db.read_committed(0, end, &mut buf)?;
    Ok(buf)
}

const fn int(n: i64) -> Writeable {
    Writeable::Int(n)
}

#[test]
fn frozen_map_writes_split_old_and_new() {
    let db = open_mem(601).unwrap();
    // History first: appends need an array root.
    let history = ArrayList::new_write(&db.root_cursor()).unwrap();
    history.append(Writeable::None).unwrap();
    db.root_cursor()
        .write_path(&[
            PathPart::ArrayListAppend,
            PathPart::HashMapInit {
                counted: false,
                set: false,
            },
            PathPart::Context(ContextPart::new(|cursor| {
                let m = HashMap::new_write(cursor)?;
                m.put("v", int(1))?;
                let child_cursor = m.put_cursor("child")?;
                let child = HashMap::new_write(&child_cursor)?;
                child.put("v", int(2))?;
                // Pre-freeze read cursors stay valid: old bytes never move.
                let v_before = m.get("v")?.unwrap();
                let child_before = m.get("child")?.unwrap();
                db.freeze()?;
                // Writes below the freeze point are rejected...
                let err = child.put("v", int(999)).err().unwrap();
                assert!(matches!(err, Error::FrozenSlot));
                // ...while the map itself stays writable (copy-on-write).
                m.put("v", int(3))?;
                let child_cursor = m.put_cursor("child")?;
                let child = HashMap::new_write(&child_cursor)?;
                child.put("v", int(4))?;
                // Frozen reads see the old values.
                assert_eq!(v_before.read_int()?, 1);
                let frozen_child = HashMap::new(child_before)?;
                assert_eq!(frozen_child.get("v")?.unwrap().read_int()?, 2);
                // Fresh reads see the new ones.
                assert_eq!(m.get("v")?.unwrap().read_int()?, 3);
                Ok(())
            })),
        ])
        .unwrap();
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let mc = history.get(1)?.unwrap();
        let m = HashMap::new(mc)?;
        assert_eq!(m.get("v")?.unwrap().read_int()?, 3);
        Ok(())
    })
    .unwrap();
}

#[test]
fn frozen_byte_writer_rejects_post_freeze_writes() {
    let db = open_mem(602).unwrap();
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        history.append_context(Writeable::None, |cursor| {
            let mut w1 = cursor.writer(&mut [0u8; 16])?;
            w1.write_bytes(&[0u8; 16])?;
            w1.finish()?;
            // A writer over the old bytes, created before the freeze.
            let mut stale = cursor.writer(&mut [0u8; 16])?;
            db.freeze()?;
            assert!(matches!(
                stale.write_bytes(&[99]).err().unwrap(),
                Error::FrozenBytes
            ));
            assert!(matches!(stale.finish().err().unwrap(), Error::FrozenBytes));
            // A writer over fresh bytes works fine.
            let mut w2 = cursor.writer(&mut [0u8; 16])?;
            w2.write_bytes(&[1u8; 16])?;
            w2.finish()?;
            assert_eq!(cursor.read_bytes_alloc()?, vec![1u8; 16]);
            Ok(())
        })
    })
    .unwrap();
    // Rollback retains frozen bytes: freeze, then fail, then read back.
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let mc = history.put_cursor(0)?;
        db.freeze()?;
        let m = HashMap::new_write(&mc)?;
        m.put("zz", int(1))?;
        Err(Error::ExpectedTxStart)
    })
    .err()
    .unwrap();
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        assert_eq!(history.count()?, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn panic_in_transaction_rolls_back() {
    let db = open_mem(603).unwrap();
    db.run_context(|w| {
        let m = HashMap::new_write(w)?;
        m.put("v", int(1))?;
        Ok(())
    })
    .unwrap();
    let before = db.committed_size().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        db.run_context(|w| {
            let m = HashMap::new_write(w)?;
            m.put("v", int(999))?;
            panic!("boom");
        })
    }));
    assert!(result.is_err());
    assert_eq!(db.committed_size().unwrap(), before);
    db.run_context(|w| {
        let m = HashMap::new_write(w)?;
        assert_eq!(m.get("v")?.unwrap().read_int()?, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn expired_cursors_reject_across_transactions() {
    // Mirrors Go's TestExpiredWriters via direct root calls (each its own
    // transaction): a non-root cursor from an older transaction expires.
    // NOTE: escaping a *Writer* across transactions is borrow-unreachable
    // in safe Rust (Go escapes via GC), so writer expiry is covered by
    // construction (epoch check) plus the frozen/expected-tx paths; the
    // reachable cursor-expiry path is asserted here.
    let db = open_mem(604).unwrap();
    let w = db.root_cursor();
    let m = HashMap::new_write(&w).unwrap();
    m.put("v", int(1)).unwrap();
    // Non-root cursor from an older transaction.
    let stale = m.put_cursor("w").unwrap();
    // Advance the epoch past it with another committed write.
    m.put("v", int(2)).unwrap();
    let err = db.run_context(|_| stale.write(int(3))).err().unwrap();
    assert!(matches!(err, Error::ExpiredTransaction));
    // Top-level writes inside a root transaction stay nested-rejected.
    let rc = db.root_cursor();
    let before = db.committed_size().unwrap();
    let err = db.run_context(|_| rc.write(int(999))).err().unwrap();
    assert!(matches!(err, Error::NestedTopLevelWrite));
    assert_eq!(db.committed_size().unwrap(), before);
}

#[test]
fn memory_core_read_write_truncate_position() {
    let mut core = MemoryCore::new();
    // Reading past the end truncates the operation, not the buffer.
    let mut buf = [0u8; 4];
    assert!(matches!(
        core.read(&mut buf).err().unwrap(),
        Error::Truncated
    ));
    core.write(b"hello world").unwrap();
    assert_eq!(core.committed_len().unwrap(), 11);
    assert_eq!(core.position().unwrap(), 11);
    core.seek_to(6).unwrap();
    let mut word = [0u8; 5];
    core.read(&mut word).unwrap();
    assert_eq!(&word, b"world");
    // Overwriting the middle leaves the length alone.
    core.seek_to(0).unwrap();
    core.write(b"HELLO").unwrap();
    assert_eq!(core.committed_len().unwrap(), 11);
    // Truncation clamps a position past the end.
    core.seek_to(11).unwrap();
    core.truncate(5).unwrap();
    assert_eq!(core.position().unwrap(), 5);
    assert_eq!(core.committed_len().unwrap(), 5);
    core.seek_to(0).unwrap();
    let mut back = [0u8; 5];
    core.read(&mut back).unwrap();
    assert_eq!(&back, b"HELLO");
}

#[test]
fn open_validates_version_and_keeps_hash_id() {
    let db = open_mem(605).unwrap();
    let image = full_bytes(&db).unwrap();
    // Tampered version is rejected.
    let mut bad = image.clone();
    bad[4..6].copy_from_slice(&99u16.to_be_bytes());
    assert!(matches!(
        mem_db_open(&bad).err().unwrap(),
        Error::InvalidDatabase | Error::InvalidVersion
    ));
    // Truncated images are rejected, not panicked on.
    assert!(mem_db_open(&image[..10]).is_err());
    // The hash id round-trips through the header (informational on open).
    let header = Header::from_bytes(image[..HEADER_LEN].try_into().unwrap());
    assert_eq!(header.magic, xitdb::header::MAGIC);
    assert_eq!(header.version, xitdb::header::VERSION);
    assert_eq!(usize::from(header.hash_size), Sha1Hasher.digest_len());
    assert_eq!(header.hash_id, bytes_to_id(*b"sha1"));
}

#[test]
fn linked_sized_stress() {
    // Slice/concat/insert/remove across tier boundaries (0,1,5,16,17,40).
    for size in [0u64, 1, 5, 16, 17, 40] {
        let db = open_mem(size.checked_add(606).unwrap()).unwrap();
        db.run_context(|w| {
            let a = WriteLinkedArrayList::new_write(w)?;
            for i in 0..size {
                a.append(Writeable::Uint(i))?;
            }
            assert_eq!(a.count()?, size);
            if size > 0 {
                // Slice to the middle third, then concat it with itself.
                let keep = size / 2 + 1;
                let off = size / 4;
                let keep = keep.min(size - off);
                a.slice(i64::try_from(off).unwrap(), i64::try_from(keep).unwrap())?;
                assert_eq!(a.count()?, keep);
                let snapshot: Vec<u64> =
                    a.try_iter()?
                        .map(|c| c?.read_uint())
                        .collect::<Result<Vec<u64>, _>>()?;
                let other = a.slot()?;
                a.concat(other)?;
                assert_eq!(a.count()?, keep * 2);
                // Insert at front, then remove it again.
                a.insert(0, Writeable::Uint(1000))?;
                assert_eq!(a.get(0)?.unwrap().read_uint()?, 1000);
                a.remove(0)?;
                let back: Vec<u64> = a
                    .try_iter()?
                    .map(|c| c?.read_uint())
                    .collect::<Result<Vec<u64>, _>>()?;
                let mut expect = snapshot.clone();
                expect.extend(snapshot.iter());
                assert_eq!(back, expect);
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn same_transaction_reads_observe_writes() {
    let db = open_mem(920).unwrap();
    db.run_context(|w| {
        let list = ArrayList::new_write(w)?;
        list.append(Writeable::Uint(1))?;
        list.append(Writeable::Uint(2))?;
        // same-transaction read through the same wrapper, incl. frozen copy
        assert_eq!(list.get(0)?.ok_or(Error::KeyNotFound)?.read_uint()?, 1);
        assert_eq!(list.get(1)?.ok_or(Error::KeyNotFound)?.read_uint()?, 2);
        Ok(())
    })
    .unwrap();
}
