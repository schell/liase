//! TOML configuration loading and saving.

use liase_wire_types::{AppConfig, Subscription, SubscriptionKind};
use std::path::{Path, PathBuf};

/// Raw TOML config structure (matches the config.toml file format).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RawConfig {
    pub github: GitHubConfig,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct GitHubConfig {
    pub token: Option<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub subscriptions: Vec<RawSubscription>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// Save the config to disk as TOML.
pub fn save_config(path: &Path, config: &RawConfig) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, toml_str)
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
    let token = resolve_token(config);
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
        has_token: token.is_some(),
        token,
    }
}

/// Convert a wire-type AppConfig back into a RawConfig for saving.
pub fn from_app_config(app_config: &AppConfig) -> RawConfig {
    RawConfig {
        github: GitHubConfig {
            token: app_config.token.clone(),
            poll_interval_secs: app_config.poll_interval_secs,
            subscriptions: app_config
                .subscriptions
                .iter()
                .map(|s| RawSubscription {
                    kind: match s.kind {
                        SubscriptionKind::Org => "org".to_string(),
                        SubscriptionKind::Repo => "repo".to_string(),
                    },
                    name: s.name.clone(),
                })
                .collect(),
        },
    }
}
