// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Pocketatui: a terminal player for Pocket Casts.

mod ads;
mod api;
mod app;
mod art;
mod cache;
mod config;
mod model;
mod player;
mod theme;
mod title;
mod transcript;
mod ui;
mod whisper;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc::unbounded_channel;

const USAGE: &str = "\
pocketatui: Pocket Casts in the terminal

USAGE: pocketatui [--login | --logout | --config | --help | --version]

  --login    log in again (switch account or fix an expired session)
  --logout   forget the saved session and keychain password
  --config   print the config file path
Needs mpv on PATH (or mpv_path in the config).";

#[tokio::main]
async fn main() -> Result<()> {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("pocketatui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--config") => {
            println!("{}", config::config_path()?.display());
            return Ok(());
        }
        Some("--logout") => {
            let email = config::load_session().map(|s| s.email);
            config::clear_session(email.as_deref());
            println!("Logged out.");
            return Ok(());
        }
        Some("--login") | None => {}
        Some(other) => {
            eprintln!("unknown option {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }

    let cfg = config::load_config()?;
    cache::prune_in_background(cfg.cache_limit_mb);
    let (session, password) = match config::load_session() {
        Some(s) if arg.as_deref() != Some("--login") => {
            let pw = std::env::var("POCKETCASTS_PASSWORD")
                .ok()
                .or_else(|| config::stored_password(&s.email));
            (s, pw)
        }
        _ => config::interactive_login().await?,
    };

    let email = session.email.clone();
    let creds = password.map(|p| (session.email.clone(), p));
    let client = api::Client::new(session.token, creds, move |token| {
        let _ = config::save_session(&config::Session {
            email: email.clone(),
            token: token.to_string(),
        });
    })?;

    let (ptx, mut prx) = unbounded_channel();
    let player = player::Player::spawn(&cfg.mpv_path, cfg.volume, ptx).await?;

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    // Release the mouse even if we panic (ratatui's own hook restores the rest).
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        hook(info);
    }));
    let mut title = title::TermTitle::new(cfg.terminal_title);
    let picker = art::ArtStore::detect_picker();
    let (tx, mut rx) = unbounded_channel();
    let mut app = app::App::new(cfg, client, player, tx, picker);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    let result: Result<()> = async {
        loop {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            title.update(&app);
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => app.handle_key(k),
                    Some(Ok(Event::Mouse(m))) => app.handle_mouse(m),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(msg) = rx.recv() => app.handle_msg(msg),
                Some(ev) = prx.recv() => app.handle_player(ev),
                _ = tick.tick() => app.tick(),
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    title.restore();
    app.shutdown().await;
    result
}
