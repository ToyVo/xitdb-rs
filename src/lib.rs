//! `xitdb`: an immutable, embedded database (Rust port of `xit-vcs/xitdb`).
//!
//! Every transaction efficiently creates a new copy of the database while
//! past copies stay readable. There is no query engine: you nest `ArrayList`,
//! `HashMap`, and friends like in-memory data structures, on disk or in memory.
//!
//! # Writes and locking
//!
//! Writes must be serialized by the caller with an exclusive file lock
//! (reads need none; each thread uses its own [`database::Database`]).
//! With the `fs2` crate that looks like this (read-only opens need no lock):
//!
//! ```no_run
//! use std::fs::OpenOptions;
//! use xitdb::core::FileCore;
//! use xitdb::database::{Database, InitOpts};
//! use xitdb::hasher::Sha1Hasher;
//!
//! let file = OpenOptions::new().read(true).write(true).create(true).open("main.db").unwrap();
//! // fs2::FileExt::lock_exclusive(&file).unwrap(); // <-- writing: lock first
//! let db = Database::open(FileCore::new(file, true), Sha1Hasher,
//!     InitOpts { hash_id: None, file_id: None, fsync: true }).unwrap();
//! ```
//!
//! # Cloning within a transaction
//!
//! Data written earlier in the *same* transaction is temporarily mutable
//! for speed. Cloning it into a second structure and then writing through
//! the clone mutates the source too — unless the cloning cursor is frozen
//! first with [`cursor::Cursor::freeze`] (or [`database::Database::freeze`]
//! for the whole transaction). Reads through other cursors are unaffected.
//!
//! # File compatibility
//!
//! Files are byte-compatible with the Zig, Go, TypeScript, Java, and
//! Clojure implementations when opened with the same hash (see
//! [`header::Header`]): the same magic, version, slots, tags, and node
//! layouts. `tests/fixtures/` holds databases written by other
//! implementations; `tests/compat.rs` proves they read identically here.

pub mod arraylist;
pub(crate) mod codec;
pub mod core;
pub mod counted_hashmap;
pub mod counted_hashset;
pub mod cursor;
pub mod database;
pub mod error;
pub mod hasher;
pub mod hashmap;
pub mod hashset;
pub mod header;
pub mod linked_arraylist;
pub mod offset_map;
pub(crate) mod random;
pub(crate) mod sha1;
pub mod slot;
pub mod sorted_map;
pub mod sorted_set;
pub mod tag;
