use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use pixtuoid_core::source::AgentEvent;
use pixtuoid_core::state::{ActivityState, ToolKind};
use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex, Reducer, SceneState, Transport};
use tokio::sync::{RwLock, mpsc};

pub(crate) async fn capture_live_scene(
    projects_root: &str,
    listen_secs: u64,
) -> Result<SceneState> {
    println!(
        "listening for real CC events under {} for {}s...",
        projects_root, listen_secs
    );
    let scene: Arc<RwLock<SceneState>> = Arc::new(RwLock::new(SceneState::uniform(12)));
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(1024);
    let tx_hook = tx.clone();
    let root = PathBuf::from(projects_root);
    // Drive THE production chain, never a hand-mirrored copy of it.
    let mut watcher = pixtuoid_core::source::claude_code::cc_watcher(root.clone());
    // The SAME liveness probe as the real source: without it an in-flight session
    // first-sights at EOF (no history replay), so session-cumulative state —
    // tokens_used above all — reads ZERO and `--live` renders no paper tower.
    if let Some(sessions_dir) = pixtuoid_core::source::cc_registry_dir(&root) {
        watcher = watcher.with_liveness_probe(std::sync::Arc::new(move || {
            pixtuoid_core::source::claude_code::live_cc_session_ids(&sessions_dir)
        }));
    }
    let watcher_handle = tokio::spawn(async move { watcher.run(tx).await });
    // ALSO listen on the real hook socket: a live CC session's hooks carry
    // activity + the burn-tier effort level the transcript alone can't provide in
    // real time. SocketBusy (a running pixtuoid owns it) degrades to
    // transcript-only, like the app.
    let hook_handle = {
        use pixtuoid_core::source::DynSource;
        use pixtuoid_core::source::hook::HookRouter;
        let socket = pixtuoid_core::source::claude_code::ClaudeCodeSource::default_socket_path();
        let router: Box<dyn DynSource> = Box::new(HookRouter::new(socket));
        let tx = tx_hook;
        tokio::spawn(async move {
            if let Err(e) = router.run(tx).await {
                eprintln!("hook listener unavailable ({e}); transcript-only capture");
            }
        })
    };

    let mut reducer = Reducer::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(listen_secs);
    let mut event_count: u64 = 0;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((transport, ev))) => {
                let now = SystemTime::now();
                let mut s = scene.write().await;
                reducer.apply(&mut s, ev, now, transport);
                event_count += 1;
            }
            _ => break,
        }
    }
    let snapshot = scene.read().await.clone();
    println!(
        "captured {} events; final scene has {} agents",
        event_count,
        snapshot.agents.len()
    );
    for (id, slot) in &snapshot.agents {
        println!(
            "  {} ({}) at desk {}: {:?} model={:?} effort={:?}",
            slot.label,
            id,
            slot.desk_index.0,
            slot.state,
            slot.model,
            slot.effort.as_ref().map(|e| e.value.clone())
        );
    }
    watcher_handle.abort();
    hook_handle.abort();
    Ok(snapshot)
}

pub(crate) fn sample_scene(now: SystemTime, max_desks: usize, n_agents: usize) -> SceneState {
    let mut s = SceneState::uniform(max_desks);
    fill_sample_agents(&mut s, now, 0..n_agents);
    s
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum GatewayState {
    Idle,
    Busy,
    Degraded,
    Down,
}

/// Stage one OpenClaw gateway presence (the wandering lobster mascot) per entry
/// in `ports` — empty ⇒ the single upstream default port, so every existing
/// caller and every gen-media baseline is unchanged.
pub(crate) fn inject_openclaw_presence(
    s: &mut SceneState,
    state: GatewayState,
    now: SystemTime,
    ports: &[String],
) -> Result<()> {
    use pixtuoid_core::state::{DaemonLiveness, DaemonPresence};
    // Busy is DERIVED from the run set: "busy" = UP + in-flight runs, never a
    // stored state.
    let (liveness, active_sessions, runs) = match state {
        GatewayState::Idle => (DaemonLiveness::UP, 1, Vec::new()),
        GatewayState::Busy => (
            DaemonLiveness::UP,
            1,
            vec!["run-a".to_string(), "run-b".to_string()],
        ),
        // Up, but the model backend fails every run — no in-flight runs, because
        // the last one FAILED out of the set.
        GatewayState::Degraded => (DaemonLiveness::Up { degraded: true }, 1, Vec::new()),
        GatewayState::Down => (DaemonLiveness::Down, 0, Vec::new()),
    };
    // `entered_at` ~20s in the past lands past the enter animation, so a static
    // snapshot captures the steady wander rather than the walk-in.
    let entered_at = now
        .checked_sub(std::time::Duration::from_secs(20))
        .unwrap_or(now);
    let ports: Vec<&str> = if ports.is_empty() {
        vec!["18789"]
    } else {
        ports.iter().map(String::as_str).collect()
    };
    for (i, port) in ports.iter().enumerate() {
        s.insert_daemon(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            pixtuoid_core::state::DaemonInstanceId::new(*port)
                .ok_or_else(|| anyhow::anyhow!("--openclaw-ports entries must be non-empty"))?,
            DaemonPresence {
                liveness,
                active_sessions,
                last_seen: now,
                entered_at,
                in_flight_runs: runs
                    .iter()
                    // Fresh leases at `now` so the snapshot renders the BUSY mascot.
                    .map(|r| (r.clone(), now))
                    .collect(),
                // Distinct per instance: a shared pid would make one exit receipt
                // look like every gateway's.
                current_pid: Some(4242 + i as i32),
            },
        );
    }
    Ok(())
}

/// One exemplar per token-meter tier, each one step past its threshold. The
/// values cross a crate boundary (the tier consts are scene-internal), so the
/// `debug_assert` in `fill_sample_agents` pins them to the REAL ladder.
const TIER_EXEMPLARS: [u64; 4] = [0, 400_000, 3_000_000, 20_000_000];

fn fill_sample_agents(s: &mut SceneState, now: SystemTime, desks: std::ops::Range<usize>) {
    debug_assert!(
        TIER_EXEMPLARS
            .iter()
            .map(|&t| pixtuoid_scene::token_meter::token_tier(t))
            .eq(0u8..=3),
        "gallery exemplars must span all four tiers"
    );
    use std::time::Duration as D;
    // (label, state, since_event, since_entry) — the two must DIFFER, or
    // `in_thinking_window` and `ENTRY_ANIMATION_MS` between them bar both seated poses.
    let agents: [(&str, ActivityState, D, D); 12] = [
        // Desk 0 is a FAR-seated desk, so this one also demonstrates the
        // screen staying dark behind a monitor the room only sees the back of.
        (
            "thinking",
            ActivityState::Idle,
            D::from_secs(5),
            D::from_secs(90),
        ),
        (
            "waiting",
            ActivityState::Waiting {
                reason: "permission?".into(),
            },
            D::from_secs(10),
            D::from_secs(30),
        ),
        (
            "working",
            ActivityState::Active {
                tool_use_id: Some("tu_a".into()),
                detail: Some("Write: src/foo.rs".into()),
                kind: ToolKind::Edit,
            },
            D::from_millis(0),
            D::from_secs(6),
        ),
        (
            "idle-a",
            ActivityState::Idle,
            D::from_secs(300),
            D::from_secs(300),
        ), // 5 min — wander/sleep cycle
        (
            "idle-b",
            ActivityState::Idle,
            D::from_secs(301),
            D::from_secs(301),
        ),
        (
            "idle-c",
            ActivityState::Idle,
            D::from_secs(303),
            D::from_secs(303),
        ),
        (
            "couch-act",
            ActivityState::Active {
                tool_use_id: Some("tu_c".into()),
                detail: Some("Read: README.md".into()),
                kind: ToolKind::Read,
            },
            D::from_millis(140),
            D::from_millis(140),
        ),
        (
            "couch-bk",
            ActivityState::Waiting {
                reason: "review".into(),
            },
            D::from_millis(0),
            D::from_millis(0),
        ),
        (
            "floor-act",
            ActivityState::Active {
                tool_use_id: Some("tu_d".into()),
                detail: Some("Bash: cargo test".into()),
                kind: ToolKind::Bash,
            },
            D::from_millis(140),
            D::from_millis(140),
        ),
        (
            "floor-idle",
            ActivityState::Idle,
            D::from_millis(2_000),
            D::from_millis(2_000),
        ),
        (
            "floor-act2",
            ActivityState::Active {
                tool_use_id: Some("tu_e".into()),
                detail: Some("Grep: TODO".into()),
                kind: ToolKind::Search,
            },
            D::from_millis(280),
            D::from_millis(280),
        ),
        (
            "floor-idle2",
            ActivityState::Idle,
            D::from_millis(3_000),
            D::from_millis(3_000),
        ),
    ];
    const DEMO_REPOS: [&str; 3] = ["/demo/api", "/demo/web", "/demo/infra"];
    for i in desks {
        let (key, state, since_event, since_entry) = &agents[i % agents.len()];
        // Keys must be unique across the full desk range so each desk slot gets its
        // own AgentId — suffixed once the archetypes cycle.
        let unique_key = if i < agents.len() {
            key.to_string()
        } else {
            format!("{key}-{i}")
        };
        let cwd_str = DEMO_REPOS[i % DEMO_REPOS.len()];
        let id = AgentId::from_transcript_path(&format!("/demo/{unique_key}.jsonl"));
        s.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: std::sync::Arc::from("claude-code"),
                session_id: std::sync::Arc::from(format!("demo-{unique_key}").as_str()),
                cwd: std::sync::Arc::from(PathBuf::from(cwd_str).as_path()),
                label: unique_key.as_str().into(),
                state: state.clone(),
                state_started_at: now - *since_event,
                created_at: now - *since_entry,
                last_event_at: now - *since_event,
                exiting_at: None,
                pending_idle_at: None,

                desk_index: GlobalDeskIndex(i),
                // Hardcoding 0 here would leave overflow agents invisible.
                floor_idx: s.floor_of(GlobalDeskIndex(i)),
                tool_call_count: 0,
                active_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: TIER_EXEMPLARS[i % 4],
                last_usage: None,
            },
        );
    }
}

#[derive(Debug, Clone)]
struct MeetingCandidate {
    path: String,
    id: AgentId,
    cycle_n: u64,
    wp_idx: usize,
    room_id: usize,
    is_sofa: bool,
    /// The room's bottom-most meeting seat renders the sitter in BACK view,
    /// mostly occluded behind the table — a staged group avoids it so all
    /// sprites read on camera.
    is_south_seat: bool,
    dwell_ms: u64,
}

/// Max per-group spread of the staged agents' desk dwells (ms) — they must rise
/// near-together so arrivals overlap well inside the 20–40s meeting dwell.
const MEETING_DWELL_SPREAD_MS: u64 = 3_000;

/// Encoded clip starts this long before the earliest staged rise.
const MEETING_WARMUP_LEAD_MS: u64 = 1_500;

/// Build a scene where `n` agents (desks 0..n) are staged to converge on ONE
/// meeting room: each `state_started_at` is back-dated by
/// `cycle_n * est_wander_cycle_ms + ε` so `advance_wander`'s bootstrap fast-forward
/// selects a cycle landing on a DISTINCT slot of the same room. It restarts
/// the phase clock on first observation (anti-teleport), so every staged agent
/// still sits out its full `seated_dwell_ms`; the returned warmup pre-rolls the
/// capture to just before the first rise.
pub(crate) fn meeting_scene(
    now: SystemTime,
    n: usize,
    cols: u16,
    rows: u16,
    floor_seed: u64,
    max_desks: usize,
    n_agents: usize,
) -> Result<(SceneState, u64)> {
    use pixtuoid_scene::layout::{SceneLayout, WaypointKind};
    use pixtuoid_scene::pose::{
        est_wander_cycle_ms, is_aimless_cycle, seated_dwell_ms, takes_trip,
        waypoint_index_for_cycle,
    };

    // The renderer's own buffer and `None` desk fill: any other layout shifts the
    // waypoint indices and the staging silently misses.
    let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(cols, rows);
    let l = SceneLayout::compute_with_seed(buf_w, buf_h, None, floor_seed)
        .ok_or_else(|| anyhow::anyhow!("--meeting: scene too small to compute a layout"))?;
    let nw = l.waypoints.len();

    // Candidate sweep over deterministic synthetic ids: the LOWEST cycle ≥1 whose
    // trip lands on a meeting slot. Cycle 0's 400ms back-date sits inside
    // ENTRY_ANIMATION_MS, where the door entry-walk override would hijack the
    // staging. Reachability rides approach_point's fallback — a boxed-in
    // seat degrades to an aimless amble, caught by the visual check at `just gen`.
    let south_y_of_room = |room: usize| -> Option<u16> {
        l.waypoints
            .iter()
            .filter(|w| {
                w.room_id == Some(room)
                    && matches!(
                        w.kind,
                        WaypointKind::MeetingSofa | WaypointKind::MeetingChair
                    )
            })
            .map(|w| w.pos.y)
            .max()
    };

    let mut cands: Vec<MeetingCandidate> = Vec::new();
    for i in 0..20_000u64 {
        let path = format!("/meeting/agent_{i}.jsonl");
        let id = AgentId::from_transcript_path(&path);
        for cycle_n in 1..=5u64 {
            if !takes_trip(id, cycle_n) || is_aimless_cycle(id, cycle_n) {
                continue;
            }
            let wp_idx = waypoint_index_for_cycle(id, cycle_n, nw);
            let wp = l.waypoints[wp_idx];
            let is_sofa = match wp.kind {
                WaypointKind::MeetingSofa => true,
                WaypointKind::MeetingChair => false,
                _ => continue,
            };
            let room_id = wp.room_id.unwrap_or(0);
            cands.push(MeetingCandidate {
                path,
                id,
                cycle_n,
                wp_idx,
                room_id,
                is_sofa,
                is_south_seat: south_y_of_room(room_id) == Some(wp.pos.y),
                dwell_ms: seated_dwell_ms(id),
            });
            break;
        }
    }
    cands.sort_by_key(|c| (c.dwell_ms, c.id.raw()));

    // Lowest-dwell window of n candidates: same room, distinct slots, dwell
    // spread within the cap. Prefer ≥2 on sofas — it reads "meeting" far better
    // than a stand-up cluster — then fall back without that bias.
    let pick = |need_sofas: usize, avoid_south: bool| -> Option<Vec<MeetingCandidate>> {
        for (i, base) in cands.iter().enumerate() {
            if avoid_south && base.is_south_seat {
                continue;
            }
            let mut sel = vec![base.clone()];
            for c in cands[i + 1..].iter() {
                if c.dwell_ms - base.dwell_ms > MEETING_DWELL_SPREAD_MS {
                    break;
                }
                if (avoid_south && c.is_south_seat)
                    || c.room_id != base.room_id
                    || sel.iter().any(|s| s.wp_idx == c.wp_idx)
                {
                    continue;
                }
                sel.push(c.clone());
                if sel.len() == n {
                    break;
                }
            }
            if sel.len() == n && sel.iter().filter(|c| c.is_sofa).count() >= need_sofas {
                return Some(sel);
            }
        }
        None
    };
    let staged = pick(2.min(n), true)
        .or_else(|| pick(2.min(n), false))
        .or_else(|| pick(0, false))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--meeting {n}: no candidate group found ({} meeting-bound candidates at \
             {buf_w}x{buf_h} seed {floor_seed})",
                cands.len()
            )
        })?;

    let min_dwell = staged.iter().map(|c| c.dwell_ms).min().unwrap_or(0);
    let warmup_ms = min_dwell.saturating_sub(MEETING_WARMUP_LEAD_MS);

    let mut s = SceneState::uniform(max_desks);
    for (i, c) in staged.iter().enumerate() {
        let wp = l.waypoints[c.wp_idx];
        eprintln!(
            "MEETING agent {} desk {} id={} cycle={} → waypoint[{}] {:?} room {} at ({}, {}) \
             desk_dwell={}ms rise@{}ms",
            (b'a' + i as u8) as char,
            i,
            c.path,
            c.cycle_n,
            c.wp_idx,
            wp.kind,
            c.room_id,
            wp.pos.x,
            wp.pos.y,
            c.dwell_ms,
            c.dwell_ms.saturating_sub(warmup_ms),
        );
        // Back-date past `cycle_n` whole estimated cycles, plus a hair so the
        // integer division can't land on the boundary.
        let back_date = Duration::from_millis(c.cycle_n * est_wander_cycle_ms(c.id) + 400);
        let label = format!("meet-{}", (b'a' + i as u8) as char);
        s.agents.insert(
            c.id,
            AgentSlot {
                agent_id: c.id,
                source: std::sync::Arc::from("claude-code"),
                session_id: std::sync::Arc::from(format!("meeting-{i}").as_str()),
                cwd: std::sync::Arc::from(PathBuf::from("/meeting").as_path()),
                label: label.as_str().into(),
                state: ActivityState::Idle,
                state_started_at: now - back_date,
                created_at: now - back_date,
                last_event_at: now - back_date,
                exiting_at: None,
                pending_idle_at: None,
                desk_index: GlobalDeskIndex(i),
                floor_idx: s.floor_of(GlobalDeskIndex(i)),
                tool_call_count: 0,
                active_ms: 0,
                unknown_cwd: false,
                parent_id: None,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
    }
    // Fill the rest so the floor doesn't look dead around the meeting.
    fill_sample_agents(&mut s, now, n..n_agents);
    eprintln!(
        "MEETING staged {n} agents, warmup={warmup_ms}ms (min desk_dwell {min_dwell}ms − {MEETING_WARMUP_LEAD_MS}ms)"
    );
    Ok((s, warmup_ms))
}

pub(crate) fn dashboard_scene(now: SystemTime) -> SceneState {
    use pixtuoid_core::source::{claude_code, codex, reasonix};
    use pixtuoid_core::state::{ActivityState, ToolKind};
    use std::time::Duration as D;

    // Named to satisfy clippy::type_complexity (a 6-field tuple trips the lint):
    // (label, transcript path, state, parent_id, desk_index, source SOURCE_NAME).
    type DashAgentSpec = (
        &'static str,
        &'static str,
        ActivityState,
        Option<AgentId>,
        usize,
        &'static str,
    );

    let mut s = SceneState::uniform(12);

    let cc_root_id = AgentId::from_transcript_path("/demo/dash_cc_root.jsonl");

    let agents: &[DashAgentSpec] = &[
        (
            "cc·pixtuoid",
            "/demo/dash_cc_root.jsonl",
            ActivityState::Active {
                tool_use_id: Some("tu0".into()),
                detail: Some("Edit: reducer.rs".into()),
                kind: ToolKind::Edit,
            },
            None,
            0,
            claude_code::SOURCE_NAME,
        ),
        (
            "code-explorer",
            "/demo/dash_cc_sub1.jsonl",
            ActivityState::Active {
                tool_use_id: Some("tu1".into()),
                detail: Some("Grep: TODO".into()),
                kind: ToolKind::Search,
            },
            Some(cc_root_id),
            1,
            claude_code::SOURCE_NAME,
        ),
        (
            "code-reviewer",
            "/demo/dash_cc_sub2.jsonl",
            ActivityState::Idle,
            Some(cc_root_id),
            2,
            claude_code::SOURCE_NAME,
        ),
        (
            "cx·sidecar",
            "/demo/dash_cx_root.jsonl",
            ActivityState::Idle,
            None,
            3,
            codex::SOURCE_NAME,
        ),
        (
            "rx·helper",
            "/demo/dash_rx_root.jsonl",
            ActivityState::Waiting {
                reason: "permission?".into(),
            },
            None,
            4,
            reasonix::SOURCE_NAME,
        ),
    ];

    for (label, path, state, parent_id, desk_index, source) in agents {
        let id = AgentId::from_transcript_path(path);
        s.agents.insert(
            id,
            AgentSlot {
                agent_id: id,
                source: std::sync::Arc::from(*source),
                session_id: std::sync::Arc::from(
                    format!("demo-dash-{}", label.replace('·', "-")).as_str(),
                ),
                cwd: std::sync::Arc::from(PathBuf::from("/demo").as_path()),
                label: (*label).into(),
                state: state.clone(),
                state_started_at: now,
                created_at: now - D::from_secs(*desk_index as u64),
                last_event_at: now,
                exiting_at: None,
                pending_idle_at: None,
                desk_index: GlobalDeskIndex(*desk_index),
                floor_idx: s.floor_of(GlobalDeskIndex(*desk_index)),
                tool_call_count: 0,
                active_ms: 0,
                unknown_cwd: false,
                parent_id: *parent_id,
                pid: None,
                model: None,
                effort: None,
                tokens_used: 0,
                last_usage: None,
            },
        );
    }
    s
}

/// The ONE name→waypoint vocabulary shared by `--anim` and `--crop-furniture`
/// ("desk" stays each caller's special case; "meeting" aliases the sofa).
pub(crate) fn waypoint_target(name: &str) -> Option<pixtuoid_scene::layout::WaypointKind> {
    use pixtuoid_scene::layout::WaypointKind as K;
    match name {
        "couch" => Some(K::Couch),
        "meeting" | "sofa" => Some(K::MeetingSofa),
        "chair" => Some(K::MeetingChair),
        "pantry" => Some(K::Pantry),
        "printer" => Some(K::Printer),
        "vending" => Some(K::VendingMachine),
        "island" => Some(K::Island),
        "snackshelf" => Some(K::SnackShelf),
        _ => None,
    }
}

pub(crate) fn anim_scene(
    now: SystemTime,
    target: &str,
    cols: u16,
    rows: u16,
    floor_seed: u64,
    facing: Option<&str>,
) -> (SceneState, u64) {
    use pixtuoid_scene::layout::{CLASSIC_OFFICE_DESKS, Facing, SceneLayout};
    use pixtuoid_scene::pose::{
        is_aimless_cycle, seated_dwell_ms, takes_trip, waypoint_index_for_cycle,
    };

    // The renderer's own buffer: a 2px mismatch shifts the waypoint set and the
    // agent targets the wrong furniture.
    let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(cols, rows);
    let l = SceneLayout::compute_with_seed(buf_w, buf_h, None, floor_seed)
        .expect("anim layout computes");
    let n = l.waypoints.len();

    // "desk" (and any unknown name) resolves None: it is visited via the
    // return-to-desk leg, not a waypoint.
    let target_kind = waypoint_target(target);
    let want_facing = match facing {
        Some("north") => Some(Facing::North),
        Some("south") => Some(Facing::South),
        Some("east") => Some(Facing::East),
        Some("west") => Some(Facing::West),
        _ => None,
    };
    let target_idxs: Vec<usize> = l
        .waypoints
        .iter()
        .enumerate()
        .filter(|(_, w)| Some(w.kind) == target_kind)
        .filter(|(_, w)| want_facing.is_none_or(|f| w.facing == f))
        .map(|(i, _)| i)
        .collect();

    // Which home desk to stage. `--anim-facing` picks one seated that way, so
    // the back-turned pose is reachable at all; desk 0 is always viewer-facing.
    let desk_idx = if target == "desk" {
        let pick = l
            .home_desks
            .iter()
            .position(|d| want_facing.is_none_or(|f| d.facing == f));
        match pick {
            Some(i) => {
                let d = l.home_desks[i];
                eprintln!(
                    "ANIM target=desk buf_pos≈({}, {}) [home desk {i}]",
                    d.at.x, d.at.y
                );
                i
            }
            None => panic!(
                "no home desk faces {:?} at {buf_w}x{buf_h} seed {floor_seed} — staging \
                 another one would render a pose you did not ask for",
                want_facing.expect("a None filter matches desk 0")
            ),
        }
    } else {
        0
    };
    // `desk` resolved above against `home_desks` and aborts there on no-match;
    // it has no waypoint, so falling into the arms below reads as unmatched.
    if target != "desk" {
        if let Some(&i) = target_idxs.first() {
            let p = l.waypoints[i].pos;
            eprintln!(
                "ANIM target={target} buf_pos=({}, {}) [{} matching waypoints, {n} total]",
                p.x,
                p.y,
                target_idxs.len()
            );
        } else {
            panic!(
                "ANIM target={target}{}: no matching waypoint at {buf_w}x{buf_h} seed \
                 {floor_seed}. Staging a DIFFERENT waypoint here is what let three \
                 captures in one review claim a pose they never rendered.",
                want_facing.map_or(String::new(), |f| format!(" facing {f:?}"))
            );
        }
    }

    // Brute-force an agent whose cycle-0 trip lands on the target.
    let path = (0u64..40_000)
        .map(|i| format!("/anim/{target}_{i}.jsonl"))
        .find(|p| {
            let id = AgentId::from_transcript_path(p);
            takes_trip(id, 0)
                && !is_aimless_cycle(id, 0)
                && (target == "desk"
                    || (n > 0 && target_idxs.contains(&waypoint_index_for_cycle(id, 0, n))))
        })
        .unwrap_or_else(|| {
            panic!("no agent id in 40k trips lands on {target} at seed {floor_seed}")
        });

    let id = AgentId::from_transcript_path(&path);
    // The agent's ACTUAL cycle-0 target — NOT the first matching waypoint above,
    // which misleads when several seats match: the agent may sit on a different
    // one, so cropping to that pos shows an empty seat.
    if target != "desk" && n > 0 {
        let wi = waypoint_index_for_cycle(id, 0, n);
        let wp = l.waypoints[wi];
        eprintln!(
            "ANIM agent ACTUAL target = waypoint[{wi}] {:?} facing {:?} at buf_pos=({}, {})",
            wp.kind, wp.facing, wp.pos.x, wp.pos.y
        );
    }
    // Fresh agent at `now` for a clean Seated start. `desk` is the one target
    // whose SUBJECT is the seat, so it pre-rolls to the dwell's MIDPOINT and
    // holds the pose; every other target is about the trip, so it pre-rolls to
    // just before the walk-out. At `dwell - 1s` the desk clip spent 29% of its
    // frames seated and the rest on an agent standing in an aisle.
    let dwell = seated_dwell_ms(id);
    let skip_ms = if target == "desk" {
        dwell / 2
    } else {
        dwell.saturating_sub(1_000)
    };
    eprintln!(
        "ANIM agent seated_dwell={}ms → pre-roll skip={skip_ms}ms",
        seated_dwell_ms(id)
    );

    let mut s = SceneState::uniform(CLASSIC_OFFICE_DESKS);
    s.agents.insert(
        id,
        AgentSlot {
            agent_id: id,
            source: std::sync::Arc::from("claude-code"),
            session_id: std::sync::Arc::from("anim"),
            cwd: std::sync::Arc::from(PathBuf::from("/anim").as_path()),
            label: target.into(),
            state: ActivityState::Idle,
            state_started_at: now,
            created_at: now,
            last_event_at: now,
            exiting_at: None,
            pending_idle_at: None,
            desk_index: GlobalDeskIndex(desk_idx),
            floor_idx: 0,
            tool_call_count: 0,
            active_ms: 0,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        },
    );
    (s, skip_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_scene_stages_a_convergent_group() {
        use pixtuoid_scene::layout::{SceneLayout, WaypointKind};
        use pixtuoid_scene::pose::{
            est_wander_cycle_ms, is_aimless_cycle, seated_dwell_ms, takes_trip,
            waypoint_index_for_cycle,
        };

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let (cols, rows, max_desks) = (208u16, 88u16, 12);
        let (scene, warmup_ms) = meeting_scene(now, 3, cols, rows, 0, max_desks, 12).unwrap();
        assert_eq!(scene.agents.len(), 12, "staged 3 + 9 archetype fillers");

        let (buf_w, buf_h) = pixtuoid::dev::scene_buf_size(cols, rows);
        let layout = SceneLayout::compute_with_seed(buf_w, buf_h, Some(max_desks), 0).unwrap();
        let staged: Vec<_> = scene
            .agents
            .values()
            .filter(|s| s.label.starts_with("meet-"))
            .collect();
        assert_eq!(staged.len(), 3);

        let mut wp_idxs = Vec::new();
        let mut rooms = Vec::new();
        let mut dwells = Vec::new();
        for slot in &staged {
            assert!(slot.desk_index.0 < 3, "staged agents take desks 0..n");
            let id = slot.agent_id;
            let elapsed_ms = now
                .duration_since(slot.state_started_at)
                .unwrap()
                .as_millis() as u64;
            // Mirror `advance_wander`'s bootstrap fast-forward exactly.
            let cycle_n = elapsed_ms / est_wander_cycle_ms(id);
            assert!(cycle_n >= 1, "cycle 0 back-dates under the thinking window");
            assert!(takes_trip(id, cycle_n), "staged cycle must be a trip");
            assert!(!is_aimless_cycle(id, cycle_n), "trip must be directed");
            let wp_idx = waypoint_index_for_cycle(id, cycle_n, layout.waypoints.len());
            let wp = layout.waypoints[wp_idx];
            assert!(
                matches!(
                    wp.kind,
                    WaypointKind::MeetingSofa | WaypointKind::MeetingChair
                ),
                "destination must be a meeting slot, got {:?}",
                wp.kind
            );
            wp_idxs.push(wp_idx);
            rooms.push(wp.room_id.expect("meeting slots carry a room_id"));
            dwells.push(seated_dwell_ms(id));
        }
        wp_idxs.sort_unstable();
        wp_idxs.dedup();
        assert_eq!(wp_idxs.len(), 3, "slots must be distinct (no seat fights)");
        assert!(
            rooms.iter().all(|r| *r == rooms[0]),
            "one room → one chitchat venue"
        );
        let (min_d, max_d) = (*dwells.iter().min().unwrap(), *dwells.iter().max().unwrap());
        assert!(
            max_d - min_d <= MEETING_DWELL_SPREAD_MS,
            "dwell spread {}ms exceeds {}ms",
            max_d - min_d,
            MEETING_DWELL_SPREAD_MS
        );
        assert_eq!(warmup_ms, min_d.saturating_sub(MEETING_WARMUP_LEAD_MS));
    }
}

#[cfg(test)]
mod vocabulary_tests {
    use super::*;

    /// Exhaustive-by-construction inverse of `waypoint_target`: a NEW
    /// `WaypointKind` fails this match until it joins the vocabulary or is
    /// deliberately excluded (PhoneBooth/StandingDesk are stand-in-place spots
    /// with no distinct approach to film or crop to).
    fn canonical_target_name(kind: pixtuoid_scene::layout::WaypointKind) -> Option<&'static str> {
        use pixtuoid_scene::layout::WaypointKind as K;
        match kind {
            K::Couch => Some("couch"),
            K::MeetingSofa => Some("sofa"),
            K::MeetingChair => Some("chair"),
            K::Pantry => Some("pantry"),
            K::Printer => Some("printer"),
            K::VendingMachine => Some("vending"),
            K::Island => Some("island"),
            K::SnackShelf => Some("snackshelf"),
            K::PhoneBooth | K::StandingDesk => None,
        }
    }

    /// `waypoint_target("desk") == None` above says desk is the callers' special
    /// case; this DRIVES the caller. Without it the desk arm fell through to the
    /// no-match `panic!` and every `--anim desk` aborted for a whole branch —
    /// `--anim` is in neither the justfile nor media.json, so nothing else looks.
    #[test]
    fn anim_scene_stages_every_target_it_advertises() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // Every target `--anim` advertises, at the tool's own default size — a
        // sample of two was the coverage when this PR moved DESK_H and
        // INTRA_POD_GAP_Y under every waypoint. A FACING is not swept: a size
        // legitimately need not host a south-facing couch, and the staging
        // panics loudly there by design.
        for (target, facing) in [
            "couch",
            "sofa",
            "chair",
            "pantry",
            "printer",
            "vending",
            "island",
            "snackshelf",
            "desk",
        ]
        .into_iter()
        .map(|t| (t, None))
        .chain([("desk", Some("north")), ("desk", Some("south"))])
        {
            let (scene, _) = anim_scene(now, target, 192, 80, 0, facing);
            assert!(
                !scene.agents.is_empty(),
                "--anim {target}{} staged no agent",
                facing.map_or(String::new(), |f| format!(" --anim-facing {f}"))
            );
        }
    }

    #[test]
    fn anim_and_crop_share_one_waypoint_vocabulary() {
        use pixtuoid_scene::layout::WaypointKind as K;
        for kind in K::ALL.iter().copied() {
            if let Some(name) = canonical_target_name(kind) {
                assert_eq!(waypoint_target(name), Some(kind), "{name} round-trips");
            }
        }
        assert_eq!(
            waypoint_target("meeting"),
            Some(K::MeetingSofa),
            "meeting aliases the sofa"
        );
        assert_eq!(
            waypoint_target("desk"),
            None,
            "desk stays the callers' special case"
        );
    }
}
