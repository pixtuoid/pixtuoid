//! Runtime wiring: `RunConfig` (the startup inputs), the boot-capacity math, and the
//! headless summary formatter. The untestable async glue (tokio runtime, reducer task,
//! source spawn, Ctrl-C loop) lives in `driver.rs`, which is excluded from coverage.

pub(crate) mod driver;
pub(crate) mod gate;
pub(crate) mod pipeline;

pub(crate) use driver::run;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pixtuoid_core::SceneState;
use pixtuoid_core::source::manager::SourceDeath;
use pixtuoid_core::state::{ActivityState, DaemonState, MAX_FLOORS};
use ratatui::layout::Size as TermSize;
use tokio::sync::watch;

use crate::graphics::Plan;

/// The reducer publishes a fresh `Arc<SceneState>` on every mutation through this watch
/// channel. Consumers (renderer, headless summary loop) hold a `Receiver`, call
/// `borrow()` for an O(1) pointer read, and never block the writer.
pub(crate) type SceneRx = watch::Receiver<Arc<SceneState>>;

/// Fallback desk capacity when the terminal cannot be queried (e.g. headless mode).
pub(crate) const FALLBACK_DESKS: usize = 16;

/// The startup inputs shared by `run` + `run_async`. The `theme` is already resolved
/// (`config::resolve_theme` validates CLI + config in one place), so an unknown theme
/// can't reach the runtime by construction.
#[derive(Debug)]
pub(crate) struct RunConfig {
    pub(crate) socket: Option<PathBuf>,
    pub(crate) desk_cap: Option<usize>,
    pub(crate) headless: bool,
    pub(crate) config_path: PathBuf,
    pub(crate) theme: &'static pixtuoid_scene::theme::Theme,
    pub(crate) pets: Vec<pixtuoid_scene::pet::Pet>,
    /// The resolved set of CONNECTED source ids (registry names). A disconnected
    /// source's events are dropped + its sprites evicted.
    pub(crate) connected: HashSet<String>,
    /// Where the warn-floor log lives, which the Sources panel reads for each
    /// source's drift history.
    pub(crate) log: crate::run_log::LogLocation,
    /// The sources this run's decode drift has named, for the footer nudge.
    pub(crate) drift: crate::doctor::DriftSeen,
    /// Nothing is connected over a config that loaded (`sources::is_first_run`) —
    /// the TUI plays the onboarding "move-in" overlay. Ignored by headless + `floating`.
    pub(crate) first_run: bool,
    /// Resolved `[audio]` settings — muted defaults TRUE (the lazy spawn waits for the
    /// first `m`), volume pre-clamped by `config::resolve_audio`. Headless ignores it.
    pub(crate) audio: crate::config::AudioConfig,
    /// `--protocol` over `config::resolve_look`'s mode. Headless and `floating` ignore it.
    pub(crate) look: crate::graphics::LookChoice,
    /// Resolved by `config::resolve_motion`; `auto` is what the TUI's graphics
    /// plan affords, and Full in the floating window.
    pub(crate) motion: crate::config::MotionMode,
}

/// A live, shared set of connected source ids — the runtime mirror of
/// `sources::connected`. On lock poison it recovers the set via `into_inner`: the data is
/// always valid (insert/remove/contains never panic), and losing it would mass-evict
/// the office.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConnectedSources(Arc<Mutex<HashSet<String>>>);

impl ConnectedSources {
    pub(crate) fn new(initial: HashSet<String>) -> Self {
        Self(Arc::new(Mutex::new(initial)))
    }
    fn guard(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn is_connected(&self, source_id: &str) -> bool {
        self.guard().contains(source_id)
    }
    pub(crate) fn snapshot(&self) -> HashSet<String> {
        self.guard().clone()
    }
    pub(crate) fn set(&self, source_id: &str, connected: bool) {
        let mut g = self.guard();
        if connected {
            g.insert(source_id.to_string());
        } else {
            g.remove(source_id);
        }
    }
}

/// Each floor's desks in the office `plan` paints on a terminal `term` cells
/// big, each floor with its own seed; 0 where a floor's layout rejects it.
fn floor_capacities(plan: Plan, term: TermSize) -> [usize; MAX_FLOORS] {
    let office = plan.office_extent(term);
    std::array::from_fn(|i| {
        // The ONE seed derivation every call site shares — an inline copy of the
        // formula would silently drift the boot capacities from the rendered layout
        // (over-seeded atomics strand agents on unrendered desks).
        let seed = pixtuoid_scene::floor::floor_seed(i);
        pixtuoid_scene::floor::floor_capacity(office.w, office.h, seed)
    })
}

/// [`floor_capacities`] as the boot seed. When a floor's layout rejects the
/// office (e.g. too small), fall back to `FALLBACK_DESKS` for that floor so the
/// reducer can still seat agents — they may render off-grid on the tiny
/// terminal, but won't be silently dropped during the boot race before the first
/// TUI frame.
pub(crate) fn boot_capacities_for(plan: Plan, term: TermSize) -> [usize; MAX_FLOORS] {
    floor_capacities(plan, term).map(|cap| if cap == 0 { FALLBACK_DESKS } else { cap })
}

/// Clamp each per-floor boot capacity to an optional `--max-desks` cap, so the boot
/// atomics are never seeded above the real layout capacity (`fetch_max` only grows; an
/// over-seed strands agents on non-existent desks until the terminal grows).
fn cap_boot_capacities(base: [usize; MAX_FLOORS], cap: Option<usize>) -> [usize; MAX_FLOORS] {
    match cap {
        Some(c) => base.map(|x| x.min(c)),
        None => base,
    }
}

/// The headless-vs-interactive boot capacity POLICY: headless (no `plan`) honors
/// `--max-desks` UNCLAMPED and never calls `measure`; interactive clamps
/// [`boot_capacities_for`] to the cap ([`cap_boot_capacities`]).
pub(crate) fn resolve_boot_caps(
    desk_cap: Option<usize>,
    plan: Option<Plan>,
    measure: impl FnOnce() -> TermSize,
) -> [usize; MAX_FLOORS] {
    match (desk_cap, plan) {
        (Some(cap), None) => [cap; MAX_FLOORS],
        (None, None) => [FALLBACK_DESKS; MAX_FLOORS],
        (cap, Some(plan)) => cap_boot_capacities(boot_capacities_for(plan, measure()), cap),
    }
}

// The headless stdout summary derives labels / tool detail / Notification reason
// from untrusted transcript+hook input, so a crafted ANSI/OSC escape would otherwise
// reach the user's terminal verbatim (the TUI is immune — ratatui neutralizes escapes
// in its cell buffer).
use crate::strip_control_chars as sanitize_line;

fn summarize(scene: &SceneState) -> String {
    let agents: Vec<String> = scene
        .agents
        .values()
        .map(|a| {
            let state = match &a.state {
                ActivityState::Idle => "idle".to_string(),
                ActivityState::Active { detail, .. } => {
                    format!(
                        "active({})",
                        sanitize_line(detail.as_deref().unwrap_or("?"))
                    )
                }
                ActivityState::Waiting { reason } => {
                    format!("waiting({})", sanitize_line(reason))
                }
            };
            format!("{}@{}:{}", sanitize_line(&a.label), a.desk_index.0, state)
        })
        .collect();
    // Daemon-style sources (the OpenClaw gateway lobster) render as wandering mascots,
    // not desk agents — surface them here too so headless is a complete window onto the
    // scene. The source name is a registry id (controlled), but sanitize it like every
    // other field on this stdout path.
    let daemons: Vec<String> = scene
        .daemons()
        .map(|(source, instance, p)| {
            let state = match p.display_state() {
                DaemonState::Idle => "idle",
                DaemonState::Busy => "busy",
                DaemonState::Degraded => "degraded",
                DaemonState::Down => "down",
            };
            // The instance (an OpenClaw gateway port) is load-bearing, not cosmetic:
            // two gateways are two rows, so the live-e2e can assert one going down
            // leaves the other alone.
            format!(
                "{}@{}:{}",
                sanitize_line(source),
                sanitize_line(instance.as_str()),
                state
            )
        })
        .collect();
    format!(
        "agents=[{}] daemons=[{}]",
        agents.join(", "),
        daemons.join(", ")
    )
}

/// Format a `SourceDeath` for the headless stdout health line. Both fields are
/// `sanitize_line`d before printing: `error` is `format!("{e:#}")` of an `anyhow` chain
/// that can embed external strings carrying terminal escapes, and `source` is sanitized
/// too for defense-in-depth.
fn format_source_death(d: &SourceDeath) -> String {
    format!(
        "warning: source '{}' died: {}",
        sanitize_line(&d.source),
        sanitize_line(&d.error)
    )
}

/// The not-yet-surfaced tail of the grow-only `SourceDeath` health watch, advancing
/// `seen` past it. Each consumer surfaces a death exactly once by tracking a running
/// count — logging the whole borrow on every change would re-warn all prior deaths,
/// reading as repeated crashes in forensics.
pub(crate) fn unseen_deaths<'a>(deaths: &'a [SourceDeath], seen: &mut usize) -> &'a [SourceDeath] {
    let unseen = &deaths[*seen..];
    *seen = deaths.len();
    unseen
}

/// A boxed quit arm.
type QuitArm = std::pin::Pin<Box<dyn std::future::Future<Output = QuitSignal> + Send>>;

/// The signals that mean quit, one list. SIGHUP is the terminal window closing.
#[cfg(unix)]
const QUIT_SIGNALS: [(tokio::signal::unix::SignalKind, &str); 4] = {
    use tokio::signal::unix::SignalKind;
    [
        (SignalKind::interrupt(), "SIGINT"),
        (SignalKind::terminate(), "SIGTERM"),
        (SignalKind::hangup(), "SIGHUP"),
        (SignalKind::quit(), "SIGQUIT"),
    ]
};

/// The quit signal that ended a run. After the painter's teardown,
/// [`reraise`](Self::reraise) hands it back to the kernel, so the exit status and a
/// SIGQUIT core dump are its own (glibc, "Termination in Handler"); for SIGINT a
/// waiting shell reads the death as the user's interrupt (cons.org/cracauer/sigint.html).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuitSignal(#[cfg(unix)] std::os::raw::c_int);

impl QuitSignal {
    #[cfg(test)]
    pub(crate) fn test() -> Self {
        #[cfg(unix)]
        return Self(libc::SIGTERM);
        #[cfg(not(unix))]
        Self()
    }

    /// Deliver the signal again under its default disposition. Returns only
    /// where the platform has no such signal.
    pub(crate) fn reraise(self) {
        #[cfg(unix)]
        // SAFETY: resetting a disposition and raising the signal that already
        // ran this process's handler.
        unsafe {
            libc::signal(self.0, libc::SIG_DFL);
            libc::raise(self.0);
        }
    }
}

/// The platform's quit signals (`QUIT_SIGNALS`; `windows_arms` on Windows), their
/// handlers installed at [`arm`](Self::arm): armed before a painter sets up what its exit
/// undoes (the TUI's terminal modes, the floating window's saved geometry, the
/// kitty shared-memory unlink), so no signal can land in between and kill the
/// process by its default disposition. A signal inherited as SIG_IGN stays
/// unarmed: a handler would replace the ignore that `nohup`, or a
/// non-job-control shell for a background job (POSIX Shell §2.12), gave a run
/// meant to outlive its terminal.
pub(crate) struct QuitArms(Vec<QuitArm>);

impl QuitArms {
    pub(crate) fn arm() -> Self {
        #[cfg(unix)]
        let arms = QUIT_SIGNALS.into_iter().filter_map(arm_signal).collect();
        #[cfg(not(unix))]
        let arms = windows_arms();
        Self(arms)
    }

    /// The first armed signal's arrival. Pin it ONCE outside a loop: a
    /// per-iteration future drops the subscription mid-gap.
    pub(crate) async fn signalled(mut self) -> QuitSignal {
        std::future::poll_fn(|cx| {
            for arm in &mut self.0 {
                if let std::task::Poll::Ready(fired) = arm.as_mut().poll(cx) {
                    return std::task::Poll::Ready(fired);
                }
            }
            std::task::Poll::Pending
        })
        .await
    }
}

/// One arm, its handler installed at the call. `None` when the signal was inherited
/// as ignored or its registration failed: an arm resolving on a non-event would
/// tear the painter down.
#[cfg(unix)]
fn arm_signal((kind, name): (tokio::signal::unix::SignalKind, &'static str)) -> Option<QuitArm> {
    let raw = kind.as_raw_value();
    if inherited_ignored(raw) {
        return None;
    }
    match tokio::signal::unix::signal(kind) {
        Ok(mut sig) => Some(Box::pin(async move {
            sig.recv().await;
            QuitSignal(raw)
        })),
        Err(e) => {
            tracing::error!(
                error = %e,
                signal = name,
                "signal handler registration failed — an external signal will not quit cleanly"
            );
            None
        }
    }
}

#[cfg(unix)]
fn inherited_ignored(raw: std::os::raw::c_int) -> bool {
    // SAFETY: a null `act` only reads the current disposition into `old`.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(raw, std::ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_IGN
    }
}

/// Ctrl-C, Ctrl-Break and console close (the Windows twin of SIGHUP). Raw mode
/// turns Ctrl-C into a key, but "CTRL+BREAK is always treated as a signal"
/// (<https://learn.microsoft.com/en-us/windows/console/ctrl-c-and-ctrl-break-signals>),
/// and the close handler has [a few seconds](https://learn.microsoft.com/en-us/windows/console/handlerroutine)
/// before the OS ends the process, which a painter's teardown fits.
#[cfg(not(unix))]
fn windows_arms() -> Vec<QuitArm> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
    let mut arms: Vec<QuitArm> = Vec::new();
    macro_rules! arm {
        ($register:expr, $name:literal) => {
            match $register {
                Ok(mut sig) => arms.push(Box::pin(async move {
                    sig.recv().await;
                    QuitSignal()
                })),
                Err(e) => tracing::error!(
                    error = %e,
                    signal = $name,
                    "console handler registration failed — an external signal will not quit cleanly"
                ),
            }
        };
    }
    arm!(ctrl_c(), "Ctrl-C");
    arm!(ctrl_break(), "Ctrl-Break");
    arm!(ctrl_close(), "console close");
    arms
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::{Reducer, Transport};
    use std::time::SystemTime;

    // The shared derivation, NOT a copy: a test-local restatement structurally
    // couldn't catch the impl diverging from `floor_seed`.
    fn floor_seed(i: usize) -> u64 {
        pixtuoid_scene::floor::floor_seed(i)
    }

    fn classic() -> Plan {
        Plan::Classic {
            reason: crate::graphics::ClassicReason::Disabled,
        }
    }

    fn term(width: u16, height: u16) -> TermSize {
        TermSize { width, height }
    }

    /// Floor 0's desks in classic's office on a `cols`×`rows` terminal.
    fn ground_floor(cols: u16, rows: u16) -> usize {
        floor_capacities(classic(), term(cols, rows))[0]
    }

    #[test]
    fn format_source_death_strips_terminal_escapes_from_both_fields() {
        let d = SourceDeath::new(
            "codex\u{1b}]0;pwned\u{7}",
            "open /tmp/a\u{1b}[2Jb.jsonl: \u{1b}[31mboom\u{7}",
        );
        let out = format_source_death(&d);
        assert!(
            !out.chars().any(|c| c.is_control()),
            "no control chars may survive into the headless terminal line: {out:?}"
        );
        assert!(out.contains("source 'codex]0;pwned' died"), "got {out:?}");
        assert!(
            out.contains("open /tmp/a[2Jb.jsonl: [31mboom"),
            "got {out:?}"
        );
    }

    #[test]
    fn unseen_deaths_yields_each_death_exactly_once() {
        let mut seen = 0usize;
        let one = vec![SourceDeath::new("codex", "boom")];
        assert_eq!(unseen_deaths(&one, &mut seen).len(), 1);
        assert_eq!(seen, 1);

        let two = vec![
            SourceDeath::new("codex", "boom"),
            SourceDeath::new("claude-code", "bind"),
        ];
        let fresh = unseen_deaths(&two, &mut seen);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].source, "claude-code");

        assert!(unseen_deaths(&two, &mut seen).is_empty());
    }

    #[test]
    fn capacity_for_normal_terminal() {
        let cap = ground_floor(192, 48);
        assert!(cap > 0);
    }

    #[test]
    fn capacity_for_small_terminal() {
        let cap = ground_floor(80, 35);
        assert!(cap > 0, "80x35 should fit at least one desk");
    }

    #[test]
    fn capacity_for_tiny_terminal_returns_zero() {
        assert_eq!(ground_floor(10, 10), 0);
    }

    #[test]
    fn capacity_for_zero_rows_returns_zero() {
        assert_eq!(ground_floor(192, 0), 0);
    }

    /// The notice advertises `min_terminal_size()`; boot capacity computes the buffer
    /// back from rows. If the two ever disagree on the footer reserve, the advertised
    /// size seeds zero desks — the #803 silent over/under-seed class, other direction.
    #[test]
    fn the_advertised_minimum_seats_someone_through_the_boot_capacity_path() {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        assert!(
            ground_floor(cols, rows) > 0,
            "the advertised minimum {cols}x{rows} seeds no desks through floor_capacities"
        );
    }

    #[test]
    fn capacity_matches_renderer_formula() {
        let cols: u16 = 160;
        let rows: u16 = 50;
        let (buf_w, buf_h) = crate::tui::renderer::scene_buf_size(cols, rows);
        let expected =
            pixtuoid_scene::layout::SceneLayout::compute_with_seed(buf_w, buf_h, None, 0)
                .map(|l| l.home_desks.len())
                .unwrap_or(0);
        assert_eq!(ground_floor(cols, rows), expected);
    }

    #[test]
    fn seed_can_produce_distinct_capacities() {
        let mut found = false;
        'outer: for cols in [120u16, 140, 160, 180, 200, 220, 240] {
            for rows in [30u16, 36, 40, 48, 56, 64] {
                let unique: std::collections::HashSet<usize> =
                    floor_capacities(classic(), term(cols, rows))
                        .into_iter()
                        .collect();
                if unique.len() > 1 {
                    found = true;
                    break 'outer;
                }
            }
        }
        assert!(
            found,
            "expected at least one terminal size in the swept range where \
             per-floor seeds produce distinct capacities"
        );
    }

    #[test]
    fn resolve_boot_caps_headless_honors_cap_unclamped_interactive_clamps() {
        assert_eq!(
            resolve_boot_caps(Some(99), None, || panic!("headless must not measure")),
            [99; MAX_FLOORS]
        );
        assert_eq!(
            resolve_boot_caps(None, None, || panic!("headless must not measure")),
            [FALLBACK_DESKS; MAX_FLOORS]
        );
        let measured = boot_capacities_for(classic(), term(192, 48));
        assert!(measured.iter().all(|&c| c > 4), "{measured:?}");
        assert_eq!(
            resolve_boot_caps(Some(4), Some(classic()), || term(192, 48)),
            [4; MAX_FLOORS]
        );
        assert_eq!(
            resolve_boot_caps(None, Some(classic()), || term(192, 48)),
            measured
        );
    }

    /// The cutaway's scale rounds up, so its office can hold fewer desks than
    /// classic's on the same terminal: the boot seed is its own office's, or it
    /// seats agents at desks the cutaway never paints.
    #[test]
    fn a_cutaway_boot_seeds_no_desk_its_office_lacks() {
        use crate::graphics::{CellSize, ImageProtocol, cutaway_fit};
        let (cols, rows) = (120, 40);
        let cell = CellSize { w: 6, h: 12 };
        let area = crate::tui::renderer::scene_rect(ratatui::layout::Rect::new(0, 0, cols, rows));
        let density = pixtuoid_core::sprite::format::Density::new(4).expect("nonzero");
        let fit = cutaway_fit(cell, area.as_size(), density).expect("fits");
        assert_eq!(fit.scale().get(), 8);
        let cutaway = Plan::Cutaway {
            fit,
            route: crate::graphics::Route::direct(ImageProtocol::Kitty, false),
            cell,
            chosen: crate::graphics::Chosen::Answer,
        };
        let seed = resolve_boot_caps(None, Some(cutaway), || term(cols, rows));
        let painted = fit.logical();
        for (i, &seeded) in seed.iter().enumerate() {
            let desks = pixtuoid_scene::floor::floor_capacity(painted.w, painted.h, floor_seed(i));
            assert!(seeded <= desks, "floor {i}: seeds {seeded} of {desks}");
        }
        let classic = resolve_boot_caps(None, Some(classic()), || term(cols, rows));
        assert!(
            classic.iter().zip(&seed).any(|(c, s)| c > s),
            "classic's seed would over-seed here: {classic:?} vs {seed:?}"
        );
    }

    #[test]
    fn floor_capacities_uses_each_floor_seed() {
        let office = classic().office_extent(term(192, 48));
        let expected: [usize; MAX_FLOORS] = std::array::from_fn(|i| {
            pixtuoid_scene::floor::floor_capacity(office.w, office.h, floor_seed(i))
        });
        assert_eq!(floor_capacities(classic(), term(192, 48)), expected);
    }

    #[test]
    fn boot_capacities_falls_back_to_default_on_tiny_terminal() {
        let caps = boot_capacities_for(classic(), term(10, 10));
        assert_eq!(caps, [FALLBACK_DESKS; MAX_FLOORS]);
    }

    #[test]
    fn summarize_reports_each_activity_state() {
        use pixtuoid_core::AgentId;
        use pixtuoid_core::source::AgentEvent;

        let mut scene = SceneState::new([8; MAX_FLOORS]);
        let mut reducer = Reducer::new();
        let now = SystemTime::now();

        let seat = |reducer: &mut Reducer, scene: &mut SceneState, id: AgentId| {
            reducer.apply(
                scene,
                AgentEvent::SessionStart {
                    agent_id: id,
                    source: "claude-code".into(),
                    session_id: "s".into(),
                    cwd: std::path::PathBuf::from("/repo"),
                    parent_id: None,
                },
                now,
                Transport::Hook,
            );
        };

        let a = AgentId::from_transcript_path("/p/a.jsonl");
        seat(&mut reducer, &mut scene, a);
        reducer.apply(
            &mut scene,
            AgentEvent::ActivityStart {
                agent_id: a,
                tool_use_id: Some("t1".into()),
                detail: Some("Edit: foo.rs".into()),
            },
            now,
            Transport::Hook,
        );

        let b = AgentId::from_transcript_path("/p/b.jsonl");
        seat(&mut reducer, &mut scene, b);
        reducer.apply(
            &mut scene,
            AgentEvent::Waiting {
                agent_id: b,
                reason: "permission".into(),
                tool_use_id: None,
            },
            now,
            Transport::Hook,
        );

        let c = AgentId::from_transcript_path("/p/c.jsonl");
        seat(&mut reducer, &mut scene, c);

        let summary = summarize(&scene);
        assert!(summary.starts_with("agents=["), "got: {summary}");
        assert!(summary.contains("active(Edit: foo.rs)"), "got: {summary}");
        assert!(summary.contains("waiting(permission)"), "got: {summary}");
        assert!(summary.contains(":idle"), "got: {summary}");
        assert!(summary.contains('@'), "got: {summary}");
    }

    #[test]
    fn summarize_reports_daemon_presence() {
        use pixtuoid_core::source::daemon::{
            DaemonInstanceKey, DaemonPresenceUpdate, apply_presence,
        };
        use pixtuoid_core::state::DaemonInstanceId;

        let mut scene = SceneState::new([8; MAX_FLOORS]);
        let now = SystemTime::now();
        let gw = DaemonInstanceKey::new(
            "openclaw",
            DaemonInstanceId::new("18789").expect("non-empty"),
        );

        assert!(
            summarize(&scene).contains("daemons=[]"),
            "got: {}",
            summarize(&scene)
        );

        apply_presence(
            &mut scene,
            &gw,
            DaemonPresenceUpdate::GatewayUp { pid: Some(4242) },
            now,
        );
        assert!(
            summarize(&scene).contains("daemons=[openclaw@18789:idle]"),
            "got: {}",
            summarize(&scene)
        );

        apply_presence(
            &mut scene,
            &gw,
            DaemonPresenceUpdate::RunStarted {
                run_key: "r".into(),
            },
            now,
        );
        assert!(
            summarize(&scene).contains("daemons=[openclaw@18789:busy]"),
            "got: {}",
            summarize(&scene)
        );

        // RunFailed drains the in-flight run, so the later GatewayDown still reads down.
        apply_presence(
            &mut scene,
            &gw,
            DaemonPresenceUpdate::RunFailed {
                run_key: "r".into(),
            },
            now,
        );
        assert!(
            summarize(&scene).contains("daemons=[openclaw@18789:degraded]"),
            "got: {}",
            summarize(&scene)
        );

        apply_presence(&mut scene, &gw, DaemonPresenceUpdate::GatewayDown, now);
        assert!(
            summarize(&scene).contains("daemons=[openclaw@18789:down]"),
            "got: {}",
            summarize(&scene)
        );
    }

    #[test]
    fn summarize_strips_terminal_escapes_from_untrusted_fields() {
        use pixtuoid_core::AgentId;
        use pixtuoid_core::source::AgentEvent;

        let mut scene = SceneState::new([8; MAX_FLOORS]);
        let mut reducer = Reducer::new();
        let now = SystemTime::now();
        let id = AgentId::from_parts("cc", "esc");
        // The label derives from the cwd basename — an attacker-controlled path can
        // smuggle an OSC set-title + BEL sequence.
        reducer.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "claude-code".into(),
                session_id: "s".into(),
                cwd: std::path::PathBuf::from("/repo\u{1b}]0;pwned\u{7}"),
                parent_id: None,
            },
            now,
            Transport::Hook,
        );
        reducer.apply(
            &mut scene,
            AgentEvent::Waiting {
                agent_id: id,
                reason: "needs \u{1b}[2J approval".to_string(),
                tool_use_id: None,
            },
            now,
            Transport::Hook,
        );

        let out = summarize(&scene);
        assert!(
            !out.chars().any(|c| c.is_control()),
            "summary must carry no control chars (terminal-escape injection): {out:?}"
        );
        assert!(out.contains("repo]0;pwned"), "got: {out}");
        assert!(out.contains("needs [2J approval"), "got: {out}");
    }

    #[test]
    fn explicit_cap_clamps_to_layout_capacity_not_above() {
        let base = boot_capacities_for(classic(), term(192, 48));
        let layout_max = *base.iter().max().unwrap();
        assert_eq!(
            cap_boot_capacities(base, Some(layout_max + 100)),
            base,
            "cap above layout capacity must clamp down to the layout, not inflate"
        );
        assert!(cap_boot_capacities(base, Some(1)).iter().all(|&c| c <= 1));
        assert_eq!(cap_boot_capacities(base, None), base);
    }

    #[test]
    fn connected_sources_set_get_snapshot() {
        let cs = ConnectedSources::new(HashSet::from(["claude-code".to_string()]));
        assert!(cs.is_connected("claude-code"));
        assert!(!cs.is_connected("codex"));
        cs.set("codex", true);
        assert!(cs.is_connected("codex"));
        cs.set("claude-code", false);
        assert!(!cs.is_connected("claude-code"));
        assert_eq!(cs.snapshot(), HashSet::from(["codex".to_string()]));
    }
}

/// Armed quit arms catch a signal raised before they are first polled — the
/// window between arming and the loop that listens. The dispositions are
/// process-wide, which only nextest's process per test (justfile `test`) isolates.
#[cfg(all(test, unix))]
mod quit_arms {
    use super::{QUIT_SIGNALS, QuitArms, QuitSignal};

    /// A test never reads ambient state.
    fn default_dispositions() {
        for (kind, _) in QUIT_SIGNALS {
            // SAFETY: resetting a disposition before any arm exists.
            unsafe {
                libc::signal(kind.as_raw_value(), libc::SIG_DFL);
            }
        }
    }

    #[tokio::test]
    async fn a_signal_before_the_first_poll_is_caught_not_fatal() {
        default_dispositions();
        let arms = QuitArms::arm();
        for (kind, _) in QUIT_SIGNALS {
            // SAFETY: raising a signal this process handles from here on.
            unsafe {
                libc::raise(kind.as_raw_value());
            }
        }
        arms.signalled().await;
    }

    #[tokio::test]
    async fn any_one_signal_alone_is_a_quit_and_is_the_one_that_fired() {
        default_dispositions();
        for (kind, name) in QUIT_SIGNALS {
            let mut quit = Box::pin(QuitArms::arm().signalled());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut quit)
                    .await
                    .is_err(),
                "signalled() resolved with no signal raised"
            );
            // SAFETY: raising a signal this process handles from here on.
            unsafe {
                libc::raise(kind.as_raw_value());
            }
            let fired = tokio::time::timeout(std::time::Duration::from_secs(5), quit)
                .await
                .unwrap_or_else(|_| panic!("{name} did not quit"));
            assert_eq!(fired, QuitSignal(kind.as_raw_value()), "{name}");
        }
    }

    #[tokio::test]
    async fn a_signal_inherited_as_ignored_stays_ignored() {
        for (kind, name) in QUIT_SIGNALS {
            let raw = kind.as_raw_value();
            // SAFETY: process-wide disposition; the test raises only this signal.
            unsafe {
                libc::signal(raw, libc::SIG_IGN);
            }
            let mut quit = Box::pin(QuitArms::arm().signalled());
            // SAFETY: the signal is ignored.
            unsafe {
                libc::raise(raw);
            }
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(200), &mut quit)
                    .await
                    .is_err(),
                "an ignored {name} must not quit"
            );
        }
    }

    #[tokio::test]
    async fn a_registration_the_runtime_refuses_arms_nothing() {
        use tokio::signal::unix::SignalKind;
        assert!(super::arm_signal((SignalKind::from_raw(libc::SIGKILL), "SIGKILL")).is_none());
    }

    /// The kernel's own verdict on a re-raised signal, read from a child that
    /// raises it: a fork, because the raise ends the process. The child starts
    /// with the signal ignored, so only `reraise`'s own reset can end it.
    #[test]
    fn reraise_ends_the_process_by_the_signal() {
        for raw in [libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: the child only resets a disposition and raises, then
            // `_exit`s if the raise returned.
            let status = unsafe {
                let pid = libc::fork();
                assert!(pid >= 0, "fork");
                if pid == 0 {
                    libc::signal(raw, libc::SIG_IGN);
                    QuitSignal(raw).reraise();
                    libc::_exit(0);
                }
                let mut status = 0;
                assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
                status
            };
            assert!(
                libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == raw,
                "signal {raw}: wait status {status:#x}"
            );
        }
    }
}
