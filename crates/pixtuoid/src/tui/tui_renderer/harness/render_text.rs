use super::*;
use pixtuoid_scene::layout::{Point, WallSegment};

#[test]
fn footer_shows_floor_indicator_on_multi_floor() {
    let scene = two_floor_scene();
    let mut r = build(120, 40, vec![]);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("1/2") || text.contains("F1"),
        "multi-floor footer should show a floor indicator; frame:\n{text}"
    );
}

#[test]
fn agent_label_painted_above_character() {
    let mut s = idle("/lbl/0.jsonl", 0, t0() - Duration::from_secs(300));
    s.label = "ZQXLBL".into();
    let scene = scene_with(vec![s], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("ZQXLBL"),
        "the agent's label should be painted above it"
    );
}

#[test]
fn hovered_agent_renders_stats_tooltip() {
    let a = AgentId::from_transcript_path("/pintip/0.jsonl");
    let scene = scene_with(vec![slot(a, 0, 0, t0() - Duration::from_secs(600))], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    let before = frame_text(r.frame_buffer());
    assert!(!before.contains("calls"));
    super::hover_agent(&mut r, a);
    r.render(&scene, t0()).unwrap();
    let after = frame_text(r.frame_buffer());
    assert!(
        after.contains("calls"),
        "hovered dossier should show the agent duration·calls stat line"
    );
}

#[test]
fn hovered_dossier_shows_token_usage_only_when_nonzero() {
    let a = AgentId::from_transcript_path("/toktip/0.jsonl");
    let mut s = slot(a, 0, 0, t0() - Duration::from_secs(600));
    let scene = scene_with(vec![s.clone()], 16);
    let mut r = build(120, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    super::hover_agent(&mut r, a);
    r.render(&scene, t0()).unwrap();
    let without = frame_text(r.frame_buffer());
    assert!(
        !without.contains("tok"),
        "zero-usage dossier must not show a Σ row"
    );
    s.tokens_used = 2_400_000;
    let scene = scene_with(vec![s], 16);
    super::hover_agent(&mut r, a);
    r.render(&scene, t0()).unwrap();
    let with = frame_text(r.frame_buffer());
    assert!(
        with.contains("Σ 2.4M tok"),
        "dossier should fold the compact token total in; frame:\n{with}"
    );
}

#[test]
fn footer_shows_agent_count() {
    let scene = scene_with(
        vec![
            active("/f/0.jsonl", 0, "Edit", t0()),
            idle("/f/1.jsonl", 1, t0()),
            idle("/f/2.jsonl", 2, t0()),
        ],
        16,
    );
    let mut r = build(140, 44, vec![]);
    r.render(&scene, t0()).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains(" 3 \u{b7} \u{25cf}1 A") && text.contains("\u{25cb}2 I"),
        "full-width footer shows the count + state rungs; frame footer area:\n{}",
        text.lines().last().unwrap_or("")
    );
}

#[test]
fn tool_glow_tint_differs_by_tool() {
    let render_tool = |detail: &str| -> (RgbBuffer, Point) {
        // Long-seated (entry walk done) so it's SeatedTyping at the desk and
        // the monitor screen-glow paints.
        let scene = scene_with(
            vec![active(
                "/tg/0.jsonl",
                0,
                detail,
                t0() - Duration::from_secs(300),
            )],
            16,
        );
        let mut r = build(120, 44, vec![]);
        r.render(&scene, t0()).unwrap();
        let desk = r.cached_layout().expect("layout").home_desks[0].at;
        (r.buf().expect("a frame").clone(), desk)
    };
    let (edit, desk) = render_tool("Edit src/main.rs");
    let (bash, _) = render_tool("Bash npm test");
    // Tool tint colours the monitor glow AND the seated worker's skin, both
    // within the cubicle box around the desk.
    let d = region_diff(
        &edit,
        &bash,
        desk.x.saturating_sub(2),
        desk.y.saturating_sub(6),
        20,
        16,
    );
    assert!(
        d > 200,
        "Edit vs Bash should tint the cubicle measurably differently (diff={d})"
    );
}

#[test]
fn weather_variants_render_without_panic_and_vary() {
    // Weather is a deterministic hash of wall-clock that changes every ~10 min,
    // so 10-min steps land each sample on a different weather window.
    let scene = scene_with(vec![idle("/w/0.jsonl", 0, t0())], 16);
    let mut r = build(120, 44, vec![]);
    let mut sigs = std::collections::HashSet::new();
    for step in 0..120u64 {
        let now = t0() + Duration::from_secs(step * 600 + 12 * 3600);
        r.render(&scene, now).unwrap();
        // Signature the top window strip (where weather effects paint).
        let buf = r.buf().expect("a frame");
        let mut s: u64 = 0;
        for y in 0..(buf.height() / 4).max(1) {
            for x in (0..buf.width()).step_by(7) {
                let c = buf.get(x, y);
                s = s
                    .wrapping_mul(1099511628211)
                    .wrapping_add(u64::from(c.r) << 16 | u64::from(c.g) << 8 | u64::from(c.b));
            }
        }
        sigs.insert(s);
    }
    assert!(
        sigs.len() >= 4,
        "weather/time variation should produce several distinct window renders, saw {}",
        sigs.len()
    );
}

fn region_text(buf: &ratatui::buffer::Buffer, cx: u16, cy: u16, cw: u16, ch: u16) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in cy..(cy + ch).min(area.y + area.height) {
        for x in cx..(cx + cw).min(area.x + area.width) {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
    }
    out
}

#[test]
fn meeting_room_fills_and_hosts_group_chitchat() {
    let mut now = t0();

    let cap = 64;
    let n_agents = 40usize;
    let mut scene = SceneState::uniform(cap);
    for i in 0..n_agents {
        let id = AgentId::from_transcript_path(&format!("/h/mtg{i}.jsonl"));
        // Stagger start times so wander cycles desync and the room sees a mix
        // of arrivals/departures.
        let started = now - Duration::from_secs(5 + (i as u64 * 11) % 80);
        scene.agents.insert(id, slot(id, 0, i, started));
    }

    let mut r = build(160, 56, vec![]);
    r.render(&scene, now).expect("render");
    let layout = r.cached_layout().expect("layout").clone();
    let mr = layout
        .meeting_room_bounds(0)
        .expect("floor 0 must have a meeting room at this size");

    // Empty-room pixel baseline (same furniture, no agents) so the region diff
    // isolates the characters.
    let mut r0 = build(160, 56, vec![]);
    r0.render(&SceneState::uniform(cap), now).expect("render");
    let baseline = r0.buf().expect("a frame").clone();

    // The layout must actually carry meeting slots (otherwise the test is
    // vacuous — agents could "occupy" the room while just passing through).
    let slot_count = layout
        .waypoints
        .iter()
        .filter(|w| {
            matches!(
                w.kind,
                pixtuoid_scene::layout::WaypointKind::MeetingSofa
                    | pixtuoid_scene::layout::WaypointKind::MeetingChair
            )
        })
        .count();
    assert!(slot_count >= 4, "expected meeting slots, got {slot_count}");

    // Meeting-room cell band (px→cell: x unchanged, y halved), padded upward so
    // a bubble drawn above a sitter's head is included.
    let cell_y0 = (mr.y / 2).saturating_sub(4);
    let cell_h = mr.height / 2 + 8;

    // Coarse 250 ms beats rather than paint frames: 250 ms stays well under the
    // stale-resume trigger (≥7 s), so the wander machine still advances normally.
    const BUDGET: usize = 1200; // 250ms beats → 300s simulated
    let mut saw_characters = false;
    let mut chat_iter: Option<usize> = None;
    for iter in 1..=BUDGET {
        now += Duration::from_millis(250);
        r.render(&scene, now).expect("render");

        if !saw_characters {
            let d = region_diff(
                &baseline,
                r.buf().expect("a frame"),
                mr.x,
                mr.y,
                mr.width,
                mr.height,
            );
            saw_characters = d > 4_000;
        }
        if chat_iter.is_none() {
            // A chitchat line inside the meeting-room band can only come from ≥2
            // agents at meeting SLOTS — slots → sprites → venue-keyed chat → bubble.
            let text = region_text(r.frame_buffer(), mr.x, cell_y0, mr.width + 6, cell_h);
            if pixtuoid_scene::chitchat::CHITCHAT_LINES
                .iter()
                .any(|l| text.contains(l))
            {
                chat_iter = Some(iter);
            }
        }
        if saw_characters && chat_iter.is_some() {
            break;
        }
    }

    assert!(
        saw_characters,
        "agents never visibly occupied the meeting room"
    );
    let chat_iter = chat_iter.expect("no group chitchat bubble ever appeared in the meeting room");
    // Headroom guard, not a timeout: a fill-erosion canary that surfaces a
    // constant change as "took too long" rather than an edge-of-budget flake.
    assert!(
        chat_iter < (BUDGET * 3) / 4,
        "group chitchat took {chat_iter}/{BUDGET} iterations — fill margin eroded; \
         expected within 3/4 of the budget"
    );
}

#[test]
fn meeting_glass_partition_connects_at_window_and_corner() {
    // Asserted relative to same-row references so the check is immune to the
    // time-of-day dim / weather tint applied globally.
    let mut r = build(192, 80, vec![]);
    let scene = scene_with(vec![idle("/h/glass.jsonl", 0, t0())], 16);
    r.render(&scene, t0()).expect("render");

    let layout = r.cached_layout().expect("layout").clone();
    let v_x = layout
        .room_walls
        .iter()
        .find_map(|w| match *w {
            WallSegment::Vertical { x, .. } => Some(x),
            WallSegment::Horizontal { .. } => None,
        })
        .expect("standard floor has a vertical divider");
    let h_y = layout
        .room_walls
        .iter()
        .find_map(|w| match *w {
            WallSegment::Horizontal { y, .. } => Some(y),
            WallSegment::Vertical { .. } => None,
        })
        .expect("standard floor has a horizontal divider");
    let top_wall_h = layout.top_margin - 4;

    let buf = r.buf().expect("a frame");
    let dist = |a: pixtuoid_core::sprite::Rgb, b: pixtuoid_core::sprite::Rgb| {
        (i32::from(a.r) - i32::from(b.r)).abs()
            + (i32::from(a.g) - i32::from(b.g)).abs()
            + (i32::from(a.b) - i32::from(b.b)).abs()
    };
    // The glass has no single colour — a rim, and panes that show what is
    // behind them — so reference BOTH its rim (dx0) and a pane (dx2), sampled
    // high on the wall where it's unambiguously glass, plus a floor sample.
    let glass_rim = buf.get(v_x, layout.top_margin + 2);
    let glass_pane = buf.get(v_x + 2, layout.top_margin + 2);
    let floor_ref = buf.get(v_x.saturating_sub(8), top_wall_h + 6);
    let is_glass = |p: pixtuoid_core::sprite::Rgb| {
        dist(p, glass_rim).min(dist(p, glass_pane)) < dist(p, floor_ref)
    };

    assert!(
        is_glass(buf.get(v_x, top_wall_h + 1)),
        "vertical divider should connect up to the window band (no floor gap)"
    );

    // The vertical's own soft edge — which the horizontal run, ending at `v_x`,
    // never covers — must extend down through the horizontal wall band.
    assert!(
        is_glass(buf.get(v_x + 2, h_y + 2)),
        "vertical divider should fill the inside corner at the horizontal wall"
    );
}
