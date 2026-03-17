//! Domain-specific error types using `snafu`.

use liase_wire_types::{AppError, ErrorKind};
use snafu::Snafu;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// GitHub API errors
// ---------------------------------------------------------------------------

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum GitHubError {
    #[snafu(display("GitHub API error: {message}"))]
    Api { message: String },

    #[snafu(display("GitHub rate limit exceeded, resets at {reset_at}"))]
    RateLimit { reset_at: String },

    #[snafu(display("GitHub authentication failed: {message}"))]
    Auth { message: String },
}

impl From<GitHubError> for AppError {
    fn from(e: GitHubError) -> Self {
        AppError::new(ErrorKind::GitHub, e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Config errors
// ---------------------------------------------------------------------------

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum ConfigError {
    #[snafu(display("Failed to read config from '{}': {source}", path.display()))]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("Failed to parse config: {source}"))]
    Parse { source: toml::de::Error },

    #[snafu(display("Failed to create config directory '{}': {source}", path.display()))]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("Failed to write config to '{}': {source}", path.display()))]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display(
        "No GitHub token configured. Set 'token' in config.toml or GITHUB_TOKEN env var."
    ))]
    NoToken,
}

impl From<ConfigError> for AppError {
    fn from(e: ConfigError) -> Self {
        AppError::new(ErrorKind::Config, e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Database errors
// ---------------------------------------------------------------------------

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum DbError {
    #[snafu(display("Database error: {message}"))]
    Store { message: String },

    #[snafu(display("Database migration failed: {message}"))]
    Migration { message: String },
}

impl From<DbError> for AppError {
    fn from(e: DbError) -> Self {
        AppError::new(ErrorKind::Database, e.to_string())
    }
}

impl From<crate::store::StoreError> for DbError {
    fn from(e: crate::store::StoreError) -> Self {
        DbError::Store {
            message: e.to_string(),
        }
    }
}
