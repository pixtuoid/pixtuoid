//! Terminal-graphics capability, and the render scale it implies.
//!
//! The cutaway profile needs real pixels — a half-block cell is one buffer
//! pixel, which is the whole reason the classic office is drawn at the density
//! it is. Kitty/iTerm2/SIXEL give a terminal actual pixels, so this module
//! decides whether we HAVE them and, if so, how many a logical office unit is
//! worth.
//!
//! Split the way [`crate::term`] is: the policy is pure and unit-tested, the
//! one IO call (asking the terminal) is a thin wrapper that the tests never
//! reach. A terminal query cannot be a test — it writes escapes to whatever
//! terminal runs the suite and waits on a reply that may never come — and a
//! detection module whose decisions are only exercised through that query is a
//! module with no tests at all.

use pixtuoid_core::sprite::format::Density;
use pixtuoid_scene::layout::Size;
use pixtuoid_scene::render_scale::PixelFit;
use ratatui::layout::Size as TermSize;

/// String Terminator: ends any APC or DCS a cut-short write left open.
pub(crate) const ST: &[u8] = b"\x1b\\";

pub(crate) mod iterm2;
pub(crate) mod kitty;
#[cfg(unix)]
mod probe;
#[cfg(unix)]
pub(crate) mod shm;
pub(crate) mod sixel;
pub(crate) mod tiles;
mod wire;

#[cfg(unix)]
pub(crate) use probe::probe;

/// [`ImageProtocol::image_budget`]'s for kitty: what Ghostty, re-scanning
/// every placement per image, keeps up with at the paint rate.
const KITTY_IMAGE_BUDGET: u32 = 400;

/// A tile's extent in cells; [`ImageProtocol::tile`] is the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileShape {
    /// Cells across.
    pub(crate) cols: u16,
    /// Cells down.
    pub(crate) rows: u16,
}

/// How long each wait of the capability probe may take: the query start to
/// finish.
///
/// The query ends with a device-status request ([`wire::ask`]), so a terminal
/// that answers ends the wait the moment its reply is complete; the budget is
/// only spent where no reply comes (a hidden tmux pane refusing passthrough),
/// which is why it can outlast [`crate::term::TRUECOLOR_PROBE_TIMEOUT`], whose
/// query has no such terminator.
#[cfg(unix)]
pub(crate) const GRAPHICS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// How a kitty image's pixels reach the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Medium {
    /// In the escapes, compressed: any terminal, on any host.
    Direct,
    /// Through a shared-memory object on this host (kitty's `t=s`), which the
    /// terminal answered it reads.
    #[cfg(unix)]
    SharedMemory,
}

/// How the cutaway's images reach the terminal: its protocol, through tmux's
/// passthrough or not, and how kitty's pixels travel. Built only by
/// [`Route::of`] or [`Route::direct`], so no route pairs shared memory with a
/// protocol or tmux `may_share` refuses (Unix-only, so not linked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Route {
    protocol: ImageProtocol,
    tmux: bool,
    medium: Medium,
}

impl Route {
    /// Whether a terminal answering `protocol`, through tmux or not, may read
    /// shared memory at all: kitty's alone, and never through tmux, which may
    /// later attach a client on another host ("Remote clients ... must send
    /// the pixel data directly").
    #[cfg(unix)]
    pub(crate) fn may_share(protocol: Option<ImageProtocol>, tmux: bool) -> bool {
        protocol == Some(ImageProtocol::Kitty) && !tmux
    }

    /// `protocol`, through tmux or not, its pixels through shared memory
    /// where the terminal answered it reads ours (`shm`), forced or not.
    pub(crate) fn of(
        protocol: ImageProtocol,
        tmux: bool,
        #[cfg_attr(
            not(unix),
            expect(unused_variables, reason = "shared memory is Unix-only")
        )]
        shm: bool,
    ) -> Self {
        #[cfg(unix)]
        if shm && Self::may_share(Some(protocol), tmux) {
            return Self {
                protocol,
                tmux,
                medium: Medium::SharedMemory,
            };
        }
        Self::direct(protocol, tmux)
    }

    /// `protocol`, through tmux or not, its pixels in the escapes.
    pub(crate) fn direct(protocol: ImageProtocol, tmux: bool) -> Self {
        Self {
            protocol,
            tmux,
            medium: Medium::Direct,
        }
    }

    pub(crate) fn protocol(self) -> ImageProtocol {
        self.protocol
    }

    /// Inside tmux: the encoder wraps each image in passthrough.
    pub(crate) fn tmux(self) -> bool {
        self.tmux
    }

    pub(crate) fn medium(self) -> Medium {
        self.medium
    }
}

/// A graphics protocol the terminal speaks and the cutaway can be handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ImageProtocol {
    /// kitty's graphics protocol.
    Kitty,
    /// DEC SIXEL.
    Sixel,
    /// iTerm2's inline images.
    Iterm2,
}

impl ImageProtocol {
    /// The protocol's name as a user would search for it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Iterm2 => "iterm2",
        }
    }

    /// The least time between two frames' transmits: kitty's is the event
    /// loop's tick, and SIXEL and iTerm2 encode heavier images less often.
    pub(crate) fn cadence(self) -> std::time::Duration {
        std::time::Duration::from_millis(match self {
            Self::Kitty => 0,
            Self::Sixel => 66,
            Self::Iterm2 => 100,
        })
    }

    /// The most tiles, each its own image, one whole frame may hold, where the
    /// terminal's cost grows with the images on screen; `None` where it
    /// doesn't. Each image added scans those held — Ghostty's placements
    /// (ghostty-org/ghostty `src/terminal/kitty/graphics_storage.zig`,
    /// `addImage` → `removePlacementsByImageId`, `removeOrphans`), kitty's
    /// images (kovidgoyal/kitty `kitty/graphics.c`, `handle_add_command` →
    /// `remove_images`, `img_by_client_id`) — so a frame costs the tiles sent
    /// times the tiles on screen.
    pub(crate) fn image_budget(self) -> Option<u32> {
        match self {
            Self::Kitty => Some(KITTY_IMAGE_BUDGET),
            Self::Sixel | Self::Iterm2 => None,
        }
    }

    /// The cells one re-sent piece of the image covers, before an
    /// [`image_budget`](Self::image_budget) coarsens it.
    pub(crate) fn tile(self) -> TileShape {
        match self {
            Self::Kitty => TileShape { cols: 4, rows: 2 },
            Self::Sixel | Self::Iterm2 => TileShape { cols: 8, rows: 4 },
        }
    }
}

/// A terminal cell's size in real pixels, as the terminal reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellSize {
    /// Cell width in pixels.
    pub(crate) w: u16,
    /// Cell height in pixels.
    pub(crate) h: u16,
}

impl CellSize {
    /// The cell of a `window`, `None` where it reports no pixels.
    pub(crate) fn of_window(window: ratatui::backend::WindowSize) -> Option<Self> {
        let (cells, px) = (window.columns_rows, window.pixels);
        if cells.width == 0 || cells.height == 0 || px.width == 0 || px.height == 0 {
            return None;
        }
        Some(Self {
            w: px.width / cells.width,
            h: px.height / cells.height,
        })
    }
}

/// Which office `run` paints: `--look`, else the `look` config key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum LookMode {
    /// The cutaway where the terminal draws it, else the classic, without
    /// asking the terminal over ssh or inside GNU screen.
    #[default]
    Auto,
    /// The cutaway where the terminal draws it, else the classic, always
    /// asking the terminal.
    Cutaway,
    /// The classic, never asking the terminal.
    Classic,
}

/// The name `--look` and the `look` key take.
impl std::fmt::Display for LookMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match clap::ValueEnum::to_possible_value(self) {
            Some(value) => f.write_str(value.get_name()),
            None => Ok(()),
        }
    }
}

/// What the user asked of the office: a [`LookMode`], and the protocol
/// `--protocol` forces, which implies the cutaway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LookChoice {
    look: LookMode,
    protocol: Option<ImageProtocol>,
}

impl LookChoice {
    /// `look`, the cutaway wherever `protocol` forces one.
    pub(crate) fn new(look: LookMode, protocol: Option<ImageProtocol>) -> Self {
        Self {
            look: if protocol.is_some() {
                LookMode::Cutaway
            } else {
                look
            },
            protocol,
        }
    }

    /// Why this choice paints classic in `env` before the terminal is asked;
    /// `None` where it asks.
    fn unasked(self, env: &TermEnv) -> Option<ClassicReason> {
        match self.look {
            LookMode::Classic => Some(ClassicReason::Disabled),
            LookMode::Cutaway => None,
            // Ours, not an upstream convention: every repaint would cross the
            // network.
            LookMode::Auto if env.ssh => Some(ClassicReason::OverSsh),
            // Screen shows the query as the window's hardstatus ("ESC _ ...
            // Application Program Command (Hardstatus)",
            // https://www.gnu.org/software/screen/manual/html_node/Control-Sequences.html)
            // and answers no protocol.
            LookMode::Auto if env.screen() => Some(ClassicReason::InsideScreen),
            LookMode::Auto => None,
        }
    }

    /// [`Self::unasked`] where this process runs.
    pub(crate) fn unasked_here(self) -> Option<ClassicReason> {
        self.unasked(&TermEnv::read())
    }
}

/// Why a run is painting classic — the honest answer to "why is it not the
/// pretty one?", which a user is entitled to and `doctor` prints.
///
/// One variant per fact the probe can establish: a merged reason makes the
/// report assert something it never established — a verdict on the terminal
/// when the real cause was a pipe, or on the pipe when this build cannot ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassicReason {
    /// [`LookMode::Classic`].
    Disabled,
    /// [`LookMode::Auto`] over ssh: the terminal was not asked.
    OverSsh,
    /// [`LookMode::Auto`] inside GNU screen: the terminal was not asked.
    InsideScreen,
    /// This build has no query to run ([`probe()`]).
    Unsupported,
    /// The terminal was never asked.
    NotQueried,
    /// The terminal was asked and its reply never completed.
    NoAnswer,
    /// The terminal answered, and it has no graphics protocol.
    NoProtocol,
    /// Inside tmux with `allow-passthrough` off: no image, and no query for
    /// one, reaches the terminal (tmux(1), `allow-passthrough`).
    TmuxPassthroughOff,
    /// The terminal has no image protocol the cutaway animates with
    /// ([`Detected::unanimated`]).
    NoCutawayProtocol,
    /// The terminal has a protocol but reports no cell size, so there is no
    /// scale to fit.
    NoCellSize,
    /// Inside tmux, with a protocol other than kitty.
    ///
    /// A SIXEL or iTerm2 image sent through tmux's passthrough bypasses tmux
    /// (tmux(1), `allow-passthrough`): tmux never holds it, so a pane or window
    /// switch redraws the pane without it. kitty's Unicode placeholders are ordinary cells tmux stores
    /// and redraws.
    TmuxNeedsKitty(ImageProtocol),
    /// The terminal has a protocol, but its cell has no [`cutaway_fit`].
    CellTooSmall {
        /// The cell the terminal reported.
        cell: CellSize,
        /// The pack's [`max_density_variant`](pixtuoid_scene::pack::OfficeArt::max_density_variant).
        max_density: Density,
    },
}

/// The resolved decision: what to paint, and — when it went the boring way —
/// why.
///
/// One type rather than a profile beside an `Option<reason>`: a cutaway plan
/// has nothing to explain and a classic plan always does, and two fields could
/// hold both contradictory shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    /// The orthographic cutaway, handed to the terminal as an image.
    Cutaway {
        /// Its geometry on this terminal.
        fit: PixelFit,
        /// How the image reaches the terminal.
        route: Route,
        /// The cell the scale was fitted to.
        cell: CellSize,
        /// What named the protocol.
        chosen: Chosen,
    },
    /// The half-block office: one buffer pixel per half-block.
    Classic {
        /// Why this run is not painting the cutaway.
        reason: ClassicReason,
    },
}

/// What named a cutaway's protocol, as `doctor` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chosen {
    /// `--protocol`, over the terminal.
    Forced,
    /// The terminal's answer to the query.
    Answer,
    /// The terminal's name alone ([`Detected::named`]).
    Name,
}

/// A text variable: `None` when unset, blank or not UTF-8.
fn env_text(name: &str) -> Option<String> {
    pixtuoid_core::platform::text_env(name).filter(|v| !v.trim().is_empty())
}

/// A marker variable, presence only, by [`path_env`](pixtuoid_core::platform::path_env)'s rule.
fn env_set(name: &str) -> bool {
    pixtuoid_core::platform::path_env(name).is_some()
}

/// The terminal's own name and whether this runs inside tmux, read as the
/// probe reads them: what a run's frame pacing is reported under.
pub(crate) fn terminal_and_tmux() -> (Option<String>, bool) {
    let env = TermEnv::read();
    let tmux = env.tmux();
    (env.term_program, tmux)
}

/// The terminal and the link to it as the environment names them: the probe
/// and the motion default read these variables only through here.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TermEnv {
    term: Option<String>,
    term_program: Option<String>,
    /// `$TMUX`: this process is a tmux client.
    tmux_client: bool,
    ssh: bool,
    /// `$STY`, which GNU screen sets in each window it starts
    /// (gnu-mirror-unofficial/screen@58aa0f9 `src/screen.c:1432`).
    screen_window: bool,
}

impl TermEnv {
    fn read() -> Self {
        Self {
            term: env_text("TERM"),
            term_program: env_text("TERM_PROGRAM"),
            tmux_client: env_set("TMUX"),
            // ssh(1) ENVIRONMENT: what a session's sshd sets, SSH_TTY only with a tty.
            ssh: env_set("SSH_CONNECTION") || env_set("SSH_TTY"),
            screen_window: env_set("STY"),
        }
    }

    /// Inside GNU screen, unless inside tmux, which the tmux rules cover.
    fn screen(&self) -> bool {
        self.screen_window && !self.tmux()
    }

    /// Inside tmux: a tmux client (`$TMUX`), or the `TERM`/`TERM_PROGRAM` test
    /// ratatui-image 11.1.0 wraps every image on (`picker.rs:341-347`), which
    /// a tmux `TERM` carried over ssh also passes.
    fn tmux(&self) -> bool {
        self.tmux_client
            || self.term.as_deref().is_some_and(|t| t.starts_with("tmux"))
            || self.term_program.as_deref() == Some("tmux")
    }

    fn link(&self) -> Link {
        Link {
            tmux: self.tmux(),
            ssh: self.ssh,
        }
    }
}

/// What stands between this process and the screen: each hop makes a repaint
/// dearer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Link {
    /// Inside tmux, which redraws every repaint itself.
    pub(crate) tmux: bool,
    /// Over ssh, where every repaint crosses the network.
    pub(crate) ssh: bool,
}

impl Link {
    /// The link this process's environment names.
    pub(crate) fn of_env() -> Self {
        TermEnv::read().link()
    }
}

impl Plan {
    /// The motion this plan affords: every loop on a local kitty image or
    /// half-block grid, a calmer pace where a repaint costs more — SIXEL's or
    /// iTerm2's heavier image, tmux, ssh.
    pub(crate) fn motion(self, link: Link) -> pixtuoid_scene::anim::Motion {
        use pixtuoid_scene::anim::Motion;
        let heavy = matches!(
            self,
            Plan::Cutaway {
                route: Route {
                    protocol: ImageProtocol::Sixel | ImageProtocol::Iterm2,
                    ..
                },
                ..
            }
        );
        if heavy || link.tmux || link.ssh {
            Motion::Calm
        } else {
            Motion::Full
        }
    }
}

/// What the probe learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Detected {
    /// The graphics protocol the terminal speaks, `None` when it has none we
    /// can drive.
    pub(crate) protocol: Option<ImageProtocol>,
    /// The cell size the terminal reports, `None` when it reports none.
    pub(crate) cell: Option<CellSize>,
    /// Whether this process runs inside tmux — from the environment, not the
    /// terminal's answer.
    pub(crate) tmux: bool,
    /// The terminal has no image protocol the cutaway animates with, so
    /// `protocol` is `None`.
    pub(crate) unanimated: bool,
    /// `protocol` rests on the terminal's name alone, not its answer.
    pub(crate) named: bool,
    /// Whether the terminal answered that it reads kitty images from this
    /// host's shared memory.
    pub(crate) shm: bool,
}

/// The outcome of asking the terminal — [`resolve`]'s input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    /// What the terminal's reply and the environment established — the
    /// environment alone where the reply never completed but the environment
    /// names a protocol (see [`probe()`]).
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(dead_code, reason = "only the Unix graphics probe returns it")
    )]
    Answered(Detected),
    /// The terminal was never asked: the caller said not to, or no controlling
    /// terminal took the query.
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(dead_code, reason = "only the Unix graphics probe returns it")
    )]
    NotQueried,
    /// The terminal was asked, its reply never completed, and the environment
    /// names no protocol to fall back on.
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(dead_code, reason = "only the Unix graphics probe returns it")
    )]
    NoAnswer,
    /// Inside tmux with `allow-passthrough` off, so nothing was asked.
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(dead_code, reason = "only the Unix graphics probe returns it")
    )]
    TmuxPassthroughOff,
    /// This build cannot ask ([`probe()`]).
    #[cfg_attr(
        all(unix, not(test)),
        expect(dead_code, reason = "only the non-Unix probe returns it")
    )]
    Unsupported,
}

/// The natural scale: what the CELL alone makes a logical unit, before the
/// pack gets a say.
///
/// Classic paints ONE buffer pixel per half-block, so a logical unit is one
/// cell wide and half a cell tall. Drawing the SAME logical office into real
/// pixels makes a logical unit `cell.w` px wide and `cell.h / 2` px tall —
/// equal exactly when the cell is the ~1:2 the half-block technique already
/// assumes. `RenderScale` is isotropic, so one axis has to give; the SMALLER
/// wins so neither axis shows less office than classic does.
fn raw_scale_for_cell(cell: CellSize) -> u16 {
    cell.w.min(cell.h / 2)
}

/// The cutaway's geometry on one terminal: `cell`'s natural scale fitted to
/// `max_density` (the pack's
/// [`max_density_variant`](pixtuoid_scene::pack::OfficeArt::max_density_variant)),
/// over an image `area` cells big. `None` when no multiple of it lies within
/// the fit's bound, or the scale is 1.
pub(crate) fn cutaway_fit(
    cell: CellSize,
    area: TermSize,
    max_density: Density,
) -> Option<PixelFit> {
    PixelFit::new(raw_scale_for_cell(cell), max_density, image_px(cell, area))
        // Scale 1 IS the classic density: an encode per frame that draws the
        // identical picture.
        .filter(|fit| fit.scale().get() > 1)
}

/// The image's pixels over `area` cells: the cells', never a window size that
/// counts the terminal's padding; past what a buffer can address, the office
/// stops growing.
fn image_px(cell: CellSize, area: TermSize) -> Size {
    let px = |cells: u16, cell_px: u16| {
        u16::try_from(u32::from(cells) * u32::from(cell_px)).unwrap_or(u16::MAX)
    };
    Size {
        w: px(area.width, cell.w),
        h: px(area.height, cell.h),
    }
}

/// Decide what to paint from the terminal's answer. Pure — [`probe()`]
/// supplies the probe, `forced` is `--protocol`, `max_density` is the pack's
/// [`max_density_variant`](pixtuoid_scene::pack::OfficeArt::max_density_variant),
/// and `area` is the image's extent in cells.
pub(crate) fn resolve(
    forced: Option<ImageProtocol>,
    probe: Probe,
    max_density: Density,
    area: TermSize,
) -> Plan {
    let classic = |reason| Plan::Classic { reason };
    let d = match probe {
        Probe::Answered(d) => d,
        Probe::NotQueried => return classic(ClassicReason::NotQueried),
        Probe::NoAnswer => return classic(ClassicReason::NoAnswer),
        Probe::TmuxPassthroughOff => return classic(ClassicReason::TmuxPassthroughOff),
        Probe::Unsupported => return classic(ClassicReason::Unsupported),
    };
    let Some(protocol) = forced.or(d.protocol) else {
        return classic(if d.unanimated {
            ClassicReason::NoCutawayProtocol
        } else {
            ClassicReason::NoProtocol
        });
    };
    let Some(cell) = d.cell else {
        return classic(ClassicReason::NoCellSize);
    };
    // The cell before tmux: a user told to switch to kitty should not then
    // find the cell was too small all along.
    let Some(fit) = cutaway_fit(cell, area, max_density) else {
        return classic(ClassicReason::CellTooSmall { cell, max_density });
    };
    if d.tmux && protocol != ImageProtocol::Kitty {
        return classic(ClassicReason::TmuxNeedsKitty(protocol));
    }
    Plan::Cutaway {
        fit,
        route: Route::of(protocol, d.tmux, d.shm),
        cell,
        chosen: if forced.is_some() {
            Chosen::Forced
        } else if d.named {
            Chosen::Name
        } else {
            Chosen::Answer
        },
    }
}

impl ClassicReason {
    /// One line for `doctor`, explaining the fallback.
    pub(crate) fn describe(self) -> String {
        match self {
            Self::Disabled => {
                let classic = LookMode::Classic;
                format!("chosen by `--look {classic}` or `look = \"{classic}\"`")
            }
            Self::OverSsh => format!(
                "over ssh, where every repaint crosses the network — `--look {}` asks the \
                 terminal anyway",
                LookMode::Cutaway
            ),
            Self::InsideScreen => format!(
                "inside GNU screen, which shows the graphics query in its status line and \
                 answers no protocol — `--look {}` asks anyway",
                LookMode::Cutaway
            ),
            Self::Unsupported => {
                "this build cannot ask the terminal for graphics (not on Unix)".to_string()
            }
            Self::NotQueried => "the terminal was not asked (stdout is not a terminal, there \
                 is no controlling terminal, or $TERM is dumb) — run in an interactive \
                 terminal to see what it supports"
                .to_string(),
            Self::NoAnswer => "the terminal did not answer the capability query".to_string(),
            Self::NoProtocol => {
                "terminal reports no graphics protocol (kitty/iterm2/sixel)".to_string()
            }
            Self::TmuxPassthroughOff => "inside tmux with allow-passthrough off — \
                 `set -g allow-passthrough on` lets kitty graphics through"
                .to_string(),
            Self::NoCutawayProtocol => {
                let names: Vec<&str> = <ImageProtocol as clap::ValueEnum>::value_variants()
                    .iter()
                    .map(|p| p.name())
                    .collect();
                format!(
                    "this terminal has no image protocol the cutaway animates with — \
                     `--protocol {}` forces one",
                    names.join("|")
                )
            }
            Self::NoCellSize => "terminal reports no cell size in pixels".to_string(),
            Self::TmuxNeedsKitty(p) => format!(
                "inside tmux only kitty graphics survive a pane switch here, and this terminal \
                 speaks {}",
                p.name()
            ),
            Self::CellTooSmall { cell, max_density }
                if max_density > Density::ONE && raw_scale_for_cell(cell) > 1 =>
            {
                format!(
                    "terminal reports a {}x{} cell — too small for the pack's {max_density}x art",
                    cell.w, cell.h
                )
            }
            Self::CellTooSmall { cell, .. } => format!(
                "terminal reports a {}x{} cell — too small to subdivide",
                cell.w, cell.h
            ),
        }
    }
}

/// The plan for the terminal this process runs in: the one call `run` and
/// `doctor` both make, probe included, so `doctor` prints the plan `run`
/// carries. `ask` is whether the terminal may be asked.
pub(crate) fn plan_this_terminal(choice: LookChoice, max_density: Density, ask: bool) -> Plan {
    detect(
        choice,
        &TermEnv::read(),
        max_density,
        terminal_cells(),
        || probe(ask),
    )
}

/// The plan for a terminal `term` cells big in `env`. `ask` is the terminal
/// query, called only where [`LookChoice::unasked`] lets it.
fn detect(
    choice: LookChoice,
    env: &TermEnv,
    max_density: Density,
    term: TermSize,
    ask: impl FnOnce() -> Probe,
) -> Plan {
    if let Some(reason) = choice.unasked(env) {
        return Plan::Classic { reason };
    }
    resolve(choice.protocol, ask(), max_density, image_area(term))
}

/// The cells the image covers: the scene, never the footer, since text and
/// image never share a cell.
fn image_area(term: TermSize) -> TermSize {
    let scene =
        crate::tui::renderer::scene_rect(ratatui::layout::Rect::new(0, 0, term.width, term.height));
    TermSize {
        width: scene.width,
        height: scene.height,
    }
}

/// The terminal's size in cells; empty when there is none to measure, where the
/// graphics probe is not asked either.
pub(crate) fn terminal_cells() -> TermSize {
    crossterm::terminal::size()
        .map(|(width, height)| TermSize { width, height })
        .unwrap_or_default()
}

/// Off Unix, where `term::query_tty` (termios) has no counterpart, nothing
/// asks.
#[cfg(not(unix))]
pub(crate) fn probe(_ask: bool) -> Probe {
    Probe::Unsupported
}

impl Plan {
    /// The office's logical extent on a terminal `term` cells big: what the
    /// plan's painter lays out there.
    pub(crate) fn office_extent(self, term: TermSize) -> Size {
        match self {
            Plan::Cutaway { fit, cell, .. } => fit.over(image_px(cell, image_area(term))).logical(),
            Plan::Classic { .. } => {
                let (w, h) = crate::tui::renderer::scene_buf_size(term.width, term.height);
                Size { w, h }
            }
        }
    }

    /// The plan as `doctor`'s `graphics:` line: for a cutaway, everything a
    /// tester reports back from a terminal, and why `run`'s own choice paints
    /// classic unasked (`run_unasked`); for classic, why it fell back.
    pub(crate) fn diagnostic_row(self, run_unasked: Option<ClassicReason>) -> String {
        match self {
            Plan::Cutaway {
                fit,
                route,
                cell,
                chosen,
            } => {
                let (protocol, tmux, medium) = (route.protocol(), route.tmux(), route.medium());
                let shape = protocol.tile();
                let budget = protocol
                    .image_budget()
                    .map(|b| format!(" (doubled until a frame is at most {b} tiles)"))
                    .unwrap_or_default();
                let cadence = match protocol.cadence().as_millis() {
                    0 => "every frame".to_string(),
                    ms => format!("at most every {ms} ms"),
                };
                let why = run_unasked
                    .map(|why| format!(" — `run` paints classic here: {}", why.describe()))
                    .unwrap_or_default();
                format!(
                    "graphics: {} ({}) on a {}x{} cell, {} — the cutaway at {}x \
                     ({}x art upscaled {}x), a {}x{} office, sent as {}x{}-cell tiles{budget} \
                     {cadence}{why}",
                    protocol.name(),
                    match chosen {
                        Chosen::Forced => "forced by --protocol",
                        Chosen::Answer => "the terminal's answer",
                        Chosen::Name => "guessed from the terminal's name",
                    },
                    cell.w,
                    cell.h,
                    match (tmux, medium) {
                        (true, _) => "through tmux passthrough",
                        (false, Medium::Direct) => "direct",
                        #[cfg(unix)]
                        (false, Medium::SharedMemory) => "direct, pixels through shared memory",
                    },
                    fit.scale().get(),
                    fit.density(),
                    fit.upscale(),
                    fit.logical().w,
                    fit.logical().h,
                    shape.cols,
                    shape.rows,
                )
            }
            Plan::Classic { reason } => {
                format!("graphics: classic half-blocks — {}", reason.describe())
            }
        }
    }
}

/// What the terminal unwind writes before it leaves the alt screen: our
/// images' delete, once any reached the terminal.
pub(crate) fn unwind_prelude() -> Vec<u8> {
    [
        kitty::unwind(),
        grid_unwind(IN_GRID.load(std::sync::atomic::Ordering::Relaxed)),
    ]
    .concat()
}

/// Whether this process has drawn SIXEL or iTerm2 pixels, which sit in the
/// text grid rather than in an image store kitty-style deletes reach: read by
/// an unwind that may run from the panic hook.
pub(crate) static IN_GRID: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The unwind's part for pixels in the grid: nothing unless this process
/// `drew` some; then an ST that ends an image a failed write cut short, and
/// ED 2. 1049 clears the alternate screen on the way IN (ctlseqs, "Use
/// Alternate Screen Buffer ... clearing it first"), so nothing promises its
/// pixels go on the way out.
pub(crate) fn grid_unwind(drew: bool) -> Vec<u8> {
    if drew {
        [ST, b"\x1b[2J"].concat()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_unwind_ends_any_image_then_erases_the_display() {
        assert_eq!(grid_unwind(false), b"");
        assert_eq!(grid_unwind(true), b"\x1b\\\x1b[2J");
    }

    const CELL_8X16: CellSize = CellSize { w: 8, h: 16 };
    /// A pack with no density variants.
    const BASE_ONLY: Density = Density::ONE;
    /// The bundled pack's densest art (`bundled_is_the_bundled_packs_max_density`).
    const BUNDLED: Density = Density::new(4).expect("nonzero");
    const AREA: TermSize = TermSize {
        width: 120,
        height: 40,
    };

    /// Each terminal kind's default motion: every loop on a local kitty image or
    /// half-block grid, the calm pace for SIXEL, iTerm2, tmux or ssh.
    #[test]
    fn each_terminal_kind_paints_at_its_affordable_motion() {
        use pixtuoid_scene::anim::Motion;
        let plan = |protocol| {
            let probe = answered(Some(protocol), CELL_8X16, false);
            resolve(None, probe, BUNDLED, AREA)
        };
        let classic = Plan::Classic {
            reason: ClassicReason::Disabled,
        };
        let local = Link::default();
        let tmux = Link {
            tmux: true,
            ..local
        };
        let ssh = Link { ssh: true, ..local };
        assert!(matches!(plan(ImageProtocol::Kitty), Plan::Cutaway { .. }));
        assert_eq!(plan(ImageProtocol::Kitty).motion(local), Motion::Full);
        assert_eq!(classic.motion(local), Motion::Full);
        for heavy in [ImageProtocol::Sixel, ImageProtocol::Iterm2] {
            assert_eq!(plan(heavy).motion(local), Motion::Calm, "{heavy:?}");
        }
        for link in [tmux, ssh] {
            assert_eq!(
                plan(ImageProtocol::Kitty).motion(link),
                Motion::Calm,
                "{link:?}"
            );
            assert_eq!(classic.motion(link), Motion::Calm, "{link:?}");
        }
    }

    #[test]
    fn the_environment_names_the_link() {
        const VARS: [&str; 5] = ["TERM", "TERM_PROGRAM", "TMUX", "SSH_CONNECTION", "SSH_TTY"];
        let link = |set: &[(&str, &str)]| {
            let vars = VARS.map(|name| {
                let value = set.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
                (name, value)
            });
            temp_env::with_vars(vars, Link::of_env)
        };
        let tmux = Link {
            tmux: true,
            ..Link::default()
        };
        let ssh = Link {
            ssh: true,
            ..Link::default()
        };
        assert_eq!(link(&[("TERM", "xterm-ghostty")]), Link::default());
        for blank in ["", " \t"] {
            assert_eq!(
                link(&[("TMUX", blank)]),
                Link::default(),
                "{blank:?} is unset"
            );
        }
        assert_eq!(link(&[("TMUX", "/tmp/tmux-501/default,1,0")]), tmux);
        assert_eq!(link(&[("TERM", "tmux-256color")]), tmux, "carried over ssh");
        assert_eq!(link(&[("TERM_PROGRAM", "tmux")]), tmux);
        assert_eq!(
            link(&[("SSH_CONNECTION", "10.0.0.2 51234 10.0.0.1 22")]),
            ssh
        );
        assert_eq!(link(&[("SSH_TTY", "/dev/ttys004")]), ssh);
    }

    #[test]
    fn bundled_is_the_bundled_packs_max_density() {
        let pack = pixtuoid_scene::pack::load_bundled_pack().expect("the bundled pack loads");
        assert_eq!(pack.max_density_variant(), BUNDLED);
    }

    fn answered(protocol: Option<ImageProtocol>, cell: CellSize, tmux: bool) -> Probe {
        Probe::Answered(Detected {
            protocol,
            cell: Some(cell),
            tmux,
            unanimated: false,
            named: false,
            shm: false,
        })
    }

    fn capable(cell: CellSize) -> Probe {
        answered(Some(ImageProtocol::Kitty), cell, false)
    }

    fn d(n: u16) -> Density {
        Density::new(n).expect("nonzero")
    }

    fn cell(w: u16, h: u16) -> CellSize {
        CellSize { w, h }
    }

    fn scale(cell: CellSize, max_density: Density) -> Option<u16> {
        cutaway_fit(cell, AREA, max_density).map(|f| f.scale().get())
    }

    fn plan(probe: Probe, max_density: Density) -> Plan {
        resolve(None, probe, max_density, AREA)
    }

    fn look(look: LookMode) -> LookChoice {
        LookChoice::new(look, None)
    }

    fn forced(protocol: ImageProtocol) -> LookChoice {
        LookChoice::new(LookMode::Auto, Some(protocol))
    }

    const LOCAL: TermEnv = TermEnv {
        term: None,
        term_program: None,
        tmux_client: false,
        ssh: false,
        screen_window: false,
    };
    const OVER_SSH: TermEnv = TermEnv { ssh: true, ..LOCAL };
    const IN_SCREEN: TermEnv = TermEnv {
        screen_window: true,
        ..LOCAL
    };

    /// The plan `choice` makes in `env`, and how often it asked the terminal.
    fn asked(choice: LookChoice, env: &TermEnv, probe: Probe) -> (Plan, u32) {
        let mut asked = 0;
        let plan = detect(choice, env, BUNDLED, AREA, || {
            asked += 1;
            probe
        });
        (plan, asked)
    }

    fn row_in(env: &TermEnv, choice: LookChoice, probe: Probe, max_density: Density) -> String {
        detect(choice, env, max_density, AREA, || probe).diagnostic_row(None)
    }

    fn row(choice: LookChoice, probe: Probe, max_density: Density) -> String {
        row_in(&LOCAL, choice, probe, max_density)
    }

    fn too_small(cell: CellSize, max_density: Density) -> Plan {
        Plan::Classic {
            reason: ClassicReason::CellTooSmall { cell, max_density },
        }
    }

    /// The ~1:2 cell the whole half-block technique assumes: 8 wide, 16
    /// tall, so a logical unit is 8px either way and the office keeps its
    /// proportions exactly.
    #[test]
    fn a_standard_cell_yields_its_width_as_the_scale() {
        assert_eq!(scale(CELL_8X16, BASE_ONLY), Some(8));
        assert_eq!(scale(cell(10, 20), BASE_ONLY), Some(10));
    }

    #[test]
    fn a_non_standard_cell_never_stretches_a_logical_unit() {
        // Taller than 1:2 — the width is the binding constraint.
        assert_eq!(scale(cell(8, 24), BASE_ONLY), Some(8));
        // WIDER than 1:2 — height binds.
        assert_eq!(scale(cell(12, 16), BASE_ONLY), Some(8));
    }

    /// `RenderScale` cannot be zero, so the Option is the honest return rather
    /// than a clamp to 1.
    #[test]
    fn a_degenerate_cell_has_no_scale_at_all() {
        assert_eq!(scale(cell(0, 0), BASE_ONLY), None);
        assert_eq!(scale(cell(8, 1), BASE_ONLY), None);
        assert_eq!(scale(cell(0, 16), BASE_ONLY), None);
    }

    /// A cell whose natural scale IS a multiple of the densest art renders at
    /// it, upscaling the density render by exactly that multiple.
    #[test]
    fn a_cell_on_an_exact_multiple_renders_at_it() {
        let d = BUNDLED.get();
        for k in 1..=8 {
            let fit = cutaway_fit(cell(d * k, 2 * d * k), AREA, BUNDLED).expect("lands");
            assert_eq!(fit.scale().get(), d * k, "k={k}");
            assert_eq!(fit.density(), BUNDLED, "k={k}");
            assert_eq!(fit.upscale(), k, "k={k}");
        }
    }

    /// Only multiples of the densest art are candidates: a cell a coarser
    /// density would land on is still too small for the pack.
    #[test]
    fn only_multiples_of_the_densest_art_are_candidates() {
        assert_eq!(scale(CELL_8X16, d(16)), None, "8 is no multiple of 16");
        assert_eq!(scale(cell(16, 32), d(16)), Some(16));
        assert_eq!(scale(cell(20, 40), d(16)), Some(16));
        assert_eq!(scale(cell(17, 41), BUNDLED), Some(16));
        assert_eq!(
            scale(cell(6, 12), BUNDLED),
            Some(8),
            "6 lands nearer 8 by ratio"
        );
    }

    /// Below the densest art its own value is the one candidate, taken while the
    /// natural scale lies within √2 of it. The pair either side of the bound,
    /// derived from each density: no integer pair sits ON √2, which is
    /// irrational, so these two are the boundary.
    #[test]
    fn the_densest_art_is_reached_within_root_two_and_not_past_it() {
        for max in [BUNDLED, d(8), d(16), d(64)] {
            let dd = u32::from(max.get());
            let first = (1..=max.get())
                .find(|&n| 2 * u32::from(n) * u32::from(n) >= dd * dd)
                .expect("the density itself is within the bound");
            assert_ne!(2 * u32::from(first).pow(2), dd * dd, "√2 is never hit");
            let inside = cell(first, 2 * first);
            let outside = cell(first - 1, 2 * (first - 1));
            assert_eq!(scale(inside, max), Some(max.get()), "{max}x at {first}");
            assert_eq!(scale(outside, max), None, "{max}x at {}", first - 1);
            assert!(matches!(plan(capable(inside), max), Plan::Cutaway { .. }));
            assert_eq!(plan(capable(outside), max), too_small(outside, max));
        }
    }

    /// The office is sized from the area's pixels on each axis: as many
    /// logical units as fit, never a fixed aspect framed by bars.
    #[test]
    fn the_office_fills_the_terminal_without_a_letterbox() {
        let c = cell(17, 41);
        for (cols, rows) in [(120, 40), (80, 24), (240, 30), (40, 60)] {
            let fit = cutaway_fit(
                c,
                TermSize {
                    width: cols,
                    height: rows,
                },
                BUNDLED,
            )
            .expect("lands");
            let s = u32::from(fit.scale().get());
            for (logical, px) in [
                (fit.logical().w, u32::from(cols) * u32::from(c.w)),
                (fit.logical().h, u32::from(rows) * u32::from(c.h)),
            ] {
                let covered = u32::from(logical) * s;
                assert!(covered <= px && px < covered + s, "{cols}x{rows}: {fit:?}");
            }
        }
    }

    /// Past what a buffer can address the office stops growing rather than
    /// wrapping to a sliver.
    #[test]
    fn a_terminal_wider_than_a_buffer_gets_the_widest_office_a_buffer_holds() {
        let huge = TermSize {
            width: u16::MAX,
            height: 1,
        };
        let fit = cutaway_fit(cell(17, 41), huge, BUNDLED).expect("lands");
        assert_eq!(fit.logical().w, fit.scale().logical(u16::MAX));
    }

    /// Every protocol the probe can report reaches the plan as itself.
    #[test]
    fn auto_takes_the_cutaway_through_the_protocol_the_terminal_speaks() {
        for protocol in [
            ImageProtocol::Kitty,
            ImageProtocol::Sixel,
            ImageProtocol::Iterm2,
        ] {
            let got = plan(answered(Some(protocol), CELL_8X16, false), BUNDLED);
            let Plan::Cutaway {
                fit,
                route,
                cell,
                chosen,
            } = got
            else {
                panic!("{protocol:?}: {got:?}");
            };
            assert_eq!(
                (route, cell, chosen),
                (Route::direct(protocol, false), CELL_8X16, Chosen::Answer)
            );
            assert_eq!((fit.scale().get(), fit.upscale()), (8, 2));
        }
    }

    /// Inside tmux, kitty alone survives — and carries the tmux fact on to
    /// the encoder; the rest fall back, naming the protocol that was refused.
    #[test]
    fn inside_tmux_only_kitty_takes_the_cutaway() {
        let plan = |p| plan(answered(Some(p), CELL_8X16, true), BUNDLED);
        assert!(matches!(
            plan(ImageProtocol::Kitty),
            Plan::Cutaway {
                route: Route {
                    protocol: ImageProtocol::Kitty,
                    tmux: true,
                    ..
                },
                ..
            }
        ));
        for p in [ImageProtocol::Sixel, ImageProtocol::Iterm2] {
            assert_eq!(
                plan(p),
                Plan::Classic {
                    reason: ClassicReason::TmuxNeedsKitty(p)
                }
            );
        }
    }

    /// A named protocol replaces the terminal's answer, whatever it was; every
    /// other fact — the cell, tmux, a terminal that never answered — still
    /// decides.
    #[test]
    fn a_named_protocol_overrides_the_answer_and_nothing_else() {
        for want in [
            ImageProtocol::Kitty,
            ImageProtocol::Sixel,
            ImageProtocol::Iterm2,
        ] {
            let mode = Some(want);
            for answered_with in [None, Some(ImageProtocol::Kitty)] {
                let got = resolve(
                    mode,
                    answered(answered_with, CELL_8X16, false),
                    BUNDLED,
                    AREA,
                );
                assert!(
                    matches!(got, Plan::Cutaway { route, chosen: Chosen::Forced, .. } if route.protocol() == want),
                    "{want:?} over {answered_with:?}: {got:?}"
                );
            }
            let tiny = cell(2, 4);
            assert_eq!(
                resolve(mode, capable(tiny), BUNDLED, AREA),
                too_small(tiny, BUNDLED)
            );
            assert_eq!(
                resolve(mode, Probe::NoAnswer, BUNDLED, AREA),
                Plan::Classic {
                    reason: ClassicReason::NoAnswer
                }
            );
        }
        assert_eq!(
            resolve(
                Some(ImageProtocol::Sixel),
                answered(Some(ImageProtocol::Kitty), CELL_8X16, true),
                BUNDLED,
                AREA
            ),
            Plan::Classic {
                reason: ClassicReason::TmuxNeedsKitty(ImageProtocol::Sixel)
            }
        );
    }

    /// Classic never reaches the terminal, wherever it runs, however capable
    /// the terminal is: the probe is the seam that would.
    #[test]
    fn classic_plans_without_asking_the_terminal() {
        for env in [LOCAL, OVER_SSH, IN_SCREEN] {
            assert_eq!(
                asked(look(LookMode::Classic), &env, capable(CELL_8X16)),
                (
                    Plan::Classic {
                        reason: ClassicReason::Disabled
                    },
                    0
                ),
                "{env:?}"
            );
        }
    }

    /// `auto` keeps classic over ssh and inside GNU screen without asking;
    /// `cutaway` asks there, and plans from the answer.
    #[test]
    fn auto_asks_neither_over_ssh_nor_inside_screen_and_cutaway_asks_both() {
        let classic = |reason| (Plan::Classic { reason }, 0);
        assert_eq!(
            asked(look(LookMode::Auto), &OVER_SSH, capable(CELL_8X16)),
            classic(ClassicReason::OverSsh)
        );
        assert_eq!(
            asked(look(LookMode::Auto), &IN_SCREEN, capable(CELL_8X16)),
            classic(ClassicReason::InsideScreen)
        );
        let both = TermEnv {
            ssh: true,
            ..IN_SCREEN
        };
        assert_eq!(
            asked(look(LookMode::Auto), &both, capable(CELL_8X16)).1,
            0,
            "over ssh into screen"
        );
        let cutaway = (
            resolve(None, capable(CELL_8X16), BUNDLED, image_area(AREA)),
            1,
        );
        for env in [OVER_SSH, IN_SCREEN, both] {
            assert_eq!(
                asked(look(LookMode::Cutaway), &env, capable(CELL_8X16)),
                cutaway,
                "{env:?}"
            );
        }
    }

    /// A tmux run inside a screen window is tmux's to answer for: `auto` asks
    /// it, whether `$TMUX` or only tmux's `TERM` says so.
    #[test]
    fn auto_asks_a_tmux_run_inside_screen() {
        for tmux in [
            TermEnv {
                tmux_client: true,
                ..IN_SCREEN
            },
            TermEnv {
                term: Some("tmux-256color".into()),
                ..IN_SCREEN
            },
        ] {
            assert_eq!(
                asked(look(LookMode::Auto), &tmux, capable(CELL_8X16)).1,
                1,
                "{tmux:?}"
            );
        }
    }

    /// `--protocol` implies the cutaway over any look, so it asks everywhere
    /// and forces its protocol over the answer.
    #[test]
    fn a_forced_protocol_implies_the_cutaway_and_asks_everywhere() {
        for over in [LookMode::Auto, LookMode::Cutaway, LookMode::Classic] {
            let choice = LookChoice::new(over, Some(ImageProtocol::Sixel));
            assert_eq!(choice, forced(ImageProtocol::Sixel), "{over:?}");
            for env in [LOCAL, OVER_SSH, IN_SCREEN] {
                let (got, n) = asked(choice, &env, capable(CELL_8X16));
                assert_eq!(n, 1, "{over:?} in {env:?}");
                assert!(
                    matches!(got, Plan::Cutaway { route, chosen: Chosen::Forced, .. } if route.protocol() == ImageProtocol::Sixel),
                    "{over:?} in {env:?}: {got:?}"
                );
            }
        }
    }

    /// `auto` is the default, and names itself as `--look` takes it.
    #[test]
    fn auto_is_the_default_look() {
        assert_eq!(LookChoice::default(), look(LookMode::Auto));
        for mode in <LookMode as clap::ValueEnum>::value_variants() {
            assert_eq!(
                <LookMode as clap::ValueEnum>::from_str(&mode.to_string(), false),
                Ok(*mode)
            );
        }
    }

    /// `--protocol`'s values are the names the protocols go by elsewhere.
    #[test]
    fn a_protocol_parses_by_its_name() {
        for p in <ImageProtocol as clap::ValueEnum>::value_variants() {
            assert_eq!(
                <ImageProtocol as clap::ValueEnum>::from_str(p.name(), false),
                Ok(*p)
            );
        }
    }

    /// `run` and `doctor` plan through the one probe this platform has, so
    /// neither prints a protocol the other never sees: off Unix it never asks.
    #[test]
    fn the_platform_probe_asks_only_where_run_can() {
        let can_ask = cfg!(unix);
        let unasked = if can_ask {
            Probe::NotQueried
        } else {
            Probe::Unsupported
        };
        assert_eq!(probe(false), unasked, "doctor, piped");
        if !can_ask {
            assert_eq!(probe(true), Probe::Unsupported, "run");
        }
    }

    /// Every other choice asks a local terminal exactly once and plans from
    /// the answer.
    #[test]
    fn every_other_choice_asks_once_and_plans_from_the_answer() {
        for choice in [
            look(LookMode::Auto),
            look(LookMode::Cutaway),
            forced(ImageProtocol::Kitty),
            forced(ImageProtocol::Sixel),
            forced(ImageProtocol::Iterm2),
        ] {
            assert_eq!(
                asked(choice, &LOCAL, capable(CELL_8X16)),
                (
                    resolve(
                        choice.protocol,
                        capable(CELL_8X16),
                        BUNDLED,
                        image_area(AREA)
                    ),
                    1
                ),
                "{choice:?}"
            );
        }
    }

    #[test]
    fn the_environment_names_a_screen_window() {
        let env = |sty: Option<&str>, tmux: Option<&str>| {
            temp_env::with_vars(
                [
                    ("STY", sty),
                    ("TMUX", tmux),
                    ("TERM", None),
                    ("TERM_PROGRAM", None),
                ],
                TermEnv::read,
            )
        };
        assert!(env(Some("12345.pts-0.host"), None).screen());
        for blank in ["", " \t"] {
            assert!(!env(Some(blank), None).screen(), "{blank:?} is unset");
        }
        assert!(!env(None, None).screen());
        assert!(
            !env(Some("12345.pts-0.host"), Some("/tmp/tmux-501/default,1,0")).screen(),
            "tmux inside screen"
        );
    }

    /// The image covers the scene and never the footer: text and image never
    /// share a cell.
    #[test]
    fn the_image_area_leaves_the_footer_to_text() {
        let area = image_area(AREA);
        assert_eq!(area.width, AREA.width);
        assert_eq!(
            area.height,
            crate::tui::renderer::scene_rect(ratatui::layout::Rect::new(
                0,
                0,
                AREA.width,
                AREA.height
            ))
            .height
        );
        assert!(area.height < AREA.height);
    }

    /// A tmux user on SIXEL with a cell too small for the art hears about the
    /// cell, not about a switch to kitty that would not help.
    #[test]
    fn a_cell_too_small_is_reported_before_tmux() {
        let tiny = cell(2, 4);
        assert_eq!(
            plan(answered(Some(ImageProtocol::Sixel), tiny, true), BUNDLED),
            too_small(tiny, BUNDLED)
        );
    }

    /// The fallback is the common path — most terminals have no protocol —
    /// so it must never be silent. A user asking "why is it not the pretty
    /// one" gets an answer in every shape.
    #[test]
    fn every_way_of_lacking_graphics_falls_back_with_a_reason() {
        let small = |cell, max_density| ClassicReason::CellTooSmall { cell, max_density };
        let cases = [
            (Probe::NotQueried, BASE_ONLY, ClassicReason::NotQueried),
            (Probe::NoAnswer, BASE_ONLY, ClassicReason::NoAnswer),
            (
                Probe::TmuxPassthroughOff,
                BASE_ONLY,
                ClassicReason::TmuxPassthroughOff,
            ),
            (Probe::Unsupported, BASE_ONLY, ClassicReason::Unsupported),
            (
                Probe::Answered(Detected {
                    protocol: Some(ImageProtocol::Kitty),
                    cell: None,
                    tmux: false,
                    unanimated: false,
                    named: false,
                    shm: false,
                }),
                BASE_ONLY,
                ClassicReason::NoCellSize,
            ),
            (
                answered(None, CELL_8X16, false),
                BASE_ONLY,
                ClassicReason::NoProtocol,
            ),
            (capable(cell(0, 0)), BASE_ONLY, small(cell(0, 0), BASE_ONLY)),
            (capable(cell(1, 2)), BASE_ONLY, small(cell(1, 2), BASE_ONLY)),
            (capable(cell(2, 4)), BUNDLED, small(cell(2, 4), BUNDLED)),
        ];
        for (probe, max_density, want) in cases {
            assert_eq!(
                plan(probe, max_density),
                Plan::Classic { reason: want },
                "for {probe:?}"
            );
            assert!(!want.describe().is_empty());
        }
    }

    /// A cell the art cannot reach and a cell with no pixels to subdivide are
    /// different sentences: a bigger font fixes the first, the second needs a
    /// terminal that reports its pixels.
    #[test]
    fn a_cell_too_small_names_the_art_it_is_too_small_for() {
        let reason =
            |cell, max_density| ClassicReason::CellTooSmall { cell, max_density }.describe();
        let small_font = reason(cell(2, 4), BUNDLED);
        assert!(small_font.contains("the pack's 4x art"), "{small_font}");
        let no_pixels = reason(cell(1, 2), BUNDLED);
        assert!(no_pixels.ends_with("too small to subdivide"), "{no_pixels}");
    }

    /// A cutaway row is a tester's evidence line: the protocol and who
    /// chose it, the cell, tmux, the fit, and the tiles and cadence it is
    /// sent in. Each protocol is pinned whole, and no two rows match.
    #[test]
    fn every_protocol_prints_its_own_cutaway_row() {
        let cases = [
            (
                look(LookMode::Auto),
                answered(Some(ImageProtocol::Kitty), cell(17, 41), true),
                "graphics: kitty (the terminal's answer) on a 17x41 cell, through tmux \
                 passthrough — the cutaway at 16x (4x art upscaled 4x), a 127x99 office, sent \
                 as 4x2-cell tiles (doubled until a frame is at most 400 tiles) every frame",
            ),
            (
                look(LookMode::Auto),
                answered(Some(ImageProtocol::Sixel), cell(17, 41), false),
                "graphics: sixel (the terminal's answer) on a 17x41 cell, direct — the cutaway \
                 at 16x (4x art upscaled 4x), a 127x99 office, sent as 8x4-cell tiles at most \
                 every 66 ms",
            ),
            (
                forced(ImageProtocol::Iterm2),
                answered(None, cell(8, 16), false),
                "graphics: iterm2 (forced by --protocol) on a 8x16 cell, direct — the cutaway \
                 at 8x (4x art upscaled 2x), a 120x78 office, sent as 8x4-cell tiles at most \
                 every 100 ms",
            ),
            (
                look(LookMode::Auto),
                Probe::Answered(Detected {
                    named: true,
                    ..match answered(Some(ImageProtocol::Iterm2), cell(8, 16), false) {
                        Probe::Answered(d) => d,
                        _ => unreachable!(),
                    }
                }),
                "graphics: iterm2 (guessed from the terminal's name) on a 8x16 cell, direct — \
                 the cutaway at 8x (4x art upscaled 2x), a 120x78 office, sent as 8x4-cell \
                 tiles at most every 100 ms",
            ),
        ];
        let rows: Vec<String> = cases
            .iter()
            .map(|&(choice, probe, want)| {
                let got = row(choice, probe, BUNDLED);
                assert_eq!(got, want, "{choice:?}");
                got
            })
            .collect();
        let distinct: std::collections::HashSet<_> = rows.iter().collect();
        assert_eq!(distinct.len(), rows.len());
    }

    /// A doctor that prints a cutaway where `run`'s own choice paints classic
    /// unasked says why, or a bare `run` would show classic unexplained; a
    /// `run` that asks, and every classic row, add nothing.
    #[test]
    fn a_cutaway_row_says_why_run_paints_classic_unasked() {
        let cutaway = plan(capable(CELL_8X16), BUNDLED);
        let painted = cutaway.diagnostic_row(None);
        assert!(!painted.contains("paints classic"), "{painted}");
        for why in [
            ClassicReason::Disabled,
            ClassicReason::OverSsh,
            ClassicReason::InsideScreen,
        ] {
            assert_eq!(
                cutaway.diagnostic_row(Some(why)),
                format!("{painted} — `run` paints classic here: {}", why.describe())
            );
        }
        let classic = plan(Probe::NoAnswer, BUNDLED);
        assert_eq!(
            classic.diagnostic_row(Some(ClassicReason::OverSsh)),
            classic.diagnostic_row(None)
        );
    }

    /// A reason keeps its variant only by printing its own row, a remedy the
    /// user can act on: each is pinned whole, reached through [`resolve`], and
    /// no two may match. The exhaustive `match` fails to compile on a new reason
    /// until it gets a row here.
    #[test]
    fn every_classic_reason_prints_its_own_row() {
        let cases = [
            (
                LOCAL,
                look(LookMode::Classic),
                capable(CELL_8X16),
                BUNDLED,
                "chosen by `--look classic` or `look = \"classic\"`",
            ),
            (
                OVER_SSH,
                look(LookMode::Auto),
                capable(CELL_8X16),
                BUNDLED,
                "over ssh, where every repaint crosses the network — `--look cutaway` asks the \
                 terminal anyway",
            ),
            (
                IN_SCREEN,
                look(LookMode::Auto),
                capable(CELL_8X16),
                BUNDLED,
                "inside GNU screen, which shows the graphics query in its status line and \
                 answers no protocol — `--look cutaway` asks anyway",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                Probe::Unsupported,
                BUNDLED,
                "this build cannot ask the terminal for graphics (not on Unix)",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                Probe::NotQueried,
                BUNDLED,
                "the terminal was not asked (stdout is not a terminal, there is no controlling \
                 terminal, or $TERM is dumb) — run in an interactive terminal to see what it \
                 supports",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                Probe::NoAnswer,
                BUNDLED,
                "the terminal did not answer the capability query",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                answered(None, CELL_8X16, false),
                BUNDLED,
                "terminal reports no graphics protocol (kitty/iterm2/sixel)",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                Probe::TmuxPassthroughOff,
                BUNDLED,
                "inside tmux with allow-passthrough off — `set -g allow-passthrough on` lets \
                 kitty graphics through",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                Probe::Answered(Detected {
                    protocol: None,
                    cell: Some(CELL_8X16),
                    tmux: false,
                    unanimated: true,
                    named: false,
                    shm: false,
                }),
                BUNDLED,
                "this terminal has no image protocol the cutaway animates with — \
                 `--protocol kitty|sixel|iterm2` forces one",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                Probe::Answered(Detected {
                    protocol: Some(ImageProtocol::Kitty),
                    cell: None,
                    tmux: false,
                    unanimated: false,
                    named: false,
                    shm: false,
                }),
                BUNDLED,
                "terminal reports no cell size in pixels",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                answered(Some(ImageProtocol::Iterm2), CELL_8X16, true),
                BUNDLED,
                "inside tmux only kitty graphics survive a pane switch here, and this terminal \
                 speaks iterm2",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                capable(cell(2, 4)),
                BUNDLED,
                "terminal reports a 2x4 cell — too small for the pack's 4x art",
            ),
            (
                LOCAL,
                look(LookMode::Auto),
                capable(cell(1, 2)),
                BUNDLED,
                "terminal reports a 1x2 cell — too small to subdivide",
            ),
        ];
        let n = cases.len();
        let (mut seen, mut rows) = (
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        );
        for (env, choice, probe, max_density, want) in cases {
            let Plan::Classic { reason } = detect(choice, &env, max_density, AREA, || probe) else {
                panic!("{probe:?} must fall back");
            };
            seen.insert(match reason {
                ClassicReason::Disabled => 0,
                ClassicReason::Unsupported => 1,
                ClassicReason::NotQueried => 2,
                ClassicReason::NoAnswer => 3,
                ClassicReason::NoProtocol => 4,
                ClassicReason::TmuxPassthroughOff => 5,
                ClassicReason::NoCellSize => 6,
                ClassicReason::TmuxNeedsKitty(_) => 7,
                ClassicReason::CellTooSmall { .. } => 8,
                ClassicReason::NoCutawayProtocol => 9,
                ClassicReason::OverSsh => 10,
                ClassicReason::InsideScreen => 11,
            });
            let row = row_in(&env, choice, probe, max_density);
            assert_eq!(row, format!("graphics: classic half-blocks — {want}"));
            rows.insert(row);
        }
        assert_eq!(seen.len(), 12, "every reason has a pinned row");
        assert_eq!(rows.len(), n, "no two reasons print the same row");
    }

    #[cfg(unix)]
    #[test]
    fn shared_memory_carries_kitty_where_the_terminal_reads_it() {
        let detected = |shm, tmux| {
            Probe::Answered(Detected {
                protocol: Some(ImageProtocol::Kitty),
                cell: Some(CELL_8X16),
                tmux,
                unanimated: false,
                named: false,
                shm,
            })
        };
        let medium = |mode, probe| match resolve(mode, probe, BUNDLED, AREA) {
            Plan::Cutaway { route, .. } => route.medium(),
            plan => panic!("no cutaway: {plan:?}"),
        };
        for mode in [None, Some(ImageProtocol::Kitty)] {
            assert_eq!(
                medium(mode, detected(true, false)),
                Medium::SharedMemory,
                "{mode:?}"
            );
            assert_eq!(
                medium(mode, detected(false, false)),
                Medium::Direct,
                "{mode:?}"
            );
            assert_eq!(
                medium(mode, detected(true, true)),
                Medium::Direct,
                "{mode:?} in tmux"
            );
        }
    }

    /// A terminal that answers no protocol the cutaway animates with falls
    /// back on its own, but `--protocol` still forces one: it overrides
    /// detection.
    #[test]
    fn a_forced_protocol_paints_where_none_animates() {
        let probe = Probe::Answered(Detected {
            protocol: None,
            cell: Some(CELL_8X16),
            tmux: false,
            unanimated: true,
            named: false,
            shm: false,
        });
        assert!(matches!(
            resolve(None, probe, BUNDLED, AREA),
            Plan::Classic {
                reason: ClassicReason::NoCutawayProtocol
            }
        ));
        assert!(matches!(
            resolve(Some(ImageProtocol::Kitty), probe, BUNDLED, AREA),
            Plan::Cutaway { .. }
        ));
    }

    /// A real Retina Ghostty reports a 17x41 cell: 17 is what the cell alone
    /// allows, and the pack's 4x art moves it to the nearest multiple.
    #[test]
    fn the_cell_gives_the_natural_scale_and_the_pack_fits_it() {
        assert_eq!(raw_scale_for_cell(cell(17, 41)), 17);
        assert_eq!(scale(cell(17, 41), BUNDLED), Some(16));
    }

    /// 2 is the first scale that buys anything over the classic density, and
    /// the boundary is pinned from BOTH sides so a future `>=` typo cannot
    /// slip through.
    #[test]
    fn the_cutoff_is_where_the_image_path_starts_buying_something() {
        assert_eq!(
            plan(capable(cell(1, 2)), BASE_ONLY),
            too_small(cell(1, 2), BASE_ONLY),
            "1px per unit buys nothing"
        );
        assert!(
            matches!(
                plan(capable(cell(2, 4)), BASE_ONLY),
                Plan::Cutaway { fit, .. } if fit.scale().get() == 2
            ),
            "2px per unit is the first density worth an encode"
        );
    }
}
