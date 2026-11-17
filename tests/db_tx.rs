use xitdb::core::{Core, MemoryCore};
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::Sha1Hasher;

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

#[test]
fn open_empty_has_minimum_committed_size() {
    let db = open_mem(42).unwrap();
    // 32-byte headers + 25-byte first trailer.
    assert_eq!(db.committed_size().unwrap(), 57);
}

#[test]
fn reopen_validates_and_preserves_size() {
    let db = open_mem(7).unwrap();
    db.run_context(|_| Ok(())).unwrap();
    let size = db.committed_size().unwrap();
    let core = db.into_core();
    let db2 = Database::open(
        core,
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(7),
            fsync: false,
        },
    )
    .unwrap();
    assert_eq!(db2.committed_size().unwrap(), size);
}

#[test]
fn corrupt_magic_rejected() {
    let mut core = MemoryCore::new();
    core.write(&[0xFFu8; 100]).unwrap();
    let err = Database::open(
        core,
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(1),
            fsync: false,
        },
    )
    .err()
    .unwrap();
    assert!(matches!(err, Error::InvalidDatabase));
}

#[test]
fn truncated_file_rejected() {
    // A header-only prefix is treated as an interrupted creation and
    // recreated, so truncate inside committed bytes to get an error.
    let db = open_mem(1).unwrap();
    let mut bytes = Vec::new();
    db.read_committed(0, db.committed_size().unwrap(), &mut bytes)
        .unwrap();
    bytes.pop();
    let mut core = MemoryCore::new();
    core.write(&bytes).unwrap();
    let res = Database::open(
        core,
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: Some(1),
            fsync: false,
        },
    );
    assert!(res.is_err());
}

#[test]
fn rollback_leaves_committed_size_unchanged() {
    let db = open_mem(9).unwrap();
    let before = db.committed_size().unwrap();
    let err = db
        .run_context(|_| Err(Error::ExpectedTxStart))
        .err()
        .unwrap();
    assert!(matches!(err, Error::ExpectedTxStart));
    assert_eq!(db.committed_size().unwrap(), before);
}

#[test]
fn empty_commit_appends_one_trailer() {
    let db = open_mem(11).unwrap();
    db.run_context(|_| Ok(())).unwrap();
    // scratch copy (9) + trailer (25) past the initial 57.
    assert_eq!(db.committed_size().unwrap(), 91);
}

#[test]
fn freeze_outside_tx_errors() {
    let db = open_mem(13).unwrap();
    let err = db.freeze().err().unwrap();
    assert!(matches!(err, Error::ExpectedTxStart));
}

#[test]
fn nested_run_context_rejected_and_rolled_back() {
    let db = open_mem(15).unwrap();
    let before = db.committed_size().unwrap();
    let err = db
        .run_context(|_| db.run_context(|_| Ok(())))
        .err()
        .unwrap();
    assert!(matches!(err, Error::NestedTopLevelWrite));
    assert_eq!(db.committed_size().unwrap(), before);
}
