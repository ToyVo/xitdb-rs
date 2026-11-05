//! Minimal REST key-value server over a file-backed xitdb `HashMap`.
//!
//! ```text
//! cargo run --example rest_server
//! curl -X PUT localhost:3000/kv/foo -d 'bar'
//! curl localhost:3000/kv/foo              # -> bar
//! curl -X DELETE localhost:3000/kv/foo    # -> 200 (404 when missing)
//! ```
//!
//! Each call is its own transaction. State is `Mutex`-guarded because
//! `Database` is `!Sync` (interior mutability); it is `Send`, so Tokio
//! multi-thread is fine. Small ops lock directly in the handler —
//! production code would `spawn_blocking` instead.

use std::fs::OpenOptions;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::put;
use xitdb::core::FileCore;
use xitdb::cursor::Writeable;
use xitdb::database::{Database, InitOpts};
use xitdb::hasher::Sha1Hasher;
use xitdb::hashmap::{HashMap, WriteHashMap};

type Db = Database<FileCore, Sha1Hasher>;
type AppState = Arc<Mutex<Db>>;

fn with_map(
    db: &AppState,
    f: impl FnOnce(
        &WriteHashMap<'_, FileCore, Sha1Hasher>,
    ) -> xitdb::error::Result<axum::response::Response>,
) -> axum::response::Response {
    let guard = match db.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let map = match WriteHashMap::new_write(&guard.root_cursor()) {
        Ok(map) => map,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let response = match f(&map) {
        Ok(response) => response,
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Release the lock before responding; nothing below borrows the map.
    drop(guard);
    response
}

async fn put_kv(
    State(db): State<AppState>,
    Path(key): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    with_map(&db, |m| {
        m.put(&key, Writeable::Bytes(body.to_vec()))?;
        Ok(StatusCode::OK.into_response())
    })
}

async fn get_kv(State(db): State<AppState>, Path(key): Path<String>) -> impl IntoResponse {
    let outcome: xitdb::error::Result<Option<Vec<u8>>> = (|| {
        let guard = match db.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let cursor = guard.root_cursor();
        let map = HashMap::new(cursor)?;
        let result = match map.get(&key)? {
            Some(cursor) => Ok(Some(cursor.read_bytes_alloc()?)),
            None => Ok(None),
        };
        // Release the lock before responding; the bytes are already owned.
        drop(guard);
        result
    })();
    match outcome {
        Ok(Some(value)) => (StatusCode::OK, value).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_kv(State(db): State<AppState>, Path(key): Path<String>) -> impl IntoResponse {
    with_map(&db, |m| {
        if m.remove(&key)? {
            Ok(StatusCode::OK.into_response())
        } else {
            Ok(StatusCode::NOT_FOUND.into_response())
        }
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "rest-kv.db".to_string());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let db: Db = Database::open(
        FileCore::new(file, true),
        Sha1Hasher,
        InitOpts {
            hash_id: None,
            file_id: None,
            fsync: true,
        },
    )?;
    let app: Router = Router::new()
        .route("/kv/{key}", put(put_kv).get(get_kv).delete(delete_kv))
        .with_state(Arc::new(Mutex::new(db)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!("serving {path} on http://127.0.0.1:3000");
    axum::serve(listener, app).await?;
    Ok(())
}
