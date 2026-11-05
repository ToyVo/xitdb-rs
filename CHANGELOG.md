# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
