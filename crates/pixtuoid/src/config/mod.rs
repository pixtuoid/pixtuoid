use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::floating::geometry::Zoom;

/// One `[[pets]]` stanza. `kind` is an OPTIONAL raw `String` (NOT a serde-derived
/// `PetKind`) on purpose: an unknown or typo'd value is warn-skipped in
/// `resolve_pets` rather than failing the whole `toml::from_str` and tripping
/// `load`'s all-or-nothing malformed arm, which would silently revert EVERY user
/// setting to defaults.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PetEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct AppConfig {
    pub theme: Option<String>,
    /// Per-floor desk cap; excess agents overflow to additional floors. Absent ⇒
    /// capacity is auto-computed from terminal size.
    #[serde(rename = "max-desks")]
    pub max_desks: Option<usize>,
    /// A [`LookMode`](crate::graphics::LookMode) name, raw so a typo warns in
    /// `resolve_look` instead of failing the whole load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<String>,
    /// A [`MotionMode`] name, raw so a typo warns in `resolve_motion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<String>,
    #[serde(
        rename = "last-seen-version",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_seen_version: Option<String>,
    /// The connection flags `sources::is_on` reads (registry source id →
    /// connected). Keep BEFORE `pets` (see `pets`).
    #[serde(
        rename = "sources",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub sources: BTreeMap<String, bool>,
    /// Keep BEFORE `pets` (see `pets`).
    #[serde(rename = "floating", default, skip_serializing_if = "Option::is_none")]
    pub floating: Option<FloatingConfigRaw>,
    /// Ambient office sound. Absent ⇒ MUTED — the office starts silent and `m`
    /// is the whole opt-in. Keep BEFORE `pets` (see `pets`).
    #[serde(rename = "audio", default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioConfigRaw>,
    /// Keep `pets` LAST in the struct: it is an array-of-tables, and any `[table]`
    /// serialized after it would re-parent under it.
    #[serde(rename = "pets", default, skip_serializing_if = "Option::is_none")]
    pub pets: Option<Vec<PetEntry>>,
}

/// Default `pixtuoid floating` window size (logical px); the window's own
/// minimum is the pack's (`floating::geometry::min_window`).
pub const FLOATING_DEFAULT_W: u32 = 480;
pub const FLOATING_DEFAULT_H: u32 = 320;

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FloatingConfigRaw {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    /// Wider than the zoom it holds: a hand-edited value past it clamps
    /// instead of failing the whole file's parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FloatingConfig {
    pub width: u32,
    pub height: u32,
    /// `None` lets the OS place the window.
    pub position: Option<(i32, i32)>,
    pub zoom: Zoom,
}

impl FloatingConfig {
    /// This geometry with its size raised to at least `min_w`×`min_h`, the
    /// one size the window opens at and is placed by.
    #[must_use]
    pub(crate) fn at_least(self, min_w: u32, min_h: u32) -> Self {
        Self {
            width: self.width.max(min_w),
            height: self.height.max(min_h),
            ..self
        }
    }
}

pub(crate) fn resolve_floating(config: &AppConfig) -> FloatingConfig {
    let raw = config.floating.clone().unwrap_or_default();
    FloatingConfig {
        width: raw.width.unwrap_or(FLOATING_DEFAULT_W),
        height: raw.height.unwrap_or(FLOATING_DEFAULT_H),
        position: raw.x.zip(raw.y),
        zoom: Zoom::new(
            raw.zoom
                .map_or(0, |z| z.clamp(i64::from(i8::MIN), i64::from(i8::MAX)) as i8),
        ),
    }
}

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AudioConfigRaw {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
}

/// `muted` is the ONE sound switch — there is deliberately no second `enabled`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AudioConfig {
    pub muted: bool,
    pub volume: f32,
}

pub(crate) fn resolve_audio(config: &AppConfig) -> AudioConfig {
    let raw = config.audio.clone().unwrap_or_default();
    AudioConfig {
        muted: raw.muted.unwrap_or(true),
        volume: raw.volume.unwrap_or(1.0).clamp(0.0, 1.0),
    }
}

pub(crate) fn save_audio_muted(path: &Path, muted: bool) -> Result<()> {
    update_config(path, |doc| {
        doc["audio"]["muted"] = toml_edit::value(muted);
    })
}

pub(crate) fn save_audio_volume(path: &Path, volume: f32) -> Result<()> {
    // Quantize to the footer's percent vocabulary before widening — a raw
    // f32→f64 writes float noise (0.949999988079071) into a hand-edited file.
    let percent = (volume * 100.0).round() / 100.0;
    update_config(path, |doc| {
        doc["audio"]["volume"] = toml_edit::value(f64::from(percent));
    })
}

/// The directory `pixtuoid/` config lives under: a set `XDG_CONFIG_HOME`, else
/// `$HOME/.config`. Empty or relative `XDG_CONFIG_HOME` is invalid (XDG spec),
/// so `nonempty_abs_env` falls through rather than resolving against the CWD.
pub(crate) fn config_base() -> Option<PathBuf> {
    crate::install::io::nonempty_abs_env("XDG_CONFIG_HOME")
        .or_else(|| pixtuoid_core::platform::user_home_opt().map(|h| h.join(".config")))
}

pub(crate) fn config_path() -> PathBuf {
    config_base().map_or_else(
        || PathBuf::from(".config/pixtuoid/config.toml"),
        |base| base.join("pixtuoid").join("config.toml"),
    )
}

/// Report ONE user-facing config warning to BOTH of its sinks from a single
/// control-char-stripped string. Both sinks are real terminals and these lines
/// interpolate config CONTENT — a `toml::de::Error` Display embeds the raw
/// offending source line — so an ANSI/OSC escape or a Trojan-Source bidi override
/// would render live. Keep it ONE emission point so neither sink drifts back to raw.
fn warn_user(warnings: &mut Vec<String>, line: String) {
    let line = crate::strip_control_chars(&line);
    tracing::warn!("{line}");
    warnings.push(line);
}

/// [`load`] plus the DEGRADED bit: the file exists but did not parse cleanly.
///
/// Captured here rather than read back off `warnings` at the call site. The
/// caller's Vec is shared with every `resolve_*` below it, so
/// `!warnings.is_empty()` only means "degraded" on the one line directly after
/// `load` — a temporal invariant nothing enforced, and reordering a resolver
/// above it silently flipped a first run into "previously configured",
/// suppressing onboarding forever (#836 review).
pub(crate) fn load_with_status(path: &Path, warnings: &mut Vec<String>) -> (AppConfig, bool) {
    let before = warnings.len();
    let cfg = load(path, warnings);
    let degraded = warnings.len() > before;
    (cfg, degraded)
}

/// Load the config, never crashing: unreadable/malformed files fall back to
/// defaults. Fallbacks go onto `warnings` (as well as the log) so `main` can
/// print them to stderr BEFORE the alternate screen swallows them; callers with
/// no user to warn pass a throwaway Vec.
pub(crate) fn load(path: &Path, warnings: &mut Vec<String>) -> AppConfig {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AppConfig::default(),
        Err(e) => {
            warn_user(
                warnings,
                format!(
                    "cannot read config {} ({e}) — using defaults",
                    path.display()
                ),
            );
            return AppConfig::default();
        }
    };
    match toml::from_str(&contents) {
        Ok(cfg) => cfg,
        Err(e) => {
            warn_user(
                warnings,
                format!(
                    "malformed config {} — ALL settings reset to defaults ({e})",
                    path.display()
                ),
            );
            AppConfig::default()
        }
    }
}

/// Load-modify-write under ONE advisory lock held across the whole
/// read→mutate→write round. The mutation edits the RAW TOML document, not a typed
/// `AppConfig` round-trip, so unknown keys (a newer pixtuoid's settings) and the
/// user's comments survive a save.
///
/// Data-safety contract: a config that EXISTS but does not parse is NEVER
/// rewritten — the save fails with the parse error, leaving the user's typo
/// fixable.
fn update_config<F>(path: &Path, mutate: F) -> Result<()>
where
    F: FnOnce(&mut toml_edit::DocumentMut),
{
    let lock = crate::install::io::lock_config(path)?;
    let real_path = lock.target();
    // Read through the guard's pinned resolution, NOT a raw read of a re-derived
    // path: every leg of the locked round must address the ONE file the flock
    // protects.
    let contents = lock.read().with_context(|| {
        format!(
            "refusing to rewrite {}: cannot read the existing config",
            real_path.display()
        )
    })?;
    let mut doc = if contents.is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        let doc = contents.parse::<toml_edit::DocumentMut>().map_err(|e| {
            anyhow::anyhow!(
                "refusing to rewrite {}: it exists but is not valid TOML ({e}); fix or delete it",
                real_path.display()
            )
        })?;
        // Syntax alone isn't enough: a type-invalid value (`max-desks = "oops"`)
        // parses as a document but fails the typed `load`, so persisting over it
        // would make this save "succeed" while never taking effect. Unknown keys
        // still pass (forward-compat).
        toml::from_str::<AppConfig>(&contents).map_err(|e| {
            anyhow::anyhow!(
                "refusing to rewrite {}: it exists but has invalid values ({e}); fix or delete it",
                real_path.display()
            )
        })?;
        doc
    };
    mutate(&mut doc);
    lock.write_atomic(&doc.to_string())
}

pub(crate) fn save(path: &Path, theme_name: &str) -> Result<()> {
    update_config(path, |doc| {
        doc["theme"] = toml_edit::value(theme_name);
    })
}

pub(crate) fn save_version(path: &Path, version: &str) -> Result<()> {
    update_config(path, |doc| {
        doc["last-seen-version"] = toml_edit::value(version);
    })
}

/// Rewrite `[sources]` as exactly the `ids` that are on once `change` applies,
/// each `= true`, dropping the table when none is. The current flags are read
/// under the config lock, so a concurrent writer's change survives.
pub(crate) fn save_flag_sources(
    path: &Path,
    ids: &[&str],
    change: Option<(&str, bool)>,
) -> Result<()> {
    update_config(path, |doc| {
        let mut table = toml_edit::Table::new();
        for &sid in ids {
            let on = match change {
                Some((id, want)) if id == sid => want,
                _ => {
                    doc.get("sources")
                        .and_then(|s| s.get(sid))
                        .and_then(toml_edit::Item::as_bool)
                        == Some(true)
                }
            };
            if on {
                table[sid] = toml_edit::value(true);
            }
        }
        if table.is_empty() {
            doc.as_table_mut().remove("sources");
        } else {
            doc["sources"] = toml_edit::Item::Table(table);
        }
    })
}

/// What the floating window keeps of itself across runs: its logical size,
/// its physical position when the OS reports one, and its zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FloatingSave {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) position: Option<(i32, i32)>,
    pub(crate) zoom: Zoom,
}

pub(crate) fn save_floating(path: &Path, save: &FloatingSave) -> Result<()> {
    let &FloatingSave {
        width,
        height,
        position,
        zoom,
    } = save;
    update_config(path, |doc| {
        doc["floating"]["width"] = toml_edit::value(i64::from(width));
        doc["floating"]["height"] = toml_edit::value(i64::from(height));
        // Set-or-CLEAR x/y: a `None` means the OS couldn't report the position
        // (ALWAYS on Wayland, or a transient at close). Keeping the OLD coords
        // would restore a stale/offscreen spot next launch, so drop the keys and
        // let the OS place the window. No zoom is no key, as an unset one reads.
        let zoom = (zoom != Zoom::default()).then(|| i32::from(zoom.steps()));
        for (key, val) in [
            ("x", position.map(|p| p.0)),
            ("y", position.map(|p| p.1)),
            ("zoom", zoom),
        ] {
            match val {
                Some(v) => doc["floating"][key] = toml_edit::value(i64::from(v)),
                // `as_table_like_mut`, not `as_table_mut`: `floating` serializes as
                // an INLINE table, for which the standard-table accessor returns
                // None and the key would never drop.
                None => {
                    if let Some(t) = doc["floating"].as_table_like_mut() {
                        t.remove(key);
                    }
                }
            }
        }
    })
}

/// Resolve the config `max-desks` into the runtime desk cap. `0` is treated as
/// unset with a warning: the cap clamps every floor via `min`, so an accepted 0
/// would permanently zero every floor and silently drop every SessionStart. The
/// `--max-desks` CLI flag rejects 0 at the clap seam; this is its config twin.
pub(crate) fn resolve_max_desks(config: &AppConfig, warnings: &mut Vec<String>) -> Option<usize> {
    match config.max_desks {
        Some(0) => {
            warn_user(
                warnings,
                "max-desks = 0 in config would hide every agent — ignoring it \
                 (the --max-desks flag or auto-computed capacity applies)"
                    .into(),
            );
            None
        }
        other => other,
    }
}

/// Resolve CLI + config into the desk cap the runtime uses (CLI > config).
///
/// The config lookup is EAGER — `.or`, never `.or_else` — so the
/// `max-desks = 0` warning still fires when the CLI flag wins (#836). Don't
/// inline this back into `build_run_config`: that call site reads the real
/// `config_path()`, so nothing there can assert the warning.
pub(crate) fn resolve_desk_cap(
    config: &AppConfig,
    cli_max_desks: Option<usize>,
    warnings: &mut Vec<String>,
) -> Option<usize> {
    cli_max_desks.or(resolve_max_desks(config, warnings))
}

/// Resolve CLI + config into the one `&'static Theme` the runtime uses
/// (CLI > config > `NORMAL`). The asymmetry is deliberate: a `--theme` typo is
/// explicit user intent and hard-errors, while a config typo soft-warns and
/// falls back so a stale config file never bricks startup.
///
/// # Errors
///
/// If `cli_theme` names no theme; the message lists the valid names.
pub(crate) fn resolve_theme(
    config: &AppConfig,
    cli_theme: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<&'static pixtuoid_scene::theme::Theme> {
    use pixtuoid_scene::theme::{ALL_THEMES, NORMAL, theme_by_name};

    // Validate the config theme even when the CLI overrides it — the warn is the
    // only signal that a persisted theme has gone stale.
    let config_theme = config.theme.as_deref().and_then(|t| {
        let theme = theme_by_name(t);
        if theme.is_none() {
            warn_user(
                warnings,
                format!("unknown theme {t:?} in config — ignoring (falling back to the default)"),
            );
        }
        theme
    });
    if let Some(name) = cli_theme {
        return theme_by_name(name).ok_or_else(|| {
            let valid: Vec<&str> = ALL_THEMES.iter().map(|t| t.name).collect();
            anyhow::anyhow!("unknown theme: {name}. Valid: {}", valid.join(", "))
        });
    }
    Ok(config_theme.unwrap_or(&NORMAL))
}

/// Resolve CLI + config into the run's [`LookMode`](crate::graphics::LookMode)
/// (CLI > config > default). The config value is checked even when a flag
/// wins, as in [`resolve_theme`], and by clap's own parser, so the file and the
/// flag accept the same names.
pub(crate) fn resolve_look(
    config: &AppConfig,
    cli: Option<crate::graphics::LookMode>,
    warnings: &mut Vec<String>,
) -> crate::graphics::LookMode {
    let configured = config.look.as_deref().and_then(|v| {
        let mode = <crate::graphics::LookMode as clap::ValueEnum>::from_str(v, false).ok();
        if mode.is_none() {
            warn_user(
                warnings,
                format!("unknown look {v:?} in config — ignoring (falling back to the default)"),
            );
        }
        mode
    });
    cli.or(configured).unwrap_or_default()
}

/// How much of the office's ambient life moves: the `motion` config key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum MotionMode {
    /// What the display affords: calmer where a repaint is dear.
    #[default]
    Auto,
    /// Every ambient loop.
    Full,
    /// Every ambient loop, at a quarter of the pace.
    Calm,
    /// No ambient loop and no flash.
    Still,
}

impl MotionMode {
    /// The tier this mode names, `auto` being `afforded`.
    pub(crate) fn or(self, afforded: pixtuoid_scene::anim::Motion) -> pixtuoid_scene::anim::Motion {
        use pixtuoid_scene::anim::Motion;
        match self {
            Self::Auto => afforded,
            Self::Full => Motion::Full,
            Self::Calm => Motion::Calm,
            Self::Still => Motion::Still,
        }
    }
}

/// Resolve config into the run's [`MotionMode`], warning on an unknown name
/// as [`resolve_look`] does.
pub(crate) fn resolve_motion(config: &AppConfig, warnings: &mut Vec<String>) -> MotionMode {
    let configured = config.motion.as_deref().and_then(|v| {
        let mode = <MotionMode as clap::ValueEnum>::from_str(v, false).ok();
        if mode.is_none() {
            warn_user(
                warnings,
                format!("unknown motion {v:?} in config — ignoring (falling back to the default)"),
            );
        }
        mode
    });
    configured.unwrap_or_default()
}

/// Resolve config into the office's `Pet`s. An unknown `kind` is warn-skipped —
/// the remaining stanzas survive. Resolving HERE (once, at startup) means the
/// render path reads `pet.name` directly, with no per-frame lookup.
pub(crate) fn resolve_pets(
    config: &AppConfig,
    warnings: &mut Vec<String>,
) -> Vec<pixtuoid_scene::pet::Pet> {
    use pixtuoid_scene::pet::{Pet, PetKind};

    match &config.pets {
        None => PetKind::ALL.iter().map(|&k| Pet::defaulted(k)).collect(),
        Some(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries {
                let Some(kind) = entry.kind.as_deref().and_then(PetKind::from_config_name) else {
                    warn_user(
                        warnings,
                        format!(
                            "missing or unknown pet `kind` {:?} in [[pets]] config — skipping that pet",
                            entry.kind.as_deref().unwrap_or("<missing>")
                        ),
                    );
                    continue;
                };
                let name = entry
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| kind.default_name().to_string());
                out.push(Pet { kind, name });
            }
            if out.is_empty() && !entries.is_empty() {
                warn_user(
                    warnings,
                    "all [[pets]] entries had unknown kinds — no pets will appear".into(),
                );
            }
            out
        }
    }
}

#[cfg(test)]
mod tests;
