//! `pixtuoid doctor` — a diagnosis of this machine's setup, one category per
//! concern (`render`). It surfaces what otherwise dies unread: the decode-drift
//! breadcrumbs (`source/drift.rs`, under the [`drift::TARGET`] tracing target) in
//! the warn-floor log. Strictly READ-ONLY: it never writes config (re-connecting
//! hooks stays the Sources panel's job) and never spawns the TUI.
//! The PROBED CLI is not read-only about its own state — see `may_probe_version`.
//! The shim an install names does run, against a throwaway endpoint, and a hook
//! endpoint nothing holds is bound for a moment, as the next instance would — see
//! [`pixtuoid_core::source::hook::endpoint_status`].

mod delivery;

use pixtuoid_core::source::registry::Hyphen;
use pixtuoid_core::source::{drift, registry};

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct LogScanResult {
    pub unknown_event: u64,
    pub missing_field: u64,
    pub unknown_dispatch: u64,
    pub shape_drift: u64,
    /// Sanitized, deduped, capped distinctive values — safe to print.
    pub samples: Vec<String>,
    /// The leading timestamp token of the latest matching log line, sanitized:
    /// `PIXTUOID_LOG` may name any file.
    pub last_ts: Option<String>,
}

impl LogScanResult {
    pub(crate) fn total(&self) -> u64 {
        self.unknown_event + self.missing_field + self.unknown_dispatch + self.shape_drift
    }
}

const SAMPLE_CAP: usize = 5;

use crate::strip_control_chars as sanitize;

struct DriftLine<'a> {
    source: &'a str,
    kind: drift::DriftKind,
    /// The fields segment AFTER the `target:` marker, so a span field of the
    /// same name (rendered BEFORE the target) can't be picked up.
    fields: &'a str,
}

/// Anchored on the STRUCTURAL tracing-fmt `target:` marker, not a loose
/// `contains`: a line merely MENTIONING the literal in a field value must not
/// match, and a longer `a::b::pixtuoid::drift` must not suffix-match ours.
/// `marker` is `"<TARGET>: "`, hoisted by the caller to avoid a per-line alloc.
/// Residual: a value literally embedding ` <TARGET>: source=… kind=… ` matches.
fn parse_drift_line<'a>(line: &'a str, marker: &str) -> Option<DriftLine<'a>> {
    let at = line.find(marker)?;
    if at != 0 && line.as_bytes()[at - 1] != b' ' {
        return None; // suffix of a longer target, not our standalone token
    }
    let fields = &line[at + marker.len()..];
    Some(DriftLine {
        source: field_value(fields, drift::SOURCE_FIELD)?,
        kind: field_value(fields, drift::KIND_FIELD)?.parse().ok()?,
        fields,
    })
}

/// Pull a field value from a tracing-fmt fields segment, in both the quoted
/// (`key="…"`) and unquoted Display (`key=val`) forms. An unquoted value runs to
/// the next ` <ident>=` field boundary, not merely the next whitespace, so a
/// hostile wire name containing spaces survives whole; and the key must START a
/// field so `name` can't match inside `displayName=`.
fn field_value<'a>(seg: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}=");
    let mut from = 0;
    let val_start = loop {
        let abs = from + seg[from..].find(&pat)?;
        if abs == 0 || seg.as_bytes()[abs - 1] == b' ' {
            break abs + pat.len();
        }
        from = abs + pat.len();
    };
    let rest = &seg[val_start..];
    if let Some(after_q) = rest.strip_prefix('"') {
        Some(&after_q[..after_q.find('"').unwrap_or(after_q.len())])
    } else {
        Some(rest[..next_field_boundary(rest).unwrap_or(rest.len())].trim_end())
    }
}

fn next_field_boundary(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    (0..b.len()).find(|&i| {
        if b[i] != b' ' {
            return false;
        }
        let mut j = i + 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        j > i + 1 && j < b.len() && b[j] == b'='
    })
}

fn push_sample(samples: &mut Vec<String>, v: Option<&str>) {
    if let Some(v) = v {
        let s = sanitize(v);
        if !s.is_empty() && samples.len() < SAMPLE_CAP && !samples.contains(&s) {
            samples.push(s);
        }
    }
}

/// Scan warn-floor log TEXT (not a path, so it's testable against real fmt
/// output) for `pixtuoid::drift` breadcrumbs for ONE source.
pub(crate) fn scan_log_for_source(log: &str, source: &str) -> LogScanResult {
    let mut r = LogScanResult::default();
    let marker = format!("{}: ", drift::TARGET);
    for line in log.lines() {
        let Some(p) = parse_drift_line(line, &marker) else {
            continue;
        };
        if p.source != source {
            continue;
        }
        match p.kind {
            drift::DriftKind::UnknownEvent => {
                r.unknown_event += 1;
                push_sample(&mut r.samples, field_value(p.fields, "name"));
            }
            drift::DriftKind::MissingField => r.missing_field += 1,
            drift::DriftKind::UnknownDispatch => {
                r.unknown_dispatch += 1;
                push_sample(&mut r.samples, field_value(p.fields, "tool"));
            }
            drift::DriftKind::ShapeDrift => r.shape_drift += 1,
        }
        if let Some(ts) = line.split_whitespace().next() {
            r.last_ts = Some(sanitize(ts));
        }
    }
    r
}

/// The sources a decode-drift breadcrumb has named in this run, first seen
/// first: the footer's nudge, kept as the events arrive, so a run reports
/// only its own (WezTerm's ring log and Alacritty's message bar likewise show
/// what their process logged). History stays with `doctor`, which reads the
/// retained runs' logs.
#[derive(Debug, Clone, Default)]
pub(crate) struct DriftSeen(
    std::sync::Arc<std::sync::Mutex<Vec<&'static registry::SourceDescriptor>>>,
);

impl DriftSeen {
    /// The label prefixes (e.g. `"cc"`) of the sources seen.
    pub(crate) fn prefixes(&self) -> Vec<&'static str> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|d| d.label_prefix)
            .collect()
    }

    /// The layer that records into this, filtered to the breadcrumbs alone so
    /// it never sees, or slows, any other event.
    pub(crate) fn layer<S>(&self) -> impl tracing_subscriber::Layer<S>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        use tracing_subscriber::Layer;
        // Exactly the target: `Targets` would match it as a prefix too.
        self.clone()
            .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                meta.target() == drift::TARGET && *meta.level() <= tracing::Level::WARN
            }))
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DriftSeen {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Source(Option<&'static registry::SourceDescriptor>);
        impl tracing::field::Visit for Source {
            // A breadcrumb's `source` is a `%` field, whose Debug is its Display.
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == drift::SOURCE_FIELD {
                    self.0 = registry::descriptor_for(&format!("{value:?}"));
                }
            }
        }
        let mut source = Source(None);
        event.record(&mut source);
        let Some(descriptor) = source.0 else {
            return;
        };
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !seen.iter().any(|d| std::ptr::eq(*d, descriptor)) {
            seen.push(descriptor);
        }
    }
}

/// The one footer warning every painter shows: a source's death (HIGHEST
/// priority — the office is partially frozen), else a passive decode-drift
/// nudge naming each `drifted` source prefix.
pub fn footer_warning(
    deaths: &[pixtuoid_core::source::manager::SourceDeath],
    drifted: &[&str],
) -> Option<String> {
    match deaths {
        [] => {}
        [d] => {
            return Some(format!(
                "{} source died — its agents are frozen; restart pixtuoid (see log)",
                d.source
            ));
        }
        many => {
            return Some(format!(
                "{} sources died — restart pixtuoid (see log)",
                many.len()
            ));
        }
    }
    if drifted.is_empty() {
        return None;
    }
    let prefixes = drifted
        .iter()
        .map(|p| format!("{p}·"))
        .collect::<Vec<_>>()
        .join(" ");
    // No leading `⚠`: the footer painter (`footer.rs` `" ⚠ {warn} "`) owns the
    // glyph, so embedding one here double-prints it (`⚠ ⚠ decode drift`).
    Some(format!("decode drift: {prefixes} — run `pixtuoid doctor`"))
}

/// Windows "installed but no sprite": `HOME` set and differing from
/// `%USERPROFILE%` can put hooks where the CLI never reads. A SAFETY NET only —
/// `platform::home_first_dir` already mirrors the HOME-first CLIs, so this
/// catches a residual resolver mismatch. Pure (env + platform injected) so it
/// unit-tests on any host.
pub(crate) fn home_split_advisory(
    is_windows: bool,
    home: Option<&str>,
    userprofile: Option<&str>,
) -> Option<String> {
    if !is_windows {
        return None;
    }
    let home = home.map(str::trim).filter(|s| !s.is_empty())?;
    let up = userprofile.map(str::trim).filter(|s| !s.is_empty())?;
    if win_path_eq(home, up) {
        return None;
    }
    // Sanitized: HOME/USERPROFILE are user-controlled env values surfaced in the
    // report. No glyph: the presenter's category verdict owns it.
    Some(format!(
        "HOME ({}) differs from USERPROFILE ({}). pixtuoid resolves \
         CodeWhale/OpenClaw HOME-first to match their CLIs — but if a source's \
         sprite is missing, confirm its hook config landed under the home that \
         CLI actually reads.",
        sanitize(home),
        sanitize(up)
    ))
}

/// Collapses only COSMETIC differences (case, slash direction, trailing
/// separator). A `/c/Users/me`-vs-`C:\Users\me` split is a REAL divergence
/// (different roots) and must stay unequal.
fn win_path_eq(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// The SHARED rollup the Connection panel, the boot preflight, and the CLI
/// report all read, so those surfaces can't drift apart. Scope is the CHEAP
/// signals only — version skew stays report-only, because the `<cli> --version`
/// probe is too costly for an interactive panel-open across N sources.
#[derive(Debug, Default)]
pub(crate) struct SourceDiagnostics {
    /// `Some` only when hooks are installed in the target's config; `None` = not
    /// checked (no target / not installed).
    pub install: Option<crate::install::verify::SchemaVerifyResult>,
    pub(crate) drift: LogScanResult,
}

impl SourceDiagnostics {
    /// A HARD install problem ⇒ the source is broken (zero sprites despite a
    /// claimed connection); nothing else counts.
    pub(crate) fn broken_install(&self) -> Option<&crate::install::verify::SchemaVerifyResult> {
        self.install.as_ref().filter(|i| !i.is_sound())
    }

    pub(crate) fn is_broken(&self) -> bool {
        self.broken_install().is_some()
    }

    /// The single worst issue as a one-line, glyph-prefixed summary. Priority:
    /// install-broken (hooks can't fire) > decode-drift.
    pub(crate) fn summary(&self) -> Option<String> {
        if let Some(i) = self.broken_install() {
            return Some(format!("⚠ install broken: {}", i.issues.join("; ")));
        }
        let n = self.drift.total();
        if n > 0 {
            return Some(format!("⚠ {n} decode drift — run `pixtuoid doctor`"));
        }
        None
    }
}

/// The install check runs whenever the source's target has managed hooks
/// installed, NOT gated on the connected flag — the report must surface a stale
/// broken install even on a disconnected source. `config` injects a config root
/// (`None` in prod) so an install-broken verdict is exercisable through the SAME
/// root both the `has_hooks` gate and `verify_target` read.
pub(crate) fn diagnose(
    source: &str,
    log: &str,
    config: Option<std::path::PathBuf>,
) -> SourceDiagnostics {
    let install = crate::install::target::by_source(source)
        .filter(|t| crate::install::has_hooks(t, config.clone()))
        .map(|t| crate::install::verify_target(t, config.clone()));
    SourceDiagnostics {
        install,
        drift: scan_log_for_source(log, source),
    }
}

pub(crate) struct DoctorSourceRow {
    /// Its `name` is the registry source id (e.g. "claude-code"), distinct from
    /// `install::Target.name`.
    pub desc: &'static registry::SourceDescriptor,
    pub connected: bool,
    pub has_target: bool,
    pub hooks_installed: bool,
    pub installed_version: InstalledVersion,
    /// The SAME [`SourceDiagnostics`] the Sources panel + boot preflight read,
    /// embedded whole so `is_broken`/`drift` stay the ONE authority.
    pub diag: SourceDiagnostics,
}

/// A major at or above this is a CalVer year (openclaw, cursor), and a new
/// year is no breaking major.
const CALVER_MIN_MAJOR: u64 = 1000;

/// Installed-vs-verified comparison, `None` when there is no parseable anchor.
fn version_cmp(
    installed: &semver::Version,
    verified: &str,
    hyphen: Hyphen,
) -> Option<std::cmp::Ordering> {
    Some(installed.cmp_precedence(&registry::parse_version(verified, hyphen)?))
}

/// The version a skew verdict compared — for the `versions` category, where
/// showing the two COMPARED values beats echoing a raw banner.
fn compared_version(installed: &semver::Version) -> String {
    let mut v = installed.clone();
    v.build = semver::BuildMetadata::EMPTY;
    v.to_string()
}

/// A broken install outranks drift (✗ vs !); the two quiet
/// states (– no install target, ○ not installed) are dimmed, not alarmed.
fn verdict_glyph(row: &DoctorSourceRow, ink: &Ink) -> String {
    if row.diag.is_broken() {
        ink.bad("✗")
    } else if row.diag.drift.total() > 0 {
        ink.warn("!")
    } else if !row.has_target {
        ink.dim("–")
    } else if !row.hooks_installed {
        ink.dim("○")
    } else {
        ink.ok("✓")
    }
}

fn drift_detail(s: &LogScanResult) -> String {
    let mut parts = Vec::new();
    if s.unknown_event > 0 {
        parts.push(format!("{} unknown-event", s.unknown_event));
    }
    if s.missing_field > 0 {
        parts.push(format!("{} missing-field", s.missing_field));
    }
    if s.unknown_dispatch > 0 {
        parts.push(format!("{} unknown-dispatch", s.unknown_dispatch));
    }
    if s.shape_drift > 0 {
        parts.push(format!("{} shape-drift", s.shape_drift));
    }
    let when = s
        .last_ts
        .as_deref()
        .map(|t| format!(" (last {t})"))
        .unwrap_or_default();
    let samples = if s.samples.is_empty() {
        String::new()
    } else {
        format!(" [{}]", s.samples.join(", "))
    };
    format!("{}{when}{samples}", parts.join(", "))
}

/// Render one source row under the `sources` category: a scannable verdict
/// line, plus an indented `↳` continuation line per problem so the long detail
/// never wrecks the table's column alignment. Widths are applied to the PLAIN
/// text before painting — escape bytes inside a `{:<N}` pad would break the
/// columns.
fn format_doctor_row(row: &DoctorSourceRow, ink: &Ink) -> String {
    let conn = format!(
        "{:<12}",
        if row.connected {
            "connected"
        } else {
            "disconnected"
        }
    );
    let conn = if row.connected { conn } else { ink.dim(&conn) };
    let state = if !row.has_target {
        "transcript-only"
    } else if !row.hooks_installed {
        "not installed"
    } else {
        "installed"
    };
    // The RAW probe string, not a lossy reformat (cursor's `2026.06.04-5fd875e` isn't
    // semver); skew against the verified anchor is the `versions` category's job.
    let version = match &row.installed_version {
        InstalledVersion::Read(line) => line.clone(),
        InstalledVersion::Unasked => ink.dim("unknown"),
        InstalledVersion::Unknown(why) => ink.dim(&format!("unknown ({why})")),
    };
    let mut out = format!(
        "{DETAIL_INDENT}{} {}\u{b7}{:<SOURCE_W$} {} {:<15} {}",
        verdict_glyph(row, ink),
        row.desc.label_prefix,
        row.desc.name,
        conn,
        state,
        version
    );
    // Each of `issues` is stripped where it is minted (a target's
    // `verify_schema`, or `install::verify_target`), so it prints as is; `notes`
    // carry error chains that quote another CLI's files, so they are stripped here.
    if let Some(s) = &row.diag.install {
        if !s.is_sound() {
            out.push_str(&format!(
                "\n{CONT_INDENT}\u{21b3} install broken: {}",
                s.issues.join("; ")
            ));
        } else if !s.notes.is_empty() {
            out.push_str(&format!(
                "\n{CONT_INDENT}\u{21b3} note: {}",
                crate::strip_control_chars(&s.notes.join("; "))
            ));
        }
    }
    if row.diag.drift.total() > 0 {
        out.push_str(&format!(
            "\n{CONT_INDENT}\u{21b3} decode drift: {}",
            drift_detail(&row.diag.drift)
        ));
    }
    out
}

/// First non-empty line of subprocess output, trimmed AND control-char
/// `sanitize`d — `--version` output is untrusted (a PATH-substituted binary
/// could emit ANSI/OSC to manipulate the terminal).
fn first_sanitized_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(sanitize)
}

/// How long a child doctor runs, a shim or a CLI's `--version`, may take to
/// exit: far past the shim's own send bound, whose clock starts only once it
/// runs, since a first launch can stall in the OS before that.
const EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// What a source's `<cli> --version` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstalledVersion {
    /// Not asked: no probe, or no evidence the user runs the CLI
    /// ([`may_probe_version`]).
    Unasked,
    /// Its first non-empty output line, sanitized ([`first_sanitized_line`]).
    Read(String),
    /// Why it told none.
    Unknown(String),
}

impl InstalledVersion {
    /// The line it read, if it read one.
    fn read(&self) -> Option<&str> {
        match self {
            Self::Read(line) => Some(line),
            Self::Unasked | Self::Unknown(_) => None,
        }
    }
}

/// The current-thread runtime doctor's child probes run on.
///
/// # Errors
///
/// If none can be built.
fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    use anyhow::Context as _;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building doctor's tokio runtime")
}

/// Each of `probes`' `--version` at once, so k hung CLIs cost doctor one
/// `deadline`, not k; a `None` is not asked.
///
/// # Errors
///
/// If no tokio runtime can be built.
fn probe_versions<'a, A: AsRef<[&'a str]>>(
    probes: &[Option<A>],
    deadline: std::time::Duration,
) -> anyhow::Result<Vec<InstalledVersion>> {
    #[cfg(unix)]
    kill_running_probes_on_quit()?;
    let asked = probes.iter().map(|argv| async move {
        match argv {
            Some(argv) => probe_version(argv.as_ref(), deadline).await,
            None => InstalledVersion::Unasked,
        }
    });
    Ok(runtime()?.block_on(futures_util::future::join_all(asked)))
}

/// The process groups of the version probes still running. Each leads its own,
/// which a terminal's Ctrl-C never reaches.
#[cfg(unix)]
static RUNNING: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// Group `0`'s place in [`RUNNING`], for as long as this lives.
#[cfg(unix)]
struct Running(u32);

#[cfg(unix)]
impl Running {
    fn listed(group: u32) -> Self {
        let mut running = RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        running.push(group);
        Self(group)
    }
}

#[cfg(unix)]
impl Drop for Running {
    fn drop(&mut self) {
        let mut running = RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        running.retain(|&group| group != self.0);
    }
}

/// From the first call on, a quit signal kills every [`RUNNING`] group, then
/// ends doctor by its default disposition.
///
/// # Errors
///
/// If no tokio runtime can be built.
#[cfg(unix)]
fn kill_running_probes_on_quit() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ARMED: AtomicBool = AtomicBool::new(false);
    if ARMED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let runtime = runtime()?;
    // Here, before any probe runs: a signal landing before the arm would end
    // doctor with its probes left running.
    let quit = {
        let _in = runtime.enter();
        crate::runtime::QuitArms::arm()
    };
    ARMED.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        let signal = runtime.block_on(quit.signalled());
        let running = RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let groups = running
            .iter()
            .filter_map(|&group| rustix::process::Pid::from_raw(i32::try_from(group).ok()?));
        for group in groups {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        signal.reraise();
    });
    Ok(())
}

/// A source's `<cli> --version` (argv from the static registry, never user
/// input), killed with its whole process group (Unix) or Job Object (Windows)
/// once `deadline` passes: a launcher can leave the hung work to a child of its
/// own. A NONZERO exit's text never shows as a version.
async fn probe_version(argv: &[&str], deadline: std::time::Duration) -> InstalledVersion {
    use std::process::Stdio;
    let Some((cmd, args)) = argv.split_first() else {
        return InstalledVersion::Unknown("its probe names no command".into());
    };
    let mut command = process_wrap::tokio::CommandWrap::with_new(cmd, |command| {
        command
            .args(args)
            // A CLI reading the terminal would wait on it.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    {
        command.wrap(process_wrap::tokio::JobObject);
        // The job's kill on close: doctor's end, however it comes, takes the job.
        command.wrap(process_wrap::tokio::KillOnDrop);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return InstalledVersion::Unknown(format!("it would not run: {e}")),
    };
    #[cfg(unix)]
    let _running = child.id().map(Running::listed);
    let (stdout, stderr) = (child.stdout().take(), child.stderr().take());
    let ran = tokio::time::timeout(deadline, async {
        tokio::join!(child.wait(), read_all(stdout), read_all(stderr))
    })
    .await;
    let (status, stdout, stderr) = match ran {
        Ok((Ok(status), stdout, stderr)) => (status, stdout, stderr),
        Ok((Err(e), ..)) => {
            return InstalledVersion::Unknown(format!("could not wait for it: {e}"));
        }
        Err(_) => {
            // `kill` waits on what it killed, so nothing is left to reap.
            let _ = Box::into_pin(child.kill()).await;
            return InstalledVersion::Unknown(format!("it did not exit within {deadline:?}"));
        }
    };
    if !status.success() {
        return InstalledVersion::Unknown(format!("it ended with {status}"));
    }
    first_sanitized_line(&stdout)
        .or_else(|| first_sanitized_line(&stderr))
        .map_or_else(
            || InstalledVersion::Unknown("it printed nothing".into()),
            InstalledVersion::Read,
        )
}

/// What `pipe` carries until it closes; a read error keeps what came before it.
async fn read_all(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;
    let mut read = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut read).await;
    }
    read
}

/// Whether `doctor` may spawn this source's `<cli> --version` probe: only with
/// evidence the user already runs that CLI — CONNECTED, or its install target
/// probed PRESENT. `cli_detected` is `None` for a transcript-only source.
///
/// `--version` is not side-effect-free on the other side: several agent CLIs
/// bootstrap their own state dir on ANY invocation, and those dirs are exactly
/// what `presence_probe` / `detect_installed` key on, so an unconditional probe
/// MANUFACTURED the presence it was diagnosing and the natural `doctor` →
/// `setup --yes` order then installed hooks into CLIs `setup --yes` alone had
/// just declined to touch. Gating on presence removes the observer effect by
/// construction: a CLI that has already written its own state cannot be
/// perturbed into existence by one more `--version`. Pinned by
/// `doctor_never_spawns_a_version_probe_for_a_cli_it_has_no_evidence_of`.
fn may_probe_version(connected: bool, cli_detected: Option<bool>) -> bool {
    connected || cli_detected.unwrap_or(false)
}

/// The focus backend's name and whether it can work AT ALL here — the verdict
/// travels as data so the focus category never has to sniff a glyph out of the
/// display string.
fn activation_backend() -> (&'static str, bool) {
    #[cfg(target_os = "macos")]
    {
        ("NSRunningApplication (macOS)", true)
    }
    #[cfg(target_os = "linux")]
    {
        let (msg, healthy) = linux_activation_backend(
            marker_set(crate::focus::SWAY_ENV),
            marker_set(crate::focus::HYPRLAND_ENV),
            marker_set("WAYLAND_DISPLAY"),
            marker_set("DISPLAY"),
        );
        (msg, healthy)
    }
    #[cfg(windows)]
    {
        (
            "SetForegroundWindow (Windows) — the foreground lock may still deny",
            true,
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        ("none — focus-jump is unsupported on this OS", false)
    }
}

/// Is a compositor/display env marker actually SET? EMPTY (or whitespace-only) counts as
/// UNSET, NOT bare presence: a leftover `WAYLAND_DISPLAY=`/`SWAYSOCK=` (systemd user units
/// and non-forwarded ssh sessions leave them routinely) would otherwise print a confidently
/// wrong verdict at a user whose X11 EWMH channel works fine. `focus::linux::detect_channel`
/// keys the live channel through the SAME reader.
#[cfg_attr(
    all(not(target_os = "linux"), not(test)),
    expect(
        dead_code,
        reason = "only the Linux activation backend reads it off test"
    )
)]
fn marker_set(name: &str) -> bool {
    pixtuoid_core::platform::path_env(name).is_some()
}

/// Mirrors `focus/linux.rs`'s ONE-channel-per-env order (sway IPC → hyprland IPC → X11
/// EWMH → nothing) — EXCEPT that a Wayland session without a pid-addressable IPC must NOT
/// be reported as "X11 EWMH ✓": XWayland sets $DISPLAY, but a native-Wayland terminal never
/// appears in XWayland's client list and mutter/kwin block focus-steal anyway, so the ✓
/// would mislead exactly the users focus fails for.
#[cfg_attr(
    all(not(target_os = "linux"), not(test)),
    expect(
        dead_code,
        reason = "only the Linux activation backend reads it off test"
    )
)]
fn linux_activation_backend(
    sway: bool,
    hyprland: bool,
    wayland: bool,
    x11: bool,
) -> (&'static str, bool) {
    if sway {
        ("sway IPC (swaymsg)", true)
    } else if hyprland {
        ("hyprland IPC (hyprctl)", true)
    } else if wayland {
        (
            "Wayland compositor without a pid-addressable focus channel (GNOME/KDE \
             forbid focus-steal; xdg-activation unimplemented) — focus will silently \
             no-op for native-Wayland terminals",
            false,
        )
    } else if x11 {
        ("X11 EWMH ($DISPLAY)", true)
    } else {
        ("none detected — focus will silently no-op", false)
    }
}

mod shown {
    /// A path as the report prints it, [`display_path`](crate::display_path)ed
    /// when minted; the private field means no path field reaches a render
    /// site raw.
    pub(super) struct ShownPath(String);

    impl ShownPath {
        pub(super) fn new(p: impl AsRef<std::path::Path>) -> Self {
            Self(crate::display_path(p.as_ref()))
        }
    }

    impl std::fmt::Display for ShownPath {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
}
use shown::ShownPath;

/// One resolved transcript root — the data behind a roots-category row.
struct RootStatus {
    source: &'static str,
    root: ShownPath,
    exists: bool,
    /// `(env var, set)` — the override that may explain a missing root.
    env: Option<(&'static str, bool)>,
}

/// Everything `doctor` probed, separated from rendering, so `render` is
/// drivable off a hand-built report (no env/fs/subprocess probing in the
/// render path).
struct DoctorReport {
    log_path: ShownPath,
    config_path: ShownPath,
    config_warnings: Vec<String>,
    /// Minted only by [`read_log`](crate::run_log::read_log), which strips it; the render prints it as
    /// is.
    log_warning: Option<String>,
    term_env: Option<String>,
    colorterm_env: Option<String>,
    /// `None` when the DECRQSS truecolor probe never ran.
    truecolor_probe: Option<crate::term::Truecolor>,
    color_pf: crate::term::ColorPreflight,
    graphics_plan: crate::graphics::Plan,
    /// The plan `run`'s look without a flag — the config's, then the default —
    /// makes over the cutaway `graphics_plan` found, where it found one
    /// ([`crate::graphics::LookChoice::over_here`]).
    run_plan: Option<crate::graphics::Plan>,
    rows: Vec<DoctorSourceRow>,
    delivery: delivery::Delivery,
    roots: Vec<RootStatus>,
    backend: &'static str,
    /// `false` = the focus backend itself can't work here (the Wayland /
    /// no-display arms) — the focus category's verdict, not a probe result.
    backend_healthy: bool,
    /// One per probe-backed source, in registry order.
    probes: Vec<ProbePath>,
    home_split: Option<String>,
    /// Whether the report may carry ANSI color — see [`report_color`].
    color: bool,
}

/// Whether the report may carry ANSI color: a tty with color allowed by the
/// SAME `color_preflight` policy the launcher acts on, or `$CLICOLOR_FORCE`
/// (bixense: forces color even piped) — except `$TERM=dumb`, which outranks
/// the force (the preflight's own rule: forcing color can't fix a terminal
/// that renders no escapes).
fn report_color(tty: bool, pf: crate::term::ColorPreflight, clicolor_force: Option<&str>) -> bool {
    pf != crate::term::ColorPreflight::RefuseDumbTerm
        && (crate::term::clicolor_forced(clicolor_force)
            || (tty && matches!(pf, crate::term::ColorPreflight::Proceed)))
}

/// ANSI paint, a no-op when [`DoctorReport::color`] is off. The escape codes
/// come from crossterm — the binary's terminal authority — never hand-rolled.
struct Ink {
    on: bool,
}

impl Ink {
    fn s(&self, s: &str, f: impl FnOnce(&str) -> String) -> String {
        if self.on { f(s) } else { s.to_string() }
    }
    fn ok(&self, s: &str) -> String {
        use crossterm::style::Stylize;
        self.s(s, |s| s.green().to_string())
    }
    fn warn(&self, s: &str) -> String {
        use crossterm::style::Stylize;
        self.s(s, |s| s.yellow().to_string())
    }
    fn bad(&self, s: &str) -> String {
        use crossterm::style::Stylize;
        self.s(s, |s| s.red().to_string())
    }
    fn hint(&self, s: &str) -> String {
        use crossterm::style::Stylize;
        self.s(s, |s| s.cyan().to_string())
    }
    fn dim(&self, s: &str) -> String {
        use crossterm::style::Stylize;
        self.s(s, |s| s.dim().to_string())
    }
    fn bold(&self, s: &str) -> String {
        use crossterm::style::Stylize;
        self.s(s, |s| s.bold().to_string())
    }
}

/// Each query only on a real tty and a non-dumb `$TERM` (`probe_ok`): the same
/// `color_preflight` gate the launcher acts on, so the rows match `run`.
fn probe_terminal_caps(
    probe_ok: bool,
    look: crate::graphics::LookChoice,
    max_density: pixtuoid_scene::sprite::format::Density,
) -> (Option<crate::term::Truecolor>, crate::graphics::Plan) {
    let truecolor_probe = probe_ok.then(crate::term::query_truecolor);
    let graphics_plan = crate::graphics::plan_this_terminal(look, max_density, probe_ok);
    (truecolor_probe, graphics_plan)
}

/// A wrong root has no symptom but an empty office, so state it outright (#880). Goes
/// through `resolved_source_root` — the SAME call the driver makes, since a second copy
/// could show a healthy path while the watcher polled another.
fn collect_roots() -> Vec<RootStatus> {
    registry::REGISTRY
        .iter()
        .filter_map(|d| {
            let root = pixtuoid_core::source::resolved_source_root(d.name)?;
            let env = d
                .home_env
                // The SAME `path_env` the resolvers use, or the two disagree: `env::var` reads
                // a non-UTF-8 override as UNSET, silencing the ⚠ when the root IS wrong (#172).
                .map(|v| (v, pixtuoid_core::platform::path_env(v).is_some()));
            let exists = root.is_dir();
            Some(RootStatus {
                source: d.name,
                root: ShownPath::new(root),
                exists,
                env,
            })
        })
        .collect()
}

/// One probe-backed source's click-time path ([`shown_path`]), and whether it is
/// there, or why its layout hosts none. Read through the same
/// `FocusProbe::paths` a click reads, so the report cannot claim a path a click
/// would not.
struct ProbePath {
    source: &'static registry::SourceDescriptor,
    path: Result<(ShownPath, bool), &'static str>,
}

fn probe_paths() -> Vec<ProbePath> {
    registry::REGISTRY
        .iter()
        .filter_map(|source| {
            let probe = source.focus_probe()?;
            let path = (probe.paths)().map(|paths| {
                let (shown, present) = shown_path(&paths, probe.present);
                (ShownPath::new(shown), present)
            });
            Some(ProbePath { source, path })
        })
        .collect()
}

/// The path a probe row shows: the first one there, as a click tries every one,
/// else the first.
fn shown_path(
    paths: &[std::path::PathBuf],
    present: fn(&std::path::Path) -> bool,
) -> (std::path::PathBuf, bool) {
    match paths.iter().find(|p| present(p)) {
        Some(found) => (found.clone(), true),
        None => (paths.first().cloned().unwrap_or_default(), false),
    }
}

/// All probing, no formatting. `log_at` is injected by `main` (resolved by
/// [`LogLocation::from_env`](crate::run_log::LogLocation::from_env));
/// `look` is the `--look` and `--protocol` flags.
fn collect(
    log_at: &crate::run_log::LogLocation,
    look: crate::graphics::LookChoice,
    delivery: impl FnOnce(&[DoctorSourceRow]) -> anyhow::Result<delivery::Delivery>,
) -> anyhow::Result<DoctorReport> {
    let mut config_warnings = Vec::new();
    let config_path = crate::config::config_path();
    let cfg = crate::config::load(&config_path, &mut config_warnings);
    // What the disk says, which a running office's gate reflects only from its
    // next launch: `delivery` compares the two.
    let connected = crate::sources::connected(&cfg);
    let (log, log_warning) = log_at.read();

    let term_env = pixtuoid_core::platform::text_env("TERM");
    let colorterm_env = pixtuoid_core::platform::text_env("COLORTERM");
    let clicolor_force = pixtuoid_core::platform::text_env("CLICOLOR_FORCE");
    let color_pf = crate::term::color_preflight(
        pixtuoid_core::platform::text_env("NO_COLOR").as_deref(),
        clicolor_force.as_deref(),
        term_env.as_deref(),
    );
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let probe_ok = tty && color_pf != crate::term::ColorPreflight::RefuseDumbTerm;
    let color = report_color(tty, color_pf, clicolor_force.as_deref());
    if color {
        // crossterm strips color under `$NO_COLOR` unless forced; pin it rather
        // than rely on the Display path happening not to check today.
        crossterm::style::force_color_output(true);
    }
    let max_density = pixtuoid_scene::pack::load_bundled_pack()?.max_density_variant();
    let (truecolor_probe, graphics_plan) = probe_terminal_caps(probe_ok, look, max_density);
    let run_look = crate::graphics::LookChoice::new(
        crate::config::resolve_look(&cfg, None, &mut config_warnings),
        None,
    );
    let run_plan = graphics_plan.cutaway().map(|c| run_look.over_here(c));

    let (mut rows, probes): (Vec<DoctorSourceRow>, Vec<_>) = registry::REGISTRY
        .iter()
        .map(|desc| {
            let src = desc.name;
            let target = crate::install::target::by_source(src);
            let hooks_installed = target
                .map(|t| crate::install::has_hooks(t, None))
                .unwrap_or(false);
            let is_connected = connected.contains(src);
            let cli_detected = target.map(|t| (t.presence_probe)());
            let row = DoctorSourceRow {
                desc,
                connected: is_connected,
                has_target: target.is_some(),
                hooks_installed,
                installed_version: InstalledVersion::Unasked,
                diag: diagnose(src, &log, None),
            };
            let probe = desc
                .version_probe
                .filter(|_| may_probe_version(is_connected, cli_detected));
            (row, probe)
        })
        .unzip();
    for (row, version) in rows.iter_mut().zip(probe_versions(&probes, EXIT_DEADLINE)?) {
        row.installed_version = version;
    }
    let delivery = delivery(&rows)?;

    let roots = collect_roots();

    // Read as BYTES, lossy ONLY here: the advisory renders these for a human and never
    // opens them, so this is the one boundary where losing an ill-formed byte costs nothing.
    let (home, up) = (
        pixtuoid_core::platform::path_env("HOME"),
        pixtuoid_core::platform::path_env("USERPROFILE"),
    );
    let home_split = home_split_advisory(
        cfg!(windows),
        home.as_ref().map(|p| p.to_string_lossy()).as_deref(),
        up.as_ref().map(|p| p.to_string_lossy()).as_deref(),
    );

    let (backend, backend_healthy) = activation_backend();
    Ok(DoctorReport {
        log_path: ShownPath::new(log_at.path()),
        config_path: ShownPath::new(config_path),
        config_warnings,
        log_warning,
        term_env,
        colorterm_env,
        truecolor_probe,
        color_pf,
        graphics_plan,
        run_plan,
        rows,
        delivery,
        roots,
        backend,
        backend_healthy,
        probes: probe_paths(),
        home_split,
        color,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CategoryStatus {
    Ok,
    Warn,
    /// Reserved for an actionable install break; advisories are `Warn`.
    Broken,
}

/// One report category: `[glyph] name — summary`, plus indented detail rows.
struct Category {
    status: CategoryStatus,
    name: &'static str,
    summary: String,
    details: Vec<String>,
}

/// Detail rows sit one level under the `[x]` line; `↳` continuations one
/// further. Shared so the report can't go ragged one site at a time.
const DETAIL_INDENT: &str = "      ";
const CONT_INDENT: &str = "          ";
/// The column a source's registry name fills.
const SOURCE_W: usize = 13;

fn terminal_category(r: &DoctorReport) -> Category {
    let verdict = crate::term::truecolor_verdict(r.colorterm_env.as_deref(), r.truecolor_probe);
    let refused = matches!(
        r.color_pf,
        crate::term::ColorPreflight::RefuseNoColor | crate::term::ColorPreflight::RefuseDumbTerm
    );
    // The graphics row stays whole — its tail names WHY a profile fell back to
    // classic, and a fallback must never go unexplained.
    let mut details = vec![format!(
        "{DETAIL_INDENT}{}",
        r.graphics_plan.diagnostic_row(r.run_plan)
    )];
    // Whenever it has something to say — incl. the ForceColor note, so a
    // NO_COLOR+CLICOLOR_FORCE report still states that color is being forced.
    if let Some(row) = crate::term::color_status_row(r.color_pf) {
        details.push(format!("{DETAIL_INDENT}{row}"));
    }
    // Same predicate as the launcher's warning, so the two never disagree; a skipped
    // probe (piped) stays ✓.
    let unconfirmed = r
        .truecolor_probe
        .is_some_and(crate::term::Truecolor::warrants_warning)
        && !verdict.starts_with("yes");
    let status = if refused || unconfirmed {
        CategoryStatus::Warn
    } else {
        CategoryStatus::Ok
    };
    Category {
        status,
        name: "terminal",
        summary: format!(
            "TERM={} \u{b7} COLORTERM={} \u{b7} truecolor {verdict}",
            crate::term::shown_env(r.term_env.as_deref()),
            crate::term::shown_env(r.colorterm_env.as_deref()),
        ),
        details,
    }
}

fn config_category(r: &DoctorReport) -> Option<Category> {
    if r.config_warnings.is_empty() {
        return None;
    }
    let n = r.config_warnings.len();
    Some(Category {
        status: CategoryStatus::Warn,
        name: "config",
        summary: format!("{n} warning{} loading {}", plural_s(n), r.config_path),
        details: r
            .config_warnings
            .iter()
            .map(|w| format!("{DETAIL_INDENT}{}", sanitize(w)))
            .collect(),
    })
}

/// Every source row rides under the category line; the verdict lives in the
/// category glyph + summary, so a glance answers "anything wrong?" and the rows
/// answer "with what?".
fn sources_category(rows: &[DoctorSourceRow], ink: &Ink) -> Category {
    let broken = rows.iter().filter(|r| r.diag.is_broken()).count();
    let mut details: Vec<String> = rows.iter().map(|r| format_doctor_row(r, ink)).collect();
    if broken == 0 {
        return Category {
            status: CategoryStatus::Ok,
            name: "sources",
            summary: format!("{} registered · connected installs sound", rows.len()),
            details,
        };
    }
    details.push(ink.hint(&format!(
        "{DETAIL_INDENT}→ fix: reconnect in the Sources panel (press s)"
    )));
    let verb = if broken == 1 { "needs" } else { "need" };
    Category {
        status: CategoryStatus::Broken,
        name: "sources",
        summary: format!("{broken} of {} {verb} attention", rows.len()),
        details,
    }
}

/// Whether the installed MAJOR is below the verified one: only the latest
/// release is supported, and a new major is where a wire breaks. A CalVer
/// year is no major.
fn below_verified_major(installed: &semver::Version, verified: &str, hyphen: Hyphen) -> bool {
    registry::parse_version(verified, hyphen)
        .is_some_and(|v| installed.major < v.major && v.major < CALVER_MIN_MAJOR)
}

/// Two skews are worth an alarm: an older major, which is unsupported, and a
/// CLI running AHEAD of the version this build's decoder was verified against
/// (its wire format may have moved). An older minor or patch stays silent —
/// that comparison is derivable, not actionable.
fn versions_category(rows: &[DoctorSourceRow]) -> Category {
    let parsed: Vec<(&DoctorSourceRow, semver::Version)> = rows
        .iter()
        .filter_map(|r| {
            let installed =
                registry::parse_version(r.installed_version.read()?, r.desc.version_hyphen)?;
            Some((r, installed))
        })
        .collect();
    let older: Vec<_> = parsed
        .iter()
        .filter(|(r, v)| below_verified_major(v, r.desc.verified_version, r.desc.version_hyphen))
        .collect();
    let newer: Vec<_> = parsed
        .iter()
        .filter(|(r, v)| {
            version_cmp(v, r.desc.verified_version, r.desc.version_hyphen)
                == Some(std::cmp::Ordering::Greater)
        })
        .collect();
    if older.is_empty() && newer.is_empty() {
        return Category {
            status: CategoryStatus::Ok,
            name: "versions",
            summary: "none newer than verified".to_string(),
            details: Vec::new(),
        };
    }
    let row = |(r, installed): &(&DoctorSourceRow, semver::Version), tail: String| {
        format!(
            "{DETAIL_INDENT}{:<15} {} installed \u{b7} {} verified{tail}",
            format!("{}\u{b7}{}", r.desc.label_prefix, r.desc.name),
            compared_version(installed),
            r.desc.verified_version
        )
    };
    let details = older
        .iter()
        .map(|&r| row(r, format!(" — unsupported, upgrade {}", r.0.desc.name)))
        .chain(newer.iter().map(|&r| row(r, String::new())))
        .collect();
    let clis = |n: usize| format!("{n} {}", if n == 1 { "CLI" } else { "CLIs" });
    let summary: Vec<String> = [
        (!older.is_empty()).then(|| format!("{} unsupported (upgrade needed)", clis(older.len()))),
        (!newer.is_empty())
            .then(|| format!("{} newer than verified (drift possible)", clis(newer.len()))),
    ]
    .into_iter()
    .flatten()
    .collect();
    Category {
        status: CategoryStatus::Warn,
        name: "versions",
        summary: summary.join(" \u{b7} "),
        details,
    }
}

/// No per-source breakdown here — it already rides each drifted source's `↳`
/// row under `sources`; this category adds the verdict and the report-upstream
/// pointer.
fn drift_category(r: &DoctorReport, ink: &Ink) -> Category {
    if let Some(w) = &r.log_warning {
        return Category {
            status: CategoryStatus::Warn,
            name: "decode drift",
            // The minted warning already says the counts are not meaningful.
            summary: w.clone(),
            details: Vec::new(),
        };
    }
    let drifted = r
        .rows
        .iter()
        .filter(|row| row.diag.drift.total() > 0)
        .count();
    if drifted == 0 {
        return Category {
            status: CategoryStatus::Ok,
            name: "decode drift",
            summary: "none recorded".to_string(),
            details: Vec::new(),
        };
    }
    let total: u64 = r.rows.iter().map(|row| row.diag.drift.total()).sum();
    Category {
        status: CategoryStatus::Warn,
        name: "decode drift",
        summary: format!(
            "{total} event{} across {drifted} source{} (see the \u{21b3} rows under sources)",
            plural_s(total as usize),
            plural_s(drifted)
        ),
        details: vec![ink.hint(&format!(
            "{DETAIL_INDENT}→ may predate a CLI's wire format — report: {}/issues",
            pixtuoid_scene::hit::REPO_URL
        ))],
    }
}

/// One row per source with a PRIMARY on-disk root (omp may watch further
/// profile roots this row does not list). A FACT row, not an alarm — a missing
/// root is normal for a CLI the user never runs, so the only alarm is missing
/// WHILE that source's override is set (#880).
fn format_root_row(r: &RootStatus, ink: &Ink) -> String {
    let via = match r.env {
        Some((var, true)) => format!(" (via ${var})"),
        _ => String::new(),
    };
    let mut line = format!("{DETAIL_INDENT}{:<SOURCE_W$} {}{via}", r.source, r.root);
    if !r.exists {
        match r.env {
            Some((var, true)) => {
                line.push_str(&format!(" — {}", ink.warn("missing")));
                line.push_str(&format!(
                    "\n{CONT_INDENT}\u{21b3} ${var} is set but that root does not exist — if this \
                     source's sprite never appears, that env var is the first thing to check"
                ));
            }
            _ => line.push_str(&format!(" — {}", ink.dim("missing"))),
        }
    }
    line
}

fn roots_category(roots: &[RootStatus], ink: &Ink) -> Option<Category> {
    if roots.is_empty() {
        return None;
    }
    let present = roots.iter().filter(|r| r.exists).count();
    let alarmed = roots
        .iter()
        .any(|r| !r.exists && matches!(r.env, Some((_, true))));
    let summary = if present == roots.len() {
        format!("{present} resolved, all present")
    } else {
        format!(
            "{present} of {} present (a missing root is normal for a CLI never run)",
            roots.len()
        )
    };
    Some(Category {
        status: if alarmed {
            CategoryStatus::Warn
        } else {
            CategoryStatus::Ok
        },
        name: "roots",
        summary,
        details: roots.iter().map(|r| format_root_row(r, ink)).collect(),
    })
}

/// Click a sprite → its terminal comes forward. The channel buckets come
/// straight from the registry, so a new source lands in the right bucket with
/// no edit here.
fn focus_category(r: &DoctorReport, ink: &Ink) -> Category {
    use registry::FocusChannel;
    let mut details = Vec::new();
    let mut problem = !r.backend_healthy;
    for p in &r.probes {
        // Behind a stamp, the probe only covers the shapes the stamp misses.
        let fallback = p.source.focus_channel() != FocusChannel::TranscriptProbe;
        let row = format!(
            "{DETAIL_INDENT}{}\u{b7}{} — pid probe{}",
            p.source.label_prefix,
            p.source.name,
            if fallback {
                " (stamp-less fallback)"
            } else {
                ""
            }
        );
        match &p.path {
            Ok((path, true)) => details.push(format!("{row} {} {path}", ink.ok("\u{2713}"))),
            Ok((path, false)) if fallback => {
                details.push(format!("{row} {} {path}", ink.warn("\u{2717} missing")));
            }
            Ok((path, false)) => {
                problem = true;
                details.push(format!(
                    "{row} {} {path} (focus no-ops until {} writes it)",
                    ink.bad("\u{2717} missing"),
                    p.source.name
                ));
            }
            Err(why) => {
                problem |= !fallback;
                details.push(format!("{row} disabled ({why}) — focus no-ops"));
            }
        }
    }
    let mut shim_stamp = Vec::new();
    let mut plugin_stamp = Vec::new();
    let mut no_channel = Vec::new();
    for d in registry::REGISTRY {
        // Daemons aren't click-focusable agents; the TranscriptProbe sources
        // got their probe rows above (omp appears in BOTH: its probe row is
        // the stamp-less fallback, this census line the primary channel).
        if d.is_daemon() || d.focus_channel() == FocusChannel::TranscriptProbe {
            continue;
        }
        let tag = format!("{}\u{b7}{}", d.label_prefix, d.name);
        match d.focus_channel() {
            FocusChannel::ShimStamp(_) => shim_stamp.push(tag),
            FocusChannel::PluginStamp => plugin_stamp.push(tag),
            FocusChannel::Unsupported | FocusChannel::ServerStamp => no_channel.push(tag),
            FocusChannel::TranscriptProbe => unreachable!("skipped above"),
        }
    }
    if !shim_stamp.is_empty() {
        details.push(format!(
            "{DETAIL_INDENT}{} — shim-stamped `_pid`",
            shim_stamp.join(" ")
        ));
    }
    if !plugin_stamp.is_empty() {
        details.push(format!(
            "{DETAIL_INDENT}{} — plugin-stamped `_pid`",
            plugin_stamp.join(" ")
        ));
    }
    if !no_channel.is_empty() {
        details.push(format!(
            "{DETAIL_INDENT}{} — no focus channel (click no-ops)",
            ink.dim(&no_channel.join(" "))
        ));
    }
    Category {
        status: if problem {
            CategoryStatus::Warn
        } else {
            CategoryStatus::Ok
        },
        name: "focus-jump",
        summary: r.backend.to_string(),
        details,
    }
}

fn home_split_category(r: &DoctorReport) -> Option<Category> {
    r.home_split.as_ref().map(|adv| Category {
        status: CategoryStatus::Warn,
        // Not "windows": it renders only ON Windows, where that distinguishes nothing.
        name: "home",
        summary: "HOME and USERPROFILE point at different homes".to_string(),
        details: vec![format!("{DETAIL_INDENT}{adv}")],
    })
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The ONE report: flutter-doctor-style categories — a `[✓]`/`[!]`/`[✗]`
/// verdict line per category, every probed fact riding as an indented detail
/// row under its category, an issues-found footer.
fn render(r: &DoctorReport) -> String {
    let ink = Ink { on: r.color };
    let mut cats: Vec<Category> = vec![terminal_category(r)];
    cats.extend(config_category(r));
    cats.push(sources_category(&r.rows, &ink));
    cats.push(delivery::hooks_category(&r.delivery, &r.rows, &ink));
    cats.push(versions_category(&r.rows));
    cats.push(drift_category(r, &ink));
    cats.extend(roots_category(&r.roots, &ink));
    cats.push(focus_category(r, &ink));
    cats.extend(home_split_category(r));

    let mut out = String::from("pixtuoid doctor\n");
    out.push_str(&ink.dim(&format!("log    {}", r.log_path)));
    out.push('\n');
    out.push_str(&ink.dim(&format!("config {}", r.config_path)));
    out.push('\n');
    out.push('\n');
    for (i, c) in cats.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let glyph = match c.status {
            CategoryStatus::Ok => ink.ok("\u{2713}"),
            CategoryStatus::Warn => ink.warn("!"),
            CategoryStatus::Broken => ink.bad("\u{2717}"),
        };
        out.push_str(&format!("[{glyph}] {} — {}\n", ink.bold(c.name), c.summary));
        for d in &c.details {
            out.push_str(d);
            out.push('\n');
        }
    }
    let issues = cats
        .iter()
        .filter(|c| c.status != CategoryStatus::Ok)
        .count();
    out.push('\n');
    if issues == 0 {
        out.push_str(&format!("{} no issues found\n", ink.ok("\u{2022}")));
    } else {
        let broken_any = cats.iter().any(|c| c.status == CategoryStatus::Broken);
        let glyph = if broken_any {
            ink.bad("!")
        } else {
            ink.warn("!")
        };
        let noun = if issues == 1 {
            "category"
        } else {
            "categories"
        };
        out.push_str(&format!("{glyph} issues in {issues} {noun}\n"));
    }
    out
}

/// Returns the rendered report rather than printing it, so the WHOLE report
/// builder is unit-testable.
///
/// # Errors
///
/// If the bundled pack does not load, or no tokio runtime can be built.
pub(crate) fn run(
    log_at: &crate::run_log::LogLocation,
    look: crate::graphics::LookChoice,
) -> anyhow::Result<String> {
    Ok(render(&collect(log_at, look, delivery::collect)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::read_log;
    use crate::test_capture::capture;
    #[cfg(unix)]
    use std::time::Duration;

    /// The plain-text plumbing: `Ink { on: false }` must be a byte-for-byte
    /// no-op (piped output stays clean), `on: true` must actually wrap in
    /// escapes — proving the color path CAN fire, not just that it parses.
    #[test]
    fn ink_paints_only_when_on() {
        let off = Ink { on: false };
        assert_eq!(off.ok("✓"), "✓");
        assert_eq!(off.bad("✗"), "✗");
        assert_eq!(off.bold("sources"), "sources");
        let on = Ink { on: true };
        for painted in [
            on.ok("✓"),
            on.warn("!"),
            on.bad("✗"),
            on.hint("→"),
            on.dim("–"),
            on.bold("sources"),
        ] {
            assert!(painted.contains('\u{1b}'), "carries an escape: {painted:?}");
        }
        assert!(on.ok("✓").contains('✓'), "the glyph survives painting");
    }

    #[test]
    fn linux_activation_backend_covers_every_channel_in_priority_order() {
        let healthy = |s: bool, h: bool, w: bool, x: bool| linux_activation_backend(s, h, w, x);
        assert!(healthy(true, true, true, true).0.contains("sway"));
        assert!(healthy(true, true, true, true).1);
        assert!(healthy(false, true, true, true).0.contains("hyprland"));
        assert!(healthy(false, true, true, true).1);
        let wayland = healthy(false, false, true, true);
        assert!(wayland.0.contains("Wayland") && !wayland.1, "{wayland:?}");
        assert!(healthy(false, false, false, true).0.contains("EWMH"));
        assert!(healthy(false, false, false, true).1);
        let none = healthy(false, false, false, false);
        assert!(none.0.contains("none") && !none.1, "{none:?}");
    }

    #[test]
    fn an_exported_but_blank_compositor_marker_is_not_a_running_compositor() {
        const KEY: &str = "PIXTUOID_TEST_DOCTOR_MARKER";
        temp_env::with_var_unset(KEY, || assert!(!marker_set(KEY)));
        for blank in ["", "  \t "] {
            temp_env::with_var(KEY, Some(blank), || {
                assert!(!marker_set(KEY), "SWAYSOCK={blank:?} is a leftover");
            });
        }
        temp_env::with_var(KEY, Some("/run/user/1000/sway-ipc.sock"), || {
            assert!(marker_set(KEY));
        });
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let path = std::ffi::OsString::from_vec(b"/run/user/1000/caf\xFF.sock".to_vec());
            temp_env::with_var(KEY, Some(&path), || {
                assert!(marker_set(KEY), "a non-UTF-8 socket path is still set");
            });
        }
    }

    #[test]
    fn focus_category_buckets_sources_from_the_registry() {
        let mut r = summary_report(vec![]);
        r.backend = "test-backend";
        let c = focus_category(&r, &Ink { on: false });
        assert_eq!(c.status, CategoryStatus::Ok);
        assert_eq!(c.summary, "test-backend");
        let s = c.details.join("\n");
        assert!(s.contains("claude-code — pid probe ✓"), "{s}");
        assert!(s.contains("codex — pid probe ✓"), "{s}");
        assert!(
            s.contains("dsh") && s.contains("plugin-stamped"),
            "plugin stampers listed separately: {s}"
        );
        assert!(
            s.contains("om\u{b7}omp — pid probe (stamp-less fallback)"),
            "omp keeps its fallback-probe row beside the stamp census: {s}"
        );
        assert!(
            s.contains("cursor") && s.contains("shim-stamped `_pid`"),
            "shim stampers listed separately: {s}"
        );
        let no_channel_line = c
            .details
            .iter()
            .find(|l| l.contains("no focus channel"))
            .expect("a no-channel line");
        assert!(
            no_channel_line.contains("opencode"),
            "a server-stamped `_pid` owns no window: {no_channel_line}"
        );
        assert!(
            no_channel_line.contains("copilot") && !no_channel_line.contains("antigravity"),
            "a transcript-only source listed, a shim stamper not: {no_channel_line}"
        );
        assert!(!s.contains("openclaw"), "daemons are excluded: {s}");
    }

    #[test]
    fn focus_category_warns_on_missing_and_disabled_probe_roots() {
        let mut r = summary_report(vec![]);
        set_probe(
            &mut r,
            "claude-code",
            Ok((ShownPath::new("/home/u/.claude/sessions"), false)),
        );
        set_probe(
            &mut r,
            "codex",
            Ok((ShownPath::new("/home/u/.codex/sessions"), false)),
        );
        let c = focus_category(&r, &Ink { on: false });
        assert_eq!(c.status, CategoryStatus::Warn);
        let s = c.details.join("\n");
        assert!(
            s.contains("✗ missing") && s.contains("focus no-ops until claude-code writes it"),
            "{s}"
        );
        assert!(s.contains("focus no-ops until codex writes it"), "{s}");

        set_probe(&mut r, "claude-code", Err("non-standard projects root"));
        set_probe(
            &mut r,
            "codex",
            Ok((ShownPath::new("/home/u/.codex/sessions"), true)),
        );
        let c = focus_category(&r, &Ink { on: false });
        assert_eq!(c.status, CategoryStatus::Warn);
        assert!(
            c.details
                .iter()
                .any(|l| l.contains("pid probe disabled (non-standard projects root)")),
            "{:?}",
            c.details
        );
    }

    /// Every focusable (non-daemon) registered source must appear SOMEWHERE in
    /// the category: the census skips `TranscriptProbe`, leaving such a source to
    /// its probe row, and omp vanished the moment it left `Unsupported`.
    #[test]
    fn every_focusable_source_appears_in_the_focus_category() {
        let r = summary_report(vec![]);
        let s = focus_category(&r, &Ink { on: false }).details.join("\n");
        for d in registry::REGISTRY {
            if d.is_daemon() {
                continue;
            }
            let tag = format!("{}\u{b7}{}", d.label_prefix, d.name);
            assert!(
                s.contains(&tag),
                "{tag} is missing from the focus category:\n{s}"
            );
        }
    }

    fn registered(id: &str) -> &'static registry::SourceDescriptor {
        registry::descriptor_for(id).expect("a registered source")
    }

    /// A version `skew` away from the source's verified pin.
    fn skewed(id: &str, skew: impl FnOnce(&mut semver::Version)) -> String {
        let desc = registered(id);
        let mut v = registry::parse_version(desc.verified_version, desc.version_hyphen)
            .expect("the pin parses");
        skew(&mut v);
        v.to_string()
    }

    /// The pin's own major at its lowest precedence: `x.0.0-0` sorts below every
    /// `x.y.z` release.
    fn lowest_of_major(v: &mut semver::Version) {
        v.minor = 0;
        v.patch = 0;
        v.pre = semver::Prerelease::new("0").unwrap();
    }

    /// Installed AT the verified pin, so the row raises no version skew.
    fn summary_row(id: &str) -> DoctorSourceRow {
        let desc = registered(id);
        DoctorSourceRow {
            desc,
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: InstalledVersion::Read(desc.verified_version.into()),
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult::default()),
                drift: LogScanResult::default(),
            },
        }
    }

    #[test]
    fn a_probe_row_shows_the_first_path_a_click_would_find() {
        let home = tempfile::tempdir().unwrap();
        let default = home.path().join("default");
        let profile = home.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        assert_eq!(
            shown_path(&[default.clone(), profile.clone()], std::path::Path::is_dir),
            (profile, true),
            "a profile's dir alone is found, as the click finds it"
        );
        assert_eq!(
            shown_path(
                &[default.clone(), home.path().join("gone")],
                std::path::Path::is_dir
            ),
            (default, false),
            "none there: the first is the one reported missing"
        );
    }

    fn set_probe(
        r: &mut DoctorReport,
        source: &str,
        path: Result<(ShownPath, bool), &'static str>,
    ) {
        r.probes
            .iter_mut()
            .find(|p| p.source.name == source)
            .expect("a probe-backed source")
            .path = path;
    }

    fn summary_report(rows: Vec<DoctorSourceRow>) -> DoctorReport {
        DoctorReport {
            log_path: ShownPath::new("/tmp/log"),
            config_path: ShownPath::new("/tmp/config.toml"),
            config_warnings: vec![],
            log_warning: None,
            term_env: Some("xterm".into()),
            colorterm_env: None,
            truecolor_probe: None,
            color_pf: crate::term::ColorPreflight::Proceed,
            graphics_plan: crate::graphics::Plan::Classic {
                reason: crate::graphics::ClassicReason::NotQueried,
            },
            run_plan: None,
            rows,
            delivery: delivery::Delivery::idle(),
            roots: vec![],
            backend: "NSRunningApplication (macOS)",
            backend_healthy: true,
            probes: registry::REGISTRY
                .iter()
                .filter(|source| source.focus_probe().is_some())
                .map(|source| ProbePath {
                    source,
                    path: Ok((ShownPath::new(format!("/tmp/{}", source.name)), true)),
                })
                .collect(),
            home_split: None,
            color: false,
        }
    }

    #[test]
    fn no_probed_text_reaches_the_terminal_raw() {
        const EVIL: &str = "\u{1b}]0;pwned\u{7}\u{202e}";
        let evil = || format!("/tmp/{EVIL}");
        // Fields with a minting fn are poisoned through it, so the test covers
        // the mint as well as the render; the rest are poisoned raw and pin the
        // render-side strip.
        let mut row = summary_row("claude-code");
        row.installed_version = InstalledVersion::Read(
            first_sanitized_line(format!("1.0.0 {EVIL}").as_bytes()).expect("a line"),
        );
        row.diag.drift = scan_log_for_source(
            &format!(
                "{EVIL}ts  WARN {}: source=claude-code kind=unknown_event name={EVIL}Hook",
                drift::TARGET
            ),
            "claude-code",
        );
        assert!(row.diag.drift.last_ts.is_some(), "the line must parse");
        let mut issues = Vec::new();
        crate::install::check_shim_binary(std::path::Path::new(&evil()), &mut issues);
        row.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            issues,
            ..Default::default()
        });
        let mut noted = summary_row("codex");
        noted.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            notes: vec![evil()],
            ..Default::default()
        });
        let mut r = summary_report(vec![row, noted]);
        r.delivery = delivery::Delivery::hostile(&evil());
        r.log_path = ShownPath::new(evil());
        r.config_path = ShownPath::new(evil());
        r.config_warnings = vec![evil()];
        // Through `read_log`, which mints it: a directory is no readable log.
        // Windows forbids the Cc half in a file name, as
        // `the_unreadable_log_warning_is_stripped_where_it_is_minted` notes.
        let dir = tempfile::tempdir().unwrap();
        let unreadable = if cfg!(windows) {
            dir.path().join("log\u{202e}")
        } else {
            dir.path().join(format!("log{EVIL}"))
        };
        std::fs::create_dir(&unreadable).unwrap();
        r.log_warning = read_log(&unreadable).1;
        assert!(
            r.log_warning.is_some(),
            "a directory must not read as a log"
        );
        r.term_env = Some(evil());
        r.colorterm_env = Some(evil());
        set_probe(&mut r, "claude-code", Ok((ShownPath::new(evil()), false)));
        set_probe(&mut r, "codex", Ok((ShownPath::new(evil()), false)));
        set_probe(&mut r, "omp", Ok((ShownPath::new(evil()), false)));
        set_probe(&mut r, "grok", Ok((ShownPath::new(evil()), false)));
        r.home_split = home_split_advisory(true, Some(&evil()), Some(r"C:\Users\me"));
        r.roots = vec![RootStatus {
            source: "claude-code",
            root: ShownPath::new(evil()),
            exists: false,
            env: None,
        }];
        let out = render(&r);
        for line in out.lines() {
            assert_eq!(sanitize(line), line, "raw control text in:\n{out}");
        }
    }

    /// The color policy is a pure fn precisely so these arms are testable —
    /// force-when-piped and dumb-beats-force both survived mutation before the
    /// extraction.
    #[test]
    fn report_color_truth_table() {
        use crate::term::ColorPreflight as Pf;
        assert!(report_color(true, Pf::Proceed, None), "tty + Proceed → on");
        assert!(
            !report_color(false, Pf::Proceed, None),
            "piped + Proceed → off"
        );
        assert!(
            report_color(false, Pf::Proceed, Some("1")),
            "CLICOLOR_FORCE forces even piped"
        );
        assert!(
            !report_color(false, Pf::Proceed, Some("0")),
            "CLICOLOR_FORCE=0 does not force"
        );
        assert!(
            !report_color(false, Pf::Proceed, Some("  ")),
            "a whitespace CLICOLOR_FORCE does not force"
        );
        assert!(
            !report_color(true, Pf::RefuseNoColor, None),
            "NO_COLOR refuses"
        );
        assert!(
            report_color(false, Pf::ForceColor, Some("1")),
            "the force overrides NO_COLOR (pf is already ForceColor)"
        );
        assert!(
            !report_color(true, Pf::RefuseDumbTerm, Some("1")),
            "TERM=dumb outranks everything, force included"
        );
    }

    #[test]
    fn terminal_category_warns_when_an_attempted_probe_did_not_confirm() {
        let mut r = summary_report(vec![]);
        use crate::term::Truecolor;
        // Asked, no answer — the launcher warned here.
        r.truecolor_probe = Some(Truecolor::NoAnswer);
        let c = terminal_category(&r);
        assert_eq!(c.status, CategoryStatus::Warn);
        assert!(c.summary.contains("did not answer"), "{}", c.summary);

        r.truecolor_probe = Some(Truecolor::Answered(false));
        assert_eq!(terminal_category(&r).status, CategoryStatus::Warn);

        r.truecolor_probe = Some(Truecolor::Answered(true));
        assert_eq!(terminal_category(&r).status, CategoryStatus::Ok);

        // Piped, or nothing could ask: no query ran, which is not a warning,
        // and the wording must not claim the terminal went silent.
        for (unasked, says) in [
            (None, "probe skipped"),
            (Some(Truecolor::CantAsk), "can't be asked"),
        ] {
            r.truecolor_probe = unasked;
            let c = terminal_category(&r);
            assert_eq!(c.status, CategoryStatus::Ok, "{unasked:?}");
            assert!(!unasked.is_some_and(Truecolor::warrants_warning));
            assert!(c.summary.contains(says), "{}", c.summary);
        }
    }

    /// The graphics row, as the terminal category prints it: first, whole,
    /// under the category line.
    #[test]
    fn terminal_category_leads_with_the_graphics_plan() {
        use crate::graphics::{CellSize, ClassicReason, ImageProtocol, Plan, cutaway_fit};
        let mut r = summary_report(vec![]);
        let cell = CellSize { w: 17, h: 41 };
        let area = ratatui::layout::Size {
            width: 200,
            height: 49,
        };
        let density = pixtuoid_scene::sprite::format::Density::new(4).expect("nonzero");
        r.graphics_plan = Plan::Cutaway(crate::graphics::CutawayPlan {
            fit: cutaway_fit(cell, area, density).expect("fits"),
            route: crate::graphics::Route::direct(ImageProtocol::Kitty, false),
            cell,
            chosen: crate::graphics::Chosen::Answer,
        });
        r.run_plan = Some(r.graphics_plan);
        assert_eq!(
            terminal_category(&r).details[0],
            format!(
                "{DETAIL_INDENT}{}",
                r.graphics_plan.diagnostic_row(r.run_plan)
            )
        );
        r.run_plan = Some(Plan::Classic {
            reason: ClassicReason::OverSsh,
        });
        let row = terminal_category(&r).details.swap_remove(0);
        assert_eq!(
            row,
            format!(
                "{DETAIL_INDENT}{}",
                r.graphics_plan.diagnostic_row(r.run_plan)
            ),
            "the row reads how run plans"
        );
        assert!(row.contains("`run` paints classic here: over ssh"), "{row}");

        r.graphics_plan = Plan::Classic {
            reason: ClassicReason::TmuxNeedsKitty(ImageProtocol::Sixel),
        };
        assert_eq!(
            terminal_category(&r).details[0],
            format!(
                "{DETAIL_INDENT}graphics: classic half-blocks — inside tmux only kitty \
                 graphics survive a pane switch here, and this terminal speaks sixel"
            )
        );
    }

    #[test]
    fn focus_category_warns_on_an_unhealthy_backend() {
        let mut r = summary_report(vec![]);
        r.backend = "Wayland compositor without a pid-addressable focus channel";
        r.backend_healthy = false;
        let c = focus_category(&r, &Ink { on: false });
        assert_eq!(c.status, CategoryStatus::Warn);
        assert!(c.summary.contains("Wayland"), "{}", c.summary);
    }

    #[test]
    fn report_clean_state_is_all_check_categories() {
        let out = render(&summary_report(vec![
            summary_row("claude-code"),
            summary_row("codex"),
        ]));
        assert!(out.starts_with("pixtuoid doctor\n"), "{out}");
        assert!(
            out.contains("[✓] sources — 2 registered · connected installs sound"),
            "{out}"
        );
        assert!(
            out.contains("[✓] versions — none newer than verified"),
            "{out}"
        );
        assert!(out.contains("[✓] decode drift — none recorded"), "{out}");
        // The redundant trailing ✓ is stripped — the category glyph says it.
        assert!(
            out.contains("[✓] focus-jump — NSRunningApplication (macOS)\n"),
            "{out}"
        );
        assert!(out.ends_with("• no issues found\n"), "{out}");
    }

    #[test]
    fn report_rows_ride_under_the_sources_category() {
        let out = render(&summary_report(vec![
            summary_row("claude-code"),
            summary_row("codex"),
        ]));
        assert!(
            out.contains(&format!(
                "\n      ✓ cc\u{b7}claude-code   connected    installed       {}\n",
                registered("claude-code").verified_version
            )),
            "every source keeps its row, healthy or not: {out}"
        );
    }

    #[test]
    fn report_broken_install_expands_with_fix_line() {
        let mut broken = summary_row("codex");
        broken.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            issues: vec!["missing hook entries for: SessionEnd".into()],
            notes: vec![],
            ..Default::default()
        });
        let out = render(&summary_report(vec![summary_row("claude-code"), broken]));
        assert!(
            out.contains("[✗] sources — 1 of 2 needs attention"),
            "{out}"
        );
        assert!(
            out.contains("      ✗ cx\u{b7}codex"),
            "the broken row leads with ✗: {out}"
        );
        assert!(
            out.contains("\n          ↳ install broken: missing hook entries for: SessionEnd"),
            "{out}"
        );
        assert!(
            out.contains("      → fix: reconnect in the Sources panel (press s)"),
            "{out}"
        );
        assert!(out.ends_with("! issues in 1 category\n"), "{out}");
    }

    #[test]
    fn report_versions_category_lists_only_newer_than_verified() {
        let pin = registered("copilot").verified_version;
        let newer_v = skewed("copilot", |v| v.patch += 1);
        let mut newer = summary_row("copilot");
        newer.installed_version = InstalledVersion::Read(format!("GitHub Copilot CLI {newer_v}."));
        let older_v = skewed("hermes", lowest_of_major);
        let mut older = summary_row("hermes");
        older.installed_version = InstalledVersion::Read(format!("v{older_v}"));
        let out = render(&summary_report(vec![newer, older]));
        assert!(
            out.contains("[!] versions — 1 CLI newer than verified (drift possible)"),
            "older-than-verified is not an alarm: {out}"
        );
        assert!(
            out.contains(&format!(
                "      cp\u{b7}copilot      {newer_v} installed \u{b7} {pin} verified"
            )),
            "the PARSED pair that was compared, not the raw banner: {out}"
        );
        assert!(
            !out.contains(&format!(
                "{} verified",
                registered("hermes").verified_version
            )),
            "older-than-verified grows no versions detail row: {out}"
        );
        // The raw banner stays on the source row, without skew prose.
        assert!(
            out.contains(&format!("GitHub Copilot CLI {newer_v}.")),
            "{out}"
        );
        assert!(!out.contains("NEWER than verified"), "{out}");
    }

    #[test]
    fn report_versions_category_flags_an_older_major_as_unsupported() {
        let v1_v = skewed("opencode", |v| v.major -= 1);
        let mut v1 = summary_row("opencode");
        v1.installed_version = InstalledVersion::Read(v1_v.clone());
        let mut older_minor = summary_row("copilot");
        older_minor.installed_version = InstalledVersion::Read(skewed("copilot", lowest_of_major));
        let mut calver = summary_row("openclaw");
        calver.installed_version = InstalledVersion::Read("OpenClaw 2025.12.1".into());
        let out = render(&summary_report(vec![v1, older_minor, calver]));
        assert!(
            out.contains("[!] versions — 1 CLI unsupported (upgrade needed)\n"),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "      oc\u{b7}opencode     {v1_v} installed \u{b7} {} verified — \
                 unsupported, upgrade opencode",
                registered("opencode").verified_version
            )),
            "{out}"
        );
        for id in ["copilot", "openclaw"] {
            assert!(
                !out.contains(&format!("{} verified", registered(id).verified_version)),
                "an older minor, or a CalVer year, is no older major: {out}"
            );
        }
    }

    #[test]
    fn report_drift_verdict_line_points_at_rows_and_upstream() {
        let mut drifted = summary_row("grok");
        drifted.diag.drift = LogScanResult {
            unknown_event: 2,
            missing_field: 1,
            ..Default::default()
        };
        let out = render(&summary_report(vec![drifted]));
        assert!(
            out.contains(
                "[!] decode drift — 3 events across 1 source (see the \u{21b3} rows under sources)\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("\n          ↳ decode drift: 2 unknown-event, 1 missing-field"),
            "the breakdown rides the source row: {out}"
        );
        assert!(
            out.contains(&format!("report: {}/issues", pixtuoid_scene::hit::REPO_URL)),
            "{out}"
        );
    }

    #[test]
    fn report_unreadable_log_surfaces_the_warning_not_a_clean_verdict() {
        let mut r = summary_report(vec![summary_row("claude-code")]);
        r.log_warning =
            Some("log unreadable: /x (denied) — the decode-drift counts are not meaningful".into());
        let out = render(&r);
        assert!(
            out.contains("[!] decode drift — log unreadable: /x"),
            "{out}"
        );
        assert!(!out.contains("none recorded"), "{out}");
    }

    #[test]
    fn report_roots_list_every_row_and_alarm_only_on_env_set_missing() {
        let mut r = summary_report(vec![summary_row("claude-code")]);
        r.roots = vec![
            RootStatus {
                source: "claude-code",
                root: ShownPath::new("/home/u/.claude/projects"),
                exists: true,
                env: None,
            },
            RootStatus {
                source: "hermes",
                root: ShownPath::new("/home/u/.hermes"),
                exists: false,
                env: Some(("HERMES_HOME", true)),
            },
        ];
        let out = render(&r);
        assert!(out.contains("[!] roots — 1 of 2 present"), "{out}");
        assert!(
            out.contains("      claude-code   /home/u/.claude/projects\n"),
            "a present root is a bare fact row: {out}"
        );
        assert!(
            out.contains("      hermes        /home/u/.hermes (via $HERMES_HOME) — missing"),
            "{out}"
        );
        assert!(
            out.contains("\n          ↳ $HERMES_HOME is set but that root does not exist"),
            "{out}"
        );

        // The same missing root WITHOUT an override is the ordinary
        // never-ran-that-CLI state — a fact, not an alarm.
        r.roots[1].env = None;
        let out = render(&r);
        assert!(out.contains("[✓] roots — 1 of 2 present"), "{out}");
        assert!(
            out.contains("      hermes        /home/u/.hermes — missing"),
            "{out}"
        );
        assert!(!out.contains("↳ $HERMES_HOME"), "{out}");
    }

    #[test]
    fn report_focus_probe_failure_warns_with_detail() {
        let mut r = summary_report(vec![summary_row("claude-code")]);
        set_probe(
            &mut r,
            "claude-code",
            Ok((ShownPath::new("/tmp/reg"), false)),
        );
        let out = render(&r);
        assert!(
            out.contains("[!] focus-jump — NSRunningApplication (macOS)\n"),
            "{out}"
        );
        assert!(
            out.contains("claude-code — pid probe ✗ missing /tmp/reg"),
            "{out}"
        );
    }

    #[test]
    fn report_footer_counts_issue_categories() {
        let mut broken = summary_row("codex");
        broken.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            issues: vec!["shim binary missing: /old".into()],
            notes: vec![],
            ..Default::default()
        });
        let mut newer = summary_row("copilot");
        newer.installed_version = InstalledVersion::Read(skewed("copilot", |v| v.major += 1));
        let out = render(&summary_report(vec![broken, newer]));
        assert!(out.ends_with("! issues in 2 categories\n"), "{out}");
    }

    #[test]
    fn an_unreadable_log_is_reported_but_a_missing_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_log(&dir.path().join("nope")), (String::new(), None));

        let f = dir.path().join("log");
        std::fs::write(&f, "hello").unwrap();
        assert_eq!(read_log(&f), ("hello".to_string(), None));

        // A directory reads as IsADirectory / PermissionDenied — never NotFound,
        // on any platform and any uid (a chmod-000 file would not bind as root).
        let (text, warning) = read_log(dir.path());
        assert!(text.is_empty());
        let warning = warning.expect("an unreadable log must be reported");
        assert!(
            warning.contains("log unreadable") && warning.contains("not meaningful"),
            "got: {warning}"
        );
    }

    #[test]
    fn the_unreadable_log_warning_is_stripped_where_it_is_minted() {
        let dir = tempfile::tempdir().unwrap();
        // Windows forbids codepoints 1-31 in a filename (Microsoft's "Naming Files, Paths,
        // and Namespaces"), so the Cc half cannot exist in a path there; U+202E can.
        let hostile = if cfg!(windows) {
            dir.path().join("l\u{202e}og")
        } else {
            dir.path().join("l\u{1b}]0;PWNED\u{7}\u{202e}og")
        };
        std::fs::create_dir(&hostile).unwrap(); // a directory never reads as NotFound
        let (_, warning) = read_log(&hostile);
        let warning = warning.expect("an unreadable log must be reported");
        assert!(
            !warning.contains(['\u{1b}', '\u{7}', '\u{202e}']),
            "asserted on `read_log`'s OWN return, not on either presenter — sanitizing per \
             presenter is how one of them shipped raw. This warning carries a live OSC / \
             Trojan-Source override: {warning:?}"
        );
        assert!(warning.contains("log unreadable"), "got: {warning}");
    }

    #[test]
    fn version_probe_is_gated_on_evidence_the_user_runs_that_cli() {
        assert!(may_probe_version(true, Some(false)));
        assert!(may_probe_version(true, None));
        assert!(may_probe_version(false, Some(true)));
        assert!(!may_probe_version(false, Some(false)));
        assert!(!may_probe_version(false, None));
    }

    /// A stand-in CLI at `dir/name` running `body`.
    #[cfg(unix)]
    fn fake_cli(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn a_version_probe_reads_its_cli_or_says_why_not() {
        const SHORT: Duration = Duration::from_millis(100);
        let dir = tempfile::tempdir().unwrap();
        let cli = |name, body| fake_cli(dir.path(), name, body);
        let (read, failed, hung) = (
            cli("read", "echo\necho '  1.2.3  '"),
            cli("failed", "echo 9.9.9; exit 3"),
            cli("hung", "exec sleep 600"),
        );
        let missing = dir.path().join("missing").display().to_string();
        let probes = [
            None,
            Some(vec![read.as_str(), "--version"]),
            Some(vec![failed.as_str(), "--version"]),
            Some(vec![missing.as_str(), "--version"]),
        ];
        let got = probe_versions(&probes, EXIT_DEADLINE).unwrap();
        assert_eq!(got[0], InstalledVersion::Unasked);
        assert_eq!(got[1], InstalledVersion::Read("1.2.3".into()));
        let why = |v: &InstalledVersion| match v {
            InstalledVersion::Unknown(why) => why.clone(),
            other => panic!("{other:?} is no unknown"),
        };
        assert!(why(&got[2]).contains('3'), "{got:?}");
        assert!(why(&got[3]).contains("would not run"), "{got:?}");
        let got = probe_versions(&[Some([hung.as_str()])], SHORT).unwrap();
        assert!(why(&got[0]).contains("did not exit within"), "{got:?}");
    }

    /// A launcher that leaves the hung work to a child of its own: the
    /// deadline's kill takes the child too.
    #[cfg(unix)]
    #[test]
    fn a_hung_probes_kill_takes_what_its_launcher_started() {
        // Long enough for the launcher to start its child under load.
        const SHORT: Duration = Duration::from_secs(2);
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("child.pid");
        let launcher = fake_cli(
            dir.path(),
            "launcher",
            &format!("sleep 600 &\necho $! > '{}'\nwait", pid.display()),
        );
        let got = probe_versions(&[Some([launcher.as_str()])], SHORT).unwrap();
        assert!(
            matches!(&got[0], InstalledVersion::Unknown(why) if why.contains("did not exit within")),
            "{got:?}"
        );
        let pid = std::fs::read_to_string(&pid).unwrap();
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .status()
                .is_ok_and(|status| status.success())
        };
        // Its reaping, once orphaned, is init's.
        let gone =
            pixtuoid_core::harness::wait_until("its reaping", Duration::from_secs(5), || {
                Ok(!alive())
            });
        assert!(
            gone.is_ok(),
            "the launcher's child {} outlived the probe",
            pid.trim()
        );
    }

    /// Two CLIs that each answer only once the other has started: probed one
    /// after the other, the first waits out the deadline.
    #[cfg(unix)]
    #[test]
    fn every_version_probe_runs_at_once() {
        const LONG: Duration = Duration::from_secs(30);
        let dir = tempfile::tempdir().unwrap();
        let mark = |name: &str| dir.path().join(format!("{name}.up")).display().to_string();
        let meets = |me: &str, other: &str| {
            let (mine, theirs) = (mark(me), mark(other));
            fake_cli(
                dir.path(),
                me,
                &format!(
                    ": > '{mine}'\nwhile [ ! -e '{theirs}' ]; do sleep 0.01; done\necho 1.0.0"
                ),
            )
        };
        let (a, b) = (meets("a", "b"), meets("b", "a"));
        let probes = [Some(vec![a.as_str()]), Some(vec![b.as_str()])];
        let got = probe_versions(&probes, LONG).unwrap();
        assert_eq!(got, vec![InstalledVersion::Read("1.0.0".into()); 2]);
    }

    #[test]
    fn root_row_states_the_fact_and_only_warns_on_missing_with_an_override() {
        let ink = Ink { on: false };
        let root = |exists, env| RootStatus {
            source: "copilot",
            root: ShownPath::new("/home/u/.copilot/session-state"),
            exists,
            env,
        };

        let ok = format_root_row(&root(true, Some(("COPILOT_HOME", true))), &ink);
        assert!(ok.contains("(via $COPILOT_HOME)"), "{ok}");
        assert!(
            !ok.contains("missing"),
            "an existing root never warns: {ok}"
        );

        // A missing root with NO override is the ordinary "never ran this CLI"
        // case — a fact, not an alarm.
        let quiet = format_root_row(&root(false, Some(("COPILOT_HOME", false))), &ink);
        assert!(quiet.contains("— missing"), "{quiet}");
        assert!(!quiet.contains('↳'), "no override set -> no alarm: {quiet}");
        assert!(
            !quiet.contains("via $"),
            "an unset var is not a 'via': {quiet}"
        );

        // Missing WHILE overridden is the #880 shape, and the only one worth an
        // alarm line.
        let loud = format_root_row(&root(false, Some(("COPILOT_HOME", true))), &ink);
        assert!(
            loud.contains('↳') && loud.contains("COPILOT_HOME"),
            "{loud}"
        );

        // A source with no override column at all never grows a `via`.
        let plain = format_root_row(&root(false, None), &ink);
        assert!(!plain.contains('↳') && !plain.contains("via $"), "{plain}");
    }

    #[test]
    fn home_split_advisory_fires_only_on_windows_with_a_real_home_split() {
        let a = home_split_advisory(true, Some(r"C:\msys\home\me"), Some(r"C:\Users\me"));
        assert!(a.is_some());
        let a = a.unwrap();
        assert!(a.contains("HOME") && a.contains("USERPROFILE") && a.contains("sprite"));

        assert!(home_split_advisory(false, Some("/home/a"), Some("/home/b")).is_none());

        assert!(home_split_advisory(true, None, Some(r"C:\Users\me")).is_none());
        assert!(home_split_advisory(true, Some("  "), Some(r"C:\Users\me")).is_none());
        assert!(home_split_advisory(true, Some(r"C:\Users\me"), None).is_none());
    }

    #[test]
    fn home_split_advisory_ignores_cosmetic_path_differences() {
        assert!(home_split_advisory(true, Some(r"C:\Users\Me"), Some(r"c:/users/me/")).is_none());
        // A POSIX-form HOME vs a native USERPROFILE IS a real split (Git Bash).
        assert!(home_split_advisory(true, Some("/c/Users/me"), Some(r"C:\Users\me")).is_some());
        assert!(win_path_eq(r"C:\a\b", "c:/a/b/"));
        assert!(!win_path_eq("/c/a/b", r"C:\a\b"));
    }

    #[test]
    fn scan_counts_real_breadcrumb_lines_per_source() {
        let log = capture(|| {
            drift::unknown_event("copilot", "NewHookV2");
            drift::missing_field("copilot", "tool.execution_start", "toolName");
            drift::unknown_dispatch("copilot", "AgentV3");
            drift::shape_drift(
                "copilot",
                drift::ShapeDrift::RegistryKey {
                    path: std::path::Path::new("/r/1.json"),
                    key: "pid",
                },
            );
            drift::unknown_event("codex", "OtherHook");
        });
        let r = scan_log_for_source(&log, "copilot");
        assert_eq!(r.unknown_event, 1, "log:\n{log}");
        assert_eq!(r.missing_field, 1);
        assert_eq!(r.unknown_dispatch, 1);
        assert_eq!(r.shape_drift, 1);
        assert_eq!(r.total(), 4);
        assert!(
            r.samples.contains(&"NewHookV2".to_string()),
            "samples={:?}",
            r.samples
        );
        assert!(r.samples.contains(&"AgentV3".to_string()));
        assert!(r.last_ts.is_some());
        let rc = scan_log_for_source(&log, "codex");
        assert_eq!(rc.unknown_event, 1);
        assert_eq!(rc.missing_field, 0);
    }

    #[test]
    fn scan_of_empty_log_is_clean() {
        assert_eq!(scan_log_for_source("", "copilot"), LogScanResult::default());
    }

    #[test]
    fn scan_ignores_a_body_mention_of_the_target_string() {
        let line = "2026-06-15T00:00:00Z  WARN pixtuoid::source::manager: a pixtuoid::drift mention source=copilot kind=unknown_event name=X";
        assert_eq!(
            scan_log_for_source(line, "copilot").total(),
            0,
            "the line carries valid `source=`/`kind=`, so ONLY the missing structural \
             `target:` marker keeps it out of the tally"
        );
    }

    #[test]
    fn scan_rejects_a_longer_target_suffixing_our_token() {
        let line = "2026-06-15T00:00:00Z  WARN myapp::pixtuoid::drift: source=copilot kind=\"unknown_event\" name=X";
        assert_eq!(
            scan_log_for_source(line, "copilot").total(),
            0,
            "unlike the body-mention case, the marker IS present — `find` succeeds and only \
             the space-guard prevents the false count"
        );
    }

    // No production code wraps a decoder in a `source=`-carrying span today; this
    // pins the parser so adding one later can't silently misattribute.
    #[test]
    fn scan_parses_event_fields_not_a_span_field_of_the_same_name() {
        let line = "2026-06-15T00:00:00Z  WARN decode{source=spanwrong}: pixtuoid::drift: source=copilot kind=\"unknown_event\" name=NewHook";
        let r = scan_log_for_source(line, "copilot");
        assert_eq!(r.unknown_event, 1, "event source must win");
        assert!(
            r.samples.contains(&"NewHook".to_string()),
            "{:?}",
            r.samples
        );
        assert_eq!(scan_log_for_source(line, "spanwrong").total(), 0);
    }

    #[test]
    fn scan_preserves_a_spaced_sample_value() {
        let line = "2026-06-15T00:00:00Z  WARN pixtuoid::drift: source=copilot kind=\"unknown_dispatch\" tool=My New Tool";
        let r = scan_log_for_source(line, "copilot");
        assert_eq!(r.unknown_dispatch, 1);
        assert!(
            r.samples.contains(&"My New Tool".to_string()),
            "{:?}",
            r.samples
        );
    }

    #[test]
    fn samples_are_sanitized_deduped_and_capped() {
        let log = capture(|| {
            for _ in 0..3 {
                drift::unknown_event("cursor", "Dup");
            }
            for i in 0..10 {
                drift::unknown_event("cursor", Box::leak(format!("E{i}").into_boxed_str()));
            }
        });
        let r = scan_log_for_source(&log, "cursor");
        assert!(r.unknown_event >= 11);
        assert!(r.samples.len() <= SAMPLE_CAP, "capped: {:?}", r.samples);
        assert_eq!(
            r.samples.iter().filter(|s| *s == "Dup").count(),
            1,
            "deduped"
        );
        assert!(!r.samples.iter().any(|s| s.chars().any(|c| c.is_control())));
    }

    #[test]
    fn format_row_clean_vs_drift_and_transcript_only() {
        let clean = DoctorSourceRow {
            desc: registered("codex"),
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: InstalledVersion::Read("2.0.0".into()),
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult::default()),
                drift: LogScanResult::default(),
            },
        };
        let ink = Ink { on: false };
        let c = format_doctor_row(&clean, &ink);
        assert!(c.contains("codex") && c.contains("connected") && c.contains("installed"));
        assert!(c.contains("2.0.0"));
        assert!(
            c.starts_with("      \u{2713}"),
            "sound row leads with ✓: {c}"
        );
        assert!(!c.contains('\n'), "a healthy row has no reason line: {c}");
        assert!(
            !c.to_lowercase().contains("broken"),
            "a sound install must not say broken: {c}"
        );

        let drifted = DoctorSourceRow {
            desc: registered("copilot"),
            connected: true,
            has_target: false, // transcript-only
            hooks_installed: false,
            installed_version: InstalledVersion::Read("1.1.0".into()),
            diag: SourceDiagnostics {
                install: None,
                drift: LogScanResult {
                    missing_field: 3,
                    ..Default::default()
                },
            },
        };
        let d = format_doctor_row(&drifted, &ink);
        assert!(d.starts_with("      !"), "a drifting row leads with !: {d}");
        assert!(d.contains("transcript-only"), "{d}");
        assert!(
            !d.contains("verified"),
            "skew prose left the row for the versions category: {d}"
        );
        assert!(
            d.contains("\n          \u{21b3} decode drift: 3 missing-field"),
            "{d}"
        );
    }

    #[test]
    fn format_row_flags_a_broken_install() {
        let broken = DoctorSourceRow {
            desc: registered("reasonix"),
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: InstalledVersion::Unasked,
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult {
                    issues: vec!["shim binary missing: /old/pixtuoid-hook".into()],
                    notes: vec![],
                    ..Default::default()
                }),
                drift: LogScanResult::default(),
            },
        };
        let b = format_doctor_row(&broken, &Ink { on: false });
        assert!(
            b.starts_with("      \u{2717}"),
            "a broken row leads with ✗: {b}"
        );
        assert!(
            b.contains("\n          \u{21b3} install broken: shim binary missing"),
            "broken reason on its own ↳ line: {b}"
        );
    }

    fn diag(
        install: Option<crate::install::verify::SchemaVerifyResult>,
        drift: LogScanResult,
    ) -> SourceDiagnostics {
        SourceDiagnostics { install, drift }
    }

    #[test]
    fn diagnostics_healthy_has_no_summary_and_is_not_broken() {
        let d = diag(
            Some(crate::install::verify::SchemaVerifyResult::default()),
            LogScanResult::default(),
        );
        assert!(!d.is_broken());
        assert_eq!(d.summary(), None);
    }

    #[test]
    fn diagnostics_broken_install_wins_over_drift() {
        let d = diag(
            Some(crate::install::verify::SchemaVerifyResult {
                issues: vec!["shim binary missing: /x".into()],
                notes: vec![],
                ..Default::default()
            }),
            LogScanResult {
                unknown_event: 2,
                ..Default::default()
            },
        );
        assert!(d.is_broken());
        let s = d.summary().unwrap();
        assert!(
            s.contains("install broken") && s.contains("shim binary missing"),
            "{s}"
        );
        assert!(!s.contains("decode drift"), "install-broken must win: {s}");
    }

    #[test]
    fn diagnostics_drift_only_summarizes_when_install_is_sound() {
        let d = diag(
            Some(crate::install::verify::SchemaVerifyResult::default()),
            LogScanResult {
                missing_field: 3,
                ..Default::default()
            },
        );
        assert!(!d.is_broken());
        assert!(d.summary().unwrap().contains("3 decode drift"));
    }

    #[test]
    fn diagnostics_soft_notes_are_not_broken_and_do_not_summarize() {
        let d = diag(
            Some(crate::install::verify::SchemaVerifyResult {
                issues: vec![],
                notes: vec!["pixtuoid-hook not on PATH".into()],
                ..Default::default()
            }),
            LogScanResult::default(),
        );
        assert!(!d.is_broken());
        assert_eq!(d.summary(), None);
    }

    #[test]
    fn diagnostics_no_install_check_is_not_broken() {
        let d = diag(None, LogScanResult::default());
        assert!(!d.is_broken());
        assert_eq!(d.summary(), None);
    }

    #[test]
    fn diagnose_surfaces_an_install_broken_verdict_through_the_injected_config_root() {
        // BOTH the has_hooks gate and verify_target must read the SAME injected
        // root, or the broken verdict is unreachable hermetically.
        use crate::install::{install_target, target::CLAUDE};
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("m/pixtuoid/hooks/hooks.json");
        let d = crate::install::tests::with_fake_hosts(|_| {
            temp_env::with_var(
                crate::install::io::HOOK_OVERRIDE_ENV,
                Some("/nonexistent/pixtuoid-hook"),
                || install_target(&CLAUDE, Some(cfg.clone())).unwrap(),
            );
            diagnose(CLAUDE.core_source, "", Some(cfg))
        });
        assert!(
            d.is_broken(),
            "an install with a missing shim must read broken through the injected root"
        );
        let s = d.summary().unwrap();
        assert!(
            s.contains("install broken") && s.contains("shim binary missing"),
            "unexpected summary: {s:?}"
        );
    }

    #[test]
    fn version_cmp_compares_only_with_a_known_anchor() {
        use std::cmp::Ordering;
        let cmp = |installed: Option<&str>, verified| {
            let installed = registry::parse_version(installed?, Hyphen::Prerelease)?;
            version_cmp(&installed, verified, Hyphen::Prerelease)
        };
        assert_eq!(cmp(Some("3.4.5"), "unknown"), None);
        assert_eq!(cmp(Some("1.1.0"), "1.0.62"), Some(Ordering::Greater));
        assert_eq!(cmp(Some("1.0.0"), "1.0.62"), Some(Ordering::Less));
        assert_eq!(cmp(Some("1.0.62"), "1.0.62"), Some(Ordering::Equal));
        assert_eq!(cmp(None, "1.0.62"), None);
        assert_eq!(cmp(Some("garbage"), "1.0.62"), None);
        assert_eq!(
            cmp(Some("0.2.0-rc.3"), "0.2.0-rc.2"),
            Some(Ordering::Greater),
            "a prerelease past the pin runs ahead of it"
        );
        assert_eq!(
            cmp(Some("0.2.0-rc.2"), "0.2.0"),
            Some(Ordering::Less),
            "a prerelease runs below its release"
        );
    }

    #[test]
    fn the_versions_row_shows_only_what_was_compared() {
        let shown = |s, hyphen| compared_version(&registry::parse_version(s, hyphen).unwrap());
        assert_eq!(
            shown("2026.10.02-abc1234", Hyphen::Build),
            "2026.10.2",
            "a build id no comparison reads is not shown"
        );
        assert_eq!(shown("0.2.0-rc.3", Hyphen::Prerelease), "0.2.0-rc.3");
    }

    #[test]
    fn a_same_day_build_of_a_build_hyphen_cli_is_its_pin() {
        use std::cmp::Ordering;
        for installed in [
            "2026.09.18-abc1234",
            "2026.09.18-1234567",
            "2026.09.18-0123456",
        ] {
            assert_eq!(
                version_cmp(
                    &registry::parse_version(installed, Hyphen::Build).unwrap(),
                    "2026.09.18",
                    Hyphen::Build
                ),
                Some(Ordering::Equal),
                "{installed}"
            );
        }
        assert_eq!(
            registry::descriptor_for("cursor").map(|d| d.version_hyphen),
            Some(Hyphen::Build),
            "cursor-agent prints its build id after a `-`"
        );
    }

    /// The footer's drift is this run's breadcrumbs alone, each source once,
    /// first seen first: no other event, nor a target that merely has the
    /// breadcrumbs' target as a prefix.
    #[test]
    fn drift_seen_and_footer_warning() {
        use tracing_subscriber::layer::SubscriberExt;
        let seen = DriftSeen::default();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(seen.layer()),
            || {
                drift::unknown_event("claude-code", "NewHook");
                tracing::warn!(source = "copilot", "an ordinary warning naming a source");
                tracing::warn!(target: "pixtuoid::drift_surface", source = %"copilot", "a prefixed target");
                drift::missing_field("codex", "function_call", "name");
                drift::unknown_event("claude-code", "Again");
            },
        );
        let d = seen.prefixes();
        assert_eq!(d, vec!["cc".to_string(), "cx".to_string()]);
        let died = |s: &str| pixtuoid_core::source::manager::SourceDeath::new(s, "boom");
        assert_eq!(
            footer_warning(&[died("claude-code")], &d).as_deref(),
            Some("claude-code source died — its agents are frozen; restart pixtuoid (see log)")
        );
        assert_eq!(
            footer_warning(&[died("claude-code"), died("codex")], &d).as_deref(),
            Some("2 sources died — restart pixtuoid (see log)")
        );
        let w = footer_warning(&[], &d).unwrap();
        assert!(
            w.contains("cc·") && w.contains("cx·") && w.contains("doctor"),
            "{w}"
        );
        // The footer painter (`footer.rs` `" ⚠ {warn} "`) owns the warning glyph;
        // an embedded one double-prints (`⚠ ⚠ …`).
        assert!(!w.contains('⚠'), "drift msg must not embed ⚠: {w}");
        let dw = footer_warning(&[died("claude-code")], &d).unwrap();
        assert!(!dw.contains('⚠'), "death msg must not embed ⚠: {dw}");
        assert_eq!(footer_warning(&[], &[]), None);
    }

    #[test]
    fn probe_output_is_sanitized_and_first_nonempty() {
        let raw = b"\n\n\x1b]0;pwned\x07cli \x1b[31m1.2.3\x1b[0m\nnext line";
        let got = first_sanitized_line(raw).unwrap();
        assert_eq!(got, "]0;pwnedcli [31m1.2.3[0m"); // ESC + BEL stripped, text kept
        assert!(
            !got.chars().any(|c| c.is_control()),
            "no control chars: {got:?}"
        );
        assert_eq!(first_sanitized_line(b""), None);
        assert_eq!(first_sanitized_line(b"   \n  \n"), None);
    }

    #[test]
    fn scan_does_not_pick_name_inside_displayname() {
        let line = "2026-06-15T00:00:00Z  WARN pixtuoid::drift: source=copilot kind=\"unknown_event\" displayName=foo name=Real";
        let r = scan_log_for_source(line, "copilot");
        assert_eq!(r.unknown_event, 1, "line:\n{line}");
        assert!(r.samples.contains(&"Real".to_string()), "{:?}", r.samples);
        assert!(!r.samples.contains(&"foo".to_string()), "{:?}", r.samples);
    }

    #[test]
    fn scan_unknown_event_without_a_name_field_counts_but_samples_none() {
        let line =
            "2026-06-15T00:00:00Z  WARN pixtuoid::drift: source=copilot kind=\"unknown_event\"";
        let r = scan_log_for_source(line, "copilot");
        assert_eq!(r.unknown_event, 1, "line:\n{line}");
        assert!(r.samples.is_empty(), "{:?}", r.samples);
    }

    #[test]
    fn scan_ignores_an_unknown_drift_kind() {
        let line =
            "2026-06-15T00:00:00Z  WARN pixtuoid::drift: source=copilot kind=\"bogus_kind\" name=X";
        let r = scan_log_for_source(line, "copilot");
        assert_eq!(r.total(), 0, "line:\n{line}");
        assert!(r.samples.is_empty(), "{:?}", r.samples);
        assert!(
            r.last_ts.is_none(),
            "an ignored kind must not stamp last_ts"
        );
    }

    #[test]
    fn verdict_glyph_dash_for_clean_transcript_only_row() {
        let row = DoctorSourceRow {
            desc: registered("copilot"),
            connected: true,
            has_target: false,
            hooks_installed: false,
            installed_version: InstalledVersion::Unasked,
            diag: SourceDiagnostics {
                install: None,
                drift: LogScanResult::default(),
            },
        };
        let s = format_doctor_row(&row, &Ink { on: false });
        assert!(
            s.starts_with("      \u{2013}"),
            "clean transcript-only leads with –: {s}"
        );
        assert!(s.contains("transcript-only"), "{s}");
        assert!(!s.contains('\n'), "a clean row is a single line: {s}");
    }

    #[test]
    fn row_installable_but_not_installed_shows_circle_and_not_installed() {
        let row = DoctorSourceRow {
            desc: registered("claude-code"),
            connected: false,
            has_target: true,
            hooks_installed: false,
            installed_version: InstalledVersion::Unasked,
            diag: SourceDiagnostics {
                install: None,
                drift: LogScanResult::default(),
            },
        };
        let s = format_doctor_row(&row, &Ink { on: false });
        assert!(
            s.starts_with("      \u{25cb}"),
            "installable-not-installed leads with ○: {s}"
        );
        assert!(s.contains("not installed"), "{s}");
        assert!(
            s.contains("disconnected"),
            "connected:false → disconnected: {s}"
        );
        assert!(!s.contains('\n'), "no problem → single line: {s}");
    }

    #[test]
    fn format_row_emits_note_continuation_for_sound_schema_with_notes() {
        let row = DoctorSourceRow {
            desc: registered("codewhale"),
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: InstalledVersion::Read("1.0.0".into()),
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult {
                    issues: vec![],
                    notes: vec!["pixtuoid-hook not on PATH".into()],
                    ..Default::default()
                }),
                drift: LogScanResult::default(),
            },
        };
        let s = format_doctor_row(&row, &Ink { on: false });
        assert!(
            s.starts_with("      \u{2713}"),
            "sound-with-notes still ✓: {s}"
        );
        assert!(
            s.contains("\n          \u{21b3} note: pixtuoid-hook not on PATH"),
            "note on its own ↳ line: {s}"
        );
        assert!(
            !s.to_lowercase().contains("broken"),
            "a note is not broken: {s}"
        );
    }

    #[test]
    fn format_row_drift_detail_covers_all_kinds_samples_and_last_ts() {
        let row = DoctorSourceRow {
            desc: registered("copilot"),
            connected: true,
            has_target: false,
            hooks_installed: false,
            installed_version: InstalledVersion::Unasked,
            diag: SourceDiagnostics {
                install: None,
                drift: LogScanResult {
                    unknown_event: 2,
                    missing_field: 0,
                    unknown_dispatch: 1,
                    shape_drift: 1,
                    samples: vec!["NewHook".into(), "MyTool".into()],
                    last_ts: Some("2026-06-15T00:00:00Z".into()),
                },
            },
        };
        let s = format_doctor_row(&row, &Ink { on: false });
        assert!(
            s.contains("decode drift: 2 unknown-event, 1 unknown-dispatch, 1 shape-drift"),
            "{s}"
        );
        assert!(s.contains("(last 2026-06-15T00:00:00Z)"), "{s}");
        assert!(s.contains("[NewHook, MyTool]"), "{s}");
    }
}
