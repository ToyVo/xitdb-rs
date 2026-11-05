# xitdb Rust Port — Design

Date: 2026-11-05
Status: draft, awaiting review
Approach: (2) Idiomatic core, Go-mirrored layout

## 1. Intent

Build an idiomatic Rust crate `xitdb`: an embedded immutable database
porting `xit-vcs/xitdb` (Zig reference) with API philosophy following
`xitdb-go` (idiomatic port, not transliteration).

- Outcome: library crate usable embedded in-process, alternative to
  SQLite for nested maps/lists with immutable history.
- Hard requirements: byte-compatible file format with Zig/Go/TS/Java
  impls (same files readable across impls given same hash algo);
  full parity (all collections, all backends, tx, compaction,
  replication); many integration tests proving the claims.
- Non-goals (YAGNI): no query engine, no async I/O, no serde/typed
  layer, no crate-owned file locking, no network transport for
  replication bytes (caller streams them).

## 2. Architecture & crate layout

Single crate `xitdb` (lib + integration tests + one example).
Edition 2024, MSRV 1.85, sync-only, deps limited to `sha1`
(default feature) + `getrandom` (for `file_id` generation).

`src/` (mirrors Go file layout for traceability):

- `lib.rs` — re-exports.
- `slot.rs`, `tag.rs`, `header.rs` — 9-byte slot, tag enum, 20-byte
  header.
- `core.rs` — `Core` trait + `MemoryCore` / `FileCore` /
  `BufferedFileCore`.
- `hasher.rs` — `Hasher` trait + `Sha1Hasher`.
- `database.rs` — `Database<C: Core, H: Hasher>`: open/init,
  root cursor, `run_context`, tx bookkeeping, replica APIs, compact.
- `cursor.rs` — `Cursor<'db, C, H, Mode>` typestate, streaming
  `Reader`/`Writer`.
- `hashmap.rs`, `hashset.rs`, `counted_hashmap.rs`,
  `counted_hashset.rs`, `arraylist.rs`, `linked_arraylist.rs`,
  `sorted_map.rs`, `sorted_set.rs` — thin collection wrappers.
- `offset_map.rs` — `OffsetMap` trait + memory + scratch-DB impls.
- `error.rs` — hand-rolled `Error`, `Result<T>` alias.

`tests/`: `high_level.rs`, `low_level.rs` (ported from Go),
`compat.rs` (golden files). `examples/dump-database.rs` mirrors Go.
`scripts/gen-fixtures.sh` regenerates `tests/fixtures/` via Zig + Go.
`tests/fixtures/` is versioned.

## 3. File format & core traits (compat invariant)

Anything here breaking changes interop and must fail `compat.rs`:

- `Slot`: 9 bytes — `tag: u8` (low 7 bits tag, high bit `full`),
  `value: u64` big-endian.
- `Tag`: `#[repr(u8)]` explicit discriminants in exact Zig order:
  `None=0, Index=1, ArrayList=2, LinkedArrayList=3, HashMap=4,
  KVPair=5, Bytes=6, ShortBytes=7, Uint=8, Int=9, Float=10,
  HashSet=11, CountedHashMap=12, CountedHashSet=13, SortedMap=14,
  SortedSet=15, SortedKVPair=16`. `validate()` rejects `>16`.
- Header: 20 bytes — magic `'xit'` + pad byte, `version: u16 BE = 1`,
  `hash_size: u16 BE`, `hash_id: u32 BE` (e.g. `BE("sha1")`),
  `file_id: u64 BE`.
- All integers big-endian. Shared constants copied from Go:
  `BIT_COUNT=4`, `SLOT_COUNT=16`, index and B-tree block sizes.
- `trait Core: Read + Write + Seek` plus `flush`, `fsync(bool)`,
  `committed_len`, `truncate`. Impls: `MemoryCore(Vec<u8>)`,
  `FileCore(std::fs::File)`, `BufferedFileCore(File + Vec<u8>`,
  flush-on-commit). Interchangeable via `Database<C, H>`.
- `trait Hasher { fn digest_len(&self) -> usize; fn digest(&self,
  data: &[u8]) -> Vec<u8>; fn id(&self) -> u32; }` plus `Sha1Hasher`
  behind default `sha1` feature. Open validates
  `hash_size`/`hash_id`; create writes them. `InitOpts { hash_id,
  file_id: Option<u64> }`, `None` = OS-random via `getrandom`.
  Future zero-dep path (vendoring SHA-1) must not change bytes.

## 4. Cursors & transactions

- `Cursor<'db, C, H, Mode>` borrows `&'db Database<C, H>`.
  `ReadCursor<'db, C, H> = Cursor<ReadOnly>`,
  `WriteCursor<'db, C, H> = Cursor<ReadWrite>`. Only `WriteCursor`
  gets `write`, `put_cursor`, `writer`, `freeze`.
- Multiple cursors coexist; write state (active tx, frozen slots)
  behind interior mutability; methods take `&self`. Runtime errors
  mirror Go: `ExpiredTransaction`, `NestedTopLevelWrite`,
  `ExpectedTxStart`.
- Transactions are `FnOnce` closures:
  `history.append_context(slot, |w: &WriteCursor| ...)`,
  `db.run_context(|w| ...)`. `Err` truncates appended bytes, rolls
  back, leaves DB unaffected.
- `freeze(&self)` pins copy-on-write from that cursor onward;
  `db.freeze()` global. Required when cloning data created in the
  same tx (else the source is mutated — documented with test).
- Reads: `read_uint/int/float`, `read_bytes(&mut [u8])`,
  `read_bytes_alloc`, `read_bytes_object` (optional `[u8; 2]`
  format tag), `slot()`, `count()`, `read_path` / `write_path`
  via `PathPart` enum, streaming `reader(&mut [u8])` /
  `writer(&mut [u8])` (caller must `finish()`).

## 5. Collections

Thin borrow wrappers, `new(cursor) -> Result<Self>` tag-validated,
`slot()` + `read_only()` everywhere.

- HAMT (`BIT_COUNT=4`): `HashMap`, `HashSet`, `CountedHashMap`,
  `CountedHashSet`.
- HAMT growable array: `ArrayList` (negative-index `get(-1)` =
  last; `append` / `append_cursor` / `get` / `put`).
- B-tree: `LinkedArrayList` (+ slice/concat/insert),
  `SortedMap` / `SortedSet` (lexicographic `&[u8]` keys,
  `put`/`get`/`remove`, `count()`).
- Key flavors (like Go): `get("str")` (hashes via `db`),
  `get_by_bytes(&[u8])`, `get_by_hash(&[u8])`; same for
  `put`/`put_cursor`/`remove`; `put_key` for map keys.
- Iteration: `iter()` everywhere; `ArrayList`/`LinkedArrayList`
  add `iter_from(index)`; `Sorted*` add `iter_from(key)` +
  `iter_from_index(i)` (O(log n) seek; powers pagination and
  type-ahead). Iterators implement
  `Iterator<Item = Result<ReadCursor>>`; maps expose
  `read_key_value_pair()`.

## 6. Compaction & replication

- `compact(target_opts, &mut impl OffsetMap) ->
  Result<Database<..>>` rebuilds into a new/empty target with only
  data reachable from the latest root, else `TargetNotEmpty`.
  `OffsetMap` trait (`get`/`put`/`reset`) with
  `MemoryOffsetMap(HashMap<u64, u64>)` + file-backed scratch-DB
  impl (run inside one `run_context`). Document OOM risk of the
  in-memory map.
- Replication = byte copy: `replica_state() ->
  {committed_size, checksum}` → `replication_start(state) ->
  start` (0 = full copy on `file_id` mismatch or
  post-compact/restore) → `read_committed(start, end, &mut dyn
  Write)` (streaming) → `apply_replicated(start, &mut dyn Read,
  len)`. Interrupted incremental syncs resume; interrupted
  `start == 0` copies leave the replica unusable until next sync
  (docs: copy-to-new-file-then-rename). Replicas mutate only via
  `apply_replicated`.

## 7. Errors

Hand-rolled `Error`, `pub type Result<T> = Result<T, Error>`,
`impl std::error::Error + From<std::io::Error>`. Variants:
`InvalidDatabase`, `InvalidVersion`, `UnexpectedTag`,
`Corrupted`/`ChecksumMismatch`, `ExpiredTransaction`,
`NestedTopLevelWrite`, `ExpectedTxStart`, `TargetNotEmpty`,
`InvalidHashSize`, `Io(io::Error)`. No panics on corrupt data;
internal borrow misuse documented as unreachable via closure API.

## 8. Testing & compat harness

- `tests/high_level.rs` + `tests/low_level.rs`: ported from Go
  (mirroring Zig `test.zig`) — every collection, tx/rollback,
  clone + `freeze` + undo-via-history-append, iterators +
  `iter_from(_index)`, pagination/type-ahead, large-bytes
  streaming, header/hash-id validation, compaction (both offset
  maps), replication (incremental + full + interrupted),
  multi-reader threads, non-`ArrayList` roots, overflow cases.
- `tests/compat.rs`: reads golden files in `tests/fixtures/`
  written by Zig + Go; Rust-written files read back by Go/Zig
  (byte-compare with fixed `file_id` fixtures where deterministic,
  semantic otherwise;
  `dump-database` output diffed).
- Inline unit tests: slot/tag/header round-trips, HAMT/B-tree
  edges.
- CI: `cargo test`, `cargo clippy -- -D warnings`,
  `cargo fmt --check`. Fixtures versioned; format drift fails.

## 9. Success criteria

`cargo test` green incl. ported suites; `compat.rs` proves
cross-impl read/write; `cargo clippy` + `fmt` clean; docs show
file-locking via `fs2` (caller-owned), `fsync` semantics, and the
same-tx-clone `freeze` rule.

Thread model (mirrors Zig/Go): one writer at a time guarded by an
external exclusive file lock; lock-free concurrent readers each use
their own `Database` instance.

Implementation phases for planning: (a) format + Core + Hasher +
header/slot/tag; (b) cursors + transactions + freeze; (c) HAMT
collections; (d) B-tree collections; (e) compaction + replication +
offset maps; (f) ported suites + compat harness + example.
