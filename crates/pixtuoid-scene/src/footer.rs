//! Backend-agnostic status-footer model.
//!
//! The SINGLE source of truth for the office's bottom status line. `scene` has no
//! terminal/window deps (invariant #1), so the model carries a backend-agnostic
//! [`FooterTone`] and [`FooterTone::rgb`] is the ONE tone→theme-role map both
//! painters share — each only converts the resolved `Rgb` to its own surface color
//! type, so the hues can't drift across surfaces.
//!
//! [`build_footer`] owns the WHOLE tier/priority policy in one place, and is PURE:
//! every scene read happens in [`FooterInputs::new`] and [`FooterContext::new`].

use std::collections::HashMap;

use pixtuoid_core::SceneState;
use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::state::{ActivityState, DaemonState, MAX_FLOORS, ToolKind, ToolUse};

use crate::neon_sign::gateway_label;
use crate::tally::StateCounts;
use crate::theme::Theme;

/// The four agent activity buckets as a shared vocabulary — each carries
/// redundant glyph/letter/word channels so hue is never the sole carrier
/// (survives colour removal, colour-blindness, a tofu'd glyph).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RungKind {
    Running,
    RequiresAction,
    Idle,
    Exiting,
}

impl RungKind {
    /// Canonical render order (the footer's left-to-right rung order).
    pub const ALL: [RungKind; 4] = [
        RungKind::Running,
        RungKind::RequiresAction,
        RungKind::Idle,
        RungKind::Exiting,
    ];

    /// A distinct geometric glyph per state — all East-Asian *ambiguous* width
    /// (1 cell in a non-CJK terminal).
    pub fn glyph(self) -> char {
        match self {
            RungKind::Running => '\u{25cf}',
            RungKind::RequiresAction => '\u{25d0}',
            RungKind::Idle => '\u{25cb}',
            RungKind::Exiting => '\u{25cc}',
        }
    }

    /// A distinct single letter — the primary colour-blind channel at the
    /// footer's narrow tier where the full word doesn't fit.
    pub fn letter(self) -> char {
        match self {
            RungKind::Running => 'R',
            RungKind::RequiresAction => 'A',
            RungKind::Idle => 'I',
            RungKind::Exiting => 'x',
        }
    }

    /// The full capitalized state word — the tooltip dossier's state line reads
    /// `{glyph} {word}`.
    pub fn word(self) -> &'static str {
        match self {
            RungKind::Running => "Running",
            RungKind::RequiresAction => "Requires action",
            RungKind::Idle => "Idle",
            RungKind::Exiting => "Exiting",
        }
    }

    /// The count for this state, so a consumer can just iterate [`RungKind::ALL`].
    pub fn count(self, counts: StateCounts) -> usize {
        match self {
            RungKind::Running => counts.running,
            RungKind::RequiresAction => counts.requires_action,
            RungKind::Idle => counts.idle,
            RungKind::Exiting => counts.exiting,
        }
    }
}

/// A footer segment's tone — backend-agnostic; each painter maps it to its own
/// color via [`FooterTone::rgb`]. Deliberately NOT [`crate::neon_sign::BoardTone`]:
/// the variant sets are disjoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterTone {
    /// Labels, separators, counts, padding, the `♩`/floor/keys suffix — muted.
    Neutral,
    /// An activity-state rung — hue by the shared [`RungKind`] vocabulary.
    Rung(RungKind),
    /// A tool tally segment — hue from the TYPED [`ToolKind`], the same
    /// monitor-glow colour the sprite shows, NEVER a re-parse of the name.
    Tool(ToolKind),
    /// The gateway `⬢gw` chip — hue by daemon liveness.
    Gateway(DaemonState),
    /// Source-death / decode-drift warning — uses the `label_warn` alarm hue.
    Warning,
}

impl FooterTone {
    /// This tone's theme color role — the SINGLE authority both footer painters
    /// share, so a `theme.ui` role change lands in ONE place and the surfaces
    /// can't drift.
    pub fn rgb(self, theme: &Theme) -> Rgb {
        match self {
            FooterTone::Neutral => theme.ui.label_idle,
            FooterTone::Rung(RungKind::Running) => theme.ui.label_running,
            FooterTone::Rung(RungKind::RequiresAction) => theme.ui.label_requires_action,
            FooterTone::Rung(RungKind::Idle) => theme.ui.label_idle,
            FooterTone::Rung(RungKind::Exiting) => theme.ui.label_exiting,
            FooterTone::Tool(kind) => theme.tool_glow.for_kind(kind),
            FooterTone::Gateway(DaemonState::Idle) => theme.ui.label_idle,
            FooterTone::Gateway(DaemonState::Running) => theme.ui.label_running,
            FooterTone::Gateway(DaemonState::Degraded | DaemonState::Down) => theme.ui.label_warn,
            FooterTone::Warning => theme.ui.label_warn,
        }
    }
}

/// One tone-tagged text run. The model bakes in the separators, the right-flush
/// padding, and the suffix, so a painter just lays the runs left-to-right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterSegment {
    pub text: String,
    pub tone: FooterTone,
}

impl FooterSegment {
    fn new(text: impl Into<String>, tone: FooterTone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// The whole footer for one frame — a flat, ordered, tone-tagged run list already
/// right-flushed to `budget` columns, so both painters right-flush identically
/// without re-implementing the fit/pad math.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterModel {
    pub segments: Vec<FooterSegment>,
}

impl FooterModel {
    /// The concatenated plain text — the oracle snapshot and substring asserts
    /// lock the exact footer wording through.
    pub fn text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// The footer as one row of screen text, each segment in its tone.
    pub fn line(&self, theme: &Theme) -> crate::display::cells::CellGrid {
        let text = self.text();
        let mut grid = crate::display::cells::CellGrid::new(crate::display::text::cells(&text), 1);
        self.segments.iter().fold(0, |x, seg| {
            grid.put((x, 0), &seg.text, Some(seg.tone.rgb(theme)), false)
        });
        grid
    }
}

/// The floor breadcrumb inputs — `current`/`total_floors` drive the `F{c}/{t}`
/// badge, `total_agents` the `n/total` slash. A single-floor office passes `None`:
/// bare count, no breadcrumb.
#[derive(Debug, Clone, Copy)]
pub struct FooterFloor {
    pub current: usize,
    pub total_floors: usize,
    pub total_agents: usize,
}

const TOOL_TALLY_CAP: usize = 4;

/// One aggregate tool-tally entry: the raw display `token` (kept verbatim), the
/// TYPED [`ToolKind`] for the hue, and how many Running slots show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolTally {
    pub token: String,
    pub kind: ToolKind,
    pub count: usize,
}

/// The aggregate tool tally: group Running slots by their raw display token (the
/// first alphanumeric run of the detail, kept verbatim) but carry the TYPED
/// [`ToolKind`] for the hue — a Task slot displays "Delegating" yet tints via
/// `kind = Task`, never the name. Sorted by count desc then name, capped at
/// [`TOOL_TALLY_CAP`].
pub(crate) fn footer_tool_tally(scene: &SceneState) -> Vec<ToolTally> {
    let mut tool_counts: HashMap<String, (ToolKind, usize)> = HashMap::new();
    for slot in scene.agents.values() {
        if let ActivityState::Running {
            item: Some(ToolUse { detail, kind, .. }),
        } = &slot.state
            && let Some(token) = detail
                .as_deref()
                .and_then(|d| d.split(|c: char| !c.is_alphanumeric()).next())
                .filter(|t| !t.is_empty())
        {
            tool_counts.entry(token.to_string()).or_insert((*kind, 0)).1 += 1;
        }
    }
    let mut tools: Vec<ToolTally> = tool_counts
        .into_iter()
        .map(|(token, (kind, count))| ToolTally { token, kind, count })
        .collect();
    tools.sort_by(|a, b| b.count.cmp(&a.count).then(a.token.cmp(&b.token)));
    tools.truncate(TOOL_TALLY_CAP);
    tools
}

/// The per-frame inputs `build_footer` renders: `counts` and `tools` come from
/// the drawn floor's scene, everything else from the [`FooterContext`].
#[derive(Debug)]
pub struct FooterInputs<'a> {
    counts: StateCounts,
    tools: Vec<ToolTally>,
    pub context: FooterContext<'a>,
}

impl<'a> FooterInputs<'a> {
    /// The footer over `drawn`, the (projected) floor scene the painter draws.
    pub fn new(drawn: &SceneState, context: FooterContext<'a>) -> Self {
        Self {
            counts: crate::tally::scene_stats(drawn),
            tools: footer_tool_tally(drawn),
            context,
        }
    }
}

/// Every footer input the drawn floor's scene can't supply; `per_floor` and
/// `gateway` are office-wide.
#[derive(Debug, Clone, Copy)]
pub struct FooterContext<'a> {
    pub per_floor: [StateCounts; MAX_FLOORS],
    pub gateway: Option<DaemonState>,
    pub floor: Option<FooterFloor>,
    /// "You would hear sound right now": audio live AND not effectively muted
    /// (m-state OR pause).
    pub(crate) audio_audible: bool,
    /// Transient +/- readout: `Some(percent)` just after a volume nudge —
    /// renders as `♩ N%`.
    pub(crate) volume_flash: Option<u8>,
    /// Pre-merged one-line death>drift warning; `None` while healthy.
    pub(crate) source_warning: Option<&'a str>,
    /// The stats-tier right keybind tail.
    pub(crate) keys_stats: &'a str,
    /// The alert-tier right keybind tail.
    pub(crate) keys_alert: &'a str,
}

impl<'a> FooterContext<'a> {
    /// The context over `office`, the FULL scene — never a projected floor.
    pub fn new(
        office: &SceneState,
        floor: Option<FooterFloor>,
        audio_audible: bool,
        volume_flash: Option<u8>,
        source_warning: Option<&'a str>,
        keys_stats: &'a str,
        keys_alert: &'a str,
    ) -> Self {
        Self {
            per_floor: crate::tally::per_floor_counts(office),
            gateway: crate::tally::office_gateway(office),
            floor,
            audio_audible,
            volume_flash,
            source_warning,
            keys_stats,
            keys_alert,
        }
    }
}

/// Column width of a footer string. The footer's own glyph vocabulary is ALL
/// single-column (ambiguous EAW = 1 in a non-CJK terminal), so
/// [`cells`](crate::display::text::cells) equals `chars().count()` here. **Accepted
/// residual**: the ONE variable-content field is the tool-tally TOKEN, and a
/// hypothetical wide-CJK token would count short of its display width and nudge
/// the right-flush by the excess.
fn cols(s: &str) -> usize {
    s.chars().count()
}

/// Clip `s` to at most `budget` columns — no ellipsis, which would read as
/// content rather than chrome.
fn clip_cols(s: &str, budget: usize) -> String {
    s.chars().take(budget).collect()
}

/// Assemble the footer for one frame — the deep builder that owns the entire
/// tier/priority policy. `budget` is the caller's column budget. Returns the
/// chosen tier already right-flushed to `budget`, so a painter renders the runs
/// with zero policy.
///
/// Tiers: **death** (preempts all; the `▲N need you` alarm stays PINNED through
/// body truncation) → **full** → **medium** → **minimal** → **fallback** (only
/// the keybind tail).
pub fn build_footer(inputs: &FooterInputs<'_>, budget: u16) -> FooterModel {
    let counts = inputs.counts;
    let ctx = &inputs.context;
    // A dead source outranks the stats: the counts go stale once a transport is
    // gone, so the warning IS the status until restart — truncated to fit rather
    // than tiered away.
    if let Some(warn) = ctx.source_warning {
        let w = budget as usize;
        // Clipped, never dropped: a row with no `[q]` is a user stuck in the
        // alternate screen.
        let quit = clip_cols(ctx.keys_alert, w);
        let avail = w.saturating_sub(cols(&quit));
        let alarm = if counts.requires_action > 0 {
            format!(" · \u{25b2}{} need you", counts.requires_action)
        } else {
            String::new()
        };
        let prefix = " \u{26a0} ";
        let suffix = " ";
        let full = format!("{prefix}{warn}{alarm}{suffix}");
        let text = if cols(&full) <= avail {
            full
        } else {
            let chrome = cols(prefix) + cols(suffix) + cols(&alarm);
            let body_budget = avail.saturating_sub(chrome);
            if body_budget >= 1 {
                let mut body: String = warn.chars().take(body_budget.saturating_sub(1)).collect();
                body.push('\u{2026}');
                format!("{prefix}{body}{alarm}{suffix}")
            } else if avail == 0 {
                String::new()
            } else {
                format!("{}\u{2026}", clip_cols(&full, avail - 1))
            }
        };
        let pad = w.saturating_sub(cols(&text) + cols(&quit));
        let mut out = Vec::new();
        if !text.is_empty() {
            out.push(FooterSegment::new(text, FooterTone::Warning));
        }
        if pad > 0 {
            out.push(FooterSegment::new(" ".repeat(pad), FooterTone::Neutral));
        }
        out.push(FooterSegment::new(quit, FooterTone::Neutral));
        return FooterModel { segments: out };
    }

    let count_str = match ctx.floor {
        Some(fi) => format!("{}/{}", counts.total, fi.total_agents),
        None => format!("{}", counts.total),
    };

    // The cross-floor `▲F{n}` cue: any OTHER floor holding a requires-action agent.
    let cross_floor = ctx.floor.and_then(|fi| {
        let cur = fi.current.saturating_sub(1);
        (0..MAX_FLOORS)
            .find(|&fl| fl != cur && ctx.per_floor[fl].requires_action > 0)
            .map(|fl| fl + 1)
    });
    let floor_suffix = match ctx.floor {
        Some(fi) => {
            let cross = match cross_floor {
                Some(n) => format!(" \u{25b2}F{n}"),
                None => String::new(),
            };
            format!(
                " F{}/{}{cross} [\u{2191}\u{2193}]",
                fi.current, fi.total_floors
            )
        }
        None => String::new(),
    };
    let audio_glyph = match (ctx.audio_audible, ctx.volume_flash) {
        (true, Some(pct)) => format!(" \u{2669} {pct}%"),
        (true, None) => " \u{2669}".to_string(),
        (false, _) => String::new(),
    };
    let quit = format!("{audio_glyph}{floor_suffix}{}", ctx.keys_stats);

    // The board owns the friendly "— office empty —"; here it's a bare count.
    if counts.total == 0 {
        return fit_tiers(
            [vec![FooterSegment::new(
                format!(" {count_str} "),
                FooterTone::Neutral,
            )]],
            &quit,
            ctx.keys_alert,
            budget,
        );
    }

    let seg_full = {
        let mut segs = vec![FooterSegment::new(
            format!(" {count_str}"),
            FooterTone::Neutral,
        )];
        for kind in RungKind::ALL {
            let c = kind.count(counts);
            if c == 0 {
                continue;
            }
            segs.push(FooterSegment::new(" · ".to_string(), FooterTone::Neutral));
            segs.push(FooterSegment::new(
                format!("{}{} {}", kind.glyph(), c, kind.letter()),
                FooterTone::Rung(kind),
            ));
        }
        if !inputs.tools.is_empty() {
            segs.push(FooterSegment::new(" · ".to_string(), FooterTone::Neutral));
            for (i, t) in inputs.tools.iter().enumerate() {
                if i > 0 {
                    segs.push(FooterSegment::new(" ".to_string(), FooterTone::Neutral));
                }
                segs.push(FooterSegment::new(
                    format!("{}\u{d7}{}", t.token, t.count),
                    FooterTone::Tool(t.kind),
                ));
            }
        }
        if let Some(g) = ctx.gateway {
            segs.push(FooterSegment::new(" · ".to_string(), FooterTone::Neutral));
            segs.push(FooterSegment::new(
                format!(
                    "{}gw {}",
                    crate::display::Icon::Gateway.terminal(),
                    gateway_label(g)
                ),
                FooterTone::Gateway(g),
            ));
        }
        segs.push(FooterSegment::new(" ".to_string(), FooterTone::Neutral));
        segs
    };

    // Medium: the exiting rung, the tools and the chip all drop out for width.
    let seg_medium = {
        let mut rungs: Vec<FooterSegment> = Vec::new();
        for kind in [RungKind::Running, RungKind::RequiresAction, RungKind::Idle] {
            let c = kind.count(counts);
            if c == 0 {
                continue;
            }
            if !rungs.is_empty() {
                rungs.push(FooterSegment::new(" ".to_string(), FooterTone::Neutral));
            }
            rungs.push(FooterSegment::new(
                format!("{}{}{}", kind.glyph(), c, kind.letter()),
                FooterTone::Rung(kind),
            ));
        }
        let mut segs = vec![FooterSegment::new(
            format!(" {count_str} \u{b7} "),
            FooterTone::Neutral,
        )];
        segs.extend(rungs);
        segs.push(FooterSegment::new(" ".to_string(), FooterTone::Neutral));
        segs
    };

    // Minimal: the requires-action alarm LEADS (the last stat to survive), then count.
    let seg_min = if counts.requires_action > 0 {
        vec![
            FooterSegment::new(
                format!(" \u{25b2}{}", counts.requires_action),
                FooterTone::Rung(RungKind::RequiresAction),
            ),
            FooterSegment::new(format!(" \u{b7} {count_str} "), FooterTone::Neutral),
        ]
    } else {
        vec![FooterSegment::new(
            format!(" {count_str} "),
            FooterTone::Neutral,
        )]
    };

    fit_tiers(
        [seg_full, seg_medium, seg_min],
        &quit,
        ctx.keys_alert,
        budget,
    )
}

/// Right-flush the widest tier that fits `budget`, else fall to the keys-only
/// rung. The fallback degrades the TAIL whole — full hints, then the alert tail,
/// then a clip — rather than clipping mid-token and losing `[q]uit` with it.
fn fit_tiers(
    tiers: impl IntoIterator<Item = Vec<FooterSegment>>,
    quit: &str,
    keys_alert: &str,
    budget: u16,
) -> FooterModel {
    for tier in tiers {
        let stats_len: usize = tier.iter().map(|s| cols(&s.text)).sum();
        if stats_len + cols(quit) <= budget as usize {
            return finish_tier(tier, quit, budget);
        }
    }
    let tail = [quit, keys_alert]
        .into_iter()
        .find(|t| cols(t) <= budget as usize)
        .map_or_else(|| clip_cols(keys_alert, budget as usize), str::to_string);
    finish_tier(Vec::new(), &tail, budget)
}

/// Right-flush a chosen tier: pad the gap so the keybind suffix sits at the exact
/// `budget` edge.
fn finish_tier(mut tier: Vec<FooterSegment>, quit: &str, budget: u16) -> FooterModel {
    let stats_len: usize = tier.iter().map(|s| cols(&s.text)).sum();
    let pad = (budget as usize).saturating_sub(stats_len + cols(quit));
    if pad > 0 {
        tier.push(FooterSegment::new(" ".repeat(pad), FooterTone::Neutral));
    }
    tier.push(FooterSegment::new(quit.to_string(), FooterTone::Neutral));
    FooterModel { segments: tier }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::state::GlobalDeskIndex;
    use pixtuoid_core::{AgentId, AgentSlot};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::SystemTime;

    const KEYS_STATS: &str = " [?]help [p]ause [t]heme [q]uit ";
    const KEYS_ALERT: &str = " [q]uit ";

    fn running_slot(id: &str, detail: &str, kind: ToolKind) -> AgentSlot {
        AgentSlot {
            agent_id: AgentId::from_transcript_path(id),
            source: Arc::from("cc"),
            session_id: Arc::from("s"),
            cwd: Arc::from(PathBuf::from("/p").as_path()),
            label: "l".into(),
            state: ActivityState::Running {
                item: Some(ToolUse {
                    tool_use_id: Some(Arc::from("t")),
                    detail: Some(Arc::from(detail)),
                    kind,
                }),
            },
            state_started_at: SystemTime::UNIX_EPOCH,
            created_at: SystemTime::UNIX_EPOCH,
            last_event_at: SystemTime::UNIX_EPOCH,
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

    fn requires_action_slot(id: &str) -> AgentSlot {
        let mut s = running_slot(id, "x", ToolKind::Other);
        s.state = ActivityState::RequiresAction {
            reason: Some(Arc::from("permission")),
        };
        s
    }

    fn inputs<'a>(
        scene: &SceneState,
        tools: &[ToolTally],
        audio_audible: bool,
        volume_flash: Option<u8>,
        source_warning: Option<&'a str>,
    ) -> FooterInputs<'a> {
        FooterInputs {
            counts: crate::tally::scene_stats(scene),
            tools: tools.to_vec(),
            context: FooterContext {
                per_floor: crate::tally::per_floor_counts(scene),
                gateway: None,
                floor: None,
                audio_audible,
                volume_flash,
                source_warning,
                keys_stats: KEYS_STATS,
                keys_alert: KEYS_ALERT,
            },
        }
    }

    #[test]
    fn tool_tally_skips_empty_leading_token() {
        let mut scene = SceneState::uniform(16);
        // Leading '/' ⇒ first token after split-on-non-alphanumeric is "".
        let slot = running_slot("/p/lead.jsonl", "/usr/bin/thing", ToolKind::Other);
        scene.agents.insert(slot.agent_id, slot);
        let tools = footer_tool_tally(&scene);
        assert!(
            tools.is_empty(),
            "empty leading token yields no tool: {tools:?}"
        );
        let line = build_footer(&inputs(&scene, &tools, false, None, None), 200).text();
        assert!(!line.contains('\u{00d7}'), "no × tool count: {line}");
        assert!(
            line.contains("\u{25cf}1 R"),
            "running rung still shows: {line}"
        );
    }

    #[test]
    fn audio_suffix_tracks_audibility_and_the_volume_flash() {
        let scene = SceneState::uniform(16);
        let go =
            |audible, flash| build_footer(&inputs(&scene, &[], audible, flash, None), 200).text();
        assert!(!go(false, None).contains('\u{2669}'), "muted shows no note");
        let line = go(true, None);
        assert!(line.contains('\u{2669}'), "audible shows ♩: {line}");
        assert!(!line.contains('%'), "no percent outside the flash: {line}");
        assert!(
            go(true, Some(65)).contains("\u{2669} 65%"),
            "the flash appends the percent"
        );
        assert!(
            !go(false, Some(65)).contains('\u{2669}'),
            "muted never shows ♩"
        );
    }

    #[test]
    fn full_tier_fills_the_width_in_columns_with_multibyte_glyphs() {
        let mut scene = SceneState::uniform(16);
        let slot = running_slot("/p/mb.jsonl", "Bash ls", ToolKind::Bash);
        scene.agents.insert(slot.agent_id, slot);
        let tools = footer_tool_tally(&scene);
        let width: u16 = 200;
        let model = build_footer(&inputs(&scene, &tools, false, None, None), width);
        let cols_sum: usize = model.segments.iter().map(|s| cols(&s.text)).sum();
        assert_eq!(cols_sum, width as usize, "fills full width: {model:?}");
        assert!(
            model.segments.iter().any(|s| s.text.contains('\u{00d7}')),
            "full tier with the tool breakdown: {model:?}"
        );
    }

    #[test]
    fn death_tier_pins_the_requires_action_alarm_through_the_narrowest_width() {
        let mut scene = SceneState::uniform(16);
        let slot = requires_action_slot("/p/wait.jsonl");
        scene.agents.insert(slot.agent_id, slot);
        let warn = "transport pixtuoid-hook died: connection refused after 3 retries";
        let line = build_footer(&inputs(&scene, &[], false, None, Some(warn)), 40).text();
        assert!(
            line.contains("\u{25b2}1 need you"),
            "the ▲N alarm survives body truncation: {line}"
        );
        assert!(
            line.contains('\u{2026}'),
            "the warning body IS truncated at this width (proving the alarm was pinned): {line}"
        );
    }

    #[test]
    fn empty_office_is_a_bare_count() {
        let scene = SceneState::uniform(16);
        let line = build_footer(&inputs(&scene, &[], false, None, None), 200).text();
        assert!(line.contains(" 0 "), "bare zero count: {line}");
        assert!(
            !line.contains('\u{25cf}'),
            "no rungs in an empty office: {line}"
        );
    }

    #[test]
    fn every_footer_path_is_exactly_budget_wide() {
        let mut office = SceneState::uniform(16);
        let slot = requires_action_slot("/p/wait.jsonl");
        office.agents.insert(slot.agent_id, slot);
        let tools = footer_tool_tally(&office);
        let empty = SceneState::uniform(16);
        for w in 0..=64u16 {
            for (label, model) in [
                (
                    "stats",
                    build_footer(&inputs(&office, &tools, false, None, None), w),
                ),
                (
                    "empty",
                    build_footer(&inputs(&empty, &[], false, None, None), w),
                ),
                (
                    "death",
                    build_footer(
                        &inputs(&office, &[], false, None, Some("transport died")),
                        w,
                    ),
                ),
            ] {
                let got: usize = model.segments.iter().map(|s| cols(&s.text)).sum();
                assert_eq!(
                    got,
                    w as usize,
                    "{label} tier at budget {w}: {:?}",
                    model.text()
                );
            }
        }
    }

    #[test]
    fn the_keys_only_rung_degrades_by_tail_not_by_clipping() {
        let mut scene = SceneState::uniform(16);
        let slot = running_slot("/p/a.jsonl", "Edit x", ToolKind::Edit);
        scene.agents.insert(slot.agent_id, slot);
        // Wide enough for the full hint tail, one column too narrow for the
        // minimal stats tier — so the fallback rung is what renders.
        let wide = build_footer(&inputs(&scene, &[], false, None, None), 34).text();
        assert_eq!(cols(&wide), 34, "right-flushed: {wide:?}");
        assert!(
            wide.ends_with(KEYS_STATS),
            "keeps the full hint tail: {wide:?}"
        );
        // Below the hint tail's own width the ALERT tail takes over, whole.
        let narrow = build_footer(&inputs(&scene, &[], false, None, None), 20).text();
        assert_eq!(cols(&narrow), 20, "right-flushed: {narrow:?}");
        assert!(
            narrow.ends_with(KEYS_ALERT),
            "quit survives whole: {narrow:?}"
        );
        assert!(
            !narrow.contains("[t]"),
            "no dangling half-token: {narrow:?}"
        );
    }

    #[test]
    fn context_derives_the_office_wide_halves_from_the_full_scene() {
        use pixtuoid_core::state::{DaemonInstanceId, DaemonLiveness, DaemonPresence};
        let mut scene = SceneState::uniform(16);
        let mut upstairs = requires_action_slot("/p/up.jsonl");
        upstairs.floor_idx = 1;
        scene.agents.insert(upstairs.agent_id, upstairs);
        scene.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            DaemonInstanceId::new("18789").expect("non-empty"),
            DaemonPresence {
                liveness: DaemonLiveness::Down,
                active_sessions: 0,
                last_seen: SystemTime::UNIX_EPOCH,
                entered_at: SystemTime::UNIX_EPOCH,
                in_flight_turns: Default::default(),
                current_pid: None,
            },
        );
        let ctx = FooterContext::new(&scene, None, false, None, None, KEYS_STATS, KEYS_ALERT);
        assert_eq!(
            ctx.per_floor[1].requires_action, 1,
            "the upstairs agent counts"
        );
        assert_eq!(ctx.gateway, Some(DaemonState::Down));
    }
}
