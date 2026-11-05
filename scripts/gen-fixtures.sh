#!/bin/bash
# Regenerates tests/fixtures/go-golden.db (fixed file id 1234) using a
# pinned xitdb-go, writes tests/fixtures/rust-golden.db via the Rust
# example, then reverse-verifies the Rust file with Go.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"

XITDB_GO_PIN="${XITDB_GO_PIN:-86db69bc728d2e9bdd015f9cefa23df40beaab65}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

git clone -q https://github.com/xit-vcs/xitdb-go "$WORK/xitdb-go"
git -C "$WORK/xitdb-go" checkout -q "$XITDB_GO_PIN"
mkdir -p "$WORK/xitdb-go/tmpgen" "$WORK/xitdb-go/tmpverify"
cp "$REPO/scripts/gen-go-golden/main.go" "$WORK/xitdb-go/tmpgen/main.go"
cp "$REPO/scripts/verify-rust-golden/main.go" "$WORK/xitdb-go/tmpverify/main.go"

(cd "$WORK/xitdb-go" && go run ./tmpgen "$REPO/tests/fixtures/go-golden.db")
echo "go-golden.db regenerated"

(cd "$REPO" && cargo run -q --example gen_fixtures -- tests/fixtures/rust-golden.db)
echo "rust-golden.db regenerated"

(cd "$WORK/xitdb-go" && go run ./tmpverify "$REPO/tests/fixtures/rust-golden.db")
