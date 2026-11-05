//! High-level API scenarios ported from xitdb-go's `high_level_test.go`:
//! backend parity, non-array roots with the append-only invariant,
//! the sorted user-directory narrative, threading, seeked iteration,
//! and replica edge cases.

use std::fs::OpenOptions;
use std::sync::{Arc, Barrier};

use xitdb::arraylist::ArrayList;
use xitdb::core::{BufferedFileCore, FileCore, MemoryCore};
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts, ROOT_HEADER_END, ROOT_HEADER_START};
use xitdb::error::Error;
use xitdb::hasher::{Hasher, Sha1Hasher};
use xitdb::hashmap::{HashMap, WriteHashMap};
use xitdb::linked_arraylist::{LinkedArrayList, WriteLinkedArrayList};
use xitdb::sorted_map::{SortedMap, WriteSortedMap};

const fn mem_opts(file_id: u64) -> InitOpts {
    InitOpts {
        hash_id: None,
        file_id: Some(file_id),
        fsync: false,
    }
}

fn open_mem(file_id: u64) -> Result<Database<MemoryCore, Sha1Hasher>, Error> {
    Database::open(MemoryCore::new(), Sha1Hasher, mem_opts(file_id))
}

const fn uint(n: u64) -> Writeable {
    Writeable::Uint(n)
}

fn tmp_file(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("xitdb-hl-{}-{name}", std::process::id()))
}

fn open_file_core(path: &std::path::Path) -> Result<FileCore, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(Error::Io)?;
    Ok(FileCore::new(file, false))
}

fn open_buffered_core(path: &std::path::Path) -> Result<BufferedFileCore, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(Error::Io)?;
    BufferedFileCore::new(file, false)
}

fn replicate(
    source: &Database<MemoryCore, Sha1Hasher>,
    replica: &Database<MemoryCore, Sha1Hasher>,
) -> Result<(), Error> {
    let state = replica.replica_state()?;
    let start = source.replication_start(state)?;
    let end = source.committed_size()?;
    let mut buf = Vec::new();
    source.read_committed(start, end, &mut buf)?;
    let len = end.checked_sub(start).ok_or(Error::Corrupted)?;
    replica.apply_replicated(start, &mut &buf[..], len)?;
    Ok(())
}

fn committed_bytes(db: &Database<MemoryCore, Sha1Hasher>) -> Result<Vec<u8>, Error> {
    let end = db.committed_size()?;
    let mut buf = Vec::new();
    db.read_committed(0, end, &mut buf)?;
    Ok(buf)
}

/// After every write: committed bytes only grow (outside the root
/// header), then the replica catches up byte-identical.
fn expect_append_only_and_replicate(
    source: &Database<MemoryCore, Sha1Hasher>,
    replica: &Database<MemoryCore, Sha1Hasher>,
    committed: &mut Vec<u8>,
) -> Result<(), Error> {
    let current = committed_bytes(source)?;
    if current.len() < committed.len() {
        return Err(Error::Corrupted);
    }
    let header_len = usize::try_from(ROOT_HEADER_START).map_err(|_| Error::Corrupted)?;
    let header_end = usize::try_from(ROOT_HEADER_END).map_err(|_| Error::Corrupted)?;
    if committed.get(..header_len) != current.get(..header_len)
        || committed.get(header_end..) != current.get(header_end..committed.len())
    {
        return Err(Error::Corrupted);
    }
    *committed = current;
    replicate(source, replica)?;
    if *committed != committed_bytes(replica)? {
        return Err(Error::Corrupted);
    }
    Ok(())
}

#[test]
fn file_backends_agree_with_memory() {
    // The same scenario on file and buffered-file backends yields the
    // same reads as memory.
    let path = tmp_file("roundtrip.db");
    let fdb: Database<FileCore, Sha1Hasher> =
        Database::open(open_file_core(&path).unwrap(), Sha1Hasher, mem_opts(503)).unwrap();
    fdb.run_context(|w| {
        let h = ArrayList::new_write(w)?;
        h.append_context(Writeable::None, |mc| {
            let m = WriteHashMap::new_write(mc)?;
            m.put("k", uint(7))?;
            Ok(())
        })
    })
    .unwrap();
    fdb.run_context(|w| {
        let h = ArrayList::new_write(w)?;
        let mc = h.get(0).unwrap().unwrap();
        let m = HashMap::new(mc).unwrap();
        assert_eq!(m.get("k").unwrap().unwrap().read_uint().unwrap(), 7);
        Ok(())
    })
    .unwrap();
    drop(fdb);

    let bpath = tmp_file("buffered.db");
    let bdb: Database<BufferedFileCore, Sha1Hasher> = Database::open(
        open_buffered_core(&bpath).unwrap(),
        Sha1Hasher,
        mem_opts(504),
    )
    .unwrap();
    bdb.run_context(|w| {
        let h = ArrayList::new_write(w)?;
        h.append_context(Writeable::None, |mc| {
            let m = WriteHashMap::new_write(mc)?;
            m.put("k", uint(7))?;
            Ok(())
        })
    })
    .unwrap();
    bdb.run_context(|w| {
        let h = ArrayList::new_write(w)?;
        let mc = h.get(0).unwrap().unwrap();
        let m = HashMap::new(mc).unwrap();
        assert_eq!(m.get("k").unwrap().unwrap().read_uint().unwrap(), 7);
        Ok(())
    })
    .unwrap();
    drop(bdb);
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&bpath).ok();
}

#[test]
fn hash_map_root_with_append_only_replication() {
    let source = open_mem(504).unwrap();
    let replica = open_mem(505).unwrap();
    let m = WriteHashMap::new_write(&source.root_cursor()).unwrap();

    // Initializing an initialized root starts no transaction.
    let size_after_init = source.committed_size().unwrap();
    let _ = WriteHashMap::new_write(&source.root_cursor()).unwrap();
    assert_eq!(size_after_init, source.committed_size().unwrap());

    // Nor can the root be initialized as another type.
    let err = ArrayList::new_write(&source.root_cursor()).err().unwrap();
    assert!(matches!(err, Error::UnexpectedTag));
    assert_eq!(size_after_init, source.committed_size().unwrap());

    let mut committed = committed_bytes(&source).unwrap();
    for i in 0..100u64 {
        m.put(&format!("key-{i}"), uint(i)).unwrap();
        if i == 50 {
            assert!(m.remove("key-25").unwrap());
        }
        expect_append_only_and_replicate(&source, &replica, &mut committed).unwrap();
    }

    // Keys are written in the same transaction as the lookup.
    let hash = Sha1Hasher.digest(b"key-only-hashed");
    m.put_by_hash(&hash, uint(1)).unwrap();
    m.put_key(
        "key-only-hashed",
        Writeable::Bytes(b"key-only-hashed".to_vec()),
    )
    .unwrap();
    let kv = m.get_key_value_pair("key-only-hashed").unwrap().unwrap();
    assert_eq!(kv.key.read_bytes_alloc().unwrap(), b"key-only-hashed");
    assert_eq!(kv.value.read_uint().unwrap(), 1);
    expect_append_only_and_replicate(&source, &replica, &mut committed).unwrap();
}

#[test]
fn sorted_user_directory_paginate_and_autocomplete() {
    // The README narrative at scale: primary HashMap by id, SortedMap
    // index by username, pages plus type-ahead.
    let db = open_mem(506).unwrap();
    let users = [
        ("user000000000001", "dave", "Dave Smith"),
        ("user000000000002", "alice", "Alice Jones"),
        ("user000000000003", "carol", "Carol White"),
        ("user000000000004", "dan", "Dan Brown"),
        ("user000000000005", "bob", "Bob Lee"),
        ("user000000000006", "eve", "Eve Adams"),
    ];
    db.run_context(|w| {
        let moment = WriteHashMap::new_write(w)?;
        let idc = moment.put_cursor("id->user")?;
        let id_to_user = WriteHashMap::new_write(&idc)?;
        let uc = moment.put_cursor("username->id")?;
        let username_to_id = WriteSortedMap::new_write(&uc)?;
        for (id, username, name) in users {
            let user_cursor = id_to_user.put_cursor(id)?;
            let user = WriteHashMap::new_write(&user_cursor)?;
            user.put("username", Writeable::Bytes(username.as_bytes().to_vec()))?;
            user.put("name", Writeable::Bytes(name.as_bytes().to_vec()))?;
            username_to_id.put(username, Writeable::Bytes(id.as_bytes().to_vec()))?;
        }
        Ok(())
    })
    .unwrap();

    db.run_context(|w| {
        let moment = WriteHashMap::new_write(w)?;
        let id_to_user = HashMap::new(moment.get("id->user")?.unwrap())?;
        let username_to_id = SortedMap::new(moment.get("username->id")?.unwrap())?;
        // Page [2,4): carol, dan.
        let mut page = Vec::new();
        for item in username_to_id.iter_from_index(2)? {
            if page.len() == 2 {
                break;
            }
            let kv = item?.read_key_value_pair()?;
            let mut id = [0u8; 16];
            let raw = kv.value.read_bytes_alloc()?;
            id.copy_from_slice(&raw);
            let user = HashMap::new(id_to_user.get(std::str::from_utf8(&id).unwrap())?.unwrap())?;
            page.push(user.get("name")?.unwrap().read_bytes_alloc()?);
        }
        assert_eq!(page, vec![b"Carol White".to_vec(), b"Dan Brown".to_vec()]);
        // Autocomplete "da".
        let mut hits = Vec::new();
        for item in username_to_id.iter_from(b"da")? {
            let kv = item?.read_key_value_pair()?;
            let username = kv.key.read_bytes_alloc()?;
            if !username.starts_with(b"da") {
                break;
            }
            hits.push(username);
        }
        assert_eq!(hits, vec![b"dan".to_vec(), b"dave".to_vec()]);
        Ok(())
    })
    .unwrap();
}

#[test]
fn multithreaded_reads_alongside_writes() {
    // Each thread opens its own handle; reads never block writes.
    let path = tmp_file("threads.db");
    {
        let db: Database<FileCore, Sha1Hasher> =
            Database::open(open_file_core(&path).unwrap(), Sha1Hasher, mem_opts(507)).unwrap();
        db.run_context(|w| {
            let h = ArrayList::new_write(w)?;
            h.append_context(Writeable::None, |mc| {
                let m = WriteHashMap::new_write(mc)?;
                m.put("foo", Writeable::Bytes(b"foo".to_vec()))?;
                Ok(())
            })
        })
        .unwrap();
    }
    let barrier = Arc::new(Barrier::new(3));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                let file = OpenOptions::new().read(true).open(&path).unwrap();
                let db: Database<FileCore, Sha1Hasher> =
                    Database::open(FileCore::new(file, false), Sha1Hasher, mem_opts(507)).unwrap();
                barrier.wait();
                let root = db.root_cursor();
                let history = ArrayList::new_write(&root).unwrap();
                let mc = history.get(0).unwrap().unwrap();
                let m = HashMap::new(mc).unwrap();
                let v = m.get("foo").unwrap().unwrap().read_bytes_alloc().unwrap();
                assert_eq!(v, b"foo");
            })
        })
        .collect();
    barrier.wait();
    for r in readers {
        r.join().unwrap();
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn all_from_seeks_past_256_element_tiers() {
    // 200 items force a second radix tier (16^2) and B-tree depth.
    let db = open_mem(508).unwrap();
    db.run_context(|w| {
        let h = ArrayList::new_write(w)?;
        h.append_context(Writeable::None, |mc| {
            let m = WriteHashMap::new_write(mc)?;
            let lc = m.put_cursor("list")?;
            let list = ArrayList::new_write(&lc)?;
            for i in 0..200u64 {
                list.append(uint(i))?;
            }
            let kc = m.put_cursor("linked")?;
            let linked = WriteLinkedArrayList::new_write(&kc)?;
            for i in 0..200u64 {
                linked.append(uint(i))?;
            }
            Ok(())
        })
    })
    .unwrap();
    db.run_context(|w| {
        let h = ArrayList::new_write(w)?;
        let mc = h.get(0).unwrap().unwrap();
        let m = HashMap::new(mc).unwrap();
        let list = ArrayList::new(m.get("list")?.unwrap())?;
        let mut first = None;
        let mut n = 0;
        for item in list.iter_from(150)? {
            let v = item?.read_uint()?;
            if first.is_none() {
                first = Some(v);
            }
            n += 1;
        }
        assert_eq!(first, Some(150));
        assert_eq!(n, 50);
        let linked = LinkedArrayList::new(m.get("linked")?.unwrap())?;
        let mut first = None;
        let mut n = 0;
        for item in linked.iter_from(150)? {
            let v = item?.read_uint()?;
            if first.is_none() {
                first = Some(v);
            }
            n += 1;
        }
        assert_eq!(first, Some(150));
        assert_eq!(n, 50);
        Ok(())
    })
    .unwrap();
}

#[test]
fn replica_of_other_file_is_replaced_not_extended() {
    let source = open_mem(1).unwrap();
    let other = open_mem(2).unwrap();
    let replica = open_mem(509).unwrap();
    // Same writes, same layout shape.
    source.root_cursor().write(uint(1)).unwrap();
    other.root_cursor().write(uint(1)).unwrap();
    replicate(&source, &replica).unwrap();
    // Other diverges: same position, different file id in the checksum.
    other.root_cursor().write(uint(2)).unwrap();
    let state = replica.replica_state().unwrap();
    assert_eq!(other.replication_start(state).unwrap(), 0);
    // The source still matches the replica at its committed size.
    assert_eq!(
        source.replication_start(state).unwrap(),
        state.committed_size
    );
    // Appended foreign bytes are rejected; the replica is untouched.
    let start = replica.committed_size().unwrap();
    let end = other.committed_size().unwrap();
    let mut buf = Vec::new();
    other.read_committed(start, end, &mut buf).unwrap();
    let err = replica
        .apply_replicated(start, &mut &buf[..], end - start)
        .err()
        .unwrap();
    assert!(matches!(err, Error::InvalidDatabase));
    assert_eq!(replica.committed_size().unwrap(), start);
}
