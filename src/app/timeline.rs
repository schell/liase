use std::collections::HashMap;

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
// Grouping: cluster comments under their parent issue/PR
// ---------------------------------------------------------------------------

/// A group of events sharing the same (repo, number).
struct EventGroup {
    /// The parent issue/PR event (or first event if no parent found).
    parent: GhEvent,
    /// Comment events, sorted by timestamp ascending.
    comments: Vec<GhEvent>,
    /// Most recent timestamp across all events in the group.
    latest_timestamp: String,
}

/// Group a flat list of events by (repo, number).
///
/// Each group has a parent (the issue/PR event) and zero or more comments.
/// Groups are sorted by most-recent activity descending, so a new comment
/// on an old issue bubbles the group to the top.
fn group_events(events: Vec<GhEvent>) -> Vec<EventGroup> {
    let mut map: HashMap<(String, u64), EventGroup> = HashMap::new();

    for event in events {
        let key = (event.repo.clone(), event.number);
        let kind = event.kind().unwrap_or(EventKind::NewIssue);

        let group = map.entry(key).or_insert_with(|| EventGroup {
            parent: event.clone(),
            comments: Vec::new(),
            latest_timestamp: event.timestamp.clone(),
        });

        match kind {
            EventKind::NewComment => {
                group.comments.push(event.clone());
            }
            _ => {
                // Issue/PR/StateChange — this is the parent. Prefer a
                // non-comment event as the parent header.
                let current_parent_is_comment = group
                    .parent
                    .kind()
                    .map(|k| k == EventKind::NewComment)
                    .unwrap_or(false);
                if current_parent_is_comment {
                    // Swap: demote old parent (was a comment placeholder)
                    // into comments list, promote this event as parent.
                    let old = std::mem::replace(&mut group.parent, event.clone());
                    group.comments.push(old);
                } else {
                    // We already have a proper parent — keep it. This event
                    // might be a state-change, which we prefer over the
                    // original if it's newer. For simplicity, just keep the
                    // first non-comment parent we saw.
                }
            }
        }

        if event.timestamp > group.latest_timestamp {
            group.latest_timestamp = event.timestamp.clone();
        }
    }

    let mut groups: Vec<EventGroup> = map.into_values().collect();
    groups.sort_by(|a, b| b.latest_timestamp.cmp(&a.latest_timestamp));
    for group in &mut groups {
        group.comments.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    }
    groups
}

// ---------------------------------------------------------------------------
// Comment row (compact, nested under parent)
// ---------------------------------------------------------------------------

struct CommentRow<V: View> {
    wrapper: V::Element,
    on_click: V::EventListener,
    event_data: GhEvent,
}

impl<V: View> CommentRow<V> {
    fn new(event: GhEvent) -> Self {
        let unread_class = if event.read {
            "event-comment-item list-group-item list-group-item-action"
        } else {
            "event-comment-item list-group-item list-group-item-action unread"
        };

        let body_preview = if event.body.len() > 100 {
            format!("{}...", &event.body[..100])
        } else {
            event.body.clone()
        };

        rsx! {
            let wrapper = div(class = unread_class, on:click = on_click) {
                small() {
                    span(class = "fw-semibold") { "@" {&event.author} }
                    {if !body_preview.is_empty() {
                        format!(" — {body_preview}")
                    } else {
                        String::new()
                    }}
                }
            }
        }

        CommentRow {
            wrapper,
            on_click,
            event_data: event,
        }
    }
}

// ---------------------------------------------------------------------------
// Event group row (parent + collapsible comments)
// ---------------------------------------------------------------------------

struct EventGroupRow<V: View> {
    wrapper: V::Element,
    /// Click on the "Open" button.
    open_click: V::EventListener,
    /// Click on the "Mark Read" button.
    mark_read_click: V::EventListener,
    parent_data: GhEvent,
    /// Click on the comment-count toggle area.
    toggle_click: V::EventListener,
    /// Chevron indicator that shows expand/collapse state.
    toggle_indicator: Option<V::Element>,
    /// Container for nested comment rows (hidden by default).
    comments_container: V::Element,
    /// Individual comment rows.
    comment_rows: Vec<CommentRow<V>>,
    /// Whether the comments section is currently expanded.
    expanded: bool,
}

impl<V: View> EventGroupRow<V> {
    fn new(group: EventGroup) -> Self {
        let event = &group.parent;
        let unread_class = if event.read {
            "list-group-item event-item"
        } else {
            "list-group-item event-item unread"
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

        let comment_count = group.comments.len();
        let comment_label = match comment_count {
            0 => String::new(),
            1 => "1 comment".to_string(),
            n => format!("{n} comments"),
        };

        // Build the comments container with nested rows
        let mut comment_rows = Vec::new();
        rsx! {
            let comments_container = div(class = "event-group-comments") {}
        }
        for comment_event in group.comments {
            let crow = CommentRow::<V>::new(comment_event);
            comments_container.append_child(&crow.wrapper);
            comment_rows.push(crow);
        }

        // Build the toggle indicator (chevron + comment count) before
        // the main rsx! block so the event listener escapes properly.
        let has_comments = comment_count > 0;
        let toggle_text = if has_comments {
            format!("{comment_label} \u{25B6}")
        } else {
            String::new()
        };

        rsx! {
            let toggle_area = small(
                class = if has_comments { "comment-toggle text-muted" } else { "d-none" },
                on:click = toggle_click,
            ) {
                {&toggle_text}
            }
        }

        let toggle_indicator = if has_comments {
            Some(toggle_area.clone())
        } else {
            None
        };

        rsx! {
            let wrapper = div(class = "event-group") {
                div(class = unread_class) {
                    div(class = "d-flex justify-content-between align-items-start gap-2") {
                        div(class = "d-flex align-items-center gap-2 flex-grow-1") {
                            span(class = "event-kind-badge") { {&kind_badge} }
                            span(class = "fw-semibold") { {&event.title} }
                        }
                        span(class = "event-timestamp text-muted") {
                            {&event.repo}
                            " #"
                            {event.number.to_string()}
                        }
                        div(class = "event-item-buttons") {
                            button(class = "btn btn-sm btn-secondary", on:click = open_click) {
                                "Open"
                            }
                            button(class = if event.read { "btn btn-sm btn-secondary d-none" } else { "btn btn-sm btn-secondary" }, on:click = mark_read_click) {
                                "Mark Read"
                            }
                        }
                    }
                    div(class = "d-flex justify-content-between align-items-center mt-1") {
                        small(class = "text-muted") {
                            "@"
                            {&event.author}
                            {if !body_preview.is_empty() {
                                format!(" — {body_preview}")
                            } else {
                                String::new()
                            }}
                        }
                        {&toggle_area}
                    }
                }
                {&comments_container}
            }
        }

        EventGroupRow {
            wrapper,
            open_click,
            mark_read_click,
            parent_data: group.parent,
            toggle_click,
            toggle_indicator,
            comments_container,
            comment_rows,
            expanded: false,
        }
    }

    /// Toggle the expanded/collapsed state of the comments section.
    fn toggle(&mut self) {
        self.expanded = !self.expanded;
        let class = if self.expanded {
            "event-group-comments expanded"
        } else {
            "event-group-comments"
        };
        self.comments_container
            .dyn_el(|el: &web_sys::HtmlElement| el.set_class_name(class));

        // Update the chevron indicator
        if let Some(ref indicator) = self.toggle_indicator {
            let count = self.comment_rows.len();
            let label = match count {
                1 => "1 comment".to_string(),
                n => format!("{n} comments"),
            };
            let chevron = if self.expanded {
                "\u{25BC}"
            } else {
                "\u{25B6}"
            };
            let text = format!("{label} {chevron}");
            indicator.dyn_el(|el: &web_sys::HtmlElement| el.set_inner_text(&text));
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
    /// Search input element
    search_input: V::Element,
    /// Search input change listener
    search_input_listener: V::EventListener,
    /// Mark all read button
    mark_all_read_btn: Button<V>,
    /// Current grouped event rows
    group_rows: Vec<EventGroupRow<V>>,
    /// Original loaded events (before filtering)
    loaded_events: Vec<GhEvent>,
    /// Current filter mode
    filter_mode: FilterMode,
    /// Current search text
    search_text: String,
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
            let search_input = input(
                class = "timeline-event-search form-control flex-grow-1",
                type = "text",
                placeholder = "Search events...",
                on:input = search_input_listener,
            ) {}
        }

        rsx! {
            let wrapper = div(class = "d-flex flex-column h-100") {
                div(class = "p-2 d-flex gap-2 align-items-center border-bottom") {
                    {&filter_all_btn}
                    {&filter_unread_btn}
                    {&search_input}
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
            search_input,
            search_input_listener,
            mark_all_read_btn,
            group_rows: Vec::new(),
            loaded_events: Vec::new(),
            filter_mode: FilterMode::All,
            search_text: String::new(),
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
    /// Search text changed
    SearchTextChanged,
    /// Parent "Open" button clicked: (group_index)
    ParentOpenClicked(usize),
    /// Parent "Mark Read" button clicked: (group_index)
    ParentMarkReadClicked(usize),
    /// Comment toggle clicked: (group_index)
    ToggleComments(usize),
    /// Comment row clicked: (group_index, comment_index)
    CommentClicked(usize, usize),
    NewServerEvents,
}

impl<V: View> TimelineView<V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load events from the backend, group them, and rebuild the list.
    async fn load_events(&mut self) {
        let filter = EventFilter {
            repo: None,
            unread_only: self.filter_mode == FilterMode::Unread,
            limit: Some(200),
        };

        let result = invoke::send(&Command::GetEvents(filter)).await;
        match result.and_then(|r| r.into_events()) {
            Ok(events) => {
                // Store the original events for client-side filtering
                self.loaded_events = events.clone();
                // Reset search when loading new events
                self.search_text.clear();
                self.search_input
                    .dyn_el(|el: &web_sys::HtmlInputElement| el.set_value(""));

                // Rebuild with the new events and empty search
                self.rebuild_filtered_events();

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

    /// Rebuild the event list with client-side filtering applied.
    /// Filters by search text and unread status.
    fn rebuild_filtered_events(&mut self) {
        let search_lower = self.search_text.to_lowercase();

        // Remove all rows from DOM
        for row in self.group_rows.drain(..) {
            self.event_list.remove_child(&row.wrapper);
        }

        // Filter events based on current filters
        let filtered_events: Vec<GhEvent> = self
            .loaded_events
            .iter()
            .filter(|event| {
                // Check unread filter
                let passes_unread = match self.filter_mode {
                    FilterMode::All => true,
                    FilterMode::Unread => !event.read,
                };

                // Check search filter
                let passes_search = if search_lower.is_empty() {
                    true
                } else {
                    // Create searchable string: title author repo number
                    let searchable = format!(
                        "{} {} {} #{}",
                        event.title.to_lowercase(),
                        event.author.to_lowercase(),
                        event.repo.to_lowercase(),
                        event.number
                    );
                    searchable.contains(&search_lower)
                };

                passes_unread && passes_search
            })
            .cloned()
            .collect();

        // Update display
        if filtered_events.is_empty() {
            let msg = if !search_lower.is_empty() {
                "No events match your search.".to_string()
            } else {
                match self.filter_mode {
                    FilterMode::Unread => "No unread events.".to_string(),
                    FilterMode::All => {
                        "No events yet. Configure subscriptions in Settings.".to_string()
                    }
                }
            };
            self.status_alert.set_text(msg);
            self.status_alert.set_flavor(Flavor::Info);
            self.status_alert.set_is_visible(true);
        } else {
            self.status_alert.set_is_visible(false);

            // Group and display filtered events
            let groups = group_events(filtered_events);
            for group in groups {
                let row = EventGroupRow::<V>::new(group);
                self.event_list.append_child(&row.wrapper);
                self.group_rows.push(row);
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

            let search_change = self
                .search_input_listener
                .next()
                .map(|_| TimelineAction::SearchTextChanged);

            // Wait for backend push
            let server_event = async {
                if let Some(rx) = &self.server_events {
                    let _ = rx.recv().await;
                } else {
                    futures_lite::future::pending::<()>().await;
                }
                TimelineAction::NewServerEvents
            };

            // Race all click events on group rows
            let group_clicks = async {
                if self.group_rows.is_empty() {
                    futures_lite::future::pending::<TimelineAction>().await
                } else {
                    // Collect all clickable futures across all groups
                    let mut futs: Vec<
                        std::pin::Pin<Box<dyn std::future::Future<Output = TimelineAction> + '_>>,
                    > = Vec::new();

                    for (gi, group) in self.group_rows.iter().enumerate() {
                        // Parent "Open" button click
                        let open_fut = group.open_click.next();
                        futs.push(Box::pin(async move {
                            open_fut.await;
                            TimelineAction::ParentOpenClicked(gi)
                        }));

                        // Parent "Mark Read" button click
                        let mark_read_fut = group.mark_read_click.next();
                        futs.push(Box::pin(async move {
                            mark_read_fut.await;
                            TimelineAction::ParentMarkReadClicked(gi)
                        }));

                        // Toggle click (only if there are comments)
                        if !group.comment_rows.is_empty() {
                            let toggle_fut = group.toggle_click.next();
                            futs.push(Box::pin(async move {
                                toggle_fut.await;
                                TimelineAction::ToggleComments(gi)
                            }));
                        }

                        // Comment row clicks
                        for (ci, crow) in group.comment_rows.iter().enumerate() {
                            let comment_fut = crow.on_click.next();
                            futs.push(Box::pin(async move {
                                comment_fut.await;
                                TimelineAction::CommentClicked(gi, ci)
                            }));
                        }
                    }

                    futures_lite::future::poll_fn(|cx| {
                        for fut in futs.iter_mut() {
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
                .or(search_change)
                .or(server_event)
                .or(group_clicks)
                .await
        };

        match action {
            TimelineAction::Refresh => {
                self.refresh_btn.start_spinner();
                self.refresh_btn.disable();

                if let Err(e) = invoke::send(&Command::PollNow).await {
                    log::warn!("PollNow failed: {e}");
                }
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
            TimelineAction::SearchTextChanged => {
                let new_text = self
                    .search_input
                    .dyn_el(|el: &web_sys::HtmlInputElement| el.value())
                    .unwrap_or_default();
                if new_text != self.search_text {
                    self.search_text = new_text;
                    self.rebuild_filtered_events();
                }
            }
            TimelineAction::ParentOpenClicked(gi) => {
                if let Some(group) = self.group_rows.get(gi) {
                    let url = group.parent_data.url.clone();
                    open::url(&url).await;
                    self.load_events().await;
                }
            }
            TimelineAction::ParentMarkReadClicked(gi) => {
                if let Some(group) = self.group_rows.get(gi) {
                    let parent_id = group.parent_data.id.clone();

                    // Mark parent as read
                    if let Err(e) = invoke::send(&Command::MarkRead { id: parent_id })
                        .await
                        .and_then(|r| r.into_ok())
                    {
                        log::error!("MarkRead parent failed: {e}");
                    }

                    // Mark all comments as read
                    for comment in &group.comment_rows {
                        let comment_id = comment.event_data.id.clone();
                        if let Err(e) = invoke::send(&Command::MarkRead { id: comment_id })
                            .await
                            .and_then(|r| r.into_ok())
                        {
                            log::error!("MarkRead comment failed: {e}");
                        }
                    }

                    self.load_events().await;
                }
            }
            TimelineAction::ToggleComments(gi) => {
                if let Some(group) = self.group_rows.get_mut(gi) {
                    group.toggle();
                }
            }
            TimelineAction::CommentClicked(gi, ci) => {
                if let Some(group) = self.group_rows.get(gi) {
                    if let Some(crow) = group.comment_rows.get(ci) {
                        let url = crow.event_data.url.clone();
                        let id = crow.event_data.id.clone();

                        if !crow.event_data.read {
                            if let Err(e) = invoke::send(&Command::MarkRead { id })
                                .await
                                .and_then(|r| r.into_ok())
                            {
                                log::error!("MarkRead failed: {e}");
                            }
                        }

                        open::url(&url).await;
                        self.load_events().await;
                    }
                }
            }
            TimelineAction::NewServerEvents => {
                self.load_events().await;
            }
        }
    }
}
