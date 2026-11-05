// Verifies tests/fixtures/rust-golden.db (written by the Rust example)
// contains the expected manifest. Exit non-zero on any mismatch.
package main

import (
	"crypto/sha1"
	"fmt"
	"os"

	xitdb "github.com/xit-vcs/xitdb-go"
)

var failures int

func check(err error) {
	if err != nil {
		fmt.Println("FAIL:", err)
		os.Exit(1)
	}
}

func expect(cond bool, msg string) {
	if !cond {
		fmt.Println("FAIL:", msg)
		failures++
	}
}

func main() {
	f, err := os.Open(os.Args[1])
	check(err)
	core := xitdb.NewCoreFile(f)
	defer core.Close()
	hasher := xitdb.Hasher{Hash: sha1.New, ID: xitdb.BytesToID([4]byte{'s', 'h', 'a', '1'})}
	db, err := xitdb.NewDatabase(core, hasher)
	check(err)

	history, err := xitdb.NewReadArrayList(db.RootCursor().ReadCursor)
	check(err)
	n, err := history.Count()
	check(err)
	expect(n == 1, fmt.Sprintf("history count = %d, want 1", n))

	mc, err := history.GetCursor(0)
	check(err)
	m, err := xitdb.NewReadHashMap(mc)
	check(err)

	strEq := func(key, want string) {
		c, err := m.GetCursor(key)
		check(err)
		if c == nil {
			expect(false, "missing "+key)
			return
		}
		v, err := c.ReadBytes(4096)
		check(err)
		expect(string(v) == want, fmt.Sprintf("%s = %q, want %q", key, string(v), want))
	}
	strEq("name", "rust")
	strEq("tagged", "abc")

	uc, err := m.GetCursor("answer")
	check(err)
	uv, err := uc.ReadUint()
	check(err)
	expect(uv == 42, fmt.Sprintf("answer = %d, want 42", uv))

	ac, err := m.GetCursor("list")
	check(err)
	a, err := xitdb.NewReadArrayList(ac)
	check(err)
	an, err := a.Count()
	check(err)
	expect(an == 2, fmt.Sprintf("list count = %d, want 2", an))

	sc, err := m.GetCursor("sorted")
	check(err)
	sm, err := xitdb.NewReadSortedMap(sc)
	check(err)
	sn, err := sm.Count()
	check(err)
	expect(sn == 2, fmt.Sprintf("sorted count = %d, want 2", sn))

	if failures > 0 {
		os.Exit(1)
	}
	fmt.Println("rust-golden.db OK")
}
