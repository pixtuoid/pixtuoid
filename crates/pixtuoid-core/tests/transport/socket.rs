//! The Unix-socket twin of `pipe.rs`, compiled only on Unix: mirror there any
//! behavior pinned here. The Windows half never compiles on a macOS box, so
//! only CI will tell you it drifted.
#![cfg(unix)]
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::sleep;

use pixtuoid_core::source::hook::{HookRouter, HookSocketListener};
use pixtuoid_core::source::{AgentEvent, Source, Transport};

#[tokio::test]
async fn listener_parses_line_and_emits_event() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(path.clone()).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });

    sleep(Duration::from_millis(20)).await;

    let mut s = UnixStream::connect(&path).await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "ses-1",
        "transcript_path": "/p/a.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

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
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(path.clone()).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let mut s = UnixStream::connect(&path).await.unwrap();
    s.write_all(b"not json\n").await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionEnd",
        "session_id": "ses-1",
        "transcript_path": "/p/a.jsonl",
        "cwd": "/repo",
        "reason": "exit"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionEnd { .. }));
    handle.abort();
}

#[tokio::test]
async fn listener_drops_slow_connection_via_timeout() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(path.clone()).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    // The client-side EOF read is what makes this test non-vacuous: without
    // it the test passes even with CONN_TIMEOUT deleted, because the
    // per-connection semaphore alone keeps the accept loop serving.
    let mut slow = UnixStream::connect(&path).await.unwrap();
    sleep(Duration::from_millis(1_200)).await;
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_millis(500), slow.read(&mut buf))
        .await
        .expect("read must complete promptly — CONN_TIMEOUT should have dropped the slow conn")
        .expect("a server-dropped unix conn reads EOF, not an error");
    assert_eq!(n, 0, "server must have closed the slow connection");

    let mut s = UnixStream::connect(&path).await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-timeout",
        "transcript_path": "/p/b.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

// Invalid UTF-8 makes tokio's `next_line()` return an io::Error, a different
// arm than the malformed-JSON serde warn the sibling test hits.
#[tokio::test]
async fn listener_survives_non_utf8_read_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(path.clone()).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let mut bad = UnixStream::connect(&path).await.unwrap();
    bad.write_all(&[0xFF, 0xFE, b'\n']).await.unwrap();
    bad.shutdown().await.unwrap();

    let mut s = UnixStream::connect(&path).await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-bad-read",
        "transcript_path": "/p/c.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

// A second instance must NOT steal the socket: an unconditional unlink would
// leave the live daemon accepting on an anonymous inode forever, with every
// hook-borne signal vanishing.
#[tokio::test]
async fn bind_bails_when_a_live_listener_holds_the_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(path.clone()).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let err = HookSocketListener::bind(path.clone())
        .await
        .expect_err("a second bind on a LIVE socket must fail loudly, not steal it");
    assert!(
        err.downcast_ref::<pixtuoid_core::source::hook::SocketBusy>()
            .is_some(),
        "the busy bind must be the typed SocketBusy so the router can subscribe: {err:#}"
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("another pixtuoid instance"),
        "error must say what is wrong: {msg}"
    );
    assert!(
        msg.contains(&path.display().to_string()),
        "error must name the contended path: {msg}"
    );

    // The losing bind must be side-effect-free — the owner keeps serving.
    let mut s = UnixStream::connect(&path).await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-probe",
        "transcript_path": "/p/probe.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

// Neither std nor tokio unlink the socket file on listener drop, so the file
// alone is not proof of life. The released `<sock>.lock` distinguishes stale
// from live, NOT connect() errnos — a backlog-saturated LIVE daemon also
// yields ECONNREFUSED on macOS.
#[tokio::test]
async fn bind_reclaims_a_stale_socket_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    drop(HookSocketListener::bind(path.clone()).await.unwrap());
    assert!(
        path.exists(),
        "premise: the socket file survives the listener drop (a crash leaves exactly this residue)"
    );

    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let listener = HookSocketListener::bind(path.clone())
        .await
        .expect("a stale socket file must be reclaimed");
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let mut s = UnixStream::connect(&path).await.unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "after-reclaim",
        "transcript_path": "/p/reclaim.jsonl",
        "cwd": "/repo"
    });
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

    let (transport, ev) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transport, Transport::Hook);
    assert!(matches!(ev, AgentEvent::SessionStart { .. }));
    handle.abort();
}

// The socket must be owner-only 0600 the moment it is reachable at the public
// path (temp-name bind + chmod + atomic rename, no umask mutation).
#[tokio::test]
async fn bound_socket_is_owner_only_with_no_temp_residue() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let _listener = HookSocketListener::bind(path.clone()).await.unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "hook socket must be owner-only rw (0600)");

    // At a umask-default mode another local user could open+flock the
    // arbiter and leave every future daemon with no hook plane.
    let lock_mode = std::fs::metadata(dir.path().join("pixtuoid.sock.lock"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(lock_mode, 0o600, "lock file must be owner-only rw (0600)");

    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "pixtuoid.sock".to_string(),
            // The lock is a permanent sibling — never unlinked, because an
            // unlink race re-introduces the TOCTOU it exists to close.
            "pixtuoid.sock.lock".to_string(),
        ],
        "the temp-name bind must leave nothing but the final socket + its lock"
    );
}

// Owner LIVE but its socket file gone, so a connect probe would see NotFound
// — the old "stale" verdict. The lock must still arbitrate.
#[tokio::test]
async fn bind_respects_lock_arbitration_even_without_a_socket_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let _owner = HookSocketListener::bind(path.clone()).await.unwrap();
    std::fs::remove_file(&path).unwrap();

    let err = HookSocketListener::bind(path.clone())
        .await
        .expect_err("a live lock-holder must make a second bind fail, socket file or not");
    assert!(
        err.downcast_ref::<pixtuoid_core::source::hook::SocketBusy>()
            .is_some(),
        "expected the typed SocketBusy: {err:#}"
    );
}

// A LOCK-LESS live owner (an older pixtuoid mid-upgrade, or a squatter) holds
// the socket open but never took `<sock>.lock`, so the lock arbiter alone says
// "stale, reclaim" — only the connect probe stops the theft. The ONLY busy
// path that reaches that probe; every other trips the try-lock first.
#[tokio::test]
async fn bind_defers_to_a_lockless_live_listener_via_the_connect_probe() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");

    // A raw listener creates the socket file and no `.lock` sibling, and need
    // never accept(): the OS backlogs the probe connect.
    let _raw = tokio::net::UnixListener::bind(&path).unwrap();
    assert!(
        !dir.path().join("pixtuoid.sock.lock").exists(),
        "premise: a raw listener creates no lock sibling, so bind will acquire the lock freely"
    );

    let err = HookSocketListener::bind(path.clone())
        .await
        .expect_err("a lock-less but LIVE listener must be deferred to, not reclaimed");
    assert!(
        err.downcast_ref::<pixtuoid_core::source::hook::SocketBusy>()
            .is_some(),
        "the connect-probe-detected live owner must defer via the typed SocketBusy: {err:#}"
    );

    assert!(
        path.exists(),
        "the deferring probe must NOT unlink the live owner's socket file"
    );
    let mut s = UnixStream::connect(&path).await.unwrap();
    s.shutdown().await.unwrap();
    drop(_raw);
}

#[tokio::test]
async fn listener_handles_concurrent_connections() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let (tx, mut rx) = mpsc::channel::<(Transport, AgentEvent)>(64);
    let listener = HookSocketListener::bind(path.clone()).await.unwrap();
    let handle = tokio::spawn(async move { listener.run(tx).await });
    sleep(Duration::from_millis(20)).await;

    let mut handles = Vec::new();
    for i in 0..5 {
        let p = path.clone();
        handles.push(tokio::spawn(async move {
            let mut s = UnixStream::connect(&p).await.unwrap();
            let payload = serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": format!("ses-{i}"),
                "transcript_path": format!("/p/{i}.jsonl"),
                "cwd": "/repo"
            });
            let mut line = serde_json::to_vec(&payload).unwrap();
            line.push(b'\n');
            s.write_all(&line).await.unwrap();
            s.shutdown().await.unwrap();
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

// A SubagentStop on the shared socket must reach the downstream channel
// unchanged AND land its child id in the one shared un-claim handle, for
// EVERY source (stamped codex here).
#[tokio::test]
async fn hook_router_tee_captures_child_ends_from_the_shared_socket() {
    use pixtuoid_core::AgentId;
    use pixtuoid_core::source::jsonl::ChildEndUnclaims;

    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("pixtuoid.sock");

    let unclaims = ChildEndUnclaims::new();
    let router = HookRouter::new(sock.clone()).with_child_end_unclaims(Some(unclaims.clone()));
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
    let mut s = UnixStream::connect(&sock).await.unwrap();
    let mut line = serde_json::to_vec(&payload).unwrap();
    line.push(b'\n');
    s.write_all(&line).await.unwrap();
    s.shutdown().await.unwrap();

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
