use xitdb::core::MemoryCore;
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::Sha1Hasher;
use xitdb::linked_arraylist::{LinkedArrayList, WriteLinkedArrayList};
use xitdb::sorted_map::WriteSortedMap;
use xitdb::sorted_set::WriteSortedSet;

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

fn bytes(s: &str) -> Writeable {
    Writeable::Bytes(s.as_bytes().to_vec())
}

#[test]
fn linked_insert_at_zero_preserves_order() {
    let db = open_mem(201).unwrap();
    db.run_context(|w| {
        let a = WriteLinkedArrayList::new_write(w)?;
        a.append(bytes("e"))?;
        for word in ["d", "c", "b", "a"] {
            a.insert(0, bytes(word))?;
        }
        assert_eq!(a.count()?, 5);
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let a = WriteLinkedArrayList::new_write(w)?;
        let mut got = Vec::new();
        for item in a.try_iter()? {
            got.push(item?.read_bytes_alloc()?);
        }
        assert_eq!(
            got,
            ["a", "b", "c", "d", "e"]
                .iter()
                .map(|s| s.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
        assert_eq!(a.get(-1)?.unwrap().read_bytes_alloc()?, b"e".to_vec());
        Ok(())
    })
    .unwrap();
}

#[test]
fn linked_slice_and_concat_round_trip() {
    use xitdb::hashmap::HashMap;
    let db = open_mem(202).unwrap();
    db.run_context(|w| {
        let m = HashMap::new_write(w)?;
        let ac = m.put_cursor("a")?;
        let a = WriteLinkedArrayList::new_write(&ac)?;
        for i in 0..10u64 {
            a.append(uint(i))?;
        }
        a.slice(2, 5)?;
        assert_eq!(a.count()?, 5);
        assert_eq!(a.get(0)?.unwrap().read_uint()?, 2);
        let bc = m.put_cursor("b")?;
        let b = WriteLinkedArrayList::new_write(&bc)?;
        b.append(uint(100))?;
        b.append(uint(101))?;
        a.concat(b.slot()?)?;
        assert_eq!(a.count()?, 7);
        assert_eq!(a.get(5)?.unwrap().read_uint()?, 100);
        assert_eq!(a.get(-1)?.unwrap().read_uint()?, 101);
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = HashMap::new_write(w)?;
        let a = LinkedArrayList::new(m.get("a")?.unwrap())?;
        assert_eq!(a.count()?, 7);
        assert_eq!(a.get(0)?.unwrap().read_uint()?, 2);
        // The donor list stays live after concat (shared subtrees).
        let b = LinkedArrayList::new(m.get("b")?.unwrap())?;
        assert_eq!(b.count()?, 2);
        Ok(())
    })
    .unwrap();
}

#[test]
fn linked_remove_and_put() {
    let db = open_mem(203).unwrap();
    db.run_context(|w| {
        let a = WriteLinkedArrayList::new_write(w)?;
        for i in 0..4u64 {
            a.append(uint(i))?;
        }
        a.remove(1)?;
        assert_eq!(a.count()?, 3);
        assert_eq!(a.get(1)?.unwrap().read_uint()?, 2);
        a.put(0, uint(99))?;
        assert_eq!(a.get(0)?.unwrap().read_uint()?, 99);
        Ok(())
    })
    .unwrap();
}

#[test]
fn sorted_iterates_lexicographically() {
    let db = open_mem(204).unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        for name in ["dave", "alice", "carol", "bob"] {
            m.put(name, bytes(name))?;
        }
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        assert_eq!(m.count()?, 4);
        let mut keys = Vec::new();
        for item in m.try_iter()? {
            let kv = item?.read_key_value_pair()?;
            keys.push(kv.key.read_bytes_alloc()?);
        }
        assert_eq!(
            keys,
            ["alice", "bob", "carol", "dave"]
                .iter()
                .map(|s| s.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            m.get("carol")?.unwrap().read_bytes_alloc()?,
            b"carol".to_vec()
        );
        assert!(m.get("mallory")?.is_none());
        assert!(m.remove("bob")?);
        assert!(!m.remove("bob")?);
        assert_eq!(m.count()?, 3);
        Ok(())
    })
    .unwrap();
}

#[test]
fn sorted_type_ahead_from_key() {
    let db = open_mem(205).unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        for name in ["dave", "alice", "carol", "dan", "bob", "eve"] {
            m.put(name, uint(u64::try_from(name.len()).unwrap()))?;
        }
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        let mut hits = Vec::new();
        for item in m.iter_from(b"da")? {
            let kv = item?.read_key_value_pair()?;
            let key = kv.key.read_bytes_alloc()?;
            if !key.starts_with(b"da") {
                break;
            }
            hits.push(key);
        }
        assert_eq!(hits, vec![b"dan".to_vec(), b"dave".to_vec()]);
        Ok(())
    })
    .unwrap();
}

#[test]
fn sorted_pagination_from_index() {
    let db = open_mem(206).unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        for name in ["dave", "alice", "carol", "dan", "bob", "eve"] {
            m.put(name, uint(1))?;
        }
        Ok(())
    })
    .unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        // Sorted: alice, bob, carol, dan, dave, eve. Page [2,4) = carol, dan.
        let mut page = Vec::new();
        for item in m.iter_from_index(2)? {
            if page.len() == 2 {
                break;
            }
            let kv = item?.read_key_value_pair()?;
            page.push(kv.key.read_bytes_alloc()?);
        }
        assert_eq!(page, vec![b"carol".to_vec(), b"dan".to_vec()]);
        // Rank + index access agree.
        assert_eq!(m.rank("dan")?, 3);
        let kv = m.get_index_key_value_pair(-1)?.unwrap();
        assert_eq!(kv.key.read_bytes_alloc()?, b"eve".to_vec());
        Ok(())
    })
    .unwrap();
}

#[test]
fn sorted_set_basics() {
    let db = open_mem(207).unwrap();
    db.run_context(|w| {
        let s = WriteSortedSet::new_write(w)?;
        s.put("b")?;
        s.put("a")?;
        s.put("a")?;
        assert_eq!(s.count()?, 2);
        assert!(s.contains("a")?);
        assert!(!s.contains("z")?);
        let mut keys = Vec::new();
        for item in s.try_iter()? {
            let kv = item?.read_key_value_pair()?;
            keys.push(kv.key.read_bytes_alloc()?);
        }
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
        assert!(s.remove("a")?);
        assert_eq!(s.count()?, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn sorted_replace_keeps_count() {
    let db = open_mem(208).unwrap();
    db.run_context(|w| {
        let m = WriteSortedMap::new_write(w)?;
        m.put("k", uint(1))?;
        m.put("k", uint(2))?;
        assert_eq!(m.count()?, 1);
        assert_eq!(m.get("k")?.unwrap().read_uint()?, 2);
        Ok(())
    })
    .unwrap();
}
