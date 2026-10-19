// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Client for Pocket Casts' private web-player API.
//!
//! There is no official public API. Endpoints follow the web player and the maintained
//! Music Assistant provider; any of them may change without notice.

use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::model::{Episode, PlayStatus, Podcast};

const API_BASE: &str = "https://api.pocketcasts.com";
const PODCAST_API_BASE: &str = "https://podcast-api.pocketcasts.com";

/// Returned when the session token is rejected and no stored password could renew it.
#[derive(Debug)]
pub struct AuthExpired;

impl std::fmt::Display for AuthExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pocket Casts session expired; run `pocketatui --login`")
    }
}

impl std::error::Error for AuthExpired {}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    api: String,
    podcast_api: String,
    token: RwLock<String>,
    /// Serialises renewals so parallel 401s trigger one login.
    renewing: tokio::sync::Mutex<()>,
    /// Email and password for silently renewing an expired token.
    credentials: Option<(String, String)>,
    /// Called with a fresh token so it can be saved.
    on_token: Box<dyn Fn(&str) + Send + Sync>,
}

impl Client {
    pub fn new(
        token: String,
        credentials: Option<(String, String)>,
        on_token: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                http: http_client()?,
                api: std::env::var("POCKETATUI_API_BASE").unwrap_or_else(|_| API_BASE.into()),
                podcast_api: std::env::var("POCKETATUI_PODCAST_API_BASE")
                    .unwrap_or_else(|_| PODCAST_API_BASE.into()),
                token: RwLock::new(token),
                renewing: tokio::sync::Mutex::new(()),
                credentials,
                on_token: Box::new(on_token),
            }),
        })
    }

    /// Log in and return the session token.
    pub async fn login(email: &str, password: &str) -> Result<String> {
        let base = std::env::var("POCKETATUI_API_BASE").unwrap_or_else(|_| API_BASE.into());
        login_with(&http_client()?, &base, email, password).await
    }

    fn token(&self) -> String {
        self.inner
            .token
            .read()
            .map(|t| t.clone())
            .unwrap_or_default()
    }

    /// Get a fresh token, unless another request already replaced `stale`.
    async fn renew(&self, stale: &str) -> Result<()> {
        let _guard = self.inner.renewing.lock().await;
        if self.token() != stale {
            return Ok(());
        }
        let Some((email, password)) = &self.inner.credentials else {
            return Err(AuthExpired.into());
        };
        let token = login_with(&self.inner.http, &self.inner.api, email, password)
            .await
            .map_err(|_| anyhow!(AuthExpired))?;
        (self.inner.on_token)(&token);
        if let Ok(mut t) = self.inner.token.write() {
            *t = token;
        }
        Ok(())
    }

    /// POST to the authenticated API, renewing the token once on 401/403.
    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.inner.api, path);
        for attempt in 0..2 {
            let token = self.token();
            let mut req = self
                .inner
                .http
                .post(&url)
                .bearer_auth(&token)
                .header("Content-Type", "application/json");
            if let Some(b) = &body {
                req = req.json(b);
            }
            let resp = req.send().await.with_context(|| format!("POST {path}"))?;
            let status = resp.status();
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) && attempt == 0 {
                self.renew(&token).await?;
                continue;
            }
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                bail!("{path} failed ({status}): {}", truncate(&text, 200));
            }
            let text = resp.text().await?;
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).with_context(|| format!("{path}: bad JSON"));
        }
        Err(AuthExpired.into())
    }

    pub async fn subscriptions(&self) -> Result<Vec<Podcast>> {
        let v = self
            .post("/user/podcast/list", Some(json!({"v": 1})))
            .await?;
        let mut list: Vec<Podcast> = array(&v, "podcasts")
            .iter()
            .filter_map(Podcast::from_value)
            .collect();
        list.sort_by_key(|p| p.title.to_lowercase());
        Ok(list)
    }

    async fn episode_list(&self, path: &str) -> Result<Vec<Episode>> {
        let v = self.post(path, None).await?;
        Ok(array(&v, "episodes")
            .iter()
            .filter_map(Episode::from_value)
            .collect())
    }

    pub async fn new_releases(&self) -> Result<Vec<Episode>> {
        self.episode_list("/user/new_releases").await
    }

    pub async fn in_progress(&self) -> Result<Vec<Episode>> {
        self.episode_list("/user/in_progress").await
    }

    /// The Up Next queue, in order. Entries carry title/url/podcast only.
    pub async fn up_next(&self) -> Result<Vec<Episode>> {
        let v = self.post("/up_next/list", None).await?;
        Ok(match v.get("episodes") {
            // Some versions key the queue by episode uuid.
            Some(Value::Object(map)) => map
                .iter()
                .filter_map(|(uuid, e)| {
                    let mut e = e.clone();
                    if let Value::Object(o) = &mut e {
                        o.entry("uuid").or_insert_with(|| uuid.clone().into());
                    }
                    Episode::from_value(&e)
                })
                .collect(),
            Some(Value::Array(a)) => a.iter().filter_map(Episode::from_value).collect(),
            _ => Vec::new(),
        })
    }

    /// Every episode of a podcast, newest first, with your play status merged in.
    pub async fn podcast_episodes(&self, podcast: &Podcast) -> Result<Vec<Episode>> {
        let url = format!("{}/podcast/full/{}", self.inner.podcast_api, podcast.uuid);
        let full_req = self.inner.http.get(&url).send();
        let status_req = self.post(
            "/user/podcast/episodes",
            Some(json!({"uuid": podcast.uuid})),
        );
        let (full, statuses) = tokio::join!(full_req, status_req);

        let full = full.context("podcast episode list")?;
        if !full.status().is_success() {
            bail!(
                "episode list for {} failed ({})",
                podcast.title,
                full.status()
            );
        }
        let full: Value = full.json().await.context("episode list: bad JSON")?;
        let pod = full.get("podcast").unwrap_or(&Value::Null);
        let title = pod
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&podcast.title)
            .to_string();

        let statuses = statuses.unwrap_or(Value::Null);
        let by_uuid: std::collections::HashMap<&str, &Value> = array(&statuses, "episodes")
            .iter()
            .filter_map(|s| Some((s.get("uuid")?.as_str()?, s)))
            .collect();

        let mut eps: Vec<Episode> = array(pod, "episodes")
            .iter()
            .filter_map(Episode::from_value)
            .map(|mut e| {
                e.podcast_uuid = podcast.uuid.clone();
                e.podcast_title = title.clone();
                if let Some(st) = by_uuid.get(e.uuid.as_str()) {
                    e.merge_status(st);
                }
                e
            })
            .collect();
        eps.sort_by(|a, b| b.published.cmp(&a.published));
        Ok(eps)
    }

    /// Search the Pocket Casts directory; returns matching podcasts, best first.
    /// Uses the apps' combined search on the podcast-api host (no login needed).
    pub async fn search(&self, term: &str) -> Result<Vec<Podcast>> {
        let url = format!("{}/v2/search/combined", self.inner.podcast_api);
        let resp = self
            .inner
            .http
            .post(&url)
            .json(&json!({"term": term}))
            .send()
            .await
            .context("search")?;
        if !resp.status().is_success() {
            bail!("search failed ({})", resp.status());
        }
        let v: Value = resp.json().await.context("search: bad JSON")?;
        Ok(array(&v, "results")
            .iter()
            .filter(|r| r.get("type").and_then(Value::as_str) == Some("podcast"))
            .filter_map(Podcast::from_value)
            .collect())
    }

    pub async fn subscribe(&self, uuid: &str) -> Result<()> {
        self.post("/user/podcast/subscribe", Some(json!({"uuid": uuid})))
            .await
            .map(drop)
    }

    pub async fn unsubscribe(&self, uuid: &str) -> Result<()> {
        self.post("/user/podcast/unsubscribe", Some(json!({"uuid": uuid})))
            .await
            .map(drop)
    }

    /// Show-notes records (with transcript links) for every episode of a podcast, by uuid.
    pub async fn show_notes(
        &self,
        podcast_uuid: &str,
    ) -> Result<std::collections::HashMap<String, Value>> {
        let url = format!(
            "{}/mobile/show_notes/full/{podcast_uuid}",
            self.inner.podcast_api
        );
        let resp = self
            .inner
            .http
            .get(&url)
            .send()
            .await
            .context("show notes")?;
        if !resp.status().is_success() {
            bail!("show notes failed ({})", resp.status());
        }
        let v: Value = resp.json().await.context("show notes: bad JSON")?;
        let pod = v.get("podcast").unwrap_or(&Value::Null);
        Ok(array(pod, "episodes")
            .iter()
            .filter_map(|e| Some((e.get("uuid")?.as_str()?.to_string(), e.clone())))
            .collect())
    }

    /// Download a text file (a transcript), following redirects.
    pub async fn fetch_text(&self, url: &str) -> Result<String> {
        let resp = self
            .inner
            .http
            .get(url)
            .send()
            .await
            .context("transcript")?;
        if !resp.status().is_success() {
            bail!("transcript download failed ({})", resp.status());
        }
        Ok(resp.text().await?)
    }

    /// Full details for one episode (resume position, status, duration).
    pub async fn episode(&self, uuid: &str) -> Result<Value> {
        self.post("/user/episode", Some(json!({"uuid": uuid})))
            .await
    }

    pub async fn save_progress(&self, ep: &Episode, position: f64, duration: f64) -> Result<()> {
        let mut body = json!({
            "uuid": ep.uuid,
            "podcast": ep.podcast_uuid,
            "status": PlayStatus::InProgress.code(),
            "position": format!("{}", position.max(0.0) as u64),
        });
        if duration > 0.0 {
            body["duration"] = format!("{}", duration as u64).into();
        }
        self.post("/sync/update_episode", Some(body))
            .await
            .map(drop)
    }

    pub async fn set_status(&self, ep: &Episode, status: PlayStatus) -> Result<()> {
        let mut body =
            json!({"uuid": ep.uuid, "podcast": ep.podcast_uuid, "status": status.code()});
        if status == PlayStatus::Unplayed {
            body["position"] = "0".into();
        }
        self.post("/sync/update_episode", Some(body))
            .await
            .map(drop)
    }

    /// Put an episode at the top of Up Next.
    pub async fn play_now(&self, ep: &Episode) -> Result<()> {
        self.up_next_add("/up_next/play_now", ep).await
    }

    /// Insert an episode right after the current one.
    pub async fn play_next(&self, ep: &Episode) -> Result<()> {
        self.up_next_add("/up_next/play_next", ep).await
    }

    /// Append an episode to the end of Up Next.
    pub async fn play_last(&self, ep: &Episode) -> Result<()> {
        self.up_next_add("/up_next/play_last", ep).await
    }

    async fn up_next_add(&self, path: &str, ep: &Episode) -> Result<()> {
        let body = json!({"version": 2, "episode": ep.up_next_json()});
        self.post(path, Some(body)).await.map(drop)
    }

    pub async fn remove_up_next(&self, uuid: &str) -> Result<()> {
        let body = json!({"version": 2, "uuids": [uuid]});
        self.post("/up_next/remove", Some(body)).await.map(drop)
    }
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("pocketatui/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

async fn login_with(
    http: &reqwest::Client,
    base: &str,
    email: &str,
    password: &str,
) -> Result<String> {
    let resp = http
        .post(format!("{base}/user/login"))
        .form(&[
            ("email", email),
            ("password", password),
            ("scope", "webplayer"),
        ])
        .send()
        .await
        .context("could not reach Pocket Casts")?;
    match resp.status() {
        StatusCode::UNAUTHORIZED | StatusCode::BAD_REQUEST => bail!("wrong email or password"),
        s if !s.is_success() => bail!("login failed ({s})"),
        _ => {}
    }
    let v: Value = resp.json().await.context("login: bad JSON")?;
    v.get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("login response had no token"))
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

pub fn is_auth_expired(e: &anyhow::Error) -> bool {
    e.downcast_ref::<AuthExpired>().is_some()
}
