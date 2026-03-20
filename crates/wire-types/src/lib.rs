//! Wire types for sending between BE<->FE.
use snafu::prelude::*;
use tymigrawr::{HasCrudFields, IsCrudField};

/// What kind of GitHub event this represents.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum EventKind {
    NewIssue,
    NewPullRequest,
    NewComment,
    IssueStateChange,
    PRStateChange,
}

impl From<EventKind> for u32 {
    fn from(value: EventKind) -> Self {
        match value {
            EventKind::NewIssue => 0,
            EventKind::NewPullRequest => 1,
            EventKind::NewComment => 2,
            EventKind::IssueStateChange => 3,
            EventKind::PRStateChange => 4,
        }
    }
}

impl TryFrom<u32> for EventKind {
    type Error = snafu::Whatever;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => EventKind::NewIssue,
            1 => EventKind::NewPullRequest,
            2 => EventKind::NewComment,
            3 => EventKind::IssueStateChange,
            4 => EventKind::PRStateChange,
            n => snafu::whatever!("{n} is not an event. Expected 0-4 inclusive."),
        })
    }
}

impl EventKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NewIssue => "Issue",
            Self::NewPullRequest => "PR",
            Self::NewComment => "Comment",
            Self::IssueStateChange => "Issue",
            Self::PRStateChange => "PR",
        }
    }

    pub fn badge_class(&self) -> &'static str {
        match self {
            Self::NewIssue => "bg-success",
            Self::NewPullRequest => "bg-primary",
            Self::NewComment => "bg-secondary",
            Self::IssueStateChange => "bg-warning text-dark",
            Self::PRStateChange => "bg-info text-dark",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A GitHub event (issue, PR, or comment) normalized for display.
///
/// Version 1 of the events table schema.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, HasCrudFields)]
pub struct GhEventV1 {
    /// Unique identifier: "{owner}/{repo}#{number}" or
    /// "{owner}/{repo}#{number}:comment:{comment_id}"
    #[primary_key]
    pub id: String,
    /// EventKind
    pub kind: u32,
    /// Repository full name, e.g. "zcash/librustzcash"
    pub repo: String,
    #[json_text]
    /// Issue or PR number
    pub number: u64,
    /// Issue or PR title
    pub title: String,
    /// GitHub username of the author
    pub author: String,
    /// Avatar URL for the author
    pub avatar_url: Option<String>,
    /// Markdown body text
    pub body: String,
    /// HTML URL to open in the browser
    pub url: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Whether the user has read this event
    pub read: bool,
}

impl GhEventV1 {
    pub fn kind(&self) -> Result<EventKind, snafu::Whatever> {
        self.kind.try_into()
    }
}

/// The current row type. When we add a V2, this alias moves forward and
/// we add a migration chain.
pub type GhEvent = GhEventV1;

/// What kind of subscription this is.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum SubscriptionKind {
    Org,
    Repo,
}

impl std::fmt::Display for SubscriptionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Org => f.write_str("org"),
            Self::Repo => f.write_str("repo"),
        }
    }
}

/// A subscription to an org or repo.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Subscription {
    pub kind: SubscriptionKind,
    /// Org name (e.g. "zcash") or repo full name (e.g. "schell/mogwai")
    pub name: String,
}

/// App configuration as seen by the frontend.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AppConfig {
    pub poll_interval_secs: u64,
    pub subscriptions: Vec<Subscription>,
    /// Whether a GitHub token is configured (never exposes the actual token).
    pub has_token: bool,
}

/// Status of a subscription (for the settings/status view).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SubStatus {
    pub subscription: Subscription,
    /// Number of repos being monitored (for orgs, the discovered repo count).
    pub repo_count: u32,
    /// Last successful poll time (ISO 8601), if any.
    pub last_poll: Option<String>,
    /// Error message from last poll, if any.
    pub error: Option<String>,
}

/// Filter for querying events.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct EventFilter {
    /// Filter to a specific repo (e.g. "zcash/librustzcash").
    pub repo: Option<String>,
    /// If true, only return unread events.
    pub unread_only: bool,
    /// Maximum number of events to return.
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// Typed IPC channel: Command / Response / ServerEvent
// ---------------------------------------------------------------------------

/// Commands sent from the frontend to the backend via `invoke`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum Command {
    /// Fetch events matching a filter.
    GetEvents(EventFilter),
    /// Fetch a single event by ID.
    GetEvent { id: String },
    /// Fetch the current app configuration.
    GetConfig,
    /// Trigger an immediate GitHub poll cycle.
    PollNow,
    /// Mark a single event as read.
    MarkRead { id: String },
    /// Mark all events as read (optionally filtered by repo).
    MarkAllRead { repo: Option<String> },
}

/// Responses returned from the backend to the frontend (paired with a Command).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    Events(Vec<GhEvent>),
    Event(Option<GhEvent>),
    Config(AppConfig),
    Ok,
}

impl Response {
    /// Unwrap a `Response::Events`, or return an error.
    pub fn into_events(self) -> Result<Vec<GhEvent>, AppError> {
        match self {
            Response::Events(events) => Ok(events),
            other => Err(AppError::new(
                ErrorKind::Serialization,
                format!("expected Response::Events, got {other:?}"),
            )),
        }
    }

    /// Unwrap a `Response::Event`, or return an error.
    pub fn into_event(self) -> Result<Option<GhEvent>, AppError> {
        match self {
            Response::Event(event) => Ok(event),
            other => Err(AppError::new(
                ErrorKind::Serialization,
                format!("expected Response::Event, got {other:?}"),
            )),
        }
    }

    /// Unwrap a `Response::Config`, or return an error.
    pub fn into_config(self) -> Result<AppConfig, AppError> {
        match self {
            Response::Config(config) => Ok(config),
            other => Err(AppError::new(
                ErrorKind::Serialization,
                format!("expected Response::Config, got {other:?}"),
            )),
        }
    }

    /// Unwrap a `Response::Ok`, or return an error.
    pub fn into_ok(self) -> Result<(), AppError> {
        match self {
            Response::Ok => Ok(()),
            other => Err(AppError::new(
                ErrorKind::Serialization,
                format!("expected Response::Ok, got {other:?}"),
            )),
        }
    }
}

/// Unsolicited events pushed from the backend to the frontend via `emit`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum ServerEvent {
    /// New events were stored after a poll cycle.
    NewEvents { count: u32 },
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Categorises errors so the frontend can branch on the kind.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ErrorKind {
    /// GitHub API errors (auth, rate limit, network, etc.).
    GitHub,
    /// Configuration file I/O or parsing errors.
    Config,
    /// Database errors.
    Database,
    /// Serialisation / deserialisation errors on the invoke bridge.
    Serialization,
}

/// Application error sent across the Tauri invoke bridge.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AppError {
    pub kind: ErrorKind,
    pub message: String,
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
