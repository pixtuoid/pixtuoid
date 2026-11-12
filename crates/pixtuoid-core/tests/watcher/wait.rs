//! The waits every watcher test binary shares, `native_watch.rs` by `#[path]`:
//! each binary's `crate::DEADLINE` is the hang guard of its own.

use std::time::Duration;

use tokio::sync::mpsc;

use pixtuoid_core::source::{AgentEvent, Transport};

/// Every event up to the first `done` accepts. `done` names an event the
/// watcher sends after every event a test asserts on, and one read's events
/// arrive in order, so a slow runner delays the verdict but never changes it.
pub(crate) async fn collect_until(
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    what: &str,
    mut done: impl FnMut(&AgentEvent) -> bool,
) -> Vec<(Transport, AgentEvent)> {
    let deadline = tokio::time::Instant::now() + crate::DEADLINE;
    let mut events = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(tagged)) => {
                let reached = done(&tagged.1);
                events.push(tagged);
                if reached {
                    return events;
                }
            }
            Ok(None) => panic!("{what}: the stream closed"),
            Err(_) => panic!("{what}: no end"),
        }
    }
}

/// Every event the watcher sends in the next `window`: nothing but listening
/// proves an absence, and a closed stream proves nothing, so that panics.
pub(crate) async fn collect_for(
    rx: &mut mpsc::Receiver<(Transport, AgentEvent)>,
    window: Duration,
    what: &str,
) -> Vec<(Transport, AgentEvent)> {
    let until = tokio::time::Instant::now() + window;
    let mut events = Vec::new();
    loop {
        match tokio::time::timeout_at(until, rx.recv()).await {
            Ok(Some(tagged)) => events.push(tagged),
            Ok(None) => panic!("{what}: the stream closed"),
            Err(_) => return events,
        }
    }
}
