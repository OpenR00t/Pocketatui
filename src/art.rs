//! Podcast cover art: fetched from Pocket Casts' image CDN, cached on disk, drawn with
//! ratatui-image (sixel/kitty/iTerm2 where the terminal supports it, half-blocks otherwise).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use image::DynamicImage;
use ratatui_image::{
    FontSize,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};
use tokio::sync::{Semaphore, mpsc::UnboundedSender};

use crate::app::Msg;

const ART_URL: &str = "https://static.pocketcasts.com/discover/images/280";
/// Re-download covers older than this.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

pub enum Art {
    Loading,
    Failed,
    Ready {
        image: DynamicImage,
        /// Protocol for grid tiles (one size, so it encodes once).
        tile: Box<StatefulProtocol>,
    },
}

pub struct ArtStore {
    pub enabled: bool,
    picker: Picker,
    map: HashMap<String, Art>,
    /// The playbar's protocol, kept apart so it doesn't re-encode the tile one.
    playbar: Option<(String, Box<StatefulProtocol>)>,
    http: reqwest::Client,
    limit: Arc<Semaphore>,
    tx: UnboundedSender<Msg>,
    base: String,
}

impl ArtStore {
    pub fn new(enabled: bool, picker: Picker, tx: UnboundedSender<Msg>) -> Self {
        Self {
            enabled,
            picker,
            map: HashMap::new(),
            playbar: None,
            http: crate::api::http_client().unwrap_or_default(),
            limit: Arc::new(Semaphore::new(4)),
            tx,
            base: std::env::var("POCKETATUI_ART_BASE").unwrap_or_else(|_| ART_URL.into()),
        }
    }

    /// Probe the terminal for an image protocol. Call after the terminal is in raw mode.
    pub fn detect_picker() -> Picker {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    }

    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }

    /// Whether real pixels are drawn (as opposed to half-block characters).
    #[allow(dead_code)]
    pub fn full_images(&self) -> bool {
        self.picker.protocol_type() != ProtocolType::Halfblocks
    }

    /// Start loading a podcast's cover if it isn't already.
    pub fn want(&mut self, uuid: &str) {
        if !self.enabled || uuid.is_empty() || self.map.contains_key(uuid) {
            return;
        }
        self.map.insert(uuid.to_string(), Art::Loading);
        let (http, limit, tx, uuid) = (
            self.http.clone(),
            self.limit.clone(),
            self.tx.clone(),
            uuid.to_string(),
        );
        let url = format!("{}/{uuid}.jpg", self.base);
        tokio::spawn(async move {
            let _permit = limit.acquire_owned().await;
            let img = load(&http, &url, &uuid).await;
            let _ = tx.send(Msg::Art(uuid, img));
        });
    }

    pub fn insert(&mut self, uuid: String, image: Option<DynamicImage>) {
        let art = match image {
            Some(image) => Art::Ready {
                tile: Box::new(self.picker.new_resize_protocol(image.clone())),
                image,
            },
            None => Art::Failed,
        };
        self.map.insert(uuid, art);
    }

    pub fn get_mut(&mut self, uuid: &str) -> Option<&mut Art> {
        self.map.get_mut(uuid)
    }

    /// The playbar protocol for `uuid`, built on first use.
    pub fn playbar(&mut self, uuid: &str) -> Option<&mut StatefulProtocol> {
        let stale = self.playbar.as_ref().is_none_or(|(u, _)| u != uuid);
        if stale {
            let Some(Art::Ready { image, .. }) = self.map.get(uuid) else {
                return None;
            };
            let proto = self.picker.new_resize_protocol(image.clone());
            self.playbar = Some((uuid.to_string(), Box::new(proto)));
        }
        self.playbar.as_mut().map(|(_, p)| p.as_mut())
    }
}

fn cache_path(uuid: &str) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "pocketatui")?;
    let dir = dirs.cache_dir().join("art");
    std::fs::create_dir_all(&dir).ok()?;
    // uuids are hex and dashes; keep anything else out of the path.
    let safe: String = uuid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    Some(dir.join(format!("{safe}.jpg")))
}

async fn load(http: &reqwest::Client, url: &str, uuid: &str) -> Option<DynamicImage> {
    let path = cache_path(uuid);
    let fresh = path.as_ref().and_then(|p| {
        let age = SystemTime::now()
            .duration_since(p.metadata().ok()?.modified().ok()?)
            .ok()?;
        (age < MAX_AGE).then(|| std::fs::read(p).ok())?
    });
    let bytes = match fresh {
        Some(b) => b,
        None => {
            let resp = http.get(url).send().await.ok()?;
            if !resp.status().is_success() {
                // Fall back to a stale copy if there is one.
                return decode(path.and_then(|p| std::fs::read(p).ok())?).await;
            }
            let b = resp.bytes().await.ok()?.to_vec();
            if let Some(p) = &path {
                let _ = std::fs::write(p, &b);
            }
            b
        }
    };
    decode(bytes).await
}

async fn decode(bytes: Vec<u8>) -> Option<DynamicImage> {
    tokio::task::spawn_blocking(move || image::load_from_memory(&bytes).ok())
        .await
        .ok()
        .flatten()
}
