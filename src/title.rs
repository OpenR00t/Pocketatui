// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! The terminal's tab / window title: what's playing, put back as it was on exit.

use std::io::Write;

use crossterm::{execute, terminal::SetTitle};

use crate::app::App;

pub struct TermTitle {
    enabled: bool,
    last: String,
    /// The title before we started (Windows can read it back).
    #[cfg(windows)]
    saved: Option<String>,
}

impl TermTitle {
    pub fn new(enabled: bool) -> Self {
        if enabled {
            // Where supported (xterm, Kitty, WezTerm…), save the current title so it can
            // be restored; other terminals ignore this.
            let _ = std::io::stdout().write_all(b"\x1b[22;0t");
            let _ = std::io::stdout().flush();
        }
        Self {
            enabled,
            last: String::new(),
            #[cfg(windows)]
            saved: enabled.then(console_title).flatten(),
        }
    }

    /// Show what `app` is playing; only writes when it changes.
    pub fn update(&mut self, app: &App) {
        if !self.enabled {
            return;
        }
        let t = title_for(app);
        if t != self.last {
            let _ = execute!(std::io::stdout(), SetTitle(&t));
            self.last = t;
        }
    }

    /// Put the original title back.
    pub fn restore(&self) {
        if !self.enabled {
            return;
        }
        #[cfg(windows)]
        if let Some(t) = &self.saved {
            let _ = execute!(std::io::stdout(), SetTitle(t));
        }
        let _ = std::io::stdout().write_all(b"\x1b[23;0t");
        let _ = std::io::stdout().flush();
    }
}

/// `▶ Episode · Podcast`, `❚❚ …` when paused, or `Pocketatui` with nothing loaded.
pub fn title_for(app: &App) -> String {
    let Some(n) = &app.now else {
        return "Pocketatui".into();
    };
    let state = if n.loading || n.buffering {
        "◌"
    } else if n.paused {
        "❚❚"
    } else {
        "▶"
    };
    clean(&format!(
        "{state} {} · {}",
        n.episode.title, n.episode.podcast_title
    ))
}

/// Feed text goes straight to the terminal: drop control characters so a title can't
/// carry escape sequences.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(windows)]
fn console_title() -> Option<String> {
    use windows_sys::Win32::System::Console::GetConsoleTitleW;
    let mut buf = [0u16; 1024];
    // SAFETY: the buffer and its length match.
    let n = unsafe { GetConsoleTitleW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_characters() {
        assert_eq!(clean("Ep 1\x1b]0;evil\x07 · Show\n"), "Ep 1]0;evil · Show");
    }
}
