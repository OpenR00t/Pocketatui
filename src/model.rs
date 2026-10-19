// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Data types shared by the API client and the UI.
//!
//! Pocket Casts' private API uses several schemas for the same thing (camelCase on the
//! `api.` host, snake_case on the `podcast-api.` host, a slimmer shape in Up Next), so
//! episodes are parsed leniently from `serde_json::Value` instead of with strict derives.

use serde_json::Value;

/// Playing status as the API reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayStatus {
    #[default]
    Unplayed,
    InProgress,
    Played,
}

impl PlayStatus {
    pub fn from_code(code: i64) -> Self {
        match code {
            2 => Self::InProgress,
            3 => Self::Played,
            _ => Self::Unplayed,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Unplayed => 1,
            Self::InProgress => 2,
            Self::Played => 3,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Episode {
    pub uuid: String,
    pub podcast_uuid: String,
    pub title: String,
    pub podcast_title: String,
    pub url: String,
    /// ISO-8601 publish timestamp as sent by the server.
    pub published: String,
    /// Seconds; 0 when unknown.
    pub duration: f64,
    /// Resume position in seconds.
    pub played_up_to: f64,
    pub status: PlayStatus,
    pub starred: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Podcast {
    pub uuid: String,
    pub title: String,
    pub author: String,
    pub last_episode_published: String,
}

fn s(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        match v.get(*k) {
            Some(Value::String(x)) if !x.is_empty() => return x.clone(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

fn f(v: &Value, keys: &[&str]) -> f64 {
    for k in keys {
        match v.get(*k) {
            Some(Value::Number(n)) => return n.as_f64().unwrap_or(0.0),
            Some(Value::String(x)) => {
                if let Ok(n) = x.trim().parse::<f64>() {
                    return n;
                }
            }
            _ => {}
        }
    }
    0.0
}

fn i(v: &Value, keys: &[&str]) -> i64 {
    f(v, keys) as i64
}

fn b(v: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|k| match v.get(*k) {
        Some(Value::Bool(x)) => *x,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    })
}

impl Episode {
    /// Parse any of the API's episode shapes. Returns `None` without a uuid.
    pub fn from_value(v: &Value) -> Option<Self> {
        let uuid = s(v, &["uuid"]);
        if uuid.is_empty() {
            return None;
        }
        Some(Self {
            uuid,
            podcast_uuid: s(v, &["podcastUuid", "podcast_uuid", "podcast"]),
            title: s(v, &["title"]),
            podcast_title: s(v, &["podcastTitle", "podcast_title"]),
            url: s(v, &["url"]),
            published: s(v, &["published"]),
            duration: f(v, &["duration"]),
            played_up_to: f(v, &["playedUpTo", "played_up_to"]),
            status: PlayStatus::from_code(i(v, &["playingStatus", "playing_status"])),
            starred: b(v, &["starred"]),
        })
    }

    /// Fill gaps from a second record for the same episode (e.g. user status data).
    pub fn merge_status(&mut self, other: &Value) {
        if let Some(code) = other.get("playingStatus").and_then(Value::as_i64) {
            self.status = PlayStatus::from_code(code);
        }
        let up_to = f(other, &["playedUpTo"]);
        if up_to > 0.0 || other.get("playedUpTo").is_some() {
            self.played_up_to = up_to;
        }
        let dur = f(other, &["duration"]);
        if dur > 0.0 {
            self.duration = dur;
        }
        if other.get("starred").is_some() {
            self.starred = b(other, &["starred"]);
        }
    }

    /// The date part of `published`, e.g. `2026-10-18`.
    pub fn date(&self) -> &str {
        self.published.get(..10).unwrap_or(&self.published)
    }

    /// The JSON shape Up Next endpoints take for an episode.
    pub fn up_next_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("uuid".into(), self.uuid.clone().into());
        m.insert("podcast".into(), self.podcast_uuid.clone().into());
        m.insert("title".into(), self.title.clone().into());
        m.insert("url".into(), self.url.clone().into());
        if !self.published.is_empty() {
            m.insert("published".into(), self.published.clone().into());
        }
        Value::Object(m)
    }
}

impl Podcast {
    pub fn from_value(v: &Value) -> Option<Self> {
        let uuid = s(v, &["uuid"]);
        if uuid.is_empty() {
            return None;
        }
        Some(Self {
            uuid,
            title: s(v, &["title"]),
            author: s(v, &["author"]),
            last_episode_published: s(v, &["lastEpisodePublished"]),
        })
    }
}

/// `h:mm:ss` or `m:ss`.
pub fn fmt_time(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Short length label like `1h 05m` or `42m`.
pub fn fmt_len(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (h, m) = (t / 3600, (t % 3600) / 60);
    if t == 0 {
        String::from("--")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{}m", m.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_camel_case_episode() {
        let v = json!({"uuid":"e1","podcastUuid":"p1","title":"Ep","podcastTitle":"Show",
            "url":"http://x/a.mp3","published":"2026-10-18T10:00:00Z","duration":3600,
            "playedUpTo":900,"playingStatus":2,"starred":true});
        let e = Episode::from_value(&v).unwrap();
        assert_eq!(e.podcast_uuid, "p1");
        assert_eq!(e.status, PlayStatus::InProgress);
        assert_eq!(e.date(), "2026-10-18");
        assert_eq!(e.played_up_to, 900.0);
        assert!(e.starred);
    }

    #[test]
    fn parses_up_next_and_snake_case() {
        let up = json!({"uuid":"e2","podcast":"p2","title":"T","url":"u","published":"2026-01-01"});
        assert_eq!(Episode::from_value(&up).unwrap().podcast_uuid, "p2");
        let full =
            json!({"uuid":"e3","title":"T","url":"u","duration":"125","file_type":"audio/mp3"});
        assert_eq!(Episode::from_value(&full).unwrap().duration, 125.0);
        assert!(Episode::from_value(&json!({"title":"no id"})).is_none());
    }

    #[test]
    fn merges_status() {
        let mut e = Episode::from_value(&json!({"uuid":"e","duration":100})).unwrap();
        e.merge_status(&json!({"uuid":"e","playingStatus":3,"playedUpTo":100}));
        assert_eq!(e.status, PlayStatus::Played);
        assert_eq!(e.played_up_to, 100.0);
    }

    #[test]
    fn formats_times() {
        assert_eq!(fmt_time(65.0), "1:05");
        assert_eq!(fmt_time(3725.0), "1:02:05");
        assert_eq!(fmt_len(3900.0), "1h 05m");
        assert_eq!(fmt_len(30.0), "1m");
        assert_eq!(fmt_len(0.0), "--");
    }
}
