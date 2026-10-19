// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Audio playback through a headless mpv driven over its JSON IPC.
//!
//! mpv handles streaming, seeking and pitch-preserving speed changes. On Windows the IPC
//! endpoint is a named pipe (`\\.\pipe\...`); elsewhere a Unix socket.

use std::time::Duration;

use crate::config::TrimMode;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc::{UnboundedSender, unbounded_channel},
};

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    TimePos(f64),
    Duration(f64),
    Paused(bool),
    Speed(f64),
    Volume(f64),
    Buffering(bool),
    /// Playback reached the end of the file.
    Finished,
    /// The file couldn't be played.
    Failed(String),
    /// mpv went away.
    Exited,
}

pub struct Player {
    tx: UnboundedSender<String>,
    _child: Child,
}

const OBSERVED: &[(u64, &str)] = &[
    (1, "time-pos"),
    (2, "duration"),
    (3, "pause"),
    (4, "speed"),
    (5, "paused-for-cache"),
    (6, "volume"),
];

impl Player {
    pub async fn spawn(
        mpv: &str,
        volume: u32,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Self> {
        let endpoint = ipc_endpoint();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&endpoint);

        let mut cmd = Command::new(mpv);
        cmd.args([
            "--idle=yes",
            "--no-video",
            "--no-terminal",
            "--force-window=no",
            "--audio-display=no",
            "--keep-open=no",
            &format!("--volume={}", volume.min(130)),
            &format!("--input-ipc-server={endpoint}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let child = cmd.spawn().with_context(|| {
            format!("couldn't start mpv (`{mpv}`). Install it (e.g. `scoop install extras/mpv`) or set mpv_path in config.toml")
        })?;

        let (tx, rx) = unbounded_channel::<String>();
        let stream = connect(&endpoint).await?;
        let (read, write) = tokio::io::split(stream);
        tokio::spawn(writer(write, rx));
        tokio::spawn(reader(read, events));

        let p = Self { tx, _child: child };
        for (id, name) in OBSERVED {
            p.send(json!({"command": ["observe_property", id, name]}));
        }
        Ok(p)
    }

    fn send(&self, v: Value) {
        let _ = self.tx.send(v.to_string());
    }

    /// Start playing `url` from `start` seconds.
    pub fn load(&self, url: &str, start: f64) {
        let start = if start >= 1.0 {
            format!("{}", start as u64)
        } else {
            "none".into()
        };
        self.send(json!({"command": ["set_property", "start", start]}));
        self.send(json!({"command": ["loadfile", url, "replace"]}));
        self.set_pause(false);
    }

    pub fn set_pause(&self, paused: bool) {
        self.send(json!({"command": ["set_property", "pause", paused]}));
    }

    pub fn toggle_pause(&self) {
        self.send(json!({"command": ["cycle", "pause"]}));
    }

    pub fn seek_relative(&self, secs: f64) {
        self.send(json!({"command": ["seek", secs, "relative"]}));
    }

    pub fn seek_absolute(&self, secs: f64) {
        self.send(json!({"command": ["seek", secs.max(0.0), "absolute"]}));
    }

    pub fn set_speed(&self, speed: f64) {
        self.send(json!({"command": ["set_property", "speed", speed]}));
    }

    /// Apply Trim Silence and Voice Boost as mpv audio filters.
    pub fn set_effects(&self, trim: TrimMode, boost: bool) {
        self.send(json!({"command": ["set_property", "af", effects_chain(trim, boost)]}));
    }

    pub fn set_volume(&self, percent: f64) {
        self.send(json!({"command": ["set_property", "volume", percent.clamp(0.0, 130.0)]}));
    }

    pub fn stop(&self) {
        self.send(json!({"command": ["stop"]}));
    }

    pub fn quit(&self) {
        self.send(json!({"command": ["quit"]}));
    }
}

/// The mpv `af` value for the chosen effects (empty = no filters).
///
/// Trim Silence cuts the audio into fixed chunks, measures each chunk's loudness and drops
/// the ones below a threshold. Dropping frames keeps the original timestamps, so mpv's
/// position stays in episode time (resume, sync and seeking keep working); it just jumps
/// over the cut silence. Shorter chunks and a higher threshold trim harder. (ffmpeg's
/// `silenceremove` rewrites timestamps, which would make positions drift.)
///
/// Voice Boost runs after the trim so the boosted noise floor doesn't hide silences:
/// a high-pass to drop rumble, then `speechnorm`, which raises quiet speech quickly.
pub fn effects_chain(trim: TrimMode, boost: bool) -> String {
    let mut filters: Vec<String> = Vec::new();
    let trim_params = match trim {
        TrimMode::Off => None,
        // (chunk in samples at ~44.1 kHz, threshold dBFS)
        TrimMode::Mild => Some((22050, -50)),
        TrimMode::Medium => Some((11025, -45)),
        TrimMode::MadMax => Some((4410, -40)),
    };
    if let Some((chunk, db)) = trim_params {
        filters.push(format!("asetnsamples=n={chunk}:p=0"));
        filters.push(
            "astats=metadata=1:reset=1:measure_perchannel=none:measure_overall=RMS_level".into(),
        );
        filters.push(format!(
            "ametadata=mode=select:key=lavfi.astats.Overall.RMS_level:value={db}:function=greater"
        ));
    }
    if boost {
        filters.push("highpass=f=80".into());
        filters.push("speechnorm=e=6:r=0.0001:l=1".into());
    }
    if filters.is_empty() {
        String::new()
    } else {
        format!("lavfi=[{}]", filters.join(","))
    }
}

fn ipc_endpoint() -> String {
    let id = std::process::id();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\pocketatui-mpv-{id}")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir()
            .join(format!("pocketatui-mpv-{id}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

trait Duplex: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Duplex for T {}

/// mpv creates the endpoint a moment after launch, so retry for a few seconds.
async fn connect(endpoint: &str) -> Result<Box<dyn Duplex>> {
    for _ in 0..50 {
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            if let Ok(pipe) = ClientOptions::new().open(endpoint) {
                return Ok(Box::new(pipe));
            }
        }
        #[cfg(unix)]
        {
            if let Ok(sock) = tokio::net::UnixStream::connect(endpoint).await {
                return Ok(Box::new(sock));
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("mpv started but its IPC endpoint never appeared ({endpoint})")
}

async fn writer(
    mut w: impl AsyncWrite + Unpin,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        if w.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        let _ = w.flush().await;
    }
}

async fn reader(r: impl AsyncRead + Unpin, events: UnboundedSender<PlayerEvent>) {
    let mut lines = BufReader::new(r).lines();
    let mut last_pos = f64::NEG_INFINITY;
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        for ev in parse_event(&v) {
            // time-pos arrives many times a second; pass on quarter-second steps and jumps.
            if let PlayerEvent::TimePos(t) = ev {
                if (t - last_pos).abs() < 0.25 {
                    continue;
                }
                last_pos = t;
            }
            if events.send(ev).is_err() {
                return;
            }
        }
    }
    let _ = events.send(PlayerEvent::Exited);
}

/// Translate one mpv IPC message into player events.
pub fn parse_event(v: &Value) -> Vec<PlayerEvent> {
    match v.get("event").and_then(Value::as_str) {
        Some("property-change") => {
            let data = v.get("data");
            let num = data.and_then(Value::as_f64);
            let flag = data.and_then(Value::as_bool);
            let ev = match v.get("name").and_then(Value::as_str) {
                Some("time-pos") => num.map(PlayerEvent::TimePos),
                Some("duration") => num.map(PlayerEvent::Duration),
                Some("pause") => flag.map(PlayerEvent::Paused),
                Some("speed") => num.map(PlayerEvent::Speed),
                Some("paused-for-cache") => flag.map(PlayerEvent::Buffering),
                Some("volume") => num.map(PlayerEvent::Volume),
                _ => None,
            };
            ev.into_iter().collect()
        }
        Some("end-file") => match v.get("reason").and_then(Value::as_str) {
            Some("eof") => vec![PlayerEvent::Finished],
            Some("error") => {
                let why = v
                    .get("file_error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                vec![PlayerEvent::Failed(why.to_string())]
            }
            _ => vec![],
        },
        Some("shutdown") => vec![PlayerEvent::Exited],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_effect_chains() {
        assert_eq!(effects_chain(TrimMode::Off, false), "");
        let mild = effects_chain(TrimMode::Mild, false);
        assert!(mild.starts_with("lavfi=[asetnsamples=n=22050"));
        assert!(mild.contains("value=-50:function=greater]"));
        let both = effects_chain(TrimMode::MadMax, true);
        // trim before boost
        assert!(both.find("ametadata").unwrap() < both.find("speechnorm").unwrap());
        assert_eq!(
            effects_chain(TrimMode::Off, true),
            "lavfi=[highpass=f=80,speechnorm=e=6:r=0.0001:l=1]"
        );
    }

    #[test]
    fn parses_mpv_events() {
        let pc = |name: &str, data: Value| json!({"event":"property-change","id":1,"name":name,"data":data});
        assert_eq!(
            parse_event(&pc("time-pos", json!(12.5))),
            vec![PlayerEvent::TimePos(12.5)]
        );
        assert_eq!(
            parse_event(&pc("pause", json!(true))),
            vec![PlayerEvent::Paused(true)]
        );
        assert!(parse_event(&pc("duration", Value::Null)).is_empty());
        assert_eq!(
            parse_event(&json!({"event":"end-file","reason":"eof"})),
            vec![PlayerEvent::Finished]
        );
        assert!(parse_event(&json!({"event":"end-file","reason":"stop"})).is_empty());
        assert_eq!(
            parse_event(
                &json!({"event":"end-file","reason":"error","file_error":"loading failed"})
            ),
            vec![PlayerEvent::Failed("loading failed".into())]
        );
        assert!(parse_event(&json!({"request_id":0,"error":"success"})).is_empty());
    }
}
