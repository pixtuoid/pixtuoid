use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::AgentId;
use crate::source::decoder::{CwdExtractor, LineDecoder, SUBAGENTS_DIR};
use crate::source::registry::cwd_extractor_for;
use crate::source::{AgentEvent, TaggedSender, Transport};

use super::health::FailureLatch;
use super::liveness::{probe_admits, revouch_gated_files};
use super::{
    ActivityRecency, HeadLabel, SessionEndChecker, Sight, SourceDecoders, TailActivity, WatchCtx,
};

/// Oversized-span skip threshold: a pending span past this is never replayed.
pub(super) const MAX_PENDING_BYTES: u64 = 1 << 20;

/// The path form EVERY id derivation runs on. The `normalize_path_key` fold
/// lives HERE at the seam, not inside each deriver, so every lane mints ONE id
/// per file on Windows while the fixture-fed conformance goldens stay
/// platform-invariant. Identity on Unix.
pub(super) fn id_path(path: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(crate::id::normalize_path_key(&path.to_string_lossy()))
}

/// First-sight decision, shared by EVERY path that can be the first to see a
/// file: seed the cursor at EOF — suppressing SessionStart — when the session
/// is historical (mtime outside `window`) OR already ended. Unifying the gate
/// here is the #85 fix: the post-startup rescan used to bypass it and
/// resurrect an ended/stale session as a phantom live sprite.
///
/// `probe_live` (the liveness vouch) exempts only the RECENCY half — a
/// structural end marker is the source's own first-hand report that no vouch
/// outranks: a vouch answers "is the owning process alive", never "is this
/// session over" (omp's fd vouch fires for a bun tool merely READING an old
/// transcript).
async fn should_seed_at_eof(
    meta: &std::fs::Metadata,
    window: Duration,
    path: &Path,
    check_ended: SessionEndChecker,
    activity_recency: ActivityRecency,
    probe_live: bool,
) -> bool {
    let mtime_recent = meta.modified().ok().is_some_and(|m| within(m, window));
    if !mtime_recent && !probe_live {
        return true;
    }
    let Some(tail) = read_tail(path, TAIL_BYTES).await else {
        // An unreadable tail is no evidence that the session is over.
        return false;
    };
    if check_ended(&tail) {
        return true;
    }
    if probe_live {
        return false;
    }
    // Recent-but-unvouched: were those recent bytes the SESSION writing — the
    // only thing mtime ever proxied, and the half CC falsifies?
    match activity_recency(&tail) {
        TailActivity::At(secs) => !within(
            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            window,
        ),
        TailActivity::SidecarOnly => true,
        TailActivity::Unknown => false,
    }
}

/// `elapsed()` Errs when `t` is in the FUTURE (APFS nanosecond clock jitter, a
/// clock-skewed wire stamp), and a future instant is within any window.
fn within(t: std::time::SystemTime, window: Duration) -> bool {
    t.elapsed().unwrap_or(Duration::ZERO) <= window
}

/// How far back from EOF the first-sight predicates read. ONE value for both
/// the end-marker scan and the activity-recency probe — a split would let a
/// transcript be "ended" and "active" over different bytes.
pub(super) const TAIL_BYTES: u64 = 8192;

pub(super) async fn scan_root(
    root: &Path,
    decoders: SourceDecoders,
    ctx: &WatchCtx<'_>,
    root_health: &mut FailureLatch,
) {
    revouch_gated_files(decoders, ctx).await;
    match tokio::fs::read_dir(root).await {
        Ok(mut read) => {
            if root_health.on_success() {
                tracing::info!(root = ?root, "watched root is readable again");
            }
            loop {
                match read.next_entry().await {
                    Ok(Some(entry)) => walk_jsonl(&entry.path(), decoders, ctx).await,
                    Ok(None) => break,
                    Err(e) => {
                        // Latched, unlike the subdirectory twin below: truncation here
                        // hides every remaining PROJECT, and there is only one root.
                        if root_health.on_failure() {
                            warn!(
                                root = ?root,
                                error = %e,
                                "watched root listing truncated; some sessions will not be \
                                 discovered this pass"
                            );
                        }
                        break;
                    }
                }
            }
        }
        // The watcher never creates its CLI's root, so an uninstalled CLI's is
        // absent: nothing to discover yet, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            if root_health.on_failure() {
                warn!(
                    root = ?root,
                    error = %e,
                    "cannot read watched root; new sessions will not be discovered until it \
                     is readable again"
                );
            }
        }
    }
}

pub(super) async fn walk_jsonl(path: &Path, decoders: SourceDecoders, ctx: &WatchCtx<'_>) {
    let WatchCtx {
        source,
        cursors,
        seen,
        tx,
        window,
        live: _,
    } = *ctx;
    let SourceDecoders {
        decode_line,
        check_ended,
        ..
    } = decoders;
    // symlink_metadata, not metadata: a directory symlink under the root would
    // recurse unboundedly, or walk foreign `.jsonl` trees into this source's id
    // space. The ROOT itself may still be a symlink — only entries are checked.
    let meta = match tokio::fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                // This walk is the last the watcher hears of the path, so retire its
                // map entries or they leak for the process lifetime. NotFound ONLY —
                // a transient EACCES must not drop a live session's cursor.
                cursors.lock().await.remove(path);
                seen.lock().await.remove(path);
            }
            return;
        }
    };
    use crate::source::admit::{Entry, classify};
    // Shared with the offline drivers, so a fixture can never be recorded from a
    // file production would not read (#931).
    let entry = classify(&meta, path, &|p| (decoders.path_filter)(p));
    if entry == Entry::SkipSymlink {
        // debug!, not warn!: a persistent symlink re-logs on every scan pass.
        debug!(path = ?path, "symlinked entry skipped");
        return;
    }
    if entry == Entry::Recurse {
        match tokio::fs::read_dir(path).await {
            Ok(mut read) => loop {
                match read.next_entry().await {
                    Ok(Some(entry)) => Box::pin(walk_jsonl(&entry.path(), decoders, ctx)).await,
                    Ok(None) => break,
                    Err(e) => {
                        // Split from `Ok(None)` for the LOG only; the listing stops
                        // either way. `break` not `continue` — a sticky error spins.
                        debug!(path = ?path, error = %e, "listing truncated");
                        break;
                    }
                }
            },
            // `debug!`, not `scan_root`'s latched `warn!`: subdirs are unbounded and
            // re-walked every scan pass, so a warn here floods.
            Err(e) => debug!(path = ?path, error = %e, "unreadable directory skipped"),
        }
        return;
    }
    if entry != Entry::Take {
        return;
    }

    let file_len = meta.len();

    let (known, cursor_now): (bool, u64) = {
        let cursors_g = cursors.lock().await;
        let entry = cursors_g.get(path).copied();
        (entry.is_some(), entry.unwrap_or(0))
    };
    if !known
        && should_seed_at_eof(
            &meta,
            window,
            path,
            check_ended,
            decoders.activity_recency,
            probe_admits(path, decoders, ctx).await,
        )
        .await
    {
        cursors.lock().await.insert(path.to_path_buf(), file_len);
        return;
    }
    // Reset-to-0 is the LIVE-session resync. Exit-path drains must NOT take it —
    // they pre-park at the new EOF, or the un-claim behind the drain turns this
    // into a ghost replay (`park_if_truncated_below_cursor`).
    if cursor_now > file_len {
        warn!(
            path = ?path,
            file_len,
            cursor = cursor_now,
            "file truncated below cursor, resetting cursor"
        );
        cursors.lock().await.insert(path.to_path_buf(), 0);
        return;
    }
    if cursor_now == file_len {
        return;
    }
    if file_len - cursor_now > MAX_PENDING_BYTES {
        warn!(
            path = ?path,
            pending = file_len - cursor_now,
            max = MAX_PENDING_BYTES,
            "pending bytes over the cap; skipping backlog to end"
        );
        // A skipped span may bury the session-end marker, leaving the slot to the
        // slow stale-sweep. Unconditional (a KNOWN file's span can end mid-skip) and
        // cursor-independent, so it must run before the seed below.
        let ended_in_skip = check_session_ended(path, check_ended).await;
        // Seed to EOF BEFORE the awaited head-read below, so a concurrent
        // walk_jsonl sees `known` and won't re-enter this branch.
        cursors.lock().await.insert(path.to_path_buf(), file_len);
        if ended_in_skip {
            // An already-ENDED span stays unregistered: a SessionStart or seeded
            // Task after the SessionEnd just sent would animate a ghost.
            let id = AgentId::from_parts(source, &decoders.id_derive.id_for(path));
            let _ = tx
                .send((
                    Transport::Jsonl,
                    AgentEvent::SessionEnd {
                        agent_id: id,
                        as_child: false,
                    },
                ))
                .await;
            // Ended AFTER the terminator, as a decoded end is below: a pinned
            // claim kept the path "registered" until a watcher restart.
            seen.lock().await.insert(path.to_path_buf(), Sight::Ended);
            return;
        }
        // #204: register even on an oversized first sight, or a >1 MB transcript
        // stays invisible until its next small append; the backlog is not replayed.
        // Keys on `seen`, NOT `!known` — a first-sight-GATED file is already `known`,
        // and keying on `!known` left it invisible until a later ≤1 MiB append.
        let sight = seen.lock().await.get(path).copied();
        // The span is too big to hold, so the tail is the instrument — a
        // metadata run always lands at the end.
        let tail = read_tail(path, TAIL_BYTES).await;
        let metadata_only = matches!(
            tail.as_deref().map(decoders.activity_recency),
            Some(super::TailActivity::SidecarOnly)
        );
        // Judged on the window `scan_pending_tasks` reads, wide enough to hold a
        // tool result's whole line.
        let revives = sight != Some(Sight::Ended)
            || read_tail(path, TASK_SCAN_BYTES)
                .await
                .is_some_and(|t| tail_shows_activity(&t, path, source, decoders.decode_line));
        if sight != Some(Sight::Claimed) && !metadata_only && revives {
            // The decoder never reaches a head-borne title once the backlog is
            // skipped, so this bounded read is its only carrier.
            let (head_cwd, head_label) = match read_head(path, MAX_PENDING_BYTES).await {
                Some(head) => {
                    extract_head_fields(&head, cwd_extractor_for(source), decoders.head_label)
                }
                None => (None, None),
            };
            emit_first_sight(path, source, decoders, seen, tx, head_cwd, head_label).await;
        }
        // #222: the skipped span may bury an IN-FLIGHT Task dispatch. Only when
        // registered — JSONL events for an unknown id are reducer no-ops.
        if seen.lock().await.get(path) == Some(&Sight::Claimed) {
            scan_pending_tasks(path, decoders, ctx).await;
        }
        return;
    }

    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            warn!(path = ?path, error = %e, "open failed");
            return;
        }
    };
    if let Err(e) = file.seek(SeekFrom::Start(cursor_now)).await {
        warn!(path = ?path, error = %e, "seek failed");
        return;
    }
    let mut new_chunk = Vec::with_capacity((file_len - cursor_now) as usize);
    if let Err(e) = file.read_to_end(&mut new_chunk).await {
        warn!(path = ?path, error = %e, "read tail failed");
        return;
    }

    let safe_end_relative = match new_chunk.iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => 0,
    };
    if safe_end_relative == 0 {
        return;
    }
    let new_cursor = cursor_now + safe_end_relative as u64;
    {
        let mut cursors_g = cursors.lock().await;
        cursors_g.insert(path.to_path_buf(), new_cursor);
    }

    let new_bytes = &new_chunk[..safe_end_relative];
    // Normalized like `id_derive` above, or on Windows the hook key and the
    // per-line key disagree and every JSONL event lands on a phantom id.
    let transcript_path_str = crate::id::normalize_path_key(&path.to_string_lossy());

    // A revived GATED file reads only the tail, but Codex rollouts carry cwd ONLY
    // on the head `session_meta` line — so the revive would register empty-cwd onto
    // the short reap. The same bounded head read also carries the title, which the
    // tail is likewise past.
    let extract = cwd_extractor_for(source);
    let mut first_sight_cwd = extract_cwd(new_bytes, extract);
    let mut head_label = None;
    if first_sight_cwd.is_none()
        && seen.lock().await.get(path) != Some(&Sight::Claimed)
        && let Some(head) = read_head(path, MAX_PENDING_BYTES).await
    {
        (first_sight_cwd, head_label) = extract_head_fields(&head, extract, decoders.head_label);
    }

    let path_agent_id = AgentId::from_parts(source, &decoders.id_derive.id_for(path));
    let ends_this_agent = |ev: &AgentEvent| {
        matches!(ev, AgentEvent::SessionEnd { .. }) && ev.agent_id() == path_agent_id
    };
    let mut decoded = Vec::new();
    for line in new_bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let s = match std::str::from_utf8(line) {
            Ok(s) => s,
            Err(_) => {
                warn!(path = ?path, "non-utf8 line skipped");
                continue;
            }
        };
        let v: serde_json::Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(e) => {
                debug!(path = ?path, error = %e, "non-json line skipped");
                continue;
            }
        };
        match decode_line(&transcript_path_str, source, v) {
            Ok(events) => decoded.extend(events),
            Err(e) => warn!(path = ?path, error = %e, "decode error"),
        }
    }

    // A trailer flushed after the end line (grok's `hook_execution{stop}`)
    // proves no revival.
    let ended = seen.lock().await.get(path) == Some(&Sight::Ended);
    if (!ended || decoded.iter().any(|ev| ev.shows_activity()))
        && !matches!(
            (decoders.activity_recency)(new_bytes),
            super::TailActivity::SidecarOnly
        )
    {
        emit_first_sight(
            path,
            source,
            decoders,
            seen,
            tx,
            first_sight_cwd,
            head_label,
        )
        .await;
    }

    let session_ended = decoded.iter().any(ends_this_agent);
    for ev in decoded {
        if tx.send((Transport::Jsonl, ev)).await.is_err() {
            return;
        }
    }
    if session_ended {
        seen.lock().await.insert(path.to_path_buf(), Sight::Ended);
    }
}

/// Whether `tail`'s whole lines decode to an event that shows activity: the
/// evidence an ended claim's oversized append is judged on, since its skipped
/// span is never decoded.
fn tail_shows_activity(
    tail: &[u8],
    path: &Path,
    source: &Arc<str>,
    decode_line: LineDecoder,
) -> bool {
    let transcript = crate::id::normalize_path_key(&path.to_string_lossy());
    // The window starts mid-file, so its first chunk is a partial line.
    tail.split(|b| *b == b'\n')
        .skip(1)
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter_map(|v| decode_line(&transcript, source, v).ok())
        .flatten()
        .any(|ev| ev.shows_activity())
}

/// Pre-drain guard for the exit-path drains: a transcript truncated/recreated
/// BELOW its cursor at the moment a drain runs would hit `walk_jsonl`'s
/// truncation arm — cursor reset to 0, return WITHOUT draining — so the next
/// pass replays the whole file as a first-sight and re-registers the just-ended
/// session as a ghost. Park the cursor at the new EOF instead; only genuinely
/// NEW bytes (len > cursor) re-register. The reset-to-0 replay stays the right
/// call on the NORMAL walk path, where a live session's truncate-rewrite
/// resyncs from the top.
pub(super) async fn park_if_truncated_below_cursor(path: &Path, ctx: &WatchCtx<'_>) {
    let Ok(meta) = tokio::fs::symlink_metadata(path).await else {
        return;
    };
    let len = meta.len();
    let mut cursors = ctx.cursors.lock().await;
    if let Some(c) = cursors.get_mut(path) {
        *c = (*c).min(len);
    }
}

/// Claim first-sight for `path` and, if this is the first pass to see it, emit
/// the synthesized `SessionStart` + `Rename`. Shared by the normal tail-read
/// path and the oversized-first-sight path so the two emit IDENTICAL events
/// from one place. A `None`/empty `cwd` falls back to the project-dir label in
/// `derive_label`.
async fn emit_first_sight(
    path: &Path,
    source: &Arc<str>,
    decoders: SourceDecoders,
    seen: &Arc<Mutex<HashMap<PathBuf, Sight>>>,
    tx: &TaggedSender,
    cwd: Option<PathBuf>,
    head_label: Option<String>,
) {
    // An `Ended` claim registers like an absent one: the walk calls here for it
    // only on an append that shows activity.
    let already_claimed =
        seen.lock().await.insert(path.to_path_buf(), Sight::Claimed) == Some(Sight::Claimed);
    if already_claimed {
        return;
    }
    // Same deriver as the AgentId: hook slots carry the bare session UUID and
    // `backfill_identity` never heals a non-empty session_id, so a raw file-stem
    // here disagrees with the hook-created twin forever.
    let session_id = decoders.id_derive.id_for(path);
    let id = AgentId::from_parts(source, &session_id);
    // Content-derived cwd wins; the PATH deriver covers sources carrying none, an
    // empty cwd being what puts a slot on the unknown-cwd short reap.
    let cwd = cwd
        .or_else(|| (decoders.cwd_derive)(path))
        .unwrap_or_default();
    let parent_id = detect_parent_id(path, source);
    let _ = tx
        .send((
            Transport::Jsonl,
            AgentEvent::SessionStart {
                agent_id: id,
                source: source.to_string(),
                session_id,
                cwd: cwd.clone(),
                parent_id,
            },
        ))
        .await;

    // A head-read name beats the cwd deriver: it is the session's OWN title,
    // while the cwd basename is shared by every concurrent session in a repo.
    let label = head_label.unwrap_or_else(|| (decoders.derive_label)(path, source, &cwd));
    let _ = tx
        .send((
            Transport::Jsonl,
            AgentEvent::Rename {
                agent_id: id,
                label,
            },
        ))
        .await;
}

/// Read at most `limit` bytes from the START of a file, so registration never
/// reads a whole multi-MB transcript. Returned raw because first-sight pulls
/// TWO fields out of the same bytes (`cwd`, and the head label for a source
/// that names its session there) — one read, not one per field.
async fn read_head(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file_len = tokio::fs::metadata(path).await.ok()?.len();
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut head = vec![0u8; limit.min(file_len) as usize];
    let n = file.read(&mut head).await.ok()?;
    head.truncate(n);
    Some(head)
}

/// Read at most `bytes` from the END of a file (clamped to file size). `None`
/// on any I/O error — callers treat that as "nothing to scan".
async fn read_tail(path: &Path, bytes: u64) -> Option<Vec<u8>> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let file_len = meta.len();
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let start = file_len.saturating_sub(bytes);
    file.seek(SeekFrom::Start(start)).await.ok()?;
    let mut buf = Vec::with_capacity(bytes.min(file_len) as usize);
    file.read_to_end(&mut buf).await.ok()?;
    Some(buf)
}

pub(super) async fn check_session_ended(path: &Path, checker: SessionEndChecker) -> bool {
    match read_tail(path, TAIL_BYTES).await {
        Some(buf) => checker(&buf),
        None => false,
    }
}

/// How far back from EOF the oversized-skip Task scan looks.
pub(super) const TASK_SCAN_BYTES: u64 = 256 * 1024;

/// #222: tail-scan an oversized skipped span for IN-FLIGHT Task dispatches and
/// re-emit exactly their `ActivityStart`s. Mid-attach to a delegating session
/// whose backlog exceeds `MAX_PENDING_BYTES` never decodes the in-flight
/// dispatch line, leaving the reducer's `active_tasks` empty: subagent-leak
/// suppression stays OFF and the b1 completion cascade never arms.
///
/// Tail-window geometry guarantees no false leak: a completion is always LATER
/// in the file than its start, so any windowed start's completion is also in
/// the window. A dispatch buried deeper than `TASK_SCAN_BYTES` keeps the
/// pre-#222 skip behavior — bounded, documented residual.
///
/// Everything `decode_line` emits EXCEPT the unmatched Task starts is
/// DISCARDED: this is a Task-seeding scan, not a replay — replaying 256 KiB of
/// activity would animate a burst of stale tools.
async fn scan_pending_tasks(path: &Path, decoders: SourceDecoders, ctx: &WatchCtx<'_>) {
    let Some(buf) = read_tail(path, TASK_SCAN_BYTES).await else {
        return;
    };
    let transcript_path_str = crate::id::normalize_path_key(&path.to_string_lossy());
    let mut lines = buf.split(|b| *b == b'\n');
    // The window always starts mid-file, so its first chunk is a partial line —
    // skip it rather than decode a fragment that may parse as JSON by accident.
    let _ = lines.next();
    let mut pending: Vec<(String, AgentEvent)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
            continue;
        };
        let events = match (decoders.decode_line)(&transcript_path_str, ctx.source, v) {
            Ok(events) => events,
            Err(e) => {
                debug!(path = ?path, error = %e, "task-scan decode error");
                continue;
            }
        };
        for ev in events {
            match &ev {
                AgentEvent::ActivityStart {
                    tool_use_id: Some(tuid),
                    detail: Some(d),
                    ..
                } if d.is_task() => {
                    if !pending.iter().any(|(t, _)| t == tuid) {
                        pending.push((tuid.clone(), ev));
                    }
                }
                AgentEvent::ActivityEnd {
                    tool_use_id: Some(tuid),
                    ..
                } => {
                    pending.retain(|(t, _)| t != tuid);
                }
                _ => {}
            }
        }
    }
    for (tuid, ev) in pending {
        debug!(
            ?tuid,
            path = ?path,
            "re-emitting in-flight Task dispatch from the oversized tail"
        );
        if ctx.tx.send((Transport::Jsonl, ev)).await.is_err() {
            return;
        }
    }
}

/// Detect a CC subagent by the `subagents` path component and link it to its
/// parent via the directory component immediately before it (`<parent-uuid>`).
/// That UUID equals the parent's own id, so the link resolves even when the
/// subagent transcript lands under a DIFFERENT project dir than the parent (a
/// git-worktree cwd-split). CC-layout-specific.
pub(super) fn detect_parent_id(path: &Path, source: &str) -> Option<AgentId> {
    let mut prev: Option<&str> = None;
    for c in path.components() {
        if c.as_os_str() == SUBAGENTS_DIR {
            return prev.map(|uuid| AgentId::from_parts(source, uuid));
        }
        prev = c.as_os_str().to_str();
    }
    None
}

/// Scan a byte span line-by-line and return the first cwd the SCANNED source's
/// own extractor finds. The per-source shape knowledge stays in the registry
/// row (invariant #3): an if-chain trying every source's shape against every
/// transcript lets a foreign-shaped line label a session with a foreign,
/// identity-bearing cwd.
pub(super) fn extract_cwd(bytes: &[u8], extract: CwdExtractor) -> Option<PathBuf> {
    let mut found = None;
    scan_jsonl(bytes, |v| {
        found = extract(v);
        found.is_some()
    });
    found
}

/// Walk a byte span's JSONL lines, handing each parsed line to `visit` until it
/// returns `true`. Malformed/non-UTF-8 lines are skipped — a transcript head can
/// straddle a partial write.
fn scan_jsonl(bytes: &[u8], mut visit: impl FnMut(&serde_json::Value) -> bool) {
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
            continue;
        };
        if visit(&v) {
            return;
        }
    }
}

/// Whether a head scan can stop: everything it is ABLE to find, it has found.
///
/// `wants_label` is the whole reason `head_label` is an `Option`. A source with
/// no head name can never satisfy a label term, so folding one in unconditionally
/// turns its first-line stop into a full up-to-`MAX_PENDING_BYTES` parse — on a
/// path every transcript source rides.
fn head_scan_complete(cwd: Option<&Path>, label: Option<&str>, wants_label: bool) -> bool {
    cwd.is_some() && (!wants_label || label.is_some())
}

/// The first-sight fields a HEAD read yields, pulled in ONE parse pass: scanning
/// a 1 MiB head once per field is a real cost. For a TITLED omp root — the only
/// source with a head label — the two land on adjacent lines (`title`, then
/// `session`) and the scan stops there. A title-less head (every subagent, and
/// legacy files with no slot) has no label to find, so it scans to the byte cap:
/// bounded, once per first sight, and off the reducer thread.
pub(super) fn extract_head_fields(
    bytes: &[u8],
    extract: CwdExtractor,
    head_label: Option<HeadLabel>,
) -> (Option<PathBuf>, Option<String>) {
    let (mut cwd, mut label) = (None, None);
    scan_jsonl(bytes, |v| {
        if cwd.is_none() {
            cwd = extract(v);
        }
        if let (Some(f), None) = (head_label, &label) {
            label = f(v);
        }
        head_scan_complete(cwd.as_deref(), label.as_deref(), head_label.is_some())
    });
    (cwd, label)
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    /// The truth table the scan's cost depends on. The `wants_label = false`
    /// row is load-bearing: it is the difference between stopping on the first
    /// `cwd` line and parsing the entire head.
    #[test]
    fn head_scan_stops_on_cwd_alone_when_the_source_has_no_head_label() {
        let cwd = Path::new("/repo");

        assert!(head_scan_complete(Some(cwd), None, false));
        assert!(head_scan_complete(Some(cwd), Some("t"), false));
        assert!(!head_scan_complete(None, Some("t"), false));

        assert!(!head_scan_complete(Some(cwd), None, true));
        assert!(head_scan_complete(Some(cwd), Some("t"), true));
        assert!(!head_scan_complete(None, None, true));
    }
}
