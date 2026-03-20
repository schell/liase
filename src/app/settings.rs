use futures_lite::FutureExt;
use iti::components::alert::Alert;
use iti::components::button::Button;
use iti::components::select::Select;
use iti::components::Flavor;
use liase_wire_types::{AppConfig, Command, Subscription, SubscriptionKind};
use mogwai::future::MogwaiFutureExt;
use mogwai::web::prelude::*;

use super::invoke;

// ---------------------------------------------------------------------------
// A single subscription row in the form
// ---------------------------------------------------------------------------

struct SubRow<V: View> {
    wrapper: V::Element,
    kind_select: Select<V>,
    name_input: V::Element,
    remove_btn: Button<V>,
}

impl<V: View> SubRow<V> {
    fn new(kind: SubscriptionKind, name: &str) -> Self {
        let mut kind_select = Select::new(None);
        kind_select.push("Organization", "org");
        kind_select.push("Repository", "repo");
        match kind {
            SubscriptionKind::Org => kind_select.set_selected_index(0),
            SubscriptionKind::Repo => kind_select.set_selected_index(1),
        }

        let mut remove_btn = Button::new("Remove", Some(Flavor::Danger));
        remove_btn.set_has_icon(false);

        rsx! {
            let wrapper = div(class = "d-flex gap-2 mb-2 align-items-center") {
                div(style:width = "160px") {
                    {&kind_select}
                }
                let name_input = input(
                    class = "form-control",
                    type = "text",
                    value = name,
                    placeholder = "org name or owner/repo",
                ) {}
                {&remove_btn}
            }
        }

        SubRow {
            wrapper,
            kind_select,
            name_input,
            remove_btn,
        }
    }

    fn read_subscription(&self) -> Option<Subscription> {
        let kind_str = self.kind_select.selected_value().unwrap_or_default();
        let name = self
            .name_input
            .dyn_el(|el: &web_sys::HtmlInputElement| el.value())
            .unwrap_or_default();
        if name.is_empty() {
            return None;
        }
        let kind = match kind_str.as_str() {
            "org" => SubscriptionKind::Org,
            _ => SubscriptionKind::Repo,
        };
        Some(Subscription { kind, name })
    }
}

// ---------------------------------------------------------------------------
// SettingsView
// ---------------------------------------------------------------------------

#[derive(ViewChild)]
pub struct SettingsView<V: View> {
    #[child]
    wrapper: V::Element,
    // Form inputs
    token_input: V::Element,
    poll_interval_input: V::Element,
    // Dynamic subscription rows
    subs_container: V::Element,
    sub_rows: Vec<SubRow<V>>,
    // Buttons
    add_sub_btn: Button<V>,
    save_btn: Button<V>,
    // Status
    status_alert: Alert<V>,
    // State
    loaded: bool,
}

impl<V: View> Default for SettingsView<V> {
    fn default() -> Self {
        let status_alert = Alert::new("Loading configuration...", Flavor::Info);

        let mut add_sub_btn = Button::new("Add Subscription", Some(Flavor::Secondary));
        add_sub_btn.set_has_icon(false);

        let mut save_btn = Button::new("Save", Some(Flavor::Primary));
        save_btn.set_has_icon(false);

        rsx! {
            let subs_container = div() {}
        }

        rsx! {
            let wrapper = div(class = "container-fluid p-3", style:overflow_y = "auto", style:height = "100%") {
                h4(class = "mb-3") { "Settings" }

                {&status_alert}

                div(class = "mb-3") {
                    label(class = "form-label") { "GitHub Token" }
                    let token_input = input(
                        class = "form-control",
                        type = "password",
                        placeholder = "ghp_... or github_pat_...",
                    ) {}
                    div(class = "form-text") {
                        "Personal access token. Classic PAT scopes: repo, read:org. "
                        "Falls back to GITHUB_TOKEN env var if empty."
                    }
                }

                div(class = "mb-3") {
                    label(class = "form-label") { "Poll Interval (seconds)" }
                    let poll_interval_input = input(
                        class = "form-control",
                        type = "number",
                        value = "60",
                        min = "10",
                        placeholder = "60",
                    ) {}
                }

                h5(class = "mb-2") { "Subscriptions" }
                {&subs_container}
                div(class = "d-flex gap-2 mb-3") {
                    {&add_sub_btn}
                }

                div(class = "d-flex gap-2") {
                    {&save_btn}
                }
            }
        }

        SettingsView {
            wrapper,
            token_input,
            poll_interval_input,
            subs_container,
            sub_rows: Vec::new(),
            add_sub_btn,
            save_btn,
            status_alert,
            loaded: false,
        }
    }
}

enum SettingsAction {
    Save,
    AddSub,
    RemoveSub(usize),
}

impl<V: View> SettingsView<V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the form from a loaded config.
    fn set_config_values(&mut self, config: &AppConfig) {
        // Token
        self.token_input
            .dyn_el(|el: &web_sys::HtmlInputElement| {
                el.set_value(config.token.as_deref().unwrap_or(""));
            });

        // Poll interval
        self.poll_interval_input
            .dyn_el(|el: &web_sys::HtmlInputElement| {
                el.set_value(&config.poll_interval_secs.to_string());
            });

        // Clear existing subscription rows
        for row in self.sub_rows.drain(..) {
            self.subs_container.remove_child(&row.wrapper);
        }

        // Add rows for each subscription
        for sub in &config.subscriptions {
            let row = SubRow::<V>::new(sub.kind, &sub.name);
            self.subs_container.append_child(&row.wrapper);
            self.sub_rows.push(row);
        }
    }

    /// Read the current form values into an AppConfig.
    fn read_config(&self) -> AppConfig {
        let token_str = self
            .token_input
            .dyn_el(|el: &web_sys::HtmlInputElement| el.value())
            .unwrap_or_default();
        let token = if token_str.is_empty() {
            None
        } else {
            Some(token_str)
        };

        let interval_str = self
            .poll_interval_input
            .dyn_el(|el: &web_sys::HtmlInputElement| el.value())
            .unwrap_or_else(|| "60".into());
        let poll_interval_secs: u64 = interval_str.parse().unwrap_or(60);

        let subscriptions: Vec<Subscription> = self
            .sub_rows
            .iter()
            .filter_map(|row| row.read_subscription())
            .collect();

        AppConfig {
            poll_interval_secs,
            subscriptions,
            has_token: token.is_some(),
            token,
        }
    }

    /// Load config from backend and populate the form.
    async fn load_config(&mut self) {
        let result = invoke::send(&Command::GetConfig).await;
        match result.and_then(|r| r.into_config()) {
            Ok(config) => {
                self.set_config_values(&config);
                self.status_alert.set_is_visible(false);
            }
            Err(e) => {
                self.status_alert
                    .set_text(format!("Error loading config: {e}"));
                self.status_alert.set_flavor(Flavor::Danger);
                self.status_alert.set_is_visible(true);
            }
        }
    }

    fn add_empty_sub_row(&mut self) {
        let row = SubRow::<V>::new(SubscriptionKind::Org, "");
        self.subs_container.append_child(&row.wrapper);
        self.sub_rows.push(row);
    }

    fn remove_sub_row(&mut self, index: usize) {
        if index < self.sub_rows.len() {
            let row = self.sub_rows.remove(index);
            self.subs_container.remove_child(&row.wrapper);
        }
    }

    pub async fn step(&mut self) {
        // On first step, load config from backend
        if !self.loaded {
            self.load_config().await;
            self.loaded = true;
        }

        // Race all possible actions
        let action = {
            let save = self.save_btn.step().map(|_| SettingsAction::Save);
            let add_sub = self.add_sub_btn.step().map(|_| SettingsAction::AddSub);

            // Race remove button clicks on any sub row
            let remove_sub = async {
                if self.sub_rows.is_empty() {
                    futures_lite::future::pending::<SettingsAction>().await
                } else {
                    let mut remove_futures: Vec<_> = self
                        .sub_rows
                        .iter()
                        .enumerate()
                        .map(|(i, row)| {
                            let fut = row.remove_btn.step();
                            Box::pin(async move {
                                fut.await;
                                SettingsAction::RemoveSub(i)
                            })
                        })
                        .collect();

                    futures_lite::future::poll_fn(|cx| {
                        for fut in remove_futures.iter_mut() {
                            if let std::task::Poll::Ready(action) = fut.as_mut().poll(cx) {
                                return std::task::Poll::Ready(action);
                            }
                        }
                        std::task::Poll::Pending
                    })
                    .await
                }
            };

            save.or(add_sub).or(remove_sub).await
        };

        match action {
            SettingsAction::Save => {
                let config = self.read_config();

                self.save_btn.start_spinner();
                self.save_btn.disable();

                let result = invoke::send(&Command::SaveConfig(config))
                    .await
                    .and_then(|r| r.into_ok());

                match result {
                    Ok(()) => {
                        self.status_alert.set_text("Settings saved.");
                        self.status_alert.set_flavor(Flavor::Success);
                        self.status_alert.set_is_visible(true);
                    }
                    Err(e) => {
                        self.status_alert
                            .set_text(format!("Failed to save: {e}"));
                        self.status_alert.set_flavor(Flavor::Danger);
                        self.status_alert.set_is_visible(true);
                    }
                }

                self.save_btn.stop_spinner();
                self.save_btn.enable();
            }
            SettingsAction::AddSub => {
                self.add_empty_sub_row();
            }
            SettingsAction::RemoveSub(index) => {
                self.remove_sub_row(index);
            }
        }
    }
}
