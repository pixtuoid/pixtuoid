use super::*;
use crate::tui::geometry::CellArea;

/// A scene carrying an OpenClaw gateway presence and nothing else, so its diff
/// against [`no_presence`] is the lobster alone.
fn gateway_scene(
    liveness: pixtuoid_core::state::DaemonLiveness,
    entered_at: SystemTime,
    last_seen: SystemTime,
    sessions: u32,
) -> SceneState {
    gateway_scene_runs(liveness, entered_at, last_seen, sessions, &[])
}

/// As `gateway_scene`, with in-flight RUN keys. Busy is DERIVED: pass
/// `DaemonLiveness::UP` + ≥1 run, not a stored Busy state.
fn gateway_scene_runs(
    liveness: pixtuoid_core::state::DaemonLiveness,
    entered_at: SystemTime,
    last_seen: SystemTime,
    sessions: u32,
    runs: &[&str],
) -> SceneState {
    let mut s = SceneState::uniform(16);
    s.insert_daemon(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        harness_gateway(),
        pixtuoid_core::state::DaemonPresence {
            liveness,
            active_sessions: sessions,
            last_seen,
            entered_at,
            in_flight_runs: runs
                .iter()
                // Stamped at the scene's own clock so the run lease is fresh —
                // these fixtures assert the BUSY render, not the decay.
                .map(|r| (r.to_string(), last_seen))
                .collect(),
            current_pid: Some(1),
        },
    );
    s
}

fn harness_gateway() -> pixtuoid_core::state::DaemonInstanceId {
    pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty")
}

fn gateway_scene_at(ports: &[&str], entered_at: SystemTime, last_seen: SystemTime) -> SceneState {
    let mut s = SceneState::uniform(16);
    for port in ports {
        s.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            pixtuoid_core::state::DaemonInstanceId::new(*port).expect("non-empty"),
            pixtuoid_core::state::DaemonPresence {
                liveness: pixtuoid_core::state::DaemonLiveness::UP,
                active_sessions: 0,
                last_seen,
                entered_at,
                in_flight_runs: Default::default(),
                current_pid: Some(1),
            },
        );
    }
    s
}

/// The renderer under test beside a twin that paints the daemon-free
/// baseline at every instant it paints: a mascot walks on cross-frame state,
/// so the baseline must not step the renderer under test, and the twin keeps
/// every other cross-frame state (the dimming, the neon) in lockstep.
struct Probe {
    r: TuiRenderer<TestBackend>,
    base: TuiRenderer<TestBackend>,
}

impl Probe {
    fn new() -> Self {
        Self {
            r: build(160, 80, vec![]),
            base: build(160, 80, vec![]),
        }
    }

    /// Cells where `scene` differs from `baseline` at the same instant — the
    /// mascot's footprint, whatever the painter did to its colours.
    ///
    /// Renders BOTH arms here rather than reading one buffer for an authored RGB:
    /// the foreground wash recolours every drawable, so a colour probe would have to
    /// predict the wash, which couples a presence assertion to the lighting model
    /// and expects a value the code under test computed.
    fn diff(
        &mut self,
        scene: &SceneState,
        baseline: &SceneState,
        now: SystemTime,
    ) -> std::collections::BTreeSet<(u16, u16)> {
        self.base.render(baseline, now).unwrap();
        self.r.render(scene, now).unwrap();
        let (base, buf) = (
            self.base.buf().expect("a frame"),
            self.r.buf().expect("a frame"),
        );
        (0..buf.height())
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y) != base.get(x, y))
            .collect()
    }

    fn cells(
        &mut self,
        scene: &SceneState,
        now: SystemTime,
    ) -> std::collections::BTreeSet<(u16, u16)> {
        self.diff(scene, &no_presence(), now)
    }

    fn px(&mut self, scene: &SceneState, now: SystemTime) -> usize {
        self.cells(scene, now).len()
    }

    /// Bounding box in PIXEL coords, or `None` if the lobster isn't on screen.
    fn bbox(&mut self, scene: &SceneState, now: SystemTime) -> Option<(u16, u16, u16, u16)> {
        let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0u16, 0u16);
        let mut any = false;
        for (x, y) in self.cells(scene, now) {
            any = true;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        any.then_some((x0, y0, x1, y1))
    }
}

/// The daemon-free scene every mascot probe subtracts.
fn no_presence() -> SceneState {
    SceneState::uniform(16)
}

#[test]
fn two_gateways_render_two_independent_mascots() {
    let (entered, seen) = (t0() - Duration::from_secs(20), t0());
    let cells_of = |ports: &[&str]| {
        let mut p = Probe::new();
        p.cells(&gateway_scene_at(ports, entered, seen), t0())
    };
    let a = cells_of(&["18789"]);
    let b = cells_of(&["19789"]);
    assert!(
        !a.is_empty() && !b.is_empty(),
        "each gateway draws a lobster"
    );
    assert_ne!(
        a, b,
        "two gateways differing only in port must wander to DIFFERENT cells"
    );
    let both = cells_of(&["18789", "19789"]);
    assert!(
        both.len() > a.len() && both.len() > b.len(),
        "two mascots must cover more floor than either alone (got {} vs {}/{})",
        both.len(),
        a.len(),
        b.len()
    );

    // Consecutive ports on purpose: that is what a real multi-gateway host runs,
    // and their folded wander seeds differ by 1.
    let mut prev = 0usize;
    for n in 1..=4 {
        let ports = &["18901", "18902", "18903", "18904"][..n];
        let cells = cells_of(ports);
        assert!(
            cells.len() > prev,
            "{n} gateways must cover more floor than {}: {} vs {prev}",
            n - 1,
            cells.len()
        );
        prev = cells.len();
    }
}

#[test]
fn the_port_suffix_names_a_gateway_only_when_it_has_a_sibling() {
    // `GatewayCard::of` owns this decision (`GatewayCard.instance`) and the only
    // way to observe it is the hover text.
    let (entered, seen) = (t0() - Duration::from_secs(20), t0());
    let gateway_tooltips = |ports: &[&str]| -> Vec<String> {
        let scene = gateway_scene_at(ports, entered, seen);
        let mut p = Probe::new();
        let cells: Vec<_> = p.cells(&scene, t0()).into_iter().collect();
        assert!(!cells.is_empty(), "the gateways must paint lobsters");
        let mut out = Vec::new();
        // A stride, not every cell: the hover is the painted lobster frame, so
        // it still lands inside BOTH mascots without paying a full render per
        // pixel.
        for &(x, y) in cells.iter().step_by(5) {
            p.r.set_mouse_pos(Some((x, CellArea::row_of(y))));
            p.r.render(&scene, t0()).unwrap();
            let text = frame_text(p.r.frame_buffer());
            if text.contains("gateway") {
                out.push(text);
            }
        }
        out
    };

    let lone = gateway_tooltips(&["18789"]);
    assert!(!lone.is_empty(), "the lone gateway must be hoverable");
    assert!(
        lone.iter().all(|t| !t.contains("18789")),
        "a gateway with no sibling must NOT be named by port: {lone:?}"
    );

    let pair = gateway_tooltips(&["18789", "19789"]);
    assert!(
        pair.iter().any(|t| t.contains("18789")),
        "with a sibling, each tooltip must name WHICH gateway: {pair:?}"
    );
    assert!(
        pair.iter().any(|t| t.contains("19789")),
        "…including the second one: {pair:?}"
    );

    // "Has a sibling" is per SOURCE, not roster-wide: a second daemon SOURCE draws
    // no mascot yet still occupies a roster row. Unreachable from the registry
    // today, which is precisely why it needs staging by hand.
    let (entered, seen) = (t0() - Duration::from_secs(20), t0());
    let mut mixed = gateway_scene_at(&["18789"], entered, seen);
    mixed.insert_daemon(
        "daemon2",
        pixtuoid_core::state::DaemonInstanceId::new("1").expect("non-empty"),
        pixtuoid_core::state::DaemonPresence {
            liveness: pixtuoid_core::state::DaemonLiveness::UP,
            active_sessions: 0,
            last_seen: seen,
            entered_at: entered,
            in_flight_runs: Default::default(),
            current_pid: Some(1),
        },
    );
    let mut p = Probe::new();
    let cells: Vec<_> = p.cells(&mixed, t0()).into_iter().collect();
    assert!(!cells.is_empty(), "openclaw still paints its lobster");
    for &(x, y) in cells.iter().step_by(5) {
        p.r.set_mouse_pos(Some((x, CellArea::row_of(y))));
        p.r.render(&mixed, t0()).unwrap();
        let text = frame_text(p.r.frame_buffer());
        if text.contains("gateway") {
            assert!(
                !text.contains("18789"),
                "a foreign daemon SOURCE must not make openclaw's lone gateway \
                 name its port: {text}"
            );
        }
    }
}

#[test]
fn one_gateway_going_down_leaves_its_sibling_on_the_floor() {
    let (entered, seen) = (t0() - Duration::from_secs(20), t0());
    let mut scene = gateway_scene_at(&["18789", "19789"], entered, seen);
    let a = pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty");
    pixtuoid_core::source::daemon::apply_presence(
        &mut scene,
        &pixtuoid_core::source::daemon::DaemonInstanceKey::new(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            a.clone(),
        ),
        pixtuoid_core::source::daemon::DaemonPresenceUpdate::GatewayDown,
        t0(),
    );
    assert_eq!(
        scene
            .daemon(pixtuoid_core::source::openclaw::SOURCE_NAME, &a)
            .map(|p| p.display_state()),
        Some(pixtuoid_core::state::DaemonState::Down)
    );
    let b = pixtuoid_core::state::DaemonInstanceId::new("19789").expect("non-empty");
    assert_eq!(
        scene
            .daemon(pixtuoid_core::source::openclaw::SOURCE_NAME, &b)
            .map(|p| p.display_state()),
        Some(pixtuoid_core::state::DaemonState::Idle),
        "the sibling gateway is untouched"
    );
    let mut p = Probe::new();
    assert!(
        p.px(&scene, t0() + Duration::from_secs(5)) > 0,
        "the surviving gateway must still paint its lobster"
    );
}

/// The differential's own control: the metric every probe in this file reads has
/// to be SIGNAL. Two identical scenes must differ in zero cells, or a render that
/// churns between calls would satisfy `> 0` with no mascot drawn at all.
#[test]
fn the_mascot_differential_is_signal_not_render_churn() {
    let mut p = Probe::new();
    assert_eq!(
        p.diff(&no_presence(), &no_presence(), t0()).len(),
        0,
        "two renders of the SAME scene must be byte-identical"
    );

    // And a real mascot's cells stay a mascot-sized blob rather than scattering
    // across the office, which is what `gateway_scene`'s doc claims.
    let up = gateway_scene(
        pixtuoid_core::state::DaemonLiveness::UP,
        t0() - Duration::from_secs(20),
        t0(),
        0,
    );
    let cells = p.cells(&up, t0());
    assert!(!cells.is_empty(), "the fixture must paint a mascot");
    let (xs, ys): (Vec<_>, Vec<_>) = cells.iter().copied().unzip();
    let (w, h) = (
        xs.iter().max().unwrap() - xs.iter().min().unwrap() + 1,
        ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1,
    );
    // Bounded by the PACK's own lobster frame, not a transcribed size.
    let pk = pack();
    let frame = pk
        .piece(pixtuoid_core::sprite::format::Piece::LobsterWalk)
        .first();
    let (sw, sh) = (frame.width(), frame.height());
    assert!(
        w <= sw * 2 && h <= sh * 2,
        "the differing cells span {w}x{h}, over twice the {sw}x{sh} lobster — \
         the diff is catching something besides the mascot"
    );
}

#[test]
fn gateway_mascot_present_when_up() {
    // entered_at well in the past ⇒ steady wander (past the walk-in).
    let scene = gateway_scene(
        pixtuoid_core::state::DaemonLiveness::UP,
        t0() - Duration::from_secs(20),
        t0(),
        0,
    );
    let mut p = Probe::new();
    assert!(
        p.px(&scene, t0()) > 10,
        "a live gateway ⇒ the lobster scuttles the floor"
    );
}

#[test]
fn gateway_mascot_busy_bubbles_track_runs_not_sessions() {
    let entered = t0() - Duration::from_secs(20);

    // sessions=1 with NO runs; then the same session WITH two in-flight runs.
    let idle = gateway_scene(pixtuoid_core::state::DaemonLiveness::UP, entered, t0(), 1);
    let busy = gateway_scene_runs(
        pixtuoid_core::state::DaemonLiveness::UP,
        entered,
        t0(),
        1,
        &["r1", "r2"],
    );

    let mut p = Probe::new();
    // Bubbles animate by `now`; scan a few frames so we don't land on an
    // all-off-screen phase.
    let mut runs_add = 0;
    let mut session_adds = 0;
    for k in 0..8u64 {
        let now = t0() + Duration::from_millis(k * 130);
        // Runs are the ONLY difference between the two scenes, so their diff is
        // the bubbles. The session arm is the negative half: a second session
        // with no runs must move nothing.
        runs_add = runs_add.max(p.diff(&busy, &idle, now).len());
        let more_sessions =
            gateway_scene(pixtuoid_core::state::DaemonLiveness::UP, entered, t0(), 3);
        session_adds = session_adds.max(p.diff(&more_sessions, &idle, now).len());
    }
    assert!(runs_add > 0, "an in-flight run ⇒ activity bubbles render");
    assert_eq!(session_adds, 0, "more idle sessions must NOT bubble");
}

#[test]
fn gateway_mascot_walks_out_then_is_gone() {
    let mut p = Probe::new();
    let entered = t0() - Duration::from_secs(20);
    let up = gateway_scene(pixtuoid_core::state::DaemonLiveness::UP, entered, t0(), 0);
    assert!(p.px(&up, t0()) > 0, "a live gateway's lobster is in");

    let mut now = t0() + Duration::from_secs(1);
    let down = gateway_scene(pixtuoid_core::state::DaemonLiveness::Down, entered, now, 0);
    assert!(
        p.px(&down, now) > 0,
        "its gateway dead, the lobster walks out"
    );

    // Swept from the roster, it walks on until it is out.
    let mut frames = 0;
    while p.px(&no_presence(), now) > 0 {
        now += Duration::from_millis(33);
        frames += 1;
        assert!(frames < 3_600, "the lobster never reached the elevator");
    }
    assert!(frames > 0, "it walked on past the roster dropping it");
    assert_eq!(
        p.px(&down, now),
        0,
        "a gateway seen leaving does not walk back in"
    );
}

#[test]
fn gateway_mascot_wanders_over_time() {
    let scene = gateway_scene(
        pixtuoid_core::state::DaemonLiveness::UP,
        t0() - Duration::from_secs(20),
        t0(),
        0,
    );
    let mut p = Probe::new();
    let mut tops = std::collections::HashSet::new();
    for k in 0..8u64 {
        let now = t0() + Duration::from_secs(k * 3);
        if let Some(top) = p
            .cells(&scene, now)
            .into_iter()
            .min_by_key(|&(x, y)| (y, x))
        {
            tops.insert(top);
        }
    }
    assert!(
        tops.len() >= 2,
        "the lobster should wander to ≥2 distinct positions, saw {}",
        tops.len()
    );
}

#[test]
fn gateway_mascot_tooltip_on_hover() {
    // entered_at well in the past ⇒ steady wander: a stable lobster to aim at.
    let scene = gateway_scene(
        pixtuoid_core::state::DaemonLiveness::UP,
        t0() - Duration::from_secs(20),
        t0(),
        0,
    );
    // vec![] = no pet, so the pet hover arm is skipped and the mascot arm runs.
    let mut p = Probe::new();
    p.r.render(&scene, t0()).unwrap();

    assert!(
        !frame_text(p.r.frame_buffer()).contains("gateway"),
        "no hover ⇒ no mascot tooltip"
    );

    let (x0, y0, x1, y1) = p.bbox(&scene, t0()).expect("lobster on screen");
    let cx = u16::midpoint(x0, x1);
    let cy_px = u16::midpoint(y0, y1);
    // The hover is the painted frame, which tolerates the approximate
    // center.
    p.r.set_mouse_pos(Some((cx, CellArea::row_of(cy_px))));
    p.r.render(&scene, t0()).unwrap();

    // The literal "gateway" is exclusive to the mascot arm — pet/coffee/furniture
    // tooltips never say it — so it distinguishes the branch from the fallthroughs.
    assert!(
        frame_text(p.r.frame_buffer()).contains("gateway"),
        "hovering the lobster shows the gateway mascot tooltip"
    );
}
