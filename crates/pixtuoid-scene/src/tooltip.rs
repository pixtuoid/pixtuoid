//! What a hovered thing's tooltip says, for every painter: tone-tagged rows
//! that [`Tooltip::card`] lays out and colours into one cell grid. A painter
//! owns only placing and drawing it.

use std::time::SystemTime;

use pixtuoid_core::source::registry::descriptor_for;
use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::state::{ActivityState, ToolKind};
use pixtuoid_core::{AgentId, SceneState};

use crate::display::cells::{CellGrid, CellRect};
use crate::display::text::cells;
use crate::display::{GatewayCard, HoverTarget, PetHover};
use crate::floor::FloorInputs;
use crate::footer::{FooterTone, RungKind};
use crate::hit::SceneHit;
use crate::pet::PetKind;
use crate::theme::Theme;

/// The longest tool detail or requires-action reason a tooltip shows, in chars.
pub const DETAIL_CHARS: usize = 34;

/// How a tooltip run draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipTone {
    /// The tooltip's subject, drawn bold.
    Title,
    Text,
    Dim,
    /// The painter's own text colour.
    Plain,
    /// An agent's state word.
    Rung(RungKind),
    /// The tool a running agent runs.
    Tool(ToolKind),
    /// A source's `[xx]` badge, in that source's hue.
    Source(&'static str),
}

impl TipTone {
    /// This tone's theme colour, the one authority [`Tooltip::card`] colours
    /// through; `None` for [`Self::Plain`].
    pub fn rgb(self, theme: &Theme) -> Option<Rgb> {
        Some(match self {
            Self::Title => theme.ui.tooltip_title,
            Self::Text => theme.ui.tooltip_text,
            Self::Dim => theme.ui.tooltip_dim,
            Self::Plain => return None,
            Self::Rung(kind) => FooterTone::Rung(kind).rgb(theme),
            Self::Tool(kind) => theme.tool_glow.for_kind(kind),
            Self::Source(tag) => theme.source_hue(tag),
        })
    }
}

/// One tone-tagged text run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TipSpan {
    pub(crate) text: String,
    pub(crate) tone: TipTone,
}

impl TipSpan {
    fn new(text: impl Into<String>, tone: TipTone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// One row of a tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TipRow {
    /// Runs laid left to right.
    Spans(Vec<TipSpan>),
    /// `left`, then `right` flush to the box's right edge.
    Heading { left: Vec<TipSpan>, right: TipSpan },
    /// A rule across the box.
    Rule,
}

/// Which side of the pointer a tooltip opens on first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipAnchor {
    /// Below, flipping above when there is no room: an agent's card.
    Below,
    /// Above, flipping below when there is no room: a one-line label.
    Above,
}

/// A tooltip: its rows, top to bottom, and where it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tooltip {
    pub(crate) rows: Vec<TipRow>,
    pub anchor: TipAnchor,
}

impl Tooltip {
    /// A one-line label; `text` carries its own padding.
    pub fn label(text: &str) -> Self {
        Self {
            rows: vec![TipRow::Spans(vec![TipSpan::new(text, TipTone::Title)])],
            anchor: TipAnchor::Above,
        }
    }

    /// The card every painter draws: the rows on the tooltip's fill inside a
    /// cell of padding, a heading's right run flush to the widest body row,
    /// a rule across, the title bold. An agent's card is at least
    /// [`MIN_CARD_W`] wide.
    pub fn card(&self, theme: &Theme) -> CellGrid {
        let width = |spans: &[TipSpan]| spans.iter().map(|s| cells(&s.text)).sum::<u16>();
        let body_w = self
            .rows
            .iter()
            .filter_map(|row| match row {
                TipRow::Spans(spans) => Some(width(spans)),
                TipRow::Heading { .. } | TipRow::Rule => None,
            })
            .max()
            .unwrap_or(0);
        let content_w = self.rows.iter().fold(body_w, |w, row| match row {
            TipRow::Heading { left, right } => {
                w.max(width(left) + HEADING_GAP + cells(&right.text))
            }
            TipRow::Spans(_) | TipRow::Rule => w,
        });
        let min_w = match self.anchor {
            TipAnchor::Below => MIN_CARD_W,
            TipAnchor::Above => 0,
        };
        let rows = u16::try_from(self.rows.len()).unwrap_or(u16::MAX);
        let mut grid = CellGrid::new(
            (content_w + 2 * CARD_PAD).max(min_w),
            rows.saturating_add(2 * CARD_PAD),
        );
        grid.fill(theme.ui.tooltip_bg);
        let put = |grid: &mut CellGrid, at: (u16, u16), s: &TipSpan| {
            grid.put(at, &s.text, s.tone.rgb(theme), s.tone == TipTone::Title)
        };
        for (y, row) in (CARD_PAD..).zip(&self.rows) {
            match row {
                TipRow::Spans(spans) => {
                    spans
                        .iter()
                        .fold(CARD_PAD, |x, s| put(&mut grid, (x, y), s));
                }
                TipRow::Heading { left, right } => {
                    left.iter().fold(CARD_PAD, |x, s| put(&mut grid, (x, y), s));
                    put(
                        &mut grid,
                        (CARD_PAD + content_w - cells(&right.text), y),
                        right,
                    );
                }
                TipRow::Rule => {
                    let rule = RULE.repeat(usize::from(content_w));
                    grid.put((CARD_PAD, y), &rule, Some(theme.ui.tooltip_dim), false);
                }
            }
        }
        grid
    }
}

/// Cells of padding around a card's rows.
const CARD_PAD: u16 = 1;
/// The fewest cells between a heading's two runs.
const HEADING_GAP: u16 = 2;
/// The narrowest an agent's card is, in cells.
pub(crate) const MIN_CARD_W: u16 = 20;
/// The line a [`TipRow::Rule`] draws, a cell at a time.
const RULE: &str = "\u{2500}";

/// Where a card of `card` cells opens by the pointer at `pointer` inside
/// `area`, all in cells: an agent's card (at least `MIN_CARD_W` wide) below
/// the pointer, flipping above when it would cross the bottom; a label
/// above, flipping below when it would cross the top; either just right of
/// the pointer, flipping left when it would cross the right edge. The card
/// is at most `area` big; a painter clips what still overflows.
pub fn place(card: CellRect, pointer: (u16, u16), area: CellRect, anchor: TipAnchor) -> CellRect {
    let (mx, my) = pointer;
    let h = card.h.min(area.h);
    let (w, y) = match anchor {
        TipAnchor::Below => {
            let below = my.saturating_add(1);
            let y = if below.saturating_add(h) > area.y + area.h {
                my.saturating_sub(h).max(area.y)
            } else {
                below
            };
            (card.w.min(area.w).max(MIN_CARD_W), y)
        }
        TipAnchor::Above => {
            let y = if my < area.y + h {
                my.saturating_add(1)
            } else {
                my.saturating_sub(h)
            };
            (card.w.min(area.w), y)
        }
    };
    let right = mx.saturating_add(POINTER_GAP);
    let x = if right.saturating_add(w) > area.x + area.w {
        mx.saturating_sub(w + 1)
    } else {
        right
    };
    CellRect { x, y, w, h }
}

/// Cells from the pointer to a card opening right of it.
const POINTER_GAP: u16 = 2;

/// The tooltip for what `hit` names on `world`'s frame; `None` for the star,
/// whose click is its whole story, and for an agent or gateway gone from the
/// scene.
pub fn for_hit(hit: SceneHit<'_>, world: &FloorInputs<'_>) -> Option<Tooltip> {
    match hit {
        SceneHit::Figure(HoverTarget::Agent(id)) => agent(world.scene, *id, world.now),
        SceneHit::Figure(&HoverTarget::Pet(PetHover { kind, anim, .. })) => {
            let on_cooldown = world.pets.petting.is_some_and(|p| p.is_active(world.now));
            // The hover was drawn from `pets.pet`, so the kinds agree and the
            // `default_name` arm is not a live path.
            let name = world
                .pets
                .pet
                .map_or_else(|| kind.default_name(), |p| p.name.as_str());
            Some(pet(kind, anim, on_cooldown, name))
        }
        SceneHit::Figure(HoverTarget::Mascot(key)) => {
            GatewayCard::of(world.scene, key).map(|card| Tooltip::label(&mascot_text(&card)))
        }
        SceneHit::Coffee => Some(coffee()),
        SceneHit::Furniture(label) => Some(Tooltip::label(&format!(" {label} "))),
        SceneHit::Star => None,
    }
}

/// An agent's card: its badge, label and session id, then its state and
/// tool, what it works on, under whom, where, on which model, and its
/// session's stats. An exiting agent shows none of the live affordances: the
/// walking-out slot keeps its Running/RequiresAction payload, so the exiting-first
/// kind gates them, not the raw state.
pub fn agent(scene: &SceneState, id: AgentId, now: SystemTime) -> Option<Tooltip> {
    let agent = scene.agents.get(&id)?;
    let kind = if agent.exiting_at.is_some() {
        RungKind::Exiting
    } else {
        match agent.state {
            ActivityState::Running { .. } => RungKind::Running,
            ActivityState::RequiresAction { .. } => RungKind::RequiresAction,
            ActivityState::Idle => RungKind::Idle,
        }
    };
    let mut state = vec![TipSpan::new(
        format!("{} {}", kind.glyph(), kind.word()),
        TipTone::Rung(kind),
    )];
    let mut detail: Option<String> = None;
    if kind != RungKind::Exiting {
        if let ActivityState::Running {
            detail: Some(d),
            kind: tool_kind,
            ..
        } = &agent.state
            && !d.is_empty()
        {
            let (tool, rest) = d
                .split_once(char::is_whitespace)
                .map(|(t, r)| (t.trim_end_matches(':'), r.trim()))
                .unwrap_or((d.trim_end_matches(':'), ""));
            if !tool.is_empty() {
                state.push(TipSpan::new(" \u{b7} ", TipTone::Plain));
                // The hue comes from the TYPED kind, never a re-parse of the name.
                state.push(TipSpan::new(tool, TipTone::Tool(*tool_kind)));
            }
            if !rest.is_empty() {
                detail = Some(rest.chars().take(DETAIL_CHARS).collect());
            }
        } else if let ActivityState::RequiresAction {
            reason: Some(reason),
        } = &agent.state
        {
            detail = Some(format!(
                "?{}",
                reason.chars().take(DETAIL_CHARS).collect::<String>()
            ));
        }
    }
    let dim = |text: String| TipRow::Spans(vec![TipSpan::new(text, TipTone::Dim)]);
    let mut rows = vec![
        TipRow::Heading {
            left: vec![
                TipSpan::new(
                    format!(
                        "[{:<2}]",
                        descriptor_for(agent.source.as_ref()).map_or("??", |d| d.label_prefix)
                    ),
                    TipTone::Source(
                        descriptor_for(agent.source.as_ref()).map_or("??", |d| d.label_prefix),
                    ),
                ),
                TipSpan::new(format!(" {}", agent.label), TipTone::Title),
            ],
            right: TipSpan::new(
                format!("\u{b7}{}", crate::badge::disambig_suffix(&agent.session_id)),
                TipTone::Dim,
            ),
        },
        TipRow::Rule,
        TipRow::Spans(state),
    ];
    if let Some(d) = detail {
        rows.push(TipRow::Spans(vec![TipSpan::new(
            format!("  {d}"),
            TipTone::Text,
        )]));
    }
    if let Some(parent) = agent.parent_id.and_then(|p| scene.agents.get(&p)) {
        rows.push(dim(format!("\u{21b3} under {}", parent.label)));
    }
    rows.push(dim(format!("\u{25a4} {}", short_cwd(&agent.cwd))));
    // The effort is suffixed only while FRESH — the same burn-TTL the flame
    // reads, so the text can't outlive the fire.
    if let Some(model) = agent.model.as_deref() {
        let mut row = format!("\u{2605} {model}");
        if let Some(effort) = crate::burn::fresh_effort(agent, now) {
            row.push_str(&format!(" \u{b7} {effort}"));
        }
        rows.push(dim(row));
    }
    let session_secs = now
        .duration_since(agent.created_at)
        .unwrap_or_default()
        .as_secs();
    let mut stats = format!(
        "\u{25f7} {} \u{b7} {} calls",
        crate::neon_sign::compact_hms(session_secs),
        agent.tool_call_count
    );
    // Skipped at zero so sources with no usage wire keep their card unchanged.
    if agent.tokens_used > 0 {
        stats.push_str(&format!(
            " \u{b7} \u{3a3} {} tok",
            crate::token_meter::compact_tokens(agent.tokens_used)
        ));
    }
    // Fresh agents show no running-% meter — the % is noise before ~5s of accounting.
    if kind == RungKind::Running && session_secs >= 5 {
        let pct = ((agent.running_ms / 1000) * 100 / session_secs).min(100);
        let filled = (pct as usize * 5).div_ceil(100).min(5);
        let meter: String = "\u{25ae}".repeat(filled) + &"\u{25af}".repeat(5 - filled);
        stats.push_str(&format!(" \u{b7} {meter} {pct}%"));
    }
    rows.push(dim(stats));
    Some(Tooltip {
        rows,
        anchor: TipAnchor::Below,
    })
}

/// A short form of a cwd path: the TAIL (most informative — project dir) with a
/// leading `…`. Char-sliced, never a byte slice, so a multibyte path can't panic.
fn short_cwd(cwd: &std::path::Path) -> String {
    const MAX: usize = 30;
    let s = cwd.to_string_lossy();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= MAX {
        s.into_owned()
    } else {
        format!(
            "\u{2026}{}",
            chars[chars.len() - (MAX - 1)..].iter().collect::<String>()
        )
    }
}

/// The coffee machine's label.
pub fn coffee() -> Tooltip {
    Tooltip::label(" \u{2615} Buy Ivan a coffee ")
}

/// A pet's label: its reaction while a petting plays, else what it is doing,
/// else its name.
pub fn pet(
    kind: PetKind,
    anim: pixtuoid_core::sprite::format::Piece,
    on_cooldown: bool,
    name: &str,
) -> Tooltip {
    let idle = format!(" {name} ");
    let text: &str = if on_cooldown {
        match kind {
            PetKind::Cat => " purr... ",
            PetKind::Dog => " woof! ",
        }
    } else if anim == kind.sleep_anim() {
        " Shhh... sleeping "
    } else if anim == kind.sit_anim() {
        " Pet me! "
    } else {
        &idle
    };
    Tooltip::label(text)
}

/// The gateway mascot's label. The verb keys on `running` — see
/// [`GatewayCard::running`] for why the turn state, not the session count — and
/// `degraded` outranks running/idle.
pub fn mascot_text(card: &GatewayCard) -> String {
    let &GatewayCard {
        name,
        ref instance,
        running,
        degraded,
        active_sessions,
    } = card;
    // `OpenClaw:19789` — `instance` is set only when there IS a sibling to tell
    // apart, so the single-gateway label stays byte-unchanged.
    let name = match instance {
        Some(i) => format!("{name}:{i}"),
        None => name.to_string(),
    };
    let verb = if degraded {
        "model error"
    } else if running {
        "running"
    } else {
        "idle"
    };
    if active_sessions > 1 {
        format!(" {name} gateway \u{b7} {verb} \u{b7} {active_sessions} sessions ")
    } else {
        format!(" {name} gateway \u{b7} {verb} ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, tone: TipTone) -> TipSpan {
        TipSpan::new(text, tone)
    }

    fn row(card: &CellGrid, y: u16) -> String {
        (0..card.width())
            .filter_map(|x| card.get(x, y).map(|c| c.symbol.clone()))
            .collect()
    }

    /// The card pads its rows a cell all round on the tooltip's fill, flushes
    /// a heading's right run to the widest row and draws a rule across.
    #[test]
    fn a_card_lays_its_rows_out_in_padded_cells() {
        let theme = crate::theme::ALL_THEMES[0];
        let tip = Tooltip {
            rows: vec![
                TipRow::Heading {
                    left: vec![span("ab", TipTone::Title)],
                    right: span("z", TipTone::Dim),
                },
                TipRow::Rule,
                TipRow::Spans(vec![span("0123456789", TipTone::Text)]),
            ],
            anchor: TipAnchor::Above,
        };
        let card = tip.card(theme);
        assert_eq!((card.width(), card.height()), (12, 5));
        assert_eq!(row(&card, 1), " ab       z ");
        assert_eq!(row(&card, 2), format!(" {} ", "\u{2500}".repeat(10)));
        assert_eq!(row(&card, 3), " 0123456789 ");
        assert!((0..card.height()).all(|y| {
            (0..card.width())
                .all(|x| card.get(x, y).and_then(|c| c.bg) == Some(theme.ui.tooltip_bg))
        }));
        assert!(card.get(1, 1).is_some_and(|c| c.bold));
    }

    /// An agent's card is at least [`MIN_CARD_W`] wide; a label is its text.
    #[test]
    fn an_agents_card_is_never_narrower_than_its_minimum() {
        let theme = crate::theme::ALL_THEMES[0];
        let label = Tooltip::label("hi");
        assert_eq!(label.card(theme).width(), 4);
        let card = Tooltip {
            anchor: TipAnchor::Below,
            ..label
        };
        assert_eq!(card.card(theme).width(), MIN_CARD_W);
    }

    /// A card opens right of the pointer below (a label above) and flips
    /// where it would cross the area's edge.
    #[test]
    fn a_card_flips_off_the_areas_edges() {
        let area = CellRect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        let card = CellRect {
            w: 20,
            h: 5,
            ..CellRect::default()
        };
        let at = |pointer, anchor| place(card, pointer, area, anchor);
        assert_eq!(
            at((10, 5), TipAnchor::Below),
            CellRect {
                x: 12,
                y: 6,
                w: 20,
                h: 5
            }
        );
        assert_eq!(at((10, 22), TipAnchor::Below).y, 17);
        assert_eq!(at((70, 5), TipAnchor::Below).x, 49);
        assert_eq!(at((10, 10), TipAnchor::Above).y, 5);
        assert_eq!(at((10, 2), TipAnchor::Above).y, 3);
    }
}
