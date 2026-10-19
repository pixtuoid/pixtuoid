//! Run each profiled agent CLI for real against a scripted model (aimock), its
//! install through its lifecycle (`<source> lifecycle`), and a daemon's live
//! tier beside it, with a headless pixtuoid on the same run, and judge the result.
//! Profiles, scripts and the judgement: `pixtuoid_core::harness::hermetic`.
//!
//! `--cli pinned` (the default) installs each CLI at its registry
//! `verified_version`, the merge queue's end-to-end gate; `--cli latest` installs
//! its package's latest release, the nightly drift run; `--cli <release>`
//! installs that release, a bump's planned one. `--record` writes a source's
//! fixtures, and off the pin moves its pin up to the release recorded.
//! `--shard <n>` runs the `n`th of `SHARDS`' sources, one CI leg's.
//!
//! Exits 0 when every run passed, 1 on any other verdict (a recording refused
//! after its runs included), and anything else when the tier itself could not
//! run or refused a recording before any CLI ran.

fn main() {
    match run::main() {
        Ok(all_passed) => std::process::exit(i32::from(!all_passed)),
        Err(e) => {
            eprintln!("hermetic: {e}");
            std::process::exit(2);
        }
    }
}

mod run {
    use std::ffi::{OsStr, OsString};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::Duration;

    use pixtuoid_core::harness::hermetic::bump::{self, Moved};
    use pixtuoid_core::harness::hermetic::digest;
    use pixtuoid_core::harness::hermetic::pin;
    use pixtuoid_core::harness::hermetic::tui;
    use pixtuoid_core::harness::hermetic::verify::{self, Label, Verdict};
    use pixtuoid_core::harness::hermetic::{
        self, DAEMON_PORT_PLACEHOLDER, DUMMY_KEY, HostConfig, LIFECYCLE, LIFECYCLE_RECORD,
        LIVE_PIXTUOID_ENV, LIVE_PREREQUISITE, Live, MANIFEST, Manifest, NOT_HERMETIC, PROFILES,
        Package, PerOs, Profile, SCENE_LOG, SHARDS, STEPS, Scenario, Step, StepRecord, Surface,
        TUI_LOG, USER_PLACEHOLDER, WORKSPACE_PLACEHOLDER, WORKSPACE_SLASHED_PLACEHOLDER,
    };
    use pixtuoid_core::harness::host::{RELEASE_PATIENCE, hold_lock, remove_once_released};
    use pixtuoid_core::harness::tap::Endpoint;
    use pixtuoid_core::harness::{
        BOOT, CAPTURE_RECORD, CORPUS_REFUSAL, CaptureRecord, capture_root, capture_workspace,
        capture_workspace_tail, recorder_flag, required_version_probe, version_answer, wait_until,
    };
    use semver::Version;

    /// How long one read from aimock may hang.
    const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(5);
    /// How long the headless scene runs on after the recorder returns, to
    /// decode the last payloads and log any breadcrumb they provoke.
    const SCENE_SETTLE: Duration = Duration::from_secs(4);
    /// Where the recorder's stdout and stderr go.
    const RECORDER_LOG: &str = "recorder.log";
    /// Where aimock's stdout and stderr go.
    const AIMOCK_LOG: &str = "aimock.log";
    /// The scripted model's npm package.
    const AIMOCK_PACKAGE: &str = "@copilotkit/aimock";
    /// The variables a CLI may inherit from the host, with each platform's own
    /// in [`PLATFORM_INHERITED`]: everything else is cleared, so an operator's
    /// provider keys can never reach a CLI whose base-URL wiring slipped and bill
    /// a real provider.
    const INHERITED_EVERYWHERE: &[&str] = &["PATH"];
    const PLATFORM_INHERITED: PerOs<&[&str]> = PerOs {
        unix: &[
            "LANG", "LC_ALL", "TERM", "TMPDIR", "SHELL", "USER", "LOGNAME",
        ],
        windows: &[
            // `connect` finds a CLI's `.cmd` through `which`, which reads it
            // (`which-8.0.6/src/finder.rs:277`).
            "PATHEXT",
            // "In order to run a side-by-side assembly the specified env must
            // include a valid %SystemRoot%" (docs.python.org, subprocess).
            "SystemRoot",
            // A CLI's shell: Node falls back to `cmd.exe` without it
            // (nodejs.org/api/child_process.html, "Default Windows shell").
            "ComSpec",
            // TMPDIR's twins: GetTempPath reads TMP, then TEMP, then
            // USERPROFILE, a sandbox home (learn.microsoft.com, GetTempPath2W).
            "TMP",
            "TEMP",
        ],
    };
    /// What the tier's tools inherit beyond [`inherited`]: network settings,
    /// with their homes in [`PLATFORM_TOOL_INHERITED`]. A CLI's install scripts
    /// and the npm-installed aimock are untrusted code, so no provider key
    /// reaches them.
    const TOOL_INHERITED: &[&str] = &[
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "UV_SYSTEM_CERTS",
    ];
    /// npm's and uv's homes, whose caches are `~/.npm` and `~/.cache/uv`, or
    /// `%LocalAppData%\npm-cache` and `%LOCALAPPDATA%\uv\cache` on Windows
    /// (docs.npmjs.com/cli/v11/using-npm/config, docs.astral.sh/uv/reference/storage).
    const PLATFORM_TOOL_INHERITED: PerOs<&[&str]> = PerOs {
        unix: &["HOME"],
        windows: &["USERPROFILE", "APPDATA", "LOCALAPPDATA"],
    };

    /// npm, which Windows installs as a batch file, spawned as one despite what
    /// [`Launch`] says of them: each argument here is a word this file writes
    /// (a subcommand, flag, field or the `latest` tag), a package name, which
    /// npm keeps URL-safe (docs.npmjs.com, package.json `name`), or a parsed
    /// [`Version`], none holding a character `cmd.exe` escapes.
    const NPM: &str = if cfg!(windows) { "npm.cmd" } else { "npm" };

    /// Whether `name` is on `list`, as `windows` compares variable names: there
    /// they ignore case, and a host may spell `PATH` as `Path`.
    fn listed(windows: bool, list: &[&str], name: &str) -> bool {
        list.iter().any(|k| {
            if windows {
                k.eq_ignore_ascii_case(name)
            } else {
                *k == name
            }
        })
    }

    /// Every variable a CLI inherits on this platform.
    fn inherited() -> impl Iterator<Item = &'static str> {
        INHERITED_EVERYWHERE
            .iter()
            .chain(PLATFORM_INHERITED.here().iter())
            .copied()
    }

    /// `program` with the host env cleared to what [`inherited`],
    /// [`TOOL_INHERITED`], [`PLATFORM_TOOL_INHERITED`] and npm's `npm_config_*`
    /// settings keep.
    fn tool(program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear().stdin(Stdio::null());
        let keeps: Vec<&str> = inherited()
            .chain(TOOL_INHERITED.iter().copied())
            .chain(PLATFORM_TOOL_INHERITED.here().iter().copied())
            .collect();
        for (k, v) in std::env::vars_os() {
            let keep = k.to_str().is_some_and(|k| {
                listed(cfg!(windows), &keeps, k)
                    || k.to_ascii_lowercase().starts_with("npm_config_")
            });
            if keep {
                c.env(k, v);
            }
        }
        c
    }

    #[derive(Clone, PartialEq, Eq, Debug)]
    enum Versions {
        Pinned,
        Latest,
        /// The release a bump planned, so a newer `latest` published since
        /// isn't the one recorded.
        Exact(Version),
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Args {
        versions: Versions,
        record: bool,
        /// The [`SHARDS`] entry `--shard` names, counting from 1, and the
        /// sources it alone runs.
        shard: Option<(usize, &'static [&'static str])>,
        source: Option<String>,
        scenario: Option<String>,
    }

    impl Args {
        /// Refuses what no run could honor: a selection that names nothing
        /// runnable is an operator's typo, not a run where everything was blocked.
        fn check(&self, windows: bool) -> Result<(), String> {
            if let Some(why) = CORPUS_REFUSAL.filter(|_| self.record) {
                return Err(format!("--record: {why}"));
            }
            if self.record && self.versions != Versions::Pinned && self.source.is_none() {
                return Err("--record off the pin moves one source's pin: name it".into());
            }
            if self.record && self.scenario.is_some() {
                return Err("--record writes a whole source: its census judges every run".into());
            }
            if self.shard.is_some() && self.source.is_some() {
                return Err("--shard selects its own sources: name none".into());
            }
            let Some(source) = self.source.as_deref() else {
                return Ok(());
            };
            if let Some((_, why)) = NOT_HERMETIC.iter().find(|(s, _)| *s == source) {
                return Err(format!("{source} has no hermetic profile: {why:?}"));
            }
            let p = hermetic::profile(source)
                .ok_or_else(|| format!("{source} is not a registered source"))?;
            let Some(sc) = self.scenario.as_deref() else {
                if p.runs_on(windows) {
                    return Ok(());
                }
                return Err(format!(
                    "{source} doesn't run here: {}",
                    not_run_of(p, windows).join(", ")
                ));
            };
            if sc == LIFECYCLE {
                return match lifecycle_turn(self, p, windows) {
                    Some(_) => Ok(()),
                    None => Err(format!("{source}/{LIFECYCLE} has no scenario to turn here")),
                };
            }
            if let (LIVE, Some(live)) = (sc, &p.live) {
                return match live.not_run_on(windows) {
                    Some(why) => Err(format!("{source}/{LIVE} doesn't run here: {why}")),
                    None => Ok(()),
                };
            }
            let s = p
                .scenario(sc)
                .ok_or_else(|| format!("{source} has no scenario {sc}"))?;
            match p.not_run_on(s, windows) {
                Some(why) => Err(format!("{source}/{sc} doesn't run here: {why}")),
                None => Ok(()),
            }
        }

        fn selects(&self, source: &str, scenario: &str) -> bool {
            self.selects_source(source) && self.scenario.as_deref().is_none_or(|s| s == scenario)
        }

        fn selects_source(&self, source: &str) -> bool {
            self.source.as_deref().is_none_or(|s| s == source)
                && self
                    .shard
                    .is_none_or(|(_, sources)| sources.contains(&source))
        }
    }

    const CLI_USAGE: &str = "--cli takes pinned, latest or a release";

    fn shard_usage() -> String {
        format!("--shard takes 1 to {}", SHARDS.len())
    }

    fn parse(mut rest: &[String]) -> Result<Args, String> {
        let (mut versions, mut record, mut shard) = (Versions::Pinned, false, None);
        loop {
            match rest {
                [flag, v, tail @ ..] if flag == "--cli" => {
                    versions = match v.as_str() {
                        "pinned" => Versions::Pinned,
                        "latest" => Versions::Latest,
                        release => Version::parse(release)
                            .map(Versions::Exact)
                            .map_err(|_| CLI_USAGE.to_string())?,
                    };
                    rest = tail;
                }
                [flag] if flag == "--cli" => return Err(CLI_USAGE.into()),
                [flag, n, tail @ ..] if flag == "--shard" => {
                    shard = Some(
                        n.parse::<usize>()
                            .ok()
                            .and_then(|n| Some((n, *SHARDS.get(n.checked_sub(1)?)?)))
                            .ok_or_else(shard_usage)?,
                    );
                    rest = tail;
                }
                [flag] if flag == "--shard" => return Err(shard_usage()),
                [flag, tail @ ..] if flag == "--record" => {
                    record = true;
                    rest = tail;
                }
                _ => break,
            }
        }
        match rest {
            [] | [_] | [_, _] => Ok(Args {
                versions,
                record,
                shard,
                source: rest.first().cloned(),
                scenario: rest.get(1).cloned(),
            }),
            _ => Err(format!("unexpected {:?}", &rest[2..])),
        }
    }

    /// Whether every selected run passed.
    pub(crate) fn main() -> std::io::Result<bool> {
        let args = match parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
            Ok(a) => a,
            Err(e) => {
                eprintln!(
                    "{e}\nusage: hermetic [--cli pinned|latest|<release>] [--record] [--shard <n>] [<source> [<scenario>]]"
                );
                std::process::exit(2);
            }
        };
        if let Err(e) = args.check(cfg!(windows)) {
            eprintln!("{e}");
            std::process::exit(2);
        }
        let tier = tier_dir()?;
        // Every invocation of this build shares the tier's runs, shim and installs.
        std::fs::create_dir_all(&tier)?;
        let _tier = hold_lock(&tier.join(TIER_LOCK))?;
        let runs = tier.join("runs");
        match std::fs::remove_dir_all(&runs) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::create_dir_all(&runs)?;
        let built = Built::find()?;
        let ctx = Ctx {
            shim_dir: warmed_shim_dir(&tier, &built.shim)?,
            record: args.record,
            built,
            aimock: Aimock::install(&tier)?,
            tier,
        };

        let (mut entries, mut moved) = (Vec::new(), Vec::new());
        for p in PROFILES {
            let (ran, now) = run_source(&ctx, &args, p, &runs)?;
            entries.extend(ran);
            moved.extend(now);
        }
        let report = report(
            &args.versions,
            args.shard.map(|(n, _)| n),
            &host_tool_versions(),
            &entries,
            &not_run(&args, cfg!(windows)),
        );
        std::fs::write(runs.join(REPORT), &report)?;
        std::fs::write(runs.join(bump::MOVED), serde_json::to_string(&moved)?)?;
        println!("\n{report}");
        Ok(entries.iter().all(|e| e.verdict.label() == Label::Pass))
    }

    /// `p`'s selected runs, then, once every scenario ran, its census, the
    /// recording `args` asks for, and the release it moved to.
    fn run_source(
        ctx: &Ctx,
        args: &Args,
        p: &Profile,
        runs: &Path,
    ) -> std::io::Result<(Vec<Entry>, Option<Moved>)> {
        let mut entries = match &p.live {
            Some(live) => run_live(ctx, args, p, live, runs)?,
            None => Vec::new(),
        };
        let windows = cfg!(windows);
        let selected = selected(args, p, windows);
        let lifecycle = lifecycle_turn(args, p, windows);
        if selected.is_empty() && lifecycle.is_none() {
            return Ok((entries, None));
        }
        // Once per source, so every scenario runs, and records, one release.
        let installed = resolve(p, &args.versions);
        // Worked out before any CLI runs, so a refusal writes nothing.
        let registry = match (&installed, args.record) {
            (Ok(v), true) => {
                let src = std::fs::read_to_string(pin::registry_path())?;
                pin::repinned(&src, p.descriptor(), v).map_err(std::io::Error::other)?
            }
            _ => None,
        };
        let lock = match (&installed, args.record, p.package) {
            (
                Ok(v),
                true,
                Package::UpdaterManifest {
                    manifest,
                    platforms,
                    ..
                },
            ) => Some(lock_of(committed_lock(p)?, manifest, platforms, v)?),
            _ => None,
        };
        let (mut judged, mut banners) = (Vec::new(), Vec::new());
        let mut scenarios = selected;
        if let Some(s) = lifecycle.filter(|s| !scenarios.iter().any(|x| x.fixture == s.fixture)) {
            scenarios.insert(0, s);
        }
        for scenario in scenarios {
            let dir = run_dir(runs, p, scenario);
            std::fs::create_dir_all(&dir)?;
            let key = format!("{}/{}", p.source, scenario.name);
            println!("== {key}");
            let ran = match &installed {
                Ok(v) => run_one(ctx, p, scenario, v, &dir),
                Err(e) => Err(std::io::Error::other(e.clone())),
            };
            let (banner, verdict, run) = match ran {
                Ok((manifest, run)) => {
                    let verdict = verify::verify(&dir)?;
                    let run = lifecycle_goes_on(&verdict, run);
                    (Some(manifest.capture.version), verdict, run)
                }
                Err(e) => (
                    None,
                    Verdict::new(vec![(Label::Blocked, e.to_string())]),
                    None,
                ),
            };
            if verdict.judged() {
                judged.push(dir);
            }
            banners.extend(banner.clone());
            entries.push(Entry::new(key, banner, verdict));
            if lifecycle.is_none_or(|l| l.fixture != scenario.fixture) {
                continue;
            }
            let dir = runs.join(p.source).join(LIFECYCLE);
            std::fs::create_dir_all(&dir)?;
            let key = format!("{}/{LIFECYCLE}", p.source);
            println!("== {key}");
            let verdict = match run {
                Some(run) => match run_lifecycle(ctx, p, scenario, &run, &dir) {
                    Ok(_) => verify::verify_lifecycle(&dir)?,
                    // The steps' record is judged though the turn after them failed.
                    Err(e) if dir.join(LIFECYCLE_RECORD).is_file() => {
                        let mut findings = verify::verify_lifecycle(&dir)?.findings().to_vec();
                        findings.push((Label::Blocked, e.to_string()));
                        Verdict::new(findings)
                    }
                    Err(e) => Verdict::new(vec![(Label::Blocked, e.to_string())]),
                },
                None => Verdict::new(vec![(
                    Label::Blocked,
                    format!("its {} run was not judged", scenario.name),
                )]),
            };
            entries.push(Entry::new(key, None, verdict));
        }
        if !judges_leg(p, &judged, windows) {
            return Ok((entries, None));
        }
        if judges_source(&judged, p.scenarios.len()) {
            let key = format!("{} (every scenario)", p.source);
            println!("== {key}");
            let verdict = verify::verify_source(p, &judged)?;
            entries.push(Entry::new(key, None, verdict));
        }
        let passed = entries.iter().all(|e| e.verdict.label() == Label::Pass);
        let mut moved = None;
        if let (Ok(installed), true) = (&installed, passed) {
            match pin::every_run_reported(installed, p.descriptor().version_hyphen, &banners) {
                Ok(()) => {
                    if args.record {
                        record(ctx, p, runs)?;
                        if let Some(text) = registry {
                            std::fs::write(pin::registry_path(), text)?;
                        }
                        if let (Some(lock), Some(path)) = (&lock, p.manifest_lock()) {
                            std::fs::write(path, serde_json::to_string_pretty(lock)? + "\n")?;
                        }
                    }
                    if pin::moves(p.descriptor(), installed) == Ok(true) {
                        moved = Some(Moved {
                            source: p.source.to_string(),
                            version: installed.to_string(),
                        });
                    }
                }
                Err(why) if args.record => entries.push(Entry::new(
                    format!("{} (recording)", p.source),
                    None,
                    Verdict::new(vec![(Label::Fail, why)]),
                )),
                Err(_) => {}
            }
        }
        Ok((entries, moved))
    }

    /// The name a [`Live`] run is selected and reported by.
    const LIVE: &str = "live";

    /// The live tier's own log, beside its run.
    const LIVE_LOG: &str = "live.log";

    /// `p`'s [`Live`] run, when `args` selects it and this leg runs it: its CLI
    /// installed as a scenario's is, then its tier driven against this build. It
    /// records no capture, so no pin moves on it
    /// (`a_recorded_capture_anchors_its_sources_verified_version`).
    fn run_live(
        ctx: &Ctx,
        args: &Args,
        p: &Profile,
        live: &Live,
        runs: &Path,
    ) -> std::io::Result<Vec<Entry>> {
        if !args.selects(p.source, LIVE) || live.not_run_on(cfg!(windows)).is_some() {
            return Ok(Vec::new());
        }
        let key = format!("{}/{LIVE}", p.source);
        println!("== {key}");
        let dir = runs.join(p.source).join(LIVE);
        std::fs::create_dir_all(&dir)?;
        let ran = match resolve(p, &args.versions) {
            Ok(v) => live_once(ctx, p, live, &v, &dir),
            Err(e) => Err(std::io::Error::other(e)),
        };
        let (banner, verdict) = match ran {
            Ok((banner, verdict)) => (Some(banner), verdict),
            Err(e) => (None, Verdict::new(vec![(Label::Blocked, e.to_string())])),
        };
        Ok(vec![Entry::new(key, banner, verdict)])
    }

    /// What `live`'s exit `code` says: [`LIVE_PREREQUISITE`] is a missing
    /// prerequisite, which no run of the build caused.
    fn live_verdict(live: &Live, code: Option<i32>) -> Verdict {
        match code {
            Some(0) => Verdict::new(vec![]),
            Some(LIVE_PREREQUISITE) => Verdict::new(vec![(
                Label::Blocked,
                format!("{} found a prerequisite missing ({LIVE_LOG})", live.script),
            )]),
            _ => Verdict::new(vec![(
                Label::Fail,
                format!("{} exited {code:?} ({LIVE_LOG})", live.script),
            )]),
        }
    }

    /// One run of `live`'s tier under the sandbox, on ports free now: what the
    /// CLI's `--version` answered, and what the tier's exit says.
    fn live_once(
        ctx: &Ctx,
        p: &Profile,
        live: &Live,
        version: &Version,
        dir: &Path,
    ) -> std::io::Result<(String, Verdict)> {
        // No model runs, so `{port}` names a port nothing listens on.
        let (ports, extra) = Ports::pick(live.ports)?;
        let (_, cli, sandbox) = sandboxed(ctx, p, version, ports)?;
        let banner = cli_version(&sandbox, p, &cli.launch)?;
        let status = sandbox
            .command("bash")
            .arg(bump::repo_root().join(live.script))
            .args(extra.iter().map(u16::to_string))
            .env(LIVE_PIXTUOID_ENV, &ctx.built.pixtuoid)
            .current_dir(sandbox.dir.path())
            .stdout(log(dir, LIVE_LOG)?)
            .stderr(log(dir, LIVE_LOG)?)
            .status()?;
        Ok((banner, live_verdict(live, status.code())))
    }

    /// `p`'s scenarios `args` selects that a leg on `windows` runs.
    fn selected<'a>(args: &Args, p: &'a Profile, windows: bool) -> Vec<&'a Scenario> {
        p.scenarios
            .iter()
            .filter(|s| args.selects(p.source, s.name))
            .filter(|s| p.not_run_on(s, windows).is_none())
            .collect()
    }

    /// The scenario `p`'s install lifecycle goes on from and turns under its
    /// last uninstall, when `args` selects the lifecycle. `--record` writes
    /// scenarios alone.
    fn lifecycle_turn<'a>(args: &Args, p: &'a Profile, windows: bool) -> Option<&'a Scenario> {
        if args.record || !args.selects(p.source, LIFECYCLE) {
            return None;
        }
        headless_first(
            p.scenarios
                .iter()
                .filter(|s| p.not_run_on(s, windows).is_none()),
        )
    }

    /// The sandbox a lifecycle goes on in, when its scenario's `verdict` was
    /// judged: a CLI that couldn't run its scenario says nothing of its
    /// uninstall.
    fn lifecycle_goes_on<T>(verdict: &Verdict, run: T) -> Option<T> {
        verdict.judged().then_some(run)
    }

    /// The first of `scenarios`, a headless one first, since a TUI's gate waits
    /// on a hook.
    fn headless_first<'a>(scenarios: impl Iterator<Item = &'a Scenario>) -> Option<&'a Scenario> {
        scenarios.min_by_key(|s| !matches!(s.surface, Surface::Headless))
    }

    /// Every run of the sources `args` selects that a leg on `windows` never
    /// makes, and why, as the report names it.
    fn not_run(args: &Args, windows: bool) -> Vec<String> {
        PROFILES
            .iter()
            .filter(|p| args.selects_source(p.source))
            .flat_map(|p| not_run_of(p, windows))
            .collect()
    }

    /// Each of `p`'s runs a leg on `windows` never makes, and why.
    fn not_run_of(p: &Profile, windows: bool) -> Vec<String> {
        let scenarios = p.scenarios.iter().filter_map(|s| {
            let why = p.not_run_on(s, windows)?;
            Some(format!("{}/{} ({why})", p.source, s.name))
        });
        let live = p.live.as_ref().and_then(|l| {
            let why = l.not_run_on(windows)?;
            Some(format!("{}/{LIVE} ({why})", p.source))
        });
        scenarios.chain(live).collect()
    }

    fn run_dir(runs: &Path, p: &Profile, scenario: &Scenario) -> PathBuf {
        runs.join(p.source).join(scenario.name)
    }

    /// Each of `p`'s runs under `runs`, written into its scenario's fixture by
    /// the recorder.
    fn record(ctx: &Ctx, p: &Profile, runs: &Path) -> std::io::Result<()> {
        for scenario in p.scenarios {
            let status = Command::new(&ctx.built.recorder)
                .arg(recorder_flag::PROMOTE)
                .arg(run_dir(runs, p, scenario))
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "recording {}/{} exited {status}",
                    p.source, scenario.name
                )));
            }
        }
        Ok(())
    }

    /// Whether a leg on `windows` may move `p` on its `judged` runs: once they
    /// are every scenario it runs, though only one that ran them all takes the
    /// census ([`judges_source`]).
    fn judges_leg(p: &Profile, judged: &[PathBuf], windows: bool) -> bool {
        let leg = p
            .scenarios
            .iter()
            .filter(|s| p.not_run_on(s, windows).is_none());
        judged.len() == leg.count()
    }

    /// Whether `verify_source` takes a source's `judged` runs: only when they are
    /// every one of its `scenarios`, since a subset, one selected or one Blocked,
    /// fails on a field only another carries.
    fn judges_source(judged: &[PathBuf], scenarios: usize) -> bool {
        judged.len() == scenarios
    }

    /// The run's report, beside the runs; its H1 is the tracking issue's title.
    const REPORT: &str = "report.md";

    /// One section of the report: one run, or a source's runs together.
    struct Entry {
        key: String,
        /// The CLI's `--version`, for one run.
        version: Option<String>,
        verdict: Verdict,
    }

    impl Entry {
        fn new(key: String, version: Option<String>, verdict: Verdict) -> Self {
            println!("   {:?}", verdict.label());
            Self {
                key,
                version,
                verdict,
            }
        }
    }

    /// The host's own tools a CLI runs, which the runner image supplies unpinned.
    const HOST_TOOLS: PerOs<&[&str]> = PerOs {
        unix: &["git"],
        windows: &["git", "pwsh"],
    };

    /// Each of [`HOST_TOOLS`]' `--version`, or that it has none.
    fn host_tool_versions() -> Vec<String> {
        HOST_TOOLS
            .here()
            .iter()
            .map(|t| {
                tool(t)
                    .arg("--version")
                    .output()
                    .ok()
                    .as_ref()
                    .and_then(version_answer)
                    .unwrap_or_else(|| format!("{t}: none"))
            })
            .collect()
    }

    fn report(
        versions: &Versions,
        shard: Option<usize>,
        host: &[String],
        entries: &[Entry],
        not_run: &[String],
    ) -> String {
        let which = match versions {
            Versions::Pinned => "verified",
            Versions::Latest => "latest",
            Versions::Exact(_) => "planned",
        };
        let labels = || entries.iter().map(|e| e.verdict.label());
        let title = match labels().max() {
            Some(worst) if labels().any(|l| l != Label::Blocked) => format!("{worst:?}"),
            _ => "NOTHING RAN".to_owned(),
        };
        let os = std::env::consts::OS;
        let shard = shard.map_or(String::new(), |n| format!(", shard {n}"));
        let mut out = format!(
            "# hermetic, {which} CLIs on {os}{shard}: {title}\n\nHost: {}\n",
            host.join(", ")
        );
        for e in entries {
            let version = e
                .version
                .as_deref()
                .map_or(String::new(), |v| format!(" ({v})"));
            out += &format!("\n## {}{version}: {:?}\n", e.key, e.verdict.label());
            for (label, finding) in e.verdict.findings() {
                out += &format!("- {label:?}: {finding}\n");
            }
        }
        out += "\nNot covered:";
        for (source, absence) in NOT_HERMETIC {
            out += &format!(" {source} ({absence:?})");
        }
        if !not_run.is_empty() {
            out += &format!("\nNot run here: {}", not_run.join(" "));
        }
        out + "\n"
    }

    /// Where installs and runs live: `hermetic/` in the target dir this example
    /// was built into, which holds `<profile>/examples/hermetic`.
    fn tier_dir() -> std::io::Result<PathBuf> {
        let me = std::env::current_exe()?;
        me.ancestors()
            .nth(3)
            .map(|target| target.join("hermetic"))
            .ok_or_else(|| std::io::Error::other("an example sits three below its target dir"))
    }

    /// The build under test, beside this example in its profile's directory:
    /// the binary, the shim and the recorder. Never a `pixtuoid` off PATH: a
    /// released one would pass with none of the change under test in it.
    struct Built {
        pixtuoid: PathBuf,
        shim: PathBuf,
        recorder: PathBuf,
        /// What runs a [`Surface::Tui`] scenario's CLI under a pty.
        driver: PathBuf,
    }

    impl Built {
        fn find() -> std::io::Result<Self> {
            let me = std::env::current_exe()?;
            let profile_dir = me
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| std::io::Error::other("an example sits two below its target dir"))?;
            let exe = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
            let found = Self {
                pixtuoid: profile_dir.join(exe("pixtuoid")),
                shim: profile_dir.join(exe("pixtuoid-hook")),
                recorder: me.with_file_name(exe("capture_fixture")),
                driver: me.with_file_name(exe("tui_drive")),
            };
            for p in [&found.pixtuoid, &found.shim, &found.recorder, &found.driver] {
                if !p.is_file() {
                    return Err(std::io::Error::other(format!(
                        "{} is not built — `just hermetic` builds it",
                        p.display()
                    )));
                }
            }
            Ok(found)
        }
    }

    /// The directory every sandbox's PATH names, holding the tier's one shim:
    /// Unix links the built one, and Windows, where a symlink needs privilege,
    /// copies it once. The shim runs once here, at no endpoint, so no run's first
    /// hook is a cold exec of a new file: a CLI lost its first payload behind one.
    fn warmed_shim_dir(tier: &Path, shim: &Path) -> std::io::Result<PathBuf> {
        let dir = tier.join("bin");
        match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::create_dir_all(&dir)?;
        let name = shim
            .file_name()
            .ok_or_else(|| std::io::Error::other("the shim has no file name"))?;
        let placed = dir.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(shim, &placed)?;
        #[cfg(windows)]
        std::fs::copy(shim, &placed)?;
        let mut warm = tool(&placed)
            .env(Endpoint::ENV, dir.join("nothing-listens"))
            .stdin(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = warm.stdin.take() {
            stdin.write_all(b"{}")?;
        }
        warm.wait()?;
        Ok(dir)
    }

    /// The scripted model, installed from the committed lockfile, scripts off.
    struct Aimock {
        launch: Launch,
        version: String,
    }

    impl Aimock {
        fn install(tier: &Path) -> std::io::Result<Self> {
            let src = hermetic::scripts_dir().join("aimock");
            let lock: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(src.join("package-lock.json"))?)?;
            let version = lock["packages"][format!("node_modules/{AIMOCK_PACKAGE}")]["version"]
                .as_str()
                .ok_or_else(|| std::io::Error::other("the aimock lockfile pins no aimock"))?
                .to_string();
            // `npm ci` every run: it installs exactly the lock.
            let dir = tier.join("aimock");
            std::fs::create_dir_all(&dir)?;
            for f in ["package.json", "package-lock.json"] {
                std::fs::copy(src.join(f), dir.join(f))?;
            }
            succeed(npm(&dir, &["ci", "--ignore-scripts"]))?;
            let modules = dir.join("node_modules");
            Ok(Self {
                launch: Launch::of(
                    &modules.join(AIMOCK_PACKAGE),
                    &modules.join(".bin"),
                    "llmock",
                )?,
                version,
            })
        }
    }

    /// Marks a finished install; written only after it succeeds.
    const INSTALL_DONE: &str = ".hermetic-installed";

    /// `install` into a fresh `dir` unless an earlier one finished there: an
    /// interrupted install, or one whose scripts npm blocked, leaves a binary
    /// stub behind, so the binary's presence proves nothing.
    fn install_once(
        dir: &Path,
        install: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        if dir.join(INSTALL_DONE).is_file() {
            return Ok(());
        }
        match std::fs::remove_dir_all(dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::create_dir_all(dir)?;
        install(dir)?;
        std::fs::write(dir.join(INSTALL_DONE), "")
    }

    fn npm(dir: &Path, args: &[&str]) -> Command {
        let mut c = tool(NPM);
        c.args(args)
            .args(["--no-audit", "--no-fund"])
            .current_dir(dir);
        c
    }

    /// How many times curl retries a transient failure.
    const CURL_RETRIES: &str = "3";

    /// What [`curl_get`] returns of a response.
    enum Curl {
        /// The headers, redirects unfollowed: a redirect's `location` is the answer.
        Headers,
        Body,
    }

    /// `url`'s headers or body; a failure names curl's own reason.
    fn curl_get(url: &str, part: Curl) -> std::io::Result<Vec<u8>> {
        let mode = match part {
            Curl::Headers => "-fsSI",
            Curl::Body => "-fsSL",
        };
        let out = tool(CURL)
            .args([mode, "--retry", CURL_RETRIES, url])
            .output()
            .map_err(|e| std::io::Error::other(format!("curl {url}: {e}")))?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "curl {url} exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }

    fn curl_download(url: &str, dest: &Path) -> std::io::Result<()> {
        let mut c = tool(CURL);
        c.args(["-fsSL", "--retry", CURL_RETRIES, "-o"])
            .arg(dest)
            .arg(url);
        succeed(c)
    }

    /// Run `c` to success. Its error names the program and arguments alone: a
    /// `Command`'s `Debug` prints its environment, npm's credentials included.
    fn succeed(mut c: Command) -> std::io::Result<()> {
        let status = c.status()?;
        if status.success() {
            return Ok(());
        }
        let args: Vec<_> = c.get_args().collect();
        Err(std::io::Error::other(format!(
            "{} {args:?} exited {status}",
            c.get_program().to_string_lossy()
        )))
    }

    /// `npm install <spec>` into `dir`. An install script writes where its
    /// CLI's home variables point: grok's extracts the binary into
    /// `$GROK_HOME/bin` and repoints the `grok` there
    /// (`crates/codegen/xai-grok-pager/npm/grok/bin/postinstall.js:26-27`), so
    /// each points into the install, never at the operator's.
    fn npm_install(p: &Profile, dir: &Path, spec: &str) -> Command {
        let mut c = npm(dir, &["install", spec]);
        for var in p.home_vars() {
            c.env(var, dir.join("homes").join(var));
        }
        c
    }

    /// The release `versions` names for `p`.
    fn resolve(p: &Profile, versions: &Versions) -> Result<Version, String> {
        match (versions, p.package) {
            (Versions::Pinned, _) => pin::pinned(p.descriptor()),
            (Versions::Exact(release), _) => Ok(release.clone()),
            (Versions::Latest, Package::Npm(package)) => {
                // A bare name reads npm's `tag` config, which an operator's
                // `.npmrc` can point at another dist-tag
                // (https://github.com/npm/cli/blob/83ea1a1c19648a6b4f5e52a498d8a6aaaf60e52b/lib/commands/view.js#L141-L145).
                let spec = format!("{package}@latest");
                let out = tool(NPM)
                    .args(["view", &spec, "version"])
                    .output()
                    .map_err(|e| format!("npm view {spec}: {e}"))?;
                npm_release(&spec, out.status, &out.stdout, &out.stderr)
            }
            (Versions::Latest, Package::GithubRelease { repo, .. } | Package::UvLocked(repo)) => {
                // It redirects to the latest release
                // (docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases).
                let url = format!("https://github.com/{repo}/releases/latest");
                let headers = curl_get(&url, Curl::Headers).map_err(|e| e.to_string())?;
                latest_tag(&String::from_utf8_lossy(&headers))
                    .ok_or_else(|| format!("{url} redirected to no release tag"))
            }
            (
                Versions::Latest,
                Package::UpdaterManifest {
                    manifest,
                    platforms,
                    ..
                },
            ) => {
                let here = asset_here(platforms).map_err(|e| e.to_string())?;
                let current = fetch_manifest(manifest, here).map_err(|e| e.to_string())?;
                Version::parse(&current.version)
                    .map_err(|_| format!("{here}'s manifest names {:?}", current.version))
            }
        }
    }

    /// One platform's build, as an updater manifest and a manifest lock name it.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Build {
        version: String,
        url: String,
        sha512: String,
    }

    /// `platform`'s build that the updater serves now.
    fn fetch_manifest(manifest: &str, platform: &str) -> std::io::Result<Build> {
        let url = manifest.replace("{platform}", platform);
        Ok(serde_json::from_slice(&curl_get(&url, Curl::Body)?)?)
    }

    /// The pinned build of every platform, as `--record` writes it.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct ManifestLock {
        version: String,
        platforms: std::collections::BTreeMap<String, Build>,
    }

    /// `p`'s committed manifest lock, if it has one.
    fn committed_lock(p: &Profile) -> std::io::Result<Option<ManifestLock>> {
        match p.manifest_lock().map(std::fs::read_to_string) {
            Some(Ok(text)) => Ok(Some(serde_json::from_str(&text)?)),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(None),
        }
    }

    /// The lock of `version`: `committed` when it pins it, since the updater
    /// serves only its newest build, else every platform's current manifest, each
    /// of which must serve `version`, so the pin installs on every leg.
    fn lock_of(
        committed: Option<ManifestLock>,
        manifest: &str,
        platforms: &[(&str, &str, &'static str)],
        version: &Version,
    ) -> std::io::Result<ManifestLock> {
        if let Some(lock) = committed.filter(|l| {
            l.version == version.to_string()
                && platforms
                    .iter()
                    .all(|(_, _, pf)| l.platforms.contains_key(*pf))
        }) {
            return Ok(lock);
        }
        let mut lock = ManifestLock {
            version: version.to_string(),
            platforms: std::collections::BTreeMap::new(),
        };
        for (_, _, platform) in platforms {
            let build = fetch_manifest(manifest, platform)?;
            if build.version != lock.version {
                return Err(std::io::Error::other(format!(
                    "{platform}'s manifest serves {}, not {version}",
                    build.version
                )));
            }
            lock.platforms.insert((*platform).to_string(), build);
        }
        Ok(lock)
    }

    /// `platform`'s build of `version`: the lock's when it pins that version,
    /// else the updater's when it serves it now.
    fn build_of(
        lock: Option<&ManifestLock>,
        manifest: &str,
        platform: &str,
        version: &Version,
    ) -> std::io::Result<Build> {
        let wanted = version.to_string();
        if let Some(build) = lock
            .filter(|l| l.version == wanted)
            .and_then(|l| l.platforms.get(platform))
        {
            return Ok(build.clone());
        }
        let current = fetch_manifest(manifest, platform)?;
        if current.version == wanted {
            return Ok(current);
        }
        Err(std::io::Error::other(format!(
            "no {platform} build of {version}: the lock pins {}, the updater serves {}",
            lock.map_or("none", |l| l.version.as_str()),
            current.version
        )))
    }

    /// Whether `bytes` hash to the lowercase hex SHA-512 `want`.
    fn matches_sha512(bytes: &[u8], want: &str) -> bool {
        digest::digest_hex::<sha2::Sha512>(bytes) == want
    }

    const CURL: &str = "curl";
    const GIT: &str = "git";
    const TAR: &str = "tar";
    const UV: &str = "uv";

    /// The release a `releases/latest` response's `location` names.
    fn latest_tag(headers: &str) -> Option<Version> {
        let location = headers.lines().find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("location").then(|| value.trim())
        })?;
        let tag = location.rsplit_once("/tag/")?.1;
        Version::parse(tag.strip_prefix('v')?).ok()
    }

    /// This platform's binary among a release's `assets`.
    fn asset_here(assets: &[(&str, &str, &'static str)]) -> std::io::Result<&'static str> {
        let here = (std::env::consts::OS, std::env::consts::ARCH);
        assets
            .iter()
            .find(|(os, arch, _)| (*os, *arch) == here)
            .map(|(_, _, asset)| *asset)
            .ok_or_else(|| std::io::Error::other(format!("no release asset for {here:?}")))
    }

    /// Downloads `asset` of `repo`'s `v<version>` release to `to`, holds it to
    /// the release's `sums` file, and makes it executable. A failure leaves no
    /// install marker, so [`install_once`] discards the download.
    fn fetch_release(
        repo: &str,
        version: &Version,
        asset: &str,
        sums: &str,
        to: &Path,
    ) -> std::io::Result<()> {
        let base = format!("https://github.com/{repo}/releases/download/v{version}");
        let fetch = |name: &str, dest: &Path| curl_download(&format!("{base}/{name}"), dest);
        let listed = to.with_file_name(sums);
        fetch(sums, &listed)?;
        fetch(asset, to)?;
        if !digest::vouches(
            &std::fs::read_to_string(&listed)?,
            asset,
            &std::fs::read(to)?,
        ) {
            return Err(std::io::Error::other(format!(
                "{asset} does not match v{version}'s {sums}"
            )));
        }
        make_executable(to)
    }

    fn make_executable(path: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    /// How many of npm's `npm error` lines a report carries: its code and
    /// summary, not the operator's log path that follows.
    const NPM_ERROR_LINES: usize = 2;

    /// The release `npm view <spec> version` answered, strict, or why there is
    /// none: an absent `latest` tag exits 0 with nothing on stdout
    /// (<https://github.com/npm/cli/blob/83ea1a1c19648a6b4f5e52a498d8a6aaaf60e52b/lib/commands/view.js#L190>).
    fn npm_release(
        spec: &str,
        status: ExitStatus,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Result<Version, String> {
        if !status.success() {
            let stderr = String::from_utf8_lossy(stderr);
            let why: Vec<&str> = stderr
                .lines()
                .filter(|l| l.starts_with("npm error"))
                .take(NPM_ERROR_LINES)
                .collect();
            return Err(match why.as_slice() {
                [] => format!("npm view {spec} exited {status}"),
                _ => format!("npm view {spec} failed: {}", why.join("; ")),
            });
        }
        let v = String::from_utf8_lossy(stdout).trim().to_string();
        if v.is_empty() {
            return Err(format!("npm gave no version for {spec}"));
        }
        Version::parse(&v).map_err(|_| format!("npm view {spec} gave {v:?}, not a release"))
    }

    /// The CLI at the version this run tests, installed into its own prefix the
    /// way users install it.
    fn install_cli(tier: &Path, p: &Profile, version: &Version) -> std::io::Result<Installed> {
        let prefix = tier.join("cli").join(format!("{}-{version}", p.source));
        let name = p.invocation[0];
        match p.package {
            // Its own install scripts run, since they link the binary: npm 12
            // blocks a dependency's unless the project's `allowScripts` names it
            // (`npm help install-scripts`), and older npm ignores the field.
            Package::Npm(package) => {
                let spec = format!("{package}@{version}");
                install_once(&prefix, |dir| {
                    let project = serde_json::json!({
                        "private": true,
                        "allowScripts": { &spec: true },
                    });
                    std::fs::write(dir.join("package.json"), project.to_string())?;
                    succeed(npm_install(p, dir, &spec))
                })?;
                let modules = prefix.join("node_modules");
                let bin = modules.join(".bin");
                Ok(Installed {
                    launch: Launch::of(&modules.join(package), &bin, name)?,
                    bin,
                })
            }
            Package::GithubRelease { repo, assets, sums } => {
                let program = prefix.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
                install_once(&prefix, |_| {
                    fetch_release(repo, version, asset_here(assets)?, sums, &program)
                })?;
                Ok(Installed {
                    launch: Launch {
                        program,
                        args: Vec::new(),
                        env: Vec::new(),
                    },
                    bin: prefix,
                })
            }
            Package::UvLocked(repo) => {
                let src = prefix.join("src");
                install_once(&prefix, |_| {
                    let mut clone = tool(GIT);
                    clone
                        .args(["clone", "--quiet", "--depth", "1", "--branch"])
                        .arg(format!("v{version}"))
                        .arg(format!("https://github.com/{repo}.git"))
                        .arg(&src);
                    succeed(clone)?;
                    // A workspace's members, a Python of uv's own at the tag's pin and
                    // no operator's uv config, as hermes's installer syncs
                    // (`pm/environment.py:406-409`, `scripts/install.sh:21` at v0.22.2);
                    // `UV_NO_CONFIG` is `--no-config`, which disables `.python-version`
                    // discovery too (docs.astral.sh/uv/concepts/python-versions).
                    let python = std::fs::read_to_string(src.join(".python-version"))?;
                    let mut sync = tool(UV);
                    sync.args(["sync", "--frozen", "--all-packages", "--managed-python"])
                        .args(["--python", python.trim()])
                        .env("UV_NO_CONFIG", "1")
                        .current_dir(&src);
                    succeed(sync)
                })?;
                let bin = src
                    .join(".venv")
                    .join(if cfg!(windows) { "Scripts" } else { "bin" });
                Ok(Installed {
                    launch: Launch {
                        program: bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
                        args: Vec::new(),
                        env: Vec::new(),
                    },
                    bin,
                })
            }
            // As agy's own installer does: the build's SHA-512 against the
            // manifest's, then its tarball's binary installed under the CLI's
            // name (antigravity.google/cli/install.sh, `:54`, `:191-196`, `:210`).
            Package::UpdaterManifest {
                manifest,
                platforms,
                member,
            } => {
                let program = prefix.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
                install_once(&prefix, |dir| {
                    let lock = committed_lock(p)?;
                    let build = build_of(lock.as_ref(), manifest, asset_here(platforms)?, version)?;
                    let payload = dir.join("payload");
                    curl_download(&build.url, &payload)?;
                    if !matches_sha512(&std::fs::read(&payload)?, &build.sha512) {
                        return Err(std::io::Error::other(format!(
                            "{} does not match its manifest's SHA-512",
                            build.url
                        )));
                    }
                    if build.url.ends_with(".tar.gz") {
                        let mut untar = tool(TAR);
                        untar
                            .arg("-xzf")
                            .arg(&payload)
                            .arg("-C")
                            .arg(dir)
                            .arg(member);
                        succeed(untar)?;
                        std::fs::rename(dir.join(member), &program)?;
                    } else {
                        std::fs::rename(&payload, &program)?;
                    }
                    make_executable(&program)
                })?;
                Ok(Installed {
                    launch: Launch {
                        program,
                        args: Vec::new(),
                        env: Vec::new(),
                    },
                    bin: prefix,
                })
            }
        }
    }

    /// An installed CLI: the directory holding its command, which the sandbox's
    /// PATH names, and how to start it.
    struct Installed {
        bin: PathBuf,
        launch: Launch,
    }

    /// How to start an installed command; on Windows not the `.cmd` npm
    /// writes. std runs a batch file's arguments through `cmd.exe` escaping,
    /// refuses at spawn any it "might not be possible to safely escape", and
    /// says not to rely on it (std::process, "Batch file special handling"):
    /// a CLI's free-text prompt is one. And std's `kill` is `TerminateProcess`,
    /// which ends only "the specified process" (learn.microsoft.com), so
    /// aimock's node would outlive a killed npm shim.
    #[derive(Debug, PartialEq, Eq)]
    struct Launch {
        program: PathBuf,
        args: Vec<String>,
        /// What the entry's shebang sets.
        env: Vec<(String, String)>,
    }

    impl Launch {
        /// `name` from the package at `package` npm linked into `bin`: on Unix
        /// the link itself, which the kernel runs by its shebang; on Windows
        /// what npm's shim there runs ([`Self::from_bin_field`]).
        fn of(package: &Path, bin: &Path, name: &str) -> std::io::Result<Self> {
            if cfg!(windows) {
                Self::from_bin_field(package, bin, name)
            } else {
                Ok(Self {
                    program: bin.join(name),
                    args: Vec::new(),
                    env: Vec::new(),
                })
            }
        }

        /// What npm's Windows shim runs for `name`, as cmd-shim writes it
        /// (`npm/cmd-shim` `lib/index.js:48-58,73-119` @v9.0.2): a `bin`
        /// entry with a shebang runs under its program (the `.exe` beside the
        /// shim if there is one) with its arguments and variables, and one
        /// without (a PE) runs itself.
        fn from_bin_field(package: &Path, bin: &Path, name: &str) -> std::io::Result<Self> {
            let manifest: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(package.join("package.json"))?)?;
            let rel = match &manifest["bin"] {
                serde_json::Value::String(only) => Some(only.as_str()),
                bins => bins[name].as_str(),
            }
            .ok_or_else(|| {
                std::io::Error::other(format!("{} has no bin {name}", package.display()))
            })?;
            let entry = package.join(rel);
            let text = String::from_utf8_lossy(&std::fs::read(&entry)?).into_owned();
            let first = text.trim().lines().next().unwrap_or_default();
            Ok(match Shebang::parse(first)? {
                None => Self {
                    program: entry,
                    args: Vec::new(),
                    env: Vec::new(),
                },
                Some(sb) => {
                    let beside = bin.join(format!("{}.exe", sb.program));
                    let mut args = sb.args;
                    args.push(entry.to_string_lossy().into_owned());
                    Self {
                        program: if beside.is_file() {
                            beside
                        } else {
                            PathBuf::from(sb.program)
                        },
                        args,
                        env: sb.env,
                    }
                }
            })
        }

        /// The command line, program first.
        fn argv(&self) -> Vec<OsString> {
            std::iter::once(self.program.clone().into_os_string())
                .chain(self.args.iter().map(OsString::from))
                .collect()
        }
    }

    /// cmd-shim's own `shebangExpr` (`lib/index.js:24` @v9.0.2).
    const SHEBANG_EXPR: &str =
        r"^#!\s*(?:/usr/bin/env\s+(?:-S\s+)?((?:[^ \t=]+=[^ \t=]+\s+)*))?([^ \t]+)(.*)$";

    /// A `#!` line as cmd-shim reads it: `/usr/bin/env`, an optional `-S`,
    /// leading `VAR=value` words, then the program and its arguments.
    #[derive(Debug, PartialEq, Eq)]
    struct Shebang {
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    }

    impl Shebang {
        fn parse(line: &str) -> std::io::Result<Option<Self>> {
            let expr = regex::Regex::new(SHEBANG_EXPR).map_err(std::io::Error::other)?;
            Ok(expr.captures(line).map(|c| Self {
                program: c[2].to_owned(),
                args: c[3].split_whitespace().map(str::to_owned).collect(),
                env: c
                    .get(1)
                    .map_or("", |vars| vars.as_str())
                    .split_whitespace()
                    .filter_map(|var| var.split_once('='))
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
            }))
        }
    }

    /// One sandbox: every home a CLI or pixtuoid resolves points inside it, so
    /// nothing reads or writes the operator's.
    struct Sandbox {
        dir: tempfile::TempDir,
        env: Vec<(String, OsString)>,
        /// Where the headless pixtuoid listens and the recorder tees to.
        scene: Endpoint,
    }

    /// Every home a CLI or pixtuoid may resolve, each a directory of the
    /// sandbox. All are set on every platform: a variable no reader there reads
    /// is inert.
    const SANDBOX_HOMES: &[(&str, &str)] = &[
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("APPDATA", "home/AppData/Roaming"),
        ("LOCALAPPDATA", "home/AppData/Local"),
        // Each at its default under the home: reasonix refuses to link a
        // plugin from outside it
        // (`internal/ext/installsource/plugin_package.go:480-481` at v2.39.0).
        ("XDG_CONFIG_HOME", "home/.config"),
        ("XDG_DATA_HOME", "home/.local/share"),
        ("XDG_STATE_HOME", "home/.local/state"),
        ("XDG_CACHE_HOME", "home/.cache"),
    ];

    impl Sandbox {
        fn new(
            p: &Profile,
            shim_dir: &Path,
            cli_bin: &Path,
            ports: Ports,
            workspace: &Path,
        ) -> std::io::Result<Self> {
            let dir = tempfile::tempdir()?;
            let at = |rel: &str| -> std::io::Result<PathBuf> {
                let d = dir.path().join(rel);
                std::fs::create_dir_all(&d)?;
                Ok(d)
            };
            let mut env: Vec<(String, OsString)> = inherited()
                .filter(|k| *k != "PATH")
                .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)))
                .collect();
            let host_path = std::env::var_os("PATH").unwrap_or_default();
            let path = std::env::join_paths(
                [shim_dir.to_path_buf(), cli_bin.to_path_buf()]
                    .into_iter()
                    .chain(std::env::split_paths(&host_path)),
            )
            .map_err(std::io::Error::other)?;
            env.push(("PATH".into(), path));
            for (var, rel) in SANDBOX_HOMES {
                env.push(((*var).into(), at(rel)?.into()));
            }
            for var in p.home_vars() {
                env.push((var.into(), at(&format!("homes/{var}"))?.into()));
            }
            let expand = |t: &str| expanded(t, ports);
            // Every child gets the profile's env, `connect`'s spawned CLI included;
            // the CLI under test also gets it as `--env` (`Sandbox::cli_env`).
            for (k, v) in p.env {
                env.push(((*k).to_string(), expand(v).into()));
            }
            let sandbox = Self {
                scene: Endpoint::in_dir(dir.path(), "scene"),
                dir,
                env,
            };
            for c in p.config {
                let home = sandbox.var(c.home).ok_or_else(|| {
                    std::io::Error::other(format!(
                        "{}: config home {} is not sandboxed",
                        p.source, c.home
                    ))
                })?;
                let file = home.join(c.path);
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(
                    &file,
                    config_text(c.template, ports, &reported_cwd(workspace)),
                )?;
            }
            Ok(sandbox)
        }

        /// The profile's pairs as this sandbox holds them, then what the CLI's
        /// launch sets, for the recorder to set on the CLI as [`recorder_flag::ENV`].
        fn cli_env(&self, p: &Profile, launch: &Launch) -> Vec<String> {
            p.env
                .iter()
                .filter_map(|(k, _)| {
                    self.var_os(k)
                        .map(|v| format!("{k}={}", v.to_string_lossy()))
                })
                .chain(launch.env.iter().map(|(k, v)| format!("{k}={v}")))
                .collect()
        }

        fn var(&self, key: &str) -> Option<PathBuf> {
            self.var_os(key).map(PathBuf::from)
        }

        fn var_os(&self, key: &str) -> Option<&OsString> {
            self.env
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
        }

        /// A child of the run. Its stdin is empty: `opencode run` reads a non-TTY
        /// stdin to its end (`packages/cli/src/run/run.ts:76`), so an inherited
        /// one that never closes hangs the run.
        fn command(&self, program: impl AsRef<OsStr>) -> Command {
            let mut c = Command::new(program);
            c.env_clear()
                .envs(self.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null());
            c
        }
    }

    /// A child killed when the run ends, however it ends.
    struct Reaped(Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// What every run in one invocation shares.
    struct Ctx {
        tier: PathBuf,
        built: Built,
        /// Whether its runs record, and so run in the one canonical workspace.
        record: bool,
        /// The directory holding the tier's one shim, on every sandbox's PATH.
        shim_dir: PathBuf,
        aimock: Aimock,
    }

    /// The file in the tier whose lock each invocation holds.
    const TIER_LOCK: &str = "hermetic.lock";

    /// A replay's own workspace root, removed when its run drops, waiting out
    /// a process still holding it as the recorder waits for the canonical one.
    struct ReplayRoot(PathBuf);

    impl Drop for ReplayRoot {
        fn drop(&mut self) {
            if let Err(e) =
                remove_once_released(RELEASE_PATIENCE, || std::fs::remove_dir_all(&self.0))
            {
                let _ = writeln!(
                    std::io::stderr(),
                    "hermetic: left {}: {e}",
                    self.0.display()
                );
            }
        }
    }

    /// The workspace a run's CLI works in: the canonical one when it records,
    /// whose path the fixtures carry, else one of its own under the same root.
    fn run_workspace(record: bool) -> std::io::Result<(Option<ReplayRoot>, PathBuf)> {
        if record {
            return Ok((None, capture_workspace()?));
        }
        let own = tempfile::Builder::new()
            .prefix("pixtuoid-run-")
            .tempdir_in(capture_root()?)?
            .keep();
        let workspace = own.join(capture_workspace_tail());
        Ok((Some(ReplayRoot(own)), workspace))
    }

    /// `p`'s CLI at `version` installed and sandboxed, with the workspace it
    /// runs in.
    fn sandboxed(
        ctx: &Ctx,
        p: &Profile,
        version: &Version,
        ports: Ports,
    ) -> std::io::Result<((PathBuf, Option<ReplayRoot>), Installed, Sandbox)> {
        let (own, workspace) = run_workspace(ctx.record)?;
        if let Some(found) = outside_config(
            p.system_config.here(),
            p.ancestor_config,
            &workspace,
            std::env::var_os("USER").as_deref().and_then(OsStr::to_str),
        ) {
            return Err(std::io::Error::other(format!(
                "{found} configures {} from outside the sandbox",
                p.source
            )));
        }
        let cli = install_cli(&ctx.tier, p, version)?;
        let sandbox = Sandbox::new(p, &ctx.shim_dir, &cli.bin, ports, &workspace)?;
        Ok(((workspace, own), cli, sandbox))
    }

    /// `scenario`'s run in a sandbox of its own, connected first, and the
    /// sandbox, which an install lifecycle goes on in.
    fn run_one(
        ctx: &Ctx,
        p: &Profile,
        scenario: &Scenario,
        version: &Version,
        dir: &Path,
    ) -> std::io::Result<(Manifest, Sandboxed)> {
        let (ports, _) = Ports::pick(0)?;
        let ((workspace, workspace_root), cli, sandbox) = sandboxed(ctx, p, version, ports)?;
        let connect = sandbox
            .command(&ctx.built.pixtuoid)
            .args(["connect", p.source, "--json"])
            .output()?;
        std::fs::write(dir.join("connect.log"), &connect.stdout)?;
        let rows: Vec<serde_json::Value> =
            serde_json::from_slice(&connect.stdout).unwrap_or_default();
        if !rows.iter().any(|r| r["outcome"] == Step::Connect.outcome()) {
            return Err(std::io::Error::other(format!(
                "pixtuoid connect {} did not connect: {}",
                p.source,
                String::from_utf8_lossy(&connect.stderr).trim()
            )));
        }
        let run = Sandboxed {
            port: ports.model,
            workspace,
            _workspace_root: workspace_root,
            cli,
            sandbox,
        };
        Ok((turn(ctx, p, scenario, &run, dir)?, run))
    }

    /// A sandbox `p`'s CLI is installed in, the workspace it runs in, and the
    /// port its scripted model listens on.
    struct Sandboxed {
        port: u16,
        workspace: PathBuf,
        _workspace_root: Option<ReplayRoot>,
        cli: Installed,
        sandbox: Sandbox,
    }

    /// `p`'s install lifecycle ([`STEPS`]) in `run`, the sandbox `scenario` ran
    /// in connected, so the CLI has run as a user's has; then `scenario`'s turn
    /// under its last uninstall, the steps' record and the turn's capture in
    /// `dir` (`verify::verify_lifecycle` judges them).
    fn run_lifecycle(
        ctx: &Ctx,
        p: &Profile,
        scenario: &Scenario,
        run: &Sandboxed,
        dir: &Path,
    ) -> std::io::Result<Manifest> {
        let sandbox = &run.sandbox;
        let pixtuoid = |args: &[&str]| -> std::io::Result<(Option<serde_json::Value>, String)> {
            let out = sandbox.command(&ctx.built.pixtuoid).args(args).output()?;
            let rows: Vec<serde_json::Value> =
                serde_json::from_slice(&out.stdout).unwrap_or_default();
            let row = rows.into_iter().find(|r| r["id"] == p.source);
            Ok((row, String::from_utf8_lossy(&out.stderr).trim().to_owned()))
        };
        let mut records = Vec::new();
        for step in STEPS {
            let (row, stderr) = pixtuoid(&[step.command(), p.source, "--json"])?;
            let field = |r: &Option<serde_json::Value>, k: &str| {
                r.as_ref().and_then(|r| r[k].as_str()).map(str::to_owned)
            };
            let health = match step {
                Step::Disconnect => None,
                Step::Connect => match pixtuoid(&["sources", "--json"])? {
                    (Some(r), _) if r["connected"] == true => field(&Some(r), "health"),
                    (Some(_), _) => Some("sources --json shows it disconnected".into()),
                    (None, e) => Some(format!("sources --json has no row for it: {e}")),
                },
            };
            records.push(StepRecord {
                step: *step,
                outcome: field(&row, "outcome"),
                message: field(&row, "message").or_else(|| (!stderr.is_empty()).then_some(stderr)),
                health,
            });
        }
        std::fs::write(
            dir.join(LIFECYCLE_RECORD),
            serde_json::to_string_pretty(&records)?,
        )?;
        turn(ctx, p, scenario, run, dir)
    }

    /// `scenario`'s turn in `run`'s sandbox, connected or not: the scripted
    /// model, a headless pixtuoid and the recorder, its capture and manifest in
    /// `dir`.
    fn turn(
        ctx: &Ctx,
        p: &Profile,
        scenario: &Scenario,
        run: &Sandboxed,
        dir: &Path,
    ) -> std::io::Result<Manifest> {
        let Ctx { built, aimock, .. } = ctx;
        let Sandboxed {
            port,
            workspace,
            cli,
            sandbox,
            ..
        } = run;
        let port = *port;
        let script = render_script(p, scenario, workspace, sandbox.dir.path())?;
        let mut model = tool(&aimock.launch.program);
        model
            .args(&aimock.launch.args)
            .envs(aimock.launch.env.iter().map(|(k, v)| (k, v)));
        let model = Reaped(
            model
                // Codex's subagent tools are namespaced Responses tools
                // (`codex-rs/core/src/tools/handlers/multi_agents/spawn.rs:31`
                // at rust-v0.163.0), which only this mode lets a script call
                // (`CopilotKit/aimock` `CHANGELOG.md`, 1.45.0).
                .args(["--responses-tools", "extended"])
                // A script's `turnIndex` is a rule, not a tiebreak (aimock
                // `README.md`, "Replay matching"): for a `result_unseen` CLI only
                // the assistant count tells its turns apart.
                .env("AIMOCK_STRICT_TURN_INDEX", "1")
                .args(["-p", &port.to_string(), "-f"])
                .arg(&script)
                .stdout(log(dir, AIMOCK_LOG)?)
                .stderr(log(dir, AIMOCK_LOG)?)
                .spawn()?,
        );
        wait_until("aimock", BOOT, || Ok(http_get(port, "/v1/models").is_ok()))?;

        // A root absent at boot is watched only from the next poll, so a short
        // run would reach pixtuoid by hook alone.
        let mut prepare = sandbox.command(&built.recorder);
        prepare
            .arg(recorder_flag::WITHIN)
            .arg(sandbox.dir.path())
            .args([recorder_flag::PREPARE_ROOT, p.source]);
        succeed(prepare)?;

        let scene = Reaped(
            sandbox
                .command(&built.pixtuoid)
                .args(["run", "--headless", "--log-level", "warn", "--socket"])
                .arg(sandbox.scene.path())
                // Its log colors a file too, and the scene log is read as text.
                .env("NO_COLOR", "1")
                .stdout(log(dir, SCENE_LOG)?)
                .stderr(log(dir, SCENE_LOG)?)
                .spawn()?,
        );
        wait_until("the headless pixtuoid's endpoint", BOOT, || {
            Ok(sandbox.scene.accepting())
        })?;

        let recorder = sandbox
            .command(&built.recorder)
            .args(recorder_args(
                &RecorderRun {
                    out: dir,
                    scene: sandbox.scene.path(),
                    within: sandbox.dir.path(),
                    workspace,
                    cli_env: &sandbox.cli_env(p, &cli.launch),
                    cli_version: &cli_version(sandbox, p, &cli.launch)?,
                    launch: &cli.launch,
                    driver: &built.driver,
                },
                p,
                scenario,
            ))
            .stdout(log(dir, RECORDER_LOG)?)
            .stderr(log(dir, RECORDER_LOG)?)
            .status()?;
        std::thread::sleep(SCENE_SETTLE);
        drop(scene);

        let journal: serde_json::Value =
            serde_json::from_str(&http_get(port, "/__aimock/journal")?)?;
        drop(model);
        std::fs::write(
            dir.join("aimock-journal.json"),
            serde_json::to_string_pretty(&journal)?,
        )?;
        let entries = journal.as_array().cloned().unwrap_or_default();
        let (requests, unmatched) = model_requests(&entries);
        let results_seen = results_seen(&entries);

        // The recorder writes its record whenever the CLI ran, so a missing
        // one means the CLI never did; it fails past one only on a capture it
        // could not complete, which its log's last line names.
        let capture: CaptureRecord = match std::fs::read_to_string(dir.join(CAPTURE_RECORD)) {
            Ok(text) if recorder.success() => serde_json::from_str(&text)?,
            read => {
                let log = std::fs::read_to_string(dir.join(RECORDER_LOG)).unwrap_or_default();
                let last = log
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default();
                let relation = if read.is_ok() { "after" } else { "with no" };
                return Err(std::io::Error::other(format!(
                    "the recorder exited {recorder} {relation} {CAPTURE_RECORD}: {last}"
                )));
            }
        };
        let manifest = Manifest {
            source: p.source.into(),
            scenario: scenario.name.into(),
            capture,
            aimock: aimock.version.clone(),
            requests,
            unmatched,
            results_seen,
        };
        std::fs::write(dir.join(MANIFEST), serde_json::to_string_pretty(&manifest)?)?;
        Ok(manifest)
    }

    /// Config a CLI would read that no sandbox home redirects: one of
    /// `system_config`'s places, its [`USER_PLACEHOLDER`] the operator's
    /// (unchecked with none), or one of `ancestor_config` in an ancestor of
    /// `workspace`, which need not exist yet. An ancestor may be a symlink (macOS's `/tmp`), so
    /// the ancestors of the nearest existing one's target count too.
    fn outside_config(
        system_config: &[HostConfig],
        ancestor_config: &[&str],
        workspace: &Path,
        user: Option<&str>,
    ) -> Option<String> {
        let lexical = workspace.ancestors().skip(1);
        let real = lexical
            .clone()
            .find_map(|a| std::fs::canonicalize(a).ok())
            .into_iter()
            .flat_map(|r| r.ancestors().map(Path::to_path_buf).collect::<Vec<_>>());
        let ancestors: Vec<PathBuf> = lexical.map(Path::to_path_buf).chain(real).collect();
        let inherited = ancestors
            .iter()
            .flat_map(|dir| ancestor_config.iter().map(move |name| dir.join(name)))
            .find(|path| path.exists())
            .map(|path| path.display().to_string());
        system_config
            .iter()
            .find_map(|row| host_config_found(row, user))
            .or(inherited)
    }

    /// `row`, as found on this host.
    fn host_config_found(row: &HostConfig, user: Option<&str>) -> Option<String> {
        match row {
            HostConfig::Path(path) => {
                let path = match user {
                    _ if !path.contains(USER_PLACEHOLDER) => (*path).to_owned(),
                    Some(name) => path.replace(USER_PLACEHOLDER, name),
                    None => return None,
                };
                let path = pixtuoid_core::harness::host::known_folders_expanded(&path)?;
                path.exists().then(|| path.display().to_string())
            }
            #[cfg(windows)]
            HostConfig::Registry(key) => {
                pixtuoid_core::harness::host::registry_key_exists(key).then(|| (*key).to_owned())
            }
            #[cfg(not(windows))]
            HostConfig::Registry(_) => None,
        }
    }

    /// The CLI's `--version`, asked of it alone in the sandbox the run uses:
    /// the recorder cannot repeat a launch through an interpreter.
    fn cli_version(sandbox: &Sandbox, p: &Profile, launch: &Launch) -> std::io::Result<String> {
        let probe = required_version_probe(p.descriptor())?;
        let out = sandbox
            .command(&launch.program)
            .args(&launch.args)
            .args(&probe[1..])
            .envs(launch.env.iter().map(|(k, v)| (k, v)))
            .output()?;
        version_answer(&out).ok_or_else(|| {
            std::io::Error::other(format!(
                "{:?} {:?} answered nothing (exit {})",
                launch.program,
                &probe[1..],
                out.status
            ))
        })
    }

    /// What the tier decides about one recorder run.
    struct RecorderRun<'a> {
        /// The raw capture goes here.
        out: &'a Path,
        /// Teed to the headless scene listening here.
        scene: &'a Path,
        /// Its harvest held inside this sandbox.
        within: &'a Path,
        /// The workspace the CLI runs in, as this tier resolved it.
        workspace: &'a Path,
        /// Set on the CLI after the recorder's scrub.
        cli_env: &'a [String],
        cli_version: &'a str,
        launch: &'a Launch,
        /// [`Built::driver`].
        driver: &'a Path,
    }

    /// The recorder's command line for one run: its options, then the source,
    /// the scenario and the CLI's own launch and invocation.
    fn recorder_args(run: &RecorderRun<'_>, p: &Profile, scenario: &Scenario) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            recorder_flag::OUT.into(),
            run.out.into(),
            recorder_flag::TEE.into(),
            run.scene.into(),
            recorder_flag::WITHIN.into(),
            run.within.into(),
            recorder_flag::CLI_VERSION.into(),
            run.cli_version.into(),
            recorder_flag::WORKSPACE.into(),
            run.workspace.into(),
        ];
        for kv in run.cli_env {
            args.extend([recorder_flag::ENV.into(), kv.into()]);
        }
        if let Some(daemon) = &p.daemon {
            for (flag, words) in [
                (recorder_flag::DAEMON, daemon.start),
                (recorder_flag::DAEMON_READY, daemon.ready),
            ] {
                let words = words.iter().map(OsString::from);
                for word in run.launch.argv().into_iter().chain(words) {
                    args.extend([flag.into(), word]);
                }
            }
        }
        args.extend([p.source, scenario.name].map(OsString::from));
        match scenario.surface {
            Surface::Headless => {
                args.extend(run.launch.argv());
                args.extend(p.invocation[1..].iter().map(OsString::from));
            }
            Surface::Tui(tui) => {
                args.push(run.driver.into());
                args.extend(
                    tui::CommandLine {
                        source: p.source.to_owned(),
                        scenario: scenario.name.to_owned(),
                        log: run.out.join(TUI_LOG),
                        // The recorder expands it, as in a headless invocation.
                        prompt: "{prompt}".to_owned(),
                        program: run
                            .launch
                            .argv()
                            .into_iter()
                            .chain(tui.args().iter().map(OsString::from))
                            .collect(),
                    }
                    .to_args(),
                );
            }
        }
        args
    }

    /// The requests aimock answered and those no script line matched, every
    /// GET aside: a GET is a model catalog's, which no script answers (hermes
    /// probes LM Studio's `/api/v1/models`, `agent/model_metadata.py:745` at
    /// v0.22.2).
    fn model_requests(journal: &[serde_json::Value]) -> (usize, usize) {
        let requests: Vec<_> = journal.iter().filter(|e| e["method"] != "GET").collect();
        let unmatched = requests
            .iter()
            .filter(|e| e["response"]["fixture"].is_null())
            .count();
        (requests.len(), unmatched)
    }

    /// The requests whose messages, as aimock normalized them, carry a tool result.
    fn results_seen(journal: &[serde_json::Value]) -> usize {
        journal
            .iter()
            .filter(|e| {
                e["body"]["messages"]
                    .as_array()
                    .is_some_and(|m| m.iter().any(|m| m["role"] == "tool"))
            })
            .count()
    }

    /// The scenario's script with its placeholders expanded in every string,
    /// written into `into`: a CLI that takes an absolute path gets the one the
    /// recorder runs it in, as its own cwd reads it, and a reply waits for the
    /// note the tool read.
    fn render_script(
        p: &Profile,
        scenario: &Scenario,
        workspace: &Path,
        into: &Path,
    ) -> std::io::Result<PathBuf> {
        let mut script: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(scenario.script(p.source))?)?;
        expand_strings(&mut script, &p.script_values(&reported_cwd(workspace)));
        let out = into.join("script.json");
        std::fs::write(&out, serde_json::to_string_pretty(&script)?)?;
        Ok(out)
    }

    /// A [`ConfigFile`](pixtuoid_core::harness::hermetic::ConfigFile)'s text
    /// with its placeholders expanded.
    fn config_text(template: &str, ports: Ports, workspace: &str) -> String {
        expanded(template, ports)
            .replace(WORKSPACE_SLASHED_PLACEHOLDER, &workspace.replace('\\', "/"))
            .replace(WORKSPACE_PLACEHOLDER, workspace)
    }

    /// A profile string with its run's ports and the dummy key in it.
    fn expanded(template: &str, ports: Ports) -> String {
        template
            .replace("{port}", &ports.model.to_string())
            .replace(DAEMON_PORT_PLACEHOLDER, &ports.daemon.to_string())
            .replace("{key}", DUMMY_KEY)
    }

    /// `workspace` as a CLI's `getcwd` reports it: Unix's realpath (macOS's
    /// `/tmp` is `/private/tmp`); Windows' cwd reads back as it was set, and
    /// canonicalizing would resolve a junction it never sees. The workspace may
    /// not exist yet (a config seeded before the recorder makes it), so its
    /// nearest existing ancestor is the part resolved.
    fn reported_cwd(workspace: &Path) -> String {
        let workspace = if cfg!(windows) {
            workspace.to_path_buf()
        } else {
            workspace
                .ancestors()
                .find_map(|have| {
                    let real = std::fs::canonicalize(have).ok()?;
                    let tail = workspace.strip_prefix(have).ok()?;
                    // `join("")` would end the path in a separator.
                    Some(if tail.as_os_str().is_empty() {
                        real
                    } else {
                        real.join(tail)
                    })
                })
                .unwrap_or_else(|| workspace.to_path_buf())
        };
        workspace.to_string_lossy().into_owned()
    }

    /// Replace every placeholder inside `v`'s strings: a raw `C:\…` spliced
    /// into JSON text would be an invalid escape.
    fn expand_strings(v: &mut serde_json::Value, values: &[(&str, String)]) {
        match v {
            serde_json::Value::String(text) => {
                for (k, value) in values {
                    *text = text.replace(k, value);
                }
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(|i| expand_strings(i, values));
            }
            serde_json::Value::Object(map) => {
                map.values_mut().for_each(|i| expand_strings(i, values));
            }
            _ => {}
        }
    }

    /// One child's stdout and stderr, interleaved in one file of the run.
    fn log(dir: &Path, name: &str) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
    }

    /// The ports one run uses, each free when picked: the scripted model's, which
    /// `{port}` names, and a daemon's, which [`DAEMON_PORT_PLACEHOLDER`] names.
    #[derive(Debug, Clone, Copy)]
    struct Ports {
        model: u16,
        daemon: u16,
    }

    impl Ports {
        /// A run's ports, and `extra` more, all free now.
        fn pick(extra: usize) -> std::io::Result<(Self, Vec<u16>)> {
            let held = free_ports(2 + extra)?;
            let (run, rest) = held.split_at(2);
            let ports = Self {
                model: run[0],
                daemon: run[1],
            };
            Ok((ports, rest.to_vec()))
        }
    }

    /// `n` ports free now, held together while picked so no two are one.
    fn free_ports(n: usize) -> std::io::Result<Vec<u16>> {
        let held = (0..n)
            .map(|_| TcpListener::bind("127.0.0.1:0"))
            .collect::<std::io::Result<Vec<_>>>()?;
        held.iter().map(|l| Ok(l.local_addr()?.port())).collect()
    }

    /// A bare HTTP/1.0 GET to aimock on loopback: the core crate carries no
    /// HTTP client, and this is the whole of the protocol the tier needs.
    fn http_get(port: u16, path: &str) -> std::io::Result<String> {
        let mut s = TcpStream::connect(("127.0.0.1", port))?;
        s.set_read_timeout(Some(HTTP_READ_TIMEOUT))?;
        write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;
        let mut raw = String::new();
        s.read_to_string(&mut raw)?;
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
            return Err(std::io::Error::other(format!(
                "GET {path}: {}",
                head.lines().next().unwrap_or("")
            )));
        }
        Ok(body.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const TEST_PORTS: Ports = Ports {
            model: 4321,
            daemon: 4322,
        };

        fn args(v: &[&str]) -> Vec<String> {
            v.iter().map(|s| (*s).to_string()).collect()
        }

        #[test]
        fn the_versions_flag_leads_and_the_rest_selects() {
            let a = parse(&args(&["--cli", "latest", "codex", "tool-run"])).expect("parse");
            assert_eq!(a.versions, Versions::Latest);
            assert!(a.selects("codex", "tool-run"));
            assert!(!a.selects("codex", "other"));
            assert!(!a.selects("opencode", "tool-run"));
            let a = parse(&[]).expect("parse");
            assert_eq!(a.versions, Versions::Pinned);
            assert!(a.selects("anything", "at-all"));
            assert!(parse(&args(&["--cli"])).is_err());
            assert!(parse(&args(&["--cli", "newest"])).is_err());
            let a = parse(&args(&["--cli", "1.2.3", "codex"])).expect("parse");
            assert_eq!(a.versions, Versions::Exact(Version::new(1, 2, 3)));
            assert!(
                parse(&args(&["--cli", "1.x"])).is_err(),
                "a release, never a range"
            );
            assert!(parse(&args(&["codex", "tool-run", "extra"])).is_err());
        }

        #[test]
        fn a_shard_runs_its_own_sources_and_names_only_their_absences() {
            let shard = |n: usize| parse(&args(&["--shard", &n.to_string()])).expect("parse");
            for (i, sources) in SHARDS.iter().enumerate() {
                let a = shard(i + 1);
                assert_eq!(a.check(false), Ok(()));
                assert_eq!(
                    a.shard.map(|(n, _)| n),
                    Some(i + 1),
                    "--shard counts from 1"
                );
                for p in PROFILES {
                    assert_eq!(
                        a.selects(p.source, "tool-run"),
                        sources.contains(&p.source),
                        "{} in shard {}",
                        p.source,
                        i + 1
                    );
                }
                let named = not_run(&a, true);
                let every = not_run(&parse(&[]).expect("parse"), true);
                let own: Vec<&String> = every
                    .iter()
                    .filter(|r| sources.iter().any(|s| r.starts_with(&format!("{s}/"))))
                    .collect();
                assert_eq!(named.iter().collect::<Vec<_>>(), own);
            }
            for bad in ["0", &(SHARDS.len() + 1).to_string(), "one"] {
                assert_eq!(parse(&args(&["--shard", bad])), Err(shard_usage()), "{bad}");
            }
            assert_eq!(parse(&args(&["--shard"])), Err(shard_usage()));
            let named = parse(&args(&["--shard", "1", "codex"])).expect("parse");
            assert!(named.check(false).is_err(), "a shard and a source");
        }

        #[test]
        fn a_live_tier_s_exit_reads_pass_blocked_or_fail() {
            let Some(live) = &hermetic::profile("openclaw").expect("openclaw").live else {
                panic!("openclaw runs live");
            };
            let label = |code| live_verdict(live, code).label();
            assert_eq!(label(Some(0)), Label::Pass);
            assert_eq!(label(Some(LIVE_PREREQUISITE)), Label::Blocked);
            assert_eq!(label(Some(1)), Label::Fail);
            assert_eq!(label(None), Label::Fail, "a signal");
        }

        #[test]
        fn a_windows_leg_runs_what_renders_under_conpty_and_names_the_rest() {
            let grok = hermetic::profile("grok").expect("grok's profile");
            let every = parse(&args(&[])).expect("parse");
            let names = |windows| -> Vec<&str> {
                selected(&every, grok, windows)
                    .iter()
                    .map(|s| s.name)
                    .collect()
            };
            assert_eq!(
                names(true),
                ["tool-run", "tool-error", "subagent", "model-error"]
            );
            assert_eq!(
                names(false),
                [
                    "tool-run",
                    "tool-error",
                    "subagent",
                    "permission",
                    "model-error"
                ]
            );
            assert!(not_run(&every, false).is_empty());
            let r = report(&Versions::Pinned, None, &[], &[], &not_run(&every, true));
            assert!(
                r.ends_with(
                    "\nNot run here: grok/permission (its TUI paints nothing under ConPTY) \
                     openclaw/tool-run (the recorder stops its daemon by a Unix signal) \
                     openclaw/model-error (the recorder stops its daemon by a Unix signal) \
                     openclaw/live (its tier waits on a Unix socket)\n"
                ),
                "{r}"
            );
            assert!(!report(&Versions::Pinned, None, &[], &[], &[]).contains("Not run"));
            let judged = |n| vec![PathBuf::new(); n];
            assert!(judges_leg(grok, &judged(4), true), "grok moves on Windows");
            assert!(!judges_leg(grok, &judged(4), false));
            assert!(judges_leg(grok, &judged(5), false));
        }

        #[test]
        fn the_reports_title_is_the_worst_verdict_or_that_nothing_ran() {
            let os = std::env::consts::OS;
            let host = ["git version 2.55.0".to_string()];
            let pass = Entry::new(
                "codex/tool-run".into(),
                Some("codex-cli 0.155.1".into()),
                Verdict::new(vec![]),
            );
            let blocked = || {
                Entry::new(
                    "opencode/tool-run".into(),
                    None,
                    Verdict::new(vec![(Label::Blocked, "npm view failed".into())]),
                )
            };
            let r = report(&Versions::Latest, None, &host, &[pass, blocked()], &[]);
            assert_eq!(
                r.lines().next(),
                Some(format!("# hermetic, latest CLIs on {os}: Blocked").as_str())
            );
            assert!(r.contains("Host: git version 2.55.0"), "{r}");
            assert!(r.contains("## opencode/tool-run: Blocked\n- Blocked: npm view failed"));
            assert!(r.contains("## codex/tool-run (codex-cli 0.155.1): Pass"));
            let r = report(&Versions::Pinned, None, &host, &[blocked()], &[]);
            assert_eq!(
                r.lines().next(),
                Some(format!("# hermetic, verified CLIs on {os}: NOTHING RAN").as_str())
            );
            // The script lost a CLI that then exited: it ran, and its drift is the title.
            let lost = Entry::new(
                "codex/tool-run".into(),
                None,
                Verdict::new(vec![
                    (Label::Blocked, "exit 1".into()),
                    (Label::Script, "1 unmatched".into()),
                ]),
            );
            let r = report(&Versions::Latest, None, &host, &[lost, blocked()], &[]);
            assert_eq!(
                r.lines().next(),
                Some(format!("# hermetic, latest CLIs on {os}: Script").as_str())
            );
            // A shard's leg names it, so the issue tells two legs of one OS apart.
            let r = report(&Versions::Latest, Some(2), &host, &[blocked()], &[]);
            assert_eq!(
                r.lines().next(),
                Some(format!("# hermetic, latest CLIs on {os}, shard 2: NOTHING RAN").as_str())
            );
        }

        #[cfg(unix)]
        #[test]
        fn config_outside_the_sandbox_is_found_through_a_symlinked_ancestor() {
            let d = tempfile::tempdir().expect("tempdir");
            let real = d.path().join("real/sub");
            std::fs::create_dir_all(&real).expect("mkdir");
            std::os::unix::fs::symlink(&real, d.path().join("alias")).expect("symlink");
            // The workspace and its parent don't exist yet, as on a clean runner.
            let workspace = d.path().join("alias/capture/proj");
            let names = &["hermetic-probe.md"];
            assert_eq!(outside_config(&[], names, &workspace, None), None);
            let planted = d.path().join("real/hermetic-probe.md");
            std::fs::write(&planted, "").expect("plant");
            let found = outside_config(&[], names, &workspace, None).expect("found");
            assert_eq!(
                std::fs::canonicalize(found).ok(),
                std::fs::canonicalize(&planted).ok()
            );
        }

        /// A profile's rows are `'static`; a test's temp path lives for the run.
        fn path_row(p: &Path) -> HostConfig {
            HostConfig::Path(Box::leak(p.to_string_lossy().into_owned().into_boxed_str()))
        }

        #[test]
        fn a_listed_system_path_is_config_outside_the_sandbox_once_it_exists() {
            let d = tempfile::tempdir().expect("tempdir");
            let workspace = d.path().join("proj");
            let system = d.path().join("etc-cli");
            let listed = [path_row(&system)];
            assert_eq!(
                outside_config(&listed, &[], &workspace, None),
                None,
                "listed but absent"
            );
            std::fs::create_dir(&system).expect("mkdir");
            assert_eq!(
                outside_config(&listed, &[], &workspace, None),
                Some(system.display().to_string())
            );
        }

        #[test]
        fn a_per_user_system_path_is_the_operators_and_unchecked_with_no_user() {
            let d = tempfile::tempdir().expect("tempdir");
            let workspace = d.path().join("proj");
            let per_user = d.path().join("prefs").join("alice").join("managed.plist");
            let pattern = d
                .path()
                .join("prefs")
                .join(USER_PLACEHOLDER)
                .join("managed.plist");
            // The placeholder kept literal, and expanded to nothing: what the
            // no-user arm would find if it checked anything.
            for planted in [&per_user, &pattern, &d.path().join("prefs/managed.plist")] {
                std::fs::create_dir_all(planted.parent().expect("parent")).expect("mkdir");
                std::fs::write(planted, "").expect("plant");
            }
            let rows = [path_row(&pattern)];
            assert_eq!(
                outside_config(&rows, &[], &workspace, Some("alice")),
                Some(per_user.display().to_string())
            );
            assert_eq!(outside_config(&rows, &[], &workspace, Some("bob")), None);
            assert_eq!(
                outside_config(&rows, &[], &workspace, None),
                None,
                "no user: unchecked"
            );
        }

        #[cfg(windows)]
        #[test]
        fn a_registry_row_is_config_outside_the_sandbox_while_its_key_exists() {
            let present = HostConfig::Registry(r"HKCU\Software");
            assert_eq!(
                host_config_found(&present, None).as_deref(),
                Some(r"HKCU\Software")
            );
            let absent = HostConfig::Registry(r"HKCU\Software\pixtuoid-absent-row");
            assert_eq!(host_config_found(&absent, None), None);
        }

        #[test]
        fn the_recorder_is_held_to_the_sandbox_and_handed_the_clis_env() {
            let p = &PROFILES[0];
            let launch = Launch {
                program: PathBuf::from("/cli/node"),
                args: vec!["/cli/entry.js".into()],
                env: Vec::new(),
            };
            let args = recorder_args(
                &RecorderRun {
                    out: Path::new("/runs/x"),
                    scene: Path::new("/sb/scene.sock"),
                    within: Path::new("/sb"),
                    workspace: Path::new("/ws/proj"),
                    cli_env: &["A=b".to_string()],
                    cli_version: "1.2.3",
                    launch: &launch,
                    driver: Path::new("/build/tui_drive"),
                },
                p,
                &p.scenarios[0],
            );
            let pair =
                |flag: &str, value: &str| args.windows(2).any(|w| w[0] == *flag && w[1] == *value);
            assert!(pair("--within", "/sb"), "{args:?}");
            assert!(pair("--env", "A=b"), "{args:?}");
            assert!(pair("--tee", "/sb/scene.sock"), "{args:?}");
            assert!(pair("--out", "/runs/x"), "{args:?}");
            assert!(pair("--cli-version", "1.2.3"), "{args:?}");
            assert!(pair("--workspace", "/ws/proj"), "{args:?}");
            // The recorder reads options only before its positionals.
            let source = args.iter().position(|a| a == p.source).expect("the source");
            assert!(
                args[..source]
                    .chunks(2)
                    .all(|kv| kv[0].to_str().is_some_and(|f| f.starts_with("--"))),
                "{args:?}"
            );
            let positionals: Vec<OsString> = [p.source, p.scenarios[0].name]
                .into_iter()
                .map(OsString::from)
                .chain(launch.argv())
                .chain(p.invocation[1..].iter().map(OsString::from))
                .collect();
            assert_eq!(args[source..], positionals[..], "{args:?}");
        }

        #[test]
        fn a_daemon_profile_hands_the_recorder_its_daemon_and_its_config_the_held_port() {
            let p = hermetic::profile("openclaw").expect("openclaw");
            let launch = Launch {
                program: PathBuf::from("/cli/openclaw"),
                args: Vec::new(),
                env: Vec::new(),
            };
            let args = recorder_args(
                &RecorderRun {
                    out: Path::new("/runs/x"),
                    scene: Path::new("/sb/scene.sock"),
                    within: Path::new("/sb"),
                    workspace: Path::new("/ws/proj"),
                    cli_env: &[],
                    cli_version: "1.2.3",
                    launch: &launch,
                    driver: Path::new("/build/tui_drive"),
                },
                p,
                &p.scenarios[0],
            );
            let words = |flag: &str| -> Vec<String> {
                args.windows(2)
                    .filter(|w| w[0] == *flag)
                    .map(|w| w[1].to_string_lossy().into_owned())
                    .collect()
            };
            assert_eq!(
                words(recorder_flag::DAEMON),
                ["/cli/openclaw", "gateway", "run"]
            );
            assert_eq!(
                words(recorder_flag::DAEMON_READY),
                ["/cli/openclaw", "gateway", "health"]
            );
            let config: serde_json::Value =
                serde_json::from_str(&config_text(p.config[0].template, TEST_PORTS, "/ws"))
                    .expect("valid JSON");
            assert_eq!(config["gateway"]["port"], TEST_PORTS.daemon);
        }

        #[test]
        fn a_tui_scenario_hands_the_recorder_the_driver_and_the_clis_interactive_launch() {
            let p = hermetic::profile("claude-code").expect("profile");
            let (scenario, tui) = p
                .scenarios
                .iter()
                .find_map(|s| match s.surface {
                    Surface::Tui(tui) => Some((s, tui)),
                    Surface::Headless => None,
                })
                .expect("a TUI scenario");
            let launch = Launch {
                program: PathBuf::from("/cli/node"),
                args: vec!["/cli/claude.js".into()],
                env: Vec::new(),
            };
            let args = recorder_args(
                &RecorderRun {
                    out: Path::new("/runs/x"),
                    scene: Path::new("/sb/scene.sock"),
                    within: Path::new("/sb"),
                    workspace: Path::new("/ws/proj"),
                    cli_env: &[],
                    cli_version: "1.2.3",
                    launch: &launch,
                    driver: Path::new("/build/tui_drive"),
                },
                p,
                scenario,
            );
            let driver = args
                .iter()
                .position(|a| a == "/build/tui_drive")
                .expect("the driver runs as the command");
            let line = tui::CommandLine::parse(&args[driver + 1..]).expect("a driver command line");
            assert_eq!(line.scenario, scenario.name);
            assert_eq!(line.log, Path::new("/runs/x").join(TUI_LOG));
            assert_eq!(line.prompt, "{prompt}");
            let program: Vec<OsString> = launch
                .argv()
                .into_iter()
                .chain(tui.args().iter().map(OsString::from))
                .collect();
            assert_eq!(line.program, program);
        }

        #[cfg(unix)]
        #[test]
        fn a_workspace_reports_its_real_path_made_or_not() {
            let d = tempfile::tempdir().expect("tempdir");
            let real = d.path().join("real");
            std::fs::create_dir(&real).expect("real");
            std::os::unix::fs::symlink(&real, d.path().join("alias")).expect("alias");
            let real = std::fs::canonicalize(&real).expect("real");
            // A string, not a `PathBuf`: Claude Code matches the trust key's text,
            // and a `PathBuf` compare ignores a trailing separator.
            let want = |tail: &str| format!("{}{tail}", real.display());
            assert_eq!(
                reported_cwd(&d.path().join("alias/capture/proj")),
                want("/capture/proj")
            );
            assert_eq!(reported_cwd(&d.path().join("alias")), want(""));
        }

        #[test]
        fn a_windows_workspace_keys_claude_codes_trust_seed_as_it_looks_it_up() {
            let workspace = r"C:\ProgramData\pixtuoid-capture\proj";
            let seed = hermetic::profile("claude-code").expect("profile").config[0].template;
            let parsed: serde_json::Value =
                serde_json::from_str(&config_text(seed, TEST_PORTS, workspace))
                    .expect("valid JSON");
            assert!(
                parsed["projects"]["C:/ProgramData/pixtuoid-capture/proj"]["hasTrustDialogAccepted"]
                    == true,
                "{parsed}"
            );
        }

        #[test]
        fn a_windows_workspace_keys_codexs_trust_by_its_own_spelling() {
            let workspace = r"C:\ProgramData\pixtuoid-capture\proj";
            let config = hermetic::profile("codex").expect("profile").config[0].template;
            let parsed: toml::Table =
                toml::from_str(&config_text(config, TEST_PORTS, workspace)).expect("valid TOML");
            assert_eq!(
                parsed["projects"][workspace]["trust_level"].as_str(),
                Some("trusted"),
                "{parsed}"
            );
        }

        #[test]
        fn every_json_or_toml_config_parses_with_a_windows_workspace() {
            let workspace = r"C:\ProgramData\pixtuoid-capture\proj";
            for p in PROFILES {
                for c in p.config {
                    let text = config_text(c.template, TEST_PORTS, workspace);
                    let parsed = match Path::new(c.path).extension().and_then(OsStr::to_str) {
                        Some("json" | "jsonc") => serde_json::from_str::<serde_json::Value>(&text)
                            .map(drop)
                            .map_err(|e| e.to_string()),
                        Some("toml") => toml::from_str::<toml::Table>(&text)
                            .map(drop)
                            .map_err(|e| e.to_string()),
                        _ => continue,
                    };
                    assert!(parsed.is_ok(), "{}: {}: {parsed:?}", p.source, c.path);
                }
            }
        }

        #[test]
        fn a_sources_runs_are_judged_together_only_when_every_scenario_was() {
            let one = [PathBuf::from("/runs/x")];
            assert!(judges_source(&one, 1));
            assert!(!judges_source(&one, 2), "a sibling selected out or Blocked");
        }

        #[test]
        fn every_child_of_a_run_gets_the_profiles_env_expanded() {
            // What a shebang's `env -S VAR=value` sets.
            let launch = Launch {
                program: PathBuf::from("node"),
                args: Vec::new(),
                env: vec![("NODE_OPTIONS".into(), "--no-warnings".into())],
            };
            for p in PROFILES {
                let sandbox = Sandbox::new(
                    p,
                    Path::new("/nowhere/shims"),
                    Path::new("/nowhere/bin"),
                    TEST_PORTS,
                    Path::new("/nowhere/ws"),
                )
                .expect("sandbox");
                let cli_env = sandbox.cli_env(p, &launch);
                assert_eq!(
                    cli_env.len(),
                    p.env.len() + launch.env.len(),
                    "{}",
                    p.source
                );
                assert!(
                    cli_env.contains(&"NODE_OPTIONS=--no-warnings".to_owned()),
                    "{}: the launch's own env misses the CLI",
                    p.source
                );
                for (k, v) in p.env {
                    let want = v
                        .replace("{port}", &TEST_PORTS.model.to_string())
                        .replace(DAEMON_PORT_PLACEHOLDER, &TEST_PORTS.daemon.to_string())
                        .replace("{key}", DUMMY_KEY);
                    assert_eq!(
                        sandbox.var_os(k),
                        Some(&OsString::from(&want)),
                        "{}: {k} misses `connect`'s children",
                        p.source
                    );
                    // The only path past the recorder's `CLAUDE_CODE_*` scrub.
                    assert!(
                        cli_env.contains(&format!("{k}={want}")),
                        "{}: {k} misses the CLI's `--env`",
                        p.source
                    );
                }
                for (var, _) in SANDBOX_HOMES {
                    let home = sandbox.var(var).expect("set");
                    assert!(home.starts_with(sandbox.dir.path()), "{var} escapes");
                }
                let path = sandbox.var_os("PATH").expect("PATH");
                assert_eq!(
                    std::env::split_paths(path).next(),
                    Some(PathBuf::from("/nowhere/shims")),
                    "{}: the tier's shim comes first",
                    p.source
                );
            }
        }

        /// The built shim itself, run once before any CLI: a fresh copy, or a
        /// first exec, is cold.
        #[cfg(unix)]
        #[test]
        fn a_tier_links_the_built_shim_and_runs_it_once() {
            use std::os::unix::fs::PermissionsExt;
            let d = tempfile::tempdir().expect("tempdir");
            let shim = d.path().join("pixtuoid-hook");
            let ran = d.path().join("ran");
            std::fs::write(&shim, format!("#!/bin/sh\ncat > '{}'\n", ran.display()))
                .expect("a shim");
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
            let dir = warmed_shim_dir(&d.path().join("tier"), &shim).expect("placed");
            let placed = dir.join("pixtuoid-hook");
            assert_eq!(std::fs::read_link(&placed).ok(), Some(shim), "not a copy");
            assert_eq!(
                std::fs::read_to_string(&ran).ok().as_deref(),
                Some("{}"),
                "it ran"
            );
        }

        #[test]
        fn an_install_points_every_home_its_cli_reads_into_the_install() {
            let dir = Path::new("/tier/cli/x-1.0.0");
            for p in PROFILES {
                let install = npm_install(p, dir, "x@1.0.0");
                let homes: Vec<(&std::ffi::OsStr, Option<&std::ffi::OsStr>)> =
                    install.get_envs().collect();
                for var in p.home_vars() {
                    let at = homes
                        .iter()
                        .find(|(k, _)| *k == var)
                        .and_then(|(_, v)| *v)
                        .unwrap_or_else(|| panic!("{}: {var} is the operator's", p.source));
                    assert!(Path::new(at).starts_with(dir), "{}: {var}", p.source);
                }
            }
        }

        #[test]
        fn npm_and_aimock_keep_npm_config_and_lose_provider_keys() {
            temp_env::with_vars(
                [
                    ("ANTHROPIC_API_KEY", Some("sk-host")),
                    ("npm_config_cache", Some("/npm-cache")),
                ],
                || {
                    // A real child: `get_envs` shows no variable the child would inherit.
                    let runs = |argv: &[&str]| {
                        tool(argv[0])
                            .args(&argv[1..])
                            .status()
                            .expect("the shell runs")
                            .success()
                    };
                    #[cfg(unix)]
                    assert!(runs(&[
                        "sh",
                        "-c",
                        r#"test -z "$ANTHROPIC_API_KEY" && test "$npm_config_cache" = /npm-cache"#,
                    ]));
                    #[cfg(windows)]
                    {
                        let key = [
                            "cmd",
                            "/c",
                            "if",
                            "defined",
                            "ANTHROPIC_API_KEY",
                            "exit",
                            "1",
                        ];
                        assert!(runs(&key), "a provider key reached the child");
                        let npm = [
                            "cmd",
                            "/c",
                            "if",
                            "not",
                            "defined",
                            "npm_config_cache",
                            "exit",
                            "1",
                        ];
                        assert!(runs(&npm), "npm's config missed the child");
                    }
                },
            );
        }

        #[test]
        fn a_variable_is_kept_as_its_platform_spells_names() {
            let list = ["PATH", "SystemRoot"];
            assert!(listed(true, &list, "Path"), "Windows ignores case");
            assert!(listed(true, &list, "SYSTEMROOT"));
            assert!(!listed(false, &list, "Path"), "Unix does not");
            assert!(listed(false, &list, "PATH"));
            assert!(!listed(true, &list, "PATHEXT"));
        }

        #[test]
        fn a_shebang_reads_as_cmd_shim_reads_it() {
            let sb = |l: &str| {
                Shebang::parse(l)
                    .expect("the expression")
                    .expect("a shebang")
            };
            assert_eq!(
                sb("#!/usr/bin/env node"),
                Shebang {
                    program: "node".into(),
                    args: vec![],
                    env: vec![]
                }
            );
            assert_eq!(
                sb("#!/usr/bin/env node --no-warnings --stack-size=8"),
                Shebang {
                    program: "node".into(),
                    args: vec!["--no-warnings".into(), "--stack-size=8".into()],
                    env: vec![]
                }
            );
            assert_eq!(
                sb("#!/usr/bin/env -S NODE_OPTIONS=--x DEBUG=1 node --flag"),
                Shebang {
                    program: "node".into(),
                    args: vec!["--flag".into()],
                    env: vec![
                        ("NODE_OPTIONS".into(), "--x".into()),
                        ("DEBUG".into(), "1".into())
                    ]
                }
            );
            assert_eq!(sb("#!/bin/sh").program, "/bin/sh");
            assert_eq!(
                Shebang::parse("MZ\u{90}\0").expect("the expression"),
                None,
                "a PE has none"
            );
        }

        /// A package as npm lays it out, its `bin` entry holding `head`.
        fn package(d: &Path, bin: serde_json::Value, entry: &str, head: &[u8]) -> PathBuf {
            let pkg = d.join("node_modules/pkg");
            std::fs::create_dir_all(pkg.join("bin")).expect("mkdir");
            std::fs::write(
                pkg.join("package.json"),
                serde_json::json!({ "bin": bin }).to_string(),
            )
            .expect("manifest");
            std::fs::write(pkg.join(entry), head).expect("entry");
            pkg
        }

        #[test]
        fn a_windows_launch_runs_what_npm_s_shim_runs() {
            let d = tempfile::tempdir().expect("tempdir");
            let bin = d.path().join("node_modules/.bin");
            std::fs::create_dir_all(&bin).expect("mkdir");
            let node = package(
                d.path(),
                serde_json::json!({ "cli": "bin/cli.js" }),
                "bin/cli.js",
                b"\n#!/usr/bin/env node --no-warnings\nimport './main.js';\n",
            );
            let l = Launch::from_bin_field(&node, &bin, "cli").expect("launch");
            assert_eq!(l.program, PathBuf::from("node"), "node on PATH");
            assert_eq!(
                l.args,
                [
                    "--no-warnings".to_string(),
                    node.join("bin/cli.js").to_string_lossy().into_owned()
                ]
            );
            std::fs::write(bin.join("node.exe"), "").expect("a node beside the shim");
            let l = Launch::from_bin_field(&node, &bin, "cli").expect("launch");
            assert_eq!(
                l.program,
                bin.join("node.exe"),
                "the shim prefers its sibling"
            );
            let pe = package(
                d.path(),
                serde_json::json!("bin/cli.exe"),
                "bin/cli.exe",
                b"MZ\x90\x00",
            );
            let l = Launch::from_bin_field(&pe, &bin, "cli").expect("launch");
            assert_eq!(
                l,
                Launch {
                    program: pe.join("bin/cli.exe"),
                    args: vec![],
                    env: vec![]
                },
                "a PE runs itself"
            );
            assert!(
                Launch::from_bin_field(&pe, &bin, "other").is_ok(),
                "a string bin is every name's"
            );
        }

        /// A bare name would leave aimock, whose link no run's PATH holds, unfound.
        #[cfg(unix)]
        #[test]
        fn a_unix_launch_is_the_link_npm_made() {
            let l =
                Launch::of(Path::new("/m/pkg"), Path::new("/m/.bin"), "llmock").expect("launch");
            assert_eq!(l.program, PathBuf::from("/m/.bin/llmock"));
            assert!(l.args.is_empty() && l.env.is_empty());
        }

        #[test]
        fn a_script_expands_inside_its_strings_whatever_the_path_holds() {
            let mut script = serde_json::json!({
                "fixtures": [{ "response": { "toolCalls": [{
                    "name": "{shell_tool}",
                    "arguments": { "file_path": "{workspace}/NOTE.txt" }
                }]}}]
            });
            let values = [
                (
                    "{workspace}",
                    r"C:\ProgramData\pixtuoid-capture\proj".to_string(),
                ),
                ("{shell_tool}", "pwsh".to_string()),
            ];
            expand_strings(&mut script, &values);
            let text = serde_json::to_string(&script).expect("serializes");
            let back: serde_json::Value = serde_json::from_str(&text).expect("still JSON");
            let call = &back["fixtures"][0]["response"]["toolCalls"][0];
            assert_eq!(call["name"], "pwsh");
            assert_eq!(
                call["arguments"]["file_path"],
                r"C:\ProgramData\pixtuoid-capture\proj/NOTE.txt"
            );
        }

        #[test]
        fn a_lifecycle_goes_on_only_from_a_judged_scenario() {
            let on = |findings: Vec<(Label, String)>| {
                lifecycle_goes_on(&Verdict::new(findings), ()).is_some()
            };
            assert!(!on(vec![(Label::Blocked, "login wall".into())]));
            assert!(!on(vec![
                (Label::Blocked, "exit 1".into()),
                (Label::Script, "unmatched".into()),
            ]));
            for label in [Label::Pass, Label::Stale, Label::Script, Label::Fail] {
                assert!(on(vec![(label, "judged".into())]), "{label:?}");
            }
            assert!(on(Vec::new()));
        }

        #[test]
        fn a_replay_runs_in_a_workspace_of_its_own_and_a_recording_in_the_canonical_one() {
            let (first, a) = run_workspace(false).expect("a replay's workspace");
            let (second, b) = run_workspace(false).expect("another's");
            assert_ne!(a, b, "two replays at once would share their files");
            for ws in [&a, &b] {
                assert!(ws.starts_with(capture_root().expect("the root")), "{ws:?}");
                assert!(
                    pixtuoid_core::harness::capture_base(ws).is_some(),
                    "the recorder clears only a capture base: {ws:?}"
                );
            }
            let root = second.as_ref().expect("a replay owns its root").0.clone();
            assert!(root.is_dir());
            drop(second);
            assert!(!root.exists(), "a replay's root goes with it");
            drop(first);
            let (none, canonical) = run_workspace(true).expect("a recording's workspace");
            assert!(none.is_none());
            assert_eq!(canonical, capture_workspace().expect("the canonical one"));
        }

        #[test]
        fn the_lifecycle_turns_a_headless_scenario_and_never_records() {
            let tui = PROFILES
                .iter()
                .flat_map(|p| p.scenarios)
                .find(|s| matches!(s.surface, Surface::Tui(_)))
                .expect("a TUI scenario");
            let scenario = |surface, fixture| Scenario {
                name: tui.name,
                shape: tui.shape,
                surface,
                expects: &[],
                fixture,
            };
            let tui_first = [
                scenario(tui.surface, "tui"),
                scenario(Surface::Headless, "headless"),
            ];
            let picked = |s: &[Scenario]| headless_first(s.iter()).map(|s| s.fixture);
            assert_eq!(picked(&tui_first), Some("headless"));
            assert_eq!(picked(&tui_first[..1]), Some("tui"));
            let codex = hermetic::profile("codex").expect("profiled");
            let record = parse(&args(&["--record", "codex"])).expect("parse");
            assert!(lifecycle_turn(&record, codex, false).is_none());
            let all = parse(&args(&[])).expect("parse");
            assert!(lifecycle_turn(&all, codex, false).is_some());
            let other = parse(&args(&["codex", "tool-run"])).expect("parse");
            assert!(
                lifecycle_turn(&other, codex, false).is_none(),
                "not selected"
            );
        }

        #[test]
        fn a_selection_naming_nothing_runnable_is_refused() {
            let check = |v: &[&str]| parse(&args(v)).expect("parse").check(cfg!(windows));
            assert!(check(&["codex"]).is_ok());
            assert!(check(&["codex", "tool-run"]).is_ok());
            assert!(check(&["codex", "no-such-scenario"]).is_err());
            assert!(check(&["no-such-source"]).is_err());
            let (absent, _) = NOT_HERMETIC[0];
            assert!(check(&[absent]).is_err(), "an unprofiled source says why");
            let on = |windows| {
                parse(&args(&["grok", "permission"]))
                    .expect("parse")
                    .check(windows)
            };
            assert!(on(false).is_ok());
            let refused = on(true).expect_err("grok's TUI under ConPTY");
            assert!(refused.contains("ConPTY"), "{refused}");
            assert!(check(&["openclaw", "no-such-scenario"]).is_err());
            let turn = |windows| {
                parse(&args(&["openclaw", "tool-run"]))
                    .expect("parse")
                    .check(windows)
            };
            assert!(turn(false).is_ok());
            let refused = turn(true).expect_err("a daemon on Windows");
            assert!(refused.contains("Unix signal"), "{refused}");
            assert!(
                check(&["codex", LIVE]).is_err(),
                "a scenario profile's live run"
            );
            assert!(check(&["codex", LIFECYCLE]).is_ok());
            let lifecycle = |windows| {
                parse(&args(&["openclaw", LIFECYCLE]))
                    .expect("parse")
                    .check(windows)
            };
            assert!(lifecycle(false).is_ok());
            assert!(
                lifecycle(true).is_err(),
                "its one scenario runs on no Windows leg"
            );
            for named in [&["openclaw"][..], &["openclaw", LIVE]] {
                let check = |windows| parse(&args(named)).expect("parse").check(windows);
                assert!(check(false).is_ok(), "{named:?}");
                let refused = check(true).expect_err("a Unix-socket tier on Windows");
                assert!(refused.contains("Unix socket"), "{refused}");
            }
        }

        #[cfg(not(unix))]
        #[test]
        fn record_is_refused_off_unix() {
            let check = |v: &[&str]| parse(&args(v)).expect("parse").check(cfg!(windows));
            let refused = check(&["--record", "dsh"]).expect_err("the corpus pins Unix paths");
            assert!(refused.contains("Unix"), "{refused}");
        }

        #[cfg(unix)]
        #[test]
        fn record_takes_whole_sources() {
            let a = parse(&args(&["--record", "--cli", "pinned", "dsh"])).expect("parse");
            assert!(a.record);
            assert_eq!(a.versions, Versions::Pinned);
            let check = |v: &[&str]| parse(&args(v)).expect("parse").check(cfg!(windows));
            assert!(check(&["--record"]).is_ok());
            assert!(check(&["--record", "dsh"]).is_ok());
            assert!(check(&["--cli", "latest", "--record", "dsh"]).is_ok());
            assert!(
                check(&["--cli", "latest", "--record"]).is_err(),
                "a later source's refusal would strand an earlier one's repin"
            );
            assert!(
                check(&["--record", "dsh", "tool-run"]).is_err(),
                "the census needs all"
            );
            assert!(!parse(&args(&["dsh"])).expect("parse").record);
        }

        #[test]
        fn npm_answers_a_release_or_says_why_not() {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt;
            // A Unix raw status is a wait(2) word, exit code in its high byte; a
            // Windows one is the exit code itself.
            let exit_1 = if cfg!(unix) { 1 << 8 } else { 1 };
            let (ok, failed) = (ExitStatus::from_raw(0), ExitStatus::from_raw(exit_1));
            let spec = "@openai/codex@latest";
            assert_eq!(
                npm_release(spec, ok, b"0.162.1\n", b""),
                Ok(Version::new(0, 162, 1))
            );
            for bad in [&b"0.162.1\""[..], b"latest", b"", b"  \n"] {
                assert!(npm_release(spec, ok, bad, b"").is_err(), "{bad:?}");
            }
            assert!(
                npm_release(spec, ok, b"\n", b"").is_err_and(|why| why.contains("no version")),
                "an absent `latest` exits 0 with nothing on stdout"
            );
            let stderr = b"npm warn config ignoring a setting\nnpm error code E404\n\
                           npm error 404 Not Found - GET https://registry.npmjs.org/nope - Not found\n\
                           npm error A complete log of this run can be found in: /Users/op/x.log\n";
            let why = npm_release(spec, failed, b"", stderr).expect_err("npm failed");
            assert!(why.contains("E404") && why.contains("Not Found"), "{why}");
            assert!(!why.contains("warn"), "npm's errors alone: {why}");
            assert!(
                !why.contains("/Users/op"),
                "the code and summary alone: {why}"
            );
            assert!(
                npm_release(spec, failed, b"", b"\x1b[31mERR!\x1b[0m\n")
                    .is_err_and(|why| why.contains(&format!("exited {failed}"))),
                "no npm error line still says why"
            );
        }

        #[test]
        fn a_get_is_no_model_request_and_a_websocket_request_is_one() {
            let journal = serde_json::json!([
                {"method": "GET", "path": "/api/v1/models", "response": {"status": 200, "fixture": null}},
                {"method": "POST", "path": "/v1/chat/completions", "response": {"status": 200, "fixture": {"match": {}}}},
                {"method": "POST", "path": "/v1/chat/completions", "response": {"status": 404, "fixture": null}},
                {"method": "WS", "path": "/v1/responses", "response": {"status": 404, "fixture": null}},
            ]);
            assert_eq!(model_requests(journal.as_array().expect("entries")), (3, 2));
        }

        #[test]
        fn a_payload_matches_only_its_own_sha512() {
            // FIPS 180-2's one-block example.
            let abc = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                       2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
            assert!(matches_sha512(b"abc", abc));
            assert!(!matches_sha512(b"abd", abc));
        }

        #[test]
        fn a_lock_pinning_the_version_records_again_without_the_updater() {
            let build = Build {
                version: "1.4.2".into(),
                url: "https://example.invalid/agy.tar.gz".into(),
                sha512: "00".into(),
            };
            let lock = ManifestLock {
                version: "1.4.2".into(),
                platforms: [("darwin_arm64".to_string(), build)].into(),
            };
            let platforms: &[(&str, &str, &'static str)] = &[("macos", "aarch64", "darwin_arm64")];
            let again = lock_of(
                Some(lock),
                "https://example.invalid/{platform}.json",
                platforms,
                &Version::new(1, 4, 2),
            )
            .expect("the committed lock");
            assert_eq!(again.version, "1.4.2");
        }

        #[test]
        fn a_lock_pinning_the_version_installs_without_the_updater() {
            let build = Build {
                version: "1.4.2".into(),
                url: "https://example.invalid/agy.tar.gz".into(),
                sha512: "00".into(),
            };
            let lock = ManifestLock {
                version: "1.4.2".into(),
                platforms: [("darwin_arm64".to_string(), build.clone())].into(),
            };
            let got = build_of(
                Some(&lock),
                "https://example.invalid/{platform}.json",
                "darwin_arm64",
                &Version::new(1, 4, 2),
            )
            .expect("the lock's build");
            assert_eq!(got, build);
        }

        #[test]
        fn a_result_counts_only_where_aimock_read_a_tool_message() {
            let journal = serde_json::json!([
                {"method": "POST", "body": {"messages": [{"role": "user"}]}},
                {"method": "POST", "body": {"messages": [{"role": "assistant"}, {"role": "assistant"}]}},
                {"method": "POST", "body": {"messages": [{"role": "assistant"}, {"role": "tool"}]}},
            ]);
            assert_eq!(results_seen(journal.as_array().expect("entries")), 1);
        }

        #[test]
        fn the_latest_release_is_the_tag_its_redirect_names() {
            let headers = "HTTP/2 302\r\ndate: x\r\n\
                           Location: https://github.com/o/r/releases/tag/v18.12.1\r\n\r\n";
            assert_eq!(latest_tag(headers), Some(Version::new(18, 12, 1)));
            for bad in [
                "HTTP/2 200\r\n\r\n",
                "location: https://github.com/o/r/releases\r\n",
                "location: https://github.com/o/r/releases/tag/nightly\r\n",
            ] {
                assert_eq!(latest_tag(bad), None, "{bad:?}");
            }
        }

        /// Each OS leg of the tier runs every profile on its own host.
        #[test]
        fn every_release_profile_names_this_hosts_binary() {
            for p in PROFILES {
                if let Package::GithubRelease { assets, .. } = p.package {
                    let asset = asset_here(assets).expect(p.source);
                    assert_eq!(asset.ends_with(".exe"), cfg!(windows), "{asset}");
                }
            }
        }
    }
}
