//! The hook relay on Unix and Windows alike: routers sharing one endpoint.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use pixtuoid_core::AgentId;
use pixtuoid_core::harness::tap::Endpoint;
use pixtuoid_core::source::daemon::PresenceMsg;
use pixtuoid_core::source::hook::{HookRouter, HookSocketListener};
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
