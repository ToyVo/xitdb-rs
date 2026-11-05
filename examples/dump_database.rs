//! Dumps a database file in human-readable or JSON form.
//! Mirrors `xitdb-go/cmd/dump-database`.
//!
//! Usage: `dump-database [--json] <database-file>`

use std::fs::OpenOptions;
use std::io::Read;

use xitdb::core::{Core, FileCore};
use xitdb::cursor::{Cursor, KeyValuePair};
use xitdb::database::{Database, InitOpts};
use xitdb::error::Error;
use xitdb::hasher::{Hasher, Sha1Hasher};
use xitdb::header::{HEADER_LEN, Header};
use xitdb::tag::Tag;

type DumpResult<T> = Result<T, Error>;

fn open_db(path: &str) -> DumpResult<Database<FileCore, Sha1Hasher>> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| Error::InvalidDatabase)?;
    let mut header_bytes = [0u8; HEADER_LEN];
    file.read_exact(&mut header_bytes)
        .map_err(|_| Error::Truncated)?;
    let header = Header::from_bytes(header_bytes);
    if header.magic != xitdb::header::MAGIC {
        return Err(Error::InvalidDatabase);
    }
    if usize::from(header.hash_size) != Sha1Hasher.digest_len() {
        return Err(Error::InvalidHashSize);
    }
    Database::open(
        FileCore::new(file, false),
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: None,
            fsync: false,
        },
    )
}

fn scalar<C: Core, H: Hasher, M>(cursor: &Cursor<C, H, M>) -> DumpResult<String> {
    match cursor.slot()?.tag {
        Tag::None => Ok("(none)".to_string()),
        Tag::Bytes | Tag::ShortBytes => {
            let value = cursor.read_bytes_alloc()?;
            Ok(format!("{:?}", String::from_utf8_lossy(&value)))
        }
        Tag::Uint => Ok(cursor.read_uint()?.to_string()),
        Tag::Int => Ok(cursor.read_int()?.to_string()),
        Tag::Float => Ok(cursor.read_float()?.to_string()),
        other => Ok(format!("<key tag: {other:?}>")),
    }
}

fn print_value<C: Core, H: Hasher, M>(cursor: &Cursor<C, H, M>, indent: &str) -> DumpResult<()> {
    let pad = format!("{indent}  ");
    match cursor.slot()?.tag {
        Tag::None | Tag::Bytes | Tag::ShortBytes | Tag::Uint | Tag::Int | Tag::Float => {
            println!("{indent}{}", scalar(cursor)?);
        }
        Tag::ArrayList | Tag::LinkedArrayList => {
            let count = cursor.count()?;
            println!("{indent}[list len={count}]");
            for item in cursor.try_iter()? {
                print_value(&item?, &pad)?;
            }
        }
        Tag::HashMap | Tag::CountedHashMap | Tag::SortedMap => {
            println!("{indent}{{map}}");
            for item in cursor.try_iter()? {
                let kv: KeyValuePair<'_, C, H> = item?.read_key_value_pair()?;
                println!("{pad}key: {}", scalar(&kv.key)?);
                print!("{pad}value: ");
                print_value(&kv.value, &format!("{pad}  "))?;
            }
        }
        Tag::HashSet | Tag::CountedHashSet | Tag::SortedSet => {
            println!("{indent}{{set}}");
            for item in cursor.try_iter()? {
                let kv: KeyValuePair<'_, C, H> = item?.read_key_value_pair()?;
                println!("{pad}{}", scalar(&kv.key)?);
            }
        }
        other => println!("{indent}<unprintable: {other:?}>"),
    }
    Ok(())
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if u32::from(c) < 0x20 => {
                use std::fmt::Write as _;
                write!(out, "\\u{:04x}", u32::from(c)).unwrap_or(());
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn to_json<C: Core, H: Hasher, M>(cursor: &Cursor<C, H, M>) -> DumpResult<String> {
    match cursor.slot()?.tag {
        Tag::None => Ok("null".to_string()),
        Tag::Bytes | Tag::ShortBytes => Ok(json_escape(&String::from_utf8_lossy(
            &cursor.read_bytes_alloc()?,
        ))),
        Tag::Uint => Ok(cursor.read_uint()?.to_string()),
        Tag::Int => Ok(cursor.read_int()?.to_string()),
        Tag::Float => {
            let value = cursor.read_float()?;
            if value.fract() == 0.0 && value.abs() < 1e15 {
                Ok(format!("{value:.1}"))
            } else {
                Ok(format!("{value}"))
            }
        }
        Tag::ArrayList | Tag::LinkedArrayList => {
            let mut parts = Vec::new();
            for item in cursor.try_iter()? {
                parts.push(to_json(&item?)?);
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        Tag::HashMap | Tag::CountedHashMap | Tag::SortedMap => {
            let mut parts = Vec::new();
            for item in cursor.try_iter()? {
                let kv: KeyValuePair<'_, C, H> = item?.read_key_value_pair()?;
                let key = match kv.key.slot()?.tag {
                    Tag::Bytes | Tag::ShortBytes => {
                        String::from_utf8_lossy(&kv.key.read_bytes_alloc()?).into_owned()
                    }
                    _ => scalar(&kv.key)?,
                };
                parts.push(format!("{}: {}", json_escape(&key), to_json(&kv.value)?));
            }
            Ok(format!("{{{}}}", parts.join(", ")))
        }
        Tag::HashSet | Tag::CountedHashSet | Tag::SortedSet => {
            let mut parts = Vec::new();
            for item in cursor.try_iter()? {
                let kv: KeyValuePair<'_, C, H> = item?.read_key_value_pair()?;
                parts.push(json_escape(&String::from_utf8_lossy(
                    &kv.key.read_bytes_alloc()?,
                )));
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        other => Ok(format!("{{\"$tag\": \"{other:?}\"}}")),
    }
}

fn run() -> DumpResult<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|item| item == "--json");
    let Some(path) = args.iter().find(|item| *item != "--json") else {
        eprintln!("usage: dump-database [--json] <file>");
        return Ok(());
    };
    let db = open_db(path)?;
    let root = db.root_cursor();
    if json {
        println!("{}", to_json(&root)?);
    } else {
        println!("Database: {path}");
        println!("---");
        print_value(&root, "")?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
    }
}
