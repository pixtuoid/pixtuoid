//! Record a conformance fixture from the bytes a real agent CLI actually sent.
//! Worked examples, the rules, and why this is Rust and not the shell script it
//! replaces: `tests/sources/fixtures/README.md`.
//!
//! ⚠ BILLED — runs one real model turn on that provider's account.
//!
//! It records the SHIM'S OUTPUT, by pointing `PIXTUOID_SOCKET` at a listener of
//! our own — the one seam that does not care how the payload reached the shim.
//! Recording its INPUT cannot cover every source: codewhale is env-mode, so a
//! stdin tee captures a file of empty payloads.

fn main() -> std::io::Result<()> {
    recorder::main()
}

mod recorder {
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use pixtuoid_core::harness::reads::{self, Deidentified};
    use pixtuoid_core::source::{registry, resolved_source_root};

    /// One prompt for every source, so captures stay comparable: reading the SAME
    /// file twice around a list forces both shapes the composed fixtures got wrong —
    /// tools that interleave, and a tool id that repeats. `CAPTURE_PROMPT` overrides
    /// it for a scenario the shared one cannot reach (a permission gate needs a tool
    /// the CLI refuses to run unasked).
    const DEFAULT_PROMPT: &str =
        "Read NOTE.txt, then list this directory, then read NOTE.txt again.";

    use pixtuoid_core::harness::hermetic::{self, MANIFEST, Manifest};
    use pixtuoid_core::harness::host::{create_private_dir, owned_by_us};
    use pixtuoid_core::harness::recorder_flag::{
        CLI_VERSION, ENV, OUT, PREPARE_ROOT, PROMOTE, TEE, WITHIN, WORKSPACE,
    };
    use pixtuoid_core::harness::tap::{Endpoint, Tap};
    use pixtuoid_core::harness::{
        CAPTURE_NOTE, CAPTURE_RECORD, CORPUS_REFUSAL, CaptureRecord, HOOK_PAYLOADS, PROVENANCE,
        capture_base, capture_workspace, capture_workspace_tail, jsonl_files_under,
        required_version_probe, version_answer,
    };

    /// Where a driver names the CLI it drives, a command or the path to its
    /// binary, for the recorder to ask its `--version`.
    const CLI_ENV: &str = "PIXTUOID_CAPTURE_CLI";

    /// Where the recorder names the source's registry `verified_version`, for a
    /// driver that installs its CLI: a capture must anchor that version.
    const VERSION_ENV: &str = "PIXTUOID_CAPTURE_VERSION";

    /// A hermetic run's overrides: where the capture goes, and what the harvest
    /// and the CLI may see.
    #[derive(Debug, Default, Clone, PartialEq)]
    struct Options {
        /// Write the RAW capture here: not stripped, no provenance, no PII scan.
        /// The strip blanks what the decoder under test does not read, which is
        /// exactly where a renamed upstream field lands, so drift evidence must
        /// keep it; these bytes are never committed.
        out: Option<PathBuf>,
        /// Forward each payload, as received, to this endpoint: a headless pixtuoid
        /// renders the same run the recorder records.
        tee: Option<PathBuf>,
        /// Refuse a harvest root outside this directory, a sandbox the caller
        /// pointed every home into: a resolver that slipped would otherwise
        /// harvest the operator's own transcripts.
        within: Option<PathBuf>,
        /// Set on the CLI after the agent-env scrub, which would strip a
        /// caller's own `CLAUDE_CODE_*` opt-outs with the agent's.
        env: Vec<(String, String)>,
        /// Run no CLI: write this hermetic run's raw capture into the committed
        /// fixture its scenario names, as a live recording would be written.
        promote: Option<PathBuf>,
        /// The CLI's `--version`, asked by a caller that launched it some way
        /// the recorder cannot repeat (an interpreter and an entry script).
        cli_version: Option<String>,
        /// The workspace as the caller resolved it: Windows resolves its known
        /// ProgramData only in an environment the sandbox's no longer holds.
        workspace: Option<PathBuf>,
        /// Run no CLI: create this source's transcript root inside `within`, as
        /// a user who ran the CLI before has it.
        prepare_root: Option<String>,
    }

    impl Options {
        /// Why this host may not run what these options ask: a raw [`OUT`]
        /// capture is any host's, the committed corpus [`CORPUS_REFUSAL`]'s.
        fn corpus_refusal(&self) -> Option<&'static str> {
            CORPUS_REFUSAL.filter(|_| self.promote.is_some() || self.out.is_none())
        }

        /// [`PREPARE_ROOT`] runs no CLI, so it takes [`WITHIN`] and nothing else:
        /// any other option asks for a run or a write this mode never makes.
        fn prepare_root_alone(&self, positionals: &[String]) -> Result<(), String> {
            let alone = Options {
                prepare_root: self.prepare_root.clone(),
                within: self.within.clone(),
                ..Options::default()
            };
            if *self == alone && positionals.is_empty() {
                Ok(())
            } else {
                Err(format!("{PREPARE_ROOT} takes {WITHIN} and nothing else"))
            }
        }
    }

    /// Leading option pairs, each a [`pixtuoid_core::harness::recorder_flag`]
    /// and its value, then the positionals.
    fn parse_options(mut args: Vec<String>) -> Result<(Options, Vec<String>), String> {
        let mut options = Options::default();
        // The flag's value, taking the pair off the front of `args`.
        let take = |args: &mut Vec<String>, flag: &str| {
            if args.len() < 2 {
                return Err(format!("{flag} takes a value"));
            }
            args.remove(0);
            Ok(args.remove(0))
        };
        while let Some(flag) = args.first().cloned() {
            match flag.as_str() {
                OUT => options.out = Some(PathBuf::from(take(&mut args, OUT)?)),
                TEE => options.tee = Some(PathBuf::from(take(&mut args, TEE)?)),
                WITHIN => options.within = Some(PathBuf::from(take(&mut args, WITHIN)?)),
                PROMOTE => options.promote = Some(PathBuf::from(take(&mut args, PROMOTE)?)),
                CLI_VERSION => options.cli_version = Some(take(&mut args, CLI_VERSION)?),
                PREPARE_ROOT => options.prepare_root = Some(take(&mut args, PREPARE_ROOT)?),
                WORKSPACE => {
                    let ws = PathBuf::from(take(&mut args, WORKSPACE)?);
                    if capture_base(&ws).is_none() {
                        return Err(format!(
                            "{WORKSPACE} {} must be absolute and end in {}: the recorder clears \
                             its parent",
                            ws.display(),
                            capture_workspace_tail().display()
                        ));
                    }
                    options.workspace = Some(ws);
                }
                ENV => {
                    let value = take(&mut args, ENV)?;
                    let (k, v) = value
                        .split_once('=')
                        .ok_or_else(|| format!("{ENV} takes KEY=VALUE, got {value:?}"))?;
                    options.env.push((k.to_string(), v.to_string()));
                }
                _ => break,
            }
        }
        Ok((options, args))
    }

    /// Why `root` may not be harvested from inside `within`, if it may not.
    /// Lexical: every home the caller set is a join under `within`.
    fn harvest_root_refusal(root: Option<&Path>, within: &Path) -> Option<String> {
        let root = root?;
        let escapes = root
            .components()
            .any(|c| c == std::path::Component::ParentDir);
        (escapes || !root.starts_with(within)).then(|| {
            format!(
                "the harvest root {} is outside {}: a home the sandbox did not set",
                root.display(),
                within.display()
            )
        })
    }

    /// Create `root` when it lies inside `within`: an operator's own root would
    /// read to pixtuoid's auto-detect as an installed CLI. A source with no
    /// transcript root has nothing to create.
    fn prepare_root(root: Option<&Path>, within: Option<&Path>) -> std::io::Result<()> {
        let Some(root) = root else {
            return Ok(());
        };
        let within = within
            .ok_or_else(|| std::io::Error::other(format!("{PREPARE_ROOT} needs {WITHIN}")))?;
        if let Some(why) = harvest_root_refusal(Some(root), within) {
            return Err(std::io::Error::other(why));
        }
        std::fs::create_dir_all(root)
    }

    pub(crate) fn main() -> std::io::Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--strip-corpus") {
            return strip_corpus(&sources_root());
        }
        let (options, args) = match parse_options(args) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        if let Some(source) = options.prepare_root.as_deref() {
            if let Err(why) = options.prepare_root_alone(&args) {
                eprintln!("{why}");
                std::process::exit(2);
            }
            return prepare_root(
                resolved_source_root(source).as_deref(),
                options.within.as_deref(),
            );
        }
        if let Some(why) = options.corpus_refusal() {
            eprintln!("{why}");
            std::process::exit(2);
        }
        if let Some(run) = options.promote.as_deref() {
            return promote(run, &sources_root(), &identity_needles(git_config));
        }
        if args.len() < 3 {
            eprintln!(
                "usage: capture_fixture [{OUT} <dir>] [{TEE} <socket|pipe>] [{WITHIN} <dir>] [{ENV} K=V]... \
                 [{CLI_VERSION} <version>] [{WORKSPACE} <dir>] <source-id> <scenario> <cmd...>   \
                 ('{{prompt}}' expands)\n       \
                 capture_fixture --strip-corpus   (strip every committed capture in place)\n       \
                 capture_fixture {PROMOTE} <run>   (write a hermetic run into its fixture)\n       \
                 capture_fixture {WITHIN} <dir> {PREPARE_ROOT} <source-id>   (create its root; no CLI)"
            );
            std::process::exit(2);
        }
        let (source, scenario) = (args[0].clone(), args[1].clone());
        // Set-but-empty is unset: `CAPTURE_PROMPT=` would otherwise expand
        // `{prompt}` to nothing and record `"prompt": ""` as if it were a choice.
        let prompt = nonempty_env("CAPTURE_PROMPT").unwrap_or_else(|| DEFAULT_PROMPT.to_string());
        let cmd: Vec<String> = args[2..]
            .iter()
            .map(|a| a.replace("{prompt}", &prompt))
            .collect();

        let Some(descriptor) = registry::descriptor_for(&source) else {
            let known: Vec<&str> = registry::registered_source_names().collect();
            eprintln!("no source {source:?} — registered: {}", known.join(", "));
            std::process::exit(2);
        };

        let dest = options
            .out
            .clone()
            .unwrap_or_else(|| scenario_dest(&sources_root(), &source, &scenario));
        let root = resolved_source_root(&source);
        if let Some(why) = options
            .within
            .as_deref()
            .and_then(|w| harvest_root_refusal(root.as_deref(), w))
        {
            eprintln!("{why}");
            std::process::exit(2);
        }

        let workspace = prepare_workspace(match options.workspace.clone() {
            Some(ws) => ws,
            None => capture_workspace()?,
        })?;
        let sandbox = tempfile::tempdir()?;
        let endpoint = Endpoint::in_dir(sandbox.path(), "capture");
        let tap = Tap::bind(&endpoint, options.tee.clone().map(Endpoint::at))?;

        println!("capturing {source}/{scenario} — one real model turn");
        let started = SystemTime::now();
        let status = run_cli(
            &cmd,
            &workspace,
            CliWiring {
                endpoint: &endpoint,
                scratch: sandbox.path(),
            },
            descriptor.verified_version,
            &options.env,
        )?;
        tap.settle();
        let payloads = tap.payloads();
        let incomplete = tap.stopped().map(|why| {
            std::io::Error::other(format!(
                "the tap stopped accepting, so this capture is incomplete: {why}"
            ))
        });

        // A committed capture is complete or not written at all.
        let incomplete = match incomplete {
            Some(e) if options.out.is_none() => return Err(e),
            kept => kept,
        };
        std::fs::create_dir_all(&dest)?;
        let mut wrote: Vec<PathBuf> = Vec::new();

        // A source can be BOTH: CC's tool run is in the transcript while its
        // permission gate is a hook event, so the two are harvested together.
        if let Some(root) = root.as_deref() {
            let candidates = pixtuoid_core::harness::transcripts_under(&source, root);
            let born = born_after(&candidates, started);
            for fresh in harvested(born, options.within.is_some()) {
                wrote.push(place_transcript(&source, &fresh, &dest)?);
            }
        }
        if !payloads.is_empty() {
            wrote.push(write_payloads(&payloads, &dest)?);
        }

        let (cli, version) = match options.cli_version {
            Some(version) => (registered_cli(descriptor)?, version),
            None => {
                let named = std::fs::read_to_string(named_cli(sandbox.path())).ok();
                identity(&cmd[0], named.as_deref(), probe_version)
            }
        };
        if options.out.is_some() {
            // An empty raw capture is still recorded: a hook that never fired
            // is evidence about the run, which a missing record would hide.
            let record = CaptureRecord {
                workspace: std::fs::canonicalize(&workspace).unwrap_or(workspace),
                cli,
                version,
                exit_code: status.code(),
            };
            std::fs::write(
                dest.join(CAPTURE_RECORD),
                format!("{}\n", serde_json::to_string_pretty(&record)?),
            )?;
            for p in &wrote {
                println!("wrote raw {} ({} lines)", p.display(), count_lines(p));
            }
            return incomplete.map_or(Ok(()), Err);
        }
        if wrote.is_empty() {
            eprintln!(
                "captured nothing — no hook fired and no transcript was created{}; did the turn run?",
                root.map(|r| format!(" under {}", r.display()))
                    .unwrap_or_default()
            );
            std::process::exit(1);
        }

        let deidentified = deidentify(&source, &wrote, reads::exemption(&dest.join(PROVENANCE)))?;
        write_provenance(
            &dest,
            (&cli, &version),
            &args[2..],
            &deidentified,
            &[
                ("prompt", nonempty_env("CAPTURE_PROMPT")),
                ("seed", nonempty_env("CAPTURE_SEED")),
            ],
        )?;
        for p in &wrote {
            println!("wrote {} ({} lines)", p.display(), count_lines(p));
        }
        match &deidentified {
            Deidentified::Stripped(n) => println!("blanked {n} subtrees no decoder reads"),
            Deidentified::Exempt(why) => println!("not stripped, as the record declares: {why}"),
        }
        refuse_on_pii(&wrote, &identity_needles(git_config))?;
        if !status.success() {
            eprintln!("WARNING: the CLI exited {status} — this capture may be truncated");
        }
        println!("next: just test conformance   then   cargo insta review");
        Ok(())
    }

    fn deidentify(
        source: &str,
        files: &[PathBuf],
        exemption: Option<String>,
    ) -> std::io::Result<Deidentified> {
        Ok(match exemption {
            Some(why) => Deidentified::Exempt(why),
            None => {
                let strip = reads::strip_unread(source, files)?;
                report_not_json(&strip);
                Deidentified::Stripped(strip.blanked)
            }
        })
    }

    /// Replaces the fixture's bytes whole, where a live recording writes `.new`
    /// beside them: the old ones are git's.
    fn promote(run: &Path, root: &Path, needles: &BTreeSet<String>) -> std::io::Result<()> {
        let manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(run.join(MANIFEST))?)?;
        let named = format!("{}/{}", manifest.source, manifest.scenario);
        let scenario = hermetic::profile(&manifest.source)
            .and_then(|p| {
                p.scenarios
                    .iter()
                    .find(|s| s.shape.name() == manifest.scenario)
            })
            .ok_or_else(|| std::io::Error::other(format!("{named}: no profiled scenario")))?;
        let dest = root.join(scenario.fixture);
        let exempt = reads::exemption(&dest.join(PROVENANCE));
        for old in jsonl_files_under(&dest)
            .into_iter()
            .chain([dest.join(PROVENANCE)])
        {
            match std::fs::remove_file(&old) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let mut wrote = Vec::new();
        for raw in jsonl_files_under(run) {
            let to = dest.join(raw.strip_prefix(run).map_err(std::io::Error::other)?);
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::copy(&raw, &to)?;
            wrote.push(to);
        }
        let deidentified = deidentify(&manifest.source, &wrote, exempt)?;
        let script = scenario.script(&manifest.source);
        // The record names its script as every host's checkout spells it.
        let script = script
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&script)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let command = ["just", "hermetic", "--record", manifest.source.as_str()].map(String::from);
        write_provenance(
            &dest,
            (&manifest.capture.cli, &manifest.capture.version),
            &command,
            &deidentified,
            &[
                (hermetic::PROVENANCE_AIMOCK, Some(manifest.aimock.clone())),
                ("script", Some(script)),
            ],
        )?;
        refuse_on_pii(&wrote, needles)?;
        println!("recorded {named} into {}", dest.display());
        Ok(())
    }

    // ── the decisions ────────────────────────────────────────────────────────────

    /// The agent's own env reaches the CLI under test: a nested `claude` inherited
    /// `CLAUDE_CODE_CHILD_SESSION` and turned its transcript saving OFF, so the run
    /// left nothing to harvest. Scrub the whole namespace, not the one that bit.
    fn agent_env_names<I: IntoIterator<Item = String>>(names: I) -> Vec<String> {
        names
            .into_iter()
            .filter(|k| k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_"))
            .collect()
    }

    /// The transcripts born since `after`, oldest first: in a sandbox, the ones
    /// the run created, a delegating CLI's child's beside its parent's. BIRTH
    /// time, not mtime: a live agent session is appended to forever, which once
    /// harvested the developer's own.
    fn born_after(candidates: &[PathBuf], after: SystemTime) -> Vec<PathBuf> {
        let mut born: Vec<(SystemTime, PathBuf)> = candidates
            .iter()
            .filter_map(|p| {
                let born = std::fs::metadata(p).ok()?.created().ok()?;
                (born >= after).then(|| (born, p.clone()))
            })
            .collect();
        born.sort_by_key(|(at, _)| *at);
        born.into_iter().map(|(_, p)| p).collect()
    }

    /// What the harvest keeps of the transcripts `born` during the run, oldest
    /// first: every one in a sandbox, and only the newest in the operator's own
    /// root, where another session may be born during the run.
    fn harvested(mut born: Vec<PathBuf>, sandboxed: bool) -> Vec<PathBuf> {
        if !sandboxed {
            born = born.split_off(born.len().saturating_sub(1));
        }
        born
    }

    /// Where `fresh` lands under `dest`: under as few of its own parent dirs as
    /// keep the session `source` keys it on (grok's is the parent dir's name),
    /// so flattening never renames the session after the scenario. Asked of the
    /// source's own id deriver, never a table of which sources those are; one
    /// keyed on the whole path, which any placement changes, lands flat.
    fn placement(source: &str, fresh: &Path, dest: &Path) -> PathBuf {
        let derive = registry::id_deriver_for(source);
        let name = fresh.file_name().expect("a walked file has a name");
        let dirs: Vec<&std::ffi::OsStr> = fresh
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .filter_map(|c| match c {
                std::path::Component::Normal(d) => Some(d),
                _ => None,
            })
            .collect();
        (0..=dirs.len())
            .map(|kept| {
                dirs[dirs.len() - kept..]
                    .iter()
                    .fold(dest.to_path_buf(), |d, dir| d.join(dir))
                    .join(name)
            })
            .find(|at| derive(at) == derive(fresh))
            .unwrap_or_else(|| dest.join(name))
    }

    fn place_transcript(source: &str, fresh: &Path, dest: &Path) -> std::io::Result<PathBuf> {
        let out = no_clobber(placement(source, fresh, dest));
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(fresh, &out)?;
        Ok(out)
    }

    /// A re-record of an existing scenario is drift EVIDENCE, so it lands beside the
    /// original to be diffed rather than overwriting a committed, redacted capture.
    fn no_clobber(p: PathBuf) -> PathBuf {
        if p.exists() {
            let mut s = p.into_os_string();
            s.push(".new");
            PathBuf::from(s)
        } else {
            p
        }
    }

    // ── plumbing ─────────────────────────────────────────────────────────────────

    fn prepare_workspace(ws: PathBuf) -> std::io::Result<PathBuf> {
        let base = capture_base(&ws).ok_or_else(|| {
            std::io::Error::other(format!("{} is not a capture workspace", ws.display()))
        })?;
        // A fixed name in a shared directory is pre-plantable; a foreign owner is
        // refused, not removed — under the sticky bit the remove fails and the
        // capture lands in theirs. Asked of the entry, so a dangling planted link
        // is refused too.
        if std::fs::symlink_metadata(base).is_ok() {
            if !owned_by_us(base) {
                eprintln!(
                    "{} exists and is not yours — remove it or run as its owner",
                    base.display()
                );
                std::process::exit(2);
            }
            match std::fs::remove_dir_all(base) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        // Made in one call no other local user can write into, and refused if
        // anyone re-created it since the remove.
        create_private_dir(base)?;
        std::fs::create_dir(&ws)?;
        std::fs::write(ws.join("NOTE.txt"), format!("{CAPTURE_NOTE}\n"))?;
        if let Some(seed) = std::env::var_os("CAPTURE_SEED") {
            copy_tree(Path::new(&seed), &ws)?;
        }
        init_repo(&ws)?;
        Ok(ws)
    }

    /// Make `ws` its own repository, every step of which must succeed: that
    /// stops codex's project-root walk and opencode's AGENTS.md walk there, so
    /// a failed `git init` would let a repository above it in; the walks that
    /// reach `/` are each profile's `ancestor_config`.
    fn init_repo(ws: &Path) -> std::io::Result<()> {
        for cmd in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec![
                "-c",
                "user.email=fixture@pixtuoid",
                "-c",
                "user.name=fixture",
                "commit",
                "-qm",
                "init",
            ],
        ] {
            git_in(ws, &cmd)?;
        }
        Ok(())
    }

    /// One git step, reading none of the operator's config: their hooks,
    /// templates or signing would otherwise decide whether the workspace is made
    /// (git(1): `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` set to `/dev/null`).
    fn git_in(ws: &Path, args: &[&str]) -> std::io::Result<()> {
        let status = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .arg("-C")
            .arg(ws)
            .args(args)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "git {args:?} exited {status}"
            )))
        }
    }

    /// A per-CLI ask rule (CC's `.claude/settings.json`, opencode's
    /// `opencode.json`) is what makes a gate fire at all, so it seeds the
    /// sandbox rather than the user's own config.
    fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let dest = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                std::fs::create_dir_all(&dest)?;
                copy_tree(&entry.path(), &dest)?;
            } else {
                std::fs::copy(entry.path(), &dest)?;
            }
        }
        Ok(())
    }

    /// Where one run's shim connects, and its private scratch directory.
    struct CliWiring<'a> {
        endpoint: &'a Endpoint,
        scratch: &'a Path,
    }

    fn run_cli(
        cmd: &[String],
        ws: &Path,
        wiring: CliWiring<'_>,
        verified: &str,
        env: &[(String, String)],
    ) -> std::io::Result<std::process::ExitStatus> {
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..])
            .current_dir(ws)
            .env(Endpoint::ENV, wiring.endpoint.path());
        // A driver's transcript goes in this run's private scratch: a fixed
        // shared-temp name is symlink-followable and two concurrent captures interleave.
        c.env("TUIDRIVE_LOG", wiring.scratch.join("tuidrive.log"));
        c.env(CLI_ENV, named_cli(wiring.scratch))
            .env(VERSION_ENV, verified);
        for k in agent_env_names(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok())) {
            c.env_remove(k);
        }
        c.envs(env.iter().map(|(k, v)| (k, v)));
        c.status()
    }

    fn write_payloads(payloads: &[String], dest: &Path) -> std::io::Result<PathBuf> {
        let out = no_clobber(dest.join(HOOK_PAYLOADS));
        let mut f = std::fs::File::create(&out)?;
        for p in payloads {
            // The shim already stamped `_pixtuoid_source`.
            let v: serde_json::Value = serde_json::from_str(p.trim())
                .unwrap_or_else(|e| panic!("a captured payload is not JSON: {e}: {p:?}"));
            writeln!(f, "{}", serde_json::to_string(&v)?)?;
        }
        Ok(out)
    }

    /// In this run's private scratch.
    fn named_cli(scratch: &Path) -> PathBuf {
        scratch.join("cli")
    }

    /// The CLI's own name, as its registry row asks it its version.
    fn registered_cli(descriptor: &registry::SourceDescriptor) -> std::io::Result<String> {
        required_version_probe(descriptor)?
            .first()
            .map(|cli| (*cli).to_owned())
            .ok_or_else(|| {
                std::io::Error::other(format!("{}'s version probe names no CLI", descriptor.name))
            })
    }

    /// The CLI a capture ran, and its version, asked of that CLI alone: a
    /// command named by a path is a driver, which `--version` would run whole,
    /// billed and outside the sandbox, so only the CLI it names at [`CLI_ENV`]
    /// is asked.
    fn identity(
        cmd0: &str,
        named: Option<&str>,
        probe: impl FnOnce(&str) -> Option<String>,
    ) -> (String, String) {
        let cli = named
            .and_then(|n| n.lines().next())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .or_else(|| (!cmd0.contains('/')).then_some(cmd0));
        let name = Path::new(cli.unwrap_or(cmd0))
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_owned();
        let version = cli.and_then(probe).unwrap_or_else(|| "unknown".into());
        (name, version)
    }

    fn probe_version(cli: &str) -> Option<String> {
        std::process::Command::new(cli)
            .arg("--version")
            .output()
            .ok()
            .as_ref()
            .and_then(version_answer)
    }

    fn write_provenance(
        dest: &Path,
        (cli, version): (&str, &str),
        raw: &[String],
        deidentified: &Deidentified,
        overrides: &[(&str, Option<String>)],
    ) -> std::io::Result<()> {
        let mut prov = serde_json::json!({
            "origin": "recorded",
            "cli": cli,
            "version": version,
            "captured": today(),
            "command": raw.join(" "),
            "deidentified": deidentified.record(),
        });
        // `command` is the UN-expanded argv, so an override-driven scenario records a
        // `{prompt}` placeholder; written only when set, so committed records stay schema-clean.
        if let Some(map) = prov.as_object_mut() {
            for (key, value) in overrides {
                if let Some(v) = value {
                    map.insert((*key).into(), serde_json::Value::String(v.clone()));
                }
            }
        }
        let out = no_clobber(dest.join(PROVENANCE));
        std::fs::write(&out, format!("{}\n", serde_json::to_string_pretty(&prov)?))?;
        // Loud, because the record just written is unusable.
        if version == "unknown" {
            eprintln!(
                "WARNING: {} records version \"unknown\" — `{cli} --version` did not \
                 answer, or a driver named no CLI at ${CLI_ENV}. Fix `cli` and `version` \
                 by hand before committing, or the anchor gate skips this source.",
                out.display()
            );
        }
        Ok(())
    }

    fn nonempty_env(key: &str) -> Option<String> {
        pixtuoid_core::platform::text_env(key).filter(|s| !s.trim().is_empty())
    }

    fn today() -> String {
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let days = secs / 86_400;
        let (mut y, mut d) = (1970i64, days as i64);
        loop {
            let len = if leap(y) { 366 } else { 365 };
            if d < len {
                break;
            }
            d -= len;
            y += 1;
        }
        let months = [
            31,
            if leap(y) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut m = 0;
        while d >= months[m] {
            d -= months[m];
            m += 1;
        }
        format!("{y:04}-{:02}-{:02}", m + 1, d + 1)
    }

    fn leap(y: i64) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }

    /// Strings this machine would leak into a capture, the git identity included:
    /// a CLI that renders `git status` embeds the committer's real name, which no
    /// value shape can match (`.gitleaks-identity.toml` carries the committed-tree half).
    fn identity_needles(git: impl Fn(&str) -> Option<String>) -> BTreeSet<String> {
        let mut needles: BTreeSet<String> = BTreeSet::new();
        for var in ["HOME", "USER", "LOGNAME"] {
            if let Some(v) = pixtuoid_core::platform::text_env(var)
                && !v.is_empty()
            {
                needles.insert(v);
            }
        }
        for key in ["user.name", "user.email"] {
            if let Some(v) = git(key).filter(|v| !v.is_empty()) {
                needles.insert(v);
            }
        }
        needles
    }

    fn git_config(key: &str) -> Option<String> {
        let out = std::process::Command::new("git")
            .args(["config", "--get", key])
            .output()
            .ok()?;
        parse_git_config(&out.stdout)
    }

    /// `git config` answers with a trailing newline, and an unset key answers with
    /// nothing — an untrimmed value never matches the bytes a CLI embedded, and an
    /// empty one would make every capture match the needle.
    fn parse_git_config(stdout: &[u8]) -> Option<String> {
        let v = String::from_utf8_lossy(stdout).trim().to_string();
        (!v.is_empty()).then_some(v)
    }

    /// Every reason `body` cannot be committed, or empty: the operator's own
    /// identity, the one set gitleaks cannot know. Everything with a value shape
    /// is the tree scan's; everything no decoder reads is already gone.
    fn pii_hits(body: &str, needles: &BTreeSet<String>) -> Vec<String> {
        needles
            .iter()
            .filter(|n| body.contains(n.as_str()))
            .map(String::to_string)
            .collect()
    }

    fn scan_for_pii(files: &[PathBuf], needles: &BTreeSet<String>) -> Vec<String> {
        let mut found = Vec::new();
        for f in files {
            let Ok(body) = std::fs::read_to_string(f) else {
                continue;
            };
            let hits = pii_hits(&body, needles);
            if !hits.is_empty() {
                found.push(format!("{}: {}", f.display(), hits.join(", ")));
            }
        }
        found
    }

    /// A REFUSAL, not a warning. The old form printed to stderr and left the exit
    /// code at 0 on a run that had already written into the repo tree, so the
    /// capturer had to notice a line scrolling past — and twice did not.
    fn refuse_on_pii(files: &[PathBuf], needles: &BTreeSet<String>) -> std::io::Result<()> {
        let found = scan_for_pii(files, needles);
        if found.is_empty() {
            return Ok(());
        }
        for line in &found {
            eprintln!("PII: {line}");
        }
        Err(std::io::Error::other(
            "the capture embeds the recorder's own identity — redact those files, \
             then re-run. The bytes are on disk; nothing was discarded.",
        ))
    }

    fn count_lines(p: &Path) -> usize {
        std::fs::read_to_string(p)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    fn sources_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sources")
    }

    /// The library strips and records; printing the outcome is the CLI's job.
    fn strip_corpus(root: &Path) -> std::io::Result<()> {
        reads::strip_corpus(root, |capture, strip| {
            report_not_json(strip);
            println!(
                "{}: blanked {}",
                capture
                    .dir
                    .strip_prefix(root)
                    .unwrap_or(&capture.dir)
                    .display(),
                strip.blanked
            );
        })
    }

    fn report_not_json(strip: &reads::Strip) {
        for (file, error) in &strip.not_json {
            eprintln!("{}", not_json_notice(file, error));
        }
    }

    fn not_json_notice(file: &Path, error: &str) -> String {
        format!(
            "not stripping {}: a line is not JSON: {error}",
            file.display()
        )
    }

    /// Where this scenario's bytes ALREADY live, else the conformance root: a module
    /// keeps its own rounds out of conformance's reach (omp's hook keys fold on
    /// Windows), and a re-record sent to the root instead lands as a NEW directory
    /// `conformance.rs` auto-scans, with no committed bytes for `.new` to protect.
    /// Keyed on the SOURCE, never a search across modules: scenario names repeat
    /// (`approval-recorded` is hermes' and omp's), so a search finds the wrong
    /// module's single match and a billed capture lands as `.new` files inside it.
    /// `claude/` owning `claude-code` is the one name mismatch, and it holds no
    /// scenario subdirectory, so it cannot reach here.
    fn scenario_dest(sources: &Path, source: &str, scenario: &str) -> PathBuf {
        let owned = sources.join(source).join("fixtures").join(scenario);
        if owned.join(PROVENANCE).is_file() {
            return owned;
        }
        sources.join("fixtures").join(source).join(scenario)
    }

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use super::*;

        /// An operator nobody's bytes carry, so no test reads the machine's.
        const OPERATOR: &str = "Ada Lovelace";

        struct Promotable {
            _dir: tempfile::TempDir,
            run: PathBuf,
            root: PathBuf,
            dest: PathBuf,
        }

        /// The run's raw capture is the committed recording with a key no
        /// decoder reads planted on line 1, then `edit`ed; the root's fixture
        /// holds `old.jsonl` and the `record` given.
        fn promotable(
            record: serde_json::Value,
            edit: impl FnOnce(&mut [serde_json::Value]),
        ) -> Promotable {
            let d = tempfile::tempdir().expect("tempdir");
            let (run, root) = (d.path().join("run"), d.path().join("sources"));
            let fixture = hermetic::profile("dsh")
                .and_then(|p| p.scenarios.iter().find(|s| s.shape.name() == "tool-run"))
                .map(|s| s.fixture)
                .expect("dsh's tool-run");
            let committed =
                std::fs::read_to_string(sources_root().join(fixture).join(HOOK_PAYLOADS))
                    .expect("the committed recording");
            let mut lines: Vec<serde_json::Value> = committed
                .lines()
                .map(|l| serde_json::from_str(l).expect("json"))
                .collect();
            lines[0]["planted"] = "unread".into();
            edit(&mut lines);
            std::fs::create_dir_all(&run).expect("mkdir");
            let raw: Vec<String> = lines.iter().map(ToString::to_string).collect();
            std::fs::write(run.join(HOOK_PAYLOADS), raw.join("\n") + "\n").expect("raw");
            let manifest = Manifest {
                source: "dsh".into(),
                scenario: "tool-run".into(),
                capture: CaptureRecord {
                    workspace: capture_workspace().expect("the workspace"),
                    cli: "dsh".into(),
                    version: "0.2.0-rc.2".into(),
                    exit_code: Some(0),
                },
                aimock: "1.43.0".into(),
                requests: 3,
                unmatched: 0,
            };
            std::fs::write(
                run.join(MANIFEST),
                serde_json::to_string(&manifest).expect("json"),
            )
            .expect("manifest");
            let dest = root.join(fixture);
            std::fs::create_dir_all(&dest).expect("mkdir");
            std::fs::write(dest.join("old.jsonl"), "{}\n").expect("old");
            std::fs::write(dest.join(PROVENANCE), record.to_string()).expect("record");
            Promotable {
                _dir: d,
                run,
                root,
                dest,
            }
        }

        fn needles() -> BTreeSet<String> {
            BTreeSet::from([OPERATOR.to_string()])
        }

        fn written(dest: &Path) -> (String, serde_json::Value) {
            let bytes = std::fs::read_to_string(dest.join(HOOK_PAYLOADS)).expect("bytes");
            let record = std::fs::read_to_string(dest.join(PROVENANCE)).expect("record");
            (bytes, serde_json::from_str(&record).expect("json"))
        }

        #[test]
        fn a_promoted_run_replaces_its_fixture_stripped_and_says_what_played_the_model() {
            let p = promotable(
                serde_json::json!({"origin": "recorded", "note": "the old record"}),
                |_| {},
            );
            promote(&p.run, &p.root, &needles()).expect("promote");
            assert!(
                !p.dest.join("old.jsonl").exists(),
                "the old bytes leave whole"
            );
            let (bytes, record) = written(&p.dest);
            assert!(!bytes.contains("unread"), "the planted key is stripped");
            assert_eq!(record["deidentified"]["method"], "decoder-allowlist");
            assert!(
                record["deidentified"]["blanked"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
            );
            assert_eq!(record[hermetic::PROVENANCE_AIMOCK], "1.43.0");
            assert_eq!(record["script"], "tests/sources/hermetic/dsh/tool-run.json");
            assert_eq!(record["version"], "0.2.0-rc.2");
            assert!(
                record.get("note").is_none(),
                "the record is the run's, not the old one"
            );
        }

        #[test]
        fn a_promoted_run_carrying_the_operator_is_refused() {
            let p = promotable(serde_json::json!({"origin": "recorded"}), |lines| {
                lines[0]["cwd"] = format!("/Users/{OPERATOR}/proj").into();
            });
            assert!(
                promote(&p.run, &p.root, &needles()).is_err(),
                "the operator's name is refused"
            );
        }

        #[test]
        fn a_promoted_run_keeps_its_fixtures_exemption() {
            let why = "a test reads the raw bytes";
            let p = promotable(
                serde_json::json!({
                    "origin": "recorded",
                    "deidentified": {"method": "none", "why": why},
                }),
                |_| {},
            );
            promote(&p.run, &p.root, &needles()).expect("promote");
            let (bytes, record) = written(&p.dest);
            assert!(
                bytes.contains("unread"),
                "an exempt capture is not stripped"
            );
            assert_eq!(record["deidentified"]["method"], "none");
            assert_eq!(record["deidentified"]["why"], why);
        }

        #[test]
        fn the_workspace_becomes_its_own_repository_or_fails() {
            let d = tempfile::tempdir().expect("tempdir");
            let ws = d.path().join("ws");
            std::fs::create_dir(&ws).expect("mkdir");
            std::fs::write(ws.join("NOTE.txt"), format!("{CAPTURE_NOTE}\n")).expect("write");
            // An operator config whose signing and commit-msg hook both fail.
            let hooks = d.path().join("hooks");
            std::fs::create_dir(&hooks).expect("mkdir");
            let hook = hooks.join("commit-msg");
            std::fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("hook");
            #[cfg(unix)]
            std::fs::set_permissions(
                &hook,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
            )
            .expect("chmod");
            let config = d.path().join("gitconfig");
            std::fs::write(
                &config,
                format!(
                    "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = false\n\
                     [core]\n\thooksPath = {}\n",
                    hooks.display()
                ),
            )
            .expect("config");
            let made = temp_env::with_vars(
                [
                    ("GIT_CONFIG_GLOBAL", Some(config.as_os_str())),
                    ("GIT_CONFIG_SYSTEM", Some(config.as_os_str())),
                ],
                || init_repo(&ws),
            );
            assert!(made.is_ok(), "{made:?}");
            assert!(git_in(&ws, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok());
            let file = d.path().join("a-file");
            std::fs::write(&file, "").expect("write");
            assert!(init_repo(&file).is_err(), "a failed step fails it");
        }

        #[test]
        fn a_workspace_whose_parent_is_not_a_capture_base_is_refused() {
            let args = |ws: &Path| vec![WORKSPACE.to_owned(), ws.display().to_string()];
            let root = std::env::temp_dir();
            assert!(
                parse_options(args(&root.join("proj"))).is_err(),
                "its parent is the operator's"
            );
            let tail = capture_workspace_tail();
            let why = parse_options(args(&tail)).expect_err("a relative path with the right tail");
            assert!(
                why.contains("absolute"),
                "refused for being relative: {why}"
            );
            assert!(
                why.contains(&format!("end in {}:", tail.display())),
                "names the tail it checks, never a root any absolute one may have: {why}"
            );
            let ws = root.join(&tail);
            let (o, _) = parse_options(args(&ws)).expect("a capture workspace");
            assert_eq!(o.workspace, Some(ws));
        }

        #[test]
        fn prepare_root_takes_within_and_nothing_else() {
            let alone = Options {
                within: Some(PathBuf::from("sandbox")),
                prepare_root: Some("codex".into()),
                ..Options::default()
            };
            assert_eq!(alone.prepare_root_alone(&[]), Ok(()));
            assert!(
                alone.prepare_root_alone(&["codex".into()]).is_err(),
                "a positional"
            );
            let promote = Options {
                promote: Some(PathBuf::from("run")),
                ..alone.clone()
            };
            assert!(
                promote.prepare_root_alone(&[]).is_err(),
                "a promote it would skip"
            );
            let out = Options {
                out: Some(PathBuf::from("raw")),
                ..alone
            };
            assert!(
                out.prepare_root_alone(&[]).is_err(),
                "a capture it never makes"
            );
        }

        #[test]
        fn only_a_unix_host_writes_the_corpus() {
            let raw = Options {
                out: Some(PathBuf::from("raw")),
                ..Options::default()
            };
            assert_eq!(raw.corpus_refusal(), None, "a raw capture is any host's");
            let promote = Options {
                promote: Some(PathBuf::from("run")),
                ..raw
            };
            for corpus in [Options::default(), promote] {
                assert_eq!(corpus.corpus_refusal().is_some(), !cfg!(unix), "{corpus:?}");
            }
        }

        #[test]
        fn leading_option_pairs_are_options_and_the_rest_is_positional() {
            let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
            let ws = capture_workspace().expect("the workspace");
            let (o, rest) = parse_options(args(&[
                "--tee",
                "/s",
                "--out",
                "/o",
                "--within",
                "/w",
                "--env",
                "A=b=c",
                "--cli-version",
                "1.2",
                "--workspace",
                &ws.display().to_string(),
                "codex",
                "tool-run",
                "codex",
            ]))
            .expect("parse");
            assert_eq!(o.cli_version.as_deref(), Some("1.2"));
            assert_eq!(o.workspace, Some(ws));
            assert_eq!(o.out, Some(PathBuf::from("/o")));
            assert_eq!(o.tee, Some(PathBuf::from("/s")));
            assert_eq!(o.within, Some(PathBuf::from("/w")));
            assert_eq!(o.env, [("A".to_string(), "b=c".to_string())]);
            assert_eq!(rest, args(&["codex", "tool-run", "codex"]));
            assert!(
                parse_options(args(&["--env", "NOEQUALS", "codex"])).is_err(),
                "an env pair without its value"
            );
            let (o, rest) = parse_options(args(&["--other", "x=y", "codex"])).expect("parse");
            assert_eq!(o, Options::default(), "an unknown flag ends the options");
            assert_eq!(rest, args(&["--other", "x=y", "codex"]));
            let (o, rest) = parse_options(args(&["codex", "tool-run", "--out"])).expect("parse");
            assert_eq!(
                o,
                Options::default(),
                "a flag after the positionals is the CLI's"
            );
            assert_eq!(rest.len(), 3);
            assert!(
                parse_options(args(&["--out"])).is_err(),
                "a flag without its path"
            );
        }

        #[test]
        fn a_root_is_prepared_only_inside_the_sandbox() {
            let sandbox = tempfile::tempdir().expect("tempdir");
            let elsewhere = tempfile::tempdir().expect("tempdir");
            let inside = sandbox.path().join("homes/GROK_HOME/sessions");
            prepare_root(Some(&inside), Some(sandbox.path())).expect("inside");
            assert!(inside.is_dir());

            let outside = elsewhere.path().join("sessions");
            assert!(prepare_root(Some(&outside), Some(sandbox.path())).is_err());
            assert!(
                prepare_root(Some(&outside), None).is_err(),
                "no sandbox named"
            );
            assert!(!outside.exists(), "a refusal creates nothing");
            assert!(
                prepare_root(None, None).is_ok(),
                "a hook-only source has no root"
            );
        }

        #[test]
        fn a_harvest_root_outside_the_sandbox_is_refused() {
            let within = Path::new("/sandbox");
            assert_eq!(harvest_root_refusal(None, within), None);
            assert_eq!(
                harvest_root_refusal(
                    Some(Path::new("/sandbox/homes/CODEX_HOME/sessions")),
                    within
                ),
                None
            );
            assert!(
                harvest_root_refusal(Some(Path::new("/Users/me/.claude/projects")), within)
                    .is_some()
            );
            assert!(
                harvest_root_refusal(Some(Path::new("/sandbox/../Users/me/.codex")), within)
                    .is_some(),
                "a lexical prefix that climbs out"
            );
            assert!(
                harvest_root_refusal(Some(Path::new("/sandboxed")), within).is_some(),
                "a sibling sharing the prefix's text"
            );
        }

        #[test]
        fn the_not_json_notice_names_the_file_and_the_parse_error() {
            let notice = not_json_notice(
                Path::new("/x/hook-payloads.jsonl"),
                "expected value at line 2",
            );
            assert_eq!(
                notice,
                "not stripping /x/hook-payloads.jsonl: a line is not JSON: expected value at line 2"
            );
        }

        #[test]
        fn a_module_owned_scenario_is_re_recorded_where_it_lives() {
            let d = tempfile::tempdir().expect("tempdir");
            let root = d.path();
            let owned = root.join("omp/fixtures/bridge-run-recorded");
            std::fs::create_dir_all(&owned).expect("mkdir");
            std::fs::write(owned.join("provenance.json"), "{}").expect("write");

            assert_eq!(scenario_dest(root, "omp", "bridge-run-recorded"), owned);
            assert_eq!(
                scenario_dest(root, "omp", "tool-run-recorded"),
                root.join("fixtures/omp/tool-run-recorded")
            );
            assert_eq!(
                scenario_dest(root, "kimi", "tool-run"),
                root.join("fixtures/kimi/tool-run")
            );
            std::fs::create_dir_all(root.join("kimi/fixtures/tool-run")).expect("mkdir");
            assert_eq!(
                scenario_dest(root, "kimi", "tool-run"),
                root.join("fixtures/kimi/tool-run")
            );

            let mine = root.join("omp/fixtures/approval-recorded");
            std::fs::create_dir_all(&mine).expect("mkdir");
            std::fs::write(mine.join("provenance.json"), "{}").expect("write");
            assert_eq!(scenario_dest(root, "omp", "approval-recorded"), mine);
            assert_eq!(
                scenario_dest(root, "hermes", "approval-recorded"),
                root.join("fixtures/hermes/approval-recorded")
            );
        }

        #[test]
        fn a_git_config_answer_is_trimmed_and_an_unset_key_is_none() {
            assert_eq!(
                parse_git_config(b"Ada Lovelace\n").as_deref(),
                Some("Ada Lovelace")
            );
            assert_eq!(
                parse_git_config(b"  ada@example.org  ").as_deref(),
                Some("ada@example.org")
            );
            assert_eq!(parse_git_config(b""), None);
            assert_eq!(parse_git_config(b"\n"), None);
        }

        /// Injected rather than read from the machine: on a CI runner `git config
        /// user.name` is unset, so a test asserting the real one passes VACUOUSLY
        /// exactly where it is most needed.
        #[test]
        fn the_git_identity_joins_the_needles() {
            let needles = identity_needles(|k| match k {
                "user.name" => Some("Ada Lovelace".to_string()),
                "user.email" => Some("ada@example.org".to_string()),
                _ => None,
            });
            assert!(needles.contains("Ada Lovelace"), "{needles:?}");
            assert!(needles.contains("ada@example.org"), "{needles:?}");
            assert!(!identity_needles(|_| None).contains("Ada Lovelace"));
        }

        /// A name has no shape to match, so it is refused where the recorder knows
        /// it — this machine's own git identity — and nowhere else. The tree half is
        /// `.gitleaks-identity.toml`'s label-anchored rule.
        #[test]
        fn the_operators_own_git_identity_is_refused_wherever_it_is_rendered() {
            let needles = BTreeSet::from(["Ada Lovelace".to_string()]);
            let body = r#"{"gitStatus":"Current branch: main\nGit user: Ada Lovelace"}"#;
            assert_eq!(pii_hits(body, &needles), vec!["Ada Lovelace".to_string()]);
        }

        #[test]
        fn a_capture_carrying_nothing_of_the_operator_passes() {
            let needles = BTreeSet::from(["Ada Lovelace".to_string(), "/Users/ada".to_string()]);
            let body = r#"{"cwd":"/tmp/pixtuoid-capture/proj","tool_use_id":"t1"}"#;
            assert!(pii_hits(body, &needles).is_empty(), "{body}");
        }

        #[test]
        fn the_env_scrub_names_the_agent_namespace_and_nothing_else() {
            let got = agent_env_names(
                [
                    "CLAUDECODE",
                    "CLAUDE_CODE_CHILD_SESSION",
                    "CLAUDE_CONFIG_DIR",
                    "PATH",
                    "HOME",
                ]
                .map(String::from),
            );
            assert_eq!(got, vec!["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION"]);
        }

        #[test]
        fn birth_time_selection_ignores_a_file_that_predates_the_run() {
            let d = tempfile::tempdir().expect("tempdir");
            let (old, new) = (d.path().join("old.jsonl"), d.path().join("new.jsonl"));
            std::fs::write(&old, "{}").expect("write");
            let started = SystemTime::now();
            std::thread::sleep(Duration::from_millis(1100));
            std::fs::write(&new, "{}").expect("write");
            std::thread::sleep(Duration::from_millis(1100));
            let newer = d.path().join("newer.jsonl");
            std::fs::write(&newer, "{}").expect("write");
            // Listed newest first, returned oldest first.
            let candidates = vec![newer.clone(), old.clone(), new.clone()];
            assert_eq!(
                born_after(&candidates, started),
                vec![new.clone(), newer.clone()]
            );
            // The live-session trap: touching the old file must NOT take it in,
            // which is the whole reason this reads birth time and not mtime.
            std::fs::write(&old, "{}{}").expect("touch");
            assert_eq!(born_after(&candidates, started), vec![new, newer]);
        }

        #[test]
        fn only_a_sandbox_harvests_every_transcript_born_during_the_run() {
            let born = vec![PathBuf::from("parent.jsonl"), PathBuf::from("child.jsonl")];
            assert_eq!(harvested(born.clone(), true), born);
            assert_eq!(harvested(born, false), [PathBuf::from("child.jsonl")]);
            assert!(harvested(Vec::new(), false).is_empty());
        }

        #[test]
        fn no_candidate_born_after_the_run_is_nothing_not_a_panic() {
            let d = tempfile::tempdir().expect("tempdir");
            let old = d.path().join("old.jsonl");
            std::fs::write(&old, "{}").expect("write");
            std::thread::sleep(Duration::from_millis(1100));
            assert!(born_after(&[old], SystemTime::now()).is_empty());
        }

        #[test]
        fn a_harvested_transcript_keeps_the_dirs_its_session_is_keyed_on() {
            let dest = Path::new("/fixture");
            // grok keys its session on the parent dir's name.
            let grok = Path::new("/sb/.grok/sessions/%2Fw/0198-abc/updates.jsonl");
            assert_eq!(
                placement("grok", grok, dest),
                dest.join("0198-abc/updates.jsonl")
            );
            let root = "2026-07-09t08-00-00-000z_0197f0aa-0000-7000-8000-000000000001";
            let child = PathBuf::from(format!("/sb/.omp/agent/sessions/-w/{root}/alpha.jsonl"));
            assert_eq!(
                placement("omp", &child, dest),
                dest.join(root).join("alpha.jsonl")
            );
            let cc = Path::new("/sb/.claude/projects/-w/5f2c.jsonl");
            assert_eq!(placement("claude-code", cc, dest), dest.join("5f2c.jsonl"));
            // Keyed on the whole path, which any placement changes.
            let ag = Path::new("/sb/.gemini/x/conversations/9/transcript.jsonl");
            assert_eq!(
                placement("antigravity", ag, dest),
                dest.join("transcript.jsonl")
            );
        }

        #[test]
        fn a_re_record_lands_beside_the_committed_one_instead_of_over_it() {
            let d = tempfile::tempdir().expect("tempdir");
            let p = d.path().join("hook-payloads.jsonl");
            assert_eq!(no_clobber(p.clone()), p, "a fresh path is used as-is");
            std::fs::write(&p, "{}").expect("write");
            assert!(no_clobber(p).to_string_lossy().ends_with(".new"));
        }

        #[test]
        fn provenance_records_an_override_only_when_it_was_actually_set() {
            // Both directions: committed records predate these fields and must stay
            // schema-clean, so an unset override writes nothing at all.
            let read = |d: &Path| -> serde_json::Value {
                serde_json::from_str(
                    &std::fs::read_to_string(d.join("provenance.json")).expect("read"),
                )
                .expect("json")
            };
            let argv = ["true".to_string()];

            let bare = tempfile::tempdir().expect("tempdir");
            write_provenance(
                bare.path(),
                ("true", "1.0"),
                &argv,
                &Deidentified::Stripped(0),
                &[("prompt", None)],
            )
            .expect("write");
            assert!(read(bare.path()).get("prompt").is_none());

            let set = tempfile::tempdir().expect("tempdir");
            write_provenance(
                set.path(),
                ("true", "1.0"),
                &argv,
                &Deidentified::Stripped(0),
                &[("prompt", Some("read NOTE.txt".into()))],
            )
            .expect("write");
            assert_eq!(read(set.path())["prompt"], "read NOTE.txt");
        }

        #[test]
        fn the_seed_lands_in_the_sandbox_including_a_dotted_config_dir() {
            let src = tempfile::tempdir().expect("src");
            let dst = tempfile::tempdir().expect("dst");
            std::fs::create_dir_all(src.path().join(".claude")).expect("mkdir");
            std::fs::write(src.path().join(".claude/settings.json"), "{}").expect("write");
            std::fs::write(src.path().join("opencode.json"), "{}").expect("write");

            copy_tree(src.path(), dst.path()).expect("copy");

            assert!(dst.path().join(".claude/settings.json").is_file());
            assert!(dst.path().join("opencode.json").is_file());
        }

        #[test]
        fn only_the_cli_is_asked_its_version_never_its_driver() {
            let asked = std::cell::RefCell::new(Vec::new());
            let probe = |c: &str| {
                asked.borrow_mut().push(c.to_owned());
                Some(format!("{c} 1.0"))
            };
            let driver = "scripts/lib/capture-x.sh";
            assert_eq!(
                identity(driver, Some("dsh\n"), probe),
                ("dsh".into(), "dsh 1.0".into())
            );
            assert_eq!(
                identity(driver, Some("/tmp/oc/package/bin/opencode\n"), probe),
                ("opencode".into(), "/tmp/oc/package/bin/opencode 1.0".into())
            );
            assert_eq!(
                identity(driver, None, probe),
                ("capture-x.sh".into(), "unknown".into())
            );
            assert_eq!(
                identity("claude", None, probe),
                ("claude".into(), "claude 1.0".into())
            );
            assert_eq!(
                identity("grok", None, |_| None),
                ("grok".into(), "unknown".into())
            );
            assert_eq!(
                *asked.borrow(),
                ["dsh", "/tmp/oc/package/bin/opencode", "claude"]
            );
        }

        /// A driver is a Unix shell script.
        #[cfg(unix)]
        #[test]
        fn a_driver_gets_the_verified_version_and_its_callers_env_and_names_its_cli() {
            let sandbox = tempfile::tempdir().expect("sandbox");
            let ws = tempfile::tempdir().expect("workspace");
            let endpoint = Endpoint::in_dir(sandbox.path(), "capture");
            // An inherited agent var is scrubbed; a caller's `CLAUDE_CODE_*`
            // opt-out outlives the scrub.
            let script = format!(
                r#"test "${VERSION_ENV}" = 1.0.0 && test -z "$CLAUDE_CODE_CHILD_SESSION" \
                   && test "$CLAUDE_CODE_PROBE" = 1 && echo dsh >"${CLI_ENV}""#
            );
            let cmd = ["sh", "-c", &script].map(String::from);
            let env = [("CLAUDE_CODE_PROBE".to_string(), "1".to_string())];
            // The host holds the probe too, so only scrub-then-set yields its 1.
            let ran = temp_env::with_vars(
                [
                    ("CLAUDE_CODE_CHILD_SESSION", Some("1")),
                    ("CLAUDE_CODE_PROBE", Some("0")),
                ],
                || {
                    let wiring = CliWiring {
                        endpoint: &endpoint,
                        scratch: sandbox.path(),
                    };
                    run_cli(&cmd, ws.path(), wiring, "1.0.0", &env).expect("runs")
                },
            );
            assert!(ran.success());
            assert_eq!(
                std::fs::read_to_string(named_cli(sandbox.path()))
                    .ok()
                    .as_deref(),
                Some("dsh\n")
            );
        }

        /// The recorder's env name crosses into every driver, each a copy a rename
        /// would strand: its capture would then record version "unknown".
        #[test]
        fn every_driver_names_its_cli_where_the_recorder_reads_it() {
            let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/lib");
            let mut drivers = 0;
            for entry in std::fs::read_dir(&lib).expect("scripts/lib") {
                let path = entry.expect("an entry").path();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if name.starts_with("capture-") || name == "tuidrive.py" {
                    drivers += 1;
                    let text = std::fs::read_to_string(&path).expect("a driver");
                    assert!(text.contains(CLI_ENV), "{name} names no CLI at ${CLI_ENV}");
                }
            }
            assert!(drivers > 0, "scripts/lib holds the drivers");
        }

        #[test]
        fn today_is_an_iso_date_the_provenance_gate_can_parse() {
            let t = today();
            assert_eq!(t.len(), 10, "{t}");
            let parts: Vec<&str> = t.split('-').collect();
            assert_eq!(parts.len(), 3);
            assert!(parts[0].parse::<u32>().expect("year") >= 2026);
            assert!((1..=12).contains(&parts[1].parse::<u32>().expect("month")));
            assert!((1..=31).contains(&parts[2].parse::<u32>().expect("day")));
        }
    }
}
