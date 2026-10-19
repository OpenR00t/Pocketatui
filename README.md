# Pocketatui

Pocket Casts in the terminal. A Rust + ratatui player that works with your Pocket Casts
account: Up Next, New Releases, In Progress and your subscriptions, with your listening
position synced back so the phone app picks up where you left off. Search the directory
and follow podcasts; Trim Silence (Mild / Medium / Mad Max) and Voice Boost like the
mobile app.

Styled in Gruvbox Cove (green primary, aqua second, yellow only for warnings). Podcasts
show as a grid of cover art, and the playbar shows the playing podcast's cover. Real
pixels in terminals with sixel, Kitty or iTerm2 graphics (Windows Terminal 1.22+, WezTerm,
Kitty…), coloured half-blocks elsewhere.

## Needs

- **Rust** (stable, 2024 edition).
- **mpv** on `PATH`, which does the actual audio: streaming, seeking, and speed changes
  that keep the pitch. On Windows: `scoop bucket add extras` then `scoop install extras/mpv`, or download a build from
  <https://mpv.io/installation/> and either add it to `PATH` or set `mpv_path` in the config.
- A Pocket Casts account (email + password; Google/Apple sign-in accounts need a password
  set in Pocket Casts first).

## Run

```powershell
cargo run --release              # first run asks for your email and password
cargo install --path .           # puts `pocketatui` in %USERPROFILE%\.cargo\bin
```

```
pocketatui --login     log in again (switch account / fix an expired session)
pocketatui --logout    forget the saved session and keychain password
pocketatui --config    print where config.toml lives
```

The session token is saved in the config folder (`%APPDATA%\pocketatui\config\` on
Windows). If you let it, the password goes in Windows Credential Manager so an expired
token renews itself without asking. `POCKETCASTS_EMAIL` / `POCKETCASTS_PASSWORD` skip the
prompt.

## Keys

| Key | Does |
|---|---|
| `enter` | play now (moves it to the top of Up Next, like the apps); on a podcast, open it |
| `space` / `p` | play / pause; with nothing loaded, starts Up Next |
| `←` `→` (or `,` `.`) | skip back / forward (10 s / 30 s by default) |
| `<` | back to the start |
| `[` `]` / `=` | speed down / up by 0.1×, back to 1.0× |
| `-` `+` | volume (always shown in the playbar; remembered between runs) |
| `n` | next in Up Next |
| `t` | Trim Silence: Off → Mild → Medium → Mad Max |
| `b` | Voice Boost on / off |
| `a` / `A` | add to the end of Up Next / play next |
| `d` / `del` | remove from Up Next |
| `m` / `u` | mark played / unplayed |
| `o` / `x` | in an open podcast: oldest first ↔ newest first · hide / show played (also clickable on the list's top border; remembered per podcast in `podcasts.toml`) |
| `1`–`5` | Up Next · New Releases · In Progress · Podcasts · Search |
| `s` | in Search: type a search (`enter` runs it, `↓` to the results) |
| `f` | follow a podcast (Search, Podcasts or an open podcast); press twice to unfollow |
| `←` `→` `↑` `↓` | in the Podcasts grid: move between covers (skip with `,` `.` there) |
| `tab` / `h` / `l` | sidebar ↔ list |
| `j` `k` `↑` `↓` `g` `G` `PgUp` `PgDn` | move |
| `esc` | close a podcast / clear the filter |
| `/` | filter the current list |
| `r` | refresh |
| `?` | all keys |
| `q` | quit (saves your position first) |

## Mouse

| Where | Does |
|---|---|
| list row / cover tile | click selects, double-click plays (episode) or opens (podcast) |
| right-click | on an episode: add to Up Next · on a podcast: follow (twice to unfollow) |
| wheel | scrolls the list or grid under the pointer; sidebar: changes view |
| sidebar, search box, `‹ Podcast` title | click to switch view, type a search, go back |
| playbar buttons | `« 10` `▶/❚❚` `30 »` `Next ›` `✂ Trim` `⇡ Boost` `− +` |
| progress bar | click or drag to seek; wheel skips |
| volume bar | click to set; wheel ±5 |
| speed (`1.0×`) | wheel ±0.1, click resets to 1.0× |

When an episode finishes it's marked played, removed from Up Next, and the next one starts,
the same as the apps.

## Config

`config.toml` is written with defaults on first run:

```toml
mpv_path = "mpv"      # or a full path to mpv.exe
skip_back = 10
skip_forward = 30
default_speed = 1.0
sync_interval = 30    # seconds between position syncs while playing
volume = 100          # also saved on quit
cover_art = true      # false: plain list, no covers, nothing downloaded
trim_silence = "off"  # off | mild | medium | mad-max   (t in the app saves it here)
voice_boost = false   # (b in the app saves it here)
```

Covers are cached for a week in the cache folder (`%LOCALAPPDATA%\pocketatui\cache\art`).

## Trim Silence and Voice Boost

Both are mpv audio filters (ffmpeg's), applied to everything you play.

- **Trim Silence** splits the audio into short chunks and drops the ones quieter than a
  threshold: Mild drops ½-second chunks under −50 dB, Medium ¼-second chunks under
  −45 dB, Mad Max 0.1-second chunks under −40 dB. Dropping chunks keeps the episode's own
  timestamps, so positions, syncing and seeking stay in real episode time; the playbar
  shows how much has been skipped this session. Pocket Casts doesn't publish its own
  settings, so these are tuned by ear, not copied.
- **Voice Boost** is a high-pass at 80 Hz (rumble) plus ffmpeg's `speechnorm`, which
  lifts quiet speech quickly. It runs after the trim.

## About the API

Pocket Casts has no public API. This uses the private one its web player uses
(`api.pocketcasts.com`), following the endpoints in Music Assistant's Pocket Casts
provider; search uses the apps' `podcast-api.pocketcasts.com/v2/search/combined`. It works today but may change without notice; `src/api.rs` is the only file
that talks to it.

## Layout

| File | What |
|---|---|
| `src/api.rs` | Pocket Casts client: login, token renewal, lists, Up Next, sync |
| `src/model.rs` | Episode / podcast types, lenient parsing of the API's several schemas |
| `src/player.rs` | mpv over JSON IPC (named pipe on Windows, Unix socket elsewhere) |
| `src/app.rs` | state, keys, background calls |
| `src/ui.rs` | drawing |
| `src/art.rs` | cover download, disk cache, terminal image protocol |
| `src/theme.rs` | Gruvbox Cove colors |
| `src/config.rs` | config, saved session, keychain, login prompt |

`POCKETATUI_API_BASE` and `POCKETATUI_PODCAST_API_BASE` point the client at another server
(used for testing against a mock).
