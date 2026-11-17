# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-11-17

- Database format version 2 (matches upstream Zig
  [torn-header fix](https://github.com/xit-vcs/xitdb/commit/bdaa55e3326ecdf9e367a789a59ee9ba93c40cf0)):
  the root header's file size now lives at the 8-byte-aligned offset 24,
  so a concurrent read cannot see it half-written. New files are version 2
  (32-byte headers, 57-byte empty database); version 1 files (37-byte
  headers, 62-byte empty database) are still fully supported for reading,
  writing, and replication, keeping their layout. `ROOT_HEADER_END` is now
  32, with `FILE_SIZE_START_V1`/`ROOT_HEADER_END_V1` and
  `file_size_start(version)`/`data_start(version)`/`min_committed(version)`
  for the version-dependent offsets; `write_root_header` only rewrites the
  file size, and `apply_replicated` stages full copies with an empty root
  header until the data arrives (interrupted copies recover on next sync).
- Async integration guide in the crate docs (sync API + `spawn_blocking`
  pattern, no async wrapper by design)

## [0.2.0] - 2026-11-05

- Collections are single mode-generic structs (`ArrayList<'db, C, H, Mode>`,
  etc.): the `Write*` wrappers, `Deref` impls, and `Read*` aliases are
  removed — **breaking change**, will ship as 0.2.0. `WriteX::new_write`
  becomes `X::new_write`, `ReadX::new` becomes `X::new`; write methods live
  in `impl ReadWrite` blocks. Same paths, same file bytes.
- README rewritten to mirror the Zig original's structure and narrative,
  with Rust snippets for every section, each executed by `tests/readme.rs`
- Criterion benchmarks (`cargo bench`) for puts, gets, iteration,
  pagination, commits, and file-backed writes (dev-dependency only)

## [0.1.0] - 2026-11-05

Initial release: Rust port of [xit-vcs/xitdb](https://github.com/xit-vcs/xitdb),
following [xitdb-go](https://github.com/xit-vcs/xitdb-go) as the idiomatic-port model.

- Immutable, embedded database: every transaction cheaply creates a new copy
  while past copies stay readable and revertible. No query engine.
- Collections: `HashMap`/`HashSet` (+ counted variants), `ArrayList`,
  `LinkedArrayList` (B-tree), `SortedMap`/`SortedSet` (B-tree), scalars
  (`Uint`/`Int`/`Float`/`Bytes` with optional format tags).
- Backends: `MemoryCore`, `FileCore`, `BufferedFileCore`. Sync-only API;
  the caller serializes writes with an exclusive file lock.
- Byte-compatible files with the Zig, Go, TypeScript, Java, and Clojure
  implementations when opened with the same hash.
- Compaction (`compact`) and replication (`replica_state` /
  `replication_start` / `read_committed` / `apply_replicated`).
- Zero dependencies: SHA-1 and OS randomness are vendored in-tree.
- MSRV 1.85.
