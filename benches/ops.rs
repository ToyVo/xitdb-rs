//! Hot-path benchmarks (memory-backed unless noted). Criterion is a
//! dev-dependency only; the library itself has zero dependencies.
//!
//! Run with `cargo bench`. CI only compile-checks these
//! (`cargo bench --no-run`).
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use xitdb::arraylist::ArrayList;
use xitdb::core::{BufferedFileCore, MemoryCore};
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::hasher::Sha1Hasher;
use xitdb::hashmap::HashMap;
use xitdb::sorted_map::SortedMap;

fn bench_db(file_id: u64) -> Option<Database<MemoryCore, Sha1Hasher>> {
    Database::open(
        MemoryCore::new(),
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(file_id),
            fsync: false,
        },
    )
    .ok()
}

fn hashmap_put(c: &mut Criterion) {
    c.bench_function("hashmap_put_1000", |b| {
        b.iter(|| {
            let Some(db) = bench_db(1) else {
                eprintln!("bench setup failed");
                return;
            };
            let outcome = db.run_context(|w| {
                let map = HashMap::new_write(w)?;
                for i in 0..1000u64 {
                    map.put(&format!("key-{i}"), Writeable::Uint(i))?;
                }
                Ok(())
            });
            if outcome.is_err() {
                eprintln!("bench transaction failed");
            }
        });
    });
}

fn hashmap_get(c: &mut Criterion) {
    let Some(db) = bench_db(2) else {
        eprintln!("bench setup failed");
        return;
    };
    let seeded = db.run_context(|w| {
        let map = HashMap::new_write(w)?;
        for i in 0..10_000u64 {
            map.put(&format!("key-{i}"), Writeable::Uint(i))?;
        }
        Ok(())
    });
    if seeded.is_err() {
        eprintln!("bench seed failed");
        return;
    }
    let keys: Vec<String> = (0..100u64).map(|i| format!("key-{i}")).collect();
    c.bench_function("hashmap_get_100", |b| {
        b.iter(|| {
            let cursor = db.root_cursor();
            let map = match HashMap::new_write(&cursor) {
                Ok(map) => map,
                Err(error) => {
                    eprintln!("bench open failed: {error}");
                    return;
                }
            };
            for key in &keys {
                black_box(map.get(key).ok());
            }
        });
    });
}

fn arraylist_append(c: &mut Criterion) {
    c.bench_function("arraylist_append_1000", |b| {
        b.iter(|| {
            let Some(db) = bench_db(3) else {
                eprintln!("bench setup failed");
                return;
            };
            let outcome = db.run_context(|w| {
                let list = ArrayList::new_write(w)?;
                for i in 0..1000u64 {
                    list.append(Writeable::Uint(i))?;
                }
                Ok(())
            });
            if outcome.is_err() {
                eprintln!("bench transaction failed");
            }
        });
    });
}

fn arraylist_iterate(c: &mut Criterion) {
    let Some(db) = bench_db(4) else {
        eprintln!("bench setup failed");
        return;
    };
    let seeded = db.run_context(|w| {
        let list = ArrayList::new_write(w)?;
        for i in 0..10_000u64 {
            list.append(Writeable::Uint(i))?;
        }
        Ok(())
    });
    if seeded.is_err() {
        eprintln!("bench seed failed");
        return;
    }
    c.bench_function("arraylist_iterate_10000", |b| {
        b.iter(|| {
            let cursor = db.root_cursor();
            let list = match ArrayList::new_write(&cursor) {
                Ok(list) => list,
                Err(error) => {
                    eprintln!("bench open failed: {error}");
                    return;
                }
            };
            let walker = match list.try_iter() {
                Ok(walker) => walker,
                Err(error) => {
                    eprintln!("bench iterate failed: {error}");
                    return;
                }
            };
            for entry in walker {
                black_box(entry.ok());
            }
        });
    });
}

fn sorted_put(c: &mut Criterion) {
    c.bench_function("sorted_put_500", |b| {
        b.iter(|| {
            let Some(db) = bench_db(5) else {
                eprintln!("bench setup failed");
                return;
            };
            let outcome = db.run_context(|w| {
                let map = SortedMap::new_write(w)?;
                for i in 0..500u64 {
                    map.put(&format!("user-{i:05}"), Writeable::Uint(i))?;
                }
                Ok(())
            });
            if outcome.is_err() {
                eprintln!("bench transaction failed");
            }
        });
    });
}

fn sorted_paginate(c: &mut Criterion) {
    let Some(db) = bench_db(6) else {
        eprintln!("bench setup failed");
        return;
    };
    let seeded = db.run_context(|w| {
        let map = SortedMap::new_write(w)?;
        for i in 0..10_000u64 {
            map.put(&format!("user-{i:05}"), Writeable::Uint(i))?;
        }
        Ok(())
    });
    if seeded.is_err() {
        eprintln!("bench seed failed");
        return;
    }
    c.bench_function("sorted_paginate_100", |b| {
        b.iter(|| {
            let cursor = db.root_cursor();
            let map = match SortedMap::new_write(&cursor) {
                Ok(map) => map,
                Err(error) => {
                    eprintln!("bench open failed: {error}");
                    return;
                }
            };
            let mut walker = match map.iter_from_index(5000) {
                Ok(walker) => walker,
                Err(error) => {
                    eprintln!("bench seek failed: {error}");
                    return;
                }
            };
            for _ in 0..100u64 {
                let Some(entry) = walker.next() else { break };
                black_box(entry.ok());
            }
        });
    });
}

fn commit_moment(c: &mut Criterion) {
    c.bench_function("commit_moment", |b| {
        b.iter(|| {
            let Some(db) = bench_db(7) else {
                eprintln!("bench setup failed");
                return;
            };
            let outcome = db.run_context(|w| {
                let moment = HashMap::new_write(w)?;
                for i in 0..10u64 {
                    moment.put(&format!("key-{i}"), Writeable::Uint(i))?;
                }
                let list_cursor = moment.put_cursor("list")?;
                let list = ArrayList::new_write(&list_cursor)?;
                for i in 0..10u64 {
                    list.append(Writeable::Uint(i))?;
                }
                Ok(())
            });
            if outcome.is_err() {
                eprintln!("bench transaction failed");
            }
        });
    });
}

fn file_write_throughput(c: &mut Criterion) {
    let path = std::env::temp_dir().join("xitdb-bench-file.db");
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("bench setup failed: {error}");
            return;
        }
    };
    let buffered = match BufferedFileCore::new(file, false) {
        Ok(core) => core,
        Err(error) => {
            eprintln!("bench setup failed: {error}");
            return;
        }
    };
    let db = match Database::open(
        buffered,
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(8),
            fsync: false,
        },
    ) {
        Ok(db) => db,
        Err(error) => {
            eprintln!("bench setup failed: {error}");
            return;
        }
    };
    c.bench_function("file_write_100", |b| {
        b.iter(|| {
            let outcome = db.run_context(|w| {
                let map = HashMap::new_write(w)?;
                for i in 0..100u64 {
                    map.put(&format!("key-{i}"), Writeable::Uint(i))?;
                }
                Ok(())
            });
            if outcome.is_err() {
                eprintln!("bench transaction failed");
            }
        });
    });
    let _ = std::fs::remove_file(&path);
}

criterion_group!(
    benches,
    hashmap_put,
    hashmap_get,
    arraylist_append,
    arraylist_iterate,
    sorted_put,
    sorted_paginate,
    commit_moment,
    file_write_throughput,
);
criterion_main!(benches);
