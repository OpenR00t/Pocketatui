// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Settings, the saved session, and the first-run login prompt.
//!
//! Files live in the OS config dir (`%APPDATA%\pocketatui\config` on Windows):
//! - `config.toml`  user settings (written with defaults on first run)
//! - `session.toml` email + session token
//!
//! The password, if you choose to save it, goes in the OS keychain (Windows Credential
//! Manager) so an expired token can be renewed without asking again.

use std::{
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "pocketatui";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// mpv executable; a bare name is looked up on PATH.
    pub mpv_path: String,
    /// Seconds jumped by the skip-back key.
    pub skip_back: u32,
    /// Seconds jumped by the skip-forward key.
    pub skip_forward: u32,
    /// Playback speed new episodes start at.
    pub default_speed: f64,
    /// Seconds between progress syncs while playing.
    pub sync_interval: u32,
    /// Start mpv volume, 0-100.
    pub volume: u32,
    /// Podcast covers in the Podcasts grid and playbar.
    pub cover_art: bool,
    /// Skip silences: off, mild, medium or mad-max (`t` cycles it).
    pub trim_silence: TrimMode,
    /// Make voices louder and even out quiet speech (`b` toggles it).
    pub voice_boost: bool,
    /// Transcripts: off, auto (the episode's own transcript, else Whisper), or whisper.
    pub transcribe: TranscribeMode,
    /// Whisper model, downloaded on first use: tiny.en, base.en, small.en, medium.en,
    /// large-v3-turbo-q5_0 (and the same without `.en` for other languages).
    pub whisper_model: String,
    /// auto, cpu or gpu. GPU needs a build with the `vulkan` or `cuda` feature.
    pub whisper_device: WhisperDevice,
    /// CPU threads for Whisper; 0 = half the cores (at most 8).
    pub whisper_threads: u32,
    /// How many seconds of audio Whisper keeps transcribed ahead of playback.
    pub transcribe_ahead: u32,
    /// Keep the episode's own transcript lined up with your copy of the audio (whose ads
    /// may differ) by checking a short Whisper sample every 30 s. Needs a Whisper build.
    pub transcript_autosync: bool,
    /// Skip ads stitched into your download: Whisper scans ahead and compares your audio
    /// with the episode's own transcript. Needs a Whisper build and an episode transcript.
    pub skip_ads: bool,
    /// Mark likely host-read sponsor segments (in blue). Never skipped.
    pub sponsor_hints: bool,
    /// Size cap for cached transcripts and covers, in MB; the least recently used go
    /// first. 0 = no cap. Whisper models aren't counted.
    pub cache_limit_mb: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscribeMode {
    Off,
    #[default]
    Auto,
    Whisper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperDevice {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

impl WhisperDevice {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Cpu,
            Self::Cpu => Self::Gpu,
            Self::Gpu => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
        }
    }
}

/// Whisper models offered by the in-app cycle key, smallest first.
pub const WHISPER_MODELS: &[&str] = &[
    "tiny.en",
    "base.en",
    "small.en",
    "medium.en",
    "large-v3-turbo-q5_0",
];

/// Trim Silence levels, after the Pocket Casts mobile app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrimMode {
    #[default]
    Off,
    Mild,
    Medium,
    MadMax,
}

impl TrimMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Mild,
            Self::Mild => Self::Medium,
            Self::Medium => Self::MadMax,
            Self::MadMax => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Mild => "Mild",
            Self::Medium => "Medium",
            Self::MadMax => "Mad Max",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mpv_path: "mpv".into(),
            skip_back: 10,
            skip_forward: 30,
            default_speed: 1.0,
            sync_interval: 30,
            volume: 100,
            cover_art: true,
            trim_silence: TrimMode::Off,
            voice_boost: false,
            transcribe: TranscribeMode::Auto,
            whisper_model: "small.en".into(),
            whisper_device: WhisperDevice::Auto,
            whisper_threads: 0,
            transcribe_ahead: 120,
            transcript_autosync: true,
            skip_ads: true,
            sponsor_hints: true,
            cache_limit_mb: 200,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub email: String,
    pub token: String,
}

fn dir() -> Result<PathBuf> {
    let d = ProjectDirs::from("", "", "pocketatui").context("no config directory")?;
    let p = d.config_dir().to_path_buf();
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

pub fn config_path() -> Result<PathBuf> {
    Ok(dir()?.join("config.toml"))
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        let cfg = Config::default();
        std::fs::write(&path, toml::to_string_pretty(&cfg)?)?;
        return Ok(cfg);
    }
    let text = std::fs::read_to_string(&path)?;
    toml::from_str(&text).with_context(|| format!("bad config at {}", path.display()))
}

/// Write settings back (used when effects are toggled in the app).
pub fn save_config(cfg: &Config) -> Result<()> {
    std::fs::write(config_path()?, toml::to_string_pretty(cfg)?)?;
    Ok(())
}

fn session_path() -> Result<PathBuf> {
    Ok(dir()?.join("session.toml"))
}

/// How one podcast's episode list is shown; remembered per podcast in `podcasts.toml`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EpisodePrefs {
    pub oldest_first: bool,
    pub hide_played: bool,
}

fn prefs_path() -> Result<PathBuf> {
    Ok(dir()?.join("podcasts.toml"))
}

pub fn load_episode_prefs() -> std::collections::HashMap<String, EpisodePrefs> {
    prefs_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_episode_prefs(prefs: &std::collections::HashMap<String, EpisodePrefs>) -> Result<()> {
    // Only keep podcasts that differ from the default.
    let kept: std::collections::BTreeMap<_, _> = prefs
        .iter()
        .filter(|(_, p)| **p != EpisodePrefs::default())
        .collect();
    std::fs::write(prefs_path()?, toml::to_string(&kept)?)?;
    Ok(())
}

pub fn load_session() -> Option<Session> {
    let text = std::fs::read_to_string(session_path().ok()?).ok()?;
    toml::from_str::<Session>(&text)
        .ok()
        .filter(|s| !s.token.is_empty())
}

pub fn save_session(s: &Session) -> Result<()> {
    std::fs::write(session_path()?, toml::to_string(s)?)?;
    Ok(())
}

pub fn clear_session(email: Option<&str>) {
    if let Ok(p) = session_path() {
        let _ = std::fs::remove_file(p);
    }
    if let Some(e) = email
        && let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, e)
    {
        let _ = entry.delete_credential();
    }
}

pub fn stored_password(email: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, email)
        .ok()?
        .get_password()
        .ok()
}

fn store_password(email: &str, password: &str) -> Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, email)?.set_password(password)?;
    Ok(())
}

/// Ask for credentials on the plain terminal (before the TUI starts) and log in.
/// `POCKETCASTS_EMAIL` / `POCKETCASTS_PASSWORD` skip the prompt.
pub async fn interactive_login() -> Result<(Session, Option<String>)> {
    let env_email = std::env::var("POCKETCASTS_EMAIL").ok();
    let env_pass = std::env::var("POCKETCASTS_PASSWORD").ok();
    if let (Some(email), Some(password)) = (env_email, env_pass) {
        let token = crate::api::Client::login(&email, &password).await?;
        let s = Session { email, token };
        save_session(&s)?;
        return Ok((s, Some(password)));
    }

    println!("Log in to Pocket Casts (the account you use in the app).");
    loop {
        let email = prompt("Email: ")?;
        let password = rpassword::prompt_password("Password: ")?;
        match crate::api::Client::login(&email, &password).await {
            Ok(token) => {
                let s = Session { email, token };
                save_session(&s)?;
                let keep = prompt(
                    "Save password in the system keychain so expired sessions renew themselves? [Y/n] ",
                )?;
                let mut saved = None;
                if !keep.eq_ignore_ascii_case("n") {
                    match store_password(&s.email, &password) {
                        Ok(()) => saved = Some(password),
                        Err(e) => println!(
                            "Couldn't save to the keychain ({e}); you'll be asked again when the session expires."
                        ),
                    }
                }
                return Ok((s, saved));
            }
            Err(e) => println!("{e}. Try again (Ctrl-C to quit)."),
        }
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}
