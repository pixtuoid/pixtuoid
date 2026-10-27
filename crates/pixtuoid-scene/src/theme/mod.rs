mod catppuccin;
mod cyberpunk;
mod dracula;
mod gruvbox;
mod normal;
mod tokyo_night;

use pixtuoid_core::sprite::Rgb;

pub(crate) use catppuccin::CATPPUCCIN;
pub use cyberpunk::CYBERPUNK;
pub(crate) use dracula::DRACULA;
pub(crate) use gruvbox::GRUVBOX;
pub use normal::NORMAL;
pub(crate) use tokyo_night::TOKYO_NIGHT;

/// Light vs Dark classification — drives effects that only look right on
/// one or the other (e.g. ceiling halos read as soft glow on dark themes
/// but as dirt smears on light themes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeKind {
    /// A light-background theme.
    Light,
    /// A dark-background theme.
    Dark,
}

/// A complete office color palette: nine role groups plus name/kind.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Theme id — the `--theme` value and the registry lookup key.
    pub name: &'static str,
    /// Light vs dark classification.
    pub(crate) kind: ThemeKind,
    /// Room-shell colors (walls, carpet, window frame).
    pub surface: SurfaceColors,
    /// Office-fixture colors (cubicles, runner, skyline, clock, shadow).
    pub office: OfficeColors,
    /// Sky, sun/moon, and interior-light colors.
    pub lighting: LightingColors,
    /// Furniture colors (desks, rugs, aquarium, token-meter paper).
    pub(crate) furniture: FurnitureColors,
    /// Transient-effect cue colors (glow, sleep z, steam, dust, bubble).
    pub(crate) effects: EffectColors,
    /// Name-badge, tooltip, and neon-brand UI colors.
    pub ui: UiColors,
    /// Per-tool monitor-glow hues.
    pub tool_glow: ToolGlowColors,
    /// Corridor-appliance colors (vending, printer, coat rack).
    pub(crate) appliance: ApplianceColors,
    /// Per-CLI agent-badge hues.
    pub source: SourceColors,
}

impl Theme {
    /// A source's hue by its 2-char label prefix, the idle label's for an
    /// unknown one.
    pub fn source_hue(&self, prefix: &str) -> Rgb {
        self.source.by_prefix(prefix).unwrap_or(self.ui.label_idle)
    }
}

/// The room shell: perimeter walls, carpet, and window frame.
#[derive(Debug, Clone)]
pub struct SurfaceColors {
    /// Perimeter wall fill.
    pub wall: Rgb,
    /// Perimeter wall base/top trim line.
    pub wall_trim: Rgb,
    /// Carpet base fill.
    pub carpet_base: Rgb,
    /// The carpet toward the light: a fleck in the classic painter, the lit
    /// floor by the windows in the cutaway, so it must read both as a fleck and
    /// as a field.
    pub carpet_light: Rgb,
    /// The carpet in shadow: a fleck in the classic painter, the far floor and
    /// the contact tone in the cutaway.
    pub carpet_dark: Rgb,
    /// Floor-to-ceiling window frame / mullions.
    pub window_frame: Rgb,
    /// Base buffer fill behind the whole scene.
    pub bg_fallback: Rgb,
}

/// Office fixtures: cubicles, corridor runner, city skyline, clock, shadow.
#[derive(Debug, Clone)]
pub struct OfficeColors {
    /// Frosted-glass room-divider highlight / gradient base.
    pub(crate) room_wall_trim_light: Rgb,
    /// Room-divider door jambs + dark trim (also the aquarium frame).
    pub(crate) room_wall_trim_dark: Rgb,
    /// Corridor runner-rug base.
    pub(crate) runner_base: Rgb,
    /// Corridor runner-rug stripe.
    pub(crate) runner_stripe: Rgb,
    /// Corridor runner-rug edge border.
    pub(crate) runner_edge: Rgb,
    /// Neon wall-board panel background.
    pub neon_panel_bg: Rgb,
    /// City-skyline building, shaded side.
    pub building_dark: Rgb,
    /// City-skyline building, lit side.
    pub building_light: Rgb,
    /// The three lit city-skyline window hues.
    pub city_lit_windows: [Rgb; 3],
    /// Unlit city-skyline window.
    pub city_dark_window: Rgb,
    /// Wall-clock rim.
    pub(crate) clock_rim: Rgb,
    /// Wall-clock face.
    pub(crate) clock_face: Rgb,
    /// Wall-clock hands.
    pub(crate) clock_hand: Rgb,
    /// Drop-shadow tint under sprites and furniture.
    pub(crate) shadow: Rgb,
}

/// Sky gradients, sun/moon disc, light spill, and night tint.
#[derive(Debug, Clone)]
pub struct LightingColors {
    /// Daytime window-sky gradient near the horizon.
    pub(crate) day_sky_a: Rgb,
    /// Daytime window-sky gradient near the top.
    pub(crate) day_sky_b: Rgb,
    /// Night window-sky gradient near the horizon.
    pub night_sky_a: Rgb,
    /// Night window-sky gradient near the top.
    pub night_sky_b: Rgb,
    /// Twilight (dawn/dusk) warm-shift near the horizon.
    pub(crate) twilight_a: Rgb,
    /// Twilight (dawn/dusk) warm-shift near the top.
    pub(crate) twilight_b: Rgb,
    /// Warm sunlight spill on the floor and walls.
    pub(crate) sun_spill: Rgb,
    /// Lounge floor-lamp glow halo.
    pub(crate) floor_lamp_halo: Rgb,
    /// Per-desk task-lamp light — WARM in every theme, or the night's warm-against-cold read is lost.
    pub desk_lamp: Rgb,
    /// Night-time interior darkening tint.
    pub(crate) night_tint: Rgb,
    /// The sun disc's core color — the halo ring reuses this SAME hue at lower
    /// alpha, and it must read warm.
    pub(crate) sun_core: Rgb,
    /// The moon disc's core color (lit side; the dark limb is the fixed
    /// `MOON_SHADOW`, not per-theme). Must read cool.
    pub moon_core: Rgb,
}

/// Desk, rug, aquarium, and token-meter furniture colors.
#[derive(Debug, Clone)]
pub(crate) struct FurnitureColors {
    /// Desk/table wood surface.
    pub(crate) wood_top: Rgb,
    /// Desk/table wood edge trim (darker).
    pub(crate) wood_trim: Rgb,
    /// Lounge area-rug field (main fill).
    pub(crate) rug_field: Rgb,
    /// Lounge area-rug border trim.
    pub(crate) rug_trim: Rgb,
    /// Lounge area-rug accent detail.
    pub(crate) rug_accent: Rgb,
    /// Coffee-table magazine cover.
    pub(crate) magazine: Rgb,
    /// Coffee-table magazine spine/trim.
    pub(crate) magazine_trim: Rgb,
    /// Chair wood trim.
    pub(crate) chair_trim: Rgb,
    /// Desk coffee-cup body.
    pub(crate) coffee_cup: Rgb,
    /// Desk coffee-cup shaded side.
    pub(crate) coffee_cup_shadow: Rgb,
    /// Aquarium water body (deep) + its lit surface row.
    pub(crate) tank_water: Rgb,
    /// Aquarium surface highlight line; also the water-cooler glug bubble.
    pub(crate) tank_water_line: Rgb,
    /// The two fish (frame/cabinet reuse `room_wall_trim_dark` / `wood_*`).
    pub(crate) tank_fish: Rgb,
    /// The second aquarium fish (kept off the lobster's carapace reds).
    pub(crate) tank_fish_alt: Rgb,
    /// Aquarium plant.
    pub(crate) tank_plant: Rgb,
    /// Token-meter paper tower: the sheet face.
    pub(crate) paper: Rgb,
    /// Token-meter paper ream-shading (the every-other-row band).
    pub(crate) paper_shade: Rgb,
}

/// Transient effect cues (glow, sleep z, steam, dust, bubble).
#[derive(Debug, Clone)]
pub(crate) struct EffectColors {
    /// Lit monitor bezel on a running agent's screen.
    pub(crate) monitor_frame_lit: Rgb,
    /// A powered-but-quiet screen's glass — a back-turned desk shows it after dark whether or not anyone is at it.
    pub(crate) monitor_idle: Rgb,
    /// Floating "z" of a sleeping agent.
    pub(crate) sleep_z: Rgb,
    /// Steam wisp off a fresh coffee cup.
    pub(crate) coffee_steam: Rgb,
    /// Dust puff kicked up while walking.
    pub(crate) walking_dust: Rgb,
    /// Overhead bubble on a requires-action agent.
    pub(crate) requires_action_bubble: Rgb,
}

/// Per-tool monitor-glow hues (the screen glow while a tool runs).
#[derive(Debug, Clone)]
pub struct ToolGlowColors {
    /// Glow while an Edit/Write tool runs.
    pub(crate) edit: Rgb,
    /// Glow while a Read tool runs.
    pub(crate) read: Rgb,
    /// Glow while a Bash tool runs.
    pub(crate) bash: Rgb,
    /// Glow while an Agent/subagent tool runs.
    pub agent: Rgb,
    /// Glow while a Grep/search tool runs.
    pub(crate) grep: Rgb,
    /// Glow for any other tool.
    pub(crate) default: Rgb,
}

impl ToolGlowColors {
    /// The exhaustive `ToolKind → hue` map, shared by the monitor glow, the
    /// footer tint and the tooltip so each tool has one hue.
    pub fn for_kind(&self, kind: pixtuoid_core::state::ToolKind) -> Rgb {
        use pixtuoid_core::state::ToolKind;
        match kind {
            ToolKind::Edit => self.edit,
            ToolKind::Read => self.read,
            ToolKind::Bash => self.bash,
            ToolKind::Task => self.agent,
            ToolKind::Search => self.grep,
            ToolKind::Other => self.default,
        }
    }

    /// The hue of a screen's work: its tool's, or `default` for a turn
    /// between items.
    pub(crate) fn for_work(&self, work: crate::lighting::ScreenWork) -> Rgb {
        match work {
            crate::lighting::ScreenWork::ToolUse(kind) => self.for_kind(kind),
            crate::lighting::ScreenWork::Turn => self.default,
        }
    }
}

/// Name-badge, tooltip, and neon-brand UI colors.
#[derive(Debug, Clone)]
pub struct UiColors {
    /// Work in flight: an agent's tool call, or a gateway's turn.
    pub label_running: Rgb,
    /// The `ok` tone, not a slot state: a connected CLI, a checked checkbox.
    pub label_ok: Rgb,
    /// The needs-you tone: an agent that requires action.
    pub label_requires_action: Rgb,
    /// The alarm: a fault or a dead connection.
    pub label_warn: Rgb,
    /// Name-badge tone for an idle agent.
    pub label_idle: Rgb,
    /// Name-badge tone for an exiting agent.
    pub label_exiting: Rgb,
    /// Tooltip / popup background fill, and the cutaway's badge plate.
    pub tooltip_bg: Rgb,
    /// Tooltip title text.
    pub tooltip_title: Rgb,
    /// Tooltip body text.
    pub tooltip_text: Rgb,
    /// Tooltip dimmed/secondary text.
    pub tooltip_dim: Rgb,
    /// The neon sign's hue — brand text, tube and wall halo — and the theme-picker
    /// swatch.
    pub neon_brand: Rgb,
    /// Neon wall-board ★ star-CTA accent.
    pub(crate) neon_star: Rgb,
    /// The neon sign's hue while an agent waits on the user. The hue change IS the
    /// signal, so it must read as a different light from [`Self::neon_brand`]
    /// (`the_neon_alert_hue_is_a_different_light_in_every_theme`) — which a
    /// theme's `label_requires_action` is not always.
    pub(crate) neon_alert: Rgb,
}

/// Corridor appliance colors (vending machine, printer, coat rack) — each
/// theme supplies its own harmonized set.
#[derive(Debug, Clone)]
pub(crate) struct ApplianceColors {
    /// Vending machine chassis (the dark box body).
    pub(crate) vending_body: Rgb,
    /// Vending front sign / accent strip — the theme's signature accent.
    pub(crate) vending_panel: Rgb,
    /// Four distinct drink-bottle colors behind the glass.
    pub(crate) vending_drinks: [Rgb; 4],
    /// Warm small detail (coin-slot trim).
    pub(crate) vending_trim: Rgb,
    /// Darkest recess / slot.
    pub(crate) vending_dark: Rgb,
    /// Printer chassis — a light neutral.
    pub(crate) printer_body: Rgb,
    /// Printer lid / top — darker.
    pub(crate) printer_top: Rgb,
    /// Scanner glass — a cool tint.
    pub(crate) printer_glass: Rgb,
    /// Paper stack — near-white.
    pub(crate) printer_paper: Rgb,
    /// Output tray — mid neutral.
    pub(crate) printer_tray: Rgb,
    /// Three hanging coats on the coat rack.
    pub(crate) coats: [Rgb; 3],
}

/// Per-source badge hues — one color per registered source, drawn as a leading
/// `[xx]` badge in the agent-dashboard popup and the Sources panel. Each theme
/// supplies its own so the badge stays legible on `tooltip_bg`.
#[derive(Debug, Clone)]
pub struct SourceColors {
    /// Claude Code badge hue.
    pub claude_code: Rgb,
    /// Codex badge hue.
    pub codex: Rgb,
    /// Reasonix badge hue.
    pub(crate) reasonix: Rgb,
    /// Antigravity badge hue.
    pub(crate) antigravity: Rgb,
    /// CodeWhale badge hue.
    pub(crate) codewhale: Rgb,
    /// OpenCode badge hue.
    pub(crate) opencode: Rgb,
    /// Copilot badge hue.
    pub(crate) copilot: Rgb,
    /// Cursor badge hue.
    pub(crate) cursor: Rgb,
    /// OpenClaw daemon badge hue.
    pub(crate) openclaw: Rgb,
    /// Hermes badge hue.
    pub(crate) hermes: Rgb,
    /// omp badge hue.
    pub(crate) omp: Rgb,
    /// Grok badge hue.
    pub(crate) grok: Rgb,
    /// Kimi badge hue.
    pub(crate) kimi: Rgb,
    /// DeepSeek Harness badge hue.
    pub(crate) dsh: Rgb,
}

impl SourceColors {
    /// All badge hues in declaration order — the ONE enumeration the legibility
    /// guard and the count-pin test share, so a new source can't silently escape
    /// the per-theme distinctness check.
    #[cfg(test)]
    pub(crate) fn all(&self) -> [Rgb; 14] {
        [
            self.claude_code,
            self.codex,
            self.reasonix,
            self.antigravity,
            self.codewhale,
            self.opencode,
            self.copilot,
            self.cursor,
            self.openclaw,
            self.hermes,
            self.omp,
            self.grok,
            self.kimi,
            self.dsh,
        ]
    }

    /// Badge hue for a source's 2-char label prefix, or `None` for an unknown
    /// one. The arms are a hand-kept copy of the registry's authoritative
    /// `SourceDescriptor::label_prefix` values.
    pub fn by_prefix(&self, prefix: &str) -> Option<Rgb> {
        Some(match prefix {
            "cc" => self.claude_code,
            "cx" => self.codex,
            "rx" => self.reasonix,
            "ag" => self.antigravity,
            "cw" => self.codewhale,
            "oc" => self.opencode,
            "cp" => self.copilot,
            "cu" => self.cursor,
            "ok" => self.openclaw,
            "hm" => self.hermes,
            "om" => self.omp,
            "gk" => self.grok,
            "km" => self.kimi,
            "ds" => self.dsh,
            _ => return None,
        })
    }
}

/// Every built-in theme, in picker order — the slice `theme_by_name` searches.
pub static ALL_THEMES: &[&Theme] = &[
    &NORMAL,
    &CYBERPUNK,
    &DRACULA,
    &TOKYO_NIGHT,
    &CATPPUCCIN,
    &GRUVBOX,
];

/// Resolve a theme by its `name` (the `--theme` value), or `None` if unknown.
pub fn theme_by_name(name: &str) -> Option<&'static Theme> {
    ALL_THEMES.iter().find(|t| t.name == name).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_hue_falls_back_to_the_idle_label() {
        assert_eq!(NORMAL.source_hue("cc"), NORMAL.source.claude_code);
        assert_eq!(NORMAL.source_hue("zz"), NORMAL.ui.label_idle);
    }

    #[test]
    fn unknown_theme_returns_none() {
        assert!(theme_by_name("doesnotexist").is_none());
    }

    /// The night's read is warm lamp AGAINST cold screen, so the pair has to
    /// stay on opposite sides in every theme — gruvbox shipped an olive
    /// `monitor_idle` (R over B) because only the lamp half was ever stated.
    #[test]
    fn the_desk_lamp_reads_warm_and_the_standby_screen_cold_in_every_theme() {
        for t in ALL_THEMES {
            let (lamp, screen) = (t.lighting.desk_lamp, t.effects.monitor_idle);
            assert!(
                lamp.r > lamp.b,
                "{}: desk_lamp {lamp:?} must read WARM (R over B)",
                t.name
            );
            assert!(
                screen.b > screen.r,
                "{}: monitor_idle {screen:?} must read COLD (B over R)",
                t.name
            );
        }
    }

    #[test]
    fn theme_gallery_manifest_matches_all_themes() {
        // Site CI never runs the binary, so this test is the only bridge between
        // themes.json and ALL_THEMES. Set equality, not order: the manifest's
        // order is a site presentation choice.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../site/src/themes.json");
        let json = match std::fs::read_to_string(path) {
            Ok(s) => s,
            // crates.io-packaged test runs don't ship the repo's site/ tree.
            Err(_) => {
                eprintln!("skipping: {path} not present (packaged build)");
                return;
            }
        };
        let manifest: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("themes.json parses");
        let mut ids: Vec<&str> = manifest
            .iter()
            .map(|t| t["id"].as_str().expect("themes.json entry has a string id"))
            .collect();
        let mut names: Vec<&str> = ALL_THEMES.iter().map(|t| t.name).collect();
        ids.sort_unstable();
        names.sort_unstable();
        assert_eq!(
            ids, names,
            "site/src/themes.json ids must match ALL_THEMES names — update the \
             manifest + run `just gen-media` when the registry changes"
        );
    }

    #[test]
    fn dark_themes_marked_dark() {
        assert_eq!(CYBERPUNK.kind, ThemeKind::Dark);
        assert_eq!(DRACULA.kind, ThemeKind::Dark);
        assert_eq!(TOKYO_NIGHT.kind, ThemeKind::Dark);
        assert_eq!(GRUVBOX.kind, ThemeKind::Dark);
        assert_eq!(CATPPUCCIN.kind, ThemeKind::Dark);
    }

    #[test]
    fn sun_and_moon_read_warm_and_cool_for_every_theme() {
        for t in ALL_THEMES {
            let l = &t.lighting;
            assert!(
                l.sun_core.r > l.sun_core.b,
                "{}: sun_core {:?} should read warm (r > b)",
                t.name,
                l.sun_core
            );
            assert!(
                l.moon_core.b > l.moon_core.r,
                "{}: moon_core {:?} should read cool (b > r)",
                t.name,
                l.moon_core
            );
        }
    }

    #[test]
    fn token_paper_is_legible_on_the_desk_for_every_theme() {
        // The tower is 3px wide, so it needs real contrast, but themes like
        // gruvbox run warm and low.
        const MIN_PAPER_MARGIN: f32 = 0.15;
        for t in ALL_THEMES {
            let f = &t.furniture;
            assert!(
                f.paper.lightness() >= f.wood_top.lightness() + MIN_PAPER_MARGIN,
                "{}: paper must read bright against the desk wood",
                t.name
            );
            assert_ne!(
                f.paper, f.paper_shade,
                "{}: ream banding needs two distinct paper tones",
                t.name
            );
        }
    }

    // Per-channel sum-of-abs-diff, deliberately not a luminance test: two hues
    // can share a luminance yet read as different colors, so mutual
    // distinguishability is a Manhattan-distance question, not a brightness one.
    fn manhattan(a: Rgb, b: Rgb) -> u32 {
        u32::from(a.r).abs_diff(u32::from(b.r))
            + u32::from(a.g).abs_diff(u32::from(b.g))
            + u32::from(a.b).abs_diff(u32::from(b.b))
    }

    // Two glow roles too close to tell apart is a CORRECTNESS bug, not taste:
    // an open turn showing no item also glows `default`, so an aliased
    // `default` makes it read as an editing one.
    #[test]
    fn tool_glow_hues_are_distinct_for_every_theme() {
        /// Deliberately looser than `MIN_SOURCE_HUE_DIST`: six glow roles share
        /// ONE palette's accent range, so the legitimate pairs run tight.
        const MIN_TOOL_GLOW_DIST: u32 = 30;
        for t in ALL_THEMES {
            // Destructured with NO `..`: a seventh glow role must join the guard
            // or this stops compiling.
            let ToolGlowColors {
                edit,
                read,
                bash,
                agent,
                grep,
                default,
            } = t.tool_glow;
            let hues = [
                ("edit", edit),
                ("read", read),
                ("bash", bash),
                ("agent", agent),
                ("grep", grep),
                ("default", default),
            ];
            for i in 0..hues.len() {
                for j in (i + 1)..hues.len() {
                    let d = manhattan(hues[i].1, hues[j].1);
                    assert!(
                        d >= MIN_TOOL_GLOW_DIST,
                        "{}: tool glow {} and {} too close ({d} < {MIN_TOOL_GLOW_DIST})",
                        t.name,
                        hues[i].0,
                        hues[j].0
                    );
                }
            }
        }
    }

    #[test]
    fn the_neon_alert_hue_is_a_different_light_in_every_theme() {
        /// Below this the sign's alert and brand light read as one color at a glance.
        const MIN_NEON_ALERT_DIST: u32 = 100;
        for t in ALL_THEMES {
            let d = manhattan(t.ui.neon_alert, t.ui.neon_brand);
            assert!(
                d >= MIN_NEON_ALERT_DIST,
                "{}: neon_alert is {d} from neon_brand (< {MIN_NEON_ALERT_DIST})",
                t.name
            );
        }
    }

    #[test]
    fn appliance_palette_is_legible_for_every_theme() {
        for t in ALL_THEMES {
            let a = &t.appliance;
            assert!(
                a.printer_paper.lightness() > a.printer_body.lightness()
                    && a.printer_body.lightness() > a.printer_top.lightness(),
                "{}: printer must layer paper > body > top by lightness",
                t.name
            );
            assert_ne!(
                a.vending_panel, a.vending_body,
                "{}: vending panel invisible",
                t.name
            );
            for (i, d) in a.vending_drinks.iter().enumerate() {
                assert_ne!(
                    *d, a.vending_body,
                    "{}: drink {i} invisible on body",
                    t.name
                );
                // The busy drop darkens the picked product cell to vending_dark;
                // a drink equal to it would erase that feedback frame.
                assert_ne!(
                    *d, a.vending_dark,
                    "{}: drink {i} indistinguishable from the darkened drop cell",
                    t.name
                );
            }
            // The water cooler's glug bubble paints tank_water_line over the
            // bottle's fixed fill — equal colors erase the third appliance anim.
            assert_ne!(
                t.furniture.tank_water_line,
                crate::pack::COOLER_WATER,
                "{}: glug bubble invisible in the cooler bottle",
                t.name
            );
            // The busy printer ejects its page onto corridor carpet — the paper
            // must not dissolve into any carpet speckle color.
            for c in [
                t.surface.carpet_base,
                t.surface.carpet_light,
                t.surface.carpet_dark,
            ] {
                assert_ne!(
                    a.printer_paper, c,
                    "{}: ejected page invisible on carpet",
                    t.name
                );
            }
            let brightest_drink = a
                .vending_drinks
                .iter()
                .map(|c| c.lightness())
                .fold(0.0, f32::max);
            assert!(
                a.vending_body.lightness() < brightest_drink,
                "{}: vending body should be darker than its drinks",
                t.name
            );
        }
    }

    #[test]
    fn source_badges_legible_for_every_theme() {
        // Floor at which two source badges read as different colors at the 2-char
        // badge scale — new themes/sources must clear it, not merely differ by
        // one bit.
        const MIN_SOURCE_HUE_DIST: u32 = 60;
        // A 2-char badge on the tooltip must stand out by lightness alone.
        const MIN_BADGE_LIGHTNESS_GAP: f32 = 0.1;
        for t in ALL_THEMES {
            let s = &t.source;
            let bg = t.ui.tooltip_bg;
            let hues = s.all();
            for (i, h) in hues.iter().enumerate() {
                assert!(
                    h.lightness() >= bg.lightness() + MIN_BADGE_LIGHTNESS_GAP,
                    "{}: source hue {i} too close to tooltip_bg",
                    t.name
                );
            }
            for i in 0..hues.len() {
                for j in (i + 1)..hues.len() {
                    let d = manhattan(hues[i], hues[j]);
                    assert!(
                        d >= MIN_SOURCE_HUE_DIST,
                        "{}: source hues {i} and {j} too close ({d} < {MIN_SOURCE_HUE_DIST})",
                        t.name
                    );
                }
            }
        }
    }

    // Without this count pin, a newly registered source with no SourceColors
    // field escapes the distinctness guard above entirely.
    #[test]
    fn source_colors_cover_every_registered_source() {
        use pixtuoid_core::source::registry;
        assert_eq!(
            NORMAL.source.all().len(),
            registry::registered_source_names().count(),
            "SourceColors has a different hue count than the registered sources — add the \
             new source's field to SourceColors + all() (and a hue in every theme file)"
        );
    }

    // The guards above pin the HUES, not the prefix STRINGS: a registry prefix
    // RENAME that misses the matching `by_prefix` arm silently drops that
    // source's badge to the idle fallback.
    #[test]
    fn by_prefix_accepts_every_registered_label_prefix() {
        for d in pixtuoid_core::source::registry::REGISTRY {
            assert!(
                NORMAL.source.by_prefix(d.label_prefix).is_some(),
                "theme::by_prefix has no arm for source {:?} label_prefix {:?} — its badge \
                 would fall back to idle; add the arm (or align it with the registry rename)",
                d.name,
                d.label_prefix
            );
        }
    }
}
