// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Local transcription with Whisper (whisper.cpp), for episodes without a transcript.
//!
//! A worker thread keeps a stretch of audio ahead of the playback position transcribed:
//! mpv decodes a 30-second chunk to 16 kHz mono WAV (fast, and it reads every format
//! and URL the player can), Whisper turns it into timed segments, and those go back to
//! the app. Nothing runs while the transcript view is closed or once the window ahead is
//! done. Built only with the `whisper` feature (`vulkan` / `cuda` add GPU support).

use tokio::sync::mpsc::UnboundedSender;

use crate::{
    app::Msg,
    config::WhisperDevice,
    transcript::{Cue, Transcript},
};

/// What the worker reports back.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
#[derive(Debug, Clone)]
pub enum Event {
    /// What it's doing right now (downloading the model, transcribing a stretch…).
    Status(String),
    /// Cues for `span` of episode `uuid`.
    Cues {
        uuid: String,
        span: (f64, f64),
        cues: Vec<Cue>,
    },
    /// Reached the end of the episode.
    Finished(String),
    /// A short sample transcribed for lining up an existing transcript: cues in episode
    /// time, for audio starting at `at`.
    Sample {
        uuid: String,
        at: f64,
        cues: Vec<Cue>,
    },
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub model: String,
    pub device: WhisperDevice,
    pub threads: u32,
    pub ahead: u32,
    pub mpv: String,
}

/// True when this build can transcribe at all.
pub const AVAILABLE: bool = cfg!(feature = "whisper");
/// True when this build has a GPU backend.
pub const GPU_BUILT: bool = cfg!(any(feature = "vulkan", feature = "cuda"));

/// Label for where Whisper would run with `device`.
pub fn device_label(device: WhisperDevice) -> &'static str {
    match (device, GPU_BUILT) {
        (WhisperDevice::Cpu, _) => "CPU",
        (_, true) => "GPU",
        (WhisperDevice::Gpu, false) => "CPU (GPU not built in)",
        (WhisperDevice::Auto, false) => "CPU",
    }
}

#[cfg(feature = "whisper")]
pub use imp::Whisper;

#[cfg(not(feature = "whisper"))]
pub use stub::Whisper;

#[cfg(not(feature = "whisper"))]
mod stub {
    use super::*;

    /// Stand-in when built without the `whisper` feature: does nothing.
    pub struct Whisper;

    impl Whisper {
        pub fn new(_tx: UnboundedSender<Msg>, _settings: Settings) -> Self {
            Self
        }
        pub fn start(&self, _uuid: &str, _url: &str, _pos: f64, _done: Option<&Transcript>) {}
        pub fn position(&self, _pos: f64) {}
        pub fn stop(&self) {}
        pub fn configure(&self, _settings: Settings) {}
        pub fn sample(&self, _uuid: &str, _url: &str, _at: f64, _len: f64) {}
    }
}

#[cfg(feature = "whisper")]
mod imp {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{Receiver, RecvTimeoutError, Sender, channel},
        },
        time::{Duration, Instant},
    };

    use whisper_rs::{
        FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    };

    use super::*;
    use crate::{model::fmt_time, transcript};

    /// Seconds of audio per Whisper run.
    const CHUNK: f64 = 30.0;
    /// Segments ending this close to a chunk's cut are redone in the next chunk, so
    /// words aren't split across chunks.
    const CUT_MARGIN: f64 = 1.5;
    const RATE: usize = 16_000;
    const MODEL_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

    enum Cmd {
        Start {
            uuid: String,
            url: String,
            pos: f64,
            covered: Vec<(f64, f64)>,
        },
        Position(f64),
        Stop,
        Configure(Settings),
        Sample {
            uuid: String,
            url: String,
            at: f64,
            len: f64,
        },
    }

    pub struct Whisper {
        tx: Sender<Cmd>,
        /// Interrupts a running chunk when the job changes.
        cancel: Arc<AtomicBool>,
    }

    impl Whisper {
        pub fn new(out: UnboundedSender<Msg>, settings: Settings) -> Self {
            let (tx, rx) = channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let handle = tokio::runtime::Handle::current();
            let c = cancel.clone();
            std::thread::Builder::new()
                .name("whisper".into())
                .spawn(move || Worker::new(rx, out, c, handle, settings).run())
                .expect("start whisper thread");
            Self { tx, cancel }
        }

        /// Transcribe episode `uuid` from `pos` on; `done` holds what's already cached.
        pub fn start(&self, uuid: &str, url: &str, pos: f64, done: Option<&Transcript>) {
            self.cancel.store(true, Ordering::SeqCst);
            let _ = self.tx.send(Cmd::Start {
                uuid: uuid.into(),
                url: url.into(),
                pos,
                covered: done.map(|t| t.covered.clone()).unwrap_or_default(),
            });
        }

        pub fn position(&self, pos: f64) {
            let _ = self.tx.send(Cmd::Position(pos));
        }

        pub fn stop(&self) {
            self.cancel.store(true, Ordering::SeqCst);
            let _ = self.tx.send(Cmd::Stop);
        }

        pub fn configure(&self, settings: Settings) {
            self.cancel.store(true, Ordering::SeqCst);
            let _ = self.tx.send(Cmd::Configure(settings));
        }

        /// Transcribe `len` seconds from `at` once, for aligning another transcript.
        pub fn sample(&self, uuid: &str, url: &str, at: f64, len: f64) {
            let _ = self.tx.send(Cmd::Sample {
                uuid: uuid.into(),
                url: url.into(),
                at,
                len,
            });
        }
    }

    struct Job {
        uuid: String,
        url: String,
        pos: f64,
        covered: Vec<(f64, f64)>,
        /// Where the audio ends, once found.
        end: Option<f64>,
    }

    impl Job {
        /// Start of the next stretch to transcribe, if the window ahead isn't done.
        fn next_start(&self, ahead: f64) -> Option<f64> {
            let mut cur = self.pos.max(0.0);
            while let Some(&(_, b)) = self
                .covered
                .iter()
                .find(|(a, b)| *a <= cur + 0.5 && cur < *b)
            {
                cur = b;
            }
            let past_end = self.end.is_some_and(|e| cur >= e - 0.5);
            (cur < self.pos + ahead && !past_end).then_some(cur)
        }
    }

    struct Loaded {
        key: (String, bool),
        state: WhisperState,
        english_only: bool,
    }

    struct Worker {
        rx: Receiver<Cmd>,
        out: UnboundedSender<Msg>,
        cancel: Arc<AtomicBool>,
        handle: tokio::runtime::Handle,
        settings: Settings,
        job: Option<Job>,
        loaded: Option<Loaded>,
        failed_model: Option<(String, bool)>,
    }

    impl Worker {
        fn new(
            rx: Receiver<Cmd>,
            out: UnboundedSender<Msg>,
            cancel: Arc<AtomicBool>,
            handle: tokio::runtime::Handle,
            settings: Settings,
        ) -> Self {
            Self {
                rx,
                out,
                cancel,
                handle,
                settings,
                job: None,
                loaded: None,
                failed_model: None,
            }
        }

        fn send(&self, ev: Event) {
            let _ = self.out.send(Msg::Whisper(ev));
        }

        fn run(mut self) {
            // whisper.cpp logs to stderr, which would scribble over the TUI.
            whisper_rs::install_logging_hooks();
            lower_priority();
            loop {
                let ahead = f64::from(self.settings.ahead.max(30));
                let next = self.job.as_ref().and_then(|j| j.next_start(ahead));
                // Take every waiting command first; block only when there's no work.
                let cmd = if next.is_some() {
                    self.rx.try_recv().ok()
                } else {
                    match self.rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(c) => Some(c),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                };
                if let Some(cmd) = cmd {
                    self.handle_cmd(cmd);
                    continue;
                }
                if let Some(start) = next {
                    self.chunk(start);
                }
            }
        }

        fn handle_cmd(&mut self, cmd: Cmd) {
            match cmd {
                Cmd::Start {
                    uuid,
                    url,
                    pos,
                    covered,
                } => {
                    self.job = Some(Job {
                        uuid,
                        url,
                        pos,
                        covered,
                        end: None,
                    });
                }
                Cmd::Position(p) => {
                    if let Some(j) = &mut self.job {
                        j.pos = p;
                    }
                }
                Cmd::Stop => self.job = None,
                Cmd::Configure(s) => {
                    if s.model != self.settings.model || s.device != self.settings.device {
                        self.loaded = None;
                        self.failed_model = None;
                    }
                    self.settings = s;
                }
                Cmd::Sample { uuid, url, at, len } => self.sample(uuid, url, at, len),
            }
        }

        /// Load the configured model if it isn't already; false (and reported) on failure.
        fn ensure_model(&mut self) -> bool {
            let key = (self.settings.model.clone(), use_gpu(self.settings.device));
            if self.failed_model.as_ref() == Some(&key) {
                return false;
            }
            if self.loaded.as_ref().map(|l| &l.key) != Some(&key) {
                self.loaded = None;
                match self.load(&key) {
                    Ok(l) => self.loaded = Some(l),
                    Err(e) => {
                        self.failed_model = Some(key);
                        self.send(Event::Error(e));
                        return false;
                    }
                }
            }
            true
        }

        /// One-off transcription of a short stretch, sent back as `Event::Sample`.
        fn sample(&mut self, uuid: String, url: String, at: f64, len: f64) {
            if !self.ensure_model() {
                return;
            }
            let Ok(samples) = decode(&self.settings.mpv, &url, at, len) else {
                return;
            };
            if samples.len() < RATE {
                return;
            }
            self.cancel.store(false, Ordering::SeqCst);
            let threads = threads(self.settings.threads);
            let loaded = self.loaded.as_mut().expect("model loaded");
            if let Ok(Some(segs)) = run_model(loaded, &samples, threads, &self.cancel) {
                let cues = segs
                    .into_iter()
                    .map(|(a, b, text)| Cue {
                        start: at + a,
                        end: at + b,
                        text,
                    })
                    .collect();
                self.send(Event::Sample { uuid, at, cues });
            }
        }

        /// Transcribe one chunk starting at `start` and report it.
        fn chunk(&mut self, start: f64) {
            if !self.ensure_model() {
                self.job = None; // reported; wait for new settings or a new job
                return;
            }
            let Some(job) = &self.job else { return };
            let (uuid, url) = (job.uuid.clone(), job.url.clone());

            let samples = match decode(&self.settings.mpv, &url, start, CHUNK) {
                Ok(s) => s,
                Err(e) => {
                    self.send(Event::Error(format!("couldn't read the audio: {e}")));
                    self.job = None;
                    return;
                }
            };
            let len = samples.len() as f64 / RATE as f64;
            if len < 0.5 {
                if let Some(j) = &mut self.job {
                    j.end = Some(start);
                }
                self.send(Event::Finished(uuid));
                return;
            }
            let last = len < CHUNK - 0.5;
            self.send(Event::Status(format!(
                "transcribing {}–{}",
                fmt_time(start),
                fmt_time(start + len)
            )));

            self.cancel.store(false, Ordering::SeqCst);
            let threads = threads(self.settings.threads);
            let t0 = Instant::now();
            let loaded = self.loaded.as_mut().expect("model loaded");
            let segs = match run_model(loaded, &samples, threads, &self.cancel) {
                Ok(Some(s)) => s,
                Ok(None) => return, // cancelled: the job changed
                Err(e) => {
                    self.send(Event::Error(format!("Whisper failed: {e}")));
                    self.job = None;
                    return;
                }
            };
            let took = t0.elapsed().as_secs_f64();

            let mut cues: Vec<Cue> = segs
                .into_iter()
                .map(|(a, b, text)| Cue {
                    start: start + a,
                    end: start + b,
                    text,
                })
                .collect();
            let mut span_end = start + len;
            if !last {
                let cut = start + len - CUT_MARGIN;
                cues.retain(|c| c.end <= cut);
                span_end = cues.last().map_or(cut, |c| c.end).max(start + 5.0);
            }
            // Stale job (user moved on while this ran)? Still worth keeping for that episode.
            if let Some(j) = self.job.as_mut().filter(|j| j.uuid == uuid) {
                j.covered.push((start, span_end));
                j.covered = transcript::merge_spans(std::mem::take(&mut j.covered));
                if last {
                    j.end = Some(start + len);
                }
            }
            self.send(Event::Cues {
                uuid: uuid.clone(),
                span: (start, span_end),
                cues,
            });
            let speed = len / took.max(0.01);
            self.send(Event::Status(format!("{speed:.0}× real time")));
            if last {
                self.send(Event::Finished(uuid));
            }
        }

        fn load(&self, key: &(String, bool)) -> Result<Loaded, String> {
            let (model, gpu) = key;
            let path = self.model_file(model)?;
            self.send(Event::Status(format!("loading {model}")));
            let mut params = WhisperContextParameters::default();
            params.use_gpu(*gpu);
            params.flash_attn(*gpu);
            let ctx = WhisperContext::new_with_params(&path, params)
                .map_err(|e| format!("couldn't load model {model}: {e}"))?;
            let english_only = !ctx.is_multilingual();
            let state = ctx.create_state().map_err(|e| format!("Whisper: {e}"))?;
            Ok(Loaded {
                key: key.clone(),
                state,
                english_only,
            })
        }

        /// The model file, downloading it first if needed.
        fn model_file(&self, model: &str) -> Result<PathBuf, String> {
            if model.is_empty()
                || !model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            {
                return Err(format!("bad whisper_model name: {model:?}"));
            }
            let dir = models_dir().ok_or("no data folder for models")?;
            let path = dir.join(format!("ggml-{model}.bin"));
            if path.exists() {
                return Ok(path);
            }
            let base = std::env::var("POCKETATUI_MODEL_BASE").unwrap_or_else(|_| MODEL_BASE.into());
            let url = format!("{base}/ggml-{model}.bin");
            let part = dir.join(format!("ggml-{model}.bin.part"));
            let out = self.out.clone();
            let name = model.to_string();
            self.handle
                .block_on(download(&url, &part, move |pct| {
                    let _ = out.send(Msg::Whisper(Event::Status(format!(
                        "downloading model {name} {pct}%"
                    ))));
                }))
                .map_err(|e| format!("couldn't download model {model}: {e}"))?;
            std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
            Ok(path)
        }
    }

    fn use_gpu(device: WhisperDevice) -> bool {
        GPU_BUILT && device != WhisperDevice::Cpu
    }

    fn threads(setting: u32) -> i32 {
        if setting > 0 {
            return setting as i32;
        }
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        (cores / 2).clamp(1, 8) as i32
    }

    /// Abort callback for whisper.cpp: `data` is the worker's `AtomicBool` cancel flag.
    unsafe extern "C" fn abort_requested(data: *mut std::ffi::c_void) -> bool {
        // SAFETY: `data` is `Arc::as_ptr` of the worker's cancel flag, which lives as long
        // as the worker thread (and so as long as the whisper state that calls this).
        let flag = unsafe { &*(data as *const AtomicBool) };
        flag.load(Ordering::SeqCst)
    }

    /// (start, end, text), seconds from the chunk's start.
    type Segment = (f64, f64, String);

    /// Run Whisper on 16 kHz mono samples; `None` if cancelled.
    fn run_model(
        l: &mut Loaded,
        samples: &[f32],
        threads: i32,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<Vec<Segment>>, String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(threads);
        params.set_language(Some(if l.english_only { "en" } else { "auto" }));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_no_context(true);
        // whisper.cpp keeps the abort callback's data pointer inside the state after a run,
        // so it must outlive the state: point it at the worker's long-lived cancel flag
        // (the safe closure version leaves a dangling pointer and every later run fails).
        unsafe {
            params.set_abort_callback(Some(abort_requested));
            params.set_abort_callback_user_data(Arc::as_ptr(cancel) as *mut std::ffi::c_void);
        }
        let r = l.state.full(params, samples);
        if cancel.load(Ordering::SeqCst) {
            return Ok(None);
        }
        r.map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for seg in l.state.as_iter() {
            let text = seg
                .to_str_lossy()
                .map(|t| t.trim().to_string())
                .unwrap_or_default();
            // Skip non-speech markers like [BLANK_AUDIO] or (music).
            let marker = (text.starts_with('[') && text.ends_with(']'))
                || (text.starts_with('(') && text.ends_with(')'));
            if text.is_empty() || marker {
                continue;
            }
            let a = seg.start_timestamp() as f64 / 100.0;
            let b = seg.end_timestamp() as f64 / 100.0;
            out.push((a, b, text));
        }
        Ok(Some(out))
    }

    /// Decode `len` seconds from `start` to 16 kHz mono f32 with mpv.
    fn decode(mpv: &str, url: &str, start: f64, len: f64) -> Result<Vec<f32>, String> {
        let wav = std::env::temp_dir().join(format!("pocketatui-{}-chunk.wav", std::process::id()));
        let _ = std::fs::remove_file(&wav);
        let mut cmd = std::process::Command::new(mpv);
        cmd.args([
            "--no-config",
            "--no-video",
            "--really-quiet",
            "--no-terminal",
            "--ao=pcm",
            "--ao-pcm-waveheader=yes",
            "--audio-samplerate=16000",
            "--audio-channels=mono",
            "--audio-format=s16",
            &format!("--ao-pcm-file={}", wav.display()),
            &format!("--start={start:.2}"),
            &format!("--length={len:.2}"),
            url,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let status = cmd.status().map_err(|e| format!("mpv: {e}"))?;
        let bytes = std::fs::read(&wav).unwrap_or_default();
        let _ = std::fs::remove_file(&wav);
        if bytes.is_empty() && !status.success() {
            return Err(format!("mpv exited with {status}"));
        }
        Ok(wav_samples(&bytes))
    }

    /// 16-bit PCM samples from a WAV file's `data` chunk.
    fn wav_samples(bytes: &[u8]) -> Vec<f32> {
        let Some(i) = bytes.windows(4).position(|w| w == b"data") else {
            return Vec::new();
        };
        bytes
            .get(i + 8..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
            .collect()
    }

    async fn download(
        url: &str,
        part: &std::path::Path,
        progress: impl Fn(u64),
    ) -> anyhow::Result<()> {
        use std::io::Write;
        let http = reqwest::Client::builder()
            .user_agent(concat!("pocketatui/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let mut resp = http.get(url).send().await?.error_for_status()?;
        let total = resp.content_length().unwrap_or(0);
        let mut file = std::fs::File::create(part)?;
        let (mut got, mut shown) = (0u64, u64::MAX);
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk)?;
            got += chunk.len() as u64;
            let pct = (got * 100).checked_div(total).unwrap_or(0);
            if pct != shown {
                shown = pct;
                progress(pct);
            }
        }
        file.flush()?;
        Ok(())
    }

    fn models_dir() -> Option<PathBuf> {
        let d = directories::ProjectDirs::from("", "", "pocketatui")?;
        let p = d.data_local_dir().join("models");
        std::fs::create_dir_all(&p).ok()?;
        Some(p)
    }

    /// Run below normal priority so transcription never competes with the UI or audio.
    fn lower_priority() {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{
                BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
            };
            SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Needs a model: `POCKETATUI_TEST_MODEL=path/ggml-tiny.en.bin POCKETATUI_TEST_AUDIO=speech.mp3`.
        #[test]
        #[ignore]
        fn transcribes_twice() {
            let model = std::env::var("POCKETATUI_TEST_MODEL").unwrap();
            let audio = std::env::var("POCKETATUI_TEST_AUDIO").unwrap();
            whisper_rs::install_logging_hooks();
            let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
                .unwrap();
            let state = ctx.create_state().unwrap();
            let mut l = Loaded {
                key: (String::new(), false),
                state,
                english_only: true,
            };
            let cancel = Arc::new(AtomicBool::new(false));
            for start in [0.0, 27.0, 54.0] {
                let s = decode("mpv", &audio, start, CHUNK).unwrap();
                let r = run_model(&mut l, &s, 2, &cancel);
                eprintln!(
                    "{start}: {} samples -> {:?}",
                    s.len(),
                    r.as_ref().map(|o| o.as_ref().map(|v| v.len()))
                );
                assert!(matches!(r, Ok(Some(_))), "{r:?}");
            }
            // A cancel during a run stops it and reports None.
            let s = decode("mpv", &audio, 0.0, CHUNK).unwrap();
            let c2 = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                c2.store(true, Ordering::SeqCst);
            });
            let t = Instant::now();
            let r = run_model(&mut l, &s, 2, &cancel);
            eprintln!(
                "cancelled after {:?}: {:?}",
                t.elapsed(),
                r.as_ref().map(|o| o.is_none())
            );
            assert!(matches!(r, Ok(None)), "{r:?}");
        }

        #[test]
        fn picks_next_stretch() {
            let mut j = Job {
                uuid: "e".into(),
                url: String::new(),
                pos: 10.0,
                covered: vec![],
                end: None,
            };
            assert_eq!(j.next_start(120.0), Some(10.0));
            j.covered = vec![(10.0, 40.0), (40.0, 70.0)];
            assert_eq!(j.next_start(120.0), Some(70.0));
            j.covered = vec![(0.0, 200.0)];
            assert_eq!(j.next_start(120.0), None);
            j.covered = vec![(0.0, 90.0)];
            j.end = Some(90.0);
            assert_eq!(j.next_start(120.0), None);
        }

        #[test]
        fn reads_wav_data() {
            let mut w = b"RIFF....WAVEfmt ".to_vec();
            w.extend([0u8; 20]);
            w.extend(b"data");
            w.extend(4u32.to_le_bytes());
            w.extend(16384i16.to_le_bytes());
            w.extend((-32768i16).to_le_bytes());
            assert_eq!(wav_samples(&w), vec![0.5, -1.0]);
        }
    }
}
