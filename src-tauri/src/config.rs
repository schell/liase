//! TOML configuration loading.

use liase_wire_types::{AppConfig, Subscription, SubscriptionKind};
use std::path::PathBuf;

/// Raw TOML config structure (matches the config.toml file format).
#[derive(Debug, serde::Deserialize)]
pub struct RawConfig {
    pub github: GitHubConfig,
}

#[derive(Debug, serde::Deserialize)]
pub struct GitHubConfig {
    pub token: Option<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub subscriptions: Vec<RawSubscription>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawSubscription {
    pub kind: String,
    pub name: String,
}

fn default_poll_interval() -> u64 {
    60
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            github: GitHubConfig {
                token: None,
                poll_interval_secs: default_poll_interval(),
                subscriptions: Vec::new(),
            },
        }
    }
}

/// Load the config file from disk, falling back to defaults if missing.
pub fn load_config(path: &PathBuf) -> RawConfig {
    if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(config) => config,
                Err(e) => {
                    log::error!("Failed to parse config at {}: {e}", path.display());
                    RawConfig::default()
                }
            },
            Err(e) => {
                log::error!("Failed to read config at {}: {e}", path.display());
                RawConfig::default()
            }
        }
    } else {
        log::info!("No config file at {}, using defaults", path.display());
        RawConfig::default()
    }
}

/// Resolve the GitHub token: config file value takes priority over env var.
pub fn resolve_token(config: &RawConfig) -> Option<String> {
    config
        .github
        .token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
}

/// Convert a RawConfig into the wire-type AppConfig for the frontend.
pub fn to_app_config(config: &RawConfig) -> AppConfig {
    let subscriptions = config
        .github
        .subscriptions
        .iter()
        .map(|s| Subscription {
            kind: match s.kind.as_str() {
                "org" => SubscriptionKind::Org,
                _ => SubscriptionKind::Repo,
            },
            name: s.name.clone(),
        })
        .collect();

    AppConfig {
        poll_interval_secs: config.github.poll_interval_secs,
        subscriptions,
        has_token: resolve_token(config).is_some(),
    }
}
