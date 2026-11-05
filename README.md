# xitdb-rs

[![crates.io](https://img.shields.io/crates/v/xitdb.svg)](https://crates.io/crates/xitdb)
[![docs.rs](https://img.shields.io/docsrs/xitdb.svg)](https://docs.rs/xitdb)
[![CI](https://github.com/ToyVo/xitdb-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/ToyVo/xitdb-rs/actions/workflows/ci.yml)

<p align="center">
  xitdb is an immutable database written in Rust
  <br/>
  <br/>
  <b>Choose your flavor:</b>
  <a href="https://github.com/xit-vcs/xitdb">Zig</a> |
  <a href="https://github.com/xit-vcs/xitdb-java">Java</a> |
  <a href="https://github.com/codeboost/xitdb-clj">Clojure</a> |
  <a href="https://github.com/xit-vcs/xitdb-ts">TypeScript</a> |
  <a href="https://github.com/xit-vcs/xitdb-go">Go</a> |
  <a href="https://github.com/ToyVo/xitdb-rs">Rust</a>
</p>

* Each transaction efficiently creates a new "copy" of the database, and past copies can still be read from and reverted to.
* Supports storing in a single file as well as purely in-memory use.
* Runs as a library (embedded in process).
* Incrementally reads and writes, so file-based databases can contain larger-than-memory datasets.
* Reads never block writes, and a database can be read from multiple threads/processes without locks (each thread uses its own `Database`).
* No query engine of any kind. You just write data structures (primarily an `ArrayList` and `HashMap`) that can be nested arbitrarily.
* No dependencies at all: SHA-1 and OS randomness are vendored in-tree (requires Rust 1.85+).

This is a Rust port of the database originally made for the [xit version control system](https://github.com/xit-vcs/xit), following [xitdb-go](https://github.com/xit-vcs/xitdb-go) as the idiomatic-port model. The combination of being immutable and having an API similar to in-memory data structures is pretty powerful. Consider using it instead of SQLite for your Rust projects: it's simpler, it's pure Rust, and it creates no impedance mismatch with your program the way SQL databases do. Check out [xitdb.net](https://xitdb.net/zig.html) and the video [My SQLite killer was a happy accident](https://www.youtube.com/watch?v=YRUF-EaTFFM).

* [Example](#example)
* [Initializing a Database](#initializing-a-database)
* [Types](#types)
* [Cloning and Undoing](#cloning-and-undoing)
* [Sorting and Paginating](#sorting-and-paginating)
* [Large Byte Arrays](#large-byte-arrays)
* [Iterators](#iterators)
* [Hashing](#hashing)
* [Compaction](#compaction)
* [Replication](#replication)
* [Thread Safety](#thread-safety)
* [Benchmarks](#benchmarks)

Every snippet below is executed by `tests/readme.rs`, so the documented calls always work as shown.

## Example

In this example, we create a new database, write some data in a transaction, and read the data afterwards.

```rust
use std::fs::OpenOptions;
use xitdb::arraylist::ArrayList;
use xitdb::core::BufferedFileCore;
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::Sha1Hasher;
use xitdb::hashmap::{ReadHashMap, WriteHashMap};

// create db file
let file = OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .open("main.db")?;

// acquire an exclusive file lock (only needed when writing)
// fs2::FileExt::lock_exclusive(&file)?;

// buffered writes are highly recommended for file databases
let db = Database::open(
    BufferedFileCore::new(file, false)?,
    Sha1Hasher,
    InitOpts { hash_id: None, file_id: None, fsync: false },
)?;

// the top-level data structure is an ArrayList, so each transaction is
// stored as an item in it, and past copies of the db stay readable

// this is how a transaction is executed. we call append_context,
// providing it with the most recent copy of the db. the closure runs
// before the transaction completes, and it is where we write our
// changes. if any error happens in it, the transaction will not
// complete, the data added to the file will be truncated, and the db
// will be unaffected.
//
// after this transaction, the db will look like this if represented
// as JSON (in reality the format is binary):
//
// {"foo": "foo",
//  "bar": "bar",
//  "fruits": ["apple", "pear", "grape"],
//  "people": [
//    {"name": "Alice", "age": 25},
//    {"name": "Bob", "age": 42}
//  ]}
db.run_context(|w| {
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
})?;

// get the most recent copy of the database, like a moment
// in time. the -1 index returns the last item in the list.
let root = db.root_cursor();
let history = ArrayList::new_write(&root)?;
let moment = HashMap::new(history.get(-1)?.ok_or(Error::KeyNotFound)?)?;

// we can read the value of "foo" from the map by getting
// the cursor to "foo" and then reading the bytes
let foo = moment.get("foo")?.ok_or(Error::KeyNotFound)?;
assert_eq!(foo.read_bytes_alloc()?, b"foo");

// to get the "fruits" list, we get the cursor to it and
// then wrap it as an ArrayList
let fruits = ArrayList::new(moment.get("fruits")?.ok_or(Error::KeyNotFound)?)?;

// now we can get the first item from the fruits list and read it
let apple = fruits.get(0)?.ok_or(Error::KeyNotFound)?;
assert_eq!(apple.read_bytes_alloc()?, b"apple");
```

## Initializing a Database

There are three backends you can open a `Database` with: `MemoryCore`, `FileCore`, and `BufferedFileCore`.

* `BufferedFileCore` databases, like in the example above, write to a file while using an in-memory buffer to dramatically improve performance. This is highly recommended if you want to create a file-based database.
* `FileCore` databases use no buffering when reading and writing data. You can open one like in the example above, except with `FileCore::new(file, false)`. This is almost never necessary but it's useful as a benchmark comparison with `BufferedFileCore` databases.
* `MemoryCore` databases work completely in memory:
  `Database::open(MemoryCore::new(), Sha1Hasher, InitOpts { .. })`.

`InitOpts` carries the `hash_id` (see [Hashing](#hashing)), an optional fixed `file_id`, and whether to `fsync` on commit.

Since xitdb is an append-only database, the data you are writing is invisible to any reader until the very last step, when a small header at the start of the file is updated to point at it. This is how it supports transactions, despite not having any rollback journal or write-ahead log: if a transaction fails, the data it appended is simply discarded.

Usually, you want to use a top-level `ArrayList` like in the example above, because that allows you to store a reference to each copy of the database (which I call a "moment"), so past copies stay readable and can be reverted to. If you don't need to keep past copies, you can also use a top-level `HashMap`. Keep in mind that each write to a top-level `HashMap` is its own transaction, so if you make many writes, group them into one transaction with `run_context`.

Since xitdb supports in-memory databases, you could use it as an over-the-wire serialization format. Much like "Cap'n Proto", xitdb has no encoding/decoding step: you just give the buffer to xitdb and it can immediately read from it. You can send it over a network and read it with xitdb on the other end.

## Types

In xitdb there are a variety of immutable data structures that you can nest arbitrarily:

* `HashMap` contains key-value pairs stored with a hash
* `HashSet` is like a `HashMap` that only sets the keys; it is useful when only checking for membership
* `CountedHashMap` and `CountedHashSet` are just a `HashMap` and `HashSet` that maintain a count of their contents
* `ArrayList` is a growable array
* `LinkedArrayList` is like an `ArrayList` that can also be efficiently sliced and concatenated
* `SortedMap` and `SortedSet` are like a `HashMap` and `HashSet` where the keys are byte arrays kept in lexicographic order

The `Hash`-based data structures and the `ArrayList` use the hash array mapped trie, invented by Phil Bagwell (originally made immutable and widely available by Rich Hickey in Clojure). The `LinkedArrayList`, `SortedMap`, and `SortedSet` are based on a B-tree.

There are also scalar types you can store in the above-mentioned data structures:

* `Writeable::Bytes` is a byte array
* `Writeable::Uint` is an unsigned 64-bit int
* `Writeable::Int` is a signed 64-bit int
* `Writeable::Float` is a 64-bit float

You may also want to define custom types. For example, you may want to store a big integer that can't fit in 64 bits. You could just store this with `Bytes`, but when reading the byte array there wouldn't be any indication that it should be interpreted as a big integer.

In xitdb, you can optionally store a format tag with a byte array. A format tag is a 2 byte tag that is stored alongside the byte array. Readers can use it to decide how to interpret the byte array. Here's an example of storing a 256-bit number with `bi` as the format tag:

```rust
moment.put(
    "random-number",
    Writeable::BytesObject {
        value: vec![7u8; 32],
        format_tag: Some(*b"bi"),
    },
)?;
```

Then, you can read it like this:

```rust
let cursor = moment.get("random-number")?.ok_or(Error::KeyNotFound)?;
let number = cursor.read_bytes_object(&mut [])?;
assert_eq!(number.format_tag, Some(*b"bi"));
```

There are many types you may want to store this way. Maybe an ISO-8601 date like `2026-01-01T18:55:48Z` could be stored with `dt` as the format tag. It's also great for storing custom structs. Just define the struct, serialize it as a byte array using whatever mechanism you wish, and store it with a format tag. Keep in mind that format tags can be *any* 2 bytes, so there are 65536 possible format tags.

## Cloning and Undoing

A powerful feature of immutable data is fast cloning. Any data structure can be instantly cloned and changed without affecting the original. Starting with the example code above, we can make a new transaction that creates a "food" list based on the existing "fruits" list. New moments start as a clone of the previous one, so we pass its slot into `append_context`:

```rust
let prev = history.get_slot(-1)?;
history.append_context(Writeable::Slot(prev), |mc| {
    let moment = HashMap::new_write(mc)?;
    let fruits = moment.get("fruits")?.ok_or(Error::KeyNotFound)?;

    // create a new key called "food" whose initial value is
    // based on the "fruits" list
    let food_cursor = moment.put_cursor("food")?;
    food_cursor.write(Writeable::Slot(ArrayList::new(fruits)?.slot()?))?;

    let food = ArrayList::new_write(&food_cursor)?;
    food.append(Writeable::Bytes(b"eggs".to_vec()))?;
    food.append(Writeable::Bytes(b"rice".to_vec()))?;
    food.append(Writeable::Bytes(b"fish".to_vec()))?;
    Ok(())
})?;

// the food list includes the fruits (6 items)...
// ...but the fruits list hasn't been changed (3 items)
```

Before we continue, let's save the latest history index, so we can revert back to this moment of the database later:

```rust
let history_index = i64::try_from(history.count()?)?;
```

There's one catch you'll run into when cloning. If we try cloning a data structure that was created in the same transaction, it doesn't seem to work: the clone observes the later writes too, so "big-cities" ends up holding all four cities instead of two.

The reason is that all data in a given transaction is temporarily mutable. This is a very important optimization, but in this case, it's not what we want.

To show how to fix this, let's first undo the transaction we just made. Here we use the `history_index` we saved before to revert back to the older database moment:

```rust
let slot = history.get_slot(history_index)?;
history.append(Writeable::Slot(slot))?;
```

This time, after cloning "big cities" into the "cities" list, we call `freeze` on the cursor. This tells xitdb that writes through that cursor must treat all data made so far in the transaction as immutable, so they copy it instead of changing it. Writes through other cursors aren't affected. Now it works the way we wanted (4 and 2):

```rust
let cities_cursor = moment.put_cursor("cities")?;
cities_cursor.write(Writeable::Slot(big.slot()?))?;

// freeze here, so writes to cities won't mutate big-cities.
// note: you can also freeze globally with `db.freeze()`.
cities_cursor.freeze()?;

let cities = ArrayList::new_write(&cities_cursor)?;
cities.append(Writeable::Bytes(b"Charleston, SC".to_vec()))?;
cities.append(Writeable::Bytes(b"Louisville, KY".to_vec()))?;
```

## Sorting and Paginating

The `Hash`-based structures are great for looking data up by key, but they store their contents in hash order, which is meaningless to a human. Real apps need to show data in a sensible order (such as users listed alphabetically) one page at a time. Relational databases like SQLite have this built-in: you declare a `CREATE INDEX`, write `ORDER BY username LIMIT 20 OFFSET 40`, and the query planner maintains the index for you.

In xitdb there are no built-in indexes, so you build and maintain them yourself. That's a little more code, but the index is just another data structure: a `SortedMap` whose keys sort the way you want. You keep it in sync by writing to it in the same transaction that writes the primary data.

Why a `SortedMap` and not an `ArrayList`? An `ArrayList` keeps things in insertion order, which is only useful when the order you want *is* the order you wrote them in. The moment you want a different order (alphabetical, by score, by anything that isn't "when it arrived") you need a structure that stays sorted by a key. A `SortedMap` does, and it can seek straight to the first key greater than or equal to a given value, which is what makes type-ahead search possible.

Let's model a user directory: a collection of users we look up by id, plus a secondary index that lists them alphabetically by username. The primary store is a `HashMap` from user id to the user's fields (like a row keyed by its primary key). The secondary index is a `SortedMap` keyed by username, whose value is the user id to look up.

A `SortedMap` orders its keys lexicographically by their raw bytes. For ASCII usernames that's just alphabetical order, and since usernames are unique, every key is already distinct, so the key is simply the username itself. For a sort key that *isn't* unique, like a score, you'd append the id to keep keys distinct. See the note at the end.

Now we write some users. Note they're inserted in arbitrary order; the index sorts them, so insertion order doesn't matter. On each insert we write the user into the primary map and add an entry to the secondary index (keeping both in sync is your job, not the database's):

```rust
// inserted in arbitrary order; the index will sort them alphabetically
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

    // the primary store: a HashMap from user id to the user's fields
    let idc = moment.put_cursor("id->user")?;
    let id_to_user = HashMap::new_write(&idc)?;

    // the secondary index: a SortedMap ordered alphabetically by username.
    // there's no CREATE INDEX here, so we maintain it ourselves on every write.
    let uc = moment.put_cursor("username->id")?;
    let username_to_id = SortedMap::new_write(&uc)?;

    for (id, username, name) in users {
        // write the user into the primary map under its id
        let user_cursor = id_to_user.put_cursor(id)?;
        let user = HashMap::new_write(&user_cursor)?;
        user.put("username", Writeable::Bytes(username.as_bytes().to_vec()))?;
        user.put("name", Writeable::Bytes(name.as_bytes().to_vec()))?;

        // add an entry to the secondary index: the key is the username (the
        // sort key), and the value is the user id we'll use to look it back up.
        username_to_id.put(username, Writeable::Bytes(id.as_bytes().to_vec()))?;
    }
    Ok(())
})?;
```

To display a page, we walk the `SortedMap` instead of the `HashMap`. A web app would take a `page_size` and an `after` offset from the request (something like `/users?after=20`), so this is the xitdb equivalent of `ORDER BY username LIMIT page_size OFFSET after`:

```rust
// a web request would supply these; here we just grab the first page
let page_size = 2u64;
let after = 0i64;

let mut pager = username_to_id.iter_from_index(after)?;
let mut page = Vec::new();
for _ in 0..page_size {
    let Some(entry) = pager.next() else { break };
    let kv = entry?.read_key_value_pair()?;
    page.push(kv.key.read_bytes_alloc()?);
}
// page == [b"alice", b"bob"]
```

Seeking works because `SortedMap` is a count-augmented B-tree: `iter_from_index` finds rank `after` in O(log n) without scanning the entries it skips, so jumping to page 500 is just as cheap as page 1. Once you hold an index entry, its value is the user id to look the full user back up in the primary map.

Pagination by index is only half of what the ordering buys us. Because the index is sorted by username, we can also seek straight to a *key* (the first username greater than or equal to a prefix) and walk forward only as far as the prefix matches. That's a type-ahead search (think @-mention autocomplete), and it's the thing an `ArrayList` can't do: with no sorted index, there's nothing to seek into. We use `iter_from` (which takes a key) instead of `iter_from_index` (which takes a rank):

```rust
// the user typed "da" into an @-mention box; find everyone whose username
// starts with it. iter_from seeks to the first key >= "da" in O(log n),
// then we walk forward until a username no longer starts with the prefix.
let mut ac = username_to_id.iter_from(b"da")?;
while let Some(entry) = ac.next() {
    let kv = entry?.read_key_value_pair()?;
    let username = kv.key.read_bytes_alloc()?;
    if !username.starts_with(b"da") {
        break;
    }
    // a real app would offer this as a suggestion (here: "dan", then "dave")
}
```

This works for any ordering you need: sort by a username with a string key like we did here, by score with a big-endian integer key (encode numbers big-endian so their byte order matches numeric order), or build several `SortedMap` indexes over the same primary `HashMap` to offer the data in different orders. When a sort key isn't unique (many users could share a score), append the id to keep every key distinct:

```rust
// build a SortedMap key that sorts by score. the big-endian score makes byte
// order match numeric order; the user id is appended so two users with the
// same score still get distinct keys.
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
```

With xitdb you "bring your own index". It takes a bit more effort than the declarative convenience of SQL databases, but it gives you more explicit control, and avoids the common problem in SQL where queries silently become inefficient due to not using indexes. In xitdb, inefficiency is hard to miss because you are always writing your queries as imperative code and the indexes are always explicit.

## Large Byte Arrays

When reading and writing large byte arrays, you probably don't want to have all of their contents in memory at once. To incrementally write to a byte array, just get a writer from a cursor:

```rust
let text_cursor = moment.put_cursor("long-text")?;
let mut buf = [0u8; 1024];
let mut writer = text_cursor.writer(&mut buf)?;
for _ in 0..50 {
    writer.write_bytes(b"hello, world!\n")?;
}
writer.set_format_tag(*b"tx");
writer.finish()?; // remember to call this!
```

If you need to set a format tag for the byte array, call `set_format_tag` on the writer before you call `finish`.

To read a byte array incrementally, get a reader from a cursor:

```rust
let cursor = moment.get("long-text")?.ok_or(Error::KeyNotFound)?;
let mut rbuf = [0u8; 1024];
let mut reader = cursor.reader(&mut rbuf)?;
let bytes = reader.read_all()?;
// `bytes` holds all 50 lines; stream it in `rbuf`-sized pieces instead
// when the value doesn't fit in memory.
```

## Iterators

All data structures support iteration. Here's an example of iterating over an `ArrayList` and printing all of the keys and values of each `HashMap` contained in it:

```rust
let people = ArrayList::new(moment.get("people")?.ok_or(Error::KeyNotFound)?)?;
let mut people_walker = people.try_iter()?;
while let Some(person) = people_walker.next() {
    let fields = HashMap::new(person?)?;
    let mut fields_walker = fields.try_iter()?;
    while let Some(kv_cursor) = fields_walker.next() {
        let kv = kv_cursor?.read_key_value_pair()?;
        let key = kv.key.read_bytes_alloc()?;
        match kv.value.slot()?.tag {
            Tag::Bytes | Tag::ShortBytes => {
                println!("{}: {}", String::from_utf8_lossy(&key), String::from_utf8_lossy(&kv.value.read_bytes_alloc()?));
            }
            Tag::Uint => println!("{}: {}", String::from_utf8_lossy(&key), kv.value.read_uint()?),
            Tag::Int => println!("{}: {}", String::from_utf8_lossy(&key), kv.value.read_int()?),
            Tag::Float => println!("{}: {}", String::from_utf8_lossy(&key), kv.value.read_float()?),
            _ => return Err(Error::UnexpectedTag),
        }
    }
}
```

The above code iterates over `people`, which is an `ArrayList`, and for each person (which is a `HashMap`), it iterates over each of its key-value pairs.

The iteration of the `HashMap` looks the same with `HashSet`, `CountedHashMap`, and `CountedHashSet`. When iterating, you call `read_key_value_pair` on the cursor and can read the `key` and `value` cursors from it. In maps, `put` sets the value and `put_key` sets the key (see the tests for examples). In sets, there is only `put` and it sets the key; the value will always have a tag of `None`.

`ArrayList` and `LinkedArrayList` also have an `iter_from` method, which starts the iterator from the given index. `SortedMap` and `SortedSet` have `iter_from` and `iter_from_index` to start the iterator from a key or index respectively. This is especially useful for pagination: you can seek straight to the start of a page and walk forward only as far as you need. See the [Sorting and Paginating](#sorting-and-paginating) section for an example.

## Hashing

Hashing is never done by xitdb itself. The `Sha1Hasher` used in the above examples is part of the library, and you can plug in your own by implementing the `Hasher` trait (`digest_len`, `digest`, `id`):

```rust
assert_eq!(Sha1Hasher.digest_len(), 20);
assert_eq!(Sha1Hasher.digest(b"foo").len(), 20);
```

When initializing a database, you only tell xitdb the size of the hash via the hasher. If you're using SHA-1, this will be 20 bytes, stored in the database's header. If you try opening it later with the wrong hash size, it will return an error. If you are unsure what hash size the database uses, this creates a chicken-and-egg problem. You can read the header before initializing the database like this:

```rust
let bytes = committed_bytes(&db)?;
let header = Header::from_bytes(bytes[..HEADER_LEN].try_into()?);
assert_eq!(header.hash_size, 20);
```

The hash size alone does not disambiguate hashing algorithms, though. In addition, xitdb reserves four bytes in the header that you can use to put the name of the algorithm. You must provide it in the init options:

```rust
InitOpts { hash_id: Some(bytes_to_id(*b"sha1")), file_id: None, fsync: false }
```

The hash id is only written to the database header when it is first initialized. When you open it later, that init option is ignored. You can read the hash id of an existing database from its header, and convert it back with `id_to_bytes`:

```rust
assert_eq!(header.hash_id, bytes_to_id(*b"sha1"));
assert_eq!(id_to_bytes(bytes_to_id(*b"sha1")), *b"sha1");
```

If you want to use SHA-256, I recommend using `sha2` as the hash id. You can then distinguish between SHA-256 and SHA-512 using the hash size, matching on the header like this:

```rust
enum HashAlgo { Sha1, Sha256, Sha512 }

let algo = match header.hash_id {
    id if id == bytes_to_id(*b"sha1") => HashAlgo::Sha1,
    id if id == bytes_to_id(*b"sha2") => match header.hash_size {
        32 => HashAlgo::Sha256,
        64 => HashAlgo::Sha512,
        _ => return Err(Error::InvalidHashSize),
    },
    _ => return Err(Error::InvalidHashAlgo),
};
```

## Compaction

Normally, an immutable database grows forever, because old data is never deleted. To reclaim disk space and clear the history, xitdb supports compaction. This involves completely rebuilding the database file to only contain the data accessible from the latest copy (i.e., "moment") of the database:

```rust
let mut offsets = MemoryOffsetMap::new();
let compacted = db.compact(
    MemoryCore::new(),
    Sha1Hasher,
    InitOpts { hash_id: None, file_id: Some(2), fsync: false },
    &mut offsets,
)?;

// the compacted db holds one moment instead of three
let cursor = compacted.root_cursor();
let history = ArrayList::new_write(&cursor)?;
assert_eq!(history.count()?, 1);
```

This compacted database is a separate database. If you want to delete the original database and replace it with this one, you'll need to do that yourself. The target must be a new or empty database, so compaction returns `Error::TargetNotEmpty` if it already has data.

The offsets map records where each copied object lives in the compacted database so shared references and cycles point to the same copied object. It grows with the number of live objects copied, so it could theoretically OOM. To avoid this, you can instead use a file-backed xitdb map to track the offsets. Run the compaction inside a single transaction on a scratch database, so the offsets aren't each committed separately:

```rust
let scratch = open_mem()?;
let cell: RefCell<Option<Database<MemoryCore, Sha1Hasher>>> = RefCell::new(None);
scratch.run_context(|c| {
    let mut file_offsets = FileOffsetMap::new(c)?;
    let rebuilt = db.compact(
        MemoryCore::new(),
        Sha1Hasher,
        InitOpts { hash_id: None, file_id: Some(3), fsync: false },
        &mut file_offsets,
    )?;
    *cell.borrow_mut() = Some(rebuilt);
    Ok(())
})?;
```

You may be wondering if there is a way to evict certain pieces of data without losing all of your database's history. For example, imagine your moment contains a list of people:

```json
{
  "people": [
    {"name": "Alice", "age": 25},
    {"name": "Bob", "age": 42}
  ]
}
```

If Alice asks you to delete her data, you can remove her from the latest moment and compact the database. Her data will be gone, but so will all of your past moments, including every past version of Bob.

It's important to realize that the top-level history ArrayList isn't the only place you can keep history. It's the only one compaction discards, so you are free to track history of specific things within your moment. The `people` ArrayList can contain ArrayLists tracking the history of changes for each person individually.

Additionally, you can have a "global" internal history ArrayList (`versions` below), where each item is a list of integers that are indexes into each person's history. Here is what the moment looks like after Bob turns 43:

```json
{
  "people": [
    [{"name": "Alice", "age": 25}],
    [{"name": "Bob", "age": 42}, {"name": "Bob", "age": 43}]
  ],
  "versions": [
    [0, 0],
    [0, 1]
  ]
}
```

Each item in `versions` is a snapshot of the database, with one integer per person that is an index into their list. For example, the last version says that Alice is `people[0][0]` and Bob is `people[1][1]`.

Compaction can't follow an integer, so `people` is the only way to reach Alice's data. To evict her, you just clear her slot (write `Writeable::None` over it), compact the database, then delete the original. Every past version of Bob will still be there, but you'll need to handle the integers that no longer lead anywhere when reading a version.

## Replication

Since xitdb files are append-only, it is absurdly easy to replicate them: just copy the bytes that the destination doesn't have yet! Other databases like SQLite require complex external tools to replicate. In xitdb, once a transaction is committed, its bytes never change, except for a small header that points at the latest commit. That makes it easy to keep a copy of a database (a "replica") up to date, whether it's another file on the same machine or on another machine across a network.

A sync takes four steps:

```rust
// 1. on the replica: describe what it already has
let state = replica.replica_state()?;
// 2. on the source: decide which bytes to send
let start = source.replication_start(state)?;
let end = source.committed_size()?;
// 3. on the source: write those bytes
let mut buf = Vec::new();
source.read_committed(start, end, &mut buf)?;
// 4. on the replica: apply them
let len = end.checked_sub(start).ok_or(Error::Corrupted)?;
replica.apply_replicated(start, &mut &buf[..], len)?;
```

Over a network, the replica sends its state (its committed size and a checksum of its latest commit), and the source replies with `start`, the length, and then the bytes. Since `read_committed` takes a writer and `apply_replicated` takes a reader, the bytes can be streamed directly over the connection without buffering them in memory.

A replica can start out as any new database. Each database has a random file ID, so the first sync sees that the replica isn't a copy of the source and sends the entire source file, replacing the replica. The same happens if the replica can no longer be extended, such as when the source is replaced by a compacted copy (which is a new file) or restored from an older backup: `replication_start` returns 0, and the replica is replaced with a full copy.

If a sync is interrupted, such as by a dropped connection, the replica stays at its last commit, and the next sync discards whatever was partially written. The exception is when `start` is 0: the replica is truncated before the copy begins, so an interrupted copy leaves it unusable until the next sync succeeds. If that matters to you, check whether `start` is 0, and if so, write the copy to a new file and only replace the old replica after `apply_replicated` succeeds.

Only change a replica with `apply_replicated`. If you write to it directly, it no longer matches the source, and the next sync will replace it.

## Thread Safety

It is possible to read a database from multiple threads without locks, even while writes are happening. This is a big benefit of immutable databases. However, each thread needs to use its own `Database` instance. Also, keep in mind that writes still need to come from one thread at a time; see the example at the top of this file, where it acquires an exclusive file lock.

## Benchmarks

`benches/ops.rs` measures the hot paths with [Criterion](https://github.com/bheisler/criterion.rs):

```bash
cargo bench
```

It covers `HashMap` puts and gets, `ArrayList` appends and iteration, `SortedMap` puts and page walks, whole-transaction commits, and file-backed write throughput — all on fixed workloads so runs are comparable over time. Criterion is a dev-dependency only; the library itself still has zero dependencies.

## Compatibility

`tests/fixtures/` holds databases written by other implementations (`test.db` vendored from xitdb-go, `go-golden.db` generated by `scripts/gen-fixtures.sh`), and `tests/compat.rs` proves they read identically here. The same script has Go read back Rust-written files. `tests/readme.rs` executes every snippet in this file.

## Examples

```bash
cargo run --example dump_database -- [--json] <file.db>
cargo run --example rest_server # PUT/GET/DELETE localhost:3000/kv/:key
```

## Development

```bash
cargo test
cargo bench --no-run # benches compile; `cargo bench` runs them
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## License

MIT. See [LICENSE](LICENSE).
