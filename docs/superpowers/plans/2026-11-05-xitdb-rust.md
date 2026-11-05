# xitdb Rust Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build idiomatic Rust crate `xitdb` with byte-compatible file format and full API parity.

**Architecture:** Layered port of `xit-vcs/xitdb` (Zig) following `xitdb-go` module layout; `Database<C: Core, H: Hasher>` generic with `Cursor<ReadOnly|ReadWrite>` typestate, thin collection wrappers, closure transactions, trait-based offset maps.

**Tech Stack:** Rust edition 2024, MSRV 1.85, sync-only; deps `sha1` (default feature) + `getrandom`; `cargo test + clippy + fmt`.

**Spec:** `docs/superpowers/specs/2026-11-05-xitdb-rust-design.md`

## Global Constraints

- Edition 2024, MSRV 1.85, sync-only I/O.
- Deps limited to `sha1` (default feature) + `getrandom`; error type hand-rolled, no `thiserror`.
- File bytes fixed: magic `'xit'`, `VERSION=1`, 20-byte header, 9-byte slot, tag order `None=0..SortedKVPair=16`, all ints big-endian.
- Locking caller-owned (docs show `fs2`); one writer at a time, readers use own `Database` instance.
- No query engine, no async, no serde layer, no network transport.
- Every task ends green (`cargo test`) + committed; clippy/fmt clean at Task 7.

## Review Focus

- Corrupt/truncated file opened: expect `Err(InvalidDatabase|Truncated|ChecksumMismatch)`, never panic — pinned in Task 2.
- DB created with sha1 opened with wrong hash size/id: expect `Err(InvalidHashSize)` on open — pinned in Task 1.
- Write through cursor from expired transaction: expect `Err(ExpiredTransaction)` — pinned in Task 2.
- Clone-then-mutate within same tx without `freeze`: source must stay unchanged only after `freeze`, test asserts both behaviors — pinned in Task 3.
- Interrupted full-copy replication (`start==0` truncated mid-stream): replica unusable until next successful sync, test asserts error on read + recovery — pinned in Task 6.

---

## File Structure

- `Cargo.toml` — crate `xitdb`, edition 2024, features `sha1` (default), deps `sha1`, `getrandom`.
- `src/lib.rs` — re-exports only.
- `src/error.rs` — `Error` enum + `Result<T>` alias.
- `src/slot.rs` — `Slot` + 9-byte codec.
- `src/tag.rs` — `Tag` repr + validate.
- `src/header.rs` — `Header` + 20-byte codec.
- `src/core.rs` — `Core` trait + `MemoryCore`/`FileCore`/`BufferedFileCore`.
- `src/hasher.rs` — `Hasher` trait + `Sha1Hasher` + id helpers.
- `src/database.rs` — `Database`, `InitOpts`, `ReplicaState`, tx/freeze bookkeeping, replica + compact entry points.
- `src/cursor.rs` — `Cursor<Mode>`, `ReadCursor`/`WriteCursor` aliases, scalar/bytes/path/streaming APIs, `Reader`/`Writer`.
- `src/hashmap.rs`, `src/hashset.rs`, `src/counted_hashmap.rs`, `src/counted_hashset.rs`, `src/arraylist.rs` — HAMT collections.
- `src/linked_arraylist.rs`, `src/sorted_map.rs`, `src/sorted_set.rs` — B-tree collections + iterators.
- `src/offset_map.rs` — `OffsetMap` + `MemoryOffsetMap` + `FileOffsetMap`.
- `tests/high_level.rs`, `tests/low_level.rs` — ported suites.
- `tests/compat.rs` + `tests/fixtures/*` + `scripts/gen-fixtures.sh` — interop harness.
- `examples/dump-database.rs` — mirrors Go example.

Reference algorithms: HAMT logic in `/tmp/xitdb-go/database.go` (index/array/hash sections) + `/tmp/xitdb-upstream/src/lib.zig`; B-tree logic in same files (linked/sorted sections). Tasks cite sections, not bodies.

---

### Task 1: Foundation — errors, slot/tag/header, Core, Hasher

**Files:**
- Create: `Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/slot.rs`, `src/tag.rs`, `src/header.rs`, `src/core.rs`, `src/hasher.rs`
- Test: inline `#[cfg(test)]` units in each module

**Interfaces:**
- Consumes: none.
- Produces:
  - `pub type Result<T> = Result<T, Error>` in `error.rs`; `Error::{InvalidDatabase, InvalidVersion, UnexpectedTag, Corrupted, ExpiredTransaction, NestedTopLevelWrite, ExpectedTxStart, TargetNotEmpty, InvalidHashSize, Truncated, ChecksumMismatch, Io}` + `From<io::Error>`.
  - `pub struct Slot { pub value: i64, pub tag: Tag, pub full: bool }` + `Slot::empty()`, `to_bytes() -> [u8;9]`, `from_bytes([u8;9]) -> Slot`; `pub const SLOT_LEN: usize = 9`.
  - `#[repr(u8)] pub enum Tag { None=0, Index=1, ArrayList=2, LinkedArrayList=3, HashMap=4, KVPair=5, Bytes=6, ShortBytes=7, Uint=8, Int=9, Float=10, HashSet=11, CountedHashMap=12, CountedHashSet=13, SortedMap=14, SortedSet=15, SortedKVPair=16 }` + `validate() -> Result<()>`.
  - `pub struct Header { pub file_id: u64, pub hash_id: u32, pub hash_size: u16, pub version: u16, pub magic: [u8;3] }` + `HEADER_LEN=20`, `VERSION=1`, `MAGIC=[0x78,0x69,0x74]`, `to_bytes/from_bytes/validate`.
  - `pub trait Core { fn read(&mut self, buf: &mut [u8]) -> Result<()>; fn write(&mut self, buf: &[u8]) -> Result<()>; fn committed_len(&mut self) -> Result<u64>; fn seek_to(&mut self, pos: u64) -> Result<()>; fn position(&mut self) -> Result<u64>; fn truncate(&mut self, len: u64) -> Result<()>; fn flush(&mut self) -> Result<()>; fn sync(&mut self) -> Result<()>; }` + `MemoryCore::new()`, `FileCore::new(File, fsync: bool)`, `BufferedFileCore::new(File, fsync: bool)` (fsync flag controls whether commit calls `sync`, mirroring Zig `InitOpts.fsync`).
  - `pub trait Hasher { fn digest_len(&self) -> usize; fn digest(&self, data: &[u8]) -> Vec<u8>; fn id(&self) -> u32; }` + `Sha1Hasher`, `bytes_to_id([u8;4]) -> u32`, `id_to_bytes(u32) -> [u8;4]`.

- [ ] **Step 1: Write failing unit tests** in `slot.rs`/`tag.rs`/`header.rs`/`core.rs`/`hasher.rs`: slot 9-byte round-trip with `full` high-bit, tag discriminants `None=0..SortedKVPair=16` + `validate` rejects 17, header 20-byte round-trip + `validate` rejects bad magic/version, `MemoryCore` read/write/seek/len round-trip, `Sha1Hasher::digest_len()==20` + `id()==bytes_to_id(*b"sha1")`, wrong-hash-size open expectation (header with `hash_size=99` fails `validate` path used by Task 2).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib 2>&1 | tail -20`
Expected: FAIL — modules not found.

- [ ] **Step 3: Implement `Cargo.toml` + the seven modules** with the exact signatures above. Codec rule: tag byte = `tag as u8 | (0x80 if full)`, value = BE `u64`; header field order magic(3)+pad(1)+version(u16)+hash_size(u16)+hash_id(u32)+file_id(u64), all BE.

- [ ] **Step 4: Run units to verify pass**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: PASS, 0 failures.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/ && git commit -m "feat: foundation errors slot tag header core hasher"
```

### Task 2: Database open + transactions + freeze bookkeeping

**Files:**
- Create: `src/database.rs`
- Test: `tests/smoke_db.rs` (temp, deleted in Task 7) or inline integration `tests/db_tx.rs`

**Interfaces:**
- Consumes: `Core`, `Hasher`, `Header`, `Slot`, `Error/Result` from Task 1.
- Produces:
  - `pub struct InitOpts { pub hash_id: Option<u32>, pub file_id: Option<u64>, pub fsync: bool }`
  - `pub struct ReplicaState { pub committed_size: u64, pub checksum: u64 }`
  - `impl<C: Core, H: Hasher> Database<C,H> { pub fn open(core: C, hasher: H, opts: InitOpts) -> Result<Self>; pub fn root_cursor(&self) -> WriteCursor<'_,C,H>; pub fn run_context(&self, f: impl FnOnce(&WriteCursor<C,H>) -> Result<()>) -> Result<()>; pub fn freeze(&self) -> Result<()>; pub fn committed_size(&self) -> Result<u64>; }`
  - Runtime errors `ExpiredTransaction`, `NestedTopLevelWrite`, `ExpectedTxStart` usable by cursors.

- [ ] **Step 1: Write failing test** `tests/db_tx.rs`: open memory DB, `run_context` writing root slot then rollback on `Err` leaves `committed_size` unchanged; write via expired cursor expects `ExpiredTransaction`; nested top-level `write_path` inside `run_context` expects `NestedTopLevelWrite`; truncated-file open expects `Err`; corrupt-magic open expects `InvalidDatabase`.

```rust
#[test]
fn rollback_leaves_committed_size_unchanged() { /* open, record size, run_context returning Err, assert size equal */ }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test db_tx 2>&1 | tail -10`
Expected: FAIL — `database.rs` missing.

- [ ] **Step 3: Implement `Database::open/run_context/freeze/committed_size`** + interior-mutability tx state (`RefCell` inner holding core position, active-tx flag, frozen set). `open`: empty core writes header (random `file_id` via `getrandom` when `None`); non-empty validates magic/version/hash. Trailer + commit-pointer update on successful closure only; `Err` truncates to entry size.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --test db_tx 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs tests/db_tx.rs && git commit -m "feat: database open transactions freeze bookkeeping"
```

### Task 3: Cursor scalar/bytes/path/streaming APIs

**Files:**
- Create: `src/cursor.rs`
- Test: `tests/cursor_api.rs`

**Interfaces:**
- Consumes: `Database::root_cursor/run_context` from Task 2.
- Produces:
  - `pub struct ReadOnly; pub struct ReadWrite; pub struct Cursor<'db,C: Core,H: Hasher,Mode> {...}` + `pub type ReadCursor<'db,C,H> = Cursor<'db,C,H,ReadOnly>` + `pub type WriteCursor<'db,C,H> = Cursor<'db,C,H,ReadWrite>`.
  - Reads (both modes): `read_uint() -> Result<u64>`, `read_int() -> Result<i64>`, `read_float() -> Result<f64>`, `read_bytes(&self, buf: &mut [u8]) -> Result<Vec<u8>>`, `read_bytes_alloc(&self) -> Result<Vec<u8>>`, `read_bytes_object(&self, buf: &mut [u8]) -> Result<BytesObject>`, `read_key_value_pair(&self) -> Result<KeyValuePair>`, `slot() -> Result<Slot>`, `count() -> Result<u64>`.
  - Writes (`WriteCursor` only): `write(&self, data: Writeable) -> Result<()>`, `write_if_empty`, `freeze(&self) -> Result<()>`, `reader(&self, buf: &mut [u8]) -> Result<Reader>`, `writer(&self, buf: &mut [u8]) -> Result<Writer>` (+ `Writer::finish()`), `write_path(&self, path: &[PathPart]) -> Result<WriteCursor>`, `read_path(&self, path: &[PathPart]) -> Result<Option<ReadCursor>>`.
  - `pub enum Writeable { Slot(Slot), Bytes(Vec<u8>), BytesObject { value: Vec<u8>, format_tag: Option<[u8;2]> }, Uint(u64), Int(i64), Float(f64), None }`, `pub struct BytesObject { pub value: Vec<u8>, pub format_tag: Option<[u8;2]> }`, `pub enum PathPart { ... }` mirroring Go.

- [ ] **Step 1: Write failing tests**: uint/int/float round-trip, bytes + format-tag round-trip, `writer` streaming 50×"hello\n" then `reader` line-count==50, `write_path`/`read_path` nesting, same-tx clone without `freeze` mutates source AND with `freeze` preserves source (the Review-Focus freeze test).

- [ ] **Step 2: Run to verify fail**

Run: `cargo test --test cursor_api 2>&1 | tail -10`
Expected: FAIL — `cursor.rs` missing.

- [ ] **Step 3: Implement `cursor.rs`** per signatures; slot-codec read/write paths against `Core` at resolved offsets; streaming `Reader`/`Writer` buffer the `&mut [u8]` scratch like Go; `freeze` records cursor position in DB frozen set so later writes copy-on-write.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --test cursor_api --test db_tx 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs tests/cursor_api.rs && git commit -m "feat: cursor scalar bytes path streaming"
```

### Task 4: HAMT collections (hash maps/sets, array list)

**Files:**
- Create: `src/hashmap.rs`, `src/hashset.rs`, `src/counted_hashmap.rs`, `src/counted_hashset.rs`, `src/arraylist.rs`
- Test: `tests/hamt.rs`

**Interfaces:**
- Consumes: `ReadCursor/WriteCursor`, `Writeable` from Task 3.
- Produces (each `X<'db,C,H,Mode>` with `new(cursor) -> Result<Self>`, `slot()`, `read_only()`; write impls on `Mode=ReadWrite` only):
  - `HashMap::get(&str)/get_by_bytes(&[u8])/get_by_hash(&[u8]) -> Result<Option<ReadCursor>>`, `put/put_cursor/remove`, `get_key_value_pair`, `iter()`.
  - `HashSet`: same minus values (`put` sets key, value tag always `None`).
  - `CountedHashMap/CountedHashSet`: + `count() -> Result<u64>`.
  - `ArrayList::{append(Writeable), append_cursor() -> Result<WriteCursor>, get(i64) -> Result<Option<ReadCursor>> (negative = from end), get_slot, put(i64,..), count, iter, iter_from(i64)}`.

- [ ] **Step 1: Write failing tests**: put/get/remove round-trip incl. bytes/hash variants, counted `count()` tracks inserts/deletes, array append 100 items + `get(-1)`==last + `iter` yields all, iter over map uses `read_key_value_pair`.

- [ ] **Step 2: Run to verify fail**

Run: `cargo test --test hamt 2>&1 | tail -8`
Expected: FAIL — modules missing.

- [ ] **Step 3: Implement the five modules** following `/tmp/xitdb-go/database.go` HAMT sections (index-bitmap, `BIT_COUNT=4` tiers) and `/tmp/xitdb-upstream/src/lib.zig` same sections; string keys hashed via `db.hasher.digest`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --test hamt --test cursor_api 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hashmap.rs src/hashset.rs src/counted_*.rs src/arraylist.rs tests/hamt.rs && git commit -m "feat: hamt collections hashmap hashset arraylist"
```

### Task 5: B-tree collections + iterators

**Files:**
- Create: `src/linked_arraylist.rs`, `src/sorted_map.rs`, `src/sorted_set.rs`
- Test: `tests/btree.rs`

**Interfaces:**
- Consumes: cursor + HAMT patterns from Tasks 3–4.
- Produces:
  - `LinkedArrayList::{append, insert(i64,..), slice, concat, get, count, iter, iter_from(i64)}`.
  - `SortedMap::{put(&[u8], Writeable), get(&[u8]), remove(&[u8]), count, iter, iter_from(&[u8]), iter_from_index(i64)}`; `SortedSet` same with key-only values.
  - All iterators `impl Iterator<Item = Result<ReadCursor<'db,C,H>>>`.

- [ ] **Step 1: Write failing tests**: linked insert-at-0 ×N preserves order, slice/concat round-trip, sorted puts in arbitrary order iterate lexicographically, `iter_from("da")` type-ahead only yields `dan,dave` then stops, `iter_from_index` pagination page-500-seek correctness, `Iterator` trait usable in `for x in map.iter()`.

- [ ] **Step 2: Run to verify fail**

Run: `cargo test --test btree 2>&1 | tail -8`
Expected: FAIL — modules missing.

- [ ] **Step 3: Implement the three modules** following Go B-tree sections (`BTreeSlotCount/SplitCount`, leaf/branch headers) + Zig same sections; counts maintained for `iter_from_index` O(log n) seek.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --test btree --test hamt 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/linked_arraylist.rs src/sorted_*.rs tests/btree.rs && git commit -m "feat: btree linked sorted collections"
```

### Task 6: Compaction, offset maps, replication

**Files:**
- Create: `src/offset_map.rs`; modify `src/database.rs` (add `compact`, replica methods)
- Test: `tests/compact_repl.rs`

**Interfaces:**
- Consumes: all collections (reachability walk) from Tasks 4–5.
- Produces:
  - `pub trait OffsetMap { fn reset(&mut self) -> Result<()>; fn get(&mut self, src: u64) -> Result<Option<u64>>; fn put(&mut self, src: u64, dst: u64) -> Result<()>; }` + `MemoryOffsetMap` + `FileOffsetMap::new(cursor: WriteCursor) -> Result<Self>`.
  - `Database::compact<D: Core>(&self, target: D, hasher: H, opts: InitOpts, offsets: &mut impl OffsetMap) -> Result<Database<D,H>>` (errors `TargetNotEmpty`).
  - `Database::{replica_state() -> Result<ReplicaState>, replication_start(ReplicaState) -> Result<u64>, read_committed(start,end,&mut dyn Write), apply_replicated(start,&mut dyn Read,len), committed_size()}`.

- [ ] **Step 1: Write failing tests**: compact keeps latest moment only (`count()==1`) + preserves sharing/cycles; `MemoryOffsetMap` vs `FileOffsetMap` (in-scratch-tx) equivalence; replication incremental sync applies; full-copy on `file_id` mismatch; interrupted `start==0` copy leaves replica erroring until next sync succeeds (Review-Focus test).

- [ ] **Step 2: Run to verify fail**

Run: `cargo test --test compact_repl 2>&1 | tail -8`
Expected: FAIL — methods missing.

- [ ] **Step 3: Implement `offset_map.rs` + DB methods** following Go `database.go` compact/replica sections + `file-offset-map.go`; copy reachable objects once via offset map, new trailer + header; streaming replica bytes without buffering.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --test compact_repl 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/offset_map.rs src/database.rs tests/compact_repl.rs && git commit -m "feat: compaction offsetmap replication"
```

### Task 7: Ported suites, compat harness, example, CI polish

**Files:**
- Create: `tests/high_level.rs`, `tests/low_level.rs`, `tests/compat.rs`, `tests/fixtures/.gitkeep`, `scripts/gen-fixtures.sh`, `examples/dump-database.rs`
- Modify: `Cargo.toml` (example), delete temp `tests/db_tx.rs`/`cursor_api.rs`/`hamt.rs`/`btree.rs`/`compact_repl.rs` after porting (or keep as focused suites — keep, just add ported ones)
- Test: full `cargo test`

**Interfaces:**
- Consumes: everything above.
- Produces: green `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`; fixtures proving interop.

- [ ] **Step 1: Write failing ported tests**: port `/tmp/xitdb-go/high_level_test.go` + `low_level_test.go` case-for-case (history/array/hash/sorted/linked/counted, freeze/undo, overflow, non-ArrayList roots, `run_context` grouping) + `compat.rs` reading `tests/fixtures/*` goldens (fails: no fixtures, no suites).

- [ ] **Step 2: Run to verify fail**

Run: `cargo test 2>&1 | tail -10`
Expected: FAIL — suites/fixtures missing.

- [ ] **Step 3: Generate fixtures + implement `compat.rs`**

`scripts/gen-fixtures.sh` builds Zig + Go DBs (fixed `file_id` for byte-compare) into `tests/fixtures/`; `compat.rs` asserts Rust reads Zig/Go goldens semantically and documents the manual reverse-check (Go/Zig reading Rust outputs).

- [ ] **Step 4: Implement ported suites + example + docs**

Port `/tmp/xitdb-go/high_level_test.go` + `low_level_test.go` case-for-case into `tests/high_level.rs`/`tests/low_level.rs`; `examples/dump-database.rs` mirrors Go `cmd/dump-database`; crate docs show `fs2` locking + `freeze` rule.

- [ ] **Step 5: Run full verification**

Run: `cargo test 2>&1 | tail -5 && cargo clippy --all-targets -- -D warnings 2>&1 | tail -3 && cargo fmt --check 2>&1 | tail -3`
Expected: all PASS/clean.

- [ ] **Step 6: Commit**

```bash
git add tests/ examples/ scripts/ Cargo.toml && git commit -m "feat: ported suites compat harness example"
```
