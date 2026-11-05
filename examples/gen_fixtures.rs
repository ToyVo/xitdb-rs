//! Writes `tests/fixtures/rust-golden.db` (fixed file id) for the Go
//! reverse-check in `scripts/gen-fixtures.sh`.

use xitdb::arraylist::ArrayList;
use xitdb::core::FileCore;
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::hasher::Sha1Hasher;
use xitdb::hashmap::WriteHashMap;
use xitdb::linked_arraylist::WriteLinkedArrayList;
use xitdb::sorted_map::WriteSortedMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args().nth(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: gen_fixtures <out.db>",
        )
    })?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&out)?;
    let db = Database::open(
        FileCore::new(file, false),
        Sha1Hasher,
        InitOpts {
            hash_id: Some(xitdb::hasher::bytes_to_id(*b"sha1")),
            file_id: Some(5678),
            fsync: true,
        },
    )?;
    db.run_context(|w| {
        let history = ArrayList::new_write(w)?;
        history.append_context(Writeable::None, |mc| {
            let m = WriteHashMap::new_write(mc)?;
            m.put("name", Writeable::Bytes(b"rust".to_vec()))?;
            m.put("answer", Writeable::Uint(42))?;
            m.put(
                "tagged",
                Writeable::BytesObject {
                    value: b"abc".to_vec(),
                    format_tag: Some(*b"bi"),
                },
            )?;
            let lc = m.put_cursor("list")?;
            let list = ArrayList::new_write(&lc)?;
            list.append(Writeable::Uint(1))?;
            list.append(Writeable::Uint(2))?;
            let sc = m.put_cursor("sorted")?;
            let sm = WriteSortedMap::new_write(&sc)?;
            sm.put("b", Writeable::Uint(2))?;
            sm.put("a", Writeable::Uint(1))?;
            let kc = m.put_cursor("linked")?;
            let linked = WriteLinkedArrayList::new_write(&kc)?;
            linked.append(Writeable::Bytes(b"x".to_vec()))?;
            Ok(())
        })
    })?;
    println!("wrote {out}");
    Ok(())
}
