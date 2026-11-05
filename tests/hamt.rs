use xitdb::arraylist::{ArrayList, WriteArrayList};
use xitdb::core::MemoryCore;
use xitdb::counted_hashmap::WriteCountedHashMap;
use xitdb::counted_hashset::WriteCountedHashSet;
use xitdb::cursor::{SlotPointer, Writeable};
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::{Hasher, Sha1Hasher};
use xitdb::hashmap::{ReadHashMap, WriteHashMap};
use xitdb::hashset::WriteHashSet;
use xitdb::slot::Slot;
use xitdb::tag::Tag;

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

fn str_bytes(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn map_put_get_remove_strings() {
    let db = open_mem(101).unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        m.put("foo", uint(1))?;
        m.put("bar", uint(2))?;
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        let got = m.get("foo")?.map(|c| c.read_uint()).transpose()?.unwrap();
        assert_eq!(got, 1);
        assert!(m.get("missing")?.is_none());
        assert_eq!(m.get_slot("missing")?.tag, Tag::None);
        m.remove("bar")?;
        assert!(!m.remove("bar")?);
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        assert!(m.get("bar")?.is_none());
        assert_eq!(m.get("foo")?.unwrap().read_uint()?, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn map_bytes_and_hash_keys() {
    let db = open_mem(102).unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        m.put_by_bytes(b"k\xffkey", uint(7))?;
        m.put("s", uint(8))?;
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        assert_eq!(m.get_by_bytes(b"k\xffkey")?.unwrap().read_uint()?, 7);
        let hash = Sha1Hasher.digest(b"s");
        assert_eq!(m.get_by_hash(&hash)?.unwrap().read_uint()?, 8);
        assert!(m.remove_by_hash(&hash)?);
        assert!(m.get("s")?.is_none());
        Ok(())
    })
    .unwrap();
}

#[test]
fn map_key_value_pair_access() {
    let db = open_mem(103).unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        m.put("k", uint(9))?;
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        let kv = m.get_key_value_pair("k")?.unwrap();
        assert_eq!(kv.key.read_bytes_alloc()?, str_bytes("k"));
        assert_eq!(kv.value.read_uint()?, 9);
        assert!(m.get_key_value_pair("nope")?.is_none());
        Ok(())
    })
    .unwrap();
}

#[test]
fn counted_map_and_set_track_counts() {
    let db = open_mem(104).unwrap();
    db.run_context(|w| {
        let m = WriteCountedHashMap::new_write(w)?;
        m.put("a", uint(1))?;
        m.put("b", uint(2))?;
        m.put("a", uint(3))?;
        assert_eq!(m.count()?, 2);
        assert!(!m.remove("zz")?);
        assert_eq!(m.count()?, 2);
        assert!(m.remove("a")?);
        assert_eq!(m.count()?, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn counted_set_track_counts() {
    // Own database: reinterpreting a map root as a set would carry its
    // count, so the set flow starts from an empty root.
    let db = open_mem(114).unwrap();
    db.run_context(|w| {
        let s = WriteCountedHashSet::new_write(w)?;
        s.put("x")?;
        s.put("y")?;
        s.put("x")?;
        assert_eq!(s.count()?, 2);
        assert!(s.contains("x")?);
        assert!(!s.contains("q")?);
        assert!(s.remove("x")?);
        assert_eq!(s.count()?, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn hashset_put_get_remove() {
    let db = open_mem(105).unwrap();
    db.run_context(|w| {
        let s = WriteHashSet::new_write(w)?;
        s.put("m")?;
        assert!(s.contains("m")?);
        assert!(!s.contains("n")?);
        assert!(s.remove("m")?);
        assert!(!s.contains("m")?);
        Ok(())
    })
    .unwrap();
}

#[test]
fn array_append_get_iter() {
    let db = open_mem(106).unwrap();
    db.run_context(|w| {
        let a = WriteArrayList::new_write(w)?;
        for i in 0..100u64 {
            a.append(uint(i))?;
        }
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let a = WriteArrayList::new_write(w)?;
        assert_eq!(a.count()?, 100);
        assert_eq!(a.get(0)?.unwrap().read_uint()?, 0);
        assert_eq!(a.get(-1)?.unwrap().read_uint()?, 99);
        assert!(a.get(100)?.is_none());
        a.put(0, uint(1000))?;
        assert_eq!(a.get(0)?.unwrap().read_uint()?, 1000);
        let mut seen = Vec::new();
        for item in a.try_iter()? {
            seen.push(item?.read_uint()?);
        }
        assert_eq!(seen.len(), 100);
        assert_eq!(seen[99], 99);
        let mut tail = Vec::new();
        for item in a.iter_from(95)? {
            tail.push(item?.read_uint()?);
        }
        assert_eq!(tail, vec![95, 96, 97, 98, 99]);
        Ok(())
    })
    .unwrap();
}

#[test]
fn map_iter_yields_all_entries() {
    let db = open_mem(107).unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        for i in 0..5 {
            m.put(&format!("k{i}"), uint(u64::try_from(i).unwrap()))?;
        }
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteHashMap::new_write(w)?;
        let mut pairs = Vec::new();
        for item in m.try_iter()? {
            let kv = item?.read_key_value_pair()?;
            pairs.push((kv.key.read_bytes_alloc()?, kv.value.read_uint()?));
        }
        pairs.sort();
        assert_eq!(pairs.len(), 5);
        assert_eq!(pairs[0], (str_bytes("k0"), 0));
        Ok(())
    })
    .unwrap();
}

#[test]
fn writer_streams_large_value_then_reader_counts_lines() {
    let db = open_mem(108).unwrap();
    db.run_context(|w| {
        let a = WriteArrayList::new_write(w)?;
        let ac = a.append_cursor()?;
        let mut writer = ac.writer(&mut [0u8; 64])?;
        for _ in 0..50 {
            writer.write_bytes(b"hello\n")?;
        }
        writer.finish()?;
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let a = WriteArrayList::new_write(w)?;
        let cursor = a.get(0)?.unwrap();
        let mut reader = cursor.reader(&mut [0u8; 16])?;
        let all = reader.read_all()?;
        assert_eq!(all.len(), 50 * 6);
        let mut newlines = 0usize;
        for b in &all {
            if *b == b'\n' {
                newlines = newlines.checked_add(1).unwrap();
            }
        }
        assert_eq!(newlines, 50);
        Ok(())
    })
    .unwrap();
}

#[test]
fn clone_freeze_keeps_source_immutable() {
    let db = open_mem(109).unwrap();
    // tx1: history = [moment { fruits: [apple, pear, grape] }]
    db.run_context(|w| {
        let history = WriteArrayList::new_write(w)?;
        history.append_context(Writeable::None, |mc| {
            let m = WriteHashMap::new_write(mc)?;
            let fc = m.put_cursor("fruits")?;
            let fruits = WriteArrayList::new_write(&fc)?;
            fruits.append(Writeable::Bytes(str_bytes("apple")))?;
            fruits.append(Writeable::Bytes(str_bytes("pear")))?;
            fruits.append(Writeable::Bytes(str_bytes("grape")))?;
            Ok(())
        })
    })
    .unwrap();
    // tx2: moment = clone; food starts as fruits, frozen, then extended.
    db.run_context(|w| {
        let history = WriteArrayList::new_write(w)?;
        let prev = history.get_slot(-1)?;
        history.append_context(Writeable::Slot(prev), |mc| {
            let m = WriteHashMap::new_write(mc)?;
            let fruit_slot = m.get_slot("fruits")?;
            let food_cursor = m.put_cursor("food")?;
            food_cursor.write(Writeable::Slot(fruit_slot))?;
            food_cursor.freeze()?;
            let food = WriteArrayList::new_write(&food_cursor)?;
            food.append(Writeable::Bytes(str_bytes("eggs")))?;
            food.append(Writeable::Bytes(str_bytes("rice")))?;
            food.append(Writeable::Bytes(str_bytes("fish")))?;
            Ok(())
        })
    })
    .unwrap();
    db.run_context(|w| {
        let history = WriteArrayList::new_write(w)?;
        let mc = history.get(-1)?.unwrap();
        let m = ReadHashMap::new(mc)?;
        let food = ArrayList::new(m.get("food")?.unwrap())?;
        let fruits = ArrayList::new(m.get("fruits")?.unwrap())?;
        assert_eq!(food.count()?, 6);
        assert_eq!(fruits.count()?, 3);
        Ok(())
    })
    .unwrap();
}

#[test]
fn stale_write_cursor_is_expired() {
    let db = open_mem(110).unwrap();
    let w = db.root_cursor();
    let m = WriteHashMap::new_write(&w).unwrap();
    m.put("a", uint(1)).unwrap();
    // Non-root cursor from an older transaction.
    let stale = m.put_cursor("b").unwrap();
    // Advance the epoch with another committed write.
    m.put("c", uint(3)).unwrap();
    let err = db.run_context(|_| stale.write(uint(2))).err().unwrap();
    assert!(matches!(err, Error::ExpiredTransaction));
    let err = stale.write(uint(2)).err().unwrap();
    assert!(matches!(err, Error::ExpectedTxStart));
}

#[test]
fn init_existing_root_commits_nothing() {
    let db = open_mem(111).unwrap();
    // Empty to array-list commits the fresh header + block + trailer.
    db.run_context(|w| {
        let _a = WriteArrayList::new_write(w)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(db.committed_size().unwrap(), 256);
    // Re-initializing the existing root through a root cursor commits
    // nothing (Go's free re-init shortcut).
    let before = db.committed_size().unwrap();
    let w = db.root_cursor();
    let _a = WriteArrayList::new_write(&w).unwrap();
    assert_eq!(db.committed_size().unwrap(), before);
}

#[test]
fn slot_pointer_round_trip_helpers() {
    // Pins SlotPointer layout used across tasks.
    let sp = SlotPointer {
        position: Some(20),
        slot: Slot {
            value: 1,
            tag: Tag::Uint,
            full: false,
        },
    };
    assert_eq!(sp.position, Some(20));
    assert_eq!(sp.slot.value, 1);
}
