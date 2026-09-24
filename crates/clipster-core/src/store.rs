//! SQLite-backed history storage.

use crate::ipc::{Item, Summary};
use crate::{content_hash, now_millis, preview};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Characters of content retained in a list preview.
const PREVIEW_CHARS: usize = 160;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    hash        TEXT    NOT NULL UNIQUE,
    content     TEXT    NOT NULL,
    preview     TEXT    NOT NULL,
    bytes       INTEGER NOT NULL,
    pinned      INTEGER NOT NULL DEFAULT 0,
    label       TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    -- Recency rank, bumped on every touch. Ordering deliberately does NOT use
    -- updated_at: two operations can land in the same millisecond, and the
    -- wall clock can jump backwards across NTP steps and suspend/resume.
    -- Neither can reorder a counter. updated_at stays for display and for the
    -- age-based auto-clear planned in v1.0.
    seq         INTEGER NOT NULL
);
-- Index columns mirror the ORDER BY of each query exactly, so both are served
-- by an index scan rather than a sort.
CREATE INDEX IF NOT EXISTS idx_items_recent ON items(seq DESC);
CREATE INDEX IF NOT EXISTS idx_items_pinned ON items(pinned DESC, seq DESC);
"#;

/// What `insert` did, so the daemon can log something meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    /// A new row was created.
    New(i64),
    /// Content already present; its recency was refreshed instead.
    Bumped(i64),
}

impl Inserted {
    pub fn id(self) -> i64 {
        match self {
            Inserted::New(id) | Inserted::Bumped(id) => id,
        }
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database at {}", path.display()))?;

        // WAL is the "no data loss on crash" requirement from the PRD: commits
        // are durable once written to the log, and a killed daemon recovers on
        // next open. NORMAL synchronous is the usual WAL pairing — it can lose
        // the last transactions only on OS/power failure, not on process death.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=3000;",
        )
        .context("applying pragmas")?;
        conn.execute_batch(SCHEMA).context("creating schema")?;

        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Record a clipboard entry.
    ///
    /// Deduplication is by content hash across the whole history, not just
    /// against the previous entry: re-copying something you copied last week
    /// should move it back to the top rather than create a second row.
    pub fn insert(&self, content: &str) -> Result<Inserted> {
        let hash = content_hash(content);
        let now = now_millis();

        let existing: Option<i64> = self
            .conn
            .query_row("SELECT id FROM items WHERE hash = ?1", params![hash], |r| r.get(0))
            .optional()?;

        // Safe to read-then-write without an explicit transaction: every
        // caller holds the store mutex, and this is the only writer.
        let seq = self.next_seq()?;

        if let Some(id) = existing {
            self.conn.execute(
                "UPDATE items SET updated_at = ?1, seq = ?2 WHERE id = ?3",
                params![now, seq, id],
            )?;
            return Ok(Inserted::Bumped(id));
        }

        self.conn.execute(
            "INSERT INTO items (hash, content, preview, bytes, pinned, label, created_at, updated_at, seq)
             VALUES (?1, ?2, ?3, ?4, 0, NULL, ?5, ?5, ?6)",
            params![hash, content, preview(content, PREVIEW_CHARS), content.len() as i64, now, seq],
        )?;
        Ok(Inserted::New(self.conn.last_insert_rowid()))
    }

    fn next_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM items", [], |r| r.get(0))?)
    }

    pub fn list(&self, limit: Option<usize>, pinned_only: bool) -> Result<Vec<Summary>> {
        // Pinned entries sort first so the CLI and the rofi script both get the
        // "separate section" the PRD asks for without re-sorting client side.
        let sql = "SELECT id, preview, pinned, label, bytes, created_at, updated_at
                     FROM items
                    WHERE (?1 = 0 OR pinned = 1)
                 ORDER BY pinned DESC, seq DESC
                    LIMIT ?2";
        let limit = limit.map(|l| l as i64).unwrap_or(-1); // -1 means no limit in SQLite
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(params![pinned_only as i64, limit], |r| {
            Ok(Summary {
                id: r.get(0)?,
                preview: r.get(1)?,
                pinned: r.get::<_, i64>(2)? != 0,
                label: r.get(3)?,
                bytes: r.get::<_, i64>(4)? as usize,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Item>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, content, pinned, label, created_at, updated_at FROM items WHERE id = ?1",
        )?;
        Ok(stmt.query_row(params![id], Self::row_to_item).optional()?)
    }

    pub fn latest(&self) -> Result<Option<Item>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, content, pinned, label, created_at, updated_at
               FROM items ORDER BY seq DESC LIMIT 1",
        )?;
        Ok(stmt.query_row([], Self::row_to_item).optional()?)
    }

    fn row_to_item(r: &rusqlite::Row) -> rusqlite::Result<Item> {
        Ok(Item {
            id: r.get(0)?,
            content: r.get(1)?,
            pinned: r.get::<_, i64>(2)? != 0,
            label: r.get(3)?,
            created_at: r.get(4)?,
            updated_at: r.get(5)?,
        })
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE items SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?)
    }

    /// Setting a label on an unpinned entry pins it: a named snippet that can
    /// be evicted is a bug report waiting to happen.
    pub fn set_label(&self, id: i64, label: Option<&str>) -> Result<usize> {
        let affected = self
            .conn
            .execute("UPDATE items SET label = ?1 WHERE id = ?2", params![label, id])?;
        if affected > 0 && label.is_some() {
            self.set_pinned(id, true)?;
        }
        Ok(affected)
    }

    pub fn remove(&self, id: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM items WHERE id = ?1", params![id])?)
    }

    pub fn clear(&self, keep_pinned: bool) -> Result<usize> {
        let n = if keep_pinned {
            self.conn.execute("DELETE FROM items WHERE pinned = 0", [])?
        } else {
            self.conn.execute("DELETE FROM items", [])?
        };
        // Reclaim the pages: "clear history" should shrink the file on disk,
        // otherwise the cleared content is still sitting in the freelist.
        self.conn.execute_batch("VACUUM;")?;
        Ok(n)
    }

    /// Trim unpinned entries down to `history_size`, oldest first.
    pub fn evict(&self, history_size: usize) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM items
              WHERE pinned = 0
                AND id NOT IN (
                    SELECT id FROM items WHERE pinned = 0
                     ORDER BY seq DESC LIMIT ?1
                )",
            params![history_size as i64],
        )?)
    }

    /// `(total, pinned)`
    pub fn counts(&self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(pinned), 0) FROM items",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn insert_returns_new_then_bumped_for_same_content() {
        let s = store();
        let first = s.insert("hello").unwrap();
        assert!(matches!(first, Inserted::New(_)));
        let second = s.insert("hello").unwrap();
        assert_eq!(second, Inserted::Bumped(first.id()));
        assert_eq!(s.counts().unwrap().0, 1);
    }

    /// Regression: with second-resolution timestamps, re-copying an entry in
    /// the same second as a newer capture left it stranded down the list.
    #[test]
    fn bumped_entry_outranks_one_captured_moments_earlier() {
        let s = store();
        let old = s.insert("old").unwrap().id();
        s.insert("new").unwrap();
        // No sleep: back-to-back operations are exactly the case that broke.
        assert_eq!(s.insert("old").unwrap(), Inserted::Bumped(old));

        assert_eq!(s.list(None, false).unwrap()[0].id, old);
        assert_eq!(s.latest().unwrap().unwrap().id, old);
    }

    #[test]
    fn reinserting_moves_entry_back_to_top() {
        let s = store();
        let a = s.insert("a").unwrap().id();
        s.insert("b").unwrap();
        s.insert("c").unwrap();
        // Timestamps have second granularity, so order by updated_at alone is
        // ambiguous within a test. Force a distinct, later timestamp.
        s.conn
            .execute("UPDATE items SET seq = seq + 10 WHERE id = ?1", params![a])
            .unwrap();
        assert_eq!(s.list(None, false).unwrap()[0].id, a);
    }

    #[test]
    fn eviction_drops_oldest_unpinned_only() {
        let s = store();
        for i in 0..10 {
            let id = s.insert(&format!("item {i}")).unwrap().id();
            s.conn
                .execute("UPDATE items SET seq = ?1 WHERE id = ?2", params![i, id])
                .unwrap();
        }
        let oldest = s.list(None, false).unwrap().last().unwrap().id;
        s.set_pinned(oldest, true).unwrap();

        let removed = s.evict(3).unwrap();
        assert_eq!(removed, 6); // 10 total - 1 pinned - 3 kept

        let remaining = s.list(None, false).unwrap();
        assert_eq!(remaining.len(), 4);
        assert!(remaining.iter().any(|i| i.id == oldest), "pinned entry was evicted");
    }

    #[test]
    fn eviction_is_a_no_op_below_the_limit() {
        let s = store();
        s.insert("only").unwrap();
        assert_eq!(s.evict(500).unwrap(), 0);
        assert_eq!(s.counts().unwrap().0, 1);
    }

    #[test]
    fn pinned_entries_sort_first() {
        let s = store();
        s.insert("older").unwrap();
        let newer = s.insert("newer").unwrap().id();
        let older = s.list(None, false).unwrap().last().unwrap().id;
        s.set_pinned(older, true).unwrap();

        let list = s.list(None, false).unwrap();
        assert_eq!(list[0].id, older);
        assert_eq!(list[1].id, newer);
    }

    #[test]
    fn clear_keeps_pinned_when_asked() {
        let s = store();
        let keep = s.insert("keep").unwrap().id();
        s.insert("drop").unwrap();
        s.set_pinned(keep, true).unwrap();

        assert_eq!(s.clear(true).unwrap(), 1);
        assert_eq!(s.counts().unwrap(), (1, 1));

        assert_eq!(s.clear(false).unwrap(), 1);
        assert_eq!(s.counts().unwrap(), (0, 0));
    }

    #[test]
    fn labelling_pins_the_entry() {
        let s = store();
        let id = s.insert("git commit -m").unwrap().id();
        s.set_label(id, Some("commit template")).unwrap();
        let item = s.get(id).unwrap().unwrap();
        assert!(item.pinned);
        assert_eq!(item.label.as_deref(), Some("commit template"));
    }

    #[test]
    fn multiline_content_round_trips_intact() {
        let s = store();
        let content = "line one\nline two\n\ttabbed";
        let id = s.insert(content).unwrap().id();
        assert_eq!(s.get(id).unwrap().unwrap().content, content);
        assert_eq!(s.list(None, false).unwrap()[0].preview, "line one line two tabbed");
    }

    #[test]
    fn get_and_remove_report_missing_ids() {
        let s = store();
        assert!(s.get(999).unwrap().is_none());
        assert_eq!(s.remove(999).unwrap(), 0);
    }

    #[test]
    fn pinned_only_filter_excludes_the_rest() {
        let s = store();
        s.insert("plain").unwrap();
        let id = s.insert("pinned").unwrap().id();
        s.set_pinned(id, true).unwrap();
        let list = s.list(None, true).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
    }
}
