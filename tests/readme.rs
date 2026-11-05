//! Executable companion to `README.md`: one test per section, using the
//! same calls as the documented snippets so they cannot rot.
use std::cell::RefCell;

use xitdb::arraylist::ArrayList;
use xitdb::core::{BufferedFileCore, Core, MemoryCore};
use xitdb::cursor::{ReadOnly, WriteCursor, Writeable};
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::{Hasher, Sha1Hasher, bytes_to_id, id_to_bytes};
use xitdb::hashmap::HashMap;
use xitdb::header::{HEADER_LEN, Header};
use xitdb::offset_map::{FileOffsetMap, MemoryOffsetMap};
use xitdb::sorted_map::SortedMap;
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

fn tmp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("xitdb-readme-{name}-{}.db", std::process::id()))
}

fn open_file(
    path: &std::path::Path,
    file_id: u64,
) -> Result<Database<BufferedFileCore, Sha1Hasher>, Error> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(Error::Io)?;
    Database::open(
        BufferedFileCore::new(file, false)?,
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(file_id),
            fsync: false,
        },
    )
}

/// The README example: one moment holding `foo`, `bar`, `fruits`, `people`.
fn seed_example(w: &WriteCursor<'_, impl Core, impl Hasher>) -> Result<(), Error> {
    let history = ArrayList::new_write(w)?;
    history.append_context(Writeable::None, |mc| {
        let moment = HashMap::new_write(mc)?;
        moment.put("foo", Writeable::Bytes(b"foo".to_vec()))?;
        moment.put("bar", Writeable::Bytes(b"bar".to_vec()))?;
        let fruits_cursor = moment.put_cursor("fruits")?;
        let fruits = ArrayList::new_write(&fruits_cursor)?;
        fruits.append(Writeable::Bytes(b"apple".to_vec()))?;
        fruits.append(Writeable::Bytes(b"pear".to_vec()))?;
        fruits.append(Writeable::Bytes(b"grape".to_vec()))?;
        let people_cursor = moment.put_cursor("people")?;
        let people = ArrayList::new_write(&people_cursor)?;
        let alice_cursor = people.append_cursor()?;
        let alice = HashMap::new_write(&alice_cursor)?;
        alice.put("name", Writeable::Bytes(b"Alice".to_vec()))?;
        alice.put("age", Writeable::Uint(25))?;
        let bob_cursor = people.append_cursor()?;
        let bob = HashMap::new_write(&bob_cursor)?;
        bob.put("name", Writeable::Bytes(b"Bob".to_vec()))?;
        bob.put("age", Writeable::Uint(42))?;
        Ok(())
    })
}

fn latest_moment(
    db: &Database<MemoryCore, Sha1Hasher>,
) -> Result<HashMap<'_, MemoryCore, Sha1Hasher, ReadOnly>, Error> {
    let cursor = db.root_cursor();
    let history = ArrayList::new_write(&cursor)?;
    let moment = history.get(-1)?.ok_or(Error::KeyNotFound)?;
    HashMap::new(moment)
}

fn people_in<'a>(
    moment: &HashMap<'a, MemoryCore, Sha1Hasher, ReadOnly>,
) -> Result<ArrayList<'a, MemoryCore, Sha1Hasher, ReadOnly>, Error> {
    let cursor = moment.get("people")?.ok_or(Error::KeyNotFound)?;
    ArrayList::new(cursor)
}

fn committed_bytes(db: &Database<MemoryCore, Sha1Hasher>) -> Result<Vec<u8>, Error> {
    let end = db.committed_size()?;
    let mut buf = Vec::new();
    db.read_committed(0, end, &mut buf)?;
    Ok(buf)
}

#[test]
fn example() {
    let path = tmp_path("example");
    let db = open_file(&path, 900).unwrap();
    db.run_context(seed_example).unwrap();

    let cursor = db.root_cursor();
    let history = ArrayList::new_write(&cursor).unwrap();
    let moment = HashMap::new(history.get(-1).unwrap().unwrap()).unwrap();
    let foo = moment.get("foo").unwrap().unwrap();
    assert_eq!(foo.read_bytes_alloc().unwrap(), b"foo");

    let fruits = ArrayList::new(moment.get("fruits").unwrap().unwrap()).unwrap();
    let apple = fruits.get(0).unwrap().unwrap();
    assert_eq!(apple.read_bytes_alloc().unwrap(), b"apple");

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn initializing_memory() {
    let db = open_mem(901).unwrap();
    db.run_context(|w| {
        w.write(Writeable::Uint(42))?;
        Ok(())
    })
    .unwrap();
    assert_eq!(db.root_cursor().read_uint().unwrap(), 42);
}

#[test]
fn type_format_tag() {
    let db = open_mem(902).unwrap();
    db.run_context(|w| {
        let moment = HashMap::new_write(w)?;
        moment.put(
            "random-number",
            Writeable::BytesObject {
                value: vec![7u8; 32],
                format_tag: Some(*b"bi"),
            },
        )?;
        Ok(())
    })
    .unwrap();

    let cursor = db.root_cursor();
    let moment = HashMap::new_write(&cursor).unwrap();
    let cursor = moment.get("random-number").unwrap().unwrap();
    let mut scratch = [0u8; 64];
    let number = cursor.read_bytes_object(&mut scratch).unwrap();
    assert_eq!(number.format_tag, Some(*b"bi"));
    assert_eq!(number.value.len(), 32);
}

#[test]
fn clone_food() {
    let db = open_mem(903).unwrap();
    db.run_context(seed_example).unwrap();
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let prev = history.get_slot(-1)?;
        history.append_context(Writeable::Slot(prev), |mc| {
            let moment = HashMap::new_write(mc)?;
            let fruits = moment.get("fruits")?.ok_or(Error::KeyNotFound)?;
            let fruits_list = ArrayList::new(fruits)?;
            let food_cursor = moment.put_cursor("food")?;
            food_cursor.write(Writeable::Slot(fruits_list.slot()?))?;
            let food = ArrayList::new_write(&food_cursor)?;
            food.append(Writeable::Bytes(b"eggs".to_vec()))?;
            food.append(Writeable::Bytes(b"rice".to_vec()))?;
            food.append(Writeable::Bytes(b"fish".to_vec()))?;
            Ok(())
        })
    })
    .unwrap();

    let moment = latest_moment(&db).unwrap();
    let food = ArrayList::new(moment.get("food").unwrap().unwrap()).unwrap();
    assert_eq!(food.count().unwrap(), 6);
    let fruits = ArrayList::new(moment.get("fruits").unwrap().unwrap()).unwrap();
    assert_eq!(fruits.count().unwrap(), 3);
}

#[test]
fn clone_same_transaction_sees_writes() {
    let db = open_mem(904).unwrap();
    db.run_context(seed_example).unwrap();
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let prev = history.get_slot(-1)?;
        history.append_context(Writeable::Slot(prev), |mc| {
            let moment = HashMap::new_write(mc)?;
            let big_cursor = moment.put_cursor("big-cities")?;
            let big = ArrayList::new_write(&big_cursor)?;
            big.append(Writeable::Bytes(b"New York, NY".to_vec()))?;
            big.append(Writeable::Bytes(b"Los Angeles, CA".to_vec()))?;
            let cities_cursor = moment.put_cursor("cities")?;
            cities_cursor.write(Writeable::Slot(big.slot()?))?;
            let cities = ArrayList::new_write(&cities_cursor)?;
            cities.append(Writeable::Bytes(b"Charleston, SC".to_vec()))?;
            cities.append(Writeable::Bytes(b"Louisville, KY".to_vec()))?;
            Ok(())
        })
    })
    .unwrap();

    // Same-transaction data is temporarily mutable, so the clone saw the
    // later appends too: both lists hold all four cities.
    let moment = latest_moment(&db).unwrap();
    let cities = ArrayList::new(moment.get("cities").unwrap().unwrap()).unwrap();
    assert_eq!(cities.count().unwrap(), 4);
    let big = ArrayList::new(moment.get("big-cities").unwrap().unwrap()).unwrap();
    assert_eq!(big.count().unwrap(), 4);
}

#[test]
fn undo_and_freeze() {
    let db = open_mem(905).unwrap();
    db.run_context(seed_example).unwrap();
    let cursor = db.root_cursor();
    let history = ArrayList::new_write(&cursor).unwrap();
    let saved_index = i64::try_from(history.count().unwrap().checked_sub(1).unwrap()).unwrap();

    // The surprising transaction from the previous test, then undo it.
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let prev = history.get_slot(-1)?;
        history.append_context(Writeable::Slot(prev), |mc| {
            let moment = HashMap::new_write(mc)?;
            let big_cursor = moment.put_cursor("big-cities")?;
            let big = ArrayList::new_write(&big_cursor)?;
            big.append(Writeable::Bytes(b"New York, NY".to_vec()))?;
            let cities_cursor = moment.put_cursor("cities")?;
            cities_cursor.write(Writeable::Slot(big.slot()?))?;
            Ok(())
        })
    })
    .unwrap();
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let slot = history.get_slot(saved_index)?;
        history.append(Writeable::Slot(slot))?;
        Ok(())
    })
    .unwrap();

    // Redo with `freeze`: writes through `cities` copy instead of
    // mutating `big-cities`.
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        let prev = history.get_slot(-1)?;
        history.append_context(Writeable::Slot(prev), |mc| {
            let moment = HashMap::new_write(mc)?;
            let big_cursor = moment.put_cursor("big-cities")?;
            let big = ArrayList::new_write(&big_cursor)?;
            big.append(Writeable::Bytes(b"New York, NY".to_vec()))?;
            big.append(Writeable::Bytes(b"Los Angeles, CA".to_vec()))?;
            let cities_cursor = moment.put_cursor("cities")?;
            cities_cursor.write(Writeable::Slot(big.slot()?))?;
            cities_cursor.freeze()?;
            let cities = ArrayList::new_write(&cities_cursor)?;
            cities.append(Writeable::Bytes(b"Charleston, SC".to_vec()))?;
            cities.append(Writeable::Bytes(b"Louisville, KY".to_vec()))?;
            Ok(())
        })
    })
    .unwrap();

    let moment = latest_moment(&db).unwrap();
    let cities = ArrayList::new(moment.get("cities").unwrap().unwrap()).unwrap();
    assert_eq!(cities.count().unwrap(), 4);
    let big = ArrayList::new(moment.get("big-cities").unwrap().unwrap()).unwrap();
    assert_eq!(big.count().unwrap(), 2);
}

fn seed_users(db: &Database<MemoryCore, Sha1Hasher>) -> Result<(), Error> {
    let users = [
        ("user000000000001", "dave", "Dave Smith"),
        ("user000000000002", "alice", "Alice Jones"),
        ("user000000000003", "carol", "Carol White"),
        ("user000000000004", "dan", "Dan Brown"),
        ("user000000000005", "bob", "Bob Lee"),
        ("user000000000006", "eve", "Eve Adams"),
    ];
    db.run_context(|w| {
        let moment = HashMap::new_write(w)?;
        let idc = moment.put_cursor("id->user")?;
        let id_to_user = HashMap::new_write(&idc)?;
        let uc = moment.put_cursor("username->id")?;
        let username_to_id = SortedMap::new_write(&uc)?;
        for (id, username, name) in users {
            let user_cursor = id_to_user.put_cursor(id)?;
            let user = HashMap::new_write(&user_cursor)?;
            user.put("username", Writeable::Bytes(username.as_bytes().to_vec()))?;
            user.put("name", Writeable::Bytes(name.as_bytes().to_vec()))?;
            username_to_id.put(username, Writeable::Bytes(id.as_bytes().to_vec()))?;
        }
        Ok(())
    })
}

fn order_key(score: u64, user_id: &[u8; 16]) -> [u8; 24] {
    let mut key = [0u8; 24];
    for (slot, byte) in key
        .iter_mut()
        .zip(score.to_be_bytes().iter().chain(user_id.iter()))
    {
        *slot = *byte;
    }
    key
}

#[test]
fn sorted_pagination() {
    let db = open_mem(906).unwrap();
    seed_users(&db).unwrap();

    let cursor = db.root_cursor();
    let moment = HashMap::new_write(&cursor).unwrap();
    let username_to_id = SortedMap::new(moment.get("username->id").unwrap().unwrap()).unwrap();
    assert_eq!(username_to_id.count().unwrap(), 6);

    let page_size = 2u64;
    let mut walker = username_to_id.iter_from_index(0).unwrap();
    let mut page = Vec::new();
    for _ in 0..page_size {
        let Some(entry) = walker.next() else { break };
        let kv = entry.unwrap().read_key_value_pair().unwrap();
        page.push(kv.key.read_bytes_alloc().unwrap());
    }
    assert_eq!(page, vec![b"alice".to_vec(), b"bob".to_vec()]);

    let mut walker = username_to_id.iter_from_index(2).unwrap();
    let mut page = Vec::new();
    for _ in 0..page_size {
        let Some(entry) = walker.next() else { break };
        let kv = entry.unwrap().read_key_value_pair().unwrap();
        page.push(kv.key.read_bytes_alloc().unwrap());
    }
    assert_eq!(page, vec![b"carol".to_vec(), b"dan".to_vec()]);
}

#[test]
fn sorted_type_ahead() {
    let db = open_mem(907).unwrap();
    seed_users(&db).unwrap();

    let cursor = db.root_cursor();
    let moment = HashMap::new_write(&cursor).unwrap();
    let username_to_id = SortedMap::new(moment.get("username->id").unwrap().unwrap()).unwrap();

    let walker = username_to_id.iter_from(b"da").unwrap();
    let mut found = Vec::new();
    for entry in walker {
        let kv = entry.unwrap().read_key_value_pair().unwrap();
        let username = kv.key.read_bytes_alloc().unwrap();
        if !username.starts_with(b"da") {
            break;
        }
        found.push(username);
    }
    assert_eq!(found, vec![b"dan".to_vec(), b"dave".to_vec()]);

    assert!(order_key(9, b"user000000000001") < order_key(10, b"user000000000001"));
}

#[test]
fn large_byte_arrays() {
    let db = open_mem(908).unwrap();
    db.run_context(|w| {
        let moment = HashMap::new_write(w)?;
        let text_cursor = moment.put_cursor("long-text")?;
        let mut buf = [0u8; 1024];
        let mut writer = text_cursor.writer(&mut buf)?;
        for _ in 0..50 {
            writer.write_bytes(b"hello, world!\n")?;
        }
        writer.set_format_tag(*b"tx");
        writer.finish()?;
        Ok(())
    })
    .unwrap();

    let cursor = db.root_cursor();
    let moment = HashMap::new_write(&cursor).unwrap();
    let cursor = moment.get("long-text").unwrap().unwrap();
    // An empty buffer means "no cap": read the whole value and its tag.
    let tagged = cursor.read_bytes_object(&mut []).unwrap();
    assert_eq!(tagged.format_tag, Some(*b"tx"));

    let mut rbuf = [0u8; 1024];
    let mut reader = cursor.reader(&mut rbuf).unwrap();
    let bytes = reader.read_all().unwrap();
    let mut newlines = 0usize;
    for byte in &bytes {
        if *byte == b'\n' {
            newlines = newlines.checked_add(1).unwrap();
        }
    }
    assert_eq!(newlines, 50);
}

#[test]
fn iterators() {
    let db = open_mem(909).unwrap();
    db.run_context(seed_example).unwrap();

    let moment = latest_moment(&db).unwrap();
    let people = people_in(&moment).unwrap();
    let mut walked = Vec::new();
    let people_walker = people.try_iter().unwrap();
    for person in people_walker {
        let fields = HashMap::new(person.unwrap()).unwrap();
        let fields_walker = fields.try_iter().unwrap();
        for kv_cursor in fields_walker {
            let kv = kv_cursor.unwrap().read_key_value_pair().unwrap();
            let key = kv.key.read_bytes_alloc().unwrap();
            let value = match kv.value.slot().unwrap().tag {
                Tag::Bytes | Tag::ShortBytes => kv.value.read_bytes_alloc().unwrap(),
                Tag::Uint => kv.value.read_uint().unwrap().to_string().into_bytes(),
                _ => Vec::new(),
            };
            walked.push((key, value));
        }
    }
    assert_eq!(walked.len(), 4);
    assert!(walked.contains(&(b"name".to_vec(), b"Alice".to_vec())));
    assert!(walked.contains(&(b"age".to_vec(), b"42".to_vec())));
}

#[test]
fn hashing() {
    assert_eq!(Sha1Hasher.digest_len(), 20);
    assert_eq!(Sha1Hasher.digest(b"foo").len(), 20);
    assert_eq!(id_to_bytes(bytes_to_id(*b"sha1")), *b"sha1");

    let db = open_mem(910).unwrap();
    db.run_context(seed_example).unwrap();

    let bytes = committed_bytes(&db).unwrap();
    let header = Header::from_bytes(bytes[..HEADER_LEN].try_into().unwrap());
    assert_eq!(header.hash_size, 20);
    assert_eq!(header.hash_id, bytes_to_id(*b"sha1"));

    let algo = match header.hash_id {
        id if id == bytes_to_id(*b"sha1") => "sha1",
        _ => "unknown",
    };
    assert_eq!(algo, "sha1");
}

#[test]
fn compaction() {
    let db = open_mem(911).unwrap();
    for _ in 0..3 {
        db.run_context(seed_example).unwrap();
    }
    let mut offsets = MemoryOffsetMap::new();
    let compacted = db
        .compact(
            MemoryCore::new(),
            Sha1Hasher,
            InitOpts {
                hash_id: None,
                file_id: Some(912),
                fsync: false,
            },
            &mut offsets,
        )
        .unwrap();
    compacted
        .run_context(|w| {
            let history = ArrayList::new_write(w)?;
            assert_eq!(history.count()?, 1);
            Ok(())
        })
        .unwrap();

    // File-backed offsets inside one scratch transaction.
    let scratch = open_mem(913).unwrap();
    let cell: RefCell<Option<Database<MemoryCore, Sha1Hasher>>> = RefCell::new(None);
    scratch
        .run_context(|c| {
            let mut file_offsets = FileOffsetMap::new(c)?;
            let rebuilt = db.compact(
                MemoryCore::new(),
                Sha1Hasher,
                InitOpts {
                    hash_id: None,
                    file_id: Some(914),
                    fsync: false,
                },
                &mut file_offsets,
            )?;
            *cell.borrow_mut() = Some(rebuilt);
            Ok(())
        })
        .unwrap();
    let rebuilt = cell.into_inner().unwrap();
    rebuilt
        .run_context(|w| {
            let history = ArrayList::new_write(w)?;
            assert_eq!(history.count()?, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn replication() {
    let source = open_mem(915).unwrap();
    source.run_context(seed_example).unwrap();
    let replica = open_mem(916).unwrap();

    // 1. replica describes what it has; 2. source decides what to send.
    let state = replica.replica_state().unwrap();
    let start = source.replication_start(state).unwrap();
    let end = source.committed_size().unwrap();
    // 3. source writes those bytes; 4. replica applies them.
    let mut buf = Vec::new();
    source.read_committed(start, end, &mut buf).unwrap();
    let len = end.checked_sub(start).unwrap();
    replica.apply_replicated(start, &mut &buf[..], len).unwrap();

    assert_eq!(
        committed_bytes(&source).unwrap(),
        committed_bytes(&replica).unwrap()
    );
}
