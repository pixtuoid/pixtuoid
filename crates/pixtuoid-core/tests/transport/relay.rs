//! The hook relay on Unix and Windows alike: routers sharing one endpoint.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::tap::Endpoint;
use pixtuoid_core::source::daemon::PresenceMsg;
use pixtuoid_core::source::hook::{
    EndpointStatus, HookActivity, HookRouter, HookSocketListener, LiveConnected, endpoint_status,
};
use pixtuoid_core::source::jsonl::ChildEndUnclaims;
use pixtuoid_core::source::manager::{SourceDeath, SourceManager};
use pixtuoid_core::source::{AgentEvent, Source, TaggedReceiver, Transport};

/// How long a test waits on a state a router reaches asynchronously.
const PATIENCE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(25);

/// `name`'s endpoint and the directory that keeps it private to this test.
fn endpoint(name: &str) -> (tempfile::TempDir, Endpoint) {
    let dir = tempfile::tempdir().unwrap();
    let at = Endpoint::in_dir(dir.path(), name);
    (dir, at)
}

fn session_start(session: &str) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session,
        "transcript_path": format!("/p/{session}.jsonl"),
        "cwd": "/repo"
    }))
    .unwrap();
    line.push(b'\n');
    line
}

fn spawn_router(at: &Endpoint) -> (JoinHandle<anyhow::Result<()>>, TaggedReceiver) {
    let (tx, rx) = mpsc::channel(64);
    let router = Box::new(HookRouter::new(at.path().to_path_buf()));
    (tokio::spawn(router.run(tx)), rx)
}

/// The session the next `SessionStart` on `rx` opened.
async fn next_session(rx: &mut TaggedReceiver) -> String {
    loop {
        let (transport, ev) = rx.recv().await.expect("the router must stay alive");
        assert_eq!(transport, Transport::Hook);
        if let AgentEvent::SessionStart { session_id, .. } = ev {
            return session_id;
        }
    }
}

/// Keep delivering `session` until every one of `rxs` has seen it, which is
/// the only sign that a router has subscribed or taken the endpoint.
async fn until_seen(at: &Endpoint, session: &str, rxs: &mut [&mut TaggedReceiver]) {
    for rx in rxs {
        tokio::time::timeout(PATIENCE, async {
            loop {
                let _ = at.send(&session_start(session)).await;
                let seen = async { while next_session(rx).await != session {} };
                if tokio::time::timeout(POLL, seen).await.is_ok() {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{session} never arrived"));
    }
}

/// Drop whatever `rxs` have queued: a probe that `until_seen` sent again.
async fn settle(rxs: &mut [&mut TaggedReceiver]) {
    sleep(POLL).await;
    for rx in rxs {
        while rx.try_recv().is_ok() {}
    }
}

#[tokio::test]
async fn a_second_router_receives_the_owners_payloads_in_order() {
    let (_dir, at) = endpoint("order");
    let (owner, mut owner_rx) = spawn_router(&at);
    until_seen(&at, "owner-up", &mut [&mut owner_rx]).await;
    let (subscriber, mut rx) = spawn_router(&at);
    until_seen(&at, "subscribed", &mut [&mut rx]).await;
    settle(&mut [&mut owner_rx, &mut rx]).await;

    let mut lines = session_start("first");
    lines.extend(session_start("second"));
    at.send(&lines).await.unwrap();
    sleep(POLL).await;
    at.send(&session_start("third")).await.unwrap();

    for rx in [&mut owner_rx, &mut rx] {
        let seen = tokio::time::timeout(PATIENCE, async {
            [
                next_session(rx).await,
                next_session(rx).await,
                next_session(rx).await,
            ]
        })
        .await
        .expect("every payload reaches both routers");
        assert_eq!(seen, ["first", "second", "third"]);
    }
    owner.abort();
    subscriber.abort();
}

#[tokio::test]
async fn a_subscriber_takes_the_endpoint_when_its_owner_leaves() {
    let (_dir, at) = endpoint("takeover");
    let (owner, mut owner_rx) = spawn_router(&at);
    until_seen(&at, "owner-up", &mut [&mut owner_rx]).await;
    let (one, mut one_rx) = spawn_router(&at);
    let (two, mut two_rx) = spawn_router(&at);
    until_seen(&at, "subscribed", &mut [&mut one_rx, &mut two_rx]).await;

    owner.abort();
    let _ = owner.await;

    // One takes the endpoint and the other subscribes to it, whichever way the
    // race falls.
    until_seen(&at, "taken-over", &mut [&mut one_rx, &mut two_rx]).await;
    assert!(!one.is_finished() && !two.is_finished());
    one.abort();
    two.abort();
}

// The daemon side channel is wired on a subscriber too, or its gateway mascot
// never appears.
#[tokio::test]
async fn a_subscriber_receives_daemon_presence() {
    let (_dir, at) = endpoint("presence");
    let (owner, mut owner_rx) = spawn_router(&at);
    until_seen(&at, "owner-up", &mut [&mut owner_rx]).await;

    let (tx, mut rx) = mpsc::channel(64);
    let (presence_tx, mut presence_rx) = mpsc::unbounded_channel::<PresenceMsg>();
    let router = HookRouter::new(at.path().to_path_buf()).with_presence_tx(Some(presence_tx));
    let subscriber = tokio::spawn(Box::new(router).run(tx));
    until_seen(&at, "subscribed", &mut [&mut rx]).await;

    let gateway = b"{\"_pixtuoid_source\":\"openclaw\",\"type\":\"gateway_start\",\
                    \"gatewayPort\":18789,\"_pid\":4242}\n";
    at.send(gateway).await.unwrap();
    let msg = tokio::time::timeout(PATIENCE, presence_rx.recv())
        .await
        .expect("the gateway reaches the subscriber")
        .unwrap();
    assert_eq!(msg.key.source(), "openclaw");
    owner.abort();
    subscriber.abort();
}

// The #246 tee runs on a subscriber too, or its child ends go unclaimed.
#[tokio::test]
async fn a_subscriber_tees_child_ends() {
    let (_dir, at) = endpoint("tee");
    let (owner, mut owner_rx) = spawn_router(&at);
    until_seen(&at, "owner-up", &mut [&mut owner_rx]).await;

    let (tx, mut rx) = mpsc::channel(64);
    let unclaims = ChildEndUnclaims::new();
    let router =
        HookRouter::new(at.path().to_path_buf()).with_child_end_unclaims(Some(unclaims.clone()));
    let subscriber = tokio::spawn(Box::new(router).run(tx));
    until_seen(&at, "subscribed", &mut [&mut rx]).await;

    let child_uuid = "0d000000-0000-7000-8000-0000000000d1";
    let child = AgentId::from_parts("codex", child_uuid);
    let mut stop = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SubagentStop",
        "session_id": "parent-sess",
        "agent_id": child_uuid,
        "_pixtuoid_source": "codex",
    }))
    .unwrap();
    stop.push(b'\n');
    at.send(&stop).await.unwrap();

    tokio::time::timeout(PATIENCE, async {
        loop {
            let (_, ev) = rx.recv().await.expect("the router must stay alive");
            if matches!(ev, AgentEvent::SessionEnd { .. }) {
                return;
            }
        }
    })
    .await
    .expect("the child end reaches the subscriber");
    assert_eq!(unclaims.take_matching(|id| *id == child), vec![child]);
    owner.abort();
    subscriber.abort();
}

// An owner that never acknowledges is what an older pixtuoid looks like: its
// hook plane stays out of reach, so the footer must say so.
#[tokio::test]
async fn a_router_that_cannot_subscribe_reports_a_death() {
    let (_dir, at) = endpoint("refused");
    let _owner = HookSocketListener::bind(at.path()).await.unwrap();

    let (tx, _rx) = mpsc::channel::<(Transport, AgentEvent)>(8);
    let (deaths_tx, deaths_rx) = tokio::sync::watch::channel(Vec::<SourceDeath>::new());
    let handles = SourceManager::new()
        .with_source(Box::new(HookRouter::new(at.path().to_path_buf())))
        .spawn_with_health(tx, deaths_tx);
    for h in handles {
        tokio::time::timeout(PATIENCE, h)
            .await
            .expect("the router gives up instead of retrying forever")
            .unwrap();
    }
    let deaths = deaths_rx.borrow();
    assert_eq!(deaths.len(), 1, "got {deaths:?}");
}

async fn live(at: &Endpoint) -> HookActivity {
    match endpoint_status(at.path()).await {
        EndpointStatus::Live(activity) => activity,
        other => panic!("the endpoint's owner must answer, got {other:?}"),
    }
}

// Round trips on Windows too: a named pipe answering one line is the path only
// `windows-test` reaches.
#[tokio::test]
async fn an_owner_answers_status_from_its_live_gate_and_last_hooks() {
    let (_dir, at) = endpoint("status");
    let (tx, mut rx) = mpsc::channel(64);
    let connected: LiveConnected = Arc::new(Mutex::new(HashSet::from(["codex".to_string()])));
    let router =
        HookRouter::new(at.path().to_path_buf()).with_connected(Some(Arc::clone(&connected)));
    let owner = tokio::spawn(Box::new(router).run(tx));
    until_seen(&at, "owner-up", &mut [&mut rx]).await;

    let activity = live(&at).await;
    assert_eq!(
        activity.connected,
        Some(BTreeSet::from(["codex".to_string()]))
    );
    assert_eq!(
        activity.last_hook_ago_ms.keys().collect::<Vec<_>>(),
        ["claude-code"]
    );

    connected.lock().unwrap().insert("cursor".to_string());
    assert_eq!(
        live(&at).await.connected,
        Some(BTreeSet::from(["codex".to_string(), "cursor".to_string()])),
        "the gate is read when asked, so a panel toggle shows at once"
    );

    at.send(&session_start("after-status")).await.unwrap();
    assert_eq!(next_session(&mut rx).await, "after-status");
    owner.abort();
}

#[tokio::test]
async fn an_unwired_gate_is_absent_from_status() {
    let (_dir, at) = endpoint("unwired");
    let (owner, mut rx) = spawn_router(&at);
    until_seen(&at, "owner-up", &mut [&mut rx]).await;
    assert_eq!(live(&at).await.connected, None);
    owner.abort();
}

// A status is the owner's: "since this instance took the endpoint" has to stay
// true across a takeover.
#[tokio::test]
async fn a_takeover_starts_its_activity_from_nothing() {
    let (_dir, at) = endpoint("takeover-status");
    let (owner, mut owner_rx) = spawn_router(&at);
    until_seen(&at, "owner-up", &mut [&mut owner_rx]).await;
    let (next, mut next_rx) = spawn_router(&at);
    until_seen(&at, "subscribed", &mut [&mut next_rx]).await;
    assert!(!live(&at).await.last_hook_ago_ms.is_empty());

    owner.abort();
    let _ = owner.await;
    let taken = tokio::time::timeout(PATIENCE, async {
        loop {
            match endpoint_status(at.path()).await {
                EndpointStatus::Live(activity) => return activity,
                _ => sleep(POLL).await,
            }
        }
    })
    .await
    .expect("the subscriber takes the endpoint");
    assert!(taken.last_hook_ago_ms.is_empty(), "got {taken:?}");
    next.abort();
}

// Only a payload that did something is a sighting: grok and cursor cross-fire
// claude-code's installed hook with their own envelope, and that must not read
// as Claude Code firing.
#[tokio::test]
async fn only_a_payload_that_decoded_to_something_is_a_sighting() {
    let (_dir, at) = endpoint("sighting");
    let (tx, mut rx) = mpsc::channel(64);
    let (presence_tx, mut presence_rx) = mpsc::unbounded_channel::<PresenceMsg>();
    let router = HookRouter::new(at.path().to_path_buf()).with_presence_tx(Some(presence_tx));
    let owner = tokio::spawn(Box::new(router).run(tx));

    let unseen = |source: &str| {
        let mut line = serde_json::to_vec(&serde_json::json!({
            "_pixtuoid_source": source,
            "hookEventName": "SessionStart",
            "session_id": "cross-fired",
        }))
        .unwrap();
        line.push(b'\n');
        line
    };
    tokio::time::timeout(PATIENCE, async {
        while at.send(&unseen("claude-code")).await.is_err() {
            sleep(POLL).await;
        }
    })
    .await
    .expect("the router takes the endpoint");
    at.send(&unseen("no-such-source")).await.unwrap();
    at.send(
        b"{\"_pixtuoid_source\":\"openclaw\",\"type\":\"gateway_start\",\
               \"gatewayPort\":18789,\"_pid\":4242}\n",
    )
    .await
    .unwrap();
    tokio::time::timeout(PATIENCE, presence_rx.recv())
        .await
        .expect("the gateway's presence arrives")
        .unwrap();
    assert!(rx.try_recv().is_err(), "neither decoded to an event");
    assert_eq!(
        live(&at).await.last_hook_ago_ms.keys().collect::<Vec<_>>(),
        ["openclaw"]
    );
    owner.abort();
}

#[tokio::test]
async fn nothing_listening_is_absent() {
    let (_dir, at) = endpoint("absent");
    assert_eq!(endpoint_status(at.path()).await, EndpointStatus::Absent);
}

// Bound but not reading is what a saturated owner looks like: the connect lands
// in its backlog, so only the query's own deadline ends the wait.
#[tokio::test]
async fn an_owner_that_never_reads_is_silent() {
    let (_dir, at) = endpoint("silent");
    let _owner = HookSocketListener::bind(at.path()).await.unwrap();
    assert_eq!(endpoint_status(at.path()).await, EndpointStatus::Silent);
}
