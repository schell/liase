use futures_lite::FutureExt;
use iti::components::alert::Alert;
use iti::components::badge::Badge;
use iti::components::button::Button;
use iti::components::Flavor;
use liase_wire_types::{Command, EventFilter, EventKind, GhEvent, ServerEvent};
use mogwai::future::MogwaiFutureExt;
use mogwai::web::prelude::*;

use super::{events, invoke, open};

// ---------------------------------------------------------------------------
// Individual event row
// ---------------------------------------------------------------------------

struct EventRow<V: View> {
    wrapper: V::Element,
    on_click: V::EventListener,
    event_data: GhEvent,
}

impl<V: View> EventRow<V> {
    fn new(event: GhEvent) -> Self {
        let unread_class = if event.read {
            "list-group-item list-group-item-action event-item"
        } else {
            "list-group-item list-group-item-action event-item unread"
        };

        let event_kind = event.kind().unwrap_or(EventKind::NewIssue);
        let badge_flavor = match event_kind {
            EventKind::NewIssue => Flavor::Success,
            EventKind::NewPullRequest => Flavor::Primary,
            EventKind::NewComment => Flavor::Secondary,
            EventKind::IssueStateChange => Flavor::Warning,
            EventKind::PRStateChange => Flavor::Info,
        };
        let kind_badge = Badge::<V>::new(event_kind.label(), badge_flavor);

        let body_preview = if event.body.len() > 120 {
            format!("{}...", &event.body[..120])
        } else {
            event.body.clone()
        };

        rsx! {
            let wrapper = div(class = unread_class, on:click = on_click) {
                div(class = "d-flex justify-content-between align-items-start") {
                    div(class = "d-flex align-items-center gap-2") {
                        span(class = "event-kind-badge") { {&kind_badge} }
                        span(class = "fw-semibold") { {&event.title} }
                    }
                    span(class = "event-timestamp text-muted") {
                        {&event.repo}
                        " #"
                        {event.number.to_string()}
                    }
                }
                div(class = "d-flex justify-content-between mt-1") {
                    small(class = "text-muted") {
                        "@"
                        {&event.author}
                        {if !body_preview.is_empty() {
                            format!(" — {body_preview}")
                        } else {
                            String::new()
                        }}
                    }
                }
            }
        }

        EventRow {
            wrapper,
            on_click,
            event_data: event,
        }
    }
}

// ---------------------------------------------------------------------------
// Filter buttons
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum FilterMode {
    All,
    Unread,
}

// ---------------------------------------------------------------------------
// TimelineView
// ---------------------------------------------------------------------------

#[derive(ViewChild)]
pub struct TimelineView<V: View> {
    #[child]
    wrapper: V::Element,
    /// The scrollable list container
    event_list: V::Element,
    /// Status/empty-state alert
    status_alert: Alert<V>,
    /// Refresh button
    refresh_btn: Button<V>,
    /// Filter: All
    filter_all_btn: Button<V>,
    /// Filter: Unread
    filter_unread_btn: Button<V>,
    /// Mark all read button
    mark_all_read_btn: Button<V>,
    /// Current event rows
    event_rows: Vec<EventRow<V>>,
    /// Current filter mode
    filter_mode: FilterMode,
    /// Whether initial load has happened
    loaded: bool,
    /// Receiver for backend-pushed ServerEvents
    server_events: Option<async_channel::Receiver<ServerEvent>>,
}

impl<V: View> Default for TimelineView<V> {
    fn default() -> Self {
        let status_alert = Alert::new("Loading events...", Flavor::Info);

        let mut refresh_btn = Button::new("Refresh", Some(Flavor::Primary));
        refresh_btn.set_has_icon(false);

        let mut filter_all_btn = Button::new("All", Some(Flavor::Primary));
        filter_all_btn.set_has_icon(false);

        let mut filter_unread_btn = Button::new("Unread", Some(Flavor::Secondary));
        filter_unread_btn.set_has_icon(false);

        let mut mark_all_read_btn = Button::new("Mark All Read", Some(Flavor::Secondary));
        mark_all_read_btn.set_has_icon(false);

        rsx! {
            let event_list = div(class = "list-group list-group-flush") {}
        }

        rsx! {
            let wrapper = div(class = "d-flex flex-column h-100") {
                div(class = "p-2 d-flex gap-2 align-items-center border-bottom") {
                    {&filter_all_btn}
                    {&filter_unread_btn}
                    div(class = "ms-auto d-flex gap-2") {
                        {&mark_all_read_btn}
                        {&refresh_btn}
                    }
                }
                div(class = "flex-grow-1 overflow-auto") {
                    {&status_alert}
                    {&event_list}
                }
            }
        }

        TimelineView {
            wrapper,
            event_list,
            status_alert,
            refresh_btn,
            filter_all_btn,
            filter_unread_btn,
            mark_all_read_btn,
            event_rows: Vec::new(),
            filter_mode: FilterMode::All,
            loaded: false,
            server_events: None,
        }
    }
}

enum TimelineAction {
    Refresh,
    FilterAll,
    FilterUnread,
    MarkAllRead,
    EventClicked(usize),
    NewServerEvents,
}

impl<V: View> TimelineView<V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load events from the backend and rebuild the list.
    async fn load_events(&mut self) {
        let filter = EventFilter {
            repo: None,
            unread_only: self.filter_mode == FilterMode::Unread,
            limit: Some(200),
        };

        let result = invoke::send(&Command::GetEvents(filter)).await;
        match result.and_then(|r| r.into_events()) {
            Ok(events) => {
                // Remove old rows from DOM
                for row in self.event_rows.drain(..) {
                    self.event_list.remove_child(&row.wrapper);
                }

                if events.is_empty() {
                    let msg = match self.filter_mode {
                        FilterMode::Unread => "No unread events.",
                        FilterMode::All => "No events yet. Configure subscriptions in Settings.",
                    };
                    self.status_alert.set_text(msg);
                    self.status_alert.set_flavor(Flavor::Info);
                    self.status_alert.set_is_visible(true);
                } else {
                    self.status_alert.set_is_visible(false);
                    // Build new rows
                    for event in events {
                        let row = EventRow::<V>::new(event);
                        self.event_list.append_child(&row.wrapper);
                        self.event_rows.push(row);
                    }
                }

                self.loaded = true;
            }
            Err(e) => {
                self.status_alert
                    .set_text(format!("Error loading events: {e}"));
                self.status_alert.set_flavor(Flavor::Danger);
                self.status_alert.set_is_visible(true);
            }
        }
    }

    fn update_filter_buttons(&mut self) {
        match self.filter_mode {
            FilterMode::All => {
                self.filter_all_btn.set_flavor(Some(Flavor::Primary));
                self.filter_unread_btn.set_flavor(Some(Flavor::Secondary));
            }
            FilterMode::Unread => {
                self.filter_all_btn.set_flavor(Some(Flavor::Secondary));
                self.filter_unread_btn.set_flavor(Some(Flavor::Primary));
            }
        }
    }

    pub async fn step(&mut self) {
        // On first step, subscribe to server events and do initial load
        if !self.loaded {
            self.server_events = Some(events::subscribe().await);
            self.load_events().await;
        }

        // Race all possible user actions + server event push
        let action = {
            let refresh = self.refresh_btn.step().map(|_| TimelineAction::Refresh);
            let filter_all = self
                .filter_all_btn
                .step()
                .map(|_| TimelineAction::FilterAll);
            let filter_unread = self
                .filter_unread_btn
                .step()
                .map(|_| TimelineAction::FilterUnread);
            let mark_all = self
                .mark_all_read_btn
                .step()
                .map(|_| TimelineAction::MarkAllRead);

            // Wait for backend push instead of polling on a timer
            let server_event = async {
                if let Some(rx) = &self.server_events {
                    let _ = rx.recv().await;
                } else {
                    futures_lite::future::pending::<()>().await;
                }
                TimelineAction::NewServerEvents
            };

            // Race event row clicks against all the buttons + server events
            let event_clicks = async {
                if self.event_rows.is_empty() {
                    futures_lite::future::pending::<TimelineAction>().await
                } else {
                    // Race all row click listeners
                    let mut click_futures: Vec<_> = self
                        .event_rows
                        .iter()
                        .enumerate()
                        .map(|(i, row)| {
                            let fut = row.on_click.next();
                            Box::pin(async move {
                                fut.await;
                                TimelineAction::EventClicked(i)
                            })
                        })
                        .collect();

                    // Use select-style: poll all futures, return first
                    futures_lite::future::poll_fn(|cx| {
                        for fut in click_futures.iter_mut() {
                            if let std::task::Poll::Ready(action) = fut.as_mut().poll(cx) {
                                return std::task::Poll::Ready(action);
                            }
                        }
                        std::task::Poll::Pending
                    })
                    .await
                }
            };

            refresh
                .or(filter_all)
                .or(filter_unread)
                .or(mark_all)
                .or(server_event)
                .or(event_clicks)
                .await
        };

        match action {
            TimelineAction::Refresh => {
                self.refresh_btn.start_spinner();
                self.refresh_btn.disable();

                // Trigger a backend poll, then reload
                if let Err(e) = invoke::send(&Command::PollNow).await {
                    log::warn!("PollNow failed: {e}");
                }
                // Small delay so the backend has time to store results
                mogwai::time::wait_millis(500).await;

                self.load_events().await;
                self.refresh_btn.stop_spinner();
                self.refresh_btn.enable();
            }
            TimelineAction::FilterAll => {
                if self.filter_mode != FilterMode::All {
                    self.filter_mode = FilterMode::All;
                    self.update_filter_buttons();
                    self.load_events().await;
                }
            }
            TimelineAction::FilterUnread => {
                if self.filter_mode != FilterMode::Unread {
                    self.filter_mode = FilterMode::Unread;
                    self.update_filter_buttons();
                    self.load_events().await;
                }
            }
            TimelineAction::MarkAllRead => {
                if let Err(e) = invoke::send(&Command::MarkAllRead { repo: None })
                    .await
                    .and_then(|r| r.into_ok())
                {
                    log::error!("MarkAllRead failed: {e}");
                }
                self.load_events().await;
            }
            TimelineAction::EventClicked(index) => {
                if let Some(row) = self.event_rows.get(index) {
                    let url = row.event_data.url.clone();
                    let id = row.event_data.id.clone();

                    // Mark as read
                    if !row.event_data.read {
                        if let Err(e) = invoke::send(&Command::MarkRead { id })
                            .await
                            .and_then(|r| r.into_ok())
                        {
                            log::error!("MarkRead failed: {e}");
                        }
                    }

                    // Open in browser
                    open::url(&url).await;

                    // Reload to reflect read state
                    self.load_events().await;
                }
            }
            TimelineAction::NewServerEvents => {
                // Backend pushed new events — reload from DB
                self.load_events().await;
            }
        }
    }
}
