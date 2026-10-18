pub(crate) mod antigravity;
pub(crate) mod claude;
pub(crate) mod codewhale;
pub(crate) mod codex;
pub(crate) mod cursor;
pub(crate) mod grok;
pub(crate) mod hermes;
mod hook_cmd;
pub(crate) mod host_plugin;
pub(crate) mod io;
pub(crate) mod kimi;
pub(crate) use io::{nonempty_abs_env, owner_only_create};
pub(crate) mod dsh;
pub(crate) mod merge;
pub(crate) mod omp;
pub(crate) mod openclaw;
pub(crate) mod opencode;
pub(crate) mod reasonix;
pub(crate) mod target;
pub(crate) use target::TARGETS;
pub(crate) mod verify;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use target::{BinaryStrategy, Primary, Target};

/// The idempotency sentinel stamped on every hook entry pixtuoid merges into a
/// shared config — those targets key install/uninstall/detect on this, not the
/// command shape. A plugin's own file (Claude Code's, Codex's, Reasonix's) is ours
/// whole and carries none; for those hosts it names only what an older pixtuoid
/// merged into the CLI's own config ([`target::LegacyConfig`]).
pub(crate) const SENTINEL_KEY: &str = "_pixtuoid";

/// Whether `t`'s config currently bears pixtuoid hooks — whether its source is
/// connected (`sources::connected`), and the gate for `verify_target`, which would
/// report an uninstalled config as "broken". Errs toward INCLUDED wherever it
/// cannot tell: an UNREADABLE config counts as installed, because
/// we cannot look and so must not claim otherwise, and one that reads but cannot be
/// PARSED falls back to the marker probe (`config_mentions_us`).
pub(crate) fn has_hooks(t: &'static Target, config: Option<PathBuf>) -> bool {
    // The gate (this) and the verify it guards MUST read the SAME config, or
    // `diagnose` through an injected root could never observe an install.
    let path = match config.map(Ok).unwrap_or_else(|| (t.default_config_path)()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    match io::read_config(&path) {
        Ok(c) if c.trim().is_empty() => false,
        // An uninstall that ERRS means "we could not tell" — never "installed".
        Ok(c) => (t.primary.uninstall())(&c)
            .map(|o| o.changed)
            .unwrap_or_else(|_| config_mentions_us(&c)),
        Err(_) => true,
    }
}

const PLUGIN_MENTION: &str = "pixtuoid";

/// The fallback when a config cannot be PARSED for a real answer — without it, a
/// never-connected OpenClaw user with a comment in their legal-JSON5 config was reported
/// hooks-INSTALLED and then verified BROKEN. Lowercased for `kimi`, the one target with
/// no `_pixtuoid` sentinel (UPPERCASE `PIXTUOID_SOURCE=kimi`); an OpenClaw plugin id is
/// case-SENSITIVE upstream (`openclaw::is_plugin_id`) — don't harmonize the two.
fn config_mentions_us(content: &str) -> bool {
    content.to_ascii_lowercase().contains(PLUGIN_MENTION)
}

/// Verify a target's installed config is structurally SOUND (the silent-dead check) —
/// read-only, false-positive-free. Call only when hooks are claimed installed
/// (`has_hooks(t, config)`, same `config`). Returns the primary's per-source verify
/// verdict PLUS the shim-on-disk checks this (the only I/O) layer adds. `config`
/// overrides the default path; `None` = the target's default.
pub(crate) fn verify_target(
    t: &'static Target,
    config: Option<PathBuf>,
) -> verify::SchemaVerifyResult {
    use verify::ShimRef;
    let path = match config.map(Ok).unwrap_or_else(|| (t.default_config_path)()) {
        Ok(p) => p,
        Err(_) => {
            return verify::SchemaVerifyResult {
                issues: vec!["no config path resolves (no home dir)".into()],
                notes: vec![],
            };
        }
    };
    let content = match io::read_config(&path) {
        Ok(c) if c.trim().is_empty() => {
            return verify::SchemaVerifyResult {
                issues: vec!["config is empty — hooks are not installed".into()],
                notes: vec![],
            };
        }
        Ok(c) => c,
        Err(_) => {
            return verify::SchemaVerifyResult {
                issues: vec![format!("config unreadable: {}", crate::display_path(&path))],
                notes: vec![],
            };
        }
    };
    let parse = (t.primary.verify())(&content);
    let mut issues = parse.issues;
    let mut notes = parse.notes;
    match parse.shim {
        ShimRef::Absolute(p) => check_shim_binary(&p, &mut issues),
        ShimRef::BareName => {
            // A doctor-process PATH miss is NOT proof the CLI can't resolve the bare
            // `pixtuoid-hook` → soft note only.
            if !io::hook_on_path() {
                notes.push(
                    "pixtuoid-hook not on this process's PATH (the CLI's PATH may differ)".into(),
                );
            }
        }
        ShimRef::Unknown => {
            // SOFT, not hard: a novel-but-valid command shape lands here, and the
            // genuine no-hooks case is already a HARD issue from the primary's verify.
            notes.push("could not read the shim path from the managed hook command".into());
        }
    }
    if let Primary::Config {
        beside: Some(render),
        ..
    } = t.primary
    {
        verify_beside(render, t.display_name, &mut issues, &mut notes);
    }
    if let Some(files) = t.primary.owned_files() {
        verify_owned_drift(t, files, &path, &mut issues, &mut notes);
    }
    if let Some(host) = t.host {
        match (host.is_registered)() {
            Ok(true) => match host.copy.as_ref().map(|c| (c.stale)(&path)) {
                Some(Ok(true)) => issues.push(format!(
                    "{} runs an older copy of the plugin (a launch re-copies it; the log \
                     names a re-copy that failed)",
                    t.display_name
                )),
                Some(Err(e)) => notes.push(format!(
                    "could not compare {}'s copy of the plugin with pixtuoid's: {e:#}",
                    t.display_name
                )),
                Some(Ok(false)) | None => {}
            },
            Ok(false) => issues.push(format!(
                "the pixtuoid plugin is not installed and enabled in {} — reconnect the source",
                t.display_name
            )),
            Err(e) => notes.push(format!(
                "could not ask {} which plugins are enabled: {e:#}",
                t.display_name
            )),
        }
    }
    verify::SchemaVerifyResult { issues, notes }
}

/// The silent-dead class `verify_schema` is blind to — the config merge reads clean while
/// the plugin FILES beside it that the gateway loads (the OpenClaw plugin DIR) are missing. INVARIANT
/// (#387): a NEW code-shipping path in `install_target` MUST gain a check here, UNLESS the
/// file IS that target's owned primary — its `verify` covers it instead (the opencode
/// plugin and the omp bridge extension are that shape).
fn verify_beside(
    render: target::RenderBeside,
    display_name: &str,
    issues: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    // A placeholder hook arg yields the real install locations WITHOUT resolving the
    // binary: a read-only check must not hard-error just because it isn't locatable.
    let arts = match render(io::HOOK_BIN) {
        Ok(arts) => arts,
        Err(e) => {
            notes.push(format!("could not resolve plugin artifact paths: {e}"));
            return;
        }
    };
    let mut missing: Vec<PathBuf> = Vec::new();
    // Pinned by `verify_target_hard_flags_a_missing_beside_file_for_every_beside_target`.
    for (p, intended) in arts {
        if !p.exists() {
            missing.push(p);
            continue;
        }
        let Ok(installed) = io::read_config(&p) else {
            notes.push(format!("could not read {}", crate::display_path(&p)));
            continue;
        };
        check_beside_shim(&p, &installed, &intended, display_name, issues);
    }
    issues.extend(missing_artifact_issue(&missing));
}

/// Stat the shim path a beside file bakes: existence misses a shim that MOVED — a green
/// doctor over a plugin whose every forward fails.
fn check_beside_shim(
    p: &std::path::Path,
    installed: &str,
    intended: &str,
    display_name: &str,
    issues: &mut Vec<String>,
) {
    if !intended.contains(verify::BAKED_HOOK_MARKER) {
        return;
    }
    // HARD, as an owned primary's unreadable `HOOK_PATH` is: no shim reads back, so
    // a launch cannot re-render it and the plugin forwards nothing.
    match verify::baked_hook_path(installed) {
        Some(baked) => check_shim_binary(&baked, issues),
        None => issues.push(format!(
            "could not read the baked shim path from {} — reconnect {display_name}",
            crate::display_path(p)
        )),
    }
}

/// The HARD issue for owned files that differ from this binary's render. Each launch
/// rewrites them ([`converge`]), so the boot warning shows it only when that failed. A
/// missing file is the primary's read or [`verify_beside`]'s to report.
fn verify_owned_drift(
    t: &Target,
    files: target::OwnedFiles,
    primary: &std::path::Path,
    issues: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    match owned_state(files, t.hook_command, primary) {
        Ok(OwnedState::Differs(stale)) => {
            let present: Vec<String> = stale
                .iter()
                .filter(|(p, _)| p.exists())
                .map(|(p, _)| crate::display_path(p))
                .collect();
            if !present.is_empty() {
                issues.push(format!(
                    "{} differs from the plugin this pixtuoid ships (a launch rewrites it; \
                     the log names a rewrite that failed)",
                    present.join(", ")
                ));
            }
        }
        Ok(OwnedState::Current | OwnedState::Unknown) => {}
        Err(e) => notes.push(format!(
            "could not compare the plugin files with this pixtuoid's: {e:#}"
        )),
    }
}

/// How a target's owned files on disk compare with this binary's render of them.
#[derive(Debug)]
pub(crate) enum OwnedState {
    Current,
    /// Each file that differs or is missing, with what this binary renders for it.
    Differs(Vec<(PathBuf, String)>),
    /// The first file is gone or no longer gives back its shim, so only a reconnect
    /// knows what to bake.
    Unknown,
}

/// Compare `files` byte for byte with this binary's render, re-rendered with the shim
/// the install baked, so a build run from elsewhere (a worktree) never re-points a
/// real plugin at its own shim.
pub(crate) fn owned_state(
    files: target::OwnedFiles,
    hook_command: target::HookCommand,
    primary: &std::path::Path,
) -> Result<OwnedState> {
    let located = files.render(primary, io::HOOK_BIN)?;
    let Some((entry, _)) = located.first() else {
        return Ok(OwnedState::Unknown);
    };
    let shim = files.installed_shim(&io::read_config(entry)?);
    let Some(hook) = rehook(hook_command, &shim)? else {
        return Ok(OwnedState::Unknown);
    };
    let differs: Vec<_> = files
        .render(primary, &hook)?
        .into_iter()
        .filter(|(p, want)| io::read_config(p).map_or(true, |on_disk| on_disk != *want))
        .collect();
    Ok(if differs.is_empty() {
        OwnedState::Current
    } else {
        OwnedState::Differs(differs)
    })
}

/// What a convergence did to a target's owned files.
#[derive(Debug, Default)]
pub(crate) struct Converged {
    pub(crate) rewritten: Vec<PathBuf>,
    /// Each file it could not write, the others still converged.
    pub(crate) failed: Vec<(PathBuf, anyhow::Error)>,
}

/// Rewrite each of `t`'s owned files that differs from this binary's render. Nothing
/// else: a config merge or a host registration is a consent only `connect` asks for,
/// so [`converge_files`] is never handed either.
pub(crate) fn converge(t: &Target, config: Option<PathBuf>) -> Result<Converged> {
    let Some(files) = t.primary.owned_files() else {
        return Ok(Converged::default());
    };
    let primary = config
        .map(Ok)
        .unwrap_or_else(|| (t.default_config_path)())?;
    converge_files(files, t.hook_command, &primary)
}

fn converge_files(
    files: target::OwnedFiles,
    hook_command: target::HookCommand,
    primary: &std::path::Path,
) -> Result<Converged> {
    let mut done = Converged::default();
    let OwnedState::Differs(stale) = owned_state(files, hook_command, primary)? else {
        return Ok(done);
    };
    for (p, _) in stale {
        // Re-derived under the lock: a concurrent connect may have re-pointed the shim.
        let wrote = write_owned_file(&p, || {
            Ok(match owned_state(files, hook_command, primary)? {
                OwnedState::Differs(now) => now.into_iter().find(|(q, _)| *q == p).map(|(_, w)| w),
                OwnedState::Current | OwnedState::Unknown => None,
            })
        });
        match wrote {
            Ok(true) => done.rewritten.push(p),
            Ok(false) => {}
            Err(e) => done.failed.push((p, e)),
        }
    }
    Ok(done)
}

/// The hook command this binary writes for an installed shim: an explicit path
/// stays explicit and a PATH-resolved bare name stays bare, while the command's
/// shape is this binary's.
fn rehook(hook_command: target::HookCommand, shim: &verify::ShimRef) -> Result<Option<String>> {
    Ok(match shim {
        verify::ShimRef::Absolute(p) => Some(hook_command(p, true)?),
        verify::ShimRef::BareName => Some(hook_command(std::path::Path::new(io::HOOK_BIN), false)?),
        verify::ShimRef::Unknown => None,
    })
}

/// The HARD issue(s) for missing code artifacts, collapsed when they share a
/// directory — the whole plugin dir being gone is ONE fact, and listing every
/// absolute path overran the Sources panel's detail line into a marquee crawl.
fn missing_artifact_issue(missing: &[PathBuf]) -> Vec<String> {
    let [first, rest @ ..] = missing else {
        return Vec::new();
    };
    let dir = first.parent();
    if !rest.is_empty() && dir.is_some() && rest.iter().all(|p| p.parent() == dir) {
        let names: Vec<String> = missing
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        return vec![format!(
            "{} plugin artifacts missing from {}: {}",
            missing.len(),
            crate::display_path(dir.unwrap_or(first)),
            crate::strip_control_chars(&names.join(", "))
        )];
    }
    missing
        .iter()
        .map(|p| format!("plugin artifact missing: {}", crate::display_path(p)))
        .collect()
}

/// Stat one resolved shim path — the ONE check shared by an embedded hook command
/// and a code artifact's baked `HOOK_PATH`, so the two can't report a moved binary
/// differently.
pub(crate) fn check_shim_binary(p: &std::path::Path, issues: &mut Vec<String>) {
    let shown = crate::display_path(p);
    if !p.exists() {
        issues.push(format!("shim binary missing: {shown}"));
    } else if !is_executable(p) {
        issues.push(format!("shim binary not executable: {shown}"));
    }
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    // Windows has no executable bit; the caller already confirmed existence.
    p.exists()
}

/// A Windows drive-relative path (`C:foo.exe` — a drive prefix but no root).
/// `is_relative()` is true for it, yet `cwd.join` replaces NOTHING (std: a
/// path with a prefix replaces self in its entirety), so the absolutization
/// arm would silently no-op and embed a command that resolves against the
/// hook-spawner's per-drive cwd.
fn is_drive_relative(p: &std::path::Path) -> bool {
    !p.has_root() && matches!(p.components().next(), Some(std::path::Component::Prefix(_)))
}

/// The [`io::HOOK_OVERRIDE_ENV`] override always wins, and the returned bool reports
/// that, so `install_target` EMBEDS it rather than discarding the user's choice for a
/// bare PATH-resolved name. A `locate` failure is fatal only for targets that EMBED the
/// path (`BinaryStrategy::EmbedAbsolute`); the bare-name/PATH ones fall back to it so a
/// fresh-machine install still succeeds.
fn resolve_hook_binary_from(
    t: &Target,
    env_hook: Option<PathBuf>,
    locate: impl FnOnce() -> Result<PathBuf>,
) -> Result<(PathBuf, bool)> {
    // An EXPLICIT path gets EMBEDDED into the config, where a relative path would
    // resolve against the CLI's cwd at hook time and hooks would never fire.
    if let Some(p) = env_hook {
        let origin = io::HOOK_OVERRIDE_ENV;
        if is_drive_relative(&p) {
            bail!(
                "{origin} {} is drive-relative (a drive prefix with no root, like C:foo.exe) \
                 and would resolve against a per-drive cwd at hook time; pass an absolute path",
                p.display()
            );
        }
        // Plain join, not canonicalize — Windows canonicalize yields a \\?\ verbatim
        // path that the cmd.exe bare form can't take.
        let p = if p.is_relative() {
            // A failed cwd query must NOT fall back to embedding the relative path —
            // that re-creates the never-fires bug absolutization exists to prevent.
            let cwd = std::env::current_dir().with_context(|| {
                format!("{origin} is relative and the current directory is unreadable; pass an absolute path")
            })?;
            cwd.join(&p)
        } else {
            p
        };
        if !p.exists() {
            // tracing, not println!: install runs under the TUI alt-screen, where a stdout
            // write corrupts the frame. `?` escapes — `connect`/`setup` route to RAW stderr.
            tracing::warn!(
                %origin,
                path = ?p,
                "hook binary does not exist yet; the hook will fail until it does"
            );
        }
        return Ok((p, true));
    }
    match locate() {
        Ok(p) => Ok((p, false)),
        Err(e) if t.binary_strategy == BinaryStrategy::EmbedAbsolute => Err(e),
        Err(_) => Ok((PathBuf::from(io::HOOK_BIN), false)),
    }
}

#[derive(Debug)]
pub(crate) enum InstallOutcome {
    Installed,
    AlreadyUpToDate,
}

#[derive(Debug)]
pub(crate) struct InstallReport {
    pub outcome: InstallOutcome,
    /// True when the bare `pixtuoid-hook` isn't on PATH (Claude/Unix, no explicit
    /// hook).
    pub path_warning: bool,
    /// The target's `post_install_hint` — a step the user must still take for the
    /// install to take effect (OpenClaw's running gateway must restart).
    pub post_install_hint: Option<&'static str>,
}

/// Install pixtuoid hooks into `t`'s config, returning a structured report.
pub(crate) fn install_target(t: &Target, config: Option<PathBuf>) -> Result<InstallReport> {
    install_target_from(
        t,
        config,
        pixtuoid_core::platform::path_env(io::HOOK_OVERRIDE_ENV),
    )
}

/// [`install_target`] with its [`io::HOOK_OVERRIDE_ENV`] read injected. The ConfigLock
/// round (read→merge→write) is the load-bearing write authority (invariant #4) and
/// stays intact here; it serializes pixtuoid only against pixtuoid, since the agent CLI
/// itself cannot honor this lock. Reads and writes go through the guard's PINNED
/// resolution — re-resolving `path` splits the round across two files on a symlink retarget.
fn install_target_from(
    t: &Target,
    config: Option<PathBuf>,
    env_hook: Option<PathBuf>,
) -> Result<InstallReport> {
    let path = config
        .map(Ok)
        .unwrap_or_else(|| (t.default_config_path)())?;
    let (binary, explicit_hook) = resolve_hook_binary_from(t, env_hook, io::default_hook_binary)?;
    let hook_cmd = (t.hook_command)(&binary, explicit_hook)?;
    // Lost-update TOCTOU: two concurrent pixtuoid runs would otherwise interleave
    // read(A)→write(B)→write(A), and A's rename clobbers B's change.
    let lock = io::lock_config(&path)?;
    let content = lock.read()?;
    let prior = target::config_present(&path).then(|| content.clone());
    let (outcome, beside) = match t.primary {
        Primary::Config {
            merge_install,
            beside,
            ..
        } => {
            // Merge FIRST so a present-but-malformed config bails BEFORE we touch the
            // filesystem — else the owned files land on disk as orphans registered
            // nowhere (partial install).
            let outcome = merge_install(&content, &hook_cmd)
                .with_context(|| format!("processing {}", path.display()))?;
            let files = beside.map(|r| r(&hook_cmd)).transpose()?;
            (outcome, files.unwrap_or_default())
        }
        Primary::Owned { render, .. } => {
            let rendered =
                render(&hook_cmd).with_context(|| format!("processing {}", path.display()))?;
            let outcome = target::MergeOutcome {
                changed: content != rendered,
                content: rendered,
            };
            (outcome, Vec::new())
        }
    };
    write_beside(&beside)?;
    // Independent of whether the content changed — a no-op re-install on a box without
    // pixtuoid-hook on PATH would otherwise warn nothing. Skipped for an embedded path.
    let path_warning = t.binary_strategy == BinaryStrategy::BareNameOnPath
        && !explicit_hook
        && !io::hook_on_path();
    if outcome.changed {
        lock.write_atomic(&outcome.content)?;
    }
    // After the hooks file is written, so the plugin never registers empty, and
    // still under its lock, so a failed registration puts back the hooks file this
    // call wrote: the hooks ARE the connected fact (`has_hooks`), and a plugin that
    // never registered fires none of the new ones. Legacy hooks `connect_host`
    // already stripped stay stripped.
    if let Some(host) = t.host
        && let Err(e) = connect_host(host, &path)
    {
        if outcome.changed
            && let Err(undo) = restore(&lock, prior.as_deref())
        {
            return Err(e.context(format!("the hooks it wrote stay in place: {undo:#}")));
        }
        return Err(e);
    }
    Ok(InstallReport {
        outcome: if outcome.changed {
            InstallOutcome::Installed
        } else {
            InstallOutcome::AlreadyUpToDate
        },
        path_warning,
        post_install_hint: t.post_install_hint,
    })
}

/// Strip what an older pixtuoid merged into the CLI's own config, then register. A
/// registration that fails after the strip leaves neither firing, which is what a
/// failed connect reads as; a CLI that isn't on PATH fails it before the strip.
fn connect_host(host: &target::HostRegistration, primary: &std::path::Path) -> Result<()> {
    host.cli.require()?;
    strip_legacy(host)?;
    (host.register)(primary)
}

/// Whether it removed anything from the host's [`target::LegacyConfig`].
fn strip_legacy(host: &target::HostRegistration) -> Result<bool> {
    let Some(legacy) = &host.legacy else {
        return Ok(false);
    };
    let path = (legacy.path)()?;
    // The CLI's own config resolves from its home, not the primary a test passes.
    #[cfg(test)]
    assert!(
        path.starts_with(std::env::temp_dir()),
        "a test is about to strip {} — point the CLI's home at a TempDir first",
        path.display()
    );
    strip_managed(legacy.strip, &path)
}

/// Put the locked file back as an install found it: `prior`, or no file at all.
fn restore(lock: &io::ConfigLock, prior: Option<&str>) -> Result<()> {
    match prior {
        Some(content) => lock.write_atomic(content),
        None => Ok(std::fs::remove_file(lock.target())?),
    }
}

/// Remove the entries `uninstall` recognizes from `path` under its lock; whether
/// it rewrote. Never rewrites on a semantic no-op.
fn strip_managed(
    uninstall: fn(&str) -> Result<target::MergeOutcome>,
    path: &std::path::Path,
) -> Result<bool> {
    // Decided BEFORE locking: `lock_config` creates the parent dir + a .lock sidecar, and
    // materializing ~/.reasonix here would flip that target's presence probe on a no-op.
    if !target::config_present(path) {
        return Ok(false);
    }
    let lock = io::lock_config(path)?;
    let content = lock.read()?;
    let outcome = uninstall(&content).with_context(|| format!("processing {}", path.display()))?;
    // A byte compare would falsely fire on hand formatting.
    if !outcome.changed {
        return Ok(false);
    }
    lock.write_atomic(&outcome.content)?;
    Ok(true)
}

/// Write each of `files` that differs on disk. Called before the primary's
/// WRITE so a re-install refreshes them even when the merge is a no-op, which heals a
/// deleted or tampered plugin file. Each file is its own lock target, disjoint from the
/// primary's lock the caller holds and taken in a consistent primary→file order, so no
/// self-deadlock.
fn write_beside(files: &[(PathBuf, String)]) -> Result<()> {
    for (p, c) in files {
        write_owned_file(p, || Ok(Some(c.clone())))?;
    }
    Ok(())
}

/// Rewrite `t`'s [`RetiredStub`](target::RetiredStub) where it sits byte-exact;
/// whether it did. A missing file is left missing.
pub(crate) fn heal_retired_stub(t: &Target, config: Option<PathBuf>) -> Result<bool> {
    let Primary::Owned {
        retired: Some(stub),
        ..
    } = &t.primary
    else {
        return Ok(false);
    };
    let path = config
        .map(Ok)
        .unwrap_or_else(|| (t.default_config_path)())?;
    // Checked before the lock, which would create its file beside a CLI the
    // user may not have.
    if io::read_config(&path)? != stub.was {
        return Ok(false);
    }
    write_owned_file(&path, || {
        Ok((io::read_config(&path)? == stub.was).then(|| stub.now.to_owned()))
    })
}

/// Write `want()` to `p` under `p`'s own lock when the file holds anything else, and
/// say whether it did. `want` runs under the lock, so a caller can re-derive the
/// content there; `None` leaves the file alone.
fn write_owned_file(
    p: &std::path::Path,
    want: impl FnOnce() -> Result<Option<String>>,
) -> Result<bool> {
    // A real write here would be invisible: CI has no such dir, and the shim exits
    // 0 by invariant #5, so nothing downstream would report it.
    #[cfg(test)]
    assert!(
        p.starts_with(std::env::temp_dir()),
        "a test is about to write {} outside the temp dir — redirect the target's \
         own state resolver (OpenClaw: OPENCLAW_STATE_DIR) at a TempDir first",
        p.display()
    );
    let lock = io::lock_config(p)?;
    let Some(want) = want()? else {
        return Ok(false);
    };
    if lock.read()? == want {
        return Ok(false);
    }
    // Atomic (temp-in-dir → fsync → rename), NOT a plain `fs::write`: a torn write
    // leaves a half-rendered plugin the gateway then fails to load.
    lock.write_atomic(&want)
        .with_context(|| format!("writing {}", p.display()))?;
    Ok(true)
}

#[derive(Debug)]
pub(crate) enum UninstallOutcome {
    Removed,
    NothingToRemove,
}

#[derive(Debug)]
pub(crate) struct UninstallReport {
    pub outcome: UninstallOutcome,
    /// The host CLI that couldn't be reached to deregister its plugin, which stays
    /// registered.
    pub plugin_left_registered: Option<io::HostCli>,
}

/// Remove pixtuoid hooks from `t`'s config, returning a structured report. Same
/// lock scope as `install_target`, and never rewrites on a semantic no-op.
pub(crate) fn uninstall_target(t: &Target, config: Option<PathBuf>) -> Result<UninstallReport> {
    let path = config
        .map(Ok)
        .unwrap_or_else(|| (t.default_config_path)())?;
    let mut removed = strip_managed(t.primary.uninstall(), &path)?;
    // After the strip, which needs no CLI: a failing deregister leaves a
    // registered plugin whose hooks are gone, or whose copy fires into a
    // disconnected source, which drops the events.
    let plugin_left_registered = match t.host {
        Some(host) => match (host.unregister)(&path)? {
            target::Unregistered::Removed => {
                removed = true;
                None
            }
            target::Unregistered::Absent => None,
            target::Unregistered::Unreachable => Some(host.cli),
        },
        None => None,
    };
    // Last, so a legacy config it cannot parse fails the disconnect only once the
    // plugin is gone.
    if let Some(host) = t.host {
        removed |= strip_legacy(host)?;
    }
    Ok(UninstallReport {
        outcome: if removed {
            UninstallOutcome::Removed
        } else {
            UninstallOutcome::NothingToRemove
        },
        plugin_left_registered,
    })
}

/// Deleting a registered event ships GREEN — cargo-mutants does not mutate slice
/// initializers and nothing else asserts the SET, which is how both of #929's headline
/// registration fixes could be silently removed. Update a pin deliberately when its
/// roster changes.
#[cfg(test)]
pub(crate) fn assert_event_roster<T: Ord + std::fmt::Debug + Copy>(
    name: &str,
    actual: &[T],
    expected: &[T],
) {
    use std::collections::BTreeSet;
    assert_eq!(
        actual.iter().copied().collect::<BTreeSet<_>>(),
        expected.iter().copied().collect::<BTreeSet<_>>(),
        "{name} membership changed — a registered event that vanishes is a \
         shipping bug no other test can see."
    );
}

/// The events a bridge template's `FORWARD` set names, read out of it rather
/// than hand-copied beside it.
#[cfg(test)]
pub(crate) fn forward_set(template: &'static str) -> std::collections::BTreeSet<&'static str> {
    let block = template
        .split_once("const FORWARD = new Set<string>([")
        .and_then(|(_, rest)| rest.split_once("])"))
        .map(|(inner, _)| inner)
        .expect("the template defines a FORWARD set");
    block
        .split(',')
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
