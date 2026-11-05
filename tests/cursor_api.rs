use xitdb::core::MemoryCore;
use xitdb::cursor::{PathPart, Writeable};
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::Sha1Hasher;
use xitdb::tag::Tag;

fn open_mem(file_id: u64) -> Result<Database<MemoryCore, Sha1Hasher>, Error> {
    Database::open(
        MemoryCore::new(),
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(file_id),
            fsync: false,
        },
    )
}

fn read_root(
    db: &Database<MemoryCore, Sha1Hasher>,
) -> xitdb::cursor::WriteCursor<'_, MemoryCore, Sha1Hasher> {
    // Fresh snapshot: root_cursor re-reads the latest trailer.
    db.root_cursor()
}

#[test]
fn scalar_round_trip() {
    let db = open_mem(21).unwrap();
    db.run_context(|w| w.write(Writeable::Uint(42))).unwrap();
    assert_eq!(read_root(&db).read_uint().unwrap(), 42);
    db.run_context(|w| w.write(Writeable::Int(-7))).unwrap();
    assert_eq!(read_root(&db).read_int().unwrap(), -7);
    db.run_context(|w| w.write(Writeable::Float(2.5))).unwrap();
    assert!((read_root(&db).read_float().unwrap() - 2.5).abs() < 1e-12);
}

#[test]
fn scalar_wrong_tag_errors() {
    let db = open_mem(22).unwrap();
    db.run_context(|w| w.write(Writeable::Uint(1))).unwrap();
    let err = read_root(&db).read_int().err().unwrap();
    assert!(matches!(err, Error::UnexpectedTag));
}

#[test]
fn bytes_short_and_long_tags() {
    let db = open_mem(23).unwrap();
    db.run_context(|w| w.write(Writeable::Bytes(b"hi".to_vec())))
        .unwrap();
    let r = read_root(&db);
    assert_eq!(r.slot().unwrap().tag, Tag::ShortBytes);
    assert_eq!(r.read_bytes_alloc().unwrap(), b"hi");

    let long = vec![7u8; 100];
    db.run_context(|w| w.write(Writeable::Bytes(long.clone())))
        .unwrap();
    let r = read_root(&db);
    assert_eq!(r.slot().unwrap().tag, Tag::Bytes);
    assert_eq!(r.read_bytes_alloc().unwrap(), long);
}

#[test]
fn bytes_format_tag_round_trip() {
    let db = open_mem(24).unwrap();
    db.run_context(|w| {
        w.write(Writeable::BytesObject {
            value: b"abc".to_vec(),
            format_tag: Some(*b"bi"),
        })
    })
    .unwrap();
    let r = read_root(&db);
    let slot = r.slot().unwrap();
    assert_eq!(slot.tag, Tag::ShortBytes);
    assert!(slot.full);
    let obj = r.read_bytes_object(&mut [0u8; 16]).unwrap();
    assert_eq!(obj.value, b"abc");
    assert_eq!(obj.format_tag, Some(*b"bi"));
}

#[test]
fn reader_incremental_read_with_seek() {
    let db = open_mem(25).unwrap();
    let data = b"hello\n".repeat(3);
    db.run_context(|w| w.write(Writeable::Bytes(data.clone())))
        .unwrap();
    let r = read_root(&db);
    let mut reader = r.reader(&mut [0u8; 8]).unwrap();
    assert_eq!(reader.read_all().unwrap(), data);
    reader.seek_to(6).unwrap();
    assert_eq!(reader.read_all().unwrap(), data[6..].to_vec());
}

#[test]
fn writer_at_root_errors() {
    let db = open_mem(26).unwrap();
    let cursor = db.root_cursor();
    let mut buf = [0u8; 8];
    let err = cursor.writer(&mut buf).err().unwrap();
    assert!(matches!(err, Error::CursorNotWriteable));
}

#[test]
fn write_path_empty_is_identity() {
    let db = open_mem(27).unwrap();
    db.run_context(|w| w.write(Writeable::Uint(42))).unwrap();
    let cursor = db.root_cursor();
    let back = cursor.write_path(&[]).unwrap();
    assert_eq!(back.slot().unwrap().value, 42);
}

#[test]
fn nested_root_write_rejected() {
    let db = open_mem(28).unwrap();
    // A root cursor held across the transaction boundary must not
    // start a second top-level write inside `run_context`.
    let root = db.root_cursor();
    let before = db.committed_size().unwrap();
    let err = db
        .run_context(|_| {
            root.write_path(&[PathPart::Write(Writeable::Uint(1))])?;
            Ok(())
        })
        .err()
        .unwrap();
    assert!(matches!(err, Error::NestedTopLevelWrite));
    assert_eq!(db.committed_size().unwrap(), before);
}

#[test]
fn freeze_then_root_write_still_commits() {
    let db = open_mem(29).unwrap();
    db.run_context(|w| {
        db.freeze()?;
        w.freeze()?;
        w.write(Writeable::Uint(5))
    })
    .unwrap();
    assert_eq!(read_root(&db).read_uint().unwrap(), 5);
}

#[test]
fn count_and_slot_of_root() {
    let db = open_mem(30).unwrap();
    assert_eq!(read_root(&db).count().unwrap(), 0);
    db.run_context(|w| w.write(Writeable::Bytes(b"abcd".to_vec())))
        .unwrap();
    assert_eq!(read_root(&db).count().unwrap(), 4);
}
