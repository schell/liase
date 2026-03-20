use liase_wire_types::{AppError, Command, ErrorKind, Response, ServerEvent};
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
    config: Mutex<config::RawConfig>,
    config_path: PathBuf,
    store: Arc<Store>,
    /// Signal the background poll task to wake up immediately.
    poll_notify: Arc<Notify>,
    /// The poller, if a token was configured. Wrapped in `Arc` so it can be
    /// shared with the setup task that creates the poller in an async context.
    poller: Arc<Mutex<Option<Arc<Mutex<GitHubPoller>>>>>,
    /// Handle to the background poll task (so we can abort + respawn).
    poll_task_handle: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Tauri app handle (set during setup, needed for spawning tasks / emitting).
    app_handle: Mutex<Option<tauri::AppHandle>>,
}

impl App {
    fn new(config_path: PathBuf, db_path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let raw_config = config::load_config(&config_path);
        let store = Arc::new(Store::open(&db_path)?);
        let poll_notify = Arc::new(Notify::new());

        // The poller is created later in an async context (Tokio runtime
        // required by octocrab/tower). See the setup block in `run()`.
        Ok(App {
            config: Mutex::new(raw_config),
            config_path,
            store,
            poll_notify,
            poller: Arc::new(Mutex::new(None)),
            poll_task_handle: Mutex::new(None),
            app_handle: Mutex::new(None),
        })
    }

    /// Spawn (or respawn) the background poll task using the current config.
    /// Aborts any existing poll task first.
    async fn respawn_poll_task(&self) {
        // Abort existing task
        {
            let mut handle_guard = self.poll_task_handle.lock().await;
            if let Some(old_handle) = handle_guard.take() {
                old_handle.abort();
            }
        }

        let poller = {
            let poller_guard = self.poller.lock().await;
            match poller_guard.clone() {
                Some(p) => p,
                None => return,
            }
        };

        let interval_secs = {
            let config_guard = self.config.lock().await;
            config_guard.github.poll_interval_secs
        };

        let app_handle = {
            let guard = self.app_handle.lock().await;
            match guard.clone() {
                Some(h) => h,
                None => return,
            }
        };

        let task_store = self.store.clone();
        let task_notify = self.poll_notify.clone();

        let new_handle = tauri::async_runtime::spawn(async move {
            poll_task(poller, task_store, task_notify, interval_secs, app_handle).await;
        });

        *self.poll_task_handle.lock().await = Some(new_handle);
    }
}

// ---------------------------------------------------------------------------
// Single command dispatcher
// ---------------------------------------------------------------------------

#[tauri::command]
async fn handle_command(
    state: State<'_, App>,
    cmd: Command,
) -> Result<Response, AppError> {
    match cmd {
        Command::GetEvents(filter) => {
            let events = state
                .store
                .get_events(&filter)
                .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))?;
            Ok(Response::Events(events))
        }
        Command::GetEvent { id } => {
            let event = state
                .store
                .get_event(&id)
                .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))?;
            Ok(Response::Event(event))
        }
        Command::GetConfig => {
            let config_guard = state.config.lock().await;
            Ok(Response::Config(config::to_app_config(&config_guard)))
        }
        Command::PollNow => {
            {
                let poller_guard = state.poller.lock().await;
                if poller_guard.is_none() {
                    return Err(AppError::new(
                        ErrorKind::Config,
                        "No GitHub token configured",
                    ));
                }
            }
            state.poll_notify.notify_one();
            Ok(Response::Ok)
        }
        Command::MarkRead { id } => {
            state
                .store
                .mark_read(&id)
                .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))?;
            Ok(Response::Ok)
        }
        Command::MarkAllRead { repo } => {
            state
                .store
                .mark_all_read(repo.as_deref())
                .map_err(|e| AppError::new(ErrorKind::Database, e.to_string()))?;
            Ok(Response::Ok)
        }
        Command::SaveConfig(app_config) => {
            // Convert wire type to internal config
            let new_raw = config::from_app_config(&app_config);

            // Write to disk
            config::save_config(&state.config_path, &new_raw)
                .map_err(|e| AppError::new(ErrorKind::Config, e.to_string()))?;

            // Check what changed and update in-memory config
            let (token_changed, subs_changed, interval_changed, new_token) = {
                let mut config_guard = state.config.lock().await;
                let old_token = config::resolve_token(&config_guard);
                let new_token = config::resolve_token(&new_raw);
                let token_changed = old_token != new_token;
                let subs_changed = config_guard.github.subscriptions.len()
                    != new_raw.github.subscriptions.len()
                    || config_guard
                        .github
                        .subscriptions
                        .iter()
                        .zip(new_raw.github.subscriptions.iter())
                        .any(|(a, b)| a.kind != b.kind || a.name != b.name);
                let interval_changed =
                    config_guard.github.poll_interval_secs != new_raw.github.poll_interval_secs;
                *config_guard = new_raw;
                (token_changed, subs_changed, interval_changed, new_token)
            };

            // Recreate poller if token or subscriptions changed
            if token_changed || subs_changed {
                let new_poller = new_token.map(|token| {
                    let config_guard =
                        state.config.try_lock().expect("config lock not contended");
                    let p =
                        GitHubPoller::new(&token, config_guard.github.subscriptions.clone())
                            .expect("could not create GitHub poller");
                    Arc::new(Mutex::new(p))
                });
                *state.poller.lock().await = new_poller;
            }

            // Respawn poll task if anything relevant changed
            if token_changed || subs_changed || interval_changed {
                state.respawn_poll_task().await;
            }

            // Emit config updated event
            {
                let app_handle_guard = state.app_handle.lock().await;
                if let Some(ref handle) = *app_handle_guard {
                    let event = ServerEvent::ConfigUpdated(app_config);
                    if let Err(e) = handle.emit("server-event", &event) {
                        log::error!("Failed to emit config-updated: {e}");
                    }
                }
            }

            log::info!("Configuration saved and applied");
            Ok(Response::Ok)
        }
    }
}

// ---------------------------------------------------------------------------
// Background poll task
// ---------------------------------------------------------------------------

/// Runs the polling loop. Wakes on the configured interval or when
/// `poll_notify` is signalled (by the `PollNow` command).
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
            log::info!(
                "Stored {new_count} new events ({} total upserted)",
                events.len()
            );

            // Emit typed ServerEvent to the frontend
            let server_event = ServerEvent::NewEvents { count: new_count };
            if let Err(e) = app_handle.emit("server-event", &server_event) {
                log::error!("Failed to emit server-event: {e}");
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

            let app_state =
                App::new(config_path, db_path).expect("could not initialize app state");

            let (config_summary, poll_interval, token, subs) = {
                let config_guard = app_state.config.blocking_lock();
                let summary = config::to_app_config(&config_guard);
                let interval = config_guard.github.poll_interval_secs;
                let token = config::resolve_token(&config_guard);
                let subs = config_guard.github.subscriptions.clone();
                (summary, interval, token, subs)
            };

            log::info!(
                "Loaded {} subscriptions, token configured: {}",
                config_summary.subscriptions.len(),
                config_summary.has_token,
            );

            // Clone what we need before moving app_state into Tauri
            let poller_slot = app_state.poller.clone();
            let task_store = app_state.store.clone();
            let task_notify = app_state.poll_notify.clone();
            let app_handle = app.handle().clone();

            app.manage(app_state);

            // Store the app handle in state (for SaveConfig to use later)
            let managed_state: State<App> = app.state();
            *managed_state.app_handle.blocking_lock() = Some(app_handle.clone());

            // Spawn the background poll task if we have a token.
            // The poller is created inside the async block because octocrab
            // (via tower::Buffer) requires a Tokio runtime context.
            if let Some(token) = token {
                log::info!("Starting background poll task (interval: {poll_interval}s)");
                let handle = tauri::async_runtime::spawn(async move {
                    match GitHubPoller::new(&token, subs) {
                        Ok(poller) => {
                            let poller = Arc::new(Mutex::new(poller));
                            *poller_slot.lock().await = Some(poller.clone());
                            poll_task(
                                poller,
                                task_store,
                                task_notify,
                                poll_interval,
                                app_handle,
                            )
                            .await;
                        }
                        Err(e) => {
                            log::error!("Failed to create GitHub poller: {e}");
                        }
                    }
                });
                *managed_state.poll_task_handle.blocking_lock() = Some(handle);
            } else {
                log::warn!(
                    "No GitHub token configured — polling disabled. \
                     Set token in config.toml or GITHUB_TOKEN env var."
                );
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![handle_command])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
