# Pocketatui

Pocket Casts in the terminal. A Rust + ratatui player that works with your Pocket Casts
account: Up Next, New Releases, In Progress and your subscriptions, with your listening
position synced back so the phone app picks up where you left off. Search the directory
and follow podcasts; Trim Silence (Mild / Medium / Mad Max) and Voice Boost like the
mobile app; a live transcript of what's playing, from the episode's own transcript or
transcribed on your machine with Whisper; and skipping the ads stitched into your download.

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

Transcribing on your own machine with Whisper is an optional build feature. It compiles
whisper.cpp, so it needs **CMake**, the Visual Studio C++ build tools that Rust on Windows
already uses, and **LLVM** for libclang (`winget install LLVM.LLVM`; set
`LIBCLANG_PATH=C:\Program Files\LLVM\bin` if the build can't find it):

```powershell
cargo run --release --features whisper    # CPU
cargo run --release --features cuda       # + NVIDIA GPU (needs the CUDA toolkit)
cargo run --release --features vulkan     # + any GPU: NVIDIA / AMD / Intel (needs the Vulkan SDK)
cargo install --path . --features cuda    # same choice when installing
```

On Windows, Visual Studio can't build inside folders whose paths go past 260 characters,
and the Vulkan backend nests deep, so give Cargo a short build folder first:

```powershell
$env:CARGO_TARGET_DIR = "C:\pt"   # any short folder; or set it once as a user environment variable
```

Without these the transcript view still shows episodes' own transcripts.

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
| `B` | play the ad that was just skipped |
| `$` | jump to the next likely sponsor read (to check it) |
| `a` / `A` | add to the end of Up Next / play next |
| `d` / `del` | remove from Up Next |
| `m` / `u` | mark played / unplayed |
| `T` | transcript of what's playing (see below) |
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
| transcript line | click to jump there; right-click: "I'm hearing this line" (sync); wheel scrolls |

The terminal's tab title follows what's playing (`▶ Episode · Podcast`, `❚❚` when paused)
and goes back to what it was when you quit. `terminal_title = false` leaves it alone.

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
transcribe = "auto"   # off | auto (episode's own transcript, else Whisper) | whisper (always Whisper)
whisper_model = "small.en"   # tiny.en | base.en | small.en | medium.en | large-v3-turbo-q5_0
whisper_device = "auto"      # auto | cpu | gpu (gpu needs a cuda/vulkan build; else CPU)
whisper_threads = 0          # CPU threads; 0 = half the cores, at most 8
transcribe_ahead = 120       # seconds Whisper keeps transcribed ahead of playback
transcript_autosync = true   # keep episodes' own transcripts in time with your audio (Whisper builds)
skip_ads = true              # skip ads stitched into your download (Whisper builds; see Ads)
sponsor_hints = true         # mark likely host-read sponsor segments in blue (never skipped)
terminal_title = true        # show what's playing in the terminal tab title
cache_limit_mb = 200         # cap for cached transcripts + covers; 0 = no cap
```

Covers (refreshed weekly) and transcripts are cached in `%LOCALAPPDATA%\pocketatui\cache`.
When that folder passes `cache_limit_mb`, the next start deletes what you've used longest
ago until it's back under 90% of the cap. 200 MB holds roughly 1,500 hours of transcripts
alongside the covers. Anything deleted is fetched or transcribed again if you need it.
Whisper models (in `…\pocketatui\data\models`) aren't part of the cap; delete ones you've
stopped using by hand.

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

## Transcripts

`T` (or the `≡ Transcript` button) replaces the list with the transcript of what's playing,
like a lyrics view: the line being spoken is highlighted and kept in view, earlier lines dim.

| Key | In the transcript |
|---|---|
| `j` `k` `↑` `↓` `PgUp` `PgDn` `g` `G` | scroll (stops following) |
| `enter` | jump playback to the highlighted line |
| `c` | follow playback again |
| `s` | the highlighted line is what you're hearing now: sync the transcript to it |
| `z` | clear the sync |
| `w` | switch between the episode's own transcript and Whisper's (cached separately, so nothing is lost) |
| `D` | Whisper device: auto → CPU → GPU (saved to the config) |
| `M` | next Whisper model (saved; downloaded on first use) |
| `T` / `esc` | close |

Where the text comes from, in order:

1. **The publisher's transcript** (WebVTT, SRT or Podcasting 2.0 JSON) when the feed has one.
2. **Pocket Casts' generated transcript**, which many recent episodes have.
3. **Whisper**, on this machine (with a `whisper` build): the model downloads once from
   Hugging Face (`small.en` ≈ 470 MB; to `%LOCALAPPDATA%\pocketatui\data\models`). It
   transcribes 30-second stretches, keeping `transcribe_ahead` seconds ready ahead of where
   you are, only while the transcript view is open (or while it's looking for ads, below). mpv decodes the audio, so any episode
   that plays can be transcribed. Results are cached, so an episode is never done twice.
   It runs at below-normal priority.

**Keeping it in time.** Many podcasts insert ads per download, so your copy of an episode
can be longer or shorter than the one a transcript was made from, and the text drifts
ahead or behind, by a different amount after each ad break. With a Whisper build and
`transcript_autosync` on, Pocketatui transcribes 20 seconds where you are every 30 seconds
of playback (and after a jump), finds those words in the transcript, and shifts its times to
match; the footer shows the result (`auto-synced (text +1:44 ahead)`). Without Whisper, or
when it can't find a match, pick the line you're hearing and press `s` (or right-click it).
Timestamps in the view are always your audio's time.

Transcripts are cached in `%LOCALAPPDATA%\pocketatui\cache\transcripts`, within
`cache_limit_mb` (see Config).

| Model | Download | Notes |
|---|---|---|
| `tiny.en` / `base.en` | 75 / 142 MB | fastest, rough |
| `small.en` | 466 MB | default; good on CPU |
| `medium.en` | 1.5 GB | better; wants a GPU |
| `large-v3-turbo-q5_0` | 574 MB | best; GPU recommended |

## Ads

**Ads stitched into your download** (skipped, with a Whisper build). Many podcasts have an ad
server insert ads into each download, so your copy has audio the episode's own transcript
doesn't. While an episode with its own transcript plays, Whisper transcribes the audio
ahead of you (at least 5 minutes ahead, so it can see past a long ad break) and compares it
with the transcript, four words at a time. Speech that matches nowhere is skipped when:

- it's at least 15 seconds long, and
- the rest of the episode comes later than the transcript says by 10 seconds or more (an ad
  was inserted), or it's 25+ seconds and 40+ words with no match at all (an ad swapped for
  one the same length), or it comes before the show starts or after it ends.

The playbar says `Skipped an ad (1:30) · B plays it`. Skipped stretches show grey on the
progress bar and struck through in Whisper's transcript, and the transcript footer counts
them. The same comparison keeps the episode's transcript in sync on both sides of each ad.
Whisper's text is cached, so going back to an episode finds its ads straight away.

It can only see ads that aren't in the transcript, so it needs an episode transcript
(the publisher's or Pocket Casts') and misses ads read by the hosts. A pre-roll is skipped
a few seconds in, once Whisper has heard the show start. Turn it off with
`skip_ads = false`.

**Sponsor reads by the hosts** (marked, not skipped). These are part of the transcript, so
the comparison can't find them. Instead, phrases like "brought to you by", "promo code" and
"dot com slash" are looked for; a cluster of them is marked blue on the progress bar and in
the transcript, and the playbar says `Sponsor read? 12:30–13:45` as you reach one. `$`
jumps to the next one. The edges are guesses, which is why these are only hints for now.

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
| `src/transcript.rs` | transcript formats, current line, cache |
| `src/ads.rs` | finding inserted ads and sponsor reads |
| `src/cache.rs` | cache size cap (least recently used go first) |
| `src/whisper.rs` | local Whisper worker (`whisper` feature) |
| `src/title.rs` | terminal tab title |
| `src/theme.rs` | Gruvbox Cove colors |
| `src/config.rs` | config, saved session, keychain, login prompt |

`POCKETATUI_API_BASE` and `POCKETATUI_PODCAST_API_BASE` point the client at another server
(used for testing against a mock).

## License

Licensed under the GNU Affero General Public License v3.0 or later. See [LICENSE](LICENSE).
