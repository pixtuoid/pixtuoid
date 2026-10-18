//! `pixtuoid doctor` — a diagnosis of this machine's setup, one category per
//! concern (`render`). It surfaces what otherwise dies unread: the decode-drift
//! breadcrumbs (`source/drift.rs`, under the [`drift::TARGET`] tracing target) in
//! the warn-floor log. Strictly READ-ONLY: it never writes config (re-connecting
//! hooks stays the Sources panel's job) and never spawns the TUI.
//! The PROBED CLI is not read-only about its own state — see `may_probe_version`.

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
    pub(crate) fn is_broken(&self) -> bool {
        self.install.as_ref().is_some_and(|i| !i.is_sound())
    }

    /// The single worst issue as a one-line, glyph-prefixed summary. Priority:
    /// install-broken (hooks can't fire) > decode-drift.
    pub(crate) fn summary(&self) -> Option<String> {
        if let Some(i) = &self.install
            && !i.is_sound()
        {
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
    pub prefix: &'static str,
    /// The registry source id (e.g. "claude-code"), distinct from
    /// `install::Target.name`/`display_name`.
    pub source_id: &'static str,
    pub connected: bool,
    pub has_target: bool,
    pub hooks_installed: bool,
    /// The probe's first output line, sanitized ([`first_sanitized_line`]), if
    /// probeable.
    pub installed_version: Option<String>,
    /// The version this build's decoder was verified against; `"unknown"` = no
    /// anchor.
    pub verified_version: &'static str,
    /// The SAME [`SourceDiagnostics`] the Sources panel + boot preflight read,
    /// embedded whole so `is_broken`/`drift` stay the ONE authority.
    pub diag: SourceDiagnostics,
}

/// A major at or above this is a CalVer year (openclaw, cursor), and a new
/// year is no breaking major.
const CALVER_MIN_MAJOR: u64 = 1000;

/// Installed-vs-verified comparison, `None` when either side can't parse or
/// there is no anchor.
pub(crate) fn version_cmp(
    installed: Option<&str>,
    verified: &str,
    hyphen: Hyphen,
) -> Option<std::cmp::Ordering> {
    let installed = installed.and_then(|s| registry::parse_version(s, hyphen))?;
    Some(installed.cmp_precedence(&registry::parse_version(verified, hyphen)?))
}

/// The version a skew verdict compared — for the `versions` category, where
/// showing the two COMPARED values beats echoing a raw banner.
fn parsed_version_display(s: &str, hyphen: Hyphen) -> Option<String> {
    registry::parse_version(s, hyphen).map(|mut v| {
        v.build = semver::BuildMetadata::EMPTY;
        v.to_string()
    })
}

/// How the row's CLI writes a `-` in its version.
fn hyphen_of(r: &DoctorSourceRow) -> Hyphen {
    registry::descriptor_for(r.source_id).map_or(Hyphen::Prerelease, |d| d.version_hyphen)
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
    let version = row
        .installed_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let version = version.map_or_else(|| ink.dim("unknown"), str::to_string);
    let mut out = format!(
        "{DETAIL_INDENT}{} {}\u{b7}{:<13} {} {:<15} {}",
        verdict_glyph(row, ink),
        row.prefix,
        row.source_id,
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

/// Probe a source's `<cli> --version` (argv from the static registry — never user input)
/// → the first non-empty output line, sanitized. Best-effort: a spawn error, a NONZERO
/// exit (whose error text must never show as a version), or a hang all yield None. stdin
/// is nulled so the child can't block on the inherited TTY.
fn probe_version(argv: &'static [&'static str]) -> Option<String> {
    use std::process::{Command, Stdio};
    const PROBE_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let (cmd, args) = argv.split_first()?;
    let output = crate::output_within(
        Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            // `--version` output is tiny, which `output_within` requires.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        PROBE_VERSION_TIMEOUT,
    )?;
    if !output.status.success() {
        return None;
    }
    first_sanitized_line(&output.stdout).or_else(|| first_sanitized_line(&output.stderr))
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
/// `run_never_spawns_a_version_probe_for_a_cli_it_has_no_evidence_of`.
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
    /// What `run` resolves `graphics` to without a flag: the config, then the
    /// default.
    run_graphics: crate::GraphicsMode,
    rows: Vec<DoctorSourceRow>,
    roots: Vec<RootStatus>,
    backend: &'static str,
    /// `false` = the focus backend itself can't work here (the Wayland /
    /// no-display arms) — the focus category's verdict, not a probe result.
    backend_healthy: bool,
    /// `None` = probe disabled (non-standard projects root).
    cc_registry: Option<(ShownPath, bool)>,
    codex_sessions: (ShownPath, bool),
    /// The other two probe roots (omp's probe is the stamp-less FALLBACK since
    /// the PluginStamp flip). Each is source-specific — omp's is the resolved
    /// sessions dir, grok's a registry FILE — so each needs its own
    /// hand-written row, pinned by `every_focusable_source_appears_in_the_focus_category`.
    omp_sessions: (ShownPath, bool),
    grok_registry: (ShownPath, bool),
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
    graphics: crate::GraphicsMode,
    max_density: pixtuoid_core::sprite::format::Density,
) -> (Option<crate::term::Truecolor>, crate::graphics::Plan) {
    let truecolor_probe =
        probe_ok.then(|| crate::term::query_truecolor(crate::term::TRUECOLOR_PROBE_TIMEOUT));
    let graphics_plan = crate::graphics::plan_this_terminal(graphics, max_density, probe_ok);
    (truecolor_probe, graphics_plan)
}

/// A wrong root has no symptom but an empty office, so state it outright (#880). Goes
/// through `resolved_source_root` — the SAME call the driver makes, since a second copy
/// could show a healthy path while the watcher polled another.
fn collect_roots() -> Vec<RootStatus> {
    registry::registered_source_names()
        .filter_map(|src| {
            let root = pixtuoid_core::source::resolved_source_root(src)?;
            let env = registry::descriptor_for(src)
                .and_then(|d| d.home_env)
                // The SAME `path_env` the resolvers use, or the two disagree: `env::var` reads
                // a non-UTF-8 override as UNSET, silencing the ⚠ when the root IS wrong (#172).
                .map(|v| (v, pixtuoid_core::platform::path_env(v).is_some()));
            let exists = root.is_dir();
            Some(RootStatus {
                source: src,
                root: ShownPath::new(root),
                exists,
                env,
            })
        })
        .collect()
}

/// The click's probe roots ([`crate::focus::FocusPaths::from_env`]), so the
/// report cannot claim a root a click would not read. grok's is a registry
/// FILE, not a directory; omp's is its primary root.
struct ProbeRoots {
    cc_registry: Option<(std::path::PathBuf, bool)>,
    codex_sessions: std::path::PathBuf,
    codex_exists: bool,
    omp_sessions: std::path::PathBuf,
    omp_exists: bool,
    grok_registry: std::path::PathBuf,
    grok_exists: bool,
}

fn probe_roots() -> ProbeRoots {
    let roots = crate::focus::FocusPaths::from_env();
    let cc_registry = pixtuoid_core::source::cc_registry_dir(&roots.cc_projects_root).map(|d| {
        let exists = d.is_dir();
        (d, exists)
    });
    let codex_sessions = roots.codex_sessions_root;
    let omp_sessions = roots
        .omp_sessions_roots
        .into_iter()
        .next()
        .unwrap_or_default();
    let grok_registry = roots.grok_root.join("active_sessions.json");
    ProbeRoots {
        cc_registry,
        codex_exists: codex_sessions.is_dir(),
        codex_sessions,
        omp_exists: omp_sessions.is_dir(),
        omp_sessions,
        grok_exists: grok_registry.is_file(),
        grok_registry,
    }
}

/// All probing, no formatting. `log_at` is injected by `main` (resolved by
/// [`LogLocation::from_env`](crate::run_log::LogLocation::from_env));
/// `graphics` is the `--graphics` flag.
fn collect(
    log_at: &crate::run_log::LogLocation,
    graphics: crate::GraphicsMode,
) -> anyhow::Result<DoctorReport> {
    let mut config_warnings = Vec::new();
    let config_path = crate::config::config_path();
    let cfg = crate::config::load(&config_path, &mut config_warnings);
    // A separate PROCESS from the TUI, so the live `ConnectedSources` is
    // unreachable; it reads the same facts that gate does.
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
    let (truecolor_probe, graphics_plan) = probe_terminal_caps(probe_ok, graphics, max_density);
    let run_graphics = crate::config::resolve_graphics(&cfg, None, &mut config_warnings);

    let rows: Vec<DoctorSourceRow> = registry::registered_source_names()
        .map(|src| {
            let desc = registry::descriptor_for(src);
            let target = crate::install::target::by_source(src);
            let hooks_installed = target
                .map(|t| crate::install::has_hooks(t, None))
                .unwrap_or(false);
            let is_connected = connected.contains(src);
            let cli_detected = target.map(|t| (t.presence_probe)());
            DoctorSourceRow {
                prefix: desc.map(|d| d.label_prefix).unwrap_or("??"),
                source_id: src,
                connected: is_connected,
                has_target: target.is_some(),
                hooks_installed,
                installed_version: may_probe_version(is_connected, cli_detected)
                    .then(|| desc.and_then(|d| d.version_probe).and_then(probe_version))
                    .flatten(),
                verified_version: desc.map(|d| d.verified_version).unwrap_or("unknown"),
                diag: diagnose(src, &log, None),
            }
        })
        .collect();

    let roots = collect_roots();
    let ProbeRoots {
        cc_registry,
        codex_sessions,
        codex_exists,
        omp_sessions,
        omp_exists,
        grok_registry,
        grok_exists,
    } = probe_roots();

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
        run_graphics,
        rows,
        roots,
        backend,
        backend_healthy,
        cc_registry: cc_registry.map(|(p, exists)| (ShownPath::new(p), exists)),
        codex_sessions: (ShownPath::new(codex_sessions), codex_exists),
        omp_sessions: (ShownPath::new(omp_sessions), omp_exists),
        grok_registry: (ShownPath::new(grok_registry), grok_exists),
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
        r.graphics_plan.diagnostic_row(r.run_graphics)
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
fn below_verified_major(installed: Option<&str>, verified: &str, hyphen: Hyphen) -> bool {
    match (
        installed.and_then(|s| registry::parse_version(s, hyphen)),
        registry::parse_version(verified, hyphen),
    ) {
        (Some(i), Some(v)) => i.major < v.major && v.major < CALVER_MIN_MAJOR,
        _ => false,
    }
}

/// Two skews are worth an alarm: an older major, which is unsupported, and a
/// CLI running AHEAD of the version this build's decoder was verified against
/// (its wire format may have moved). An older minor or patch stays silent —
/// that comparison is derivable, not actionable.
fn versions_category(rows: &[DoctorSourceRow]) -> Category {
    let older: Vec<&DoctorSourceRow> = rows
        .iter()
        .filter(|r| {
            below_verified_major(
                r.installed_version.as_deref(),
                r.verified_version,
                hyphen_of(r),
            )
        })
        .collect();
    let newer: Vec<&DoctorSourceRow> = rows
        .iter()
        .filter(|r| {
            version_cmp(
                r.installed_version.as_deref(),
                r.verified_version,
                hyphen_of(r),
            ) == Some(std::cmp::Ordering::Greater)
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
    let row = |r: &DoctorSourceRow, tail: String| {
        // Both buckets compared a parsed installed side.
        let inst = r
            .installed_version
            .as_deref()
            .and_then(|s| parsed_version_display(s, hyphen_of(r)))
            .unwrap_or_else(|| "unknown".to_string());
        format!(
            "{DETAIL_INDENT}{:<15} {inst} installed \u{b7} {} verified{tail}",
            format!("{}\u{b7}{}", r.prefix, r.source_id),
            r.verified_version
        )
    };
    let details = older
        .iter()
        .map(|r| row(r, format!(" — unsupported, upgrade {}", r.source_id)))
        .chain(newer.iter().map(|r| row(r, String::new())))
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
    let mut line = format!("{DETAIL_INDENT}{:<13} {}{via}", r.source, r.root);
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
    let prefix_of = |src: &str| {
        registry::descriptor_for(src)
            .map(|d| d.label_prefix)
            .unwrap_or("??")
    };
    let mut details = Vec::new();
    let mut problem = !r.backend_healthy;
    let cc_prefix = prefix_of(pixtuoid_core::source::claude_code::SOURCE_NAME);
    match &r.cc_registry {
        Some((p, true)) => details.push(format!(
            "{DETAIL_INDENT}{cc_prefix}\u{b7}claude-code — registry probe {} {}",
            ink.ok("\u{2713}"),
            p
        )),
        Some((p, false)) => {
            problem = true;
            details.push(format!(
                "{DETAIL_INDENT}{cc_prefix}\u{b7}claude-code — registry probe {} {} (focus no-ops until \
                 CC writes it)",
                ink.bad("\u{2717} missing"),
                p
            ));
        }
        None => {
            problem = true;
            details.push(format!(
                "{DETAIL_INDENT}{cc_prefix}\u{b7}claude-code — registry probe disabled (non-standard \
                 projects root) — focus no-ops"
            ));
        }
    }
    let cx_prefix = prefix_of(pixtuoid_core::source::codex::SOURCE_NAME);
    if r.codex_sessions.1 {
        details.push(format!(
            "{DETAIL_INDENT}{cx_prefix}\u{b7}codex — rollout probe {} {}",
            ink.ok("\u{2713}"),
            r.codex_sessions.0
        ));
    } else {
        problem = true;
        details.push(format!(
            "{DETAIL_INDENT}{cx_prefix}\u{b7}codex — rollout probe {} {} (focus no-ops until codex \
             writes it)",
            ink.bad("\u{2717} missing"),
            r.codex_sessions.0
        ));
    }
    let om_prefix = prefix_of(pixtuoid_core::source::omp::SOURCE_NAME);
    if r.omp_sessions.1 {
        details.push(format!(
            "{DETAIL_INDENT}{om_prefix}\u{b7}omp — append-fd probe (stamp-less fallback) {} {}",
            ink.ok("\u{2713}"),
            r.omp_sessions.0
        ));
    } else {
        // Not a problem row since the PluginStamp flip: the stamped pid is the
        // primary channel, this probe only covers stamp-less shapes.
        details.push(format!(
            "{DETAIL_INDENT}{om_prefix}\u{b7}omp — append-fd probe (stamp-less fallback) {} {}",
            ink.warn("\u{2717} missing"),
            r.omp_sessions.0
        ));
    }
    let gk_prefix = prefix_of(pixtuoid_core::source::grok::SOURCE_NAME);
    if r.grok_registry.1 {
        details.push(format!(
            "{DETAIL_INDENT}{gk_prefix}\u{b7}grok — session registry {} {}",
            ink.ok("\u{2713}"),
            r.grok_registry.0
        ));
    } else {
        problem = true;
        details.push(format!(
            "{DETAIL_INDENT}{gk_prefix}\u{b7}grok — session registry {} {} (focus no-ops until grok \
             writes it)",
            ink.bad("\u{2717} missing"),
            r.grok_registry.0
        ));
    }
    use registry::FocusChannel;
    let mut shim_stamp = Vec::new();
    let mut plugin_stamp = Vec::new();
    let mut no_channel = Vec::new();
    for src in registry::registered_source_names() {
        let Some(d) = registry::descriptor_for(src) else {
            continue;
        };
        // Daemons aren't click-focusable agents; the TranscriptProbe sources
        // got their own probe rows above (omp appears in BOTH: its probe row
        // is the stamp-less fallback, this census line the primary channel).
        if d.is_daemon() || d.focus_channel() == FocusChannel::TranscriptProbe {
            continue;
        }
        let tag = format!("{}\u{b7}{}", d.label_prefix, src);
        match d.focus_channel() {
            FocusChannel::ShimStamp => shim_stamp.push(tag),
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
/// If the bundled pack does not load.
pub(crate) fn run(
    log_at: &crate::run_log::LogLocation,
    graphics: crate::GraphicsMode,
) -> anyhow::Result<String> {
    Ok(render(&collect(log_at, graphics)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_log::read_log;
    use crate::test_capture::capture;

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
        assert!(s.contains("claude-code — registry probe ✓"), "{s}");
        assert!(s.contains("codex — rollout probe ✓"), "{s}");
        assert!(
            s.contains("dsh") && s.contains("plugin-stamped"),
            "plugin stampers listed separately: {s}"
        );
        assert!(
            s.contains("om\u{b7}omp — append-fd probe (stamp-less fallback)"),
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
        r.cc_registry = Some((ShownPath::new("/home/u/.claude/sessions"), false));
        r.codex_sessions = (ShownPath::new("/home/u/.codex/sessions"), false);
        let c = focus_category(&r, &Ink { on: false });
        assert_eq!(c.status, CategoryStatus::Warn);
        let s = c.details.join("\n");
        assert!(
            s.contains("✗ missing") && s.contains("focus no-ops until CC writes it"),
            "{s}"
        );
        assert!(s.contains("focus no-ops until codex writes it"), "{s}");

        r.cc_registry = None;
        r.codex_sessions = (ShownPath::new("/home/u/.codex/sessions"), true);
        let c = focus_category(&r, &Ink { on: false });
        assert_eq!(c.status, CategoryStatus::Warn);
        assert!(
            c.details
                .iter()
                .any(|l| l.contains("registry probe disabled (non-standard projects root)")),
            "{:?}",
            c.details
        );
    }

    /// Every focusable (non-daemon) registered source must appear SOMEWHERE in
    /// the category. A `TranscriptProbe` row is HAND-WRITTEN — the registry loop
    /// below it skips that channel — so without this a newly probe-backed source
    /// silently VANISHES from the report, which is exactly what omp did the
    /// moment it left `Unsupported`.
    #[test]
    fn every_focusable_source_appears_in_the_focus_category() {
        let mut r = summary_report(vec![]);
        r.cc_registry = Some((ShownPath::new("/home/u/x"), true));
        r.codex_sessions = (ShownPath::new("/home/u/x"), true);
        r.omp_sessions = (ShownPath::new("/home/u/x"), true);
        r.grok_registry = (ShownPath::new("/home/u/x"), true);
        let s = focus_category(&r, &Ink { on: false }).details.join("\n");
        for src in registry::registered_source_names() {
            let Some(d) = registry::descriptor_for(src) else {
                continue;
            };
            if d.is_daemon() {
                continue;
            }
            let tag = format!("{}\u{b7}{}", d.label_prefix, src);
            assert!(
                s.contains(&tag),
                "{tag} is missing from the focus category:\n{s}"
            );
        }
    }

    #[test]
    fn run_renders_the_category_report() {
        // A dev shell exporting CLICOLOR_FORCE would force escapes even under
        // captured stdout — pin the env so the plain-text asserts hold anywhere.
        let out = temp_env::with_vars_unset(["CLICOLOR_FORCE", "NO_COLOR"], || {
            run(
                &crate::run_log::LogLocation::File("/nonexistent-pixtuoid-doctor-log".into()),
                crate::GraphicsMode::Auto,
            )
        });
        let out = out.unwrap();
        assert!(out.starts_with("pixtuoid doctor\n"), "{out}");
        assert!(out.contains("log    "), "{out}");
        assert!(out.contains("config "), "{out}");
        assert!(out.contains("] terminal — TERM="), "{out}");
        assert!(out.contains("COLORTERM="), "{out}");
        assert!(out.contains("] sources — "), "{out}");
        assert!(out.contains("] decode drift — "), "{out}");
        assert!(out.contains("] focus-jump — "), "{out}");
        assert!(
            out.contains("\n\n["),
            "categories are separated by a blank line: {out}"
        );
        // Under `cargo test` stdout is captured (not a tty), so the report must
        // carry no escape codes.
        assert!(!out.contains('\u{1b}'), "piped output stays plain: {out}");
    }

    fn summary_row(prefix: &'static str, id: &'static str) -> DoctorSourceRow {
        DoctorSourceRow {
            prefix,
            source_id: id,
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: Some("1.0.0".into()),
            verified_version: "unknown",
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult::default()),
                drift: LogScanResult::default(),
            },
        }
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
            run_graphics: crate::GraphicsMode::Auto,
            rows,
            roots: vec![],
            backend: "NSRunningApplication (macOS)",
            backend_healthy: true,
            cc_registry: Some((ShownPath::new("/tmp/reg"), true)),
            codex_sessions: (ShownPath::new("/tmp/cx"), true),
            omp_sessions: (ShownPath::new("/tmp/om"), true),
            grok_registry: (ShownPath::new("/tmp/gk/active_sessions.json"), true),
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
        let mut row = summary_row("cc", "claude-code");
        row.installed_version = first_sanitized_line(format!("1.0.0 {EVIL}").as_bytes());
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
            notes: vec![],
        });
        let mut noted = summary_row("cx", "codex");
        noted.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            issues: vec![],
            notes: vec![evil()],
        });
        let mut r = summary_report(vec![row, noted]);
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
        r.cc_registry = Some((ShownPath::new(evil()), false));
        r.codex_sessions = (ShownPath::new(evil()), false);
        r.omp_sessions = (ShownPath::new(evil()), false);
        r.grok_registry = (ShownPath::new(evil()), false);
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
        let density = pixtuoid_core::sprite::format::Density::new(4).expect("nonzero");
        r.graphics_plan = Plan::Cutaway {
            fit: cutaway_fit(cell, area, density).expect("fits"),
            route: crate::graphics::Route::direct(ImageProtocol::Kitty, false),
            cell,
            chosen: crate::graphics::Chosen::Answer,
        };
        assert_eq!(
            terminal_category(&r).details[0],
            format!(
                "{DETAIL_INDENT}{}",
                r.graphics_plan.diagnostic_row(crate::GraphicsMode::Auto)
            )
        );
        r.run_graphics = crate::GraphicsMode::Off;
        assert!(
            terminal_category(&r).details[0].ends_with("paints it"),
            "the row reads run's own setting"
        );
        r.run_graphics = crate::GraphicsMode::Auto;

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
            summary_row("cc", "claude-code"),
            summary_row("cx", "codex"),
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
            summary_row("cc", "claude-code"),
            summary_row("cx", "codex"),
        ]));
        assert!(
            out.contains("\n      ✓ cc\u{b7}claude-code   connected    installed       1.0.0\n"),
            "every source keeps its row, healthy or not: {out}"
        );
    }

    #[test]
    fn report_broken_install_expands_with_fix_line() {
        let mut broken = summary_row("cx", "codex");
        broken.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            issues: vec!["missing hook entries for: SessionEnd".into()],
            notes: vec![],
        });
        let out = render(&summary_report(vec![
            summary_row("cc", "claude-code"),
            broken,
        ]));
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
        let mut newer = summary_row("cp", "copilot");
        newer.installed_version = Some("GitHub Copilot CLI 1.0.79.".into());
        newer.verified_version = "1.0.62";
        let mut older = summary_row("hm", "hermes");
        older.installed_version = Some("v0.17.0".into());
        older.verified_version = "0.18.0";
        let out = render(&summary_report(vec![newer, older]));
        assert!(
            out.contains("[!] versions — 1 CLI newer than verified (drift possible)"),
            "older-than-verified is not an alarm: {out}"
        );
        assert!(
            out.contains("      cp\u{b7}copilot      1.0.79 installed \u{b7} 1.0.62 verified"),
            "the PARSED pair that was compared, not the raw banner: {out}"
        );
        assert!(
            !out.contains("0.18.0 verified"),
            "older-than-verified grows no versions detail row: {out}"
        );
        // The raw banner stays on the source row, without skew prose.
        assert!(out.contains("GitHub Copilot CLI 1.0.79."), "{out}");
        assert!(!out.contains("NEWER than verified"), "{out}");
    }

    #[test]
    fn report_versions_category_flags_an_older_major_as_unsupported() {
        let mut v1 = summary_row("oc", "opencode");
        v1.installed_version = Some("1.18.30".into());
        v1.verified_version = "2.0.27";
        let mut older_minor = summary_row("cp", "copilot");
        older_minor.installed_version = Some("1.0.50".into());
        older_minor.verified_version = "1.0.62";
        let mut calver = summary_row("ok", "openclaw");
        calver.installed_version = Some("OpenClaw 2025.12.1".into());
        calver.verified_version = "2026.10.1";
        let out = render(&summary_report(vec![v1, older_minor, calver]));
        assert!(
            out.contains("[!] versions — 1 CLI unsupported (upgrade needed)\n"),
            "{out}"
        );
        assert!(
            out.contains(
                "      oc\u{b7}opencode     1.18.30 installed \u{b7} 2.0.27 verified — \
                 unsupported, upgrade opencode"
            ),
            "{out}"
        );
        assert!(
            !out.contains("1.0.62 verified") && !out.contains("2026.10.1 verified"),
            "an older minor, or a CalVer year, is no older major: {out}"
        );
    }

    #[test]
    fn report_drift_verdict_line_points_at_rows_and_upstream() {
        let mut drifted = summary_row("gk", "grok");
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
        let mut r = summary_report(vec![summary_row("cc", "claude-code")]);
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
        let mut r = summary_report(vec![summary_row("cc", "claude-code")]);
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
        let mut r = summary_report(vec![summary_row("cc", "claude-code")]);
        r.cc_registry = Some((ShownPath::new("/tmp/reg"), false));
        let out = render(&r);
        assert!(
            out.contains("[!] focus-jump — NSRunningApplication (macOS)\n"),
            "{out}"
        );
        assert!(
            out.contains("claude-code — registry probe ✗ missing /tmp/reg"),
            "{out}"
        );
    }

    #[test]
    fn report_footer_counts_issue_categories() {
        let mut broken = summary_row("cx", "codex");
        broken.diag.install = Some(crate::install::verify::SchemaVerifyResult {
            issues: vec!["shim binary missing: /old".into()],
            notes: vec![],
        });
        let mut newer = summary_row("cp", "copilot");
        newer.installed_version = Some("2.0.0".into());
        newer.verified_version = "1.0.0";
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

        // The report must not assert clean drift off a log it never read.
        let out = run(
            &crate::run_log::LogLocation::File(dir.path().to_path_buf()),
            crate::GraphicsMode::Auto,
        )
        .unwrap();
        assert!(out.contains("[!] decode drift — log unreadable"), "{out}");
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

    #[cfg(unix)]
    #[test]
    fn run_never_spawns_a_version_probe_for_a_cli_it_has_no_evidence_of() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (home, bin) = (dir.path().join("home"), dir.path().join("bin"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // Spawning the stand-in CLI AT ALL leaves this trace.
        let marker = dir.path().join("spawned");
        let fake = bin.join("opencode");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\n: > '{}'\necho 1.0.0\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let xdg_config = home.join(".config");
        let out = temp_env::with_vars(
            [
                ("HOME", Some(home.as_path())),
                ("XDG_CONFIG_HOME", Some(xdg_config.as_path())),
                ("OPENCODE_CONFIG_DIR", None),
                ("PATH", Some(bin.as_path())),
            ],
            || {
                run(
                    &crate::run_log::LogLocation::File("/nonexistent-pixtuoid-doctor-log".into()),
                    crate::GraphicsMode::Auto,
                )
            },
        );
        let spawned = marker.exists();

        out.expect("the report still builds");
        assert!(
            !spawned,
            "doctor spawned `opencode --version` in a pristine HOME where opencode \
             is undetected — the probe must be gated on evidence the user runs it"
        );
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
            drift::shape_drift("copilot", "registry missing pid");
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
            prefix: "cx",
            source_id: "codex",
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: Some("2.0.0".into()),
            verified_version: "unknown",
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
            prefix: "cp",
            source_id: "copilot",
            connected: true,
            has_target: false, // transcript-only
            hooks_installed: false,
            installed_version: Some("1.1.0".into()),
            verified_version: "1.0.62",
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
            prefix: "rx",
            source_id: "reasonix",
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: None,
            verified_version: "unknown",
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult {
                    issues: vec!["shim binary missing: /old/pixtuoid-hook".into()],
                    notes: vec![],
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
        let cmp = |installed, verified| version_cmp(installed, verified, Hyphen::Prerelease);
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
        assert_eq!(
            parsed_version_display("2026.10.02-abc1234", Hyphen::Build).as_deref(),
            Some("2026.10.2"),
            "a build id no comparison reads is not shown"
        );
        assert_eq!(
            parsed_version_display("0.2.0-rc.3", Hyphen::Prerelease).as_deref(),
            Some("0.2.0-rc.3")
        );
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
                version_cmp(Some(installed), "2026.09.18", Hyphen::Build),
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
            prefix: "cp",
            source_id: "copilot",
            connected: true,
            has_target: false,
            hooks_installed: false,
            installed_version: None,
            verified_version: "unknown",
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
            prefix: "cc",
            source_id: "claude-code",
            connected: false,
            has_target: true,
            hooks_installed: false,
            installed_version: None,
            verified_version: "unknown",
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
            prefix: "cw",
            source_id: "codewhale",
            connected: true,
            has_target: true,
            hooks_installed: true,
            installed_version: Some("1.0.0".into()),
            verified_version: "unknown",
            diag: SourceDiagnostics {
                install: Some(crate::install::verify::SchemaVerifyResult {
                    issues: vec![],
                    notes: vec!["pixtuoid-hook not on PATH".into()],
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
            prefix: "cp",
            source_id: "copilot",
            connected: true,
            has_target: false,
            hooks_installed: false,
            installed_version: None,
            verified_version: "unknown",
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
