use std::cell::RefCell;

use xitdb::arraylist::ArrayList;
use xitdb::core::MemoryCore;
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::Sha1Hasher;
use xitdb::hashmap::{HashMap, ReadHashMap, WriteHashMap};
use xitdb::offset_map::{FileOffsetMap, MemoryOffsetMap};

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

const fn uint(n: u64) -> Writeable {
    Writeable::Uint(n)
}

fn build_history(db: &Database<MemoryCore, Sha1Hasher>, moments: u64) -> Result<(), Error> {
    for m in 0..moments {
        db.run_context(|w| {
            let history = ArrayList::new_write(w)?;
            let prev = history.get_slot(-1)?;
            history.append_context(Writeable::Slot(prev), |mc| {
                let moment = WriteHashMap::new_write(mc)?;
                moment.put("gen", uint(m))?;
                moment.put("shared", uint(99))?;
                Ok(())
            })
        })?;
    }
    Ok(())
}

#[test]
fn compact_keeps_latest_moment_only() {
    let db = open_mem(301).unwrap();
    build_history(&db, 3).unwrap();
    let mut offsets = MemoryOffsetMap::new();
    let compacted = db
        .compact(
            MemoryCore::new(),
            Sha1Hasher,
            target_opts(302),
            &mut offsets,
        )
        .unwrap();
    compacted
        .run_context(|w| {
            let history = ArrayList::new_write(w)?;
            assert_eq!(history.count()?, 1);
            let mc = history.get(0)?.unwrap();
            let moment = ReadHashMap::new(mc)?;
            assert_eq!(moment.get("gen")?.unwrap().read_uint()?, 2);
            assert_eq!(moment.get("shared")?.unwrap().read_uint()?, 99);
            Ok(())
        })
        .unwrap();
}

const fn target_opts(file_id: u64) -> InitOpts {
    InitOpts {
        hash_id: None,
        file_id: Some(file_id),
        fsync: false,
    }
}

#[test]
fn compact_preserves_shared_substructure() {
    let db = open_mem(303).unwrap();
    // Moment with fruits cloned to food (shared blocks), like the T4 test.
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        history.append_context(Writeable::None, |mc| {
            let m = WriteHashMap::new_write(mc)?;
            let fc = m.put_cursor("fruits")?;
            let fruits = ArrayList::new_write(&fc)?;
            fruits.append(Writeable::Bytes(b"apple".to_vec()))?;
            let food = m.put_cursor("food")?;
            food.write(Writeable::Slot(fruits.slot()?))?;
            Ok(())
        })
    })
    .unwrap();
    let mut offsets = MemoryOffsetMap::new();
    let compacted = db
        .compact(
            MemoryCore::new(),
            Sha1Hasher,
            target_opts(304),
            &mut offsets,
        )
        .unwrap();
    compacted
        .run_context(|w| {
            let history = ArrayList::new_write(w)?;
            let mc = history.get(0)?.unwrap();
            let m = ReadHashMap::new(mc)?;
            let fruits = ArrayList::new(m.get("fruits")?.unwrap())?;
            let food = ArrayList::new(m.get("food")?.unwrap())?;
            assert_eq!(fruits.count()?, 1);
            assert_eq!(food.count()?, 1);
            assert_eq!(food.get(0)?.unwrap().read_bytes_alloc()?, b"apple".to_vec());
            Ok(())
        })
        .unwrap();
}

#[test]
fn compact_handles_cycles() {
    let db = open_mem(305).unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        let c = m.put_cursor("loop")?;
        // Point "loop" back at the map itself.
        c.write(Writeable::Slot(m.slot()?))?;
        Ok(())
    })
    .unwrap();
    let mut offsets = MemoryOffsetMap::new();
    let compacted = db
        .compact(
            MemoryCore::new(),
            Sha1Hasher,
            target_opts(306),
            &mut offsets,
        )
        .unwrap();
    // Compare against the committed root (not a post-copy wrapper slot).
    let root = compacted.root_cursor().slot().unwrap();
    compacted
        .run_context(|w| {
            let m = HashMap::new(w.clone())?;
            let back = m.get_slot("loop")?;
            // Still self-referential after the copy.
            assert_eq!(back, root);
            Ok(())
        })
        .unwrap();
}

#[test]
fn offset_map_backends_agree() {
    let db = open_mem(307).unwrap();
    build_history(&db, 2).unwrap();
    let mut mem_offsets = MemoryOffsetMap::new();
    let a = db
        .compact(
            MemoryCore::new(),
            Sha1Hasher,
            target_opts(308),
            &mut mem_offsets,
        )
        .unwrap();
    // File-backed offsets inside one scratch transaction.
    let scratch = open_mem(309).unwrap();
    let cell: RefCell<Option<Database<MemoryCore, Sha1Hasher>>> = RefCell::new(None);
    scratch
        .run_context(|c| {
            let mut file_offsets = FileOffsetMap::new(c)?;
            // Same file id: identical inputs must compact to identical bytes.
            let b = db.compact(
                MemoryCore::new(),
                Sha1Hasher,
                target_opts(308),
                &mut file_offsets,
            )?;
            *cell.borrow_mut() = Some(b);
            Ok(())
        })
        .unwrap();
    let b = cell.into_inner().unwrap();
    // Identical inputs compact to identical bytes, except the trailer's
    // random commit id + checksum (last 16 bytes), like Go.
    let (a_bytes, b_bytes) = (full_bytes(&a).unwrap(), full_bytes(&b).unwrap());
    assert_eq!(a_bytes.len(), b_bytes.len());
    assert_eq!(
        &a_bytes[..a_bytes.len() - 16],
        &b_bytes[..b_bytes.len() - 16]
    );
    for target in [&a, &b] {
        target
            .run_context(|w| {
                let history = ArrayList::new_write(w)?;
                assert_eq!(history.count()?, 1);
                Ok(())
            })
            .unwrap();
    }
}

fn full_bytes(db: &Database<MemoryCore, Sha1Hasher>) -> Result<Vec<u8>, Error> {
    let end = db.committed_size()?;
    let mut buf = Vec::new();
    db.read_committed(0, end, &mut buf)?;
    Ok(buf)
}

#[test]
fn replication_incremental_and_full_copy() {
    let db = open_mem(311).unwrap();
    build_history(&db, 2).unwrap();
    let replica = open_mem(312).unwrap();
    // Fresh replica: full copy.
    let state = replica.replica_state().unwrap();
    let start = db.replication_start(state).unwrap();
    assert_eq!(start, 0);
    let end = db.committed_size().unwrap();
    let mut buf = Vec::new();
    db.read_committed(start, end, &mut buf).unwrap();
    replica
        .apply_replicated(start, &mut &buf[..], end - start)
        .unwrap();
    assert_eq!(replica.replica_state().unwrap().committed_size, end);
    // New transaction: incremental sync.
    build_history(&db, 1).unwrap();
    let state = replica.replica_state().unwrap();
    let start2 = db.replication_start(state).unwrap();
    assert_eq!(start2, end);
    let end2 = db.committed_size().unwrap();
    let mut buf2 = Vec::new();
    db.read_committed(start2, end2, &mut buf2).unwrap();
    replica
        .apply_replicated(start2, &mut &buf2[..], end2 - start2)
        .unwrap();
    assert_eq!(replica.replica_state().unwrap().committed_size, end2);
    // Same history visible on both.
    for target in [&db, &replica] {
        target
            .run_context(|w| {
                let history = ArrayList::new_write(w)?;
                assert_eq!(history.count()?, 3);
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn interrupted_full_copy_recovers_on_next_sync() {
    let db = open_mem(313).unwrap();
    build_history(&db, 1).unwrap();
    let replica = open_mem(314).unwrap();
    let end = db.committed_size().unwrap();
    let mut buf = Vec::new();
    db.read_committed(0, end, &mut buf).unwrap();
    // Interrupt: apply only the first half of a full copy. The apply
    // itself reports the short read (like Go's ReadFull).
    let half = buf.len() / 2;
    let mut partial = &buf[..half];
    let _ = replica.apply_replicated(0, &mut partial, u64::try_from(half).unwrap());
    // The replica is unusable until the next sync succeeds.
    assert!(replica.committed_size().is_err());
    replica
        .apply_replicated(0, &mut &buf[..], u64::try_from(buf.len()).unwrap())
        .unwrap();
    replica
        .run_context(|w| {
            let history = ArrayList::new_write(w)?;
            assert_eq!(history.count()?, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn compact_rejects_nonempty_target_and_handles_empty_source() {
    let db = open_mem(315).unwrap();
    // Empty source compacts to a fresh target.
    let mut offsets = MemoryOffsetMap::new();
    let fresh = db
        .compact(
            MemoryCore::new(),
            Sha1Hasher,
            target_opts(316),
            &mut offsets,
        )
        .unwrap();
    fresh
        .run_context(|w| {
            let history = ArrayList::new_write(w)?;
            assert_eq!(history.count()?, 0);
            Ok(())
        })
        .unwrap();
    // Non-empty target is rejected: pre-fill a core, then compact into it.
    build_history(&db, 1).unwrap();
    let victim = open_mem(400).unwrap();
    build_history(&victim, 1).unwrap();
    let mut offsets = MemoryOffsetMap::new();
    let err = db
        .compact(
            victim.into_core(),
            Sha1Hasher,
            target_opts(317),
            &mut offsets,
        )
        .err()
        .unwrap();
    assert!(matches!(err, Error::TargetNotEmpty));
}
