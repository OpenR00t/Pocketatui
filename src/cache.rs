// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Keeps the cache folder (transcripts and covers) under `cache_limit_mb`.
//!
//! Runs once at startup, in the background. When the folder is over the limit, the files
//! used longest ago go first until it's back under 90% of it. A transcript counts as used
//! when it's opened (reading one bumps its modified time); a cover when it's downloaded,
//! which happens at least weekly for covers still on screen. Everything here can be
//! fetched or transcribed again, so nothing is lost for good. Whisper models live in the
//! data folder and are never touched.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Mark a cached file as just used so it's the last to be pruned.
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Prune the cache folder in a background thread.
pub fn prune_in_background(limit_mb: u32) {
    if limit_mb == 0 {
        return;
    }
    let Some(dirs) = directories::ProjectDirs::from("", "", "pocketatui") else {
        return;
    };
    let root = dirs.cache_dir().to_path_buf();
    std::thread::spawn(move || {
        prune(&root, u64::from(limit_mb) * 1024 * 1024);
    });
}

/// Delete least recently used files under `root` until it's under 90% of `limit` bytes.
/// Returns the bytes freed.
pub fn prune(root: &Path, limit: u64) -> u64 {
    let mut files = Vec::new();
    collect(root, &mut files);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= limit {
        return 0;
    }
    let target = limit / 10 * 9;
    files.sort_by_key(|f| f.2);
    let mut freed = 0;
    for (path, size, _) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
            freed += size;
        }
    }
    freed
}

fn collect(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            collect(&e.path(), out);
        } else if meta.is_file() {
            let when = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((e.path(), meta.len(), when));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn prunes_oldest_first_to_ninety_percent() {
        let root = std::env::temp_dir().join(format!("pocketatui-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("transcripts")).unwrap();
        std::fs::create_dir_all(root.join("art")).unwrap();
        let base = SystemTime::now() - Duration::from_secs(10_000);
        // Ten 1000-byte files, oldest first: t0 (art), t1 (transcripts), ...
        for i in 0..10u64 {
            let sub = if i % 2 == 0 { "art" } else { "transcripts" };
            let p = root.join(sub).join(format!("f{i}"));
            std::fs::write(&p, vec![0u8; 1000]).unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(base + Duration::from_secs(i * 100)).unwrap();
        }
        // Under the limit: nothing goes.
        assert_eq!(prune(&root, 20_000), 0);
        // Touching the oldest file protects it.
        touch(&root.join("art").join("f0"));
        // 10 000 bytes against a 6000 limit: down to 5400, so five files go.
        assert_eq!(prune(&root, 6000), 5000);
        assert!(root.join("art").join("f0").exists());
        for i in 1..=5 {
            let sub = if i % 2 == 0 { "art" } else { "transcripts" };
            assert!(!root.join(sub).join(format!("f{i}")).exists(), "f{i} kept");
        }
        for i in 6..10 {
            let sub = if i % 2 == 0 { "art" } else { "transcripts" };
            assert!(root.join(sub).join(format!("f{i}")).exists(), "f{i} removed");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
