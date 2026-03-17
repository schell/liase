use futures_lite::FutureExt;
use iti::components::alert::Alert;
use iti::components::button::Button;
use iti::components::card::Card;
use iti::components::Flavor;
use liase_wire_types::{AppConfig, SubscriptionKind};
use mogwai::future::MogwaiFutureExt;
use mogwai::web::prelude::*;

use super::invoke;

// ---------------------------------------------------------------------------
// SettingsView
// ---------------------------------------------------------------------------

#[derive(ViewChild)]
pub struct SettingsView<V: View> {
    #[child]
    wrapper: V::Element,
    /// Status alert
    status_alert: Alert<V>,
    /// Connection status card
    connection_card: Card<V>,
    /// Subscriptions card
    subs_card: Card<V>,
    /// Refresh button
    refresh_btn: Button<V>,
    /// Whether initial load has happened
    loaded: bool,
}

impl<V: View> Default for SettingsView<V> {
    fn default() -> Self {
        let status_alert = Alert::new(
            "Loading configuration...",
            Flavor::Info,
        );

        let mut connection_card = Card::new();
        rsx! { let conn_header = span() { "GitHub Connection" } }
        connection_card.set_header(&conn_header);
        rsx! { let conn_body = p(class = "text-muted mb-0") { "Checking..." } }
        connection_card.set_body(&conn_body);
        connection_card.hide_footer();

        let mut subs_card = Card::new();
        rsx! { let subs_header = span() { "Subscriptions" } }
        subs_card.set_header(&subs_header);
        rsx! { let subs_body = p(class = "text-muted mb-0") { "Loading..." } }
        subs_card.set_body(&subs_body);
        subs_card.hide_footer();

        let mut refresh_btn = Button::new("Refresh", Some(Flavor::Secondary));
        refresh_btn.set_has_icon(false);

        rsx! {
            let wrapper = div(class = "container-fluid p-3") {
                div(class = "d-flex justify-content-between align-items-center mb-3") {
                    h4(class = "mb-0") { "Settings" }
                    {&refresh_btn}
                }
                {&status_alert}
                div(class = "mb-3") { {&connection_card} }
                div(class = "mb-3") { {&subs_card} }
                div(class = "card") {
                    div(class = "card-body") {
                        h6(class = "card-title") { "Config File Location" }
                        p(class = "card-text") {
                            code() { "{app_data_dir}/config.toml" }
                        }
                        p(class = "card-text text-muted small") {
                            "Edit the config file and restart the app to apply changes. "
                            "See config.example.toml in the project root for the format."
                        }
                    }
                }
            }
        }

        SettingsView {
            wrapper,
            status_alert,
            connection_card,
            subs_card,
            refresh_btn,
            loaded: false,
        }
    }
}

impl<V: View> SettingsView<V> {
    pub fn new() -> Self {
        Self::default()
    }

    async fn load_config(&mut self) {
        match invoke::get_config().await {
            Ok(config) => {
                self.update_connection_card(&config);
                self.update_subs_card(&config);
                self.status_alert.set_is_visible(false);
            }
            Err(e) => {
                self.status_alert.set_text(format!("Error loading config: {e}"));
                self.status_alert.set_flavor(Flavor::Danger);
                self.status_alert.set_is_visible(true);
            }
        }
    }

    fn update_connection_card(&mut self, config: &AppConfig) {
        let (status_text, flavor) = if config.has_token {
            ("Token configured. Polling is active.", Flavor::Success)
        } else {
            (
                "No token configured. Set token in config.toml or GITHUB_TOKEN env var.",
                Flavor::Warning,
            )
        };

        let status_alert = Alert::<V>::new(status_text, flavor);

        rsx! {
            let body = div() {
                {&status_alert}
                p(class = "mb-0 small text-muted") {
                    "Poll interval: "
                    strong() { {config.poll_interval_secs.to_string()} }
                    " seconds"
                }
            }
        }
        self.connection_card.set_body(&body);
    }

    fn update_subs_card(&mut self, config: &AppConfig) {
        if config.subscriptions.is_empty() {
            rsx! {
                let body = p(class = "text-muted mb-0") {
                    "No subscriptions configured. Add [[github.subscriptions]] entries to config.toml."
                }
            }
            self.subs_card.set_body(&body);
            return;
        }

        rsx! {
            let list = ul(class = "list-group list-group-flush") {}
        }

        for sub in &config.subscriptions {
            let kind_label = match sub.kind {
                SubscriptionKind::Org => "org",
                SubscriptionKind::Repo => "repo",
            };
            let badge_flavor = match sub.kind {
                SubscriptionKind::Org => Flavor::Info,
                SubscriptionKind::Repo => Flavor::Secondary,
            };

            let kind_badge = iti::components::badge::Badge::<V>::new(kind_label, badge_flavor);

            rsx! {
                let item = li(class = "list-group-item d-flex align-items-center gap-2") {
                    {&kind_badge}
                    span() { {&sub.name} }
                }
            }
            list.append_child(&item);
        }

        self.subs_card.set_body(&list);
    }

    pub async fn step(&mut self) {
        // On first step, load config
        if !self.loaded {
            self.load_config().await;
            self.loaded = true;
        }

        // Wait for refresh button click
        self.refresh_btn.step().map(|_| ()).or(async {
            // Also just idle — this pane doesn't need to do much
            futures_lite::future::pending::<()>().await;
        }).await;

        // Refresh was clicked
        self.refresh_btn.start_spinner();
        self.refresh_btn.disable();
        self.load_config().await;
        self.refresh_btn.stop_spinner();
        self.refresh_btn.enable();
    }
}
