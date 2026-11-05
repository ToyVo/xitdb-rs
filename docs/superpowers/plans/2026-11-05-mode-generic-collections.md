# Mode-generic collections Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the eight `Write*` wrapper structs so each collection is a single mode-generic struct, with zero behavior change.

**Architecture:** In each collection file, delete the `Write*` struct, its `Deref` impl, and `op()`; move write methods into a new `impl ... ReadWrite` block with `self.cursor` substituted for `self.write_cursor` (append_cursor keeps its refresh as write-then-`slot_ptr.set` from the result, no file re-read). Then rename callers per the table in Task 1. Existing suites (especially `compat` byte parity) lock behavior.

**Tech Stack:** Rust, edition 2024, MSRV 1.85, zero normal dependencies.

**Spec:** `docs/superpowers/specs/2026-11-05-mode-generic-collections-design.md`

## Global Constraints

- No new normal dependencies (dev-dependencies unchanged).
- Edition 2024; MSRV 1.85 (Task 4 verifies with the 1.85 toolchain).
- `cargo clippy --all-targets -- -D warnings` must pass after every task.
- `cargo fmt --check` must pass after every task.
- No behavior change: `tests/compat.rs` byte parity must pass unmodified in expectation after every task.
- Version bump to `0.2.0` happens in Task 4 only; do NOT publish.

## Review Focus

- Same-transaction read-after-write observes the write (arraylist put→get, hashmap put→get, frozen-copy read): pinned by the new tests in Task 1 (`same_transaction_reads_observe_writes`) and Task 2 (`hashmap_same_transaction_reads_observe_writes`).
- Stale pre-transaction wrappers still error instead of silently reading: pinned by existing `tests/db_tx.rs` expired-transaction tests, run in every task's gates.
- Byte parity with Go golden files after the merge: pinned by existing `tests/compat.rs`, run in every task's gates.
- Frozen-write segregation (`freeze` then write copies, source untouched): pinned by existing `tests/readme.rs::undo_and_freeze`, run in every task's gates.
- No lingering old names in API or docs: pinned by the Task 4 grep check (expected: no output).

---

### Task 1: ArrayList end-to-end (recipe proof)

**Files:**
- Modify: `src/arraylist.rs:83-192` (delete `WriteArrayList`, `Deref` impl, `op()`; move write methods into new `impl ArrayList<'db, C, H, ReadWrite>` with `self.cursor`; delete line 3 `Deref` import and line 193 `ReadArrayList` alias)
- Modify: every file naming ArrayList wrappers — find with `grep -rn "WriteArrayList\|ReadArrayList" tests/ examples/ benches/ README.md` (known: tests/hamt.rs, tests/readme.rs, tests/high_level.rs, tests/compat.rs, tests/compact_repl.rs, tests/low_level.rs, examples/gen_fixtures.rs, benches/ops.rs, README.md)
- Modify: `tests/low_level.rs` (append pinning test below)

**Interfaces:**
- Consumes: nothing (first task).
- Produces: the recipe later tasks copy — rename table (`WriteArrayList::new_write(w)` → `ArrayList::new_write(w)`, `ReadArrayList::new(c)` → `ArrayList::new(c)`, type `WriteArrayList<'db,C,H>` → `ArrayList<'db,C,H,ReadWrite>` with `ReadWrite` imported from `xitdb::cursor`, type `ReadArrayList<'db,C,H>` → `ArrayList<'db,C,H,ReadOnly>`), plus refresh rule (append_cursor writes then sets `slot_ptr` from the result; all other write methods substitute the cursor with no extra refresh).

- [ ] **Step 1: Add the pinning test to `tests/low_level.rs`**

```rust
#[test]
fn same_transaction_reads_observe_writes() {
    let db = open_mem(920).unwrap();
    db.run_context(|w| {
        let list = ArrayList::new_write(w)?;
        list.append(Writeable::Uint(1))?;
        // same-transaction read through the same wrapper, incl. frozen copy
        assert_eq!(list.get(0)?.ok_or(Error::KeyNotFound)?.read_uint()?, 1);
        Ok(())
    })
    .unwrap();
}
```

- [ ] **Step 2: Run check to verify it fails**

Run: `cargo check --test low_level`
Expected: FAIL with E0433/E0599 (`ArrayList::new_write` does not exist yet).

- [ ] **Step 3: Migrate `src/arraylist.rs:83-192` per the recipe in Interfaces**

Move `new_write/put/put_cursor/append/append_cursor/append_context/slice` into `impl<'db, C: Core, H: Hasher> ArrayList<'db, C, H, crate::cursor::ReadWrite>`, substituting `self.cursor` for `self.write_cursor`; `append_cursor` becomes write-then-`self.cursor.slot_ptr.set(result.slot_ptr.get())` and returns the result cursor. Delete the struct, `Deref` impl, `op()`, the `Deref` import, and the `ReadArrayList` alias.

- [ ] **Step 4: Rename all ArrayList callers found by the grep in Files**

- [ ] **Step 5: Run gates**

Run: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
Expected: all green, including `compat` and the new test.

- [ ] **Step 6: Commit**

```bash
git add src/arraylist.rs tests/ examples/ benches/ops.rs README.md
git commit -m "refactor: single mode-generic ArrayList"
```

### Task 2: Hash family (HashMap, HashSet, CountedHashMap, CountedHashSet)

**Files:**
- Modify: `src/hashmap.rs:174-395`, `src/hashset.rs:106-230`, `src/counted_hashmap.rs:133-280`, `src/counted_hashset.rs:91-188` (same recipe as Task 1; keep the `pub(crate) fn write_cursor()` accessor with identical name/signature, implemented as `&self.cursor` in the `ReadWrite` impl)
- Modify: `src/offset_map.rs` (rename at the `reset` call site only; behavior unchanged)
- Modify: callers from `grep -rn "WriteHashMap\|ReadHashMap\|WriteHashSet\|WriteCountedHashMap\|WriteCountedHashSet" tests/ examples/ benches/ README.md` (do NOT touch `WriteKeyValuePair` in `src/cursor.rs` — out of scope; confirm with `grep -n "WriteHashMap::" src/cursor.rs src/database.rs` expecting no output)
- Modify: `tests/hamt.rs` (append pinning test below)

**Interfaces:**
- Consumes: Task 1's recipe and rename table (replace `ArrayList` with each collection name).
- Produces: hash family migrated; `offset_map.rs` compiling against the merged accessor.

- [ ] **Step 1: Add the pinning test to `tests/hamt.rs`**

```rust
#[test]
fn hashmap_same_transaction_reads_observe_writes() {
    let db = open_mem(921).unwrap();
    db.run_context(|w| {
        let map = HashMap::new_write(w)?;
        map.put("k", Writeable::Uint(7))?;
        assert_eq!(
            map.get("k")?.ok_or(Error::KeyNotFound)?.read_uint()?,
            7
        );
        Ok(())
    })
    .unwrap();
}
```

- [ ] **Step 2: Run check to verify it fails**

Run: `cargo check --test hamt`
Expected: FAIL with E0433/E0599 (`HashMap::new_write` does not exist yet).

- [ ] **Step 3: Migrate the four `src/` files per Task 1's recipe**

- [ ] **Step 4: Rename all hash-family callers (including `src/offset_map.rs`)**

- [ ] **Step 5: Run gates**

Run: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add src/hashmap.rs src/hashset.rs src/counted_hashmap.rs src/counted_hashset.rs src/offset_map.rs tests/ examples/ benches/ops.rs README.md
git commit -m "refactor: single mode-generic hash collections"
```

### Task 3: B-tree family (LinkedArrayList, SortedMap, SortedSet)

**Files:**
- Modify: `src/linked_arraylist.rs:84-223`, `src/sorted_map.rs:199-305`, `src/sorted_set.rs:168-270` (same recipe as Task 1)
- Modify: callers from `grep -rn "WriteLinkedArrayList\|WriteSortedMap\|WriteSortedSet\|ReadSortedMap" tests/ examples/ benches/ README.md` (known: tests/btree.rs, tests/readme.rs, tests/high_level.rs, tests/low_level.rs, examples/gen_fixtures.rs, benches/ops.rs, README.md)

**Interfaces:**
- Consumes: Task 1's recipe.
- Produces: all eight collections migrated; no `Write*`/`Read*` collection names remain outside history.

- [ ] **Step 1: Migrate the three `src/` files per Task 1's recipe**

No new pinning test: the recipe is proven and the suites (especially `btree`, `compat`, `readme`) lock behavior.

- [ ] **Step 2: Rename all B-tree callers**

- [ ] **Step 3: Run gates**

Run: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add src/linked_arraylist.rs src/sorted_map.rs src/sorted_set.rs tests/ examples/ benches/ops.rs README.md
git commit -m "refactor: single mode-generic B-tree collections"
```

### Task 4: Finish (docs, version, full verification)

**Files:**
- Modify: `src/lib.rs` only if it names removed wrappers (check first; expected: no change).
- Modify: `Cargo.toml` (version `0.1.0` → `0.2.0`), `CHANGELOG.md` (new `0.2.0` entry describing the breaking rename; keep Unreleased section for the rest).

**Interfaces:**
- Consumes: completed migration from Tasks 1–3.
- Produces: releasable tree (publish itself stays deferred).

- [ ] **Step 1: Negative check for leftover names**

Run: `grep -rn "WriteArrayList\|WriteHashMap\|WriteHashSet\|WriteCountedHashMap\|WriteCountedHashSet\|WriteLinkedArrayList\|WriteSortedMap\|WriteSortedSet\|ReadArrayList\|ReadHashMap\|ReadHashSet\|ReadCountedHashMap\|ReadCountedHashSet\|ReadLinkedArrayList\|ReadSortedMap\|ReadSortedSet" src/ tests/ examples/ benches/ README.md`
Expected: no output.

- [ ] **Step 2: Bump version and changelog**

- [ ] **Step 3: Run full gates including MSRV**

Run: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo bench --no-run`, `cargo +1.85.0 test --all-targets`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml CHANGELOG.md
git commit -m "chore: 0.2.0 for mode-generic collection rename"
```
