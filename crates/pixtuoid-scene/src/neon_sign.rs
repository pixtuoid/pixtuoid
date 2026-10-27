//! Backend-agnostic neon wall-board model — the SINGLE source of truth for the
//! office's "lit sign": brand + ★ CTA (L1), the mood pulse (L2: the tally and a
//! plain-English line, swapped by a split-flap roll), and the office-context row
//! (L3: uptime · floor · gateway chip), rendered by the TUI, the floating window,
//! and the wasm hero. It also owns `OfficeMood`, which the sign's light reads.
//!
//! `scene` has no terminal/window deps (invariant #1), so the model carries a
//! backend-agnostic `BoardTone` and [`BoardTone::rgb`] is the ONE tone→theme-role
//! map all three painters share. The activity tally it shows lives in
//! [`tally`](crate::tally).

use std::time::SystemTime;

use pixtuoid_core::SceneState;
use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::state::DaemonState;

use crate::display::{Content, Icon};
use crate::tally::{StateCounts, scene_stats};
use crate::theme::Theme;

/// The oldest in-scene agent's age in seconds — every agent still in the scene
/// (live or walking out; swept ones are gone).
pub(crate) fn scene_uptime_secs(scene: &SceneState, now: SystemTime) -> u64 {
    scene
        .agents
        .values()
        .filter_map(|a| now.duration_since(a.created_at).ok())
        .max()
        .unwrap_or_default()
        .as_secs()
}

/// Format a duration in seconds as a compact `"{h}h{m}m"` / `"{m}m"` / `"<1m"`
/// string, with no prefix.
pub(crate) fn compact_hms(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        "<1m".to_string()
    }
}

/// The board text's tone — backend-agnostic. Deliberately NOT
/// `badge::BadgeTone`: the variant sets are disjoint (labels never show
/// Brand/Star/Dim; the board never shows a per-agent Exiting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoardTone {
    /// L1 brand — `neon_brand`.
    Brand,
    /// L1 ★ Star CTA — `neon_star`.
    Star,
    /// A running count — `label_running`.
    Running,
    /// A requires-action "needs-you" count — `label_requires_action`.
    RequiresAction,
    /// A fault (a degraded or down gateway) — `label_warn`.
    Warn,
    /// An idle count — `label_idle`.
    Idle,
    /// Muted context/separator text, and a flap mid-roll — `tooltip_dim`.
    Dim,
}

impl BoardTone {
    /// This tone's theme color role — the SINGLE authority the three board
    /// painters share, so a `theme.ui` role change lands in ONE place.
    pub fn rgb(self, theme: &Theme) -> Rgb {
        match self {
            BoardTone::Brand => theme.ui.neon_brand,
            BoardTone::Star => theme.ui.neon_star,
            BoardTone::Running => theme.ui.label_running,
            BoardTone::RequiresAction => theme.ui.label_requires_action,
            BoardTone::Warn => theme.ui.label_warn,
            BoardTone::Idle => theme.ui.label_idle,
            BoardTone::Dim => theme.ui.tooltip_dim,
        }
    }
}

/// One tone-tagged stretch of the board: text, or an icon. The model bakes in
/// the inter-segment separators so no painter re-derives them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BoardSegment {
    pub content: Content,
    pub tone: BoardTone,
}

impl BoardSegment {
    fn new(text: impl Into<String>, tone: BoardTone) -> Self {
        Self {
            content: Content::Text(text.into()),
            tone,
        }
    }

    fn icon(icon: Icon, tone: BoardTone) -> Self {
        Self {
            content: Content::Icon(icon),
            tone,
        }
    }

    /// It as a terminal writes it: [`Content::text`].
    pub fn text(&self) -> &str {
        self.content.text()
    }
}

/// The cells `segs` take, end to end.
fn segs_cells(segs: &[BoardSegment]) -> usize {
    segs.iter()
        .map(|s| usize::from(crate::display::text::cells(s.text())))
        .sum()
}

/// The whole board, as tone-tagged segments — L1 `brand` + `star`, L2 `mood`,
/// L3 `context`. No baked padding between brand/star ([`BoardModel::runs`]
/// right-aligns the star); the mood + context separators ARE baked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BoardModel {
    pub brand: BoardSegment,
    pub star: Vec<BoardSegment>,
    pub mood: Vec<BoardSegment>,
    pub context: Vec<BoardSegment>,
}

/// The board's brand line. It carries NO version: the board is in every committed
/// still, so a version here made each release re-render all of them.
pub const BOARD_BRAND: &str = "pixtuoid";

/// The board's L1 star CTA: the star, then its word.
fn board_star() -> Vec<BoardSegment> {
    vec![
        BoardSegment::icon(Icon::Star, BoardTone::Star),
        BoardSegment::new(" Star", BoardTone::Star),
    ]
}

/// How long the requires-action lamp holds lit, then dark. A whole number of them
/// fits gen-media's hour grid, so a committed frame shows it lit.
const BLINK_HALF_MS: u64 = 600;
const _: () = assert!(crate::anim::HOUR_MS.is_multiple_of(2 * BLINK_HALF_MS));

/// Whether the requires-action lamp is lit `now_ms` into the beat.
fn lamp_lit(now_ms: u64) -> bool {
    (now_ms / BLINK_HALF_MS).is_multiple_of(2)
}

/// The lamp for agents in `tone`, the requires-action one dark while it blinks off.
fn lamp(tone: BoardTone, lit: bool) -> BoardSegment {
    let icon = match tone {
        BoardTone::RequiresAction => Icon::Alert,
        BoardTone::Running => Icon::Running,
        _ => Icon::Idle,
    };
    let tone = if tone == BoardTone::RequiresAction && !lit {
        BoardTone::Dim
    } else {
        tone
    };
    BoardSegment::icon(icon, tone)
}

/// The `⬢gw` chip's terse liveness word.
pub fn gateway_label(state: DaemonState) -> &'static str {
    match state {
        DaemonState::Idle => "ok",
        DaemonState::Running => "run",
        DaemonState::Degraded => "err",
        DaemonState::Down => "down",
    }
}

/// The gateway chip's tone — mirrors the footer's `FooterTone::Gateway` severity
/// map, but returns a plain `BoardTone` so `DaemonState` is only ever an INPUT to
/// the model and never leaks a color out of `scene`.
pub fn gateway_tone(state: DaemonState) -> BoardTone {
    match state {
        DaemonState::Idle => BoardTone::Idle,
        DaemonState::Running => BoardTone::Running,
        DaemonState::Degraded | DaemonState::Down => BoardTone::Warn,
    }
}

/// The office's mood — the ONE classification the tally's empty line, the persona
/// line and the sign's light all read, so the words and the glow can't disagree.
/// Exiting agents never count: a walkout isn't the mood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfficeMood {
    /// `requires_action` agents are blocked on the user — outranks everything.
    Alert {
        /// How many agents require action.
        requires_action: usize,
    },
    /// Nobody requires action and `running` agents are running.
    Running {
        /// How many agents are running.
        running: usize,
    },
    /// Everyone present is idle.
    Calm,
    /// Nobody is present.
    Empty,
}

impl OfficeMood {
    /// Classify `counts`.
    pub(crate) fn of(counts: StateCounts) -> Self {
        if counts.requires_action > 0 {
            Self::Alert {
                requires_action: counts.requires_action,
            }
        } else if counts.running > 0 {
            Self::Running {
                running: counts.running,
            }
        } else if counts.idle > 0 {
            Self::Calm
        } else {
            Self::Empty
        }
    }
}

/// The board's "mood pulse" tally — a lamp and a count per non-zero present
/// state, the requires-action lamp dark unless `lit`. Exiting agents are absent by
/// design: a walkout isn't the mood.
///
/// Its text is ASCII and each lamp one cell, so a roll keeps every column in
/// place (pinned by `every_l2_face_is_one_cell_per_char`).
pub(crate) fn board_mood_segments(counts: StateCounts, lit: bool) -> Vec<BoardSegment> {
    if OfficeMood::of(counts) == OfficeMood::Empty {
        return vec![BoardSegment::new(
            "\u{2014} office empty \u{2014}",
            BoardTone::Dim,
        )];
    }
    let build = |words: [&str; 3]| -> Vec<BoardSegment> {
        let rows = [
            (counts.requires_action, words[0], BoardTone::RequiresAction),
            (counts.running, words[1], BoardTone::Running),
            (counts.idle, words[2], BoardTone::Idle),
        ];
        let mut segs: Vec<BoardSegment> = Vec::new();
        for (n, word, tone) in rows {
            if n == 0 {
                continue;
            }
            if !segs.is_empty() {
                segs.push(BoardSegment::new("  ", BoardTone::Dim));
            }
            segs.push(lamp(tone, lit));
            segs.push(BoardSegment::new(format!("{n} {word}"), tone));
        }
        segs
    };
    let full = build(["req", "run", "idle"]);
    if segs_cells(&full) <= crate::layout::NEON_PANEL_INNER_W as usize {
        full
    } else {
        build(["rq", "rn", "id"])
    }
}

/// One persona line: fixed text, or the mood's agent count followed by text.
enum Persona {
    Says(&'static str),
    Counts(&'static str),
}

/// L2's plain-English pools. The `_ONE` pools carry no count, so one agent is
/// never pluralised.
const PERSONA_ALERT_ONE: &[Persona] = &[
    Persona::Says("someone needs you!"),
    Persona::Says("psst. your turn."),
];
const PERSONA_ALERT_MANY: &[Persona] = &[
    Persona::Counts("agents need you!"),
    Persona::Counts("require action..."),
];
const PERSONA_RUNNING_ONE: &[Persona] = &[
    Persona::Says("heads down, shipping"),
    Persona::Says("do not disturb :)"),
];
const PERSONA_RUNNING_MANY: &[Persona] = &[
    Persona::Says("heads down, shipping"),
    Persona::Counts("brains running"),
    Persona::Says("do not disturb :)"),
];
const PERSONA_CALM: &[Persona] = &[
    Persona::Says("quiet... too quiet"),
    Persona::Says("coffee break?"),
];
// `board_persona_segments` picks from a pool modulo its length.
const _: () = assert!(
    !PERSONA_ALERT_ONE.is_empty()
        && !PERSONA_ALERT_MANY.is_empty()
        && !PERSONA_RUNNING_ONE.is_empty()
        && !PERSONA_RUNNING_MANY.is_empty()
        && !PERSONA_CALM.is_empty()
);

/// L2's plain-English face for `mood`, after its lamp; `pick` rotates the
/// pool. Same 1-col vocabulary as the tally (see [`board_mood_segments`]).
/// `None` = L2 stays on the
/// tally: a line too wide for the panel (the tally abbreviates, this can't), and
/// an EMPTY office — its tally already reads in plain English, and the floating
/// window paints an empty office only as its beat turns (the binary's
/// `floating::cadence`), which would hold a roll mid-scramble on screen.
fn board_persona_segments(mood: OfficeMood, pick: u64, lit: bool) -> Option<Vec<BoardSegment>> {
    let (pool, n, tone) = match mood {
        OfficeMood::Alert { requires_action: 1 } => {
            (PERSONA_ALERT_ONE, 1, BoardTone::RequiresAction)
        }
        OfficeMood::Alert { requires_action } => (
            PERSONA_ALERT_MANY,
            requires_action,
            BoardTone::RequiresAction,
        ),
        OfficeMood::Running { running: 1 } => (PERSONA_RUNNING_ONE, 1, BoardTone::Running),
        OfficeMood::Running { running } => (PERSONA_RUNNING_MANY, running, BoardTone::Running),
        OfficeMood::Calm => (PERSONA_CALM, 0, BoardTone::Idle),
        OfficeMood::Empty => return None,
    };
    let text = match pool[(pick % pool.len() as u64) as usize] {
        Persona::Says(line) => format!(" {line}"),
        Persona::Counts(rest) => format!(" {n} {rest}"),
    };
    let segs = vec![lamp(tone, lit), BoardSegment::new(text, tone)];
    (segs_cells(&segs) <= crate::layout::NEON_PANEL_INNER_W as usize).then_some(segs)
}

/// L2 alternates in equal halves — tally, persona, tally, … — each HOLDING and
/// then rolling into the next so the roll ends exactly on the hand-over. An even
/// half is the tally, so a frame on gen-media's hour grid opens on it
/// (`no_committed_frame_catches_l2_mid_roll`).
const FLAP_HALF_MS: u64 = 4_000;
const _: () = assert!(crate::anim::HOUR_MS.is_multiple_of(2 * FLAP_HALF_MS));
/// A column's flap lands this long into a roll, plus [`FLAP_SETTLE_STEP_MS`] per
/// column to its left — so a roll settles left to right.
const FLAP_SETTLE_BASE_MS: u64 = 140;
const FLAP_SETTLE_STEP_MS: u64 = 24;
/// How long one drum glyph shows.
const FLAP_TICK_MS: u64 = 60;
/// The flap drum, in rolling order. ASCII, so a rolling line keeps the 1-col
/// vocabulary [`board_mood_segments`] depends on. Only text rolls: an icon's
/// column switches when it lands.
const FLAP_DRUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789#%&*+=<>/?";

/// When column `col` lands, in ms from the roll's start.
fn flap_settle_ms(col: usize) -> u64 {
    FLAP_SETTLE_BASE_MS + col as u64 * FLAP_SETTLE_STEP_MS
}

/// How long a `cols`-wide roll takes: until its LAST column lands.
fn flap_roll_ms(cols: usize) -> u64 {
    flap_settle_ms(cols.saturating_sub(1))
}

/// A column of a rolling line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flap {
    Char(char),
    Icon(Icon),
}

const BLANK: (Flap, BoardTone) = (Flap::Char(' '), BoardTone::Dim);

fn flap_cells(segs: &[BoardSegment]) -> Vec<(Flap, BoardTone)> {
    segs.iter()
        .flat_map(|s| {
            let cells: Vec<Flap> = match &s.content {
                Content::Text(text) => text.chars().map(Flap::Char).collect(),
                Content::Icon(icon) => vec![Flap::Icon(*icon)],
            };
            cells.into_iter().map(move |c| (c, s.tone))
        })
        .collect()
}

/// `from` rolling into `to`, `since_ms` into the roll. A real flap only rolls
/// FORWARD and stops ON its letter, so a column shows the drum glyphs leading up
/// to its target, never noise; a column that isn't changing doesn't move.
fn flap_roll(
    from: &[(Flap, BoardTone)],
    to: &[(Flap, BoardTone)],
    since_ms: u64,
) -> Vec<BoardSegment> {
    let drum_len = FLAP_DRUM.len() as u64;
    let blank = |c: (Flap, BoardTone)| c.0 == BLANK.0;
    let mut cells: Vec<(Flap, BoardTone)> = (0..from.len().max(to.len()))
        .map(|col| {
            let old = from.get(col).copied().unwrap_or(BLANK);
            let new = to.get(col).copied().unwrap_or(BLANK);
            let settle = flap_settle_ms(col);
            // A blank's tone is invisible, so blank-to-blank is "not changing" too.
            let unchanged = old == new || (blank(old) && blank(new));
            // An icon only re-inked takes its new tone at once, so the requires-action
            // lamp blinks through a roll.
            let reinked = matches!(old.0, Flap::Icon(_)) && old.0 == new.0;
            if since_ms >= settle || unchanged || reinked {
                return new;
            }
            let (Flap::Char(target), Flap::Char(_)) = (new.0, old.0) else {
                return old;
            };
            // A glyph the drum lacks (punctuation) rolls in from a per-column spot.
            let home = FLAP_DRUM
                .iter()
                .position(|&b| b as char == target.to_ascii_uppercase())
                .map_or(col as u64 % drum_len, |i| i as u64);
            let flips_left = (settle - since_ms).div_ceil(FLAP_TICK_MS);
            let idx = (home + drum_len - flips_left % drum_len) % drum_len;
            (Flap::Char(FLAP_DRUM[idx as usize] as char), BoardTone::Dim)
        })
        .collect();
    while cells.last().is_some_and(|&c| blank(c)) {
        cells.pop();
    }
    let mut segs: Vec<BoardSegment> = Vec::new();
    for (cell, tone) in cells {
        match (cell, segs.last_mut()) {
            (Flap::Icon(icon), _) => segs.push(BoardSegment::icon(icon, tone)),
            (
                Flap::Char(ch),
                Some(BoardSegment {
                    content: Content::Text(text),
                    tone: last,
                }),
            ) if *last == tone => text.push(ch),
            (Flap::Char(ch), _) => segs.push(BoardSegment::new(ch.to_string(), tone)),
        }
    }
    segs
}

/// L2 at `now_ms` — see [`FLAP_HALF_MS`] for the timeline.
fn board_mood_at(counts: StateCounts, now_ms: u64) -> Vec<BoardSegment> {
    let lit = lamp_lit(now_ms);
    let tally = board_mood_segments(counts, lit);
    let half = now_ms / FLAP_HALF_MS;
    let Some(persona) = board_persona_segments(OfficeMood::of(counts), half / 2, lit) else {
        return tally;
    };
    let (showing, next) = if half.is_multiple_of(2) {
        (tally, persona)
    } else {
        (persona, tally)
    };
    let roll_ms = flap_roll_ms(usize::max(segs_cells(&showing), segs_cells(&next)));
    match (now_ms % FLAP_HALF_MS + roll_ms).checked_sub(FLAP_HALF_MS) {
        Some(since_ms) => flap_roll(&flap_cells(&showing), &flap_cells(&next), since_ms),
        None => showing,
    }
}

/// Assemble the whole board model. `floor` is `(current, total_floors)` — a
/// single-floor office passes `None`; `gateway` is the [`gateway_rollup`](crate::tally::gateway_rollup), where
/// `None` suppresses the chip; `now` on `motion`'s beat drives L2's flap. The
/// context separators (`"  "`) are baked into each following segment so
/// painters just concatenate.
pub fn build_board(
    counts: StateCounts,
    uptime_secs: u64,
    floor: Option<(usize, usize)>,
    gateway: Option<DaemonState>,
    motion: crate::anim::Motion,
    now: SystemTime,
) -> BoardModel {
    let mut context = vec![BoardSegment::new(
        format!("up {}", compact_hms(uptime_secs)),
        BoardTone::Dim,
    )];
    if let Some((current, total)) = floor {
        context.push(BoardSegment::new(
            format!("  F{current}/{total}"),
            BoardTone::Dim,
        ));
    }
    if let Some(state) = gateway {
        let tone = gateway_tone(state);
        context.extend([
            BoardSegment::new("  ", tone),
            BoardSegment::icon(Icon::Gateway, tone),
            BoardSegment::new(format!("gw {}", gateway_label(state)), tone),
        ]);
    }
    BoardModel {
        brand: BoardSegment::new(BOARD_BRAND, BoardTone::Brand),
        star: board_star(),
        mood: board_mood_at(counts, motion.beat(now).ms()),
        context,
    }
}

/// The wall board every painter shows over `drawn`, the floor it draws: that
/// floor's tally and uptime, and what only the office knows — its `gateway`
/// ([`office_gateway`](crate::tally::office_gateway)) and `floor`'s place among its floors.
pub fn wall_board(
    drawn: &SceneState,
    gateway: Option<DaemonState>,
    floor: Option<crate::footer::FooterFloor>,
    motion: crate::anim::Motion,
    now: SystemTime,
) -> BoardModel {
    build_board(
        scene_stats(drawn),
        scene_uptime_secs(drawn, now),
        floor.map(|f| (f.current, f.total_floors)),
        gateway,
        motion,
        now,
    )
}

impl BoardModel {
    /// Its lines as the frame shows them, on the neon sign's dark interior: the
    /// brand at its top-left, the star flush to its right, then the mood and
    /// the context a cell row each.
    pub fn runs(&self, theme: &Theme) -> Vec<crate::display::TextRun> {
        use crate::display::{Align, TextRole, TextRun, TextSpan};
        use crate::layout::{
            CELL_ROWS, NEON_PANEL_INNER_W, NEON_PANEL_INNER_X, NEON_PANEL_INNER_Y,
        };
        let span = |s: &BoardSegment| TextSpan {
            content: s.content.clone(),
            ink: s.tone.rgb(theme),
        };
        let line = |n: u16| crate::layout::Point {
            x: NEON_PANEL_INNER_X,
            y: NEON_PANEL_INNER_Y + n * CELL_ROWS,
        };
        let run = |at, align, spans, role| TextRun {
            at,
            align,
            spans,
            plate: None,
            strip: None,
            role,
        };
        vec![
            run(
                line(0),
                Align::Left,
                vec![span(&self.brand)],
                TextRole::Brand,
            ),
            run(
                crate::layout::Point {
                    x: NEON_PANEL_INNER_X + NEON_PANEL_INNER_W,
                    ..line(0)
                },
                Align::Right,
                self.star.iter().map(span).collect(),
                TextRole::Star,
            ),
            run(
                line(1),
                Align::Left,
                self.mood.iter().map(span).collect(),
                TextRole::Board,
            ),
            run(
                line(2),
                Align::Left,
                self.context.iter().map(span).collect(),
                TextRole::Board,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Motion;

    /// Every face L2 can show — tally, persona, each drum glyph mid-roll — is
    /// one cell per char inside the sign's interior, or a roll shoves the row
    /// (`board_mood_segments`'s claim).
    #[test]
    fn every_l2_face_is_one_cell_per_char() {
        use crate::display::text::cells;
        const A_MINUTE_MS: u64 = 60_000;
        const FRAME_MS: usize = crate::anim::PAINT_FRAME_MS as usize;
        let offices = [(0, 0, 0), (0, 0, 3), (4, 0, 6), (4, 2, 6), (1, 1, 0)];
        for (running, requires_action, idle) in offices {
            let counts = StateCounts {
                running,
                requires_action,
                idle,
                exiting: 0,
                total: running + requires_action + idle,
            };
            for ms in (0..A_MINUTE_MS).step_by(FRAME_MS) {
                let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms);
                let model = build_board(counts, 0, None, None, Motion::Full, now);
                let l2: String = model.mood.iter().map(|s| s.text()).collect();
                let n = usize::from(cells(&l2));
                assert_eq!(n, l2.chars().count(), "{l2:?} at {ms}ms");
                assert!(
                    n <= usize::from(crate::layout::NEON_PANEL_INNER_W),
                    "{l2:?} at {ms}ms"
                );
            }
        }
    }

    fn mood_text(counts: StateCounts) -> String {
        board_mood_segments(counts, true)
            .into_iter()
            .map(|s| s.text().to_owned())
            .collect()
    }

    fn counts(running: usize, requires_action: usize, idle: usize) -> StateCounts {
        StateCounts {
            running,
            requires_action,
            idle,
            exiting: 0,
            total: running + requires_action + idle,
        }
    }

    fn text_of(segs: &[BoardSegment]) -> String {
        segs.iter().map(|s| s.text()).collect()
    }

    fn persona_of(c: StateCounts) -> Vec<BoardSegment> {
        board_persona_segments(OfficeMood::of(c), 0, true).expect("this office has a persona line")
    }

    /// How long the roll between `c`'s tally and its first persona line takes.
    fn roll_ms(c: StateCounts) -> u64 {
        let tally = text_of(&board_mood_segments(c, true)).chars().count();
        let persona = text_of(&persona_of(c)).chars().count();
        flap_roll_ms(tally.max(persona))
    }

    /// The capture instants `scripts/media.json` NAMES, in ms past the hour grid
    /// gen-media's clocks sit on: a still's own hour, a clip's `poster` second, a
    /// wasm still's `t0_ms + advance_ms`. Read from the manifest so a new poster
    /// time is covered the day it lands; `None` on a crates.io-packaged run, which
    /// ships no `scripts/`.
    fn committed_frame_offsets_ms() -> Option<Vec<u64>> {
        const MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/media.json");
        let raw = std::fs::read_to_string(MANIFEST).ok()?;
        let jobs: serde_json::Value = serde_json::from_str(&raw).expect("media.json is JSON");
        let mut offsets = vec![0];
        for job in jobs.as_array().expect("media.json is an array") {
            let base = job["t0_ms"].as_u64().unwrap_or(0) % crate::anim::HOUR_MS;
            if let Some(poster_secs) = job["poster"].as_f64() {
                offsets.push(base + (poster_secs * 1000.0) as u64);
            }
            if let Some(advance_ms) = job["advance_ms"].as_u64() {
                offsets.push(base + advance_ms);
            }
        }
        assert!(
            offsets.len() > 1,
            "the manifest still names poster/advance times"
        );
        Some(offsets)
    }

    #[test]
    fn the_brand_carries_no_version_so_a_release_cannot_drift_committed_media() {
        assert_eq!(BOARD_BRAND, "pixtuoid");
        assert!(!BOARD_BRAND.contains(env!("CARGO_PKG_VERSION")));
        let b = build_board(
            counts(1, 0, 0),
            0,
            None,
            None,
            Motion::Full,
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(b.brand.text(), BOARD_BRAND);
    }

    #[test]
    fn mood_is_alert_over_running_over_calm_over_empty() {
        assert_eq!(
            OfficeMood::of(counts(3, 2, 5)),
            OfficeMood::Alert { requires_action: 2 }
        );
        assert_eq!(
            OfficeMood::of(counts(3, 0, 5)),
            OfficeMood::Running { running: 3 }
        );
        assert_eq!(OfficeMood::of(counts(0, 0, 5)), OfficeMood::Calm);
        assert_eq!(OfficeMood::of(counts(0, 0, 0)), OfficeMood::Empty);
        let walkout_only = StateCounts {
            exiting: 2,
            total: 2,
            ..StateCounts::default()
        };
        assert_eq!(
            OfficeMood::of(walkout_only),
            OfficeMood::Empty,
            "a walkout isn't the mood"
        );
    }

    #[test]
    fn l2_holds_the_tally_then_the_persona_and_rolls_only_between_them() {
        let c = counts(4, 2, 6);
        let mood = OfficeMood::of(c);
        // Each face as `ms` shows its requires-action lamp, which blinks underneath.
        let tally = |ms| board_mood_segments(c, lamp_lit(ms));
        let persona = |ms| board_persona_segments(mood, 0, lamp_lit(ms)).expect("fits");
        assert_ne!(tally(0), persona(0));
        let at = |ms| board_mood_at(c, ms);
        let roll_starts = FLAP_HALF_MS - roll_ms(c);
        assert_eq!(at(0), tally(0), "an even half OPENS on the settled tally");
        let held = roll_starts - 1;
        assert_eq!(at(held), tally(held), "held until the roll");
        let rolling = roll_starts + FLAP_TICK_MS;
        assert_ne!(at(rolling), tally(rolling), "rolling");
        let last = FLAP_HALF_MS - 1;
        assert_ne!(at(last), persona(last), "last column still rolling");
        assert_eq!(
            at(FLAP_HALF_MS),
            persona(FLAP_HALF_MS),
            "the roll ENDS on the hand-over"
        );
        let held = FLAP_HALF_MS + roll_starts - 1;
        assert_eq!(at(held), persona(held), "held until the roll");
        let last = 2 * FLAP_HALF_MS - 1;
        assert_ne!(at(last), tally(last), "last column still rolling back");
        assert_eq!(
            at(2 * FLAP_HALF_MS),
            tally(2 * FLAP_HALF_MS),
            "and the next pair opens settled"
        );
    }

    #[test]
    fn the_persona_pool_rotates_once_per_pair_of_halves() {
        let c = counts(4, 0, 6);
        let shown = |pair: u64| board_mood_at(c, (2 * pair + 1) * FLAP_HALF_MS);
        assert_eq!(
            shown(0),
            board_persona_segments(OfficeMood::of(c), 0, true).unwrap()
        );
        assert_eq!(
            shown(1),
            board_persona_segments(OfficeMood::of(c), 1, true).unwrap()
        );
        assert_ne!(shown(0), shown(1));
    }

    #[test]
    fn a_roll_settles_left_to_right_in_dim_and_never_outgrows_the_panel() {
        let c = counts(4, 2, 6);
        let persona = text_of(&persona_of(c));
        let mid = FLAP_HALF_MS - roll_ms(c) / 2;
        let cells: Vec<(char, BoardTone)> = board_mood_at(c, mid)
            .iter()
            .flat_map(|s| s.text().chars().map(move |ch| (ch, s.tone)))
            .collect();
        let settled = persona
            .chars()
            .zip(cells.iter())
            .take_while(|(want, (got, _))| want == got)
            .count();
        assert!(
            settled > 0 && settled < persona.chars().count(),
            "mid-roll the LEFT has landed and the right has not: {settled}"
        );
        let first_rolling = cells[settled];
        assert_eq!(first_rolling.1, BoardTone::Dim, "a rolling flap is dim");
        assert!(
            FLAP_DRUM.contains(&(first_rolling.0 as u8)),
            "a rolling flap shows a drum glyph: {first_rolling:?}"
        );
        for ms in (0..2 * FLAP_HALF_MS).step_by(FLAP_TICK_MS as usize / 2) {
            let w = text_of(&board_mood_at(c, ms)).chars().count();
            assert!(
                w <= crate::layout::NEON_PANEL_INNER_W as usize,
                "{w} cols at {ms}ms"
            );
        }
    }

    #[test]
    fn the_last_flip_before_a_letter_lands_is_its_drum_predecessor() {
        let c = counts(0, 0, 3);
        let persona = text_of(&persona_of(c));
        let (col, target) = persona
            .chars()
            .enumerate()
            .find(|(_, ch)| ch.is_ascii_alphabetic())
            .expect("a persona line has a letter");
        let just_before = FLAP_HALF_MS - roll_ms(c) + flap_settle_ms(col) - 1;
        let shown = text_of(&board_mood_at(c, just_before))
            .chars()
            .nth(col)
            .expect("the column exists mid-roll");
        let home = FLAP_DRUM
            .iter()
            .position(|&b| b as char == target.to_ascii_uppercase())
            .expect("letters are on the drum");
        let predecessor = FLAP_DRUM[(home + FLAP_DRUM.len() - 1) % FLAP_DRUM.len()] as char;
        assert_eq!(shown, predecessor, "{target:?} lands after {predecessor:?}");
    }

    #[test]
    fn a_column_blank_on_both_sides_stays_blank_through_the_roll() {
        let c = counts(4, 2, 6);
        let tally = text_of(&board_mood_segments(c, true));
        let persona = text_of(&persona_of(c));
        let cell = |text: &str, col: usize| text.chars().nth(col).unwrap_or(' ');
        let cols = tally.chars().count().max(persona.chars().count());
        // Not the last column: a roll's trailing blanks are trimmed, which would
        // pass this vacuously.
        let col = (0..cols - 1)
            .find(|&col| cell(&tally, col) == ' ' && cell(&persona, col) == ' ')
            .expect("the fixture pair shares an interior blank column");
        let while_it_would_roll = FLAP_HALF_MS - roll_ms(c) + flap_settle_ms(col) / 2;
        let rolling = text_of(&board_mood_at(c, while_it_would_roll));
        assert_eq!(rolling.chars().nth(col), Some(' '));
    }

    #[test]
    fn a_persona_line_fits_the_panel_or_is_withheld() {
        for n in [1usize, 2, 999, usize::MAX] {
            for mood in [
                OfficeMood::Alert { requires_action: n },
                OfficeMood::Running { running: n },
                OfficeMood::Calm,
            ] {
                for pick in 0..8 {
                    let Some(line) = board_persona_segments(mood, pick, true) else {
                        continue;
                    };
                    let line = text_of(&line);
                    assert!(
                        line.chars().count() <= crate::layout::NEON_PANEL_INNER_W as usize,
                        "{mood:?}/{pick}: {line:?}"
                    );
                }
            }
        }
        assert!(board_persona_segments(OfficeMood::Running { running: 999 }, 1, true).is_some());
        let absurd = OfficeMood::Alert {
            requires_action: usize::MAX,
        };
        assert_eq!(
            board_persona_segments(absurd, 0, true),
            None,
            "withheld, not clipped"
        );
    }

    #[test]
    fn an_office_without_a_persona_never_leaves_the_tally() {
        let absurd = StateCounts {
            requires_action: usize::MAX,
            total: usize::MAX,
            ..StateCounts::default()
        };
        for c in [StateCounts::default(), absurd] {
            for ms in (0..2 * FLAP_HALF_MS).step_by(FLAP_TICK_MS as usize) {
                let tally = board_mood_segments(c, lamp_lit(ms));
                assert_eq!(board_mood_at(c, ms), tally, "{ms}ms");
            }
        }
    }

    #[test]
    fn a_counted_persona_line_reads_glyph_count_text() {
        let line = board_persona_segments(OfficeMood::Alert { requires_action: 3 }, 0, true)
            .expect("fits");
        assert_eq!(text_of(&line), "\u{25b2} 3 agents need you!");
        let line =
            board_persona_segments(OfficeMood::Running { running: 12 }, 1, true).expect("fits");
        assert_eq!(text_of(&line), "\u{25cf} 12 brains running");
    }

    #[test]
    fn one_agent_is_never_pluralised() {
        for pick in 0..8 {
            for mood in [
                OfficeMood::Alert { requires_action: 1 },
                OfficeMood::Running { running: 1 },
            ] {
                let line = text_of(&board_persona_segments(mood, pick, true).expect("fits"));
                assert!(!line.contains("1 "), "{mood:?}/{pick}: {line:?}");
            }
        }
    }

    #[test]
    fn no_committed_frame_catches_l2_mid_roll() {
        let Some(offsets) = committed_frame_offsets_ms() else {
            eprintln!("skipping: scripts/media.json not present (packaged build)");
            return;
        };
        let widest_roll = flap_roll_ms(crate::layout::NEON_PANEL_INNER_W as usize);
        for offset in offsets {
            let into_half = offset % FLAP_HALF_MS;
            assert!(
                into_half + widest_roll < FLAP_HALF_MS,
                "a frame at +{offset}ms lands {into_half}ms into a half — inside the roll"
            );
        }
        assert_eq!(
            board_mood_at(counts(4, 2, 6), 0),
            board_mood_segments(counts(4, 2, 6), true),
            "and a frame ON the hour grid shows the tally"
        );
    }

    #[test]
    fn uptime_is_the_oldest_in_scene_agent_in_whole_seconds() {
        use pixtuoid_core::state::{ActivityState, GlobalDeskIndex};
        use pixtuoid_core::{AgentId, AgentSlot};
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::time::Duration;
        fn slot(secs: u64) -> AgentSlot {
            AgentSlot {
                agent_id: AgentId::from_transcript_path(&format!("/p/{secs}.jsonl")),
                source: Arc::from("claude-code"),
                session_id: Arc::from("s"),
                cwd: Arc::from(PathBuf::from("/p").as_path()),
                label: "l".into(),
                state: ActivityState::Idle,
                state_started_at: SystemTime::UNIX_EPOCH,
                last_event_at: SystemTime::UNIX_EPOCH,
                created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
                exiting_at: None,
                desk_index: GlobalDeskIndex(0),
                floor_idx: 0,
                tool_call_count: 0,
                running_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            }
        }
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let mut scene = SceneState::uniform(16);
        for s in [slot(40), slot(10)] {
            scene.agents.insert(s.agent_id, s);
        }
        assert_eq!(scene_uptime_secs(&scene, now), 90);
        assert_eq!(scene_uptime_secs(&SceneState::uniform(16), now), 0);
    }

    #[test]
    fn mood_echoes_counts_with_requires_action_beacon_leading() {
        let c = StateCounts {
            running: 3,
            requires_action: 2,
            idle: 5,
            exiting: 1,
            total: 11,
        };
        let segs = board_mood_segments(c, true);
        let text = mood_text(c);
        assert!(
            text.contains("\u{25b2}2 req"),
            "requires-action beacon: {text}"
        );
        assert!(text.contains("\u{25cf}3 run"), "running: {text}");
        assert!(text.contains("\u{25cb}5 idle"), "idle: {text}");
        assert!(!text.contains("exit"), "no exiting on the mood: {text}");
        let w = text.find('\u{25b2}').unwrap();
        let a = text.find('\u{25cf}').unwrap();
        assert!(w < a, "requires-action leads running: {text}");
        let tone_of = |glyph: char| {
            segs.iter()
                .find(|s| s.text().starts_with(glyph))
                .map(|s| s.tone)
        };
        assert_eq!(tone_of('\u{25b2}'), Some(BoardTone::RequiresAction));
        assert_eq!(tone_of('\u{25cf}'), Some(BoardTone::Running));
        assert_eq!(tone_of('\u{25cb}'), Some(BoardTone::Idle));
    }

    /// A lamp re-inked mid-roll shows its new tone at once: a roll never
    /// holds a blink.
    #[test]
    fn a_lamp_blinks_through_a_roll() {
        let rolled = flap_roll(
            &[(Flap::Icon(Icon::Alert), BoardTone::RequiresAction)],
            &[(Flap::Icon(Icon::Alert), BoardTone::Dim)],
            0,
        );
        assert_eq!(rolled, [BoardSegment::icon(Icon::Alert, BoardTone::Dim)]);
    }

    /// The requires-action lamp blinks, dim while it is off; the others hold.
    #[test]
    fn the_requires_action_lamp_blinks() {
        let c = counts(1, 1, 1);
        let lamps = |ms| -> Vec<BoardTone> {
            board_mood_at(c, ms)
                .into_iter()
                .filter(|s| matches!(s.content, Content::Icon(_)))
                .map(|s| s.tone)
                .collect()
        };
        use BoardTone::{Dim, Idle, RequiresAction, Running};
        assert_eq!(lamps(0), [RequiresAction, Running, Idle]);
        assert_eq!(lamps(BLINK_HALF_MS), [Dim, Running, Idle]);
        assert_eq!(lamps(2 * BLINK_HALF_MS), [RequiresAction, Running, Idle]);
    }

    #[test]
    fn mood_calm_office_drops_the_beacon() {
        let c = StateCounts {
            running: 1,
            requires_action: 0,
            idle: 2,
            exiting: 0,
            total: 3,
        };
        let text = mood_text(c);
        assert!(
            !text.contains('\u{25b2}'),
            "no beacon when nobody waits: {text}"
        );
        assert!(text.contains("\u{25cf}1 run") && text.contains("\u{25cb}2 idle"));
    }

    #[test]
    fn mood_empty_office_reads_plainly_and_is_dim() {
        let segs = board_mood_segments(StateCounts::default(), true);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text(), "\u{2014} office empty \u{2014}");
        assert_eq!(segs[0].tone, BoardTone::Dim);
    }

    #[test]
    fn mood_big_office_abbreviates_to_fit_the_panel_interior() {
        let c = StateCounts {
            running: 150,
            requires_action: 150,
            idle: 150,
            exiting: 0,
            total: 450,
        };
        let text = mood_text(c);
        let width = text.chars().count();
        assert!(
            width <= crate::layout::NEON_PANEL_INNER_W as usize,
            "fits the panel interior: {text} = {width}"
        );
        assert!(text.contains("\u{25b2}150 rq"), "abbreviated big-N: {text}");
    }

    #[test]
    fn gateway_tone_mirrors_the_footer_severity_map() {
        assert_eq!(gateway_tone(DaemonState::Idle), BoardTone::Idle);
        assert_eq!(gateway_tone(DaemonState::Running), BoardTone::Running);
        assert_eq!(gateway_tone(DaemonState::Degraded), BoardTone::Warn);
        assert_eq!(gateway_tone(DaemonState::Down), BoardTone::Warn);
        assert_eq!(gateway_label(DaemonState::Idle), "ok");
        assert_eq!(gateway_label(DaemonState::Down), "down");
        let theme = crate::theme::theme_by_name("normal").expect("normal theme");
        for st in [
            DaemonState::Idle,
            DaemonState::Running,
            DaemonState::Degraded,
            DaemonState::Down,
        ] {
            assert_eq!(
                crate::footer::FooterTone::Gateway(st).rgb(theme),
                gateway_tone(st).rgb(theme),
                "board and footer must resolve the same gateway color for {st:?}"
            );
        }
    }

    #[test]
    fn build_board_assembles_all_three_rows() {
        let c = StateCounts {
            running: 2,
            requires_action: 0,
            idle: 1,
            exiting: 0,
            total: 3,
        };
        let b = build_board(c, 3661, None, None, Motion::Full, SystemTime::UNIX_EPOCH);
        assert_eq!(b.brand.tone, BoardTone::Brand);
        assert_eq!(
            b.mood,
            board_mood_segments(c, true),
            "the epoch is a whole hour"
        );
        assert_eq!(text_of(&b.star), "\u{2605} Star");
        assert!(b.star.iter().all(|s| s.tone == BoardTone::Star));
        assert_eq!(b.context.len(), 1);
        assert_eq!(b.context[0].text(), "up 1h1m");
        assert_eq!(b.context[0].tone, BoardTone::Dim);

        let b2 = build_board(
            c,
            30,
            Some((2, 3)),
            Some(DaemonState::Running),
            Motion::Full,
            SystemTime::UNIX_EPOCH,
        );
        let ctx: String = b2.context.iter().map(BoardSegment::text).collect();
        assert_eq!(ctx, "up <1m  F2/3  \u{2b22}gw run");
        let chip = b2.context.last().unwrap();
        assert_eq!(chip.tone, BoardTone::Running, "running gateway chip tone");
    }
}
