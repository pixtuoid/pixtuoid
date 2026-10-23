//! Pure model for the first-run onboarding "move-in" overlay — no ratatui.
//!
//! The typewriter/boot timing is elapsed-driven in the painter
//! (`widgets/welcome.rs`), so this model holds no clock — only interactive state.

use crate::install::target::Target;
use pixtuoid_core::source::registry::SourceDescriptor;

#[cfg(test)]
mod tests;

/// One roster row = one DETECTED agent CLI the user can opt into connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeRow {
    pub source_id: &'static str,
    /// 2-char badge id (`cc`/`cx`/…) — the same one the dashboard/panel render.
    pub label_prefix: &'static str,
    pub display_name: String,
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WelcomeUi {
    pub rows: Vec<WelcomeRow>,
    pub selected: usize,
}

impl WelcomeUi {
    /// Build the roster from detected (present) CLIs, all PRE-CHECKED:
    /// the office is empty by definition on a first run, so "connect everything I
    /// have" is the friendly default and the user unchecks what they don't want.
    pub(crate) fn from_detected(detected: &[(&'static SourceDescriptor, &'static Target)]) -> Self {
        let rows = detected
            .iter()
            .map(|&(desc, target)| WelcomeRow {
                source_id: desc.name,
                label_prefix: desc.label_prefix,
                display_name: target.display_name.to_string(),
                checked: true,
            })
            .collect();
        Self { rows, selected: 0 }
    }

    /// [`Self::from_detected`] of registered, target-bearing `ids`.
    #[cfg(test)]
    pub(crate) fn from_ids(ids: &[&str]) -> Self {
        let detected: Vec<_> = ids
            .iter()
            .map(|id| {
                (
                    pixtuoid_core::source::registry::descriptor_for(id).expect("a registered id"),
                    crate::install::target::by_source(id).expect("a target-bearing id"),
                )
            })
            .collect();
        Self::from_detected(&detected)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn move_down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    pub(crate) fn toggle_selected(&mut self) {
        if let Some(r) = self.rows.get_mut(self.selected) {
            r.checked = !r.checked;
        }
    }

    /// The rows CONFIRM connects.
    pub(crate) fn checked(&self) -> Vec<&'static str> {
        self.rows
            .iter()
            .filter(|r| r.checked)
            .map(|r| r.source_id)
            .collect()
    }
}

/// The per-frame render snapshot the event loop hands the renderer. `open` (paint
/// the CARD) is decoupled from `dim` (the office backdrop) so the CLOSE fade-out
/// keeps dimming the office for a beat AFTER the card is gone.
#[derive(Debug, Clone, Default)]
pub struct OnboardingFrame {
    pub open: bool,
    pub rows: Vec<WelcomeRow>,
    pub selected: usize,
    pub elapsed_ms: u64,
    /// The brightness the backdrop leaves the office: `DIM_FLOOR` fully
    /// dimmed.
    pub dim: pixtuoid_scene::look::Dim,
}

/// Office brightness the backdrop dims to and the ramp times — the shorter
/// fade-out "lights up" a touch quicker than it went down.
pub(crate) const DIM_FLOOR: f32 = 0.4;
pub(crate) const DIM_RAMP_MS: u64 = 450;
pub(crate) const DIM_FADE_OUT_MS: u64 = 300;

/// The dim `elapsed_ms` after the overlay OPENED — ramps from
/// [`Dim::NONE`](pixtuoid_scene::look::Dim::NONE) to `DIM_FLOOR`.
pub fn dim_opening(elapsed_ms: u64) -> pixtuoid_scene::look::Dim {
    let t = elapsed_ms.min(DIM_RAMP_MS) as f32 / DIM_RAMP_MS as f32;
    pixtuoid_scene::look::Dim::from_brightness(1.0 - t * (1.0 - DIM_FLOOR))
}

/// The dim `elapsed_ms` into the CLOSE fade — ramps `from` to
/// [`Dim::NONE`](pixtuoid_scene::look::Dim::NONE), then `None` once fully
/// restored. `from` is the dim the OPEN ramp was INTERRUPTED at, not
/// `DIM_FLOOR`: an overlay skipped mid-ramp would otherwise snap the whole office
/// darker for a frame before climbing back.
pub(crate) fn dim_closing(
    from: pixtuoid_scene::look::Dim,
    elapsed_ms: u64,
) -> Option<pixtuoid_scene::look::Dim> {
    if elapsed_ms >= DIM_FADE_OUT_MS {
        return None;
    }
    let t = elapsed_ms as f32 / DIM_FADE_OUT_MS as f32;
    let from = from.brightness();
    Some(pixtuoid_scene::look::Dim::from_brightness(
        from + t * (1.0 - from),
    ))
}
