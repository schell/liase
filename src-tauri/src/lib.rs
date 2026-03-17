use liase_wire_types::{AppConfig, AppError, ErrorKind, EventFilter, GhEvent};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tokio::sync::{Mutex, Notify};

mod config;
mod error;
mod github;
mod store;

use github::GitHubPoller;
use store::Store;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct App {
    config: config::RawConfig,
    #[allow(dead_code)]
    config_path: PathBuf,
    store: Arc<Store>,
    /// Signal the background poll task to wake up immediately.
    poll_notify: Arc<Notify>,
    /// The poller, if a token was configured.
    poller: Option<Arc<Mutex<GitHubPoller>>>,
}

impl App {
    fn new(config_path: PathBuf, db_path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let config = config::load_config(&config_path);
        let store = Arc::new(Store::open(&db_path)?);
        let poll_notify = Arc::new(Notify::new());

        // Create the poller only if we have a token
        let poller = config::resolve_token(&config).map(|token| {
            let p = GitHubPoller::new(&token, config.github.subscriptions.clone())
                .expect("could not create GitHub poller");
            Arc::new(Mutex::new(p))
        });

        Ok(App {
            config,
            config_path,
            store,
            poll_notify,
            poller,
        })
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn get_events(
    state: State<'_, App>,
    filter: EventFilter,
) -> Result<Vec<GhEvent>, AppError> {
    state
        .store
        .get_events(&filter)
        .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))
}

#[tauri::command]
async fn get_event(state: State<'_, App>, id: String) -> Result<Option<GhEvent>, AppError> {
    state
        .store
        .get_event(&id)
        .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))
}

#[tauri::command]
async fn mark_read(state: State<'_, App>, id: String) -> Result<(), AppError> {
    state
        .store
        .mark_read(&id)
        .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))
}

#[tauri::command]
async fn mark_all_read(state: State<'_, App>, repo: Option<String>) -> Result<(), AppError> {
    state
        .store
        .mark_all_read(repo.as_deref())
        .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))
}

#[tauri::command]
async fn get_config(state: State<'_, App>) -> Result<AppConfig, AppError> {
    Ok(config::to_app_config(&state.config))
}

#[tauri::command]
async fn poll_now(state: State<'_, App>) -> Result<(), AppError> {
    if state.poller.is_none() {
        return Err(AppError::new(
            ErrorKind::Config,
            "No GitHub token configured",
        ));
    }
    state.poll_notify.notify_one();
    Ok(())
}

// ---------------------------------------------------------------------------
// Background poll task
// ---------------------------------------------------------------------------

/// Runs the polling loop. Wakes on the configured interval or when
/// `poll_notify` is signalled (by the `poll_now` command).
async fn poll_task(
    poller: Arc<Mutex<GitHubPoller>>,
    store: Arc<Store>,
    poll_notify: Arc<Notify>,
    interval_secs: u64,
    app_handle: tauri::AppHandle,
) {
    let interval = std::time::Duration::from_secs(interval_secs);

    loop {
        // Wait for either the timer or an explicit wake-up
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                log::debug!("Poll task: timer elapsed");
            }
            _ = poll_notify.notified() => {
                log::info!("Poll task: woken up by poll_now");
            }
        }

        // Perform the poll
        let events = {
            let mut poller = poller.lock().await;
            poller.poll().await
        };

        if events.is_empty() {
            continue;
        }

        // Upsert into SQLite
        let mut new_count = 0u32;
        for event in &events {
            match store.upsert_event(event) {
                Ok(true) => new_count += 1,
                Ok(false) => {} // updated existing
                Err(e) => {
                    log::error!("Failed to upsert event {}: {e}", event.id);
                }
            }
        }

        if new_count > 0 {
            log::info!("Stored {new_count} new events ({} total upserted)", events.len());

            // Emit to the frontend so it can refresh
            if let Err(e) = app_handle.emit("new-events", new_count) {
                log::error!("Failed to emit new-events: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::builder().init();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                let window = app.get_webview_window("main").unwrap();
                window.open_devtools();
            }

            let app_data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));

            // Ensure the app data directory exists
            std::fs::create_dir_all(&app_data_dir)
                .expect("could not create app data directory");

            let config_path = app_data_dir.join("config.toml");
            let db_path = app_data_dir.join("liase.db");

            log::info!("Config path: {}", config_path.display());
            log::info!("Database path: {}", db_path.display());

            let app_state = App::new(config_path, db_path)
                .expect("could not initialize app state");

            let config_summary = config::to_app_config(&app_state.config);
            log::info!(
                "Loaded {} subscriptions, token configured: {}",
                config_summary.subscriptions.len(),
                config_summary.has_token,
            );

            // Clone what the background task needs before moving app_state
            let maybe_poller = app_state.poller.clone();
            let task_store = app_state.store.clone();
            let task_notify = app_state.poll_notify.clone();
            let poll_interval = app_state.config.github.poll_interval_secs;
            let app_handle = app.handle().clone();

            app.manage(app_state);

            // Spawn the background poll task if we have a token
            if let Some(poller) = maybe_poller {
                log::info!(
                    "Starting background poll task (interval: {poll_interval}s)"
                );
                tauri::async_runtime::spawn(async move {
                    poll_task(poller, task_store, task_notify, poll_interval, app_handle)
                        .await;
                });
            } else {
                log::warn!(
                    "No GitHub token configured — polling disabled. \
                     Set token in config.toml or GITHUB_TOKEN env var."
                );
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_events,
            get_event,
            mark_read,
            mark_all_read,
            get_config,
            poll_now,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
