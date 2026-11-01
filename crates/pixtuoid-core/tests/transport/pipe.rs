//! The named-pipe twin of `socket.rs`, compiled only on Windows: mirror here any
//! behavior pinned there. Nothing enforces the parity, and the rosters differ.
#![cfg(windows)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use pixtuoid_core::source::daemon::PresenceMsg;
use pixtuoid_core::source::hook::{HookRouter, HookSocketListener};
use pixtuoid_core::source::{AgentEvent, Source, Transport};

type Hooks = mpsc::Receiver<(Transport, AgentEvent)>;

fn pipe_name(suffix: &str) -> String {
    format!(r"\\.\pipe\pixtuoid-test-{}-{}", std::process::id(), suffix)
}

/// Named pipes require the client to retry on ERROR_PIPE_BUSY (os error 231) —
/// the server is between instances (the create-next-before-handoff window).
async fn connect_client(name: &str) -> tokio::net::windows::named_pipe::NamedPipeClient {
    const MAX_TRIES: u32 = 20;
    for attempt in 0..MAX_TRIES {
        match ClientOptions::new().open(name) {
            Ok(c) => return c,
            Err(e) if e.raw_os_error() == Some(231) => {
                sleep(Duration::from_millis(50)).await;
            }
            Err(e) if attempt == 0 && e.kind() == std::io::ErrorKind::NotFound => {
                sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("connect_client({name}) failed: {e}"),
        }
    }
    panic!("connect_client({name}): still ERROR_PIPE_BUSY after {MAX_TRIES} tries");
}

#[tokio::test]
async fn listener_parses_line_and_emits_event() {
    let name = pipe_name("parse");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(&name).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });

    sleep(Duration::from_millis(20)).await;

    let mut c = connect_client(&name).await;
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "ses-1",
        "transcript_path": "/p/a.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    c.write_all(&line).await.unwrap();
    drop(c);

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));

    handle.abort();
}

#[tokio::test]
async fn listener_skips_malformed_line_and_keeps_going() {
    let name = pipe_name("malformed");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(&name).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let mut c = connect_client(&name).await;
    c.write_all(b"not json\n").await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionEnd",
        "session_id": "ses-1",
        "transcript_path": "/p/a.jsonl",
        "cwd": "/repo",
        "reason": "exit"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    c.write_all(&line).await.unwrap();
    drop(c);

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionEnd { .. }));
    handle.abort();
}

#[tokio::test]
async fn listener_survives_non_utf8_read_error() {
    let name = pipe_name("nonutf8");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(&name).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let mut bad = connect_client(&name).await;
    bad.write_all(&[0xFF, 0xFE, b'\n']).await.unwrap();
    drop(bad);

    let mut c = connect_client(&name).await;
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-bad-read",
        "transcript_path": "/p/c.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    c.write_all(&line).await.unwrap();
    drop(c);

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

#[tokio::test]
async fn listener_handles_concurrent_connections() {
    let name = pipe_name("concurrent");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let listener = HookSocketListener::bind(&name).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    // Also pins create-next-before-handoff: a handoff gap would give some
    // clients NotFound, failing here.
    let mut handles = Vec::new();
    for i in 0..5usize {
        let n = name.clone();
        handles.push(tokio::spawn(async move {
            let mut c = connect_client(&n).await;
            let payload = serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": format!("ses-{i}"),
                "transcript_path": format!("/p/{i}.jsonl"),
                "cwd": "/repo"
            });
            let mut line = serde_json::to_vec(&payload).unwrap();
            line.push(b'\n');
            c.write_all(&line).await.unwrap();
            drop(c);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let mut count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        count += 1;
        if count == 5 {
            break;
        }
    }
    assert_eq!(
        count, 5,
        "all 5 concurrent connections should produce events"
    );
    handle.abort();
}

#[tokio::test]
async fn listener_drops_slow_connection_via_timeout() {
    let name = pipe_name("slowconn");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(&name).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    // The client-side read below is what gives this test teeth: without it the
    // test passes even with CONN_TIMEOUT deleted, because the per-connection
    // semaphore alone keeps the accept loop serving a second connection.
    let mut slow = connect_client(&name).await;
    sleep(Duration::from_millis(1_200)).await;
    let mut buf = [0u8; 1];
    let res = tokio::time::timeout(Duration::from_millis(500), slow.read(&mut buf))
        .await
        .expect("read must complete promptly — CONN_TIMEOUT should have dropped the slow conn");
    match res {
        Ok(0) | Err(_) => {}
        Ok(n) => panic!("unexpected {n} bytes from a dropped connection"),
    }

    let mut c = connect_client(&name).await;
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-timeout",
        "transcript_path": "/p/b.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    c.write_all(&line).await.unwrap();
    drop(c);

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

#[tokio::test]
async fn clients_reconnect_after_open_close_churn() {
    let name = pipe_name("churn");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(&name).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    // Zero-byte open-and-drop cycles: the server sees a connect + immediate
    // EOF/broken-pipe on each, triggering its instance-recreate path.
    for _ in 0..5 {
        let _c = connect_client(&name).await;
        sleep(Duration::from_millis(10)).await;
    }

    let mut c = connect_client(&name).await;
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-churn",
        "transcript_path": "/p/churn.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    c.write_all(&line).await.unwrap();
    drop(c);

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(1_000), rx.recv())
        .await
        .expect("timed out waiting for event after churn")
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

/// Windows reports a contended pipe as ACCESS_DENIED (the first instance holds
/// `first_pipe_instance(true)`), which must map to the typed `SocketBusy` for
/// Unix parity.
#[tokio::test]
async fn second_listener_on_same_name_fails_with_typed_socket_busy() {
    let name = pipe_name("squat");
    let _first = HookSocketListener::bind(&name).await.unwrap();
    let err = HookSocketListener::bind(&name)
        .await
        .expect_err("bind on an already-owned pipe must fail, not silently queue");
    assert!(
        err.downcast_ref::<pixtuoid_core::source::hook::SocketBusy>()
            .is_some(),
        "the busy bind must be the typed SocketBusy so the source can degrade: {err:#}"
    );
    assert!(
        format!("{err:#}").contains(&name),
        "error must name the contended pipe: {err:#}"
    );
}

/// How long a test waits on a state a router reaches asynchronously.
const PATIENCE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(25);

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

fn spawn_router(name: &str) -> (JoinHandle<anyhow::Result<()>>, Hooks) {
    let (tx, rx) = mpsc::channel(64);
    let router = Box::new(HookRouter::new(std::path::PathBuf::from(name)));
    (tokio::spawn(router.run(tx)), rx)
}

/// One connection carrying `bytes`, as the shim sends them. `false` while
/// nothing listens.
async fn deliver(name: &str, bytes: &[u8]) -> bool {
    match ClientOptions::new().open(name) {
        Ok(mut c) => c.write_all(bytes).await.is_ok(),
        Err(_) => false,
    }
}

/// The session the next `SessionStart` on `rx` opened.
async fn next_session(rx: &mut Hooks) -> String {
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
async fn until_seen(name: &str, session: &str, rxs: &mut [&mut Hooks]) {
    for rx in rxs {
        tokio::time::timeout(PATIENCE, async {
            loop {
                deliver(name, &session_start(session)).await;
                if tokio::time::timeout(POLL, next_session(rx)).await.is_ok() {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{session} never arrived"));
    }
}

/// Drop whatever `rxs` have queued: a probe that `until_seen` sent again.
async fn settle(rxs: &mut [&mut Hooks]) {
    sleep(POLL).await;
    for rx in rxs {
        while rx.try_recv().is_ok() {}
    }
}

#[tokio::test]
async fn a_second_router_receives_the_owners_payloads_in_order() {
    let name = pipe_name("relay-order");
    let (owner, mut owner_rx) = spawn_router(&name);
    until_seen(&name, "owner-up", &mut [&mut owner_rx]).await;
    let (follower, mut rx) = spawn_router(&name);
    until_seen(&name, "subscribed", &mut [&mut rx]).await;
    settle(&mut [&mut owner_rx, &mut rx]).await;

    let mut lines = session_start("first");
    lines.extend(session_start("second"));
    assert!(deliver(&name, &lines).await);
    sleep(POLL).await;
    assert!(deliver(&name, &session_start("third")).await);

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
    follower.abort();
}

#[tokio::test]
async fn a_follower_takes_the_endpoint_when_its_owner_leaves() {
    let name = pipe_name("relay-takeover");
    let (owner, mut owner_rx) = spawn_router(&name);
    until_seen(&name, "owner-up", &mut [&mut owner_rx]).await;
    let (one, mut one_rx) = spawn_router(&name);
    let (two, mut two_rx) = spawn_router(&name);
    until_seen(&name, "subscribed", &mut [&mut one_rx, &mut two_rx]).await;

    owner.abort();
    let _ = owner.await;

    // One takes the endpoint and the other subscribes to it, whichever way the
    // race falls.
    until_seen(&name, "taken-over", &mut [&mut one_rx, &mut two_rx]).await;
    assert!(!one.is_finished() && !two.is_finished());
    one.abort();
    two.abort();
}

// The daemon side channel is wired on a follower too, or its gateway mascot
// never appears.
#[tokio::test]
async fn a_follower_receives_daemon_presence() {
    let name = pipe_name("relay-presence");
    let (owner, mut owner_rx) = spawn_router(&name);
    until_seen(&name, "owner-up", &mut [&mut owner_rx]).await;

    let (tx, mut rx) = mpsc::channel(64);
    let (presence_tx, mut presence_rx) = mpsc::unbounded_channel::<PresenceMsg>();
    let router =
        HookRouter::new(std::path::PathBuf::from(&name)).with_presence_tx(Some(presence_tx));
    let follower = tokio::spawn(Box::new(router).run(tx));
    until_seen(&name, "subscribed", &mut [&mut rx]).await;

    let gateway = b"{\"_pixtuoid_source\":\"openclaw\",\"type\":\"gateway_start\",\
                    \"gatewayPort\":18789,\"_pid\":4242}\n";
    assert!(deliver(&name, gateway).await);
    let msg = tokio::time::timeout(PATIENCE, presence_rx.recv())
        .await
        .expect("the gateway reaches the follower")
        .unwrap();
    assert_eq!(msg.key.source(), "openclaw");
    owner.abort();
    follower.abort();
}

// An owner that never acknowledges is what an older pixtuoid looks like: its
// hook plane stays out of reach, so the footer must say so.
#[tokio::test]
async fn a_router_that_cannot_subscribe_reports_a_death() {
    use pixtuoid_core::source::manager::{SourceDeath, SourceManager};

    let name = pipe_name("relay-refused");
    let _owner = HookSocketListener::bind(&name).await.unwrap();

    let (tx, _rx) = mpsc::channel::<(Transport, AgentEvent)>(8);
    let (deaths_tx, deaths_rx) = tokio::sync::watch::channel(Vec::<SourceDeath>::new());
    let handles = SourceManager::new()
        .with_source(Box::new(HookRouter::new(std::path::PathBuf::from(&name))))
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

#[tokio::test]
async fn hook_router_tee_captures_child_ends_from_the_shared_socket() {
    use pixtuoid_core::AgentId;
    use pixtuoid_core::source::jsonl::ChildEndUnclaims;

    let name = pipe_name("tee");

    let unclaims = ChildEndUnclaims::new();
    let router = HookRouter::new(std::path::PathBuf::from(&name))
        .with_child_end_unclaims(Some(unclaims.clone()));
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(32);
    let task = tokio::spawn(async move { Box::new(router).run(tx).await });
    sleep(Duration::from_millis(50)).await;

    let child_uuid = "0d000000-0000-7000-8000-0000000000d1";
    let expected = AgentId::from_parts("codex", child_uuid);
    let payload = serde_json::json!({
        "hook_event_name": "SubagentStop",
        "session_id": "parent-sess",
        "agent_id": child_uuid,
        "_pixtuoid_source": "codex",
    });
    let mut c = connect_client(&name).await;
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    c.write_all(&line).await.unwrap();
    drop(c);

    let (transport, ev) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (transport, ev) = rx.recv().await.expect("router must stay alive");
            if matches!(ev, AgentEvent::SessionEnd { .. }) {
                return (transport, ev);
            }
        }
    })
    .await
    .expect("the SubagentStop must reach the downstream channel through the tee");
    assert_eq!(
        transport,
        Transport::Hook,
        "the Transport tag flows through"
    );
    assert_eq!(
        ev,
        AgentEvent::SessionEnd {
            agent_id: expected,
            as_child: true
        },
        "event parity: the decoded end is forwarded unchanged"
    );
    assert_eq!(
        unclaims.take_matching(|id| *id == expected),
        vec![expected],
        "the child id must land in the shared un-claim handle"
    );
    task.abort();
}
