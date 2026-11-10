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

use pixtuoid_core::source::hook::{HookRouter, HookSocketListener, SOCKET_ENV, SOCKET_FLAG};
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
    assert_names_both_settings(&msg);

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

// A mistyped `--socket` or `PIXTUOID_SOCKET` can name the user's own file
// (REVIEW.md: a pre-existing endpoint is hostile).
#[tokio::test]
async fn bind_refuses_to_reclaim_a_file_that_is_not_a_socket() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "precious").unwrap();
    let err = HookSocketListener::bind(path.clone())
        .await
        .expect_err("a regular file is not a stale socket");
    let text = format!("{err:#}");
    assert!(text.contains("notes.txt"), "{text}");
    assert_names_both_settings(&text);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
}

#[tokio::test]
async fn bind_refuses_to_replace_a_dangling_symlink() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    std::os::unix::fs::symlink(dir.path().join("gone"), &path).unwrap();
    HookSocketListener::bind(path.clone())
        .await
        .expect_err("a dangling symlink is not a stale socket");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink is left as it was"
    );
}

// bind goes through a `.tmp` sibling, so a path that fits a unix socket
// alone can still be too long.
#[tokio::test]
async fn bind_takes_a_path_of_its_budget_and_refuses_one_byte_more() {
    let dir = TempDir::new().unwrap();
    // SAFETY: sockaddr_un is plain old data; all-zero is a valid value.
    let sun_path = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    let of_len = |n: usize| {
        let base = dir.path().join("s");
        let fill = n.saturating_sub(base.as_os_str().len());
        let path = dir.path().join(format!("s{}", "x".repeat(fill)));
        assert_eq!(path.as_os_str().len(), n, "premise: the path is {n} bytes");
        path
    };
    let too_long = of_len(sun_path - 1);
    let err = HookSocketListener::bind(too_long.clone())
        .await
        .expect_err("its temp sibling cannot fit");
    let text = format!("{err:#}");
    assert!(
        text.contains(&format!("{} is too long", too_long.display())),
        "{text}"
    );
    assert_names_both_settings(&text);
    let budget: usize = text
        .split("at most ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the refusal states its budget: {text}"));
    HookSocketListener::bind(of_len(budget))
        .await
        .expect("a path of exactly the budget binds");
    let over = of_len(budget + 1);
    let err = HookSocketListener::bind(over.clone())
        .await
        .expect_err("one byte over the budget");
    assert!(
        format!("{err:#}").contains(&format!("{} is too long", over.display())),
        "{err:#}"
    );
}

// A path the temp name is shorter than (trailing slashes) fails as an error,
// never as an arithmetic panic in the length check.
#[tokio::test]
async fn bind_refuses_a_trailing_slash_path_without_panicking() {
    let dir = TempDir::new().unwrap();
    let mut path = dir.path().join("x").into_os_string();
    path.push("//////");
    HookSocketListener::bind(std::path::PathBuf::from(path))
        .await
        .expect_err("a socket path can't end in a slash");
}

#[tokio::test]
async fn bind_leaves_a_live_socket_at_its_temp_name_alone() {
    let dir = TempDir::new().unwrap();
    let other = dir.path().join("foo.tmp");
    let (tx, _rx) = mpsc::channel::<(Transport, AgentEvent)>(16);
    let owner = HookSocketListener::bind(other.clone()).await.unwrap();
    let handle = tokio::spawn(async move { owner.run(tx).await });
    sleep(Duration::from_millis(20)).await;
    let endpoint = dir.path().join("foo");
    let err = HookSocketListener::bind(endpoint.clone())
        .await
        .expect_err("the temp name is another endpoint's live socket");
    assert!(
        err.downcast_ref::<pixtuoid_core::source::hook::SocketBusy>()
            .is_none(),
        "another endpoint's owner is not this one's to subscribe to: {err:#}"
    );
    let text = format!("{err:#}");
    assert!(
        text.contains(&format!(
            "bind for {} listens at its temp name first: {} is a live hook socket",
            endpoint.display(),
            other.display()
        )),
        "{text}"
    );
    assert_names_both_settings(&text);
    UnixStream::connect(&other)
        .await
        .expect("the other endpoint still answers");
    handle.abort();
}

#[tokio::test]
async fn bind_refuses_to_reclaim_a_file_at_its_temp_name() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notes.tmp");
    std::fs::write(&file, "precious").unwrap();
    let endpoint = dir.path().join("notes");
    let err = HookSocketListener::bind(endpoint.clone())
        .await
        .expect_err("a regular file at the temp name is not a stale socket");
    let text = format!("{err:#}");
    assert!(
        text.contains(&format!(
            "bind for {} listens at its temp name first: {} exists and is not a socket",
            endpoint.display(),
            file.display()
        )),
        "{text}"
    );
    assert_names_both_settings(&text);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "precious");
}

/// A refusal can't know whether the flag or the variable chose the path, so it
/// names both.
fn assert_names_both_settings(text: &str) {
    assert!(text.contains(&format!("--{SOCKET_FLAG}")), "{text}");
    assert!(text.contains(SOCKET_ENV), "{text}");
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

// The lock says who holds an endpoint, whatever a connect finds: a tmp cleaner took
// this owner's socket file, and it must not read as missing.
#[tokio::test]
async fn a_held_lock_with_no_socket_file_is_silent() {
    use pixtuoid_core::source::hook::{EndpointStatus, endpoint_status};
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pixtuoid.sock");
    let _owner = HookSocketListener::bind(path.clone()).await.unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(endpoint_status(&path).await, EndpointStatus::Silent);
}

// Nothing can listen at any of these, so none may read as a listener gone quiet.
#[tokio::test]
async fn an_endpoint_no_bind_can_take_is_unbindable() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use pixtuoid_core::source::hook::{EndpointStatus, endpoint_status};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, "").unwrap();
    let mut refused = vec![
        dir.path().join(format!("{}.sock", "x".repeat(200))),
        dir.path().join("gone").join("hook.sock"),
        file.join("hook.sock"),
    ];
    // Root passes whatever the mode says.
    if std::fs::metadata(dir.path()).unwrap().uid() != 0 {
        let read_only = dir.path().join("read-only");
        std::fs::create_dir(&read_only).unwrap();
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500)).unwrap();
        refused.push(read_only.join("hook.sock"));
    }
    for path in refused {
        let status = endpoint_status(&path).await;
        assert!(
            matches!(status, EndpointStatus::Unbindable(_)),
            "{path:?}: {status:?}"
        );
    }
}
