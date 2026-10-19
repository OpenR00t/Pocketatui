// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! App state, key handling and the background calls to Pocket Casts.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    api::{self, Client},
    art::ArtStore,
    config::{self, Config, EpisodePrefs, TranscribeMode, TrimMode, WHISPER_MODELS},
    model::{Episode, PlayStatus, Podcast},
    player::{Player, PlayerEvent},
    transcript::{self, Source, Transcript},
    whisper::{self, Whisper},
};

/// Results coming back from background tasks.
pub enum Msg {
    UpNext(anyhow::Result<Vec<Episode>>),
    NewReleases(anyhow::Result<Vec<Episode>>),
    InProgress(anyhow::Result<Vec<Episode>>),
    Podcasts(anyhow::Result<Vec<Podcast>>),
    PodcastEpisodes(String, anyhow::Result<Vec<Episode>>),
    /// Episode details fetched right before playing (resume point etc).
    Start(Episode, Option<Value>),
    /// `/user/episode` records for Up Next entries that arrive without lengths.
    Details(Vec<Value>),
    /// Directory search results for a query.
    Search(String, anyhow::Result<Vec<Podcast>>),
    /// The episode's own transcript (publisher or Pocket Casts), or `None` if it has none;
    /// the second value is any Whisper transcript cached from before.
    Transcript(String, Option<Transcript>, Option<Transcript>),
    /// Progress and results from the local Whisper worker.
    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
    Whisper(whisper::Event),
    /// A podcast cover finished loading (`None` if it couldn't be).
    Art(String, Option<image::DynamicImage>),
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    UpNext = 0,
    NewReleases = 1,
    InProgress = 2,
    Podcasts = 3,
    PodcastEpisodes = 4,
    Search = 5,
}

pub const SIDEBAR: [Pane; 5] = [
    Pane::UpNext,
    Pane::NewReleases,
    Pane::InProgress,
    Pane::Podcasts,
    Pane::Search,
];

const PANES: usize = 6;

/// Something on screen the mouse can act on; the UI records these as it draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Sidebar(usize),
    /// Rows of a table (first row at the rect's top).
    Table(Pane),
    /// A tile in a podcast grid, by position in the visible list.
    Tile(Pane, usize),
    /// A line of the transcript view, by cue index (right-click: "I'm hearing this").
    TranscriptLine(usize),
    /// The transcript view as a whole (for the wheel).
    TranscriptArea,
    /// The "‹ back" title of an open podcast.
    Back,
    SearchBox,
    Progress,
    Volume,
    Speed,
    Control(Control),
    /// The help overlay (any click closes it).
    Help,
    /// Episode-list toggles on an open podcast's border.
    SortToggle,
    PlayedToggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    SkipBack,
    PlayPause,
    SkipForward,
    Next,
    Trim,
    Boost,
    VolumeDown,
    VolumeUp,
    Transcript,
}

/// Where the now-playing episode's transcript stands.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptState {
    /// Transcripts are off in the config.
    Off,
    Loading,
    /// Ready to show (fully, or as far as Whisper has got).
    Ready,
    /// None available, with the reason.
    Missing(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Main,
}

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub episode: Episode,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub buffering: bool,
    /// Waiting for mpv to start the file.
    pub loading: bool,
}

pub struct App {
    pub cfg: Config,
    api: Client,
    player: Player,
    tx: UnboundedSender<Msg>,

    pub pane: Pane,
    pub focus: Focus,
    pub sidebar: usize,
    pub tables: [TableState; PANES],

    pub up_next: Vec<Episode>,
    pub new_releases: Vec<Episode>,
    pub in_progress: Vec<Episode>,
    pub podcasts: Vec<Podcast>,
    pub open_podcast: Option<Podcast>,
    pub podcast_episodes: Vec<Episode>,
    pub loading: [bool; PANES],
    /// Which grid an open podcast was opened from (Podcasts or Search).
    pub episodes_from: Pane,

    pub search_query: String,
    /// Typing goes into the search box.
    pub search_editing: bool,
    pub search_results: Vec<Podcast>,
    /// Query the current results are for.
    pub searched: String,
    pending_unfollow: Option<(String, Instant)>,

    /// Episode time skipped by Trim Silence this session, in seconds.
    pub trim_saved: f64,
    last_time_pos: Option<(Instant, f64)>,
    last_seek: Instant,
    /// A new file was requested and mpv hasn't started it yet: events still coming from
    /// the old file (its position, or its end) must not touch the new episode.
    awaiting_start: bool,

    pub now: Option<NowPlaying>,
    pub speed: f64,
    last_sync: Instant,
    last_synced_pos: f64,

    /// Episode details already fetched, by uuid.
    details: HashMap<String, Value>,
    requested: HashSet<String>,
    /// Transcript of the now-playing episode.
    pub transcript: Option<Transcript>,
    pub transcript_for: String,
    pub transcript_state: TranscriptState,
    /// What the Whisper worker last said it's doing.
    pub whisper_status: String,
    /// Whisper is working on the now-playing episode.
    pub whisper_active: bool,
    pub show_transcript: bool,
    /// Keep the spoken line in view; off once the user scrolls.
    pub transcript_follow: bool,
    /// Highlighted line when not following, by cue index.
    pub transcript_sel: usize,
    whisper: Whisper,
    /// The episode's own transcript while Whisper's is shown instead (`w` switches back).
    own_transcript: Option<Transcript>,
    /// Auto-sync: a Whisper sample is on its way.
    align_pending: Option<Instant>,
    /// Auto-sync: playback position of the last sample.
    align_last: Option<f64>,
    /// Auto-sync / manual sync result, for the footer.
    pub align_status: String,
    /// Sort / hide-played choice per podcast uuid.
    episode_prefs: HashMap<String, EpisodePrefs>,

    pub filter: String,
    pub filtering: bool,
    pub show_help: bool,
    pub status: Option<(String, bool, Instant)>,
    pub quit: bool,

    /// Clickable regions from the last frame, topmost last.
    pub hits: Vec<(Rect, Hit)>,
    last_click: Option<(Hit, usize, Instant)>,
    dragging_progress: bool,
    /// mpv volume, percent.
    pub volume: f64,

    pub art: ArtStore,
    /// Columns in the podcast grid, as last drawn.
    pub grid_cols: usize,
    /// First visible row of each podcast grid, by pane.
    pub grid_scroll: [usize; PANES],
}

impl App {
    pub fn new(
        cfg: Config,
        api: Client,
        player: Player,
        tx: UnboundedSender<Msg>,
        picker: ratatui_image::picker::Picker,
    ) -> Self {
        let speed = cfg.default_speed.clamp(0.5, 3.0);
        player.set_speed(speed);
        player.set_effects(cfg.trim_silence, cfg.voice_boost);
        let art = ArtStore::new(cfg.cover_art, picker, tx.clone());
        let whisper = Whisper::new(tx.clone(), whisper_settings(&cfg));
        let mut app = Self {
            art,
            grid_cols: 1,
            grid_scroll: [0; PANES],
            hits: Vec::new(),
            last_click: None,
            dragging_progress: false,
            volume: f64::from(cfg.volume),
            episodes_from: Pane::Podcasts,
            search_query: String::new(),
            search_editing: false,
            search_results: Vec::new(),
            searched: String::new(),
            pending_unfollow: None,
            trim_saved: 0.0,
            last_time_pos: None,
            last_seek: Instant::now(),
            awaiting_start: false,
            cfg,
            api,
            player,
            tx,
            pane: Pane::UpNext,
            focus: Focus::Main,
            sidebar: 0,
            tables: Default::default(),
            up_next: Vec::new(),
            new_releases: Vec::new(),
            in_progress: Vec::new(),
            podcasts: Vec::new(),
            open_podcast: None,
            podcast_episodes: Vec::new(),
            loading: [false; PANES],
            now: None,
            speed,
            last_sync: Instant::now(),
            last_synced_pos: -1.0,
            details: HashMap::new(),
            requested: HashSet::new(),
            episode_prefs: config::load_episode_prefs(),
            transcript: None,
            transcript_for: String::new(),
            transcript_state: TranscriptState::Loading,
            whisper_status: String::new(),
            whisper_active: false,
            show_transcript: false,
            transcript_follow: true,
            transcript_sel: 0,
            own_transcript: None,
            align_pending: None,
            align_last: None,
            align_status: String::new(),
            whisper,
            filter: String::new(),
            filtering: false,
            show_help: false,
            status: None,
            quit: false,
        };
        for t in &mut app.tables {
            t.select(Some(0));
        }
        for p in SIDEBAR.into_iter().filter(|&p| p != Pane::Search) {
            app.refresh(p);
        }
        app
    }

    // ---------- background calls ----------

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = Msg> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(fut.await);
        });
    }

    /// Run an API call whose only result is success or an error message.
    fn fire<F>(&self, ok: Option<String>, then_refresh_up_next: bool, fut: F)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let tx = self.tx.clone();
        let api = self.api.clone();
        tokio::spawn(async move {
            match fut.await {
                Ok(()) => {
                    if let Some(m) = ok {
                        let _ = tx.send(Msg::Info(m));
                    }
                    if then_refresh_up_next {
                        let _ = tx.send(Msg::UpNext(api.up_next().await));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(err_text(&e)));
                }
            }
        });
    }

    pub fn refresh(&mut self, pane: Pane) {
        self.loading[pane as usize] = true;
        let api = self.api.clone();
        match pane {
            Pane::UpNext => self.spawn(async move { Msg::UpNext(api.up_next().await) }),
            Pane::NewReleases => {
                self.spawn(async move { Msg::NewReleases(api.new_releases().await) })
            }
            Pane::InProgress => self.spawn(async move { Msg::InProgress(api.in_progress().await) }),
            Pane::Podcasts => self.spawn(async move { Msg::Podcasts(api.subscriptions().await) }),
            Pane::Search => {
                let term = self.search_query.trim().to_string();
                if term.is_empty() {
                    self.loading[pane as usize] = false;
                    return;
                }
                self.spawn(async move {
                    let r = api.search(&term).await;
                    Msg::Search(term, r)
                });
            }
            Pane::PodcastEpisodes => {
                if let Some(p) = self.open_podcast.clone() {
                    self.spawn(async move {
                        let r = api.podcast_episodes(&p).await;
                        Msg::PodcastEpisodes(p.uuid, r)
                    });
                }
            }
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::UpNext(r) => self.set_list(Pane::UpNext, r),
            Msg::NewReleases(r) => self.set_list(Pane::NewReleases, r),
            Msg::InProgress(r) => self.set_list(Pane::InProgress, r),
            Msg::PodcastEpisodes(uuid, r) => {
                if self.open_podcast.as_ref().is_some_and(|p| p.uuid == uuid) {
                    self.set_list(Pane::PodcastEpisodes, r);
                }
            }
            Msg::Podcasts(r) => {
                self.loading[Pane::Podcasts as usize] = false;
                match r {
                    Ok(v) => {
                        self.podcasts = v;
                        self.enrich();
                        self.clamp(Pane::Podcasts);
                    }
                    Err(e) => self.error(err_text(&e)),
                }
            }
            Msg::Start(mut ep, details) => {
                if self.now.as_ref().map(|n| &n.episode.uuid) != Some(&ep.uuid) {
                    return; // user picked something else meanwhile
                }
                if let Some(d) = details {
                    ep.merge_status(&d);
                    if ep.url.is_empty()
                        && let Some(u) = d.get("url").and_then(Value::as_str)
                    {
                        ep.url = u.to_string();
                    }
                    if ep.podcast_uuid.is_empty()
                        && let Some(p) = d.get("podcastUuid").and_then(Value::as_str)
                    {
                        ep.podcast_uuid = p.to_string();
                    }
                }
                if ep.url.is_empty() {
                    self.error("This episode has no audio URL".into());
                    self.now = None;
                    return;
                }
                let near_end = ep.duration > 0.0 && ep.played_up_to > ep.duration - 15.0;
                let start = if ep.status == PlayStatus::Played || near_end {
                    0.0
                } else {
                    ep.played_up_to
                };
                self.player.load(&ep.url, start);
                self.player.set_speed(self.speed);
                self.begin_transcript(&ep);
                if let Some(n) = &mut self.now {
                    n.position = start;
                    n.duration = ep.duration;
                    n.episode = ep;
                }
                self.last_sync = Instant::now();
            }
            Msg::Details(list) => {
                for d in list {
                    if let Some(uuid) = d.get("uuid").and_then(Value::as_str) {
                        self.details.insert(uuid.to_string(), d);
                    }
                }
                self.enrich();
            }
            Msg::Art(uuid, img) => self.art.insert(uuid, img),
            Msg::Search(term, r) => {
                if term != self.search_query.trim() {
                    return; // an older query
                }
                self.loading[Pane::Search as usize] = false;
                match r {
                    Ok(v) => {
                        if v.is_empty() {
                            self.info(format!("No podcasts found for \"{term}\""));
                        }
                        self.search_results = v;
                        self.searched = term;
                        self.tables[Pane::Search as usize].select(Some(0));
                        self.grid_scroll[Pane::Search as usize] = 0;
                    }
                    Err(e) => self.error(err_text(&e)),
                }
            }
            Msg::Transcript(uuid, own, cached_whisper) => {
                if uuid != self.transcript_for {
                    return;
                }
                let forced = self.cfg.transcribe == TranscribeMode::Whisper;
                match own {
                    Some(t) if !forced => {
                        self.transcript = Some(t);
                        self.transcript_state = TranscriptState::Ready;
                    }
                    own if whisper::AVAILABLE => {
                        self.own_transcript = own; // `w` switches to it
                        let t = cached_whisper
                            .unwrap_or_else(|| Transcript::new(Source::Whisper, Vec::new()));
                        self.transcript = Some(t);
                        self.transcript_state = TranscriptState::Ready;
                        self.whisper_status.clear();
                        if self.show_transcript {
                            self.start_whisper();
                        }
                    }
                    _ => {
                        self.transcript = cached_whisper;
                        self.transcript_state = if self.transcript.is_some() {
                            TranscriptState::Ready
                        } else {
                            TranscriptState::Missing(
                                "No transcript for this episode. Build with the `whisper` feature to transcribe it locally.".into(),
                            )
                        };
                    }
                }
            }
            Msg::Whisper(ev) => self.whisper_event(ev),
            Msg::Info(m) => self.info(m),
            Msg::Error(m) => self.error(m),
        }
    }

    fn set_list(&mut self, pane: Pane, r: anyhow::Result<Vec<Episode>>) {
        self.loading[pane as usize] = false;
        match r {
            Ok(v) => {
                *self.list_mut(pane) = v;
                self.enrich(); // also keeps the live position of what's playing
                if pane == Pane::UpNext {
                    self.fetch_missing_details();
                }
                self.clamp(pane);
            }
            Err(e) => self.error(err_text(&e)),
        }
    }

    /// Up Next entries only carry a podcast uuid and no length or progress; fill those
    /// in from the other lists, the subscriptions and fetched details.
    fn enrich(&mut self) {
        let titles: HashMap<String, String> = self
            .podcasts
            .iter()
            .map(|p| (p.uuid.clone(), p.title.clone()))
            .collect();
        let mut known: HashMap<String, Episode> = HashMap::new();
        for e in self
            .new_releases
            .iter()
            .chain(&self.in_progress)
            .chain(&self.podcast_episodes)
        {
            if e.duration > 0.0 {
                known.entry(e.uuid.clone()).or_insert_with(|| e.clone());
            }
        }
        let playing = self
            .now
            .as_ref()
            .map(|n| (n.episode.uuid.clone(), n.position));
        for list in [
            &mut self.up_next,
            &mut self.new_releases,
            &mut self.in_progress,
            &mut self.podcast_episodes,
        ] {
            for e in list.iter_mut() {
                if e.podcast_title.is_empty()
                    && let Some(t) = titles.get(&e.podcast_uuid)
                {
                    e.podcast_title = t.clone();
                }
                if e.duration <= 0.0 {
                    if let Some(k) = known.get(&e.uuid) {
                        e.duration = k.duration;
                        e.status = k.status;
                        e.played_up_to = k.played_up_to;
                    } else if let Some(d) = self.details.get(&e.uuid) {
                        e.merge_status(d);
                    }
                }
                if let Some((uuid, pos)) = &playing
                    && *uuid == e.uuid
                {
                    e.played_up_to = *pos;
                    e.status = PlayStatus::InProgress;
                }
            }
        }
    }

    fn fetch_missing_details(&mut self) {
        let missing: Vec<String> = self
            .up_next
            .iter()
            .filter(|e| e.duration <= 0.0 && !self.requested.contains(&e.uuid))
            .map(|e| e.uuid.clone())
            .take(60)
            .collect();
        if missing.is_empty() {
            return;
        }
        self.requested.extend(missing.iter().cloned());
        let api = self.api.clone();
        self.spawn(async move {
            let mut out = Vec::new();
            // A few at a time to stay polite to an unofficial API.
            for chunk in missing.chunks(4) {
                let calls = chunk.iter().map(|u| api.episode(u));
                for d in futures::future::join_all(calls).await.into_iter().flatten() {
                    out.push(d);
                }
            }
            Msg::Details(out)
        });
    }

    pub fn handle_player(&mut self, ev: PlayerEvent) {
        if self.awaiting_start
            && matches!(
                ev,
                PlayerEvent::TimePos(_)
                    | PlayerEvent::Duration(_)
                    | PlayerEvent::Finished
                    | PlayerEvent::Failed(_)
            )
        {
            return;
        }
        match ev {
            PlayerEvent::Started => self.awaiting_start = false,
            PlayerEvent::TimePos(t) => {
                self.count_trimmed(t);
                if let Some(n) = &mut self.now {
                    n.position = t;
                    n.loading = false;
                }
            }
            PlayerEvent::Duration(d) => {
                if let Some(n) = &mut self.now {
                    n.duration = d;
                }
            }
            PlayerEvent::Paused(p) => {
                let was_playing = self.now.as_ref().is_some_and(|n| !n.paused && !n.loading);
                if let Some(n) = &mut self.now {
                    n.paused = p;
                }
                if p && was_playing {
                    self.sync_progress(true);
                }
            }
            PlayerEvent::Speed(s) => self.speed = s,
            PlayerEvent::Volume(v) => self.volume = v,
            PlayerEvent::Buffering(b) => {
                if let Some(n) = &mut self.now {
                    n.buffering = b;
                }
            }
            PlayerEvent::Finished => self.on_finished(),
            PlayerEvent::Failed(why) => {
                self.error(format!("Couldn't play episode: {why}"));
                if let Some(n) = &mut self.now {
                    n.loading = false;
                    n.paused = true;
                }
            }
            PlayerEvent::Exited => {
                self.error("mpv exited; restart pocketatui".into());
                self.now = None;
            }
        }
    }

    /// Trim Silence makes the position jump over cut silences; add those jumps up
    /// (ignoring the user's own seeks) for the "saved" counter.
    fn count_trimmed(&mut self, t: f64) {
        let now = Instant::now();
        let playing = self.now.as_ref().is_some_and(|n| !n.paused && !n.loading);
        if let Some((at, prev)) = self.last_time_pos
            && playing
            && self.cfg.trim_silence != TrimMode::Off
            && self.last_seek.elapsed() > Duration::from_millis(1500)
        {
            let expected = now.duration_since(at).as_secs_f64() * self.speed;
            let jump = (t - prev) - expected;
            if jump > 0.2 && jump < 300.0 {
                self.trim_saved += jump;
            }
        }
        self.last_time_pos = Some((now, t));
    }

    fn seek_relative(&mut self, secs: f64) {
        self.last_seek = Instant::now();
        self.player.seek_relative(secs);
    }

    fn cycle_trim(&mut self) {
        self.cfg.trim_silence = self.cfg.trim_silence.next();
        self.apply_effects();
        self.info(format!("Trim Silence: {}", self.cfg.trim_silence.label()));
    }

    fn toggle_boost(&mut self) {
        self.cfg.voice_boost = !self.cfg.voice_boost;
        self.apply_effects();
        self.info(format!(
            "Voice Boost: {}",
            if self.cfg.voice_boost { "on" } else { "off" }
        ));
    }

    fn apply_effects(&mut self) {
        self.last_seek = Instant::now(); // the filter swap can nudge the position
        self.player
            .set_effects(self.cfg.trim_silence, self.cfg.voice_boost);
        if let Err(e) = config::save_config(&self.cfg) {
            self.error(format!("Couldn't save config: {e}"));
        }
    }

    // ---------- transcripts ----------

    /// Look up the transcript for a newly started episode.
    fn begin_transcript(&mut self, ep: &Episode) {
        if self.transcript_for == ep.uuid && self.transcript.is_some() {
            return;
        }
        self.stop_whisper();
        self.transcript = None;
        self.own_transcript = None;
        self.transcript_for = ep.uuid.clone();
        self.transcript_follow = true;
        self.whisper_status.clear();
        self.align_last = None;
        self.align_pending = None;
        self.align_status.clear();
        if self.cfg.transcribe == TranscribeMode::Off {
            self.transcript_state = TranscriptState::Off;
            return;
        }
        self.transcript_state = TranscriptState::Loading;
        let api = self.api.clone();
        let (uuid, podcast) = (ep.uuid.clone(), ep.podcast_uuid.clone());
        self.spawn(async move {
            let own = transcript::load_cached(&uuid);
            let whisper = transcript::load_whisper(&uuid);
            if own.is_some() {
                return Msg::Transcript(uuid, own, whisper);
            }
            let own = fetch_own_transcript(&api, &podcast, &uuid).await;
            if let Some(t) = &own {
                transcript::save_cached(&uuid, t);
            }
            Msg::Transcript(uuid, own, whisper)
        });
    }

    /// Every 30 s of playback (and after a jump), have Whisper transcribe 20 s at the
    /// current position and line the episode's own transcript up with it.
    fn maybe_autosync(&mut self) {
        if let Some(at) = self.align_pending {
            if at.elapsed() < Duration::from_secs(60) {
                return;
            }
            self.align_pending = None; // never answered; try again
        }
        if !(whisper::AVAILABLE && self.cfg.transcript_autosync && self.show_transcript) {
            return;
        }
        let Some(n) = self.now.as_ref().filter(|n| !n.loading && !n.paused) else {
            return;
        };
        let own = self
            .transcript
            .as_ref()
            .is_some_and(|t| t.source != Source::Whisper && !t.cues.is_empty());
        if !own || n.episode.uuid != self.transcript_for {
            return;
        }
        let pos = n.position;
        let due = match self.align_last {
            None => true,
            Some(at) => pos - at >= 30.0 || pos < at - 2.0,
        };
        if due {
            self.whisper
                .sample(&n.episode.uuid, &n.episode.url, pos, 20.0);
            self.align_pending = Some(Instant::now());
            self.align_last = Some(pos);
        }
    }

    /// "The highlighted line is what I'm hearing now": line the transcript up by hand.
    fn sync_to_line(&mut self, i: usize) {
        let Some(pos) = self.now.as_ref().map(|n| n.position) else {
            return;
        };
        let Some(t) = self.transcript.as_mut() else {
            return;
        };
        let Some(c) = t.cues.get(i) else { return };
        let off = c.start - pos;
        t.set_anchor(pos, off);
        self.align_last = Some(pos);
        self.align_status = format!("synced by hand ({})", offset_label(off));
        self.transcript_follow = true;
    }

    fn clear_sync(&mut self) {
        if let Some(t) = self.transcript.as_mut() {
            t.anchors.clear();
        }
        self.align_last = None;
        self.align_status = "sync cleared".into();
    }

    fn start_whisper(&mut self) {
        if !whisper::AVAILABLE || self.whisper_active {
            return;
        }
        let Some(n) = &self.now else { return };
        if n.episode.uuid != self.transcript_for {
            return;
        }
        let t = self
            .transcript
            .as_ref()
            .filter(|t| t.source == Source::Whisper);
        self.whisper
            .start(&n.episode.uuid, &n.episode.url, n.position, t);
        self.whisper_active = true;
    }

    fn stop_whisper(&mut self) {
        if self.whisper_active {
            self.whisper.stop();
            self.whisper_active = false;
        }
    }

    fn whisper_event(&mut self, ev: whisper::Event) {
        match ev {
            whisper::Event::Status(s) => self.whisper_status = s,
            whisper::Event::Sample { uuid, at, cues } => {
                self.align_pending = None;
                if uuid != self.transcript_for {
                    return;
                }
                let Some(t) = self
                    .transcript
                    .as_mut()
                    .filter(|t| t.source != Source::Whisper)
                else {
                    return;
                };
                let guess = t.offset_at(at);
                self.align_status = match transcript::align(&t.cues, &cues, guess, 900.0) {
                    Some(off) => {
                        t.set_anchor(at, off);
                        format!("auto-synced ({})", offset_label(off))
                    }
                    None if cues.is_empty() => "auto-sync: no speech here".into(),
                    None => "auto-sync: no match here (an ad?)".into(),
                };
            }
            whisper::Event::Error(e) => {
                self.whisper_status = format!("⚠ {e}");
                self.error(e);
                self.whisper_active = false;
            }
            whisper::Event::Finished(uuid) => {
                if uuid == self.transcript_for {
                    self.whisper_status = "transcribed to the end".into();
                }
            }
            whisper::Event::Cues { uuid, span, cues } => {
                let model = self.cfg.whisper_model.clone();
                if uuid == self.transcript_for
                    && let Some(t) = self
                        .transcript
                        .as_mut()
                        .filter(|t| t.source == Source::Whisper)
                {
                    t.add_whisper(span, cues);
                    t.model = model;
                    transcript::save_cached(&uuid, t);
                } else {
                    // A stale result for another episode: still cache it.
                    let mut t = transcript::load_whisper(&uuid)
                        .unwrap_or_else(|| Transcript::new(Source::Whisper, Vec::new()));
                    t.add_whisper(span, cues);
                    t.model = model;
                    transcript::save_cached(&uuid, &t);
                }
            }
        }
    }

    fn toggle_transcript_view(&mut self) {
        self.show_transcript = !self.show_transcript;
        if self.show_transcript {
            self.transcript_follow = true;
            if self
                .transcript
                .as_ref()
                .is_some_and(|t| t.source == Source::Whisper)
            {
                self.start_whisper();
            }
        } else {
            self.stop_whisper();
        }
    }

    /// `w`: switch between the episode's own transcript and Whisper's.
    fn toggle_whisper(&mut self) {
        let showing_whisper = self
            .transcript
            .as_ref()
            .is_some_and(|t| t.source == Source::Whisper);
        if showing_whisper {
            let Some(own) = self.own_transcript.take() else {
                self.info("This episode has no transcript of its own".into());
                return;
            };
            self.stop_whisper();
            self.transcript = Some(own);
            self.transcript_state = TranscriptState::Ready;
            self.align_last = None; // re-check the sync from here
            self.transcript_follow = true;
            self.info("Showing the episode's own transcript".into());
            return;
        }
        if !whisper::AVAILABLE {
            self.error("This build has no Whisper (build with --features whisper)".into());
            return;
        }
        if self.now.is_none() {
            return;
        }
        // Keep the own transcript (and its sync) to switch back to.
        self.own_transcript = self
            .transcript
            .take()
            .filter(|t| t.source != Source::Whisper);
        let cached = transcript::load_whisper(&self.transcript_for)
            .unwrap_or_else(|| Transcript::new(Source::Whisper, Vec::new()));
        self.transcript = Some(cached);
        self.transcript_state = TranscriptState::Ready;
        self.transcript_follow = true;
        self.stop_whisper();
        self.start_whisper();
        self.info("Transcribing with Whisper · w switches back".into());
    }

    /// Showing Whisper's transcript while the episode has its own to go back to.
    pub fn can_switch_back(&self) -> bool {
        self.own_transcript.is_some()
            && self
                .transcript
                .as_ref()
                .is_some_and(|t| t.source == Source::Whisper)
    }

    fn cycle_whisper_device(&mut self) {
        self.cfg.whisper_device = self.cfg.whisper_device.next();
        self.whisper_settings_changed();
        self.info(format!(
            "Whisper device: {} (runs on {})",
            self.cfg.whisper_device.label(),
            whisper::device_label(self.cfg.whisper_device)
        ));
    }

    fn cycle_whisper_model(&mut self) {
        let i = WHISPER_MODELS
            .iter()
            .position(|m| *m == self.cfg.whisper_model)
            .map_or(0, |i| (i + 1) % WHISPER_MODELS.len());
        self.cfg.whisper_model = WHISPER_MODELS[i].to_string();
        self.whisper_settings_changed();
        self.info(format!("Whisper model: {}", self.cfg.whisper_model));
    }

    fn whisper_settings_changed(&mut self) {
        self.whisper.configure(whisper_settings(&self.cfg));
        if let Err(e) = config::save_config(&self.cfg) {
            self.error(format!("Couldn't save config: {e}"));
        }
        if self.whisper_active {
            self.whisper_active = false;
            self.start_whisper();
        }
    }

    pub fn transcript_len(&self) -> usize {
        self.transcript.as_ref().map_or(0, |t| t.cues.len())
    }

    /// Move the highlighted transcript line; stops following playback.
    fn transcript_move(&mut self, delta: isize) {
        let n = self.transcript_len();
        if n == 0 {
            return;
        }
        if self.transcript_follow {
            let pos = self.now.as_ref().map_or(0.0, |n| n.position);
            self.transcript_sel = self
                .transcript
                .as_ref()
                .and_then(|t| t.current(pos))
                .unwrap_or(0);
            self.transcript_follow = false;
        }
        let cur = self.transcript_sel as isize;
        self.transcript_sel = (cur + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Jump playback to a transcript line.
    fn seek_to_cue(&mut self, i: usize) {
        let Some(t) = self.transcript.as_ref() else {
            return;
        };
        let Some(c) = t.cues.get(i) else { return };
        let start = t.to_audio(c.start).max(0.0);
        if self.now.is_some() {
            self.last_seek = Instant::now();
            self.player.seek_absolute(start);
            self.transcript_follow = true;
        }
    }

    /// Keys while the transcript view is open; `true` if handled.
    fn transcript_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            KeyCode::Esc | KeyCode::Char('T') => self.toggle_transcript_view(),
            KeyCode::Down | KeyCode::Char('j') => self.transcript_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.transcript_move(-1),
            KeyCode::PageDown => self.transcript_move(10),
            KeyCode::PageUp => self.transcript_move(-10),
            KeyCode::Home | KeyCode::Char('g') => self.transcript_move(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.transcript_move(isize::MAX / 2),
            KeyCode::Enter if !self.transcript_follow => self.seek_to_cue(self.transcript_sel),
            KeyCode::Char('c') => self.transcript_follow = true,
            KeyCode::Char('s') if !self.transcript_follow => self.sync_to_line(self.transcript_sel),
            KeyCode::Char('z') => self.clear_sync(),
            KeyCode::Char('w') => self.toggle_whisper(),
            KeyCode::Char('D') => self.cycle_whisper_device(),
            KeyCode::Char('M') => self.cycle_whisper_model(),
            _ => return false,
        }
        true
    }

    /// Called a few times a second.
    pub fn tick(&mut self) {
        if let Some((_, _, at)) = &self.status
            && at.elapsed() > Duration::from_secs(6)
        {
            self.status = None;
        }
        if self.whisper_active
            && let Some(n) = &self.now
        {
            self.whisper.position(n.position);
        }
        self.maybe_autosync();
        let playing = self.now.as_ref().is_some_and(|n| !n.paused && !n.loading);
        if playing
            && self.last_sync.elapsed() >= Duration::from_secs(self.cfg.sync_interval.max(5).into())
        {
            self.sync_progress(false);
        }
    }

    /// Push the current position to Pocket Casts.
    fn sync_progress(&mut self, force: bool) {
        let Some(n) = &self.now else { return };
        if n.loading || (!force && (n.position - self.last_synced_pos).abs() < 1.0) {
            return;
        }
        self.last_sync = Instant::now();
        self.last_synced_pos = n.position;
        let (ep, pos, dur) = (n.episode.clone(), n.position, n.duration);
        self.update_local(&ep.uuid, |e| {
            e.played_up_to = pos;
            e.status = PlayStatus::InProgress;
            if dur > 0.0 {
                e.duration = dur;
            }
        });
        let api = self.api.clone();
        self.fire(None, false, async move {
            api.save_progress(&ep, pos, dur).await
        });
    }

    /// Final sync before exit; waits briefly so the request gets out.
    pub async fn shutdown(&mut self) {
        if let Some(n) = &self.now
            && !n.loading
            && n.position > 0.0
        {
            let (ep, pos, dur) = (n.episode.clone(), n.position, n.duration);
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                self.api.save_progress(&ep, pos, dur),
            )
            .await;
        }
        // Remember the volume for next time.
        let vol = self.volume.round().clamp(0.0, 130.0) as u32;
        if vol != self.cfg.volume {
            self.cfg.volume = vol;
            let _ = config::save_config(&self.cfg);
        }
        self.player.quit();
    }

    fn on_finished(&mut self) {
        let Some(n) = self.now.take() else { return };
        let ep = n.episode;
        self.update_local(&ep.uuid, |e| {
            e.status = PlayStatus::Played;
            e.played_up_to = e.duration;
        });
        self.up_next.retain(|e| e.uuid != ep.uuid);
        self.in_progress.retain(|e| e.uuid != ep.uuid);
        self.clamp(Pane::UpNext);
        self.clamp(Pane::PodcastEpisodes);
        let api = self.api.clone();
        let done = ep.clone();
        self.fire(None, false, async move {
            api.set_status(&done, PlayStatus::Played).await?;
            api.remove_up_next(&done.uuid).await
        });
        match self.up_next.first().cloned() {
            Some(next) => self.play(next, false),
            None => self.info(format!("Finished {}", ep.title)),
        }
    }

    // ---------- playback ----------

    /// Start an episode. `promote` also moves it to the top of Up Next (as the apps do).
    fn play(&mut self, ep: Episode, promote: bool) {
        if self.now.as_ref().is_some_and(|n| n.episode.uuid == ep.uuid) {
            self.player.toggle_pause();
            return;
        }
        self.sync_progress(true);
        self.last_synced_pos = -1.0;
        self.last_seek = Instant::now();
        self.awaiting_start = true;
        self.now = Some(NowPlaying {
            episode: ep.clone(),
            position: ep.played_up_to,
            duration: ep.duration,
            paused: false,
            buffering: false,
            loading: true,
        });
        self.art.want(&ep.podcast_uuid);
        let api = self.api.clone();
        let e = ep.clone();
        self.spawn(async move {
            let details = api.episode(&e.uuid).await.ok();
            Msg::Start(e, details)
        });
        let at_top = self.up_next.first().is_some_and(|e| e.uuid == ep.uuid);
        if promote && !at_top {
            let api = self.api.clone();
            self.fire(None, true, async move { api.play_now(&ep).await });
        }
    }

    pub fn play_next_in_queue(&mut self) {
        let current = self.now.as_ref().map(|n| n.episode.uuid.clone());
        let next = self
            .up_next
            .iter()
            .find(|e| Some(&e.uuid) != current.as_ref())
            .cloned();
        match next {
            Some(ep) => {
                if let Some(cur) = current {
                    self.up_next.retain(|e| e.uuid != cur);
                    let api = self.api.clone();
                    self.fire(None, false, async move { api.remove_up_next(&cur).await });
                }
                self.play(ep, false);
            }
            None => self.info("Up Next is empty".into()),
        }
    }

    fn change_speed(&mut self, delta: f64) {
        let s = ((self.speed + delta) * 10.0).round() / 10.0;
        self.speed = s.clamp(0.5, 3.0);
        self.player.set_speed(self.speed);
    }

    // ---------- lists ----------

    pub fn list(&self, pane: Pane) -> &[Episode] {
        match pane {
            Pane::UpNext => &self.up_next,
            Pane::NewReleases => &self.new_releases,
            Pane::InProgress => &self.in_progress,
            Pane::PodcastEpisodes => &self.podcast_episodes,
            Pane::Podcasts | Pane::Search => &[],
        }
    }

    /// The podcasts behind a grid pane.
    pub fn podcast_list(&self, pane: Pane) -> &[Podcast] {
        match pane {
            Pane::Search => &self.search_results,
            _ => &self.podcasts,
        }
    }

    pub fn is_grid(pane: Pane) -> bool {
        matches!(pane, Pane::Podcasts | Pane::Search)
    }

    pub fn is_following(&self, uuid: &str) -> bool {
        self.podcasts.iter().any(|p| p.uuid == uuid)
    }

    fn list_mut(&mut self, pane: Pane) -> &mut Vec<Episode> {
        match pane {
            Pane::UpNext => &mut self.up_next,
            Pane::NewReleases => &mut self.new_releases,
            Pane::InProgress => &mut self.in_progress,
            _ => &mut self.podcast_episodes,
        }
    }

    fn update_local(&mut self, uuid: &str, f: impl Fn(&mut Episode)) {
        for list in [
            &mut self.up_next,
            &mut self.new_releases,
            &mut self.in_progress,
            &mut self.podcast_episodes,
        ] {
            list.iter_mut().filter(|e| e.uuid == uuid).for_each(&f);
        }
    }

    fn matches(&self, text: &str) -> bool {
        self.filter.is_empty() || text.to_lowercase().contains(&self.filter.to_lowercase())
    }

    /// Indices into the pane's list that pass the filter.
    pub fn visible(&self, pane: Pane) -> Vec<usize> {
        if Self::is_grid(pane) {
            let list = self.podcast_list(pane);
            return (0..list.len())
                .filter(|&i| self.matches(&list[i].title) || self.matches(&list[i].author))
                .collect();
        }
        let list = self.list(pane);
        let prefs = if pane == Pane::PodcastEpisodes {
            self.open_prefs()
        } else {
            EpisodePrefs::default()
        };
        let mut v: Vec<usize> = (0..list.len())
            .filter(|&i| self.matches(&list[i].title) || self.matches(&list[i].podcast_title))
            .filter(|&i| !(prefs.hide_played && list[i].status == PlayStatus::Played))
            .collect();
        // Podcast episodes arrive newest first.
        if prefs.oldest_first {
            v.reverse();
        }
        v
    }

    /// Sort / hide-played settings of the open podcast.
    pub fn open_prefs(&self) -> EpisodePrefs {
        self.open_podcast
            .as_ref()
            .and_then(|p| self.episode_prefs.get(&p.uuid).copied())
            .unwrap_or_default()
    }

    fn change_open_prefs(&mut self, f: impl FnOnce(&mut EpisodePrefs)) {
        let Some(uuid) = self.open_podcast.as_ref().map(|p| p.uuid.clone()) else {
            return;
        };
        f(self.episode_prefs.entry(uuid).or_default());
        self.tables[Pane::PodcastEpisodes as usize].select(Some(0));
        if let Err(e) = config::save_episode_prefs(&self.episode_prefs) {
            self.error(format!("Couldn't save podcast settings: {e}"));
        }
    }

    fn toggle_sort(&mut self) {
        self.change_open_prefs(|p| p.oldest_first = !p.oldest_first);
        let label = if self.open_prefs().oldest_first {
            "Oldest first"
        } else {
            "Newest first"
        };
        self.info(label.into());
    }

    fn toggle_hide_played(&mut self) {
        self.change_open_prefs(|p| p.hide_played = !p.hide_played);
        let label = if self.open_prefs().hide_played {
            "Hiding played episodes"
        } else {
            "Showing played episodes"
        };
        self.info(label.into());
    }

    fn clamp(&mut self, pane: Pane) {
        let n = self.visible(pane).len();
        let t = &mut self.tables[pane as usize];
        let sel = t.selected().unwrap_or(0).min(n.saturating_sub(1));
        t.select(Some(sel));
    }

    fn selected_index(&self) -> Option<usize> {
        let sel = self.tables[self.pane as usize].selected()?;
        self.visible(self.pane).get(sel).copied()
    }

    fn selected_episode(&self) -> Option<Episode> {
        if Self::is_grid(self.pane) {
            return None;
        }
        self.selected_index()
            .map(|i| self.list(self.pane)[i].clone())
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.visible(self.pane).len() as isize;
        if n == 0 {
            return;
        }
        let t = &mut self.tables[self.pane as usize];
        let cur = t.selected().unwrap_or(0) as isize;
        t.select(Some((cur + delta).clamp(0, n - 1) as usize));
    }

    fn switch_pane(&mut self, idx: usize) {
        if self.show_transcript {
            self.toggle_transcript_view();
        }
        self.sidebar = idx.min(SIDEBAR.len() - 1);
        self.pane = SIDEBAR[self.sidebar];
        if self.open_podcast.is_some() && self.episodes_from == self.pane {
            self.pane = Pane::PodcastEpisodes;
        }
        self.filter.clear();
        self.filtering = false;
        self.search_editing = self.pane == Pane::Search && self.search_results.is_empty();
    }

    fn open_selected_podcast(&mut self) {
        let Some(i) = self.selected_index() else {
            return;
        };
        let p = self.podcast_list(self.pane)[i].clone();
        self.episodes_from = self.pane;
        let same = self.open_podcast.as_ref().is_some_and(|o| o.uuid == p.uuid);
        if !same {
            self.podcast_episodes.clear();
            self.tables[Pane::PodcastEpisodes as usize] = TableState::default().with_selected(0);
        }
        self.open_podcast = Some(p);
        self.pane = Pane::PodcastEpisodes;
        self.filter.clear();
        self.refresh(Pane::PodcastEpisodes);
    }

    fn close_podcast(&mut self) {
        self.open_podcast = None;
        self.pane = self.episodes_from;
        self.filter.clear();
    }

    pub fn info(&mut self, m: String) {
        self.status = Some((m, false, Instant::now()));
    }

    pub fn error(&mut self, m: String) {
        self.status = Some((m, true, Instant::now()));
    }

    // ---------- keys ----------

    pub fn handle_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.search_editing {
            self.search_key(k);
            return;
        }
        if self.show_transcript && self.transcript_key(k) {
            return;
        }
        if self.filtering {
            match k.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                }
                KeyCode::Enter => {
                    self.filtering = false;
                    return;
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c)
                    if !k
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
                {
                    self.filter.push(c)
                }
                KeyCode::Up | KeyCode::Down => {
                    self.main_key(k);
                    return;
                }
                _ => return,
            }
            self.tables[self.pane as usize].select(Some(0));
            return;
        }

        let back = self.cfg.skip_back as f64;
        let fwd = self.cfg.skip_forward as f64;
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(' ') | KeyCode::Char('p') => self.toggle_play(),
            KeyCode::Left if self.in_grid() => self.move_sel(-1),
            KeyCode::Right if self.in_grid() => self.move_sel(1),
            KeyCode::Left | KeyCode::Char(',') if self.now.is_some() => self.seek_relative(-back),
            KeyCode::Right | KeyCode::Char('.') if self.now.is_some() => self.seek_relative(fwd),
            KeyCode::Char('<') if self.now.is_some() => {
                self.last_seek = Instant::now();
                self.player.seek_absolute(0.0)
            }
            KeyCode::Char('t') => self.cycle_trim(),
            KeyCode::Char('b') => self.toggle_boost(),
            KeyCode::Char('[') => self.change_speed(-0.1),
            KeyCode::Char(']') => self.change_speed(0.1),
            KeyCode::Char('=') => self.change_speed(1.0 - self.speed),
            KeyCode::Char('-') => self.change_volume(-5.0),
            KeyCode::Char('+') => self.change_volume(5.0),
            KeyCode::Char('n') => self.play_next_in_queue(),
            KeyCode::Char('r') => {
                self.refresh(self.pane);
                self.info("Refreshing…".into());
            }
            KeyCode::Char('/' | 's') if self.pane == Pane::Search => {
                self.focus = Focus::Main;
                self.search_editing = true;
            }
            KeyCode::Char('/') if self.focus == Focus::Main => {
                self.filtering = true;
                self.filter.clear();
            }
            KeyCode::Char(c @ '1'..='5') => {
                self.switch_pane(c as usize - '1' as usize);
                self.focus = Focus::Main;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Main,
                    Focus::Main => Focus::Sidebar,
                }
            }
            KeyCode::Char('h') => self.focus = Focus::Sidebar,
            KeyCode::Char('l') => self.focus = Focus::Main,
            KeyCode::Char('T') => self.toggle_transcript_view(),
            _ if self.show_transcript => {}
            _ => match self.focus {
                Focus::Sidebar => self.sidebar_key(k),
                Focus::Main => self.main_key(k),
            },
        }
    }

    fn sidebar_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.switch_pane(self.sidebar + 1),
            KeyCode::Up | KeyCode::Char('k') => self.switch_pane(self.sidebar.saturating_sub(1)),
            KeyCode::Enter => self.focus = Focus::Main,
            _ => {}
        }
    }

    /// Typing into the search box.
    fn search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Enter => {
                self.search_editing = false;
                if self.search_query.trim() != self.searched || self.search_results.is_empty() {
                    self.refresh(Pane::Search);
                }
            }
            KeyCode::Esc => {
                self.search_editing = false;
                if self.search_results.is_empty() {
                    self.focus = Focus::Sidebar;
                }
            }
            KeyCode::Down | KeyCode::Tab => self.search_editing = false,
            KeyCode::Backspace => {
                self.search_query.pop();
            }
            KeyCode::Char(c)
                if !k
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
            {
                self.search_query.push(c)
            }
            _ => {}
        }
    }

    /// Follow the selected (or open) podcast, or unfollow it on a second press.
    fn toggle_follow(&mut self) {
        let podcast = match self.pane {
            Pane::Podcasts | Pane::Search => self
                .selected_index()
                .map(|i| self.podcast_list(self.pane)[i].clone()),
            Pane::PodcastEpisodes => self.open_podcast.clone(),
            _ => None,
        };
        let Some(p) = podcast else { return };
        let api = self.api.clone();
        let tx = self.tx.clone();
        if self.is_following(&p.uuid) {
            let confirmed = self
                .pending_unfollow
                .as_ref()
                .is_some_and(|(u, at)| *u == p.uuid && at.elapsed() < Duration::from_secs(4));
            if !confirmed {
                self.pending_unfollow = Some((p.uuid.clone(), Instant::now()));
                self.info(format!("Press f again to unfollow {}", p.title));
                return;
            }
            self.pending_unfollow = None;
            self.podcasts.retain(|x| x.uuid != p.uuid);
            self.clamp(Pane::Podcasts);
            self.info(format!("Unfollowed {}", p.title));
            tokio::spawn(async move {
                if let Err(e) = api.unsubscribe(&p.uuid).await {
                    let _ = tx.send(Msg::Error(err_text(&e)));
                }
                let _ = tx.send(Msg::Podcasts(api.subscriptions().await));
            });
        } else {
            self.podcasts.push(p.clone());
            self.podcasts.sort_by_key(|x| x.title.to_lowercase());
            self.info(format!("Following {}", p.title));
            tokio::spawn(async move {
                if let Err(e) = api.subscribe(&p.uuid).await {
                    let _ = tx.send(Msg::Error(err_text(&e)));
                }
                let _ = tx.send(Msg::Podcasts(api.subscriptions().await));
            });
        }
    }

    /// A podcast grid has focus, so arrows move between tiles.
    fn in_grid(&self) -> bool {
        Self::is_grid(self.pane) && self.focus == Focus::Main && !self.filtering
    }

    fn main_key(&mut self, k: KeyEvent) {
        let row = if Self::is_grid(self.pane) {
            self.grid_cols.max(1) as isize
        } else {
            1
        };
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(row),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-row),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(isize::MAX / 2),
            KeyCode::Esc | KeyCode::Backspace => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                } else if self.pane == Pane::PodcastEpisodes {
                    self.close_podcast();
                } else {
                    self.focus = Focus::Sidebar;
                }
            }
            KeyCode::Char('a') => self.queue(false),
            KeyCode::Char('A') => self.queue(true),
            KeyCode::Char('d') | KeyCode::Delete => self.remove_from_queue(),
            KeyCode::Char('m') => self.mark(PlayStatus::Played),
            KeyCode::Char('u') => self.mark(PlayStatus::Unplayed),
            KeyCode::Char('f') => self.toggle_follow(),
            KeyCode::Char('o') if self.pane == Pane::PodcastEpisodes => self.toggle_sort(),
            KeyCode::Char('x') if self.pane == Pane::PodcastEpisodes => self.toggle_hide_played(),
            KeyCode::Enter => self.activate(),
            _ => {}
        }
    }

    /// Enter / double-click: open a podcast or play an episode.
    fn activate(&mut self) {
        if Self::is_grid(self.pane) {
            self.open_selected_podcast();
        } else if let Some(ep) = self.selected_episode() {
            self.play(ep, true);
        }
    }

    fn toggle_play(&mut self) {
        if self.now.is_some() {
            self.player.toggle_pause();
        } else if let Some(ep) = self.up_next.first().cloned() {
            self.play(ep, false);
        }
    }

    fn change_volume(&mut self, delta: f64) {
        self.volume = (self.volume + delta).clamp(0.0, 130.0);
        self.player.set_volume(self.volume);
    }

    // ---------- mouse ----------

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pos))
            .copied();
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.dragging_progress = false;
                if let Some((rect, h)) = hit {
                    self.click(rect, h, pos);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some((rect, h)) = hit {
                    self.right_click(rect, h, pos);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_progress => {
                if let Some((rect, _)) = self.hits.iter().find(|(_, h)| *h == Hit::Progress) {
                    self.seek_to_column(*rect, m.column);
                }
            }
            MouseEventKind::Up(_) => self.dragging_progress = false,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                let dir: isize = if up { -1 } else { 1 };
                match hit.map(|(_, h)| h) {
                    Some(Hit::Table(p)) if p == self.pane => {
                        self.focus = Focus::Main;
                        self.move_sel(dir);
                    }
                    Some(Hit::Tile(p, _)) if p == self.pane => {
                        self.focus = Focus::Main;
                        self.move_sel(dir * self.grid_cols.max(1) as isize);
                    }
                    Some(Hit::Sidebar(_)) => {
                        let i = (self.sidebar as isize + dir).clamp(0, SIDEBAR.len() as isize - 1);
                        self.switch_pane(i as usize);
                    }
                    Some(Hit::Volume | Hit::Control(Control::VolumeDown | Control::VolumeUp)) => {
                        self.change_volume(if up { 5.0 } else { -5.0 })
                    }
                    Some(Hit::Speed) => self.change_speed(if up { 0.1 } else { -0.1 }),
                    Some(Hit::TranscriptLine(_) | Hit::TranscriptArea) => {
                        self.transcript_move(dir * 3)
                    }
                    Some(Hit::Progress) if self.now.is_some() => {
                        let secs = if up {
                            self.cfg.skip_forward
                        } else {
                            self.cfg.skip_back
                        };
                        self.seek_relative(if up { 1.0 } else { -1.0 } * f64::from(secs));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Which item of a table/tile hit the mouse is on, if any.
    fn item_at(&self, rect: Rect, hit: Hit, pos: Position) -> Option<(Pane, usize)> {
        match hit {
            Hit::Table(p) => {
                let i = self.tables[p as usize].offset() + usize::from(pos.y - rect.y);
                (i < self.visible(p).len()).then_some((p, i))
            }
            Hit::Tile(p, i) => Some((p, i)),
            _ => None,
        }
    }

    fn click(&mut self, rect: Rect, hit: Hit, pos: Position) {
        if self.show_help {
            self.show_help = false;
            return;
        }
        if hit != Hit::SearchBox {
            self.search_editing = false;
        }
        match hit {
            Hit::Sidebar(i) => {
                self.switch_pane(i);
                self.focus = Focus::Main;
            }
            Hit::Table(_) | Hit::Tile(..) => {
                let Some((pane, i)) = self.item_at(rect, hit, pos) else {
                    return;
                };
                if pane != self.pane {
                    return;
                }
                self.focus = Focus::Main;
                let double = self.last_click.is_some_and(|(h, j, at)| {
                    std::mem::discriminant(&h) == std::mem::discriminant(&hit)
                        && j == i
                        && at.elapsed() < Duration::from_millis(450)
                });
                self.tables[pane as usize].select(Some(i));
                if double {
                    self.last_click = None;
                    self.activate();
                } else {
                    self.last_click = Some((hit, i, Instant::now()));
                }
            }
            Hit::Back => {
                if self.pane == Pane::PodcastEpisodes {
                    self.close_podcast();
                }
            }
            Hit::SearchBox => {
                self.focus = Focus::Main;
                self.search_editing = true;
            }
            Hit::Progress => {
                self.dragging_progress = true;
                self.seek_to_column(rect, pos.x);
            }
            Hit::Volume => {
                let frac =
                    f64::from(pos.x - rect.x) / f64::from(rect.width.saturating_sub(1).max(1));
                self.volume = (frac * 100.0).round().clamp(0.0, 100.0);
                self.player.set_volume(self.volume);
            }
            Hit::Speed => self.change_speed(1.0 - self.speed),
            Hit::Control(c) => match c {
                Control::SkipBack if self.now.is_some() => {
                    self.seek_relative(-f64::from(self.cfg.skip_back))
                }
                Control::SkipForward if self.now.is_some() => {
                    self.seek_relative(f64::from(self.cfg.skip_forward))
                }
                Control::PlayPause => self.toggle_play(),
                Control::Next => self.play_next_in_queue(),
                Control::Trim => self.cycle_trim(),
                Control::Boost => self.toggle_boost(),
                Control::VolumeDown => self.change_volume(-5.0),
                Control::VolumeUp => self.change_volume(5.0),
                Control::Transcript => self.toggle_transcript_view(),
                _ => {}
            },
            Hit::Help => self.show_help = false,
            Hit::SortToggle => self.toggle_sort(),
            Hit::PlayedToggle => self.toggle_hide_played(),
            Hit::TranscriptLine(i) => self.seek_to_cue(i),
            Hit::TranscriptArea => {}
        }
    }

    /// Right-click: queue an episode, follow/unfollow a podcast.
    fn right_click(&mut self, rect: Rect, hit: Hit, pos: Position) {
        if let Hit::TranscriptLine(i) = hit {
            self.sync_to_line(i);
            return;
        }
        let Some((pane, i)) = self.item_at(rect, hit, pos) else {
            return;
        };
        if pane != self.pane {
            return;
        }
        self.focus = Focus::Main;
        self.tables[pane as usize].select(Some(i));
        if Self::is_grid(pane) {
            self.toggle_follow();
        } else {
            self.queue(false);
        }
    }

    fn seek_to_column(&mut self, rect: Rect, x: u16) {
        let Some(n) = &self.now else { return };
        if n.duration <= 0.0 {
            return;
        }
        let frac = f64::from(x.saturating_sub(rect.x)) / f64::from(rect.width.max(1));
        self.last_seek = Instant::now();
        self.player.seek_absolute(frac.clamp(0.0, 1.0) * n.duration);
    }

    fn queue(&mut self, next: bool) {
        let Some(ep) = self.selected_episode() else {
            return;
        };
        if self.up_next.iter().any(|e| e.uuid == ep.uuid) {
            self.info("Already in Up Next".into());
            return;
        }
        let api = self.api.clone();
        let title = ep.title.clone();
        if next {
            let pos = usize::from(self.now.is_some()).min(self.up_next.len());
            self.up_next.insert(pos, ep.clone());
            self.fire(Some(format!("Playing next: {title}")), true, async move {
                api.play_next(&ep).await
            });
        } else {
            self.up_next.push(ep.clone());
            self.fire(
                Some(format!("Added to Up Next: {title}")),
                true,
                async move { api.play_last(&ep).await },
            );
        }
    }

    fn remove_from_queue(&mut self) {
        if self.pane != Pane::UpNext {
            return;
        }
        let Some(ep) = self.selected_episode() else {
            return;
        };
        self.up_next.retain(|e| e.uuid != ep.uuid);
        self.clamp(Pane::UpNext);
        let api = self.api.clone();
        let uuid = ep.uuid.clone();
        self.fire(Some(format!("Removed: {}", ep.title)), false, async move {
            api.remove_up_next(&uuid).await
        });
    }

    fn mark(&mut self, status: PlayStatus) {
        let Some(ep) = self.selected_episode() else {
            return;
        };
        if self.now.as_ref().is_some_and(|n| n.episode.uuid == ep.uuid) {
            self.player.stop();
            self.now = None;
        }
        self.update_local(&ep.uuid, |e| {
            e.status = status;
            e.played_up_to = if status == PlayStatus::Played {
                e.duration
            } else {
                0.0
            };
        });
        let api = self.api.clone();
        let label = if status == PlayStatus::Played {
            "played"
        } else {
            "unplayed"
        };
        let refresh_queue =
            status == PlayStatus::Played && self.up_next.iter().any(|e| e.uuid == ep.uuid);
        if status == PlayStatus::Played {
            self.up_next.retain(|e| e.uuid != ep.uuid);
            self.in_progress.retain(|e| e.uuid != ep.uuid);
            self.clamp(Pane::UpNext);
            self.clamp(Pane::InProgress);
        }
        self.clamp(Pane::PodcastEpisodes);
        let msg = format!("Marked {label}: {}", ep.title);
        self.fire(Some(msg), false, async move {
            api.set_status(&ep, status).await?;
            if refresh_queue {
                api.remove_up_next(&ep.uuid).await?;
            }
            Ok(())
        });
    }
}

fn err_text(e: &anyhow::Error) -> String {
    if api::is_auth_expired(e) {
        return e.to_string();
    }
    format!("{e:#}")
}

fn whisper_settings(cfg: &Config) -> whisper::Settings {
    whisper::Settings {
        model: cfg.whisper_model.clone(),
        device: cfg.whisper_device,
        threads: cfg.whisper_threads,
        ahead: cfg.transcribe_ahead,
        mpv: cfg.mpv_path.clone(),
    }
}

/// The publisher's transcript, else Pocket Casts' generated one, if the episode has either.
async fn fetch_own_transcript(api: &Client, podcast: &str, uuid: &str) -> Option<Transcript> {
    if podcast.is_empty() {
        return None;
    }
    let notes = api.show_notes(podcast).await.ok()?;
    for offer in transcript::offers(notes.get(uuid)?) {
        let Some(format) = transcript::format_of(&offer.mime, &offer.url) else {
            continue;
        };
        let Ok(body) = api.fetch_text(&offer.url).await else {
            continue;
        };
        let cues = transcript::parse(&body, format);
        if !cues.is_empty() {
            return Some(Transcript::new(offer.source, cues));
        }
    }
    None
}

/// `+1:44 ahead` / `0:12 behind` / `in step` for a transcript offset.
pub fn offset_label(off: f64) -> String {
    if off.abs() < 1.0 {
        "in step".into()
    } else if off > 0.0 {
        format!("text +{} ahead", crate::model::fmt_time(off))
    } else {
        format!("text {} behind", crate::model::fmt_time(-off))
    }
}
