//! The `hooks` category: whether a hook can get from an agent CLI to an office,
//! which a written config does not prove and a shim that exits 0 whatever
//! happened never says.
//!
//! Two probes. The shim runs against a throwaway listener, so the running office
//! sees nothing of it, and the endpoint's owner reports what reached it. Whether a
//! CLI's environment resolves the same endpoint is not observable from here: a
//! misdirected hook reaches nobody, so the report states the endpoint instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use pixtuoid_core::source::claude_code::{ClaudeCodeSource, SOURCE_NAME};
use pixtuoid_core::source::hook::{self, EndpointStatus, HookSocketListener, OwnerStatus};
use tokio::io::AsyncWriteExt;

use super::{
    CONT_INDENT, Category, CategoryStatus, DETAIL_INDENT, DoctorSourceRow, EXIT_DEADLINE, Ink,
    SOURCE_W, ShownPath, sanitize,
};
use crate::install::io::HOOK_BIN;
use crate::install::verify::ShimRef;
use crate::panels::widgets::fmt_age;

/// The shim has exited, so the line already sits at the listener.
const EVENT_WAIT: Duration = Duration::from_secs(1);
/// The shim's watchdog can drop a send under load, which is no broken install.
const ATTEMPTS: usize = 2;
const EVENT_BACKLOG: usize = 8;

/// Enough of Claude Code's wire to decode: the probe is of the shim's transport.
const PROBE_PAYLOAD: &[u8] =
    br#"{"hook_event_name":"SessionStart","session_id":"pixtuoid-doctor","cwd":""}"#;

#[derive(Debug, PartialEq, Eq)]
enum ShimProbe {
    Delivered,
    /// A hook run the same way delivers nothing.
    Failed(String),
    /// Says nothing either way about the shim.
    NotTested(String),
}

pub(super) struct Delivery {
    shims: Vec<(ShownPath, ShimProbe)>,
    /// Why no shim could be found to try.
    unlocated: Option<String>,
    /// An install's hook names the bare `pixtuoid-hook`, and this PATH has none.
    bare_off_path: bool,
    endpoint: ShownPath,
    status: EndpointStatus,
}

impl Delivery {
    #[cfg(test)]
    pub(super) fn idle() -> Self {
        Self {
            shims: vec![(ShownPath::new("/bin/pixtuoid-hook"), ShimProbe::Delivered)],
            unlocated: None,
            bare_off_path: false,
            endpoint: ShownPath::new("/tmp/pixtuoid.sock"),
            status: EndpointStatus::Absent,
        }
    }

    #[cfg(test)]
    pub(super) fn hostile(evil: &str) -> Self {
        Self {
            shims: vec![
                (ShownPath::new(evil), ShimProbe::Failed(evil.into())),
                (ShownPath::new(evil), ShimProbe::NotTested(evil.into())),
            ],
            unlocated: Some(evil.into()),
            endpoint: ShownPath::new(evil),
            status: EndpointStatus::Unbindable(evil.into()),
            ..Self::idle()
        }
    }
}

/// The absolute shim each install names, and whether any names the bare
/// `pixtuoid-hook`.
fn shims_of(rows: &[DoctorSourceRow]) -> (BTreeSet<PathBuf>, bool) {
    let mut named = BTreeSet::new();
    let mut bare = false;
    let installs = rows.iter().filter_map(|row| row.diag.install.as_ref());
    for shim in installs.flat_map(|install| &install.shims) {
        match shim {
            ShimRef::Absolute(path) => {
                named.insert(path.clone());
            }
            ShimRef::BareName => bare = true,
            ShimRef::Unknown => {}
        }
    }
    (named, bare)
}

/// All probing, no formatting.
///
/// # Errors
///
/// If no tokio runtime can be built.
pub(super) fn collect(rows: &[DoctorSourceRow]) -> anyhow::Result<Delivery> {
    let runtime = super::runtime()?;
    let (mut named, bare) = shims_of(rows);
    let mut unlocated = None;
    // A bare hook is whatever PATH finds, never the installer's override or sibling.
    let on_path = bare.then(crate::install::io::hook_on_path).flatten();
    if let Some(path) = &on_path {
        named.insert(path.clone());
    } else if bare || named.is_empty() {
        match crate::install::located_shim() {
            Ok(path) => {
                named.insert(path);
            }
            Err(e) => unlocated = Some(format!("{e:#}")),
        }
    }
    let shims = named
        .into_iter()
        .map(|path| {
            let probed = runtime.block_on(probe(&path, EXIT_DEADLINE));
            (ShownPath::new(path), probed)
        })
        .collect();
    let endpoint = ClaudeCodeSource::default_socket_path();
    Ok(Delivery {
        shims,
        unlocated,
        bare_off_path: bare && on_path.is_none(),
        status: runtime.block_on(hook::endpoint_status(&endpoint)),
        endpoint: ShownPath::new(endpoint),
    })
}

/// The whole file name: a stem would pass the `pixtuoid-hook.sh` a user wrote.
fn named_as_the_shim(path: &Path) -> bool {
    let shim = crate::install::io::hook_sibling_name();
    path.file_name().is_some_and(|name| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case(&shim)
        } else {
            name == shim.as_str()
        }
    })
}

/// Run `shim` as a hook would, against a listener only it knows about. The real
/// listener and decoder on purpose. The path must be absolute and named as the
/// shim before anything runs: a hand-edited config can name anything, and this
/// feeds it an invented event.
async fn probe(shim: &Path, exit_deadline: Duration) -> ShimProbe {
    if !shim.is_absolute() {
        return ShimProbe::NotTested(
            "its path is relative, so what runs depends on where the CLI runs it".into(),
        );
    }
    if !named_as_the_shim(shim) {
        let name = crate::install::io::hook_sibling_name();
        return ShimProbe::NotTested(format!("it is not named {name}"));
    }
    let dir = match tempfile::Builder::new().prefix("px").tempdir() {
        Ok(dir) => dir,
        Err(e) => return ShimProbe::NotTested(format!("no temp dir: {e}")),
    };
    let endpoint = hook::private_endpoint(dir.path(), "p");
    let listener = match HookSocketListener::bind(&endpoint).await {
        Ok(listener) => listener,
        Err(e) => return ShimProbe::NotTested(format!("no throwaway endpoint: {e:#}")),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(EVENT_BACKLOG);
    let serving = tokio::spawn(listener.run(tx));
    let mut verdict = ShimProbe::Failed("it exited 0 and delivered nothing".into());
    for _ in 0..ATTEMPTS {
        match run_once(shim, &endpoint, exit_deadline).await {
            Err(why) => {
                verdict = ShimProbe::Failed(why);
                break;
            }
            Ok(()) => {
                if matches!(
                    tokio::time::timeout(EVENT_WAIT, rx.recv()).await,
                    Ok(Some(_))
                ) {
                    verdict = ShimProbe::Delivered;
                    break;
                }
            }
        }
    }
    serving.abort();
    verdict
}

/// One run to a clean exit, which the shim's contract promises, so anything else
/// is the OS refusing it or a file that is no shim.
async fn run_once(shim: &Path, endpoint: &Path, exit_deadline: Duration) -> Result<(), String> {
    let mut command =
        tokio::process::Command::from(crate::install::shim_command(shim, SOURCE_NAME));
    let mut child = command
        .env(hook::SOCKET_ENV, endpoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("it would not run: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A write error is the child leaving, which its status reports.
        let _ = stdin.write_all(PROBE_PAYLOAD).await;
    }
    match tokio::time::timeout(exit_deadline, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("it ended with {status}")),
        Ok(Err(e)) => Err(format!("could not wait for it: {e}")),
        Err(_) => Err(format!("it did not exit within {exit_deadline:?}")),
    }
}

fn shim_row((path, probed): &(ShownPath, ShimProbe), ink: &Ink) -> String {
    let outcome = match probed {
        ShimProbe::Delivered => "delivered a test event".to_string(),
        ShimProbe::Failed(why) => ink.bad(&format!("failed: {}", sanitize(why))),
        ShimProbe::NotTested(why) => ink.warn(&format!("not tested: {}", sanitize(why))),
    };
    format!("{DETAIL_INDENT}shim  {path} {outcome}")
}

/// A source's row in a live owner's report, and whether the office lags the disk:
/// `connect` from a shell reaches a running office's gate only at its next launch,
/// so hooks that arrive are dropped and nothing else says why it stays empty.
fn source_row(row: &DoctorSourceRow, status: &OwnerStatus) -> Option<(String, bool)> {
    let name = row.desc.name;
    let seen = status.last_hook_ago_ms.get(name);
    let gated = !status.connected.contains(name);
    let lagging = row.connected && gated;
    if !(row.hooks_installed || seen.is_some() || lagging) {
        return None;
    }
    let state = if lagging {
        "connected after this instance started: restart it to see its agents".to_string()
    } else if let Some(ms) = seen {
        let age = fmt_age(Duration::from_millis(*ms));
        let gate = if gated {
            ", dropped: not connected"
        } else {
            ""
        };
        format!("last hook {age} ago{gate}")
    } else if let Some(hint) = crate::sources::post_install_hint(name) {
        format!("no hooks yet \u{2014} {hint}")
    } else {
        "no hooks yet".to_string()
    };
    Some((format!("{DETAIL_INDENT}{name:<SOURCE_W$} {state}"), lagging))
}

pub(super) fn hooks_category(d: &Delivery, rows: &[DoctorSourceRow], ink: &Ink) -> Category {
    let mut details: Vec<String> = d.shims.iter().map(|shim| shim_row(shim, ink)).collect();
    if let Some(why) = &d.unlocated {
        details.push(format!(
            "{DETAIL_INDENT}shim  {}",
            ink.warn(&format!("not found: {}", sanitize(why)))
        ));
    }
    if d.bare_off_path {
        details.push(format!(
            "{DETAIL_INDENT}shim  {}",
            ink.warn(&format!(
                "a hook runs the bare `{HOOK_BIN}`, which this PATH has none of; \
                 a CLI on the same PATH cannot run it"
            ))
        ));
    }

    let endpoint = &d.endpoint;
    let mut lagging = false;
    let (owner, owner_sound) = match &d.status {
        EndpointStatus::Live(status) => {
            details.push(format!(
                "{DETAIL_INDENT}instance  held the endpoint for {}",
                fmt_age(Duration::from_millis(status.held_for_ms))
            ));
            for (row, lags) in rows.iter().filter_map(|row| source_row(row, status)) {
                lagging |= lags;
                details.push(row);
            }
            (format!("instance live on {endpoint}"), true)
        }
        EndpointStatus::Absent => {
            details.push(format!(
                "{DETAIL_INDENT}no instance: hooks are dropped until `pixtuoid` runs"
            ));
            details.push(format!(
                "{CONT_INDENT}\u{21b3} if pixtuoid is running, it and this shell resolve different \
                 endpoints: run doctor from the shell your CLI runs in"
            ));
            (format!("no instance on {endpoint}"), true)
        }
        EndpointStatus::Unbindable(why) => {
            details.push(format!("{DETAIL_INDENT}{}", ink.warn(&sanitize(why))));
            (format!("no instance can bind {endpoint}"), false)
        }
        EndpointStatus::Denied => {
            details.push(format!(
                "{DETAIL_INDENT}{}",
                ink.warn("this user may not connect, so its hooks reach no instance there")
            ));
            (format!("access denied on {endpoint}"), false)
        }
        EndpointStatus::Silent => {
            details.push(format!(
                "{DETAIL_INDENT}{}",
                ink.warn("it gave no status: restart it (an older pixtuoid gives none, and one that lost its endpoint gets no hooks)")
            ));
            (format!("instance on {endpoint} is not answering"), false)
        }
    };

    let failed = d
        .shims
        .iter()
        .any(|(_, probed)| matches!(probed, ShimProbe::Failed(_)));
    let delivers = d.unlocated.is_none()
        && !d.shims.is_empty()
        && d.shims
            .iter()
            .all(|(_, probed)| *probed == ShimProbe::Delivered);
    if failed {
        details.push(ink.hint(&format!(
            "{DETAIL_INDENT}\u{2192} fix: reconnect in the Sources panel (press s); if it \
             still fails, reinstall pixtuoid, which ships {HOOK_BIN}"
        )));
    }
    let status = if failed {
        CategoryStatus::Broken
    } else if !delivers || !owner_sound || lagging || d.bare_off_path {
        CategoryStatus::Warn
    } else {
        CategoryStatus::Ok
    };
    let shim = match (failed, delivers) {
        (true, _) => "shim cannot deliver",
        (false, true) => "shim delivers",
        (false, false) => "shim not tested",
    };
    Category {
        status,
        name: "hooks",
        summary: format!("{shim} \u{b7} {owner}"),
        details,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use pixtuoid_core::source::registry;

    use super::super::SourceDiagnostics;
    use super::*;

    const INK: Ink = Ink { on: false };

    fn row(source: &str, connected: bool, hooks_installed: bool) -> DoctorSourceRow {
        DoctorSourceRow {
            desc: registry::descriptor_for(source).expect("a registered source"),
            connected,
            has_target: true,
            hooks_installed,
            installed_version: crate::doctor::InstalledVersion::Unasked,
            diag: SourceDiagnostics::default(),
        }
    }

    fn live(connected: &[&str], seen: &[(&str, u64)]) -> EndpointStatus {
        EndpointStatus::Live(OwnerStatus {
            held_for_ms: 7_380_000,
            connected: connected.iter().map(ToString::to_string).collect(),
            last_hook_ago_ms: seen
                .iter()
                .map(|(name, ms)| ((*name).to_string(), *ms))
                .collect::<BTreeMap<_, _>>(),
        })
    }

    fn category(status: EndpointStatus, rows: &[DoctorSourceRow]) -> (CategoryStatus, String) {
        let d = Delivery {
            status,
            ..Delivery::idle()
        };
        let c = hooks_category(&d, rows, &INK);
        let text = format!("{} {}\n{}", c.name, c.summary, c.details.join("\n"));
        (c.status, text)
    }

    #[test]
    fn a_healthy_office_reads_each_sources_last_hook() {
        let rows = [row("claude-code", true, true), row("cursor", true, true)];
        let (status, text) = category(
            live(
                &["claude-code", "cursor"],
                &[("claude-code", 3000), ("no-such-source", 1)],
            ),
            &rows,
        );
        assert_eq!(status, CategoryStatus::Ok, "{text}");
        assert!(text.contains("shim delivers"), "{text}");
        assert!(text.contains("delivered a test event"), "{text}");
        assert!(text.contains("instance live on"), "{text}");
        assert!(text.contains("held the endpoint for 2h"), "{text}");
        assert!(text.contains("last hook 3s ago"), "{text}");
        assert!(text.contains("no hooks yet"), "{text}");
        assert!(
            !text.contains("no-such-source"),
            "a name from the wire is never printed: {text}"
        );
    }

    #[test]
    fn a_silent_source_gets_its_targets_own_next_step() {
        let rows = [row("codex", true, true)];
        let (status, text) = category(live(&["codex"], &[]), &rows);
        assert_eq!(status, CategoryStatus::Ok, "an unused CLI is no fault");
        assert!(
            text.contains(crate::sources::post_install_hint("codex").unwrap()),
            "{text}"
        );
    }

    #[test]
    fn a_source_with_no_install_and_no_hook_stays_out_of_the_report() {
        let (_, text) = category(live(&[], &[]), &[row("cursor", false, false)]);
        assert!(!text.contains("cursor"), "{text}");
    }

    #[test]
    fn an_office_behind_the_disk_is_told_to_restart() {
        let rows = [row("cursor", true, true)];
        let (status, text) = category(live(&["codex"], &[("cursor", 5)]), &rows);
        assert_eq!(status, CategoryStatus::Warn, "{text}");
        assert!(
            text.contains("connected after this instance started"),
            "{text}"
        );
    }

    #[test]
    fn a_source_the_panel_disconnected_is_no_lag() {
        let rows = [row("cursor", false, false)];
        let (status, text) = category(live(&[], &[("cursor", 5)]), &rows);
        assert_eq!(status, CategoryStatus::Ok, "{text}");
        assert!(text.contains("dropped: not connected"), "{text}");
    }

    #[test]
    fn no_office_is_no_issue_and_says_what_a_mismatch_would_look_like() {
        let (status, text) = category(EndpointStatus::Absent, &[]);
        assert_eq!(status, CategoryStatus::Ok, "{text}");
        assert!(text.contains("no instance on"), "{text}");
        assert!(text.contains("dropped until"), "{text}");
        assert!(text.contains("resolve different endpoints"), "{text}");
    }

    #[test]
    fn an_owner_it_cannot_read_is_a_warning_and_delivery_still_shows() {
        for (status, says) in [
            (EndpointStatus::Silent, "older pixtuoid"),
            (EndpointStatus::Denied, "may not connect"),
            (EndpointStatus::Unbindable("refusing".into()), "refusing"),
        ] {
            let (verdict, text) = category(status, &[]);
            assert_eq!(verdict, CategoryStatus::Warn, "{text}");
            assert!(text.contains(says), "{text}");
            assert!(
                text.contains("delivered a test event"),
                "an unreadable owner must not hide the shim's verdict: {text}"
            );
        }
    }

    #[test]
    fn a_failed_shim_is_broken_and_an_untested_one_only_warns() {
        let failed = Delivery {
            shims: vec![(
                ShownPath::new("/x/pixtuoid-hook"),
                ShimProbe::Failed("it ended with signal: 9".into()),
            )],
            ..Delivery::idle()
        };
        let c = hooks_category(&failed, &[], &INK);
        assert_eq!(c.status, CategoryStatus::Broken);
        assert!(c.summary.contains("shim cannot deliver"), "{}", c.summary);
        assert!(c.details.join("\n").contains("signal: 9"));

        for d in [
            Delivery {
                shims: vec![(
                    ShownPath::new("/x/log-hook.sh"),
                    ShimProbe::NotTested("it is not named pixtuoid-hook".into()),
                )],
                ..Delivery::idle()
            },
            Delivery {
                shims: vec![],
                unlocated: Some("could not locate pixtuoid-hook".into()),
                ..Delivery::idle()
            },
            Delivery {
                bare_off_path: true,
                ..Delivery::idle()
            },
        ] {
            let c = hooks_category(&d, &[], &INK);
            assert_eq!(c.status, CategoryStatus::Warn, "{}", c.details.join("\n"));
        }
    }

    #[test]
    fn the_shim_set_is_every_install_s_and_their_bare_names_are_flagged() {
        let mut one = row("claude-code", true, true);
        one.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            shims: vec![ShimRef::BareName, ShimRef::Unknown],
            ..Default::default()
        });
        let mut two = row("codex", true, true);
        two.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            shims: vec![
                ShimRef::Absolute("/a/pixtuoid-hook".into()),
                ShimRef::Absolute("/a/pixtuoid-hook".into()),
            ],
            ..Default::default()
        });
        assert_eq!(
            shims_of(&[one, two, row("cursor", false, false)]),
            (BTreeSet::from([PathBuf::from("/a/pixtuoid-hook")]), true)
        );
    }

    // PATH, not the override, is what a bare hook runs.
    #[cfg(unix)]
    #[test]
    fn a_bare_hook_is_probed_at_what_path_finds() {
        let (bin, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let on_path = stand_in(bin.path(), "exit 3");
        let override_shim = stand_in(other.path(), "exit 4");
        let endpoint = hook::private_endpoint(bin.path(), "none");
        let mut bare = row("claude-code", true, true);
        bare.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            shims: vec![ShimRef::BareName],
            ..Default::default()
        });
        let d = temp_env::with_vars(
            [
                ("PATH", Some(bin.path().as_os_str())),
                ("PIXTUOID_HOOK", Some(override_shim.as_os_str())),
                (hook::SOCKET_ENV, Some(endpoint.as_os_str())),
            ],
            || collect(&[bare]).unwrap(),
        );
        assert!(!d.bare_off_path);
        let [(path, probed)] = d.shims.as_slice() else {
            panic!("one shim, got {}", d.shims.len());
        };
        assert_eq!(path.to_string(), ShownPath::new(&on_path).to_string());
        assert!(
            matches!(probed, ShimProbe::Failed(why) if why.contains('3')),
            "{probed:?}"
        );
    }

    #[tokio::test]
    async fn a_file_of_another_name_is_never_run() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["log-hook.sh", "pixtuoid-hook.sh", "pixtuoid-hook.bak"] {
            assert!(matches!(
                probe(&dir.path().join(name), EVENT_WAIT).await,
                ShimProbe::NotTested(why) if why.contains("not named")
            ));
        }
    }

    #[tokio::test]
    async fn a_relative_path_is_never_run() {
        let relative = Path::new("bin").join(crate::install::io::hook_sibling_name());
        assert!(matches!(
            probe(&relative, EVENT_WAIT).await,
            ShimProbe::NotTested(why) if why.contains("relative")
        ));
    }

    /// A stand-in named as the shim is, so the name check passes it.
    #[cfg(unix)]
    fn stand_in(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(crate::install::io::hook_sibling_name());
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_shim_that_ends_badly_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let verdict = probe(&stand_in(dir.path(), "exit 3"), EXIT_DEADLINE).await;
        assert!(
            matches!(&verdict, ShimProbe::Failed(why) if why.contains('3')),
            "{verdict:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_shim_that_delivers_nothing_fails_after_its_retry() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let body = format!("echo >> '{}'", runs.display());
        let verdict = probe(&stand_in(dir.path(), &body), EXIT_DEADLINE).await;
        assert!(
            matches!(&verdict, ShimProbe::Failed(why) if why.contains("delivered nothing")),
            "{verdict:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&runs).unwrap().lines().count(),
            ATTEMPTS
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_shim_that_hangs_is_killed_at_the_deadline() {
        const SHORT: Duration = Duration::from_millis(100);
        let dir = tempfile::tempdir().unwrap();
        let verdict = probe(&stand_in(dir.path(), "exec sleep 600"), SHORT).await;
        assert!(
            matches!(&verdict, ShimProbe::Failed(why) if why.contains("did not exit")),
            "{verdict:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_that_will_not_run_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = stand_in(dir.path(), "exit 0");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let verdict = probe(&path, EXIT_DEADLINE).await;
        assert!(
            matches!(&verdict, ShimProbe::Failed(why) if why.contains("would not run")),
            "{verdict:?}"
        );
    }

    #[tokio::test]
    async fn a_shim_that_is_gone_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::install::io::hook_sibling_name());
        let verdict = probe(&path, EXIT_DEADLINE).await;
        assert!(
            matches!(&verdict, ShimProbe::Failed(why) if why.contains("would not run")),
            "{verdict:?}"
        );
    }
}
