// Generates tests/fixtures/go-golden.db with a fixed file id.
// Covers every collection kind plus scalars, format tags, and nesting,
// matching the manifest asserted by tests/compat.rs.
package main

import (
	"crypto/sha1"
	"fmt"
	"os"

	xitdb "github.com/xit-vcs/xitdb-go"
)

func check(err error) {
	if err != nil {
		panic(err)
	}
}

func main() {
	out := os.Args[1]
	f, err := os.Create(out)
	check(err)
	defer f.Close()

	core := xitdb.NewCoreFile(f)
	defer core.Close()
	hasher := xitdb.Hasher{
		Hash:   sha1.New,
		ID:     xitdb.BytesToID([4]byte{'s', 'h', 'a', '1'}),
		FileID: 1234,
	}
	db, err := xitdb.NewDatabase(core, hasher)
	check(err)

	// Moment 0: a small marker.
	check(db.RunContext(func(c *xitdb.WriteCursor) error {
		h, err := xitdb.NewWriteArrayList(c)
		if err != nil {
			return err
		}
		var last xitdb.Slot
		last, err = h.GetSlot(-1)
		if err != nil {
			return err
		}
		return h.AppendContext(last, func(mc *xitdb.WriteCursor) error {
			m, err := xitdb.NewWriteHashMap(mc)
			if err != nil {
				return err
			}
			return m.Put("gen", xitdb.NewUint(0))
		})
	}))

	// Moment 1: the manifest.
	check(db.RunContext(func(c *xitdb.WriteCursor) error {
		h, err := xitdb.NewWriteArrayList(c)
		if err != nil {
			return err
		}
		var last xitdb.Slot
		last, err = h.GetSlot(-1)
		if err != nil {
			return err
		}
		return h.AppendContext(last, func(mc *xitdb.WriteCursor) error {
			m, err := xitdb.NewWriteHashMap(mc)
			if err != nil {
				return err
			}
			if err := m.Put("uint", xitdb.NewUint(42)); err != nil {
				return err
			}
			if err := m.Put("int", xitdb.NewInt(-7)); err != nil {
				return err
			}
			if err := m.Put("float", xitdb.NewFloat(2.5)); err != nil {
				return err
			}
			if err := m.Put("tagged", xitdb.NewTaggedBytes([]byte("abc"), []byte("bi"))); err != nil {
				return err
			}
			long := make([]byte, 5000)
			for i := range long {
				long[i] = byte(i)
			}
			if err := m.Put("long", xitdb.NewString(string(long))); err != nil {
				return err
			}
			sc, err := m.PutCursor("sorted")
			if err != nil {
				return err
			}
			sm, err := xitdb.NewWriteSortedMap(sc)
			if err != nil {
				return err
			}
			for _, k := range []string{"c", "a", "b"} {
				if err := sm.Put(k, xitdb.NewUint(uint64(len(k)))); err != nil {
					return err
				}
			}
			lc, err := m.PutCursor("linked")
			if err != nil {
				return err
			}
			ll, err := xitdb.NewWriteLinkedArrayList(lc)
			if err != nil {
				return err
			}
			for _, s := range []string{"x", "y", "z"} {
				if err := ll.Append(xitdb.NewString(s)); err != nil {
					return err
				}
			}
			hc, err := m.PutCursor("hset")
			if err != nil {
				return err
			}
			hs, err := xitdb.NewWriteHashSet(hc)
			if err != nil {
				return err
			}
			for _, s := range []string{"s1", "s2"} {
				if err := hs.Put(s); err != nil {
					return err
				}
			}
			cc, err := m.PutCursor("chm")
			if err != nil {
				return err
			}
			chm, err := xitdb.NewWriteCountedHashMap(cc)
			if err != nil {
				return err
			}
			if err := chm.Put("k1", xitdb.NewUint(1)); err != nil {
				return err
			}
			if err := chm.Put("k2", xitdb.NewUint(2)); err != nil {
				return err
			}
			return nil
		})
	}))
	fmt.Println("wrote", out)
}
