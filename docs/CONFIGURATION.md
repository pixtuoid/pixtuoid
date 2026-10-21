# Configuration

pixtuoid stores its settings in `~/.config/pixtuoid/config.toml` (respecting
`$XDG_CONFIG_HOME`). The file is created on first launch. **Every user setting is
optional** — omit a key to use its default. CLI flags override the file
(e.g. `pixtuoid run --theme dracula`).

## Example

```toml
theme = "cyberpunk"
max-desks = 8

# One stanza per pet. Omit the whole section to show all pets with default
# names; use `pets = []` to disable all pets. `name` is optional (shown in
# the pet's hover tooltip). Keep [[pets]] last — it's a table section.
[[pets]]
kind = "cat"
name = "Whiskers"   # optional — omit for "Office Cat"

[[pets]]
kind = "dog"        # name omitted → "Office Dog"
```

## User settings (safe to edit)

| Key | Default | Description |
|-----|---------|-------------|
| `theme` | `"normal"` | Color theme — `pixtuoid run --help` lists the built-in themes. |
| `max-desks` | auto | Cap desks per floor (≥ 1; `0` is ignored with a warning). If unset, auto-computed from terminal size. Excess agents overflow to additional floors. Applies to the `run` TUI; `pixtuoid floating` sizes its floors from the window. |
| `look` | `"auto"` | Which office the TUI paints. `auto` draws the cutaway office in terminal graphics (kitty/iTerm2/SIXEL) where the terminal supports them, else the classic half-block office, and doesn't ask the terminal over ssh (every repaint would cross the network) or inside GNU screen (which shows the question in its status line); `cutaway` always asks; `classic` never does. `run --look <value>` overrides it, and `run --protocol <value>` (`kitty`, `sixel` or `iterm2`) draws the cutaway in that protocol whatever the terminal answers; an unknown value is ignored with a warning. Terminal graphics need macOS or Linux; on Windows `run` and `doctor` stay classic. `pixtuoid doctor` shows what your terminal supports. |
| `motion` | `"auto"` | How much of the office's ambient life moves — flickers, twinkles, idle wandering, lightning. `full` moves all of it; `calm` plays it at a quarter of the pace, with a quarter of the repaints; `still` holds it still and flashes no lightning, for reduced motion. Agents still walk where they're going. `auto` is `full`, except `calm` over SIXEL or iTerm2 graphics, inside tmux or over ssh; the floating window's `auto` is `full`. An unknown value is ignored with a warning. |
| `[[pets]]` | all kinds, default names | One stanza per pet. `kind` (`"cat"`/`"dog"`) is required; `name` is optional (the hover-tooltip label, default `Office Cat`/`Office Dog`). Omit the section for all pets; `pets = []` for none; an unknown `kind` is skipped without affecting other settings. Keep it last (it's a table section). |

## System-managed (don't edit — pixtuoid writes these for you)

| Key | Purpose |
|-----|---------|
| `last-seen-version` | Tracks the last version whose upgrade popup you've seen, so the popup only fires once per upgrade. Pixtuoid rewrites it when the popup fires, on first launch, or to repair an unparseable value — not on every launch. |
| `[sources]` | Which hookless CLI (Copilot CLI) is connected (`source-id = true`). Every other CLI is connected when pixtuoid's hooks are installed in its own config, so it never appears here — except an `antigravity = true` an older release wrote, which keeps agy connected until its hooks go in (an interactive launch installs them). Connecting or disconnecting any source in the **Sources panel** (`s`) or via `pixtuoid connect`/`disconnect`/`sources set`/`setup --yes` rewrites the table. While nothing is connected, the onboarding wizard offers the detected CLIs. A disconnected source's characters are hidden even if its transcripts are still present. |
| `[floating]` | Geometry of the `pixtuoid floating` desktop window (`width`/`height`/`x`/`y`), rewritten when the window closes. A size too small for the office to lay out opens at the smallest one that does; `x`/`y` are dropped when the OS can't report the position (the next launch is OS-placed). `zoom` is how many density steps the office is zoomed from its automatic scale (Cmd/Ctrl `+` `-`, and `0` to reset; no zoom writes no key), held where the office still lays out. An `opacity` key from an older release is ignored and left in the file. |
| `[audio]` | Ambient office sound — **starts muted** (`muted = true` is the default; a terminal app never speaks uninvited). Press `m` — in the TUI or the floating window — to turn it on; the toggle persists here, so the office boots exactly as you left it, and the sound system only spins up on the first unmute (muted costs nothing). A ♩ in the TUI footer means you'd hear sound right now. `volume` (0.0–1.0, clamped, default 1.0) scales everything on a perceptual curve — low percents get genuinely quiet, and the whole bus sits under your real work audio by design; `+`/`-` nudge it live (±0.05, persisted, a `♩ N%` readout flashes in the footer — the floating window flashes the same readout in its bottom-right corner; `+` from muted also unmutes). Unmuted: an endless run of lofi songs, night songs once the office's night is half in (the sundown the lights follow), each darkening in rain and playing as busy as **the floor you're viewing** (active agents fill the band; typing density tracks them; other floors are silent until you ride to them), gentle rain when the office weather rains (weather is global — it's outside the windows), plus one-shots — a door chime on walk-ins and printer/vending moments. Every sound is synthesized in code (no audio files). Prebuilt **Linux** binaries ship without audio (ALSA can't link into the static/cross builds); building from source on Linux needs `pkg-config` and the ALSA headers (`pkg-config`, `libasound2-dev`) for the default build, or `--no-default-features --features portable` for one without audio. |

## Themes

Press `t` in the TUI to switch themes with a live preview picker (`j`/`k` or
`↑`/`↓` to navigate); your choice is written back to `config.toml` and persists
across sessions. Override for a single run with `--theme <name>`;
`pixtuoid run --help` lists the built-in themes.

## Logging & troubleshooting

The TUI owns your terminal (alternate screen), so runtime diagnostics go to a
**log file** instead of stderr:

| | |
|-----|-----|
| Default path | `~/.cache/pixtuoid/logs/` (or `$XDG_STATE_HOME/pixtuoid/logs/` if set to an absolute path): one file per run, named for its start |
| Custom path | set `$PIXTUOID_LOG=/path/to/file` to log every run to that one file |
| Level | `warn` and above by default; `--log-level debug` or `trace` (or `$RUST_LOG`) raises it |
| Retention | a run's file is removed a week after its last write, unless its run is still open; `doctor` and the Sources panel read the newest 10 MB across runs; a `$PIXTUOID_LOG` file instead rotates to `<name>.old` past 5 MB at startup |

Warnings about a misconfigured `config.toml` (unknown theme, bad `[[pets]]`
kind, malformed TOML) are also printed to stderr **before** the office takes
over the screen — scroll back after quitting to see them. If a data source
dies mid-run (e.g. the hook listener), the footer shows a persistent ⚠ warning
and the full error is in the log file.

Crashes are reported separately, to `crash.log` in the same directory as the
default log path.

Non-TUI commands (`--headless`, `doctor`, …) log to stderr directly.

### `pixtuoid connect` says it can't locate `pixtuoid-hook`

Connecting a source installs a hook that runs the small `pixtuoid-hook` shim,
so `connect` has to find it: first on `$PATH`, then next to the `pixtuoid`
binary itself. Every packaged install (Homebrew, npm, the release archives)
ships both, but `cargo install pixtuoid` installs only the main binary, and a
hand-relocated install can separate them.

Either install the shim (`cargo install pixtuoid-hook`) or set
**`$PIXTUOID_HOOK`** to an absolute path to it — that override wins over both
lookups and is embedded into the agent CLI's config, so it must be absolute.

### Truecolor preflight

The pixel-art office renders in 24-bit color. On launch, `pixtuoid run`
**asks your terminal** whether it supports truecolor — it sets an unlikely
24-bit color and queries it back (a `DECRQSS` probe) — rather than guessing from
the terminal's name. If the terminal doesn't confirm, it prints a one-line
stderr warning. It's **warn-only** (never blocks) and scrolls away once the
office takes over. (`$COLORTERM=truecolor` is taken as a yes and skips the query;
the query runs only otherwise.) Run `pixtuoid doctor` for the detected
truecolor verdict.

A terminal that's genuinely truecolor but doesn't answer the query (rare) may
still get warned. If you know your terminal is fine, silence the warning with
`$PIXTUOID_NO_TRUECOLOR_WARN=1` (any of `1`/`true`/`yes`/`on`). Note: `tmux`
doesn't implement the `DECRQSS` query, so a truecolor tmux session can trip this
warning — set `$PIXTUOID_NO_TRUECOLOR_WARN=1` (tmux usually advertises
`$COLORTERM`, which skips the query, so most setups never see it).

### When color is disabled (`$NO_COLOR`, `$TERM=dumb`)

The office has **no legible monochrome mode** — it's color end to end. So rather
than render unreadable blocks, `pixtuoid run` refuses to launch the canvas and
explains why when color is turned off:

- **`$NO_COLOR`** (the [no-color.org](https://no-color.org) convention; any
  non-empty value): color output is disabled, so the office can't render. Unset
  `NO_COLOR`, or override per the standard precedence with **`$CLICOLOR_FORCE=1`**
  (forces color on despite `$NO_COLOR`; a `0` value does not force). An *empty*
  `$NO_COLOR` is ignored (it doesn't actually disable color).
- **`$TERM=dumb`**: the terminal can't render escape sequences or color at all.

In both cases use a graphical terminal, or `pixtuoid run --headless` for a plain
text summary (which works fine without color). `pixtuoid doctor` reports the
active color status. This gate applies only to the terminal `run` TUI —
`--headless`, `doctor`, `sources`, and the `floating` window are unaffected.
