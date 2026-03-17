use std::borrow::Cow;

use futures_lite::FutureExt;
use iti::components::pane::Panes;
use iti::components::tab::{TabList, TabListEvent};
use mogwai::view::AppendArg;
use mogwai::web::prelude::*;
use wasm_bindgen::prelude::*;

mod settings;
mod timeline;

use settings::SettingsView;
use timeline::TimelineView;

// ---------------------------------------------------------------------------
// Tauri IPC invoke helpers
// ---------------------------------------------------------------------------

pub mod invoke {
    use super::*;
    use liase_wire_types::{AppConfig, AppError, ErrorKind, EventFilter, GhEvent};

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
        async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
    }

    fn deserialize_as<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, AppError> {
        match serde_wasm_bindgen::from_value::<T>(value) {
            Ok(t) => Ok(t),
            Err(e) => {
                log::error!("deserialization error: {e:#?}");
                Err(AppError::new(
                    ErrorKind::Serialization,
                    "Could not deserialize",
                ))
            }
        }
    }

    pub async fn cmd<T: serde::Serialize, X: serde::de::DeserializeOwned>(
        name: &str,
        args: &T,
    ) -> Result<X, AppError> {
        let value = serde_wasm_bindgen::to_value(args).map_err(|e| {
            AppError::new(
                ErrorKind::Serialization,
                format!("could not serialize {}: {e}", std::any::type_name::<T>()),
            )
        })?;
        let result = invoke(name, value).await;
        match result {
            Ok(value) => deserialize_as::<X>(value),
            Err(e) => Err(deserialize_as::<AppError>(e)?),
        }
    }

    // Typed wrappers for each Tauri command

    pub async fn get_events(filter: &EventFilter) -> Result<Vec<GhEvent>, AppError> {
        cmd("get_events", &serde_json::json!({ "filter": filter })).await
    }

    pub async fn get_config() -> Result<AppConfig, AppError> {
        cmd("get_config", &serde_json::json!({})).await
    }

    pub async fn poll_now() -> Result<(), AppError> {
        cmd("poll_now", &serde_json::json!({})).await
    }

    pub async fn mark_read(id: &str) -> Result<(), AppError> {
        cmd("mark_read", &serde_json::json!({ "id": id })).await
    }

    pub async fn mark_all_read(repo: Option<&str>) -> Result<(), AppError> {
        cmd("mark_all_read", &serde_json::json!({ "repo": repo })).await
    }
}

// ---------------------------------------------------------------------------
// tauri-plugin-opener: open URLs in the system browser
// ---------------------------------------------------------------------------

pub mod open {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "opener"])]
        async fn openUrl(url: &str);
    }

    pub async fn url(url: &str) {
        log::info!("Opening URL: {url}");
        openUrl(url).await;
    }
}

// ---------------------------------------------------------------------------
// Tab content enum (heterogeneous pane types)
// ---------------------------------------------------------------------------

const TAB_TIMELINE: usize = 0;
const TAB_SETTINGS: usize = 1;

enum TabContent<V: View> {
    Timeline(Box<TimelineView<V>>),
    Settings(Box<SettingsView<V>>),
}

impl<V: View> ViewChild<V> for TabContent<V> {
    fn as_append_arg(&self) -> AppendArg<V, impl Iterator<Item = Cow<'_, V::Node>>> {
        match self {
            TabContent::Timeline(v) => v.as_boxed_append_arg(),
            TabContent::Settings(v) => v.as_boxed_append_arg(),
        }
    }
}

// ---------------------------------------------------------------------------
// App shell
// ---------------------------------------------------------------------------

#[derive(ViewChild)]
pub struct App<V: View> {
    #[child]
    wrapper: V::Element,
    tab_list: TabList<V, V::Element>,
    panes: Panes<V, TabContent<V>>,
    active_tab: usize,
}

impl<V: View> Default for App<V> {
    fn default() -> Self {
        let mut tab_list = TabList::<V, V::Element>::default();

        rsx! { let timeline_label = span() { "Timeline" } }
        rsx! { let settings_label = span() { "Settings" } }

        tab_list.push(timeline_label);
        tab_list.push(settings_label);
        tab_list.select(0);

        let timeline = TimelineView::<V>::new();
        let settings = SettingsView::<V>::new();

        rsx! {
            let pane_wrapper = div(class = "h-100") {}
        }

        // Throwaway default — real panes go through add_pane so that
        // select(0) == Timeline and select(1) == Settings.
        let placeholder = TabContent::Timeline(Box::new(TimelineView::<V>::new()));
        let mut panes = Panes::new(pane_wrapper, placeholder);
        panes.add_pane(TabContent::Timeline(Box::new(timeline)));
        panes.add_pane(TabContent::Settings(Box::new(settings)));
        panes.select(TAB_TIMELINE);

        rsx! {
            let wrapper = div(class = "d-flex flex-column vh-100") {
                nav(
                    class = "navbar navbar-expand navbar-dark liase-nav-bg",
                    data_tauri_drag_region = "",
                ) {
                    div(
                        class = "container-fluid",
                        data_tauri_drag_region = "",
                    ) {
                        span(class ="navbar-brand mb-0 h1") {
                            "Liase"
                        }
                        {&tab_list}
                    }
                }
                div(class = "flex-grow-1 overflow-hidden") {
                    {&panes}
                }
            }
        }

        App {
            wrapper,
            tab_list,
            panes,
            active_tab: TAB_TIMELINE,
        }
    }
}

enum AppStepResult {
    TabClicked(usize),
    ContentStep,
}

impl<V: View> App<V> {
    fn select_tab(&mut self, index: usize) {
        self.active_tab = index;
        self.tab_list.select(index);
        self.panes.select(index);
    }

    pub async fn step(&mut self) {
        let result = match self.active_tab {
            TAB_TIMELINE => {
                let tab_click = async {
                    let TabListEvent::ItemClicked { index, .. } = self.tab_list.step().await;
                    AppStepResult::TabClicked(index)
                };
                let content_step = async {
                    match self.panes.get_pane_at_mut(TAB_TIMELINE) {
                        Some(TabContent::Timeline(ref mut view)) => view.step().await,
                        _ => futures_lite::future::pending::<()>().await,
                    }
                    AppStepResult::ContentStep
                };
                tab_click.or(content_step).await
            }
            TAB_SETTINGS => {
                let tab_click = async {
                    let TabListEvent::ItemClicked { index, .. } = self.tab_list.step().await;
                    AppStepResult::TabClicked(index)
                };
                let content_step = async {
                    match self.panes.get_pane_at_mut(TAB_SETTINGS) {
                        Some(TabContent::Settings(ref mut view)) => view.step().await,
                        _ => futures_lite::future::pending::<()>().await,
                    }
                    AppStepResult::ContentStep
                };
                tab_click.or(content_step).await
            }
            _ => {
                let TabListEvent::ItemClicked { index, .. } = self.tab_list.step().await;
                AppStepResult::TabClicked(index)
            }
        };

        match result {
            AppStepResult::TabClicked(index) => {
                self.select_tab(index);
            }
            AppStepResult::ContentStep => {}
        }
    }
}
