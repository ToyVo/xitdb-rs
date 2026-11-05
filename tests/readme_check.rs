//! Mirrors the README quickstart so the documented snippet never rots.
use xitdb::arraylist::WriteArrayList;
use xitdb::core::MemoryCore;
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::hasher::Sha1Hasher;
use xitdb::hashmap::{HashMap, WriteHashMap};

fn build_hello_db() -> Result<Database<MemoryCore, Sha1Hasher>, xitdb::error::Error> {
    let db = Database::open(
        MemoryCore::new(),
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: None,
            fsync: false,
        },
    )?;

    // The top-level ArrayList is the history: each item is a moment in time.
    db.run_context(|w| {
        let history = WriteArrayList::new_write(w)?;
        history.append_context(Writeable::None, |mc| {
            let moment = WriteHashMap::new_write(mc)?;
            moment.put("hello", Writeable::Bytes(b"world".to_vec()))?;
            Ok(())
        })
    })?;
    Ok(db)
}

#[test]
fn readme_quickstart() {
    let db = build_hello_db().unwrap();
    let w = db.root_cursor();
    let history = WriteArrayList::new_write(&w).unwrap();
    let moment = HashMap::new(history.get(0).unwrap().unwrap()).unwrap();
    let value = moment
        .get("hello")
        .unwrap()
        .unwrap()
        .read_bytes_alloc()
        .unwrap();
    assert_eq!(value, b"world");
}
