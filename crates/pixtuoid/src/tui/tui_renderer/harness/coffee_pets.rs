use super::*;

#[test]
fn coffee_state_evicted_when_agent_leaves_scene() {
    let id = AgentId::from_transcript_path("/cof/leave.jsonl");
    let scene = scene_with(vec![slot(id, 0, 0, t0())], 16);
    let mut r = build(100, 40, vec![]);
    r.inject_coffee(id, t0());
    r.evict_missing(&scene);
    r.render(&scene, t0()).unwrap();
    assert!(r.coffee_contains(id));
    let empty = SceneState::uniform(16);
    r.evict_missing(&empty);
    r.render(&empty, t0() + Duration::from_millis(PAINT_FRAME_MS))
        .unwrap();
    assert!(
        !r.coffee_contains(id),
        "coffee state must be evicted when the agent leaves (no leak)"
    );
}

#[test]
fn coffee_state_is_evicted_during_a_floor_transition() {
    let cap = 16;
    let a = AgentId::from_transcript_path("/cof/slide0.jsonl");
    let b = AgentId::from_transcript_path("/cof/slide1.jsonl");
    let scene = scene_with(vec![slot(a, 0, 0, t0()), slot(b, 1, cap, t0())], cap);
    let mut r = build(100, 40, vec![]);
    r.inject_coffee(a, t0());
    let mut now = t0();
    r.evict_missing(&scene);
    r.render(&scene, now).expect("render");
    assert!(r.coffee_contains(a), "cup staged");

    now += Duration::from_millis(PAINT_FRAME_MS);
    r.navigate_floor(1, now);
    let gone = scene_with(vec![slot(b, 1, cap, t0())], cap);
    now += Duration::from_millis(PAINT_FRAME_MS);
    r.evict_missing(&gone);
    r.render(&gone, now).expect("render");
    assert!(r.transition().is_some(), "still mid-slide");
    assert!(
        !r.coffee_contains(a),
        "the office half must be evicted on the transition path too"
    );
}

#[test]
fn coffee_persists_through_floor_transition() {
    let step = Duration::from_millis(500);
    let cap = 16;
    // The pantry is 1 of ~10 wander waypoints, so many floor-0 agents reach it far
    // sooner than any single one would; the floor-1 occupant gives navigate_floor(1)
    // a destination.
    let n_f0 = 10usize;
    let mut agents: Vec<_> = (0..n_f0)
        .map(|i| {
            idle(
                &format!("/cof/f0_{i}.jsonl"),
                i,
                t0() - Duration::from_secs(120),
            )
        })
        .collect();
    agents.push(slot(
        AgentId::from_transcript_path("/cof/f1.jsonl"),
        1,
        cap,
        t0(),
    ));
    let scene = scene_with(agents, cap);
    let f0_ids: Vec<AgentId> = (0..n_f0)
        .map(|i| AgentId::from_transcript_path(&format!("/cof/f0_{i}.jsonl")))
        .collect();

    // Pass 1 (scratch): find the first frame where the NORMAL render path detects a
    // floor-0 wanderer walking back from the pantry.
    let mut scratch = build(100, 40, vec![]);
    let mut now = t0();
    scratch.render(&scene, now).unwrap();
    let mut hit = None;
    'outer: for _ in 0..400 {
        now += step;
        scratch.render(&scene, now).unwrap();
        for &id in &f0_ids {
            if scratch.coffee_contains(id) {
                hit = Some((id, now));
                break 'outer;
            }
        }
    }
    let (agent, detect_at) = hit.expect("a floor-0 wanderer should fetch coffee while wandering");

    // Pass 2 (real): stop one step BEFORE detection, begin a transition, then render
    // AT detect_at, so the carrier is first detected DURING the slide. The gap must
    // stay ≤ step < the 900ms transition window and < the wander stale-resume trigger,
    // else this timeline diverges from the scratch pass.
    let mut r = build(100, 40, vec![]);
    let mut t = t0();
    r.render(&scene, t).unwrap();
    while t + step < detect_at {
        t += step;
        r.render(&scene, t).unwrap();
    }
    assert!(
        !r.coffee_contains(agent),
        "agent must not yet hold coffee before the transition"
    );
    r.navigate_floor(1, t);
    assert!(r.transition().is_some(), "navigation begins a transition");
    r.render(&scene, detect_at).unwrap();
    assert!(
        r.coffee_contains(agent),
        "a coffee run completing mid-transition must persist (regression: \
         render_transition_floor dropped new_coffee_carriers)"
    );
}

#[test]
fn injected_coffee_changes_desk_render() {
    // The two renders share a scene and a final timestamp so the diff is attributable
    // to the cup + steam, not to elapsed-time animation.
    let id = AgentId::from_transcript_path("/cof/steam.jsonl");
    let scene = scene_with(
        vec![idle("/cof/steam.jsonl", 0, t0() - Duration::from_secs(30))],
        16,
    );
    let t1 = t0() + Duration::from_millis(PAINT_FRAME_MS);

    let mut base = build(100, 40, vec![]);
    base.render(&scene, t0()).unwrap();
    base.render(&scene, t1).unwrap();
    let baseline = base.buf().expect("a frame").clone();
    let desk = base.cached_layout().expect("layout").home_desks[0];

    let mut r = build(100, 40, vec![]);
    r.render(&scene, t0()).unwrap();
    r.inject_coffee(id, t0()); // fresh fetch ⇒ within steam window
    r.render(&scene, t1).unwrap();

    let d = region_diff(
        &baseline,
        r.buf().expect("a frame"),
        desk.at.x.saturating_sub(2),
        desk.at.y.saturating_sub(6),
        18,
        14,
    );
    assert!(
        d > 0,
        "coffee state should alter the desk render (cup + steam)"
    );
}

#[test]
fn no_pet_when_pets_disabled() {
    let scene = scene_with(vec![running("/pet/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(100, 40, vec![]);
    r.render(&scene, t0()).unwrap();
    assert!(r.drawn_pet().is_none(), "no pet when none enabled");
}

#[test]
fn pet_present_when_enabled() {
    let scene = scene_with(vec![running("/pet/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(100, 40, vec![PetKind::Cat]);
    r.render(&scene, t0()).unwrap();
    assert!(r.drawn_pet().is_some(), "a cat should be placed");
}

#[test]
fn pet_position_varies_over_its_roam() {
    let scene = scene_with(vec![running("/pet/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(100, 40, vec![PetKind::Cat]);
    let mut seen = std::collections::HashSet::new();
    for step in 0..(pixtuoid_scene::PET_LONGEST_REST_MS + 10_000) / 500 {
        r.render(&scene, t0() + Duration::from_millis(step * 500))
            .unwrap();
        if let Some(PetHover {
            centre: pos, anim, ..
        }) = r.drawn_pet()
        {
            seen.insert((pos.x, pos.y, anim));
        }
    }
    assert!(
        seen.len() >= 2,
        "pet should move/animate as it roams, saw {} distinct states",
        seen.len()
    );
}

#[test]
fn petting_freezes_pet_position() {
    let scene = scene_with(vec![running("/pet/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(100, 40, vec![PetKind::Cat]);
    r.render(&scene, t0()).unwrap();
    let PetHover {
        centre: pos, kind, ..
    } = r.drawn_pet().expect("pet placed");
    r.set_active_pet(Some(PetState {
        petted_at: t0(),
        kind,
        floor_idx: 0,
    }));
    r.render(&scene, t0() + Duration::from_millis(500)).unwrap();
    let PetHover { centre: pos2, .. } = r.drawn_pet().expect("pet still placed");
    assert_eq!(pos, pos2, "a petted pet holds its position");
}

#[test]
fn pet_walk_is_frame_stable() {
    let scene = scene_with(vec![running("/pstab/0.jsonl", 0, "Edit", t0())], 16);
    let mut r1 = build(160, 80, vec![PetKind::Cat]);
    let mut r2 = build(160, 80, vec![PetKind::Cat]);
    for step in 0..(pixtuoid_scene::PET_LONGEST_REST_MS + 10_000) / 500 {
        let now = t0() + Duration::from_millis(step * 500);
        r1.render(&scene, now).unwrap();
        r2.render(&scene, now).unwrap();
        assert_eq!(
            r1.drawn_pet().map(|f| (f.centre.x, f.centre.y, f.anim)),
            r2.drawn_pet().map(|f| (f.centre.x, f.centre.y, f.anim)),
            "identical frames must give an identical pet (no flash), step {step}"
        );
    }
}

#[test]
fn pet_walks_routed_ground_and_rests_on_walkable_floor() {
    let scene = scene_with(vec![running("/pwalk/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(160, 80, vec![PetKind::Cat]);
    r.render(&scene, t0()).unwrap();
    let layout = r.cached_layout().expect("layout after prime").clone();
    let (mut walking, mut resting) = (0, 0);
    for step in 0..3 * pixtuoid_scene::PET_LONGEST_REST_MS / 400 {
        r.render(&scene, t0() + Duration::from_millis(step * 400))
            .unwrap();
        let Some(PetHover {
            centre: pos, anim, ..
        }) = r.drawn_pet()
        else {
            continue;
        };
        if anim == PetKind::Cat.walk_anim().piece() {
            // Coarse-cell walkable is the predicate A* itself guarantees;
            // per-pixel `is_walkable` is stricter than the router delivers
            // and would hold the pet to a higher bar than the agents.
            assert!(
                pixtuoid_scene::pathfind::point_in_walkable_cell(&layout.walkable, pos),
                "walking pet at ({},{}) is in a blocked routing cell (step={step})",
                pos.x,
                pos.y
            );
            walking += 1;
        } else {
            // A rest is a snapped cell center, so the per-pixel check applies.
            assert!(
                layout.walkable.is_walkable(pos.x, pos.y),
                "resting pet at ({},{}) is on a blocked cell (step={step})",
                pos.x,
                pos.y
            );
            resting += 1;
        }
    }
    assert!(
        walking > 0 && resting > 0,
        "the sample must walk and rest: {walking}/{resting}"
    );
}

#[test]
fn pet_leg_boundary_no_pop() {
    // The two timestamps straddle the 40s leg boundary, where the snapped rest anchor
    // is also the next leg's snapped walk-start anchor.
    let scene = scene_with(vec![running("/pbnd/0.jsonl", 0, "Edit", t0())], 16);
    let mut r = build(160, 80, vec![PetKind::Cat]);
    r.render(&scene, t0() + Duration::from_millis(39_600))
        .unwrap();
    let before = r.drawn_pet().map(|f| (f.centre.x, f.centre.y));
    r.render(&scene, t0() + Duration::from_millis(40_040))
        .unwrap();
    let after = r.drawn_pet().map(|f| (f.centre.x, f.centre.y));
    if let (Some((x0, y0)), Some((x1, y1))) = (before, after) {
        let gap = (i32::from(x0) - i32::from(x1)).unsigned_abs()
            + (i32::from(y0) - i32::from(y1)).unsigned_abs();
        assert!(
            gap <= 16,
            "pet leg boundary teleports (gap={gap}px, ({x0},{y0})→({x1},{y1}))"
        );
    }
}

#[test]
fn pet_tooltip_shows_cooldown_reaction_for_cat_and_dog() {
    for (kind, word) in [(PetKind::Cat, "purr"), (PetKind::Dog, "woof")] {
        let scene = scene_with(vec![running("/ck/0.jsonl", 0, "Edit", t0())], 16);
        let mut r = build(140, 48, vec![kind]);
        r.render(&scene, t0()).unwrap();
        let PetHover { centre: pos, .. } = r.drawn_pet().expect("pet placed");
        r.set_active_pet(Some(PetState {
            petted_at: t0(),
            kind,
            floor_idx: 0,
        }));
        r.set_mouse_pos(Some((pos.x, pos.y / 2)));
        r.render(&scene, t0() + Duration::from_millis(200)).unwrap();
        let text = frame_text(r.frame_buffer());
        assert!(
            text.contains(word),
            "{kind:?} on cooldown should show '{word}'; got:\n{text}"
        );
    }
}

#[test]
fn pet_tooltip_shows_sleeping_when_all_idle() {
    // A long-idle scene is required: the cat only sleeps near idle agents.
    let scene = scene_with(
        vec![idle("/slp/0.jsonl", 0, t0() - Duration::from_secs(300))],
        16,
    );
    let mut r = build(160, 64, vec![PetKind::Cat]);
    let mut hit = None;
    for i in 0..40u64 {
        let now = t0() + Duration::from_secs(i);
        r.render(&scene, now).unwrap();
        if let Some(PetHover {
            centre: pos, anim, ..
        }) = r.drawn_pet()
            && anim == PetKind::Cat.sleep_anim()
        {
            hit = Some((pos, now));
            break;
        }
    }
    let (pos, now) = hit.expect("a long-idle cat must enter its sleep anim within the window");
    r.set_mouse_pos(Some((pos.x, pos.y / 2)));
    r.render(&scene, now).unwrap();
    let text = frame_text(r.frame_buffer());
    assert!(
        text.contains("sleeping"),
        "hovering a sleeping cat shows the sleeping line; got:\n{text}"
    );
}

#[test]
fn furniture_tooltip_flips_below_near_top_edge() {
    let scene = scene_with(vec![idle("/flip/0.jsonl", 0, t0())], 16);
    let mut r = build(140, 48, vec![]);
    r.render(&scene, t0()).unwrap();
    let layout = r.cached_layout().expect("layout");
    let mut top_hit = None;
    'scan: for my in 0..6u16 {
        for mx in 0..140u16 {
            if crate::tui::hit_test::hit_test_furniture(
                layout,
                crate::tui::geometry::CellArea::half_block(mx, my),
            )
            .is_some()
            {
                top_hit = Some((mx, my));
                break 'scan;
            }
        }
    }
    let (mx, my) = top_hit.expect("some furniture must hover-test near the top edge");
    r.set_mouse_pos(Some((mx, my)));
    r.render(&scene, t0())
        .expect("top-edge furniture hover must flip the tooltip below without panic");
}

#[test]
fn agent_tooltip_flips_up_near_bottom_edge() {
    // The agent must sit at the BOTTOM-most home desk, else the dossier fits below the
    // cursor and never takes the flip-up branch.
    let probe = SceneState::uniform(16);
    let mut r = build(120, 44, vec![]);
    r.render(&probe, t0()).unwrap();
    let layout = r.cached_layout().expect("layout").clone();
    let bottom_idx = layout
        .home_desks
        .iter()
        .enumerate()
        .max_by_key(|(_, d)| d.at.y)
        .map(|(i, _)| i)
        .expect("a home desk");
    let scene = scene_with(
        vec![idle(
            "/flup/0.jsonl",
            bottom_idx,
            t0() - Duration::from_secs(120),
        )],
        16,
    );
    r.render(&scene, t0()).unwrap();
    let id = AgentId::from_transcript_path("/flup/0.jsonl");
    super::hover_agent(&mut r, id);
    r.render(&scene, t0())
        .expect("bottom-edge hover must not panic");
}
