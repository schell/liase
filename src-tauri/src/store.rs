//! SQLite storage for GitHub events, using tymigrawr for schema management
//! and the sqlite crate for queries.

use liase_wire_types::{EventFilter, GhEvent};
use std::path::Path;
use std::sync::Mutex;
use tymigrawr::{Crud, HasCrudFields, Sqlite};

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct Store {
    conn: Mutex<sqlite::Connection>,
}

/// A simple error wrapper so callers don't need to know about sqlite vs snafu.
#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<sqlite::Error> for StoreError {
    fn from(e: sqlite::Error) -> Self {
        StoreError(e.to_string())
    }
}

impl From<snafu::Whatever> for StoreError {
    fn from(e: snafu::Whatever) -> Self {
        StoreError(e.to_string())
    }
}

impl Store {
    /// Open (or create) the database at the given path and run migrations.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = sqlite::Connection::open(path)?;
        conn.execute("PRAGMA journal_mode=WAL;")?;
        conn.execute("PRAGMA foreign_keys=ON;")?;

        // Use tymigrawr to create the table
        <GhEvent as Crud<Sqlite>>::create(&conn)?;

        // Create our custom indexes (tymigrawr doesn't manage indexes)
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_gheventv1_timestamp
                 ON gheventv1(timestamp DESC);",
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_gheventv1_repo
                 ON gheventv1(repo, timestamp DESC);",
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_gheventv1_unread
                 ON gheventv1(read, timestamp DESC);",
        )?;

        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// Insert or update an event. Returns true if the row was inserted/updated.
    pub fn upsert_event(&self, event: &GhEvent) -> Result<bool, StoreError> {
        let conn = self.conn.lock().unwrap();
        let changed = event.upsert(&*conn)?;
        Ok(changed)
    }

    /// Query events with optional filtering.
    pub fn get_events(&self, filter: &EventFilter) -> Result<Vec<GhEvent>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let table = GhEvent::table_name();
        let mut sql = format!("SELECT * FROM {table} WHERE 1=1");

        if let Some(ref repo) = filter.repo {
            // We'll bind this below
            sql.push_str(" AND repo = :repo");
            let _ = repo; // used later
        }
        if filter.unread_only {
            sql.push_str(" AND read = 0");
        }

        sql.push_str(" ORDER BY timestamp DESC");

        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let mut stmt = conn.prepare(&sql)?;
        if let Some(ref repo) = filter.repo {
            stmt.bind((":repo", repo.as_str()))?;
        }

        let mut events = Vec::new();
        while let Ok(sqlite::State::Row) = stmt.next() {
            events.push(row_to_gh_event(&stmt)?);
        }
        Ok(events)
    }

    /// Get a single event by ID.
    pub fn get_event(&self, id: &str) -> Result<Option<GhEvent>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let table = GhEvent::table_name();
        let sql = format!("SELECT * FROM {table} WHERE id = :id");
        let mut stmt = conn.prepare(&sql)?;
        stmt.bind((":id", id))?;
        match stmt.next() {
            Ok(sqlite::State::Row) => Ok(Some(row_to_gh_event(&stmt)?)),
            _ => Ok(None),
        }
    }

    /// Mark an event as read.
    pub fn mark_read(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let table = GhEvent::table_name();
        let sql = format!("UPDATE {table} SET read = 1 WHERE id = :id");
        let mut stmt = conn.prepare(&sql)?;
        stmt.bind((":id", id))?;
        while let Ok(sqlite::State::Row) = stmt.next() {}
        Ok(())
    }

    /// Mark all events as read, optionally filtered by repo.
    pub fn mark_all_read(&self, repo: Option<&str>) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let table = GhEvent::table_name();
        match repo {
            Some(repo) => {
                let sql = format!("UPDATE {table} SET read = 1 WHERE repo = :repo");
                let mut stmt = conn.prepare(&sql)?;
                stmt.bind((":repo", repo))?;
                while let Ok(sqlite::State::Row) = stmt.next() {}
            }
            None => {
                let sql = format!("UPDATE {table} SET read = 1");
                conn.execute(&sql)?;
            }
        }
        Ok(())
    }

    /// Get the list of distinct repos that have unread events.
    pub fn get_repos(&self) -> Result<Vec<(String, u32)>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let table = GhEvent::table_name();
        let sql = format!(
            "SELECT repo, COUNT(*) as cnt FROM {table}
             WHERE read = 0
             GROUP BY repo ORDER BY cnt DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut repos = Vec::new();
        while let Ok(sqlite::State::Row) = stmt.next() {
            let repo: String = stmt.read("repo")?;
            let cnt: i64 = stmt.read("cnt")?;
            repos.push((repo, cnt as u32));
        }
        Ok(repos)
    }
}

/// Extract a GhEvent from the current row of a prepared statement.
fn row_to_gh_event(stmt: &sqlite::Statement<'_>) -> Result<GhEvent, StoreError> {
    let event = tymigrawr::try_from_row(stmt)?;
    Ok(event)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn event_row() {
        let conn = sqlite::Connection::open(":memory:").unwrap();
        GhEvent::create(&conn).unwrap();

        for i in 0..10 {
            let event = GhEvent {
                id: format!("blah/blah:{i}"),
                ..Default::default()
            };
            let inserted = event.upsert(&conn).unwrap();
            assert!(inserted);
        }
    }
}
