#!/bin/bash
# Regenerates tests/fixtures/zig-golden.db (fixed file id 1234) using a
# pinned xitdb (Zig, the primary implementation), writes
# tests/fixtures/rust-golden.db via the Rust example, then
# reverse-verifies the Rust file with Zig.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"

XITDB_ZIG_PIN="${XITDB_ZIG_PIN:-274418f6ac1c37bccbe7e8e6f212dc123bca4843}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

git clone -q https://github.com/xit-vcs/xitdb "$WORK/xitdb"
git -C "$WORK/xitdb" checkout -q "$XITDB_ZIG_PIN"
cp "$REPO/scripts/gen-zig-golden/main.zig" "$WORK/xitdb/gen_main.zig"
cp "$REPO/scripts/verify-rust-golden/main.zig" "$WORK/xitdb/verify_main.zig"

(cd "$WORK/xitdb" && zig build-exe gen_main.zig -femit-bin="$WORK/gen-main")
"$WORK/gen-main" "$REPO/tests/fixtures/zig-golden.db"
echo "zig-golden.db regenerated"

(cd "$REPO" && cargo run -q --example gen_fixtures -- tests/fixtures/rust-golden.db)
echo "rust-golden.db regenerated"

(cd "$WORK/xitdb" && zig build-exe verify_main.zig -femit-bin="$WORK/verify-main")
"$WORK/verify-main" "$REPO/tests/fixtures/rust-golden.db"
