# liase v0.1 — GitHub Monitor

**Date:** 2026-03-10
**Author:** schell + claude-opus-4-6
**Status:** Active
**Supersedes:** 2026-02-17-init.md (reduced scope)

## Scope

A read-only Tauri v2 desktop app that monitors GitHub organizations and repositories
for new issues, PRs, and comments. Displays them in a unified timeline. Config-driven
via TOML. No AI, no replying, no other platforms.

## Technology Stack

| Layer | Choice | Rationale |
|---|---|---|
| Desktop framework | Tauri v2 | Native webview, Rust backend, small binary |
| Frontend framework | mogwai 0.7 (Rust-to-WASM) | Single-language stack, proven patterns from winatep/privateer |
| Component library | iti (embed-assets, system9) | Bootstrap 5 components + System-9 theme, same as privateer |
| Build tool | Trunk | WASM compilation + asset bundling |
| GitHub API | octocrab | Full-featured, well-maintained GitHub API client |
| Database | SQLite via rusqlite (bundled) | Local-first, WAL mode, FTS5 for search |
| Config | TOML file in Tauri app data dir | Human-editable, loaded at startup |
| Secrets | `GITHUB_TOKEN` env var (stronghold later) | Simple for v0.1 |

## Crate Structure

```
liase/
├── Cargo.toml                  # Workspace root (also the frontend WASM crate)
├── plans/
├── config.example.toml         # Example config
│
├── crates/
│   └── wire-types/             # Shared types (edition 2024)
│       ├── Cargo.toml
│       └── src/lib.rs
│
├── src/                        # Frontend WASM (root crate, privateer-style)
│   ├── main.rs                 # Entry: inject iti styles, create App, step loop
│   ├── app.rs                  # App shell: tabs + pane switching
│   ├── app/
│   │   ├── timeline.rs         # Event timeline view (main content)
│   │   ├── detail.rs           # Event detail view (issue/PR/comment body)
│   │   └── settings.rs         # Settings view (config status, connection info)
│   └── invoke.rs               # Tauri IPC wrappers
│
├── Trunk.toml
├── index.html
│
└── src-tauri/                  # Tauri v2 backend
    ├── Cargo.toml
    ├── build.rs
    ├── tauri.conf.json
    ├── capabilities/
    │   └── default.json
    └── src/
        ├── main.rs             # Tauri setup, commands, managed state
        ├── github.rs           # GitHub polling logic (octocrab)
        ├── store.rs            # SQLite schema, queries, migrations
        └── config.rs           # TOML config loading/parsing
```

## Config File

Located at `{tauri_app_data_dir}/config.toml`. Example:

```toml
[github]
# Token can also be set via GITHUB_TOKEN env var
# token = "ghp_..."
poll_interval_secs = 60

[[github.subscriptions]]
kind = "org"
name = "zcash"

[[github.subscriptions]]
kind = "repo"
name = "schell/mogwai"

[[github.subscriptions]]
kind = "repo"
name = "AcmeInc/some-project"
```

- `kind = "org"` — auto-discovers all repos in that org, monitors issues + PRs + comments
- `kind = "repo"` — monitors a specific `owner/repo`
- Token resolution: config file `token` field → `GITHUB_TOKEN` env var

## Wire Types

```rust
pub enum EventKind {
    NewIssue,
    NewPullRequest,
    NewComment,
    IssueStateChange,
    PRStateChange,
}

pub struct GhEvent {
    pub id: String,              // "{owner}/{repo}#{number}:{comment_id}"
    pub kind: EventKind,
    pub repo: String,            // "zcash/librustzcash"
    pub number: u64,             // issue/PR number
    pub title: String,           // issue/PR title
    pub author: String,          // GitHub username
    pub avatar_url: Option<String>,
    pub body: String,            // markdown body
    pub url: String,             // HTML URL
    pub timestamp: String,       // ISO 8601
    pub read: bool,
}

pub struct Subscription {
    pub kind: SubscriptionKind,
    pub name: String,
}

pub enum SubscriptionKind {
    Org,
    Repo,
}

pub struct AppConfig {
    pub poll_interval_secs: u64,
    pub subscriptions: Vec<Subscription>,
}
```

## SQLite Schema

```sql
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    author TEXT NOT NULL,
    avatar_url TEXT,
    body TEXT NOT NULL,
    url TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    read INTEGER DEFAULT 0,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_events_timestamp ON events(timestamp DESC);
CREATE INDEX idx_events_repo ON events(repo, timestamp DESC);
CREATE INDEX idx_events_unread ON events(read, timestamp DESC);

CREATE VIRTUAL TABLE events_fts USING fts5(
    title, body, author,
    content='events', content_rowid='rowid'
);
```

## Tauri Commands

```rust
#[tauri::command] async fn get_events(filter: EventFilter) -> Result<Vec<GhEvent>, AppError>;
#[tauri::command] async fn get_event(id: String) -> Result<GhEvent, AppError>;
#[tauri::command] async fn mark_read(id: String) -> Result<(), AppError>;
#[tauri::command] async fn mark_all_read(repo: Option<String>) -> Result<(), AppError>;
#[tauri::command] async fn get_config() -> Result<AppConfig, AppError>;
#[tauri::command] async fn get_subscriptions_status() -> Result<Vec<SubStatus>, AppError>;
#[tauri::command] async fn search(query: String) -> Result<Vec<GhEvent>, AppError>;
#[tauri::command] async fn poll_now() -> Result<u32, AppError>;
```

Backend emits events:
```rust
app_handle.emit("new-events", &new_events)?;
```

## UI Layout

```
+----------------------------------------------------------+
| liase                                         [_][#][x]  |
+-------------+--------------------------------------------+
| Filters     | Event Timeline                             |
|             |                                            |
| * All       | [PR] zcash/librustzcash #847         2m   |
| * Unread    |   "Add ZIP-317 fee calculation"            |
|             |   @user1                                   |
| ----------- |                                            |
| Orgs        | [Issue] zcash/zcashd #4521            5m   |
|  zcash (24) |   "Wallet sync fails on testnet"           |
|             |   @user2                                   |
| Repos       |                                            |
|  schell/    | [Comment] zcash/halo2 #312            12m  |
|   mogwai(3) |   Re: "Proof verification bug"             |
|             |   @user3                                   |
| ----------- |                                            |
| [Search___] | [Issue] schell/mogwai #89              1h  |
|             |   "ViewChild derive panics on..."          |
| ----------- |   @someone                                 |
| Settings    |                                            |
+-------------+--------------------------------------------+
```

Two-pane layout. Clicking an event opens it in the browser via `tauri-plugin-opener`.

## GitHub Polling Strategy

1. **Org subscriptions**: On startup (and periodically), call `GET /orgs/{org}/repos`
   to discover all repos. Then poll each repo.
2. **Per-repo polling**: For each subscribed repo, poll:
   - `GET /repos/{owner}/{repo}/issues?state=all&sort=updated&since={last_poll}`
   - `GET /repos/{owner}/{repo}/issues/comments?sort=updated&since={last_poll}`
3. **Rate limiting**: GitHub allows 5,000 req/hour with a PAT. Mitigations:
   - Conditional requests (`If-Modified-Since` / ETags)
   - Stagger repo polling
   - Respect `X-RateLimit-Remaining`, back off when low
4. **Deduplication**: Events upserted by unique ID.

## Implementation Phases

### Phase 1: Skeleton
- Workspace scaffold, all crates compile
- Tauri app launches with iti shell (tabs, empty timeline)
- Component sandbox via `trunk serve`

### Phase 2: Config + GitHub Polling
- TOML config parsing
- octocrab integration
- Org repo discovery
- Issue/PR/comment polling
- SQLite storage (schema, upsert, queries)

### Phase 3: Frontend Wiring
- Timeline view showing real events via Tauri IPC
- Filtering by repo/org/read status
- Click-to-open-in-browser

### Phase 4: Polish
- Full-text search (FTS5)
- Unread counts in sidebar
- Manual refresh button
- Connection status indicator
- Rate limit awareness
