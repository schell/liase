//! GitHub polling logic using octocrab.
//!
//! Discovers repos for org subscriptions, then polls each repo for
//! issues, PRs, and comments since the last poll.

use chrono::{DateTime, Utc};
use liase_wire_types::{EventKind, GhEvent};
use octocrab::Octocrab;
use std::collections::HashMap;

use crate::config::RawSubscription;

/// Tracks per-repo polling state.
#[derive(Debug, Clone)]
struct RepoState {
    /// Last time we successfully polled this repo's issues/PRs.
    last_issue_poll: Option<DateTime<Utc>>,
    /// Last time we successfully polled this repo's comments.
    last_comment_poll: Option<DateTime<Utc>>,
}

pub struct GitHubPoller {
    client: Octocrab,
    /// Subscriptions from config (org or repo).
    subscriptions: Vec<RawSubscription>,
    /// Discovered repos for org subscriptions: org_name -> vec of "owner/repo".
    org_repos: HashMap<String, Vec<String>>,
    /// Per-repo polling state.
    repo_states: HashMap<String, RepoState>,
    /// When we last discovered org repos.
    last_org_discovery: Option<DateTime<Utc>>,
}

impl GitHubPoller {
    pub fn new(token: &str, subscriptions: Vec<RawSubscription>) -> Result<Self, String> {
        let client = Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .map_err(|e| format!("Failed to create GitHub client: {e}"))?;

        Ok(GitHubPoller {
            client,
            subscriptions,
            org_repos: HashMap::new(),
            repo_states: HashMap::new(),
            last_org_discovery: None,
        })
    }

    /// Discover repos for all org subscriptions. Re-discovers every 10 minutes.
    async fn discover_org_repos(&mut self) {
        let now = Utc::now();
        let should_rediscover = match self.last_org_discovery {
            Some(last) => (now - last).num_minutes() >= 10,
            None => true,
        };

        if !should_rediscover {
            return;
        }

        for sub in &self.subscriptions {
            if sub.kind != "org" {
                continue;
            }

            log::info!("Discovering repos for org '{}'...", sub.name);
            match self.discover_repos_for_org(&sub.name).await {
                Ok(repos) => {
                    log::info!("Discovered {} repos in org '{}'", repos.len(), sub.name,);
                    self.org_repos.insert(sub.name.clone(), repos);
                }
                Err(e) => {
                    log::error!("Failed to discover repos for org '{}': {e}", sub.name);
                }
            }
        }

        self.last_org_discovery = Some(now);
    }

    /// List all repos in an org, paginating through all pages.
    async fn discover_repos_for_org(&self, org: &str) -> Result<Vec<String>, String> {
        let mut repos = Vec::new();
        let mut page = 1u32;

        loop {
            let result = self
                .client
                .orgs(org)
                .list_repos()
                .per_page(100)
                .page(page)
                .send()
                .await
                .map_err(|e| format!("GitHub API error listing repos for {org}: {e}"))?;

            if result.items.is_empty() {
                break;
            }

            for repo in &result.items {
                if let Some(ref full_name) = repo.full_name {
                    repos.push(full_name.clone());
                }
            }

            if result.next.is_none() {
                break;
            }

            page += 1;
        }

        Ok(repos)
    }

    /// Get all repos we should poll (org-discovered + individual subscriptions).
    fn all_repos(&self) -> Vec<String> {
        let mut repos = Vec::new();

        for sub in &self.subscriptions {
            match sub.kind.as_str() {
                "org" => {
                    if let Some(org_repos) = self.org_repos.get(&sub.name) {
                        repos.extend(org_repos.iter().cloned());
                    }
                }
                "repo" => {
                    repos.push(sub.name.clone());
                }
                _ => {}
            }
        }

        // Deduplicate
        repos.sort();
        repos.dedup();
        repos
    }

    /// Poll all subscribed repos for new issues, PRs, and comments.
    /// Returns all newly discovered events.
    pub async fn poll(&mut self) -> Vec<GhEvent> {
        self.discover_org_repos().await;

        let repos = self.all_repos();
        if repos.is_empty() {
            log::debug!("No repos to poll");
            return Vec::new();
        }

        log::info!("Polling {} repos...", repos.len());
        let mut all_events = Vec::new();

        for repo_full_name in &repos {
            let parts: Vec<&str> = repo_full_name.splitn(2, '/').collect();
            if parts.len() != 2 {
                log::warn!("Invalid repo name: {repo_full_name}");
                continue;
            }
            let (owner, repo) = (parts[0], parts[1]);

            let state = self
                .repo_states
                .entry(repo_full_name.clone())
                .or_insert(RepoState {
                    last_issue_poll: None,
                    last_comment_poll: None,
                });

            let issue_since = state.last_issue_poll;
            let comment_since = state.last_comment_poll;

            // Poll issues (includes PRs via the issues API)
            match self.poll_issues(owner, repo, issue_since).await {
                Ok(events) => {
                    if !events.is_empty() {
                        log::info!("  {repo_full_name}: {} new issues/PRs", events.len(),);
                    }
                    all_events.extend(events);
                    // Update last poll timestamp
                    if let Some(state) = self.repo_states.get_mut(repo_full_name) {
                        state.last_issue_poll = Some(Utc::now());
                    }
                }
                Err(e) => {
                    log::error!("  {repo_full_name}: failed to poll issues: {e}");
                }
            }

            // Poll comments
            match self.poll_comments(owner, repo, comment_since).await {
                Ok(events) => {
                    if !events.is_empty() {
                        log::info!("  {repo_full_name}: {} new comments", events.len(),);
                    }
                    all_events.extend(events);
                    if let Some(state) = self.repo_states.get_mut(repo_full_name) {
                        state.last_comment_poll = Some(Utc::now());
                    }
                }
                Err(e) => {
                    log::error!("  {repo_full_name}: failed to poll comments: {e}");
                }
            }
        }

        log::info!("Poll complete: {} total new events", all_events.len());
        all_events
    }

    /// Poll a repo for issues and PRs updated since `since`.
    async fn poll_issues(
        &self,
        owner: &str,
        repo: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<GhEvent>, String> {
        let mut events = Vec::new();
        let mut page = 1u32;
        let repo_full = format!("{owner}/{repo}");

        let issues_handler = self.client.issues(owner, repo);

        loop {
            let mut builder = issues_handler
                .list()
                .state(octocrab::params::State::All)
                .sort(octocrab::params::issues::Sort::Updated)
                .direction(octocrab::params::Direction::Descending)
                .per_page(100)
                .page(page);

            if let Some(since_dt) = since {
                builder = builder.since(since_dt);
            }

            let result = builder
                .send()
                .await
                .map_err(|e| format!("GitHub API error for {repo_full}: {e}"))?;

            if result.items.is_empty() {
                break;
            }

            for issue in &result.items {
                let is_pr = issue.pull_request.is_some();
                let kind = if is_pr {
                    EventKind::NewPullRequest
                } else {
                    EventKind::NewIssue
                };

                let id = format!("{repo_full}#{}", issue.number);

                events.push(GhEvent {
                    id,
                    kind: kind.into(),
                    repo: repo_full.clone(),
                    number: issue.number,
                    title: issue.title.clone(),
                    author: issue.user.login.clone(),
                    avatar_url: Some(issue.user.avatar_url.to_string()),
                    body: issue.body.clone().unwrap_or_default(),
                    url: issue.html_url.to_string(),
                    timestamp: issue.updated_at.to_rfc3339(),
                    read: false,
                });
            }

            // If we got fewer than a full page, we're done
            if result.next.is_none() {
                break;
            }

            page += 1;

            // Safety: don't paginate endlessly on first run
            if since.is_none() && page > 3 {
                log::debug!("  {repo_full}: limiting initial issue fetch to 3 pages");
                break;
            }
        }

        Ok(events)
    }

    /// Poll a repo for comments on issues/PRs updated since `since`.
    async fn poll_comments(
        &self,
        owner: &str,
        repo: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<GhEvent>, String> {
        let mut events = Vec::new();
        let mut page = 1u32;
        let repo_full = format!("{owner}/{repo}");

        let issues_handler = self.client.issues(owner, repo);

        loop {
            let mut builder = issues_handler
                .list_issue_comments()
                .sort(octocrab::params::issues::Sort::Updated)
                .direction(octocrab::params::Direction::Descending)
                .per_page(100)
                .page(page);

            if let Some(since_dt) = since {
                builder = builder.since(since_dt);
            }

            let result = builder
                .send()
                .await
                .map_err(|e| format!("GitHub API error for {repo_full} comments: {e}"))?;

            if result.items.is_empty() {
                break;
            }

            for comment in &result.items {
                // Extract the issue number from the issue_url
                // Format: https://api.github.com/repos/{owner}/{repo}/issues/{number}
                let issue_number = comment
                    .issue_url
                    .as_ref()
                    .and_then(|url| {
                        url.path_segments()
                            .and_then(|mut segs| segs.next_back())
                            .and_then(|s| s.parse::<u64>().ok())
                    })
                    .unwrap_or(0);

                let comment_id = comment.id.into_inner();
                let id = format!("{repo_full}#{issue_number}:comment:{comment_id}");

                events.push(GhEvent {
                    id,
                    kind: EventKind::NewComment.into(),
                    repo: repo_full.clone(),
                    number: issue_number,
                    title: format!("Comment on #{issue_number}"),
                    author: comment.user.login.clone(),
                    avatar_url: Some(comment.user.avatar_url.to_string()),
                    body: comment.body.clone().unwrap_or_default(),
                    url: comment.html_url.to_string(),
                    timestamp: comment
                        .updated_at
                        .unwrap_or(comment.created_at)
                        .to_rfc3339(),
                    read: false,
                });
            }

            if result.next.is_none() {
                break;
            }

            page += 1;

            // Safety: don't paginate endlessly on first run
            if since.is_none() && page > 3 {
                log::debug!("  {repo_full}: limiting initial comment fetch to 3 pages");
                break;
            }
        }

        Ok(events)
    }
}
