// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Drawing.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table,
    },
};

use image::imageops::FilterType;
use ratatui_image::{Resize, StatefulImage};

use crate::{
    app::{App, Control, Focus, Hit, Pane, SIDEBAR, TranscriptState},
    art::Art,
    config::TrimMode,
    model::{Episode, PlayStatus, fmt_len, fmt_time},
    theme::*,
    transcript::Source,
    whisper,
};

pub fn draw(f: &mut Frame, app: &mut App) {
    app.hits.clear();
    f.render_widget(Block::new().style(base()), f.area());
    let [main, bar, hints] = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(playbar_height(app)),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [side, content] =
        Layout::horizontal([Constraint::Length(24), Constraint::Min(20)]).areas(main);

    sidebar(f, app, side);
    let inner_side = side.inner(Margin::new(1, 1));
    for i in 0..SIDEBAR.len() as u16 {
        if i < inner_side.height {
            let r = Rect {
                y: inner_side.y + i,
                height: 1,
                ..inner_side
            };
            app.hits.push((r, Hit::Sidebar(usize::from(i))));
        }
    }
    if app.show_transcript {
        transcript_view(f, app, content);
    } else if App::is_grid(app.pane) {
        podcasts(f, app, content);
    } else {
        episodes(f, app, content);
    }
    playbar(f, app, bar);
    hint_line(f, app, hints);
    if app.show_help {
        help(f);
        app.hits.push((f.area(), Hit::Help));
    }
}

fn block(title: String, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border(focused))
        .title(Span::styled(format!(" {title} "), self::title(focused)))
}

fn pane_label(app: &App, p: Pane) -> String {
    let n = match p {
        Pane::UpNext => app.up_next.len(),
        Pane::NewReleases => app.new_releases.len(),
        Pane::InProgress => app.in_progress.len(),
        Pane::Podcasts => app.podcasts.len(),
        Pane::PodcastEpisodes => app.podcast_episodes.len(),
        Pane::Search => app.search_results.len(),
    };
    let name = match p {
        Pane::UpNext => "Up Next",
        Pane::NewReleases => "New Releases",
        Pane::InProgress => "In Progress",
        Pane::Podcasts => "Podcasts",
        Pane::PodcastEpisodes => "Episodes",
        Pane::Search => "Search",
    };
    format!("{name} {n}")
}

fn sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let items: Vec<ListItem> = SIDEBAR
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let active =
                app.pane == p || (app.pane == Pane::PodcastEpisodes && app.episodes_from == p);
            let label = pane_label(app, p);
            let (name, count) = label.rsplit_once(' ').unwrap_or((&label, ""));
            let name_style = if active {
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(FG1)
            };
            let loading = if app.loading[p as usize] { " …" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", i + 1), meta()),
                Span::styled(name.to_string(), name_style),
                Span::styled(format!(" {count}{loading}"), meta()),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.sidebar));
    let list = List::new(items)
        .block(block("Pocketatui".into(), focused))
        .highlight_style(selected(focused))
        .highlight_symbol(if focused { "▌" } else { " " });
    f.render_stateful_widget(list, area, &mut state);
}

fn status_cell(e: &Episode, playing: bool) -> Cell<'static> {
    if playing {
        return Cell::from(Span::styled("▶", Style::new().fg(ACCENT).bold()));
    }
    match e.status {
        PlayStatus::Unplayed => Cell::from(Span::styled("●", Style::new().fg(ACCENT))),
        PlayStatus::InProgress => Cell::from(Span::styled("◐", Style::new().fg(ACCENT_2))),
        PlayStatus::Played => Cell::from(Span::styled("✓", meta())),
    }
}

fn length_label(e: &Episode) -> String {
    match e.status {
        PlayStatus::InProgress if e.duration > e.played_up_to && e.played_up_to > 0.0 => {
            format!("{} left", fmt_len(e.duration - e.played_up_to))
        }
        _ => fmt_len(e.duration),
    }
}

fn episodes(f: &mut Frame, app: &mut App, area: Rect) {
    let pane = app.pane;
    let focused = app.focus == Focus::Main;
    let show_podcast = pane != Pane::PodcastEpisodes;
    let playing = app.now.as_ref().map(|n| n.episode.uuid.clone());
    let visible = app.visible(pane);
    let list = app.list(pane);

    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| {
            let e = &list[i];
            let is_playing = playing.as_deref() == Some(e.uuid.as_str());
            let title_style = match (is_playing, e.status) {
                (true, _) => Style::new().fg(ACCENT).bold(),
                (_, PlayStatus::Played) => Style::new().fg(FG4),
                _ => Style::new().fg(FG1),
            };
            let mut cells = vec![
                status_cell(e, is_playing),
                Cell::from(Span::styled(e.title.clone(), title_style)),
            ];
            if show_podcast {
                cells.push(Cell::from(Span::styled(
                    e.podcast_title.clone(),
                    Style::new().fg(ACCENT_2),
                )));
            }
            cells.push(Cell::from(Span::styled(e.date().to_string(), meta())));
            cells.push(Cell::from(
                Line::from(Span::styled(length_label(e), meta())).alignment(Alignment::Right),
            ));
            Row::new(cells)
        })
        .collect();

    let mut widths = vec![Constraint::Length(1), Constraint::Fill(3)];
    if show_podcast {
        widths.push(Constraint::Fill(1));
    }
    widths.extend([Constraint::Length(10), Constraint::Length(11)]);

    let title = match (&app.open_podcast, pane) {
        (Some(p), Pane::PodcastEpisodes) => {
            let follow = if app.is_following(&p.uuid) {
                ""
            } else {
                " · not followed (f to follow)"
            };
            let total = app.podcast_episodes.len();
            let count = if visible.len() == total {
                total.to_string()
            } else {
                format!("{} of {total}", visible.len())
            };
            format!("‹ {} · {count}{follow}", p.title)
        }
        _ => pane_label(app, pane),
    };
    let back_w = title.chars().count() as u16 + 2;
    let title = filter_title(app, title);
    let empty = rows.is_empty();
    let table = Table::new(rows, widths)
        .column_spacing(2)
        .block(block(title, focused))
        .row_highlight_style(selected(focused))
        .highlight_symbol(if focused { "▌" } else { " " });
    f.render_stateful_widget(table, area, &mut app.tables[pane as usize]);
    app.hits
        .push((area.inner(Margin::new(1, 1)), Hit::Table(pane)));
    if pane == Pane::PodcastEpisodes {
        let w = back_w.min(area.width.saturating_sub(2));
        app.hits.push((
            Rect {
                x: area.x + 1,
                y: area.y,
                width: w,
                height: 1,
            },
            Hit::Back,
        ));
        episode_toggles(f, app, area);
    }
    if empty {
        empty_note(f, app, area);
    }
}

/// The transcript of what's playing, following along like a lyrics view.
fn transcript_view(f: &mut Frame, app: &mut App, area: Rect) {
    let pos = app.now.as_ref().map_or(0.0, |n| n.position);
    let source = app.transcript.as_ref().map(|t| t.source);
    let title = match source {
        Some(Source::Whisper) => format!(
            "Transcript · Whisper {} · {}",
            app.cfg.whisper_model,
            whisper::device_label(app.cfg.whisper_device)
        ),
        Some(s) => format!("Transcript · {}", s.label()),
        None => "Transcript".into(),
    };
    let outer = block(title, true);
    let inner = outer.inner(area).inner(Margin::new(1, 0));
    f.render_widget(outer, area);
    app.hits.push((inner, Hit::TranscriptArea));

    // Footer: what's happening, and the keys for this view.
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let mut foot = Vec::new();
    if source == Some(Source::Whisper) {
        let t = app.transcript.as_ref().expect("whisper transcript");
        let upto = t
            .covered_until(pos)
            .map(|e| format!("ready to {}", fmt_time(e)));
        let status = [upto.unwrap_or_default(), app.whisper_status.clone()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let warn = app.whisper_status.starts_with('⚠');
        foot.push(Span::styled(
            format!("{status}   "),
            Style::new().fg(if warn { RED } else { ACCENT_2 }),
        ));
    }
    if let Some(t) = app
        .transcript
        .as_ref()
        .filter(|t| t.source != Source::Whisper)
    {
        let status = if !app.align_status.is_empty() {
            app.align_status.clone()
        } else if t.anchors.is_empty() {
            if whisper::AVAILABLE && app.cfg.transcript_autosync {
                "auto-sync starting…".into()
            } else {
                "not synced · select the line you hear, press s".into()
            }
        } else {
            crate::app::offset_label(t.offset_at(pos))
        };
        foot.push(Span::styled(
            format!("{status}   "),
            Style::new().fg(ACCENT_2),
        ));
    }
    let whisper_own = source == Some(Source::Whisper);
    let keys: &[(&str, &str)] = match (whisper::AVAILABLE, whisper_own) {
        (true, true) if app.can_switch_back() => &[
            ("j/k", "scroll"),
            ("enter", "jump"),
            ("c", "follow"),
            ("w", "own transcript"),
            ("D", "device"),
            ("M", "model"),
            ("T", "close"),
        ],
        (true, true) => &[
            ("j/k", "scroll"),
            ("enter", "jump"),
            ("c", "follow"),
            ("D", "device"),
            ("M", "model"),
            ("T", "close"),
        ],
        (true, false) => &[
            ("j/k", "scroll"),
            ("enter", "jump"),
            ("s", "sync line"),
            ("z", "unsync"),
            ("c", "follow"),
            ("w", "whisper"),
            ("T", "close"),
        ],
        (false, _) => &[
            ("j/k", "scroll"),
            ("enter", "jump"),
            ("s", "sync line"),
            ("z", "unsync"),
            ("c", "follow"),
            ("T", "close"),
        ],
    };
    for (k, d) in keys {
        foot.push(Span::styled(*k, hint_key()));
        foot.push(Span::styled(format!(" {d}  "), meta()));
    }
    f.render_widget(Paragraph::new(Line::from(foot)), footer);

    let message = match (&app.transcript_state, &app.now) {
        (_, None) => Some("Nothing playing".to_string()),
        (TranscriptState::Off, _) => {
            Some("Transcripts are off (transcribe = \"off\" in config.toml)".into())
        }
        (TranscriptState::Loading, _) => Some("Looking for a transcript…".into()),
        (TranscriptState::Missing(why), _) => Some(why.clone()),
        (TranscriptState::Ready, _) if app.transcript_len() == 0 => {
            Some(if source == Some(Source::Whisper) {
                if app.whisper_status.is_empty() {
                    "Starting Whisper…".into()
                } else {
                    app.whisper_status.clone()
                }
            } else {
                "The transcript is empty".into()
            })
        }
        _ => None,
    };
    if let Some(m) = message {
        let mid = Rect {
            y: body.y + body.height / 3,
            height: 1.min(body.height),
            ..body
        };
        f.render_widget(
            Paragraph::new(m).style(meta()).alignment(Alignment::Center),
            mid,
        );
        return;
    }

    let t = app.transcript.as_ref().expect("transcript");
    let current = t.current(pos);
    let focus = if app.transcript_follow {
        current.unwrap_or(0)
    } else {
        app.transcript_sel
    };
    // Wrap every cue to the width, remembering which cue each line belongs to.
    const TS: u16 = 9;
    let text_w = body.width.saturating_sub(TS + 1).max(10) as usize;
    let mut lines: Vec<(usize, bool, String)> = Vec::new();
    let mut focus_line = 0;
    for (i, c) in t.cues.iter().enumerate() {
        if i == focus {
            focus_line = lines.len();
        }
        for (j, l) in wrap(&c.text, text_w).into_iter().enumerate() {
            lines.push((i, j == 0, l));
        }
    }
    let h = body.height as usize;
    let first = focus_line
        .saturating_sub(h / 3)
        .min(lines.len().saturating_sub(h));
    for (row, (i, head, text)) in lines.iter().skip(first).take(h).enumerate() {
        let y = body.y + row as u16;
        let r = Rect {
            y,
            height: 1,
            ..body
        };
        let style = match current {
            Some(c) if *i == c => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            Some(c) if *i < c => Style::new().fg(FG4),
            _ => Style::new().fg(FG1),
        };
        let selected = !app.transcript_follow && *i == app.transcript_sel;
        let bg = if selected {
            Style::new().bg(BG1)
        } else {
            Style::new()
        };
        let marker = if current == Some(*i) { "▌" } else { " " };
        // Shown in your audio's time, so it matches the playbar.
        let ts = if *head {
            fmt_time(t.to_audio(t.cues[*i].start).max(0.0))
        } else {
            String::new()
        };
        let line = Line::from(vec![
            Span::styled(marker, Style::new().fg(ACCENT)),
            Span::styled(format!("{ts:>7} "), meta()),
            Span::styled(text.clone(), style),
        ]);
        f.render_widget(Paragraph::new(line).style(bg), r);
        app.hits.push((r, Hit::TranscriptLine(*i)));
    }
}

/// Word-wrap `s` to `width` columns.
fn wrap(s: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let need = if line.is_empty() {
            word.width()
        } else {
            line.width() + 1 + word.width()
        };
        if need > width && !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// Sort and hide-played toggles on the right of an open podcast's top border.
fn episode_toggles(f: &mut Frame, app: &mut App, area: Rect) {
    let prefs = app.open_prefs();
    let chip = Style::new().fg(FG1).bg(BG1);
    let on = Style::new()
        .fg(ACCENT_2)
        .bg(BG1)
        .add_modifier(Modifier::BOLD);
    let sort = if prefs.oldest_first {
        " ↑ Oldest first "
    } else {
        " ↓ Newest first "
    };
    let played = if prefs.hide_played {
        " ◌ Played hidden "
    } else {
        " ● Played shown "
    };
    let items = [
        (
            sort,
            if prefs.oldest_first { on } else { chip },
            Hit::SortToggle,
        ),
        (
            played,
            if prefs.hide_played { on } else { chip },
            Hit::PlayedToggle,
        ),
    ];
    let total: u16 = items
        .iter()
        .map(|(t, ..)| Line::from(*t).width() as u16 + 1)
        .sum();
    if area.width < total + 30 {
        return;
    }
    let mut x = area.right() - 1 - total;
    for (text, style, hit) in items {
        let w = Line::from(text).width() as u16;
        let r = Rect {
            x,
            y: area.y,
            width: w,
            height: 1,
        };
        f.render_widget(Paragraph::new(text).style(style), r);
        app.hits.push((r, hit));
        x += w + 1;
    }
}

/// A podcast pane (Podcasts or Search): outer block, search box, then the grid.
fn podcasts(f: &mut Frame, app: &mut App, area: Rect) {
    let pane = app.pane;
    let focused = app.focus == Focus::Main;
    let title = if pane == Pane::Search && !app.searched.is_empty() {
        format!(
            "Search · \"{}\" · {}",
            app.searched,
            app.search_results.len()
        )
    } else {
        pane_label(app, pane)
    };
    let outer = block(filter_title(app, title), focused);
    let mut inner = outer.inner(area).inner(Margin::new(1, 0));
    f.render_widget(outer, area);

    if pane == Pane::Search {
        search_box(f, app, Rect { height: 1, ..inner });
        app.hits.push((Rect { height: 1, ..inner }, Hit::SearchBox));
        inner.y += 2;
        inner.height = inner.height.saturating_sub(2);
        if app.search_results.is_empty() {
            let text = if app.loading[pane as usize] {
                "Searching…"
            } else if app.searched.is_empty() {
                "Search the Pocket Casts directory by name, host or topic"
            } else {
                "No podcasts found"
            };
            let mid = Rect {
                y: inner.y + inner.height / 3,
                height: 1.min(inner.height),
                ..inner
            };
            f.render_widget(
                Paragraph::new(text)
                    .style(meta())
                    .alignment(Alignment::Center),
                mid,
            );
            return;
        }
    }
    if !app.art.enabled {
        return podcast_list(f, app, inner, pane);
    }
    grid(f, app, inner, pane, focused);
}

fn search_box(f: &mut Frame, app: &App, area: Rect) {
    let editing = app.search_editing;
    let mut spans = vec![
        Span::styled(
            "Search ",
            Style::new()
                .fg(if editing { ACCENT } else { FG3 })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.search_query.clone(), Style::new().fg(FG0)),
    ];
    if editing {
        spans.push(Span::styled("▏", Style::new().fg(ACCENT)));
    } else if !app.search_query.is_empty() {
        spans.push(Span::styled("   s to edit", meta()));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(BG0_H)),
        area,
    );
}

/// Cover tiles for the podcasts of `pane`, drawn inside `inner`.
fn grid(f: &mut Frame, app: &mut App, inner: Rect, pane: Pane, focused: bool) {
    const GAP: u16 = 1;
    const MIN_TILE: u16 = 18;
    let visible = app.visible(pane);
    if visible.is_empty() || inner.width < 6 || inner.height < 4 {
        empty_note(f, app, inner);
        return;
    }
    let focused = focused && !app.search_editing;

    // Tile size: as many columns of at least MIN_TILE as fit; art square in pixels.
    let cols = ((inner.width + GAP) / (MIN_TILE + GAP)).max(1);
    let tile_w = ((inner.width + GAP) / cols).saturating_sub(GAP).max(4);
    let fs = app.art.font_size();
    let art_w = tile_w - 2;
    let art_h = ((f32::from(art_w) * f32::from(fs.width) / f32::from(fs.height.max(1))).round()
        as u16)
        .clamp(2, inner.height.saturating_sub(3).max(2));
    let tile_h = art_h + 3;
    let rows_vis = usize::from((inner.height / tile_h).max(1));
    let used_w = cols * tile_w + (cols - 1) * GAP;
    let x0 = inner.x + (inner.width.saturating_sub(used_w)) / 2;

    let cols_us = usize::from(cols);
    app.grid_cols = cols_us;
    let sel = app.tables[pane as usize].selected().unwrap_or(0);
    let total_rows = visible.len().div_ceil(cols_us);
    let scroll = &mut app.grid_scroll[pane as usize];
    *scroll = (*scroll).min(total_rows.saturating_sub(rows_vis));
    let sel_row = sel / cols_us;
    if sel_row < *scroll {
        *scroll = sel_row;
    } else if sel_row >= *scroll + rows_vis {
        *scroll = sel_row + 1 - rows_vis;
    }

    let first = *scroll * cols_us;
    for (slot, &i) in visible
        .iter()
        .enumerate()
        .skip(first)
        .take(rows_vis * cols_us)
    {
        let (r, c) = ((slot - first) / cols_us, slot % cols_us);
        let rect = Rect {
            x: x0 + c as u16 * (tile_w + GAP),
            y: inner.y + r as u16 * tile_h,
            width: tile_w,
            height: tile_h,
        };
        let is_sel = slot == sel;
        let border_color = match (is_sel, focused) {
            (true, true) => ACCENT,
            (true, false) => FG4,
            _ => BG2,
        };
        let b = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border_color));
        let tin = b.inner(rect);
        f.render_widget(b, rect);
        app.hits.push((rect, Hit::Tile(pane, slot)));
        let [art_area, title_area] =
            Layout::vertical([Constraint::Length(art_h), Constraint::Length(1)]).areas(tin);
        let p = &app.podcast_list(pane)[i];
        let (uuid, title) = (p.uuid.clone(), p.title.clone());
        // In search results, mark the ones already followed.
        let following = pane == Pane::Search && app.is_following(&uuid);
        cover(f, app, &uuid, &title, art_area);
        let style = if is_sel {
            Style::new()
                .fg(if focused { ACCENT } else { FG0 })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(FG1)
        };
        let width = title_area.width as usize;
        let line = if following {
            Line::from(vec![
                Span::styled("✓ ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(ellipsize(&title, width.saturating_sub(2)), style),
            ])
        } else {
            Line::from(Span::styled(ellipsize(&title, width), style))
        };
        f.render_widget(
            Paragraph::new(line.alignment(Alignment::Center)),
            title_area,
        );
    }

    if total_rows > rows_vis {
        let note = format!(" {}/{} ", sel_row + 1, total_rows);
        let w = note.chars().count() as u16;
        let r = Rect {
            x: (inner.right() + 1).saturating_sub(w + 1),
            y: inner.bottom(),
            width: w,
            height: 1,
        };
        f.render_widget(Paragraph::new(note).style(meta()), r);
    }
}

/// Draw a podcast's cover into `area`, or a placeholder while it loads.
fn cover(f: &mut Frame, app: &mut App, uuid: &str, title: &str, area: Rect) {
    app.art.want(uuid);
    match app.art.get_mut(uuid) {
        Some(Art::Ready { tile, .. }) => {
            let img = StatefulImage::default().resize(Resize::Scale(Some(FilterType::Triangle)));
            f.render_stateful_widget(img, area, tile.as_mut());
        }
        state => placeholder(f, title, area, matches!(state, Some(Art::Loading))),
    }
}

fn placeholder(f: &mut Frame, title: &str, area: Rect, loading: bool) {
    f.render_widget(Block::new().style(Style::new().bg(BG1)), area);
    let initials: String = title
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(char::is_alphanumeric))
        .take(2)
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    let text = if loading { "…".to_string() } else { initials };
    let mid = Rect {
        y: area.y + area.height / 2,
        height: 1.min(area.height),
        ..area
    };
    f.render_widget(
        Paragraph::new(text)
            .style(Style::new().fg(FG4).bg(BG1).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center),
        mid,
    );
}

fn ellipsize(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Plain list of podcasts, used when cover art is off.
fn podcast_list(f: &mut Frame, app: &mut App, area: Rect, pane: Pane) {
    let focused = app.focus == Focus::Main && !app.search_editing;
    let list = app.podcast_list(pane);
    let rows: Vec<Row> = app
        .visible(pane)
        .iter()
        .map(|&i| {
            let p = &list[i];
            let mark = if pane == Pane::Search && app.is_following(&p.uuid) {
                "✓ "
            } else {
                ""
            };
            Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(mark, Style::new().fg(ACCENT)),
                    Span::styled(p.title.clone(), Style::new().fg(FG1)),
                ])),
                Cell::from(Span::styled(p.author.clone(), Style::new().fg(ACCENT_2))),
                Cell::from(Span::styled(
                    p.last_episode_published.get(..10).unwrap_or("").to_string(),
                    meta(),
                )),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Length(10),
        ],
    )
    .column_spacing(2)
    .row_highlight_style(selected(focused))
    .highlight_symbol(if focused { "▌" } else { " " });
    f.render_stateful_widget(table, area, &mut app.tables[pane as usize]);
    app.hits.push((area, Hit::Table(pane)));
    if empty {
        empty_note(f, app, area);
    }
}

fn filter_title(app: &App, title: String) -> String {
    if app.filtering || !app.filter.is_empty() {
        let cursor = if app.filtering { "▏" } else { "" };
        format!("{title} · /{}{cursor}", app.filter)
    } else {
        title
    }
}

fn empty_note(f: &mut Frame, app: &App, area: Rect) {
    let text = if app.loading[app.pane as usize] {
        "Loading…"
    } else if !app.filter.is_empty() {
        "Nothing matches the filter"
    } else if app.pane == Pane::PodcastEpisodes
        && app.open_prefs().hide_played
        && !app.podcast_episodes.is_empty()
    {
        "Every episode is played · x shows them"
    } else if app.pane == Pane::UpNext {
        "Up Next is empty. Press a on any episode to queue it."
    } else {
        "Nothing here"
    };
    let inner = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    f.render_widget(
        Paragraph::new(text)
            .style(meta())
            .alignment(Alignment::Center),
        inner,
    );
}

/// Rows the playbar needs: title, podcast, controls, progress.
fn playbar_height(_app: &App) -> u16 {
    6
}

fn playbar(f: &mut Frame, app: &mut App, area: Rect) {
    let b = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border(false))
        .style(Style::new().bg(BG0_H));
    let mut inner = b.inner(area);
    f.render_widget(b, area);

    // Cover on the left, square in pixels.
    if let Some(n) = &app.now
        && app.art.enabled
        && inner.height >= 3
    {
        let fs = app.art.font_size();
        let w = ((f32::from(inner.height) * f32::from(fs.height) / f32::from(fs.width.max(1)))
            .round() as u16)
            .min(inner.width / 4);
        let art_area = Rect { width: w, ..inner };
        let uuid = n.episode.podcast_uuid.clone();
        let title = n.episode.podcast_title.clone();
        app.art.want(&uuid);
        match app.art.playbar(&uuid) {
            Some(proto) => {
                let img =
                    StatefulImage::default().resize(Resize::Scale(Some(FilterType::Triangle)));
                f.render_stateful_widget(img, art_area, proto);
            }
            None => placeholder(f, &title, art_area, true),
        }
        inner.x += w + 2;
        inner.width = inner.width.saturating_sub(w + 2);
    }
    let [top, sub, controls_row, gauge_row] =
        Layout::vertical([Constraint::Length(1); 4]).areas(inner);

    controls(f, app, controls_row);
    if app.now.is_some() {
        app.hits.push((gauge_row, Hit::Progress));
    }

    // Speed and play state, top right.
    let state = match &app.now {
        None => Span::styled("■ Stopped", meta()),
        Some(n) if n.loading => Span::styled("◌ Loading", Style::new().fg(BLUE)),
        Some(n) if n.buffering => Span::styled("◌ Buffering", Style::new().fg(BLUE)),
        Some(n) if n.paused => Span::styled("❚❚ Paused", Style::new().fg(FG3)),
        Some(_) => Span::styled("▶ Playing", Style::new().fg(ACCENT).bold()),
    };
    let speed_style = Style::new().fg(if (app.speed - 1.0).abs() < 0.01 {
        GRAY
    } else {
        ACCENT_2
    });
    let speed = Span::styled(format!("{:.1}×", app.speed), speed_style);
    let speed_w = speed.width() as u16;
    let right = Line::from(vec![speed, Span::raw("  "), state]).alignment(Alignment::Right);
    let right_w = right.width() as u16 + 1;
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(right_w)]).areas(top);
    f.render_widget(Paragraph::new(right), right_area);
    let speed_rect = Rect {
        x: right_area.x + 1,
        width: speed_w,
        ..right_area
    };
    app.hits.push((speed_rect, Hit::Speed));

    let app = &*app;
    let Some(n) = &app.now else {
        let msg = if app.up_next.is_empty() {
            "Nothing playing"
        } else {
            "Nothing playing · space or ▶ starts Up Next"
        };
        f.render_widget(Paragraph::new(msg).style(meta()), left_area);
        return;
    };
    let title = Span::styled(n.episode.title.clone(), Style::new().fg(FG0).bold());
    let podcast = Span::styled(n.episode.podcast_title.clone(), Style::new().fg(ACCENT_2));
    f.render_widget(Paragraph::new(Line::from(title)), left_area);
    f.render_widget(Paragraph::new(Line::from(podcast)), sub);

    let ratio = if n.duration > 0.0 {
        (n.position / n.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let remaining = (n.duration - n.position).max(0.0) / app.speed.max(0.1);
    let label = if n.duration > 0.0 {
        format!(
            "{} / {}  ·  -{}",
            fmt_time(n.position),
            fmt_time(n.duration),
            fmt_time(remaining)
        )
    } else {
        fmt_time(n.position)
    };
    let gauge = Gauge::default()
        .gauge_style(Style::new().fg(ACCENT).bg(BG1))
        .label(Span::styled(label, Style::new().fg(FG0).bold()))
        .use_unicode(true)
        .ratio(ratio);
    f.render_widget(gauge, gauge_row);
}

/// The playbar's button row: transport, effects, and the volume meter on the right.
fn controls(f: &mut Frame, app: &mut App, row: Rect) {
    let chip = Style::new().fg(FG1).bg(BG1);
    let on = Style::new()
        .fg(ACCENT_2)
        .bg(BG1)
        .add_modifier(Modifier::BOLD);
    let playing = app.now.as_ref().is_some_and(|n| !n.paused && !n.loading);
    let trim = app.cfg.trim_silence;
    let trim_label = if trim == TrimMode::Off {
        " ✂ Trim off ".to_string()
    } else if app.trim_saved >= 1.0 {
        format!(" ✂ {} −{} ", trim.label(), fmt_time(app.trim_saved))
    } else {
        format!(" ✂ {} ", trim.label())
    };
    let play_style = Style::new()
        .fg(BG0_H)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD);
    let boost_style = if app.cfg.voice_boost { on } else { chip };
    let trim_style = if trim == TrimMode::Off { chip } else { on };
    let buttons: Vec<(String, Style, Control)> = vec![
        (
            format!(" « {} ", app.cfg.skip_back),
            chip,
            Control::SkipBack,
        ),
        (
            (if playing { " ❚❚ " } else { " ▶ " }).into(),
            play_style,
            Control::PlayPause,
        ),
        (
            format!(" {} » ", app.cfg.skip_forward),
            chip,
            Control::SkipForward,
        ),
        (" Next › ".into(), chip, Control::Next),
        (trim_label, trim_style, Control::Trim),
        (" ⇡ Boost ".into(), boost_style, Control::Boost),
        (
            " ≡ Transcript ".into(),
            if app.show_transcript { on } else { chip },
            Control::Transcript,
        ),
    ];
    let mut x = row.x;
    for (label, style, ctl) in buttons {
        let w = Line::from(label.as_str()).width() as u16;
        if x + w > row.right() {
            break;
        }
        let r = Rect { x, width: w, ..row };
        f.render_widget(Paragraph::new(label).style(style), r);
        app.hits.push((r, Hit::Control(ctl)));
        x += w + 1;
    }

    // Volume, right-aligned: `Vol − ━━━━━━━━── + 80%`
    const BAR: u16 = 10;
    let pct_label = format!("{:>3}%", app.volume.round() as i64);
    let total = 4 + 3 + BAR + 3 + pct_label.len() as u16;
    if row.right() < x + total {
        return;
    }
    let loud = app.volume > 100.0;
    let mut vx = row.right() - total;
    let put = |f: &mut Frame, x: u16, text: &str, style: Style| -> u16 {
        let w = Line::from(text).width() as u16;
        let r = Rect { x, width: w, ..row };
        f.render_widget(Paragraph::new(text.to_string()).style(style), r);
        w
    };
    vx += put(
        f,
        vx,
        "Vol ",
        Style::new().fg(FG3).add_modifier(Modifier::BOLD),
    );
    app.hits.push((
        Rect {
            x: vx,
            width: 3,
            ..row
        },
        Hit::Control(Control::VolumeDown),
    ));
    vx += put(f, vx, " − ", chip);
    let filled = ((app.volume / 100.0).clamp(0.0, 1.0) * f64::from(BAR)).round() as u16;
    let fill_color = if loud { YELLOW } else { ACCENT };
    put(
        f,
        vx,
        &"━".repeat(usize::from(filled)),
        Style::new().fg(fill_color),
    );
    put(
        f,
        vx + filled,
        &"─".repeat(usize::from(BAR - filled)),
        Style::new().fg(BG2),
    );
    app.hits.push((
        Rect {
            x: vx,
            width: BAR,
            ..row
        },
        Hit::Volume,
    ));
    vx += BAR;
    app.hits.push((
        Rect {
            x: vx,
            width: 3,
            ..row
        },
        Hit::Control(Control::VolumeUp),
    ));
    vx += put(f, vx, " + ", chip);
    let pct_style = Style::new()
        .fg(if loud { YELLOW } else { FG0 })
        .add_modifier(Modifier::BOLD);
    put(f, vx, &pct_label, pct_style);
}

fn hint_line(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, is_err, _)) = &app.status {
        let style = if *is_err {
            Style::new().fg(RED)
        } else {
            Style::new().fg(ACCENT_2)
        };
        f.render_widget(Paragraph::new(format!(" {msg}")).style(style), area);
        return;
    }
    let hints: &[(&str, &str)] = if app.search_editing {
        &[
            ("type", "search"),
            ("enter", "go"),
            ("↓", "results"),
            ("esc", "stop typing"),
        ]
    } else if App::is_grid(app.pane) && app.focus == Focus::Main {
        &[
            ("enter", "open"),
            ("arrows", "move"),
            ("f", "follow / unfollow"),
            ("5", "search"),
            ("space", "pause"),
            ("?", "help"),
            ("q", "quit"),
        ]
    } else if app.filtering {
        &[("type", "filter"), ("enter", "keep"), ("esc", "clear")]
    } else {
        &[
            ("enter", "play"),
            ("space", "pause"),
            ("←/→", "skip"),
            ("[/]", "speed"),
            ("a", "queue"),
            ("n", "next"),
            ("/", "filter"),
            ("?", "help"),
            ("q", "quit"),
        ]
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, what) in hints {
        spans.push(Span::styled(*k, hint_key()));
        spans.push(Span::styled(format!(" {what}   "), meta()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

const HELP: &[(&str, &str)] = &[
    ("Playback", ""),
    ("space / p", "play · pause (starts Up Next if idle)"),
    ("← / ,", "skip back"),
    ("→ / .", "skip forward"),
    ("<", "back to the start"),
    ("[ / ]", "slower / faster (0.1×)"),
    ("=", "speed back to 1.0×"),
    ("- / +", "volume (shown in the playbar; remembered)"),
    ("n", "next in Up Next"),
    ("t", "Trim Silence: off · mild · medium · mad max"),
    ("b", "Voice Boost on / off"),
    ("Episodes", ""),
    ("enter", "play now (moves it to the top of Up Next)"),
    ("a", "add to end of Up Next"),
    ("A", "play next"),
    ("d / del", "remove from Up Next"),
    ("m / u", "mark played / unplayed"),
    ("o / x", "in a podcast: oldest first · hide played"),
    ("T", "transcript of what's playing (click a line to jump)"),
    ("Moving around", ""),
    (
        "1-5",
        "Up Next · New Releases · In Progress · Podcasts · Search",
    ),
    ("← → ↑ ↓", "move between covers (skip with , . there)"),
    ("s  (Search)", "type a search; enter runs it"),
    ("f", "follow / unfollow (press twice) a podcast"),
    ("Mouse", ""),
    ("click", "select · double-click plays / opens"),
    ("right-click", "add episode to Up Next · follow podcast"),
    (
        "wheel",
        "scroll lists; over volume, speed, progress: adjust",
    ),
    ("playbar", "buttons; click or drag the progress bar to seek"),
    ("tab / h / l", "switch between sidebar and list"),
    ("j k ↑ ↓ g G", "move"),
    ("enter / esc", "open / close a podcast"),
    ("/", "filter the list"),
    ("r", "refresh"),
    ("q", "quit (saves your position)"),
];

fn help(f: &mut Frame) {
    let area = f.area();
    let w = 64.min(area.width.saturating_sub(4));
    let h = (HELP.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            if d.is_empty() {
                Line::from(Span::styled(*k, Style::new().fg(ACCENT).bold()))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<13}"), hint_key()),
                    Span::styled(*d, Style::new().fg(FG1)),
                ])
            }
        })
        .collect();
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .style(Style::new().bg(BG1))
            .block(block("Keys · any key closes".into(), true).style(Style::new().bg(BG1))),
        r,
    );
}
