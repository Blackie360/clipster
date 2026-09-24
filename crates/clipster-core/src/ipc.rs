//! Wire protocol between `clipster` and `clipsterd`.
//!
//! Newline-delimited JSON over a Unix domain socket: one request line, one
//! response line, connection closed. Serialized JSON never contains a raw
//! newline, so the framing is unambiguous even for multi-line clipboard
//! content. JSON over a binary format is a deliberate MVP trade — it keeps
//! `clipster list | ...` scriptable and the protocol inspectable with `socat`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Newest first. `limit` of `None` means "everything".
    List { limit: Option<usize>, pinned_only: bool },
    Get { id: i64 },
    /// Most recent entry; the id-less path used by scripts.
    Latest,
    SetPinned { id: i64, pinned: bool },
    SetLabel { id: i64, label: Option<String> },
    Remove { id: i64 },
    Clear { keep_pinned: bool },
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Items(Vec<Summary>),
    Item(Item),
    /// Number of rows affected, for mutating commands.
    Affected(usize),
    Status(Status),
    NotFound { id: i64 },
    Error { message: String },
}

/// A history entry without its content, for list views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: i64,
    pub preview: String,
    pub pinned: bool,
    pub label: Option<String>,
    pub bytes: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A history entry with its full content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: i64,
    pub content: String,
    pub pinned: bool,
    pub label: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub backend: String,
    pub total: i64,
    pub pinned: i64,
    pub history_size: usize,
    pub captured_since_start: u64,
    pub db_path: String,
}
