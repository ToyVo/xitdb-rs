# Design: single mode-generic collection structs

Date: 2026-11-05. Context: review feedback on
[xit-vcs/xitdb#6](https://github.com/xit-vcs/xitdb/issues/6) asks for the
Zig-style shape — one generic struct per collection, e.g.
`ArrayList<'db, C, H, Mode>` — instead of the current Go-style split of a
mode-generic struct plus a `Write*` wrapper (two cursors, `Deref`
inheritance, `op()` dual-refresh).

## Goal

Remove the eight `Write*` wrapper structs (`WriteArrayList`,
`WriteHashMap`, `WriteHashSet`, `WriteCountedHashMap`,
`WriteCountedHashSet`, `WriteLinkedArrayList`, `WriteSortedMap`,
`WriteSortedSet`) with their `Deref` impls and `op()` helpers. Reads stay
on the existing mode-generic structs; write methods move into
`impl ... Mode = ReadWrite` blocks on those same structs. No behavior
change: same paths, same bytes, full suite green before and after.

Non-goals: `no_std` (deferred, no consumer); `WriteKeyValuePair` in
`cursor.rs` (not a collection wrapper); any format or algorithm change.

## Target shape

```rust
pub struct ArrayList<'db, C: Core, H: Hasher, Mode> {
    cursor: Cursor<'db, C, H, Mode>,
}

impl<Mode> ArrayList<Mode> {
    pub fn new(cursor: Cursor<'db, C, H, Mode>) -> Result<Self>; // unchanged
    pub fn slot/count/get/try_iter/... // unchanged, all modes
    pub fn read_only(&self) -> ArrayList<'db, C, H, ReadOnly>; // unchanged
}

impl ArrayList<'db, C, H, ReadWrite> {
    pub fn new_write(cursor: &WriteCursor<'db, C, H>) -> Result<Self>; // moved, same name/signature
    pub fn put/put_cursor/append/... // moved; bodies substitute self.cursor for self.write_cursor
}
```

- `Read*` aliases (`ReadArrayList`, …) are removed with the `Write*`
  names: one uniform spelling (`ArrayList<'db, C, H, ReadOnly>`) rather
  than half-kept shorthand. `ReadOnly`/`ReadWrite`/`ReadCursor`/
  `WriteCursor` in `cursor.rs` stay.
- Constructor names are unchanged (`new` takes an owned cursor, both
  modes; `new_write` borrows a write cursor, `ReadWrite` only), so
  call-site migration is close to a rename:
  `WriteArrayList::new_write(w)` → `ArrayList::new_write(w)`,
  `ReadHashMap::new(c)` → `HashMap::new(c)`, annotations
  `WriteHashMap<'db, C, H>` → `HashMap<'db, C, H, ReadWrite>`.
- The `pub(crate) fn write_cursor()` accessor on `WriteHashMap` (used
  once, by `FileOffsetMap::reset`) becomes an equivalent accessor for
  the merged cursor field.

## Why this is sound

- `WriteCursor`/`ReadCursor` are already aliases of `Cursor<Mode>`, so
  the merged field has no dead instantiation in any mode. Write-only
  methods living in `impl ReadWrite` preserve today's compile-time
  read-only enforcement (which already works through split impls).
- `write_path` self-refreshes the stored cursor (`slot_ptr.set` in its
  tail whenever a transaction is open), and `Cell`-based cursors allow
  `&self` refresh. The wrapper's second cursor plus `op()` dual-refresh
  therefore collapses to the single refresh the stored cursor already
  gets — the same shape `WriteCursor::write` already uses. No `Deref`.

## Refresh-behavior note (verified, not changed)

Only `append_cursor` routes through `op()` today; the other write
methods rely on `write_path`'s self-refresh of the stored cursor, while
the read-half clone can lag within a transaction. After the merge there
is one cursor and it is always the refreshed one, i.e. reads observe
writes exactly like Go's shared slot pointer. If any currently-passing
test depended on the lagging read half, it will fail loudly; the compat
suite (byte parity with Go goldens) is the lock. No test is expected to
depend on it (all same-transaction read-after-write flows re-acquire
cursors or run post-commit).

## Files

- Map: `src/arraylist.rs`, `src/counted_hashmap.rs`,
  `src/counted_hashset.rs`, `src/hashmap.rs`, `src/hashset.rs`,
  `src/linked_arraylist.rs`, `src/sorted_map.rs`, `src/sorted_set.rs` —
  delete `Write*` struct, `Deref` impl, `op()`; move write methods.
- `src/offset_map.rs` — accessor rename at the `reset` call site.
- Callers, renames only: `tests/*.rs` (9 files), `examples/*.rs` (3),
  `benches/ops.rs`, `README.md` snippets (still executed by
  `tests/readme.rs`), crate docs in `src/lib.rs` if they name wrappers.
- `Cargo.toml`: bump to `0.2.0` (breaking API change).
  `CHANGELOG.md`: move the change under a `0.2.0` Unreleased entry.

## Verification

- `cargo test` (all suites, especially `compat` byte parity and
  `readme`), `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt --check`, `cargo bench --no-run`, MSRV `1.85` suite.
- No new dependencies, no new syntax beyond existing patterns; MSRV
  and offline build unaffected.
- Publish deferred per current stance; `0.2.0` ships with the next
  functional release.
