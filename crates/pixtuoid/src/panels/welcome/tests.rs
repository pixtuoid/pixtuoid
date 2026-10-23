use super::*;

#[test]
fn from_detected_pre_checks_every_row_and_resolves_badges() {
    let ui = WelcomeUi::from_ids(&["codex"]);
    assert_eq!(ui.rows.len(), 1);
    let row = &ui.rows[0];
    assert_eq!(row.source_id, "codex");
    assert_eq!(row.label_prefix, "cx", "badge resolves from the registry");
    assert_eq!(
        row.display_name, "Codex",
        "name resolves from the install target"
    );
    assert!(row.checked, "all rows pre-checked on first run");
    assert_eq!(ui.selected, 0);
}

#[test]
fn empty_detected_is_empty() {
    let ui = WelcomeUi::from_detected(&[]);
    assert!(ui.is_empty());
    let mut ui = ui;
    ui.move_down();
    ui.move_up();
    ui.toggle_selected();
    assert!(ui.checked().is_empty());
}

#[test]
fn navigation_clamps_at_both_ends() {
    let mut ui = WelcomeUi::from_ids(&["codex", "claude-code", "cursor"]);
    assert_eq!(ui.selected, 0);
    ui.move_up();
    assert_eq!(ui.selected, 0);
    ui.move_down();
    ui.move_down();
    assert_eq!(ui.selected, 2);
    ui.move_down();
    assert_eq!(ui.selected, 2, "clamps at the last row");
    ui.move_up();
    assert_eq!(ui.selected, 1);
}

#[test]
fn dim_ramps_in_on_open_and_out_on_close() {
    let at = |dim: pixtuoid_scene::look::Dim| dim.brightness();
    assert_eq!(
        dim_opening(0),
        pixtuoid_scene::look::Dim::NONE,
        "no dim at t=0"
    );
    assert!(
        (at(dim_opening(DIM_RAMP_MS)) - DIM_FLOOR).abs() < 1e-6,
        "floor at full ramp"
    );
    assert!(
        (at(dim_opening(DIM_RAMP_MS * 10)) - DIM_FLOOR).abs() < 1e-6,
        "clamps past the ramp"
    );
    let half = at(dim_opening(DIM_RAMP_MS / 2));
    assert!(half > DIM_FLOOR && half < 1.0);

    let floor = dim_opening(DIM_RAMP_MS);
    assert!(
        (at(dim_closing(floor, 0).unwrap()) - DIM_FLOOR).abs() < 1e-6,
        "floor at fade start"
    );
    let mid = at(dim_closing(floor, DIM_FADE_OUT_MS / 2).unwrap());
    assert!(mid > DIM_FLOOR && mid < 1.0, "climbing back: {mid}");
    assert_eq!(
        dim_closing(floor, DIM_FADE_OUT_MS),
        None,
        "fully restored ⇒ drop the state"
    );
    assert_eq!(dim_closing(floor, DIM_FADE_OUT_MS + 100), None);

    assert_eq!(
        OnboardingFrame::default().dim,
        pixtuoid_scene::look::Dim::NONE
    );
}

/// A close ramp hardcoded to `DIM_FLOOR` darkened the whole office for a frame
/// before climbing back.
#[test]
fn the_close_fade_continues_from_the_dim_it_was_interrupted_at() {
    let at = |dim: pixtuoid_scene::look::Dim| dim.brightness();
    let interrupted = dim_opening(DIM_RAMP_MS / 3);
    assert!(
        at(interrupted) > DIM_FLOOR,
        "the probe must be mid-ramp: {interrupted:?}"
    );
    let first_close_frame = dim_closing(interrupted, 0).expect("the fade is armed");
    assert!(
        (at(first_close_frame) - at(interrupted)).abs() < 1e-6,
        "the fade must resume at {interrupted:?}, not snap to {first_close_frame:?}"
    );
    let mid = at(dim_closing(interrupted, DIM_FADE_OUT_MS / 2).expect("still fading"));
    assert!(mid > at(interrupted) && mid < 1.0, "climbing back: {mid}");
    assert_eq!(dim_closing(interrupted, DIM_FADE_OUT_MS), None);
}

#[test]
fn toggle_flips_only_the_selected_row_and_feeds_checked() {
    let mut ui = WelcomeUi::from_ids(&["codex", "claude-code"]);
    ui.move_down();
    ui.toggle_selected();
    assert_eq!(
        ui.checked(),
        ["codex"],
        "the untouched row stays, the toggled one goes"
    );
}
