# Configuration

pixtuoid keeps its settings in `~/.config/pixtuoid/config.toml`, or in
`$XDG_CONFIG_HOME/pixtuoid/config.toml` when that is an absolute path, and
creates the file on first launch.
Every setting is optional: leave a key out and pixtuoid uses its default. A
command-line flag overrides the file for that run, for example
`pixtuoid run --theme dracula`.

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

## Settings you set

| Key | Default | What it sets |
|-----|---------|--------------|
| `theme` | `"normal"` | The color theme. `pixtuoid run --help` lists the built-in themes, and [Themes](#themes) shows how to pick one live. |
| `max-desks` | auto | The most desks a floor holds, 1 or more. Agents past that open another floor. Unset, the terminal's size decides; `0` is ignored with a warning. It applies to the `run` TUI: `pixtuoid floating` sizes its floors from its window. |
| `look` | `"auto"` | Which office the TUI draws: see [The look](#the-look). |
| `motion` | `"auto"` | How much of the office moves: see [Motion](#motion). |
| `[[pets]]` | every kind, default names | The office's pets: see [Pets](#pets). |

### The look

The TUI draws either the cutaway, a detailed pixel office in terminal
graphics, or the classic office in half-block text cells.

- **`auto`** draws the cutaway where the terminal supports terminal graphics,
  else the classic office. It doesn't ask the terminal over ssh, where every
  repaint would cross the network, or inside GNU screen, which shows the
  question as its
  [hardstatus](https://www.gnu.org/software/screen/manual/html_node/Control-Sequences.html).
- **`cutaway`** always asks.
- **`classic`** starts in the classic office, and asks where `auto` does, for
  the cutaway `t` can switch to.

An unknown `look` value is ignored with a warning. `run --look <value>`
overrides the setting for one run. `run --protocol <value>` draws the cutaway
in that protocol whatever the terminal reports; `pixtuoid run --help` lists
the protocols. Terminal graphics need macOS or Linux, so on Windows `run` and
`doctor` stay classic. `pixtuoid doctor` shows what your terminal supports.

### Motion

How much of the office moves: flickers, twinkles, a pet's roam, lightning,
New Year's fireworks.

- **`full`** moves all of it.
- **`calm`** plays it slower, with fewer repaints, but walks as `full`
  does.
- **`still`** holds it still, for reduced motion: a walk ends where it's going
  at once, and the elevator stays shut.
- **`auto`** is `full`, except `calm` over SIXEL or iTerm2 graphics, inside
  tmux or over ssh. The floating window's `auto` is `full`.

An unknown value is ignored with a warning.

### Pets

Each `[[pets]]` stanza adds one pet:

- `kind` is `"cat"` or `"dog"`, and required. An unknown `kind` is skipped,
  and the other settings still load.
- `name` is optional: the pet's hover-tooltip label, `Office Cat` or
  `Office Dog` when left out.

Leave the section out for every pet, or set `pets = []` for none. Keep it last
in the file, since it's a table section.

## Settings pixtuoid writes

Don't edit these: pixtuoid rewrites them.

| Key | What it holds |
|-----|---------------|
| `last-seen-version` | The last version whose upgrade popup you've seen, so the popup shows once per upgrade. pixtuoid rewrites it when the popup shows, on first launch, or to repair a value it can't parse, not on every launch. |
| `[sources]` | Which hookless CLI, Copilot CLI, is connected (`source-id = true`). Every other CLI is connected when pixtuoid's hooks are in its own config, so it never appears here. |
| `[floating]` | The `pixtuoid floating` window's size, place and zoom: see [The floating window](#the-floating-window). |
| `[audio]` | Whether the office's sound is muted, and its volume: see [Sound](#sound). |

Connecting or disconnecting a source rewrites `[sources]`, whether in the
**Sources panel** (`s`) or with `pixtuoid connect`, `disconnect`,
`sources set` or `setup --yes`. While nothing is connected, the onboarding
wizard offers the CLIs it detects. A disconnected source's characters stay
hidden even while its transcripts are still on disk.

### The floating window

`[floating]` holds the window's `width`, `height`, `x` and `y`, rewritten when
the window closes.

- A size too small for the office to lay out opens at the smallest one that
  does.
- `x` and `y` are dropped when the OS can't report the window's position; the
  next launch lets the OS place it.
- `zoom` is how many density steps the office is zoomed from its automatic
  scale: Cmd/Ctrl `+` and `-` change it, and Cmd/Ctrl `0` resets it. With no
  zoom there is no key. The zoom holds where the office still lays out.
- An `opacity` key from an older release is ignored and left in the file.

### Sound

The office starts muted (`muted = true`): a terminal app never speaks
uninvited.

- **Turning it on.** Press `m` in the TUI or the floating window. The toggle
  is saved in `[audio]`, so the office starts as you left it, and the sound system
  only starts on the first unmute, so muted costs nothing. A ♩ in the TUI's
  footer means you'd hear sound right now.
- **Volume.** `volume` runs from 0.0 to 1.0, clamped, and defaults to 1.0. It
  scales everything on a perceptual curve, so low percents are really quiet,
  and the whole mix sits under your work audio by design. `+`/`-` nudge it live
  and save it; a `♩ N%` readout flashes in the TUI's footer, or in the floating
  window's bottom-right corner. `+` from muted also unmutes.
- **What plays.** An endless run of lofi songs, night songs once the office's
  night, which follows the same sundown as its lights, is half in. Each song darkens in rain
  and plays as busy as **the floor you're viewing**: running agents fill the
  band, and typing density tracks them; other floors are silent until you ride to
  them. Gentle rain plays when the office's weather rains (the weather is the
  same on every floor, outside the windows), and one-shots: a door chime when
  someone walks in, and printer and vending-machine moments. Every
  sound is made in code, with no audio files.
- **Linux.** There is no release archive or npm package for Linux: a binary
  with sound needs `libasound.so.2` when it loads, which neither can bring,
  and cpal can't load it later yet
  ([cpal#1421](https://github.com/RustAudio/cpal/issues/1421)). Homebrew's
  Linux bottle brings `alsa-lib`
  ([formula](https://github.com/Homebrew/homebrew-core/blob/main/Formula/p/pixtuoid.rb)).
  `cargo install` needs `pkg-config` and the ALSA headers, `libasound2-dev`
  on Debian and Ubuntu or `alsa-lib-devel` on Fedora
  ([alsa-sys](https://github.com/diwic/alsa-sys/blob/v0.4.0/build.rs#L7-L11)),
  or `--no-default-features --features portable` for a build without sound.

## Themes

Press `t` in the TUI for a picker that previews every theme live (`j`/`k` or
`↑`/`↓`), and the look where the terminal draws the cutaway (`h`/`l` or
`←`/`→`). Your choice is written to `config.toml`. `--theme <name>` overrides
it for one run; `pixtuoid run --help` lists the built-in themes.

## Logging & troubleshooting

The TUI and the floating window write their diagnostics to a log file, not to
the terminal:

| | |
|-----|-----|
| Where | `~/.local/state/pixtuoid/logs/`, or `$XDG_STATE_HOME/pixtuoid/logs/` when that is an absolute path: one file per run, named for its start |
| Your own file | `$PIXTUOID_LOG=/path/to/file` logs every run to that one file. On Unix a run opens it only if it isn't a symlink and is yours with one link, a character device such as `/dev/null` aside |
| Level | `warn` and above. A stricter `--log-level` records less. With `--log-level debug` or `trace`, or a `$PIXTUOID_LOG` file, the file records at `--log-level`'s level, whose default `pixtuoid --help` shows. A set `$RUST_LOG` replaces the level in every case |
| How long | a run's file is removed a week after its last write, unless the run is still open. A `$PIXTUOID_LOG` file instead rotates to `<name>.old` when a run starts and finds it past its size cap |

A crash writes `crash.log` beside that `logs/` directory, whatever
`$PIXTUOID_LOG` says. Every other command, such as `run --headless` or
`doctor`, logs to stderr.

Warnings about `config.toml`, such as an unknown theme, a bad `[[pets]]` kind
or malformed TOML, print to stderr **before** the office takes over the
screen: scroll back after quitting to read them. If a data source dies while
the office runs, such as the hook listener, the footer keeps a ⚠ warning up
and the full error is in the log file.

### `pixtuoid connect` says it can't locate `pixtuoid-hook`

Connecting a source installs a hook that runs the small `pixtuoid-hook` shim,
so `connect` has to find it: first on `$PATH`, then next to the `pixtuoid`
binary. Every packaged install (Homebrew, npm, the release archives) ships
both, but `cargo install pixtuoid` installs only the main binary, and moving
an install by hand can separate them.

Either install the shim (`cargo install pixtuoid-hook`), or set
**`$PIXTUOID_HOOK`** to its absolute path. That wins over both places, and it
is written into the agent CLI's config, which is why it must be absolute.

### The truecolor check

The office is drawn in 24-bit color. When `pixtuoid run` starts, it **asks
your terminal** whether it supports truecolor, rather than guessing from the
terminal's name: it sets an unlikely 24-bit color and reads it back (a
`DECRQSS` query). If the terminal doesn't confirm, pixtuoid prints a one-line
warning to stderr. It only warns, never blocks, and the warning scrolls away
once the office takes over. `$COLORTERM=truecolor` counts as a yes and skips
the query. `pixtuoid doctor` shows the verdict.

A terminal that is truecolor but doesn't answer the query can still get the
warning. tmux doesn't answer it
([input.c](https://github.com/tmux/tmux/blob/3.5a/input.c#L2310-L2353)), and
sets `$COLORTERM` in its panes only from 3.6
([environ.c](https://github.com/tmux/tmux/blob/3.6/environ.c#L265)), so a
pane in an older tmux can get the warning too. Silence it with `$PIXTUOID_NO_TRUECOLOR_WARN=1` (any
of `1`, `true`, `yes` or `on`).

### When color is disabled (`$NO_COLOR`, `$TERM=dumb`)

The office has no legible black-and-white form, so rather than draw blocks
you can't read, `pixtuoid run` refuses to start the office and says why:

- **`$NO_COLOR`** set to any non-empty value (the
  [no-color.org](https://no-color.org) convention) turns color off. Unset it
  to run the office: `$CLICOLOR_FORCE` and its alias `$FORCE_COLOR` force
  color only when it's unset ([bixense](https://bixense.com/clicolors/)). An
  *empty* `$NO_COLOR` doesn't turn color off, and is ignored.
- **`$TERM=dumb`** means the terminal can't show escape sequences or color at
  all: ncurses' `dumb` entry has no such capability
  ([terminfo.src](https://invisible-island.net/ncurses/terminfo.src.html)).

Use a graphical terminal, or `pixtuoid run --headless` for a plain-text
summary, which works without color. `pixtuoid doctor` shows the color status.
Only the `run` TUI checks this: `run --headless`, `doctor`, `sources` and the
`floating` window are unaffected.
