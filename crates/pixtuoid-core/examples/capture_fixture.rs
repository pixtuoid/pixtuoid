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

#[path = "support/bounded.rs"]
mod bounded;

mod recorder {
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use pixtuoid_core::harness::provenance::{
        Deidentified, Dropped, NonEmpty, Provenance, Recorded,
    };
    use pixtuoid_core::harness::reads;
    use pixtuoid_core::source::{AgentEvent, registry, resolved_source_root};

    use pixtuoid_core::harness::hermetic::{self, MANIFEST, Manifest, PerOs, TURN_BUDGET};
    use pixtuoid_core::harness::host::{
        RELEASE_PATIENCE, create_private_dir, hold_lock, owned_by_us, remove_once_released,
    };
    use pixtuoid_core::harness::plan::{CapturePlan, DaemonPlan, Invocation, Sink};
    use pixtuoid_core::harness::tap::{Endpoint, Tap};
    use pixtuoid_core::harness::{
        BOOT, CAPTURE_NOTE, CAPTURE_RECORD, CORPUS_REFUSAL, CaptureRecord, DRIVER_COPY,
        DRIVER_DIR_ENV, Drive, GAVE_UP_NOTE, HOOK_PAYLOADS, NOTE_FILE, PROVENANCE, SecondTurn,
        TurnEnd, canonical_capture, capture_base, capture_workspace, capture_workspace_tail,
        files_under, jsonl_files_under, required_version_probe, slashed_under, version_answer,
        wait_until,
    };

    /// Where a driver names the CLI it drives, a command or the path to its
    /// binary, for the recorder to ask its `--version`.
    const CLI_ENV: &str = "PIXTUOID_CAPTURE_CLI";

    /// Where the recorder names the source's registry `verified_version`, for a
    /// driver that installs its CLI: a capture must anchor that version.
    const VERSION_ENV: &str = "PIXTUOID_CAPTURE_VERSION";

    /// The driver's own part of the recorder's scratch, apart from everything the
    /// recorder and its listener create there: what it names at [`DRIVER_DIR_ENV`].
    fn driver_dir(scratch: &Path) -> PathBuf {
        scratch.join("driver")
    }

    /// Why the driver of a run in `scratch` gave up on its turn, if it said.
    fn give_up_reason(scratch: &Path) -> Option<String> {
        std::fs::read_to_string(driver_dir(scratch).join(GAVE_UP_NOTE))
            .ok()
            .map(|why| why.trim().to_owned())
    }

    /// [`CaptureRecord::exited_as_scripted`] for `source`'s profiled `scenario`.
    fn exited_as_scripted(source: &str, scenario: &str, record: &CaptureRecord) -> bool {
        hermetic::profile(source)
            .and_then(|p| p.scenario(scenario))
            .map_or(record.end == TurnEnd::Exited(0), |s| {
                record.exited_as_scripted(s.shape)
            })
    }

    /// The most a failed run's copied driver dir adds to the run dir: it rides an
    /// upload kept for days, beside everything else the run left.
    const DRIVER_CAP_BYTES: u64 = 8 * 1024 * 1024;

    /// What stands in a copied file's text and path for the operator's identity:
    /// a valid file name on every OS, which `<redacted>` is not on Windows.
    const REDACTED: &str = "REDACTED";

    /// Where `plan`'s failed run copies its [`driver_dir`], if it may: only a sandboxed
    /// raw capture. An operator's billed capture may hold real credentials in
    /// its driver's logs. A sandboxed run's logs hold the operator's identity
    /// through the account variables the sandbox inherits (`hermetic.rs`'s
    /// `PLATFORM_INHERITED`) and their git identity; its `HOME` is the sandbox's
    /// own. [`copy_driver_dir`] redacts those.
    fn driver_destination(plan: &CapturePlan) -> Option<PathBuf> {
        match &plan.sink {
            Sink::Raw { out, .. } if plan.sandboxed() => Some(out.as_path().join(DRIVER_COPY)),
            _ => None,
        }
    }

    /// `text` with every needle replaced by [`REDACTED`], a longer needle first
    /// where two start at one place.
    fn redact(text: &str, needles: &BTreeSet<String>) -> String {
        let mut longest_first: Vec<&String> = needles.iter().collect();
        longest_first.sort_by_key(|n| std::cmp::Reverse(n.len()));
        let (mut out, mut rest) = (String::new(), text);
        'scan: while let Some(c) = rest.chars().next() {
            for needle in &longest_first {
                if let Some(after) = rest.strip_prefix(needle.as_str()) {
                    out.push_str(REDACTED);
                    rest = after;
                    continue 'scan;
                }
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
        out
    }

    /// `rel` with each component redacted.
    fn redact_path(rel: &Path, needles: &BTreeSet<String>) -> PathBuf {
        rel.components()
            .map(|c| redact(&c.as_os_str().to_string_lossy(), needles))
            .collect()
    }

    /// What a [`copy_driver_dir`] did: the files it wrote and what it left out.
    struct DriverCopy {
        files: usize,
        notices: Vec<String>,
    }

    /// Copy the text files of `from` into `into`, in path order, up to `cap`
    /// bytes of what is written: each path and body with the operator's identity
    /// redacted, since the run dir is uploaded. The notices name, by their
    /// redacted paths, each file left out: not text, past the cap, or sharing a
    /// redacted path.
    fn copy_driver_dir(
        from: &Path,
        into: &Path,
        needles: &BTreeSet<String>,
        cap: u64,
    ) -> std::io::Result<DriverCopy> {
        let mut notices = Vec::new();
        let (mut copied, mut files) = (0u64, 0usize);
        for file in files_under(from) {
            let rel = redact_path(
                file.strip_prefix(from).map_err(std::io::Error::other)?,
                needles,
            );
            let Ok(text) = std::fs::read_to_string(&file) else {
                notices.push(format!("driver file {} left out: not text", rel.display()));
                continue;
            };
            let body = redact(&text, needles);
            let len = body.len() as u64;
            if copied + len > cap {
                notices.push(format!(
                    "driver file {} left out: past the copy's {cap}-byte cap",
                    rel.display()
                ));
                continue;
            }
            let dest = into.join(&rel);
            if dest.exists() {
                notices.push(format!(
                    "driver file {} left out: redacted, its path is another file's",
                    rel.display()
                ));
                continue;
            }
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&dest, body)?;
            copied += len;
            files += 1;
        }
        Ok(DriverCopy { files, notices })
    }

    /// Copy the driver's logs in `scratch` into `run_copy`, where a sandboxed
    /// run's upload finds them, redacting `needles()`. A copy that is whole
    /// closes `scratch`; otherwise, or with no copy, `scratch` is kept for the
    /// diagnosis of a capture that did not work when its driver left anything in
    /// [`driver_dir`], and named in `log`.
    fn keep_scratch(
        scratch: tempfile::TempDir,
        run_copy: Option<&Path>,
        needles: impl FnOnce() -> BTreeSet<String>,
        log: &mut impl Write,
    ) {
        // The scratch holds the only complete logs when the copy is not whole.
        let mut whole = true;
        if let Some(into) = run_copy {
            match copy_driver_dir(
                &driver_dir(scratch.path()),
                into,
                &needles(),
                DRIVER_CAP_BYTES,
            ) {
                Ok(copy) => {
                    copy.notices.iter().for_each(|n| {
                        let _ = writeln!(log, "{n}");
                    });
                    whole = copy.notices.is_empty();
                    if copy.files > 0 {
                        let _ =
                            writeln!(log, "the driver's logs are copied into {}", into.display());
                    }
                }
                Err(e) => {
                    whole = false;
                    let _ = writeln!(log, "the driver copy failed: {e}");
                }
            }
        }
        let left =
            std::fs::read_dir(driver_dir(scratch.path())).is_ok_and(|mut d| d.next().is_some());
        if run_copy.is_some() && whole {
            // The run dir holds the redacted copy; the originals are not left unnamed.
            let _ = scratch.close();
        } else if left {
            let kept = scratch.keep();
            let _ = writeln!(
                log,
                "the driver's logs stay in {}",
                driver_dir(&kept).display()
            );
        }
    }

    /// Why this host may not write into the committed corpus, which only a
    /// `committed` write reaches: a raw capture is any host's.
    fn corpus_refusal(committed: bool) -> Option<&'static str> {
        CORPUS_REFUSAL.filter(|_| committed)
    }

    /// Why this host may not run `plan`, if it may not: a committed write off
    /// Unix, or a workspace whose parent the recorder may not clear.
    fn plan_refusal(plan: &CapturePlan) -> Option<String> {
        corpus_refusal(matches!(plan.sink, Sink::Committed { .. }))
            .map(str::to_owned)
            .or_else(|| {
                plan.workspace
                    .as_ref()
                    .and_then(|ws| workspace_refusal(ws.as_path()))
            })
    }

    /// Why `ws` is not a workspace the recorder may clear, if it is not.
    fn workspace_refusal(ws: &Path) -> Option<String> {
        capture_base(ws).is_none().then(|| {
            format!(
                "the workspace {} must be absolute and end in {}: the recorder clears its parent",
                ws.display(),
                capture_workspace_tail().display()
            )
        })
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
    fn prepare_root(root: Option<&Path>, within: &Path) -> std::io::Result<()> {
        let Some(root) = root else {
            return Ok(());
        };
        if let Some(why) = harvest_root_refusal(Some(root), within) {
            return Err(std::io::Error::other(why));
        }
        std::fs::create_dir_all(root)
    }

    pub(crate) fn main() -> std::io::Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let invocation = match Invocation::parse_process(&args) {
            Ok(invocation) => invocation,
            Err(e) => {
                eprintln!("{e}\n{}", Invocation::usage());
                std::process::exit(2);
            }
        };
        match invocation {
            Invocation::StripCorpus => strip_corpus(&sources_root()),
            Invocation::PrepareRoot { source, within } => {
                prepare_root(resolved_source_root(&source).as_deref(), within.as_path())
            }
            Invocation::Promote(run) => match promote_here(
                &run,
                &sources_root(),
                &identity_needles(git_config, true),
                &mut std::io::stderr(),
            ) {
                Ok(promoted) => promoted,
                Err(why) => {
                    eprintln!("{why}");
                    std::process::exit(2);
                }
            },
            Invocation::Plan(file) => run(&CapturePlan::read(&file)?),
            Invocation::Capture(plan) => run(&plan),
        }
    }

    /// One capture, as `plan` spells it.
    fn run(plan: &CapturePlan) -> std::io::Result<()> {
        if let Some(why) = plan_refusal(plan) {
            eprintln!("{why}");
            std::process::exit(2);
        }
        #[cfg(unix)]
        crate::bounded::kill_child_on_signal()?;
        let (source, scenario) = (plan.source.clone(), plan.scenario.clone());
        let cmd = plan.first_turn.strings();
        let run_copy = driver_destination(plan);
        let env: Vec<(String, String)> = plan.cli_env.clone().into_iter().collect();

        let Some(descriptor) = registry::descriptor_for(&source) else {
            let known: Vec<&str> = registry::registered_source_names().collect();
            eprintln!("no source {source:?} — registered: {}", known.join(", "));
            std::process::exit(2);
        };

        let dest = match &plan.sink {
            Sink::Raw { out, .. } => out.as_path().to_path_buf(),
            Sink::Committed { .. } => scenario_dest(&sources_root(), &source, &scenario),
        };
        let root = resolved_source_root(&source);
        if let Some(why) = plan
            .within
            .as_ref()
            .and_then(|w| harvest_root_refusal(root.as_deref(), w.as_path()))
        {
            eprintln!("{why}");
            std::process::exit(2);
        }
        // Before the billed turn: a record that does not parse fails here.
        let exempt = exemption(&dest)?;

        let ws = match &plan.workspace {
            Some(ws) => ws.as_path().to_path_buf(),
            None => capture_workspace()?,
        };
        let (workspace, _recording) = prepare_workspace(ws, plan.seed.as_ref())?;
        let sandbox = tempfile::tempdir()?;
        let endpoint = Endpoint::in_dir(sandbox.path(), "capture");
        let tap = Tap::bind(
            &endpoint,
            plan.tee
                .as_ref()
                .map(|t| Endpoint::at(t.as_path().to_path_buf())),
        )?;

        println!("capturing {source}/{scenario} — one real model turn");
        let started = SystemTime::now();
        let wiring = CliWiring {
            endpoint: &endpoint,
            scratch: sandbox.path(),
        };
        let daemon = match RunningDaemon::start(
            plan.daemon.as_ref(),
            *DAEMON_BOOT.here(),
            &workspace,
            &wiring,
            descriptor.verified_version,
            &env,
        ) {
            Ok(daemon) => daemon,
            Err(e) => {
                keep_scratch(
                    sandbox,
                    run_copy.as_deref(),
                    operator_identity,
                    &mut std::io::stderr(),
                );
                return Err(e);
            }
        };
        let budget = turn_budget(plan);
        let end = run_turn(
            wired(&cmd, &workspace, &wiring, descriptor.verified_version, &env)?,
            budget,
        )?;
        let give_up_reason = give_up_reason(sandbox.path());
        let second_outcome = match plan.second_turn() {
            Some(_) if end == TurnEnd::OutOfTime => {
                eprintln!("the first turn outran its budget, so its second turn did not run");
                None
            }
            Some(words) => {
                tap.settle();
                let born = root.as_deref().map_or_else(Vec::new, |r| {
                    born_transcripts(&source, r, started, plan.sandboxed())
                });
                match first_session(&source, &tap.payloads(), &born) {
                    Some(session) => {
                        let turn: Vec<String> =
                            words.words().iter().map(|w| w.filled(&session)).collect();
                        let second = wired(
                            &turn,
                            &workspace,
                            &wiring,
                            descriptor.verified_version,
                            &env,
                        )?;
                        Some(SecondTurn {
                            end: run_turn(second, budget)?,
                        })
                    }
                    None => {
                        eprintln!("the first turn's wire named no session for its second turn");
                        None
                    }
                }
            }
            None => None,
        };
        let second_turn_skipped = plan.second_turn().is_some() && second_outcome.is_none();
        let note_survived = note_survived(&workspace);
        let daemon_stop = daemon.map_or(Ok(()), |d| d.stop(*DAEMON_STOP_GRACE.here()));
        tap.settle();
        let payloads = tap.payloads();
        let incomplete = incomplete(tap.stopped(), daemon_stop);

        // A committed capture is complete or not written at all.
        let incomplete = match incomplete {
            Some(e) if matches!(plan.sink, Sink::Committed { .. }) => {
                keep_scratch(
                    sandbox,
                    run_copy.as_deref(),
                    operator_identity,
                    &mut std::io::stderr(),
                );
                return Err(e);
            }
            kept => kept,
        };
        std::fs::create_dir_all(&dest)?;
        let mut wrote: Vec<PathBuf> = Vec::new();

        // A source can be BOTH: CC's tool run is in the transcript while its
        // permission gate is a hook event, so the two are harvested together.
        if let Some(root) = root.as_deref() {
            for fresh in born_transcripts(&source, root, started, plan.sandboxed()) {
                wrote.push(place_transcript(&source, &fresh, &dest)?);
            }
        }
        if !payloads.is_empty() {
            wrote.push(write_payloads(&payloads, &dest)?);
        }

        let (cli, version) = match &plan.cli_version {
            Some(version) => (registered_cli(descriptor)?, version.as_str().to_owned()),
            None => {
                let named = std::fs::read_to_string(named_cli(sandbox.path())).ok();
                identity(&cmd[0], named.as_deref(), probe_version)
            }
        };
        let Sink::Committed { command, prompt } = &plan.sink else {
            // An empty raw capture is still recorded: a hook that never fired
            // is evidence about the run, which a missing record would hide.
            let record = CaptureRecord {
                workspace: std::fs::canonicalize(&workspace).unwrap_or(workspace),
                cli,
                version,
                end,
                give_up_reason,
                incomplete: incomplete.as_ref().map(ToString::to_string),
                second_turn: second_outcome,
                note_survived,
            };
            std::fs::write(
                dest.join(CAPTURE_RECORD),
                format!("{}\n", serde_json::to_string_pretty(&record)?),
            )?;
            for p in &wrote {
                println!("wrote raw {} ({} lines)", p.display(), count_lines(p));
            }
            if incomplete.is_some()
                || second_turn_skipped
                || !exited_as_scripted(&source, &scenario, &record)
            {
                keep_scratch(
                    sandbox,
                    run_copy.as_deref(),
                    operator_identity,
                    &mut std::io::stderr(),
                );
            }
            return incomplete.map_or(Ok(()), Err);
        };
        if wrote.is_empty() {
            eprintln!(
                "captured nothing — no hook fired and no transcript was created{}; did the turn run?",
                root.map(|r| format!(" under {}", r.display()))
                    .unwrap_or_default()
            );
            keep_scratch(
                sandbox,
                run_copy.as_deref(),
                operator_identity,
                &mut std::io::stderr(),
            );
            std::process::exit(1);
        }

        let (deidentified, not_json) = deidentify(&source, &wrote, exempt)?;
        report_not_json(&not_json);
        write_provenance(
            &dest,
            Recorded {
                prompt: prompt.clone(),
                seed: plan.seed.clone(),
                ..record(&cli, &version, command.as_str(), deidentified.clone())?
            },
        )?;
        for p in &wrote {
            println!("wrote {} ({} lines)", p.display(), count_lines(p));
        }
        match &deidentified {
            Deidentified::Stripped { blanked } => {
                println!("blanked {blanked} subtrees no decoder reads");
            }
            Deidentified::Exempt { why } => println!("not stripped, as the record declares: {why}"),
        }
        let pii = refuse_on_pii(&wrote, &identity_needles(git_config, true));
        if end != TurnEnd::Exited(0) {
            eprintln!("WARNING: the CLI's turn ended ({end}) — this capture may be truncated");
            keep_scratch(
                sandbox,
                run_copy.as_deref(),
                operator_identity,
                &mut std::io::stderr(),
            );
        }
        pii?;
        println!("next: just test conformance   then   cargo insta review");
        Ok(())
    }

    /// What the strip did to `files`, and each it left whole as not JSON.
    fn deidentify(
        source: &str,
        files: &[PathBuf],
        exemption: Option<NonEmpty>,
    ) -> std::io::Result<(Deidentified, Vec<(PathBuf, String)>)> {
        Ok(match exemption {
            Some(why) => (Deidentified::Exempt { why }, Vec::new()),
            None => {
                let strip = reads::strip_unread(source, files)?;
                let blanked = strip.blanked;
                (Deidentified::Stripped { blanked }, strip.not_json)
            }
        })
    }

    /// The exemption the record in `dest` declares, if it holds one.
    fn exemption(dest: &Path) -> std::io::Result<Option<NonEmpty>> {
        Ok(Provenance::read_if_any(&dest.join(PROVENANCE))?.and_then(|p| p.exemption().cloned()))
    }

    /// [`promote`] on this host: refused where it may not write the corpus.
    fn promote_here(
        run: &Path,
        root: &Path,
        needles: &BTreeSet<String>,
        notices: &mut impl Write,
    ) -> Result<std::io::Result<()>, &'static str> {
        promote_unless_refused(corpus_refusal(true), run, root, needles, notices)
    }

    /// [`promote`], unless `refusal` says this host may not write the corpus; the
    /// refusal is the `Err`, the promotion's own outcome the `Ok`.
    fn promote_unless_refused(
        refusal: Option<&'static str>,
        run: &Path,
        root: &Path,
        needles: &BTreeSet<String>,
        notices: &mut impl Write,
    ) -> Result<std::io::Result<()>, &'static str> {
        match refusal {
            Some(why) => Err(why),
            None => Ok(promote(run, root, needles, notices)),
        }
    }

    /// Replaces the fixture's bytes whole, where a live recording writes `.new`
    /// beside them: the old ones are git's. A run whose record equals theirs but
    /// for `captured`, and whose bytes match theirs up to ids, leaves both be.
    fn promote(
        run: &Path,
        root: &Path,
        needles: &BTreeSet<String>,
        notices: &mut impl Write,
    ) -> std::io::Result<()> {
        let manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(run.join(MANIFEST))?)?;
        let named = format!("{}/{}", manifest.source, manifest.scenario);
        let scenario = hermetic::profile(&manifest.source)
            .and_then(|p| p.scenario(&manifest.scenario))
            .ok_or_else(|| std::io::Error::other(format!("{named}: no profiled scenario")))?;
        let dest = root.join(scenario.fixture);
        let committed = Provenance::read_if_any(&dest.join(PROVENANCE))?;
        let exempt = committed.as_ref().and_then(|p| p.exemption().cloned());
        // Stripped apart from the fixture, which it replaces only if it differs.
        let staged = tempfile::tempdir()?;
        let mut fresh = Vec::new();
        for raw in jsonl_files_under(run) {
            let to = staged
                .path()
                .join(raw.strip_prefix(run).map_err(std::io::Error::other)?);
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::copy(&raw, &to)?;
            fresh.push(to);
        }
        // Before the strip: what the decoders drop is what it blanks.
        let dropped = match exempt {
            None => Some(Dropped::of_files(&manifest.source, &fresh)?),
            Some(_) => None,
        };
        let (deidentified, not_json) = deidentify(&manifest.source, &fresh, exempt)?;
        // Named where the fixture keeps it: the staged copy is gone by then.
        for (file, error) in &not_json {
            let kept = dest.join(slashed_under(staged.path(), file)?);
            let _ = writeln!(notices, "{}", not_json_notice(&kept, error));
        }
        let script = slashed_under(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            &scenario.script(&manifest.source),
        )?;
        let command = format!("just hermetic --record {}", manifest.source);
        let fresh_record = Recorded {
            aimock: Some(filled("aimock", &manifest.aimock)?),
            script: Some(filled("script", &script)?),
            dropped,
            ..record(
                &manifest.capture.cli,
                &manifest.capture.version,
                &command,
                deidentified,
            )?
        };
        if let Some(old) = committed.as_ref().and_then(Provenance::recorded)
            && *old
                == (Recorded {
                    captured: old.captured.clone(),
                    ..fresh_record.clone()
                })
            && let Some(bytes) = canonical_capture(&dest)?
            && canonical_capture(staged.path())?.as_ref() == Some(&bytes)
        {
            println!("kept {named}: the run renamed only its ids");
            return Ok(());
        }
        for old in jsonl_files_under(&dest)
            .into_iter()
            .chain([dest.join(PROVENANCE)])
        {
            match std::fs::remove_file(&old) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            // A session dir the old run named would stay behind empty, beside
            // the new run's. `remove_dir` takes only an empty one.
            for dir in old.ancestors().skip(1).take_while(|d| *d != dest) {
                if std::fs::remove_dir(dir).is_err() {
                    break;
                }
            }
        }
        let mut wrote = Vec::new();
        for staged_file in &fresh {
            let to = dest.join(
                staged_file
                    .strip_prefix(staged.path())
                    .map_err(std::io::Error::other)?,
            );
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::copy(staged_file, &to)?;
            wrote.push(to);
        }
        write_provenance(&dest, fresh_record)?;
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

    /// What the harvest keeps of `source`'s transcripts under `root` born since
    /// `started`.
    fn born_transcripts(
        source: &str,
        root: &Path,
        started: SystemTime,
        sandboxed: bool,
    ) -> Vec<PathBuf> {
        harvested(
            born_after(
                &pixtuoid_core::harness::transcripts_under(source, root),
                started,
            ),
            sandboxed,
        )
    }

    /// The seed's files, then the note: a seed carrying its own `NOTE_FILE`
    /// must not replace the one `note_survived` compares against.
    fn fill_workspace(ws: &Path, seed: Option<&NonEmpty>) -> std::io::Result<()> {
        if let Some(seed) = seed {
            copy_tree(Path::new(seed.as_str()), ws)?;
        }
        plant_note(ws)
    }

    fn plant_note(ws: &Path) -> std::io::Result<()> {
        std::fs::write(ws.join(NOTE_FILE), format!("{CAPTURE_NOTE}\n"))
    }

    /// Whether the note [`plant_note`] wrote is still there, unchanged.
    fn note_survived(workspace: &Path) -> bool {
        std::fs::read_to_string(workspace.join(NOTE_FILE))
            .is_ok_and(|note| note == format!("{CAPTURE_NOTE}\n"))
    }

    /// The root session the first turn's own wire names: the first its hook
    /// payloads decode to, else the one its source derives from its first
    /// transcript's path, as the watcher keys it.
    fn first_session(source: &str, payloads: &[String], transcripts: &[PathBuf]) -> Option<String> {
        Drive::hooks()
            .lines(payloads)
            .events
            .into_iter()
            .find_map(|e| match e {
                AgentEvent::SessionStart {
                    session_id,
                    parent_id: None,
                    ..
                } => Some(session_id),
                _ => None,
            })
            .or_else(|| {
                transcripts
                    .first()
                    .map(|t| registry::id_deriver_for(source)(t))
            })
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

    /// The file, beside a capture base, whose lock a capture there holds.
    const RECORD_LOCK: &str = "pixtuoid-capture.lock";

    /// The lock a capture in `base` holds until it ends, in the directory the
    /// base sits in: the capture root for the canonical workspace, which every
    /// recording clears, so they take turns; a replay's own root for its own,
    /// which nothing else locks.
    fn record_lock(base: &Path) -> std::io::Result<std::fs::File> {
        // Read off the base, never `capture_root`: on Windows that resolves
        // ProgramData, which the recorder's cleared environment can't.
        hold_lock(&base.with_file_name(RECORD_LOCK))
    }

    /// `ws` made afresh, and the lock held while the capture uses it.
    fn prepare_workspace(
        ws: PathBuf,
        seed: Option<&NonEmpty>,
    ) -> std::io::Result<(PathBuf, std::fs::File)> {
        let base = capture_base(&ws).ok_or_else(|| {
            std::io::Error::other(format!("{} is not a capture workspace", ws.display()))
        })?;
        let lock = record_lock(base)?;
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
            remove_once_released(RELEASE_PATIENCE, || std::fs::remove_dir_all(base))?;
        }
        // Made in one call no other local user can write into, and refused if
        // anyone re-created it since the remove.
        create_private_dir(base)?;
        std::fs::create_dir(&ws)?;
        fill_workspace(&ws, seed)?;
        init_repo(&ws)?;
        Ok((ws, lock))
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

    /// `cmd` with the CLI's wiring: the tap's endpoint, the driver's dir and
    /// the CLI it names, the pin, the agent-env scrub, then `env`.
    fn wired(
        cmd: &[String],
        ws: &Path,
        wiring: &CliWiring<'_>,
        verified: &str,
        env: &[(String, String)],
    ) -> std::io::Result<std::process::Command> {
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..])
            .current_dir(ws)
            .env(Endpoint::ENV, wiring.endpoint.path());
        let own = driver_dir(wiring.scratch);
        std::fs::create_dir_all(&own)?;
        c.env(DRIVER_DIR_ENV, own);
        c.env(CLI_ENV, named_cli(wiring.scratch))
            .env(VERSION_ENV, verified);
        for k in agent_env_names(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok())) {
            c.env_remove(k);
        }
        c.envs(env.iter().map(|(k, v)| (k, v)));
        Ok(c)
    }

    /// `plan`'s turn budget: measured on hermetic turns, so a billed one has none.
    fn turn_budget(plan: &CapturePlan) -> Option<Duration> {
        plan.sandboxed().then(|| *TURN_BUDGET.here())
    }

    /// How `turn` ended: killed once it outran `budget`, when it has one.
    fn run_turn(
        mut turn: std::process::Command,
        budget: Option<Duration>,
    ) -> std::io::Result<TurnEnd> {
        Ok(match budget {
            Some(budget) => {
                crate::bounded::run(turn, budget)?.map_or(TurnEnd::OutOfTime, TurnEnd::from)
            }
            None => turn.status()?.into(),
        })
    }

    /// How long a daemon gets to serve: its cold boot and a cold-starting probe
    /// after it, which on Windows outlast the rig's [`BOOT`].
    const DAEMON_BOOT: PerOs<Duration> = PerOs {
        unix: BOOT,
        windows: Duration::from_secs(120),
    };
    /// How long a daemon may take to exit once its stdin closed: a kill would
    /// lose its shutdown hooks' last sends, and openclaw's Windows stop,
    /// `process.exit` included, outlasts the Unix grace.
    const DAEMON_STOP_GRACE: PerOs<Duration> = PerOs {
        unix: Duration::from_secs(30),
        windows: Duration::from_secs(150),
    };
    /// How many last lines of its log, and of its probes', a daemon that never
    /// served carries in its error: a failed run's uploaded dir keeps the
    /// recorder's error; both logs stay in its scratch.
    const DAEMON_LOG_TAIL: usize = 40;

    /// A capture's daemon, run with the CLI's wiring so its hooks reach the
    /// tap. It stops when its stdin closes ([`hermetic::Daemon`]), which
    /// [`Self::stop`] does; one dropped unstopped is killed.
    struct RunningDaemon {
        child: std::process::Child,
        /// Its stdin's write end, held until it should stop.
        lifeline: Option<std::process::ChildStdin>,
        /// Its start, which names it.
        start: String,
    }

    /// A daemon's log, in its driver's dir.
    const DAEMON_LOG: &str = "daemon.log";

    impl RunningDaemon {
        /// `daemon` started, once its probe exits 0 within `boot`.
        fn start(
            daemon: Option<&DaemonPlan>,
            boot: Duration,
            ws: &Path,
            wiring: &CliWiring<'_>,
            verified: &str,
            env: &[(String, String)],
        ) -> std::io::Result<Option<Self>> {
            let Some(daemon) = daemon else {
                return Ok(None);
            };
            let own = driver_dir(wiring.scratch);
            std::fs::create_dir_all(&own)?;
            let log_path = own.join(DAEMON_LOG);
            let log = std::fs::File::create(&log_path)?;
            let (start, ready) = (daemon.start.strings(), daemon.ready.strings());
            let mut child = wired(&start, ws, wiring, verified, env)?
                .envs(daemon.env.iter())
                .stdin(std::process::Stdio::piped())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?;
            let mut running = Self {
                lifeline: child.stdin.take(),
                child,
                start: start.join(" "),
            };
            // Every probe's output, each followed by how it ended.
            let probe_log = own.join("probe.log");
            let mut probes = std::fs::File::create(&probe_log)?;
            let deadline = std::time::Instant::now() + boot;
            wait_until("the daemon", boot, || {
                let mut probe = wired(&ready, ws, wiring, verified, env)?;
                probe
                    .stdout(probes.try_clone()?)
                    .stderr(probes.try_clone()?);
                // A probe may take as long as a CLI's cold start; only one that
                // would hold the wait past `boot` is killed.
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                let status = crate::bounded::run(probe, left)?;
                match status {
                    Some(status) => writeln!(probes, "[the probe exited {status}]")?,
                    None => writeln!(probes, "[the probe was killed at the boot deadline]")?,
                }
                match running.child.try_wait()? {
                    // A probe answered by whatever else holds the port.
                    Some(exit) => Err(std::io::Error::other(format!(
                        "the daemon exited {exit} before it served"
                    ))),
                    None => Ok(status.is_some_and(|s| s.success())),
                }
            })
            .map_err(|e| {
                std::io::Error::other(format!(
                    "{e}; its probes ended:\n{}\nits log ends:\n{}",
                    tail(&probe_log),
                    tail(&log_path)
                ))
            })?;
            Ok(Some(running))
        }

        /// Closes its stdin and waits `grace` for it to exit. One that outlives
        /// it is killed, and that fails its capture: a kill loses the hooks its
        /// shutdown would have sent.
        fn stop(mut self, grace: Duration) -> std::io::Result<()> {
            drop(self.lifeline.take());
            if wait_until("its exit", grace, || Ok(self.child.try_wait()?.is_some())).is_ok() {
                return Ok(());
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            Err(std::io::Error::other(format!(
                "the daemon `{}` outlived its stdin by {grace:?} and was killed",
                self.start
            )))
        }
    }

    impl Drop for RunningDaemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The last [`DAEMON_LOG_TAIL`] lines of the log at `path`.
    fn tail(path: &Path) -> String {
        let text = std::fs::read(path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(DAEMON_LOG_TAIL)..].join("\n")
    }

    /// Why a capture is incomplete, in one line, if it is: its tap stopped
    /// accepting, its daemon had to be killed, or both.
    fn incomplete(tap: Option<String>, daemon: std::io::Result<()>) -> Option<std::io::Error> {
        let tap = tap.map(|why| format!("the tap stopped accepting: {why}"));
        let whys: Vec<String> = tap
            .into_iter()
            .chain(daemon.err().map(|e| e.to_string()))
            .collect();
        (!whys.is_empty()).then(|| std::io::Error::other(whys.join("; ")))
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

    /// A recording's record: `command` is the operator's argv, space-joined, so an
    /// override-driven scenario records the unexpanded [`Placeholder::Prompt`](pixtuoid_core::harness::plan::Placeholder::Prompt).
    fn record(
        cli: &str,
        version: &str,
        command: &str,
        deidentified: Deidentified,
    ) -> std::io::Result<Recorded> {
        Ok(Recorded {
            cli: filled("cli", cli)?,
            version: filled("version", version)?,
            captured: filled("captured", &today())?,
            command: filled("command", command)?,
            note: None,
            deidentified: Some(deidentified),
            prompt: None,
            seed: None,
            aimock: None,
            script: None,
            dropped: None,
        })
    }

    /// `value`, refused where it is blank: the record would not parse.
    fn filled(key: &str, value: &str) -> std::io::Result<NonEmpty> {
        NonEmpty::try_from(value).map_err(|e| std::io::Error::other(format!("{key}: {e}")))
    }

    fn write_provenance(dest: &Path, record: Recorded) -> std::io::Result<()> {
        let out = no_clobber(dest.join(PROVENANCE));
        let unknown = record.version.as_str() == "unknown";
        let cli = record.cli.clone();
        Provenance::Recorded(Box::new(record)).write_to(&out)?;
        // Loud, because the record just written is unusable.
        if unknown {
            eprintln!(
                "WARNING: {} records version \"unknown\" — `{cli} --version` did not \
                 answer, or a driver named no CLI at ${CLI_ENV}. Fix `cli` and `version` \
                 by hand before committing, or the anchor gate skips this source.",
                out.display()
            );
        }
        Ok(())
    }

    fn today() -> String {
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock is after 1970")
            .as_secs();
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
    /// The `HOME` is the operator's own except in a sandbox, where it is a temp
    /// path of the run's.
    fn identity_needles(git: impl Fn(&str) -> Option<String>, own_home: bool) -> BTreeSet<String> {
        let mut needles: BTreeSet<String> = BTreeSet::new();
        let vars: &[&str] = if own_home {
            &["HOME", "USER", "LOGNAME"]
        } else {
            &["USER", "LOGNAME"]
        };
        for var in vars {
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

    /// The identity a sandboxed run's logs may carry: [`identity_needles`] but
    /// for the `HOME`, which is the run's own there.
    fn operator_identity() -> BTreeSet<String> {
        identity_needles(git_config, false)
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
            report_not_json(&strip.not_json);
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

    fn report_not_json(not_json: &[(PathBuf, String)]) {
        for (file, error) in not_json {
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
    /// (`permission-hermetic` is hermes' and omp's), so a search finds the wrong
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
        use pixtuoid_core::harness::plan::Argv;
        use std::collections::BTreeMap;

        /// An operator nobody's bytes carry, so no test reads the machine's.
        const OPERATOR: &str = "Ada Lovelace";

        #[test]
        fn only_captures_of_one_workspace_take_turns() {
            let d = tempfile::tempdir().expect("tempdir");
            let own = d.path().join("pixtuoid-run-1");
            std::fs::create_dir(&own).expect("a replay's root");
            let base = |root: &Path| {
                capture_base(&root.join(capture_workspace_tail()))
                    .expect("a capture base")
                    .to_path_buf()
            };
            let held = record_lock(&base(d.path())).expect("the canonical lock");
            let replay = record_lock(&base(&own)).expect("a replay never waits on a recording");
            let other = std::fs::File::open(d.path().join(RECORD_LOCK)).expect("the lock file");
            assert!(other.try_lock().is_err(), "a second recording waits");
            drop(held);
            assert!(other.try_lock().is_ok());
            drop((other, replay));
        }

        struct Promotable {
            _dir: tempfile::TempDir,
            run: PathBuf,
            root: PathBuf,
            dest: PathBuf,
        }

        /// The run's raw capture is the committed recording with a key no
        /// decoder reads planted on line 1, then `edit`ed; the root's fixture
        /// holds `old/old.jsonl`, as a session dir holds its transcript, and
        /// the `record` given.
        fn promotable(
            record: serde_json::Value,
            edit: impl FnOnce(&mut [serde_json::Value]),
        ) -> Promotable {
            let d = tempfile::tempdir().expect("tempdir");
            let (run, root) = (d.path().join("run"), d.path().join("sources"));
            let fixture = hermetic::profile("dsh")
                .and_then(|p| p.scenario("tool-run"))
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
                    end: TurnEnd::Exited(0),
                    give_up_reason: None,
                    incomplete: None,
                    second_turn: None,
                    note_survived: true,
                },
                aimock: "1.43.0".into(),
                requests: 3,
                unmatched: 0,
                results_seen: 1,
            };
            std::fs::write(
                run.join(MANIFEST),
                serde_json::to_string(&manifest).expect("json"),
            )
            .expect("manifest");
            let dest = root.join(fixture);
            std::fs::create_dir_all(dest.join("old")).expect("mkdir");
            std::fs::write(dest.join("old").join("old.jsonl"), "{}\n").expect("old");
            std::fs::write(dest.join(PROVENANCE), complete_record(record).to_string())
                .expect("record");
            Promotable {
                _dir: d,
                run,
                root,
                dest,
            }
        }

        /// A committed record carrying every required field, with `over`'s keys
        /// laid over it: the recorder parses the record it replaces.
        fn complete_record(over: serde_json::Value) -> serde_json::Value {
            let mut record = serde_json::json!({
                "origin": "recorded",
                "cli": "dsh",
                "version": "0.2.0-rc.1",
                "captured": "2026-01-01",
                "command": "just hermetic --record dsh",
            });
            if let (Some(base), Some(over)) = (record.as_object_mut(), over.as_object()) {
                base.extend(over.clone());
            }
            record
        }

        fn needles() -> BTreeSet<String> {
            BTreeSet::from([OPERATOR.to_string()])
        }

        fn written(dest: &Path) -> (String, Recorded) {
            let bytes = std::fs::read_to_string(dest.join(HOOK_PAYLOADS)).expect("bytes");
            let record = Provenance::read(&dest.join(PROVENANCE)).expect("record");
            (bytes, record.recorded().expect("a recording").clone())
        }

        #[test]
        fn a_promoted_run_replaces_its_fixture_stripped_and_says_what_played_the_model() {
            let p = promotable(
                serde_json::json!({"origin": "recorded", "note": "the old record"}),
                |_| {},
            );
            promote(&p.run, &p.root, &needles(), &mut std::io::sink()).expect("promote");
            assert!(
                !p.dest.join("old").exists(),
                "the old bytes leave whole, with the dir that held them"
            );
            let (bytes, record) = written(&p.dest);
            assert!(!bytes.contains("unread"), "the planted key is stripped");
            assert!(
                matches!(record.deidentified, Some(Deidentified::Stripped { blanked }) if blanked > 0),
                "{record:?}"
            );
            assert_eq!(record.aimock.as_ref().map(NonEmpty::as_str), Some("1.43.0"));
            assert_eq!(
                record.script.as_ref().map(NonEmpty::as_str),
                Some("tests/sources/hermetic/dsh/tool-run.json")
            );
            assert_eq!(record.version.as_str(), "0.2.0-rc.2");
            assert_eq!(
                record.note, None,
                "the record is the run's, not the old one"
            );
            assert!(
                record.dropped.is_some(),
                "a stripped capture records its baseline, empty for hook payloads alone"
            );
        }

        /// The baseline is taken before the strip: after it, a type the decoder
        /// drops is gone from the bytes it would be read off.
        #[test]
        fn a_promoted_run_records_the_types_its_decoders_dropped() {
            let d = tempfile::tempdir().expect("tempdir");
            let (run, root) = (d.path().join("run"), d.path().join("sources"));
            let fixture = hermetic::profile("copilot")
                .and_then(|p| p.scenario("tool-run"))
                .map(|s| s.fixture)
                .expect("copilot's tool-run");
            for from in jsonl_files_under(&sources_root().join(fixture)) {
                let rel = from
                    .strip_prefix(sources_root().join(fixture))
                    .expect("under");
                let to = run.join(rel);
                std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
                let mut text = std::fs::read_to_string(&from).expect("read");
                if from.ends_with("events.jsonl") {
                    text.push_str("{\"type\":\"assistant.zz_dropped\",\"data\":{\"x\":\"y\"}}\n");
                }
                std::fs::write(&to, text).expect("write");
            }
            let manifest = Manifest {
                source: "copilot".into(),
                scenario: "tool-run".into(),
                capture: CaptureRecord {
                    workspace: capture_workspace().expect("the workspace"),
                    cli: "copilot".into(),
                    version: "1.0.98".into(),
                    end: TurnEnd::Exited(0),
                    give_up_reason: None,
                    incomplete: None,
                    second_turn: None,
                    note_survived: true,
                },
                aimock: "1.45.1".into(),
                requests: 2,
                unmatched: 0,
                results_seen: 1,
            };
            std::fs::write(
                run.join(MANIFEST),
                serde_json::to_string(&manifest).expect("json"),
            )
            .expect("manifest");
            let dest = root.join(fixture);
            std::fs::create_dir_all(&dest).expect("mkdir");
            std::fs::write(
                dest.join(PROVENANCE),
                complete_record(serde_json::json!({"origin": "recorded"})).to_string(),
            )
            .expect("record");
            promote(&run, &root, &needles(), &mut std::io::sink()).expect("promote");
            let record = Provenance::read(&dest.join(PROVENANCE)).expect("record");
            let dropped = record.dropped().expect("a baseline");
            let json = serde_json::to_value(dropped).expect("json");
            assert_eq!(json["os"], std::env::consts::OS, "{json}");
            assert!(
                json["at"]["/type"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|t| t == "assistant.zz_dropped")),
                "the planted type the decoder drops is the baseline's: {json}"
            );
        }

        #[test]
        fn a_promoted_line_that_is_not_json_is_named_where_the_fixture_keeps_it() {
            let p = promotable(serde_json::json!({"origin": "recorded"}), |_| {});
            let raw = p.run.join(HOOK_PAYLOADS);
            let mut text = std::fs::read_to_string(&raw).expect("raw");
            text.push_str("not json\n");
            std::fs::write(&raw, text).expect("write");
            let mut notices = Vec::new();
            promote(&p.run, &p.root, &needles(), &mut notices).expect("promote");
            let notices = String::from_utf8(notices).expect("text");
            let named = format!("not stripping {}:", p.dest.join(HOOK_PAYLOADS).display());
            assert!(
                matches!(&notices.lines().collect::<Vec<_>>()[..], [n] if n.starts_with(&named)),
                "{notices}"
            );
        }

        #[test]
        fn a_promoted_run_carrying_the_operator_is_refused() {
            let p = promotable(serde_json::json!({"origin": "recorded"}), |lines| {
                lines[0]["cwd"] = format!("/Users/{OPERATOR}/proj").into();
            });
            assert!(
                promote(&p.run, &p.root, &needles(), &mut std::io::sink()).is_err(),
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
            promote(&p.run, &p.root, &needles(), &mut std::io::sink()).expect("promote");
            let (bytes, record) = written(&p.dest);
            assert!(
                bytes.contains("unread"),
                "an exempt capture is not stripped"
            );
            assert_eq!(
                record.deidentified,
                Some(Deidentified::Exempt {
                    why: why.try_into().expect("a reason")
                })
            );
            assert_eq!(
                record.dropped, None,
                "an unstripped capture drops nothing it could baseline"
            );
        }

        const AIMOCK: &str = "1.45.1";

        fn dsh_version() -> &'static str {
            hermetic::profile("dsh")
                .expect("dsh's profile")
                .descriptor()
                .verified_version
        }

        /// `c` moved one step within its class (digit, `a`-`f`, `g`-`z`,
        /// `A`-`Z`), so a renamed id keeps its family's shape.
        fn rotated(c: char) -> char {
            let step = |c: char, lo: char, hi: char| {
                if c == hi {
                    lo
                } else {
                    char::from_u32(c as u32 + 1).unwrap_or(c)
                }
            };
            match c {
                '0'..='9' => step(c, '0', '9'),
                'a'..='f' => step(c, 'a', 'f'),
                'g'..='z' => step(c, 'g', 'z'),
                'A'..='Z' => step(c, 'A', 'Z'),
                _ => c,
            }
        }

        /// `text` with every id dsh mints renamed as a second run renames it:
        /// each session and tool call past its prefix, the pid and the clock.
        fn renamed(text: &str) -> String {
            let mut ids = BTreeSet::new();
            for line in text.lines() {
                let v: serde_json::Value = serde_json::from_str(line).expect("json");
                for key in ["sessionId", "parentSession", "callId"] {
                    if let Some(id) = v[key].as_str().filter(|id| !id.is_empty()) {
                        ids.insert(id.to_owned());
                    }
                }
            }
            let mut out = text.to_owned();
            for id in ids {
                let cut = id.find(['-', '_']).map_or(0, |i| i + 1);
                let new: String = id[..cut]
                    .chars()
                    .chain(id[cut..].chars().map(rotated))
                    .collect();
                out = out.replace(&id, &new);
            }
            out.replace("\"_pid\":", "\"_pid\":9")
                .replace("\"_shim_ts_ms\":1", "\"_shim_ts_ms\":2")
        }

        /// dsh's committed `scenario`, under the record its run writes, then
        /// `record`-edited, and a run of it whose bytes are those with every id
        /// renamed, then `run`-edited, under the dsh and aimock versions given.
        fn rerun(
            scenario: &str,
            versions: (&str, &str),
            record: impl FnOnce(&mut serde_json::Value),
            run: impl Fn(String) -> String,
        ) -> Promotable {
            let d = tempfile::tempdir().expect("tempdir");
            let (run_dir, root) = (d.path().join("run"), d.path().join("sources"));
            let s = hermetic::profile("dsh")
                .and_then(|p| p.scenario(scenario))
                .expect("dsh's scenario");
            let dest = root.join(s.fixture);
            for from in jsonl_files_under(&sources_root().join(s.fixture)) {
                let rel = from
                    .strip_prefix(sources_root().join(s.fixture))
                    .expect("under");
                let text = std::fs::read_to_string(&from).expect("read");
                for (to, text) in [
                    (dest.join(rel), text.clone()),
                    (run_dir.join(rel), run(renamed(&text))),
                ] {
                    std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
                    std::fs::write(to, text).expect("write");
                }
            }
            let mut over = serde_json::json!({
                "origin": "recorded",
                "version": dsh_version(),
                "deidentified": {"method": "decoder-allowlist", "blanked": 0},
                "aimock": AIMOCK,
                "script": format!("tests/sources/hermetic/dsh/{scenario}.json"),
                "dropped": {"os": std::env::consts::OS, "at": {}},
            });
            record(&mut over);
            let committed_record: Provenance =
                serde_json::from_value(complete_record(over)).expect("a record");
            committed_record
                .write_to(&dest.join(PROVENANCE))
                .expect("record");
            let manifest = Manifest {
                source: "dsh".into(),
                scenario: scenario.into(),
                capture: CaptureRecord {
                    workspace: capture_workspace().expect("the workspace"),
                    cli: "dsh".into(),
                    version: versions.0.into(),
                    end: TurnEnd::Exited(0),
                    give_up_reason: None,
                    incomplete: None,
                    second_turn: None,
                    note_survived: true,
                },
                aimock: versions.1.into(),
                requests: 2,
                unmatched: 0,
                results_seen: 1,
            };
            std::fs::write(
                run_dir.join(MANIFEST),
                serde_json::to_string(&manifest).expect("json"),
            )
            .expect("manifest");
            Promotable {
                _dir: d,
                run: run_dir,
                root,
                dest,
            }
        }

        /// The fixture's files and record, as bytes.
        fn snapshot(dest: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
            jsonl_files_under(dest)
                .into_iter()
                .chain([dest.join(PROVENANCE)])
                .map(|f| (f.clone(), std::fs::read(&f).expect("read")))
                .collect()
        }

        /// Promotes `p`, and says whether the fixture came through untouched.
        fn kept(p: &Promotable) -> bool {
            let before = snapshot(&p.dest);
            promote(&p.run, &p.root, &needles(), &mut std::io::sink()).expect("promote");
            let after = snapshot(&p.dest);
            if after != before {
                let record = Provenance::read(&p.dest.join(PROVENANCE)).expect("record");
                assert_eq!(
                    record.recorded().map(|r| r.captured.as_str()),
                    Some(today().as_str()),
                    "a rewrite is the run's own record"
                );
            }
            after == before
        }

        fn unchanged(text: String) -> String {
            text
        }

        #[test]
        fn a_re_record_that_renamed_only_ids_keeps_the_fixture_whole() {
            let p = rerun("tool-run", (dsh_version(), AIMOCK), |_| {}, unchanged);
            assert!(kept(&p), "only ids differ");
        }

        #[test]
        fn a_re_record_that_reparented_an_id_is_rewritten() {
            let p = rerun(
                "subagent",
                (dsh_version(), AIMOCK),
                |_| {},
                |text| {
                    let parent = text
                        .lines()
                        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                        .find_map(|v| {
                            v["parentSession"]
                                .as_str()
                                .filter(|p| !p.is_empty())
                                .map(str::to_owned)
                        })
                        .expect("a child names its parent");
                    text.replace(
                        &format!("\"parentSession\":\"{parent}\""),
                        "\"parentSession\":\"session-00000000-0000-4000-8000-000000000000\"",
                    )
                },
            );
            assert!(!kept(&p), "the child hangs off another session");
        }

        #[test]
        fn a_re_record_under_another_cli_version_is_rewritten() {
            let p = rerun("tool-run", ("0.2.0-rc.3", AIMOCK), |_| {}, unchanged);
            assert!(!kept(&p));
        }

        #[test]
        fn a_re_record_under_another_aimock_is_rewritten() {
            let p = rerun("tool-run", (dsh_version(), "1.46.0"), |_| {}, unchanged);
            assert!(!kept(&p));
        }

        #[test]
        fn a_re_record_of_a_moved_script_is_rewritten() {
            let p = rerun(
                "tool-run",
                (dsh_version(), AIMOCK),
                |r| r["script"] = "tests/sources/hermetic/dsh/moved.json".into(),
                unchanged,
            );
            assert!(!kept(&p), "a kept record would name a script that is gone");
        }

        #[test]
        fn a_re_record_whose_baseline_differs_is_rewritten_whole() {
            for committed in [None, Some(serde_json::json!({"os": "plan9", "at": {}}))] {
                let p = rerun(
                    "tool-run",
                    (dsh_version(), AIMOCK),
                    |r| match committed.clone() {
                        Some(d) => r["dropped"] = d,
                        None => {
                            r.as_object_mut().expect("an object").remove("dropped");
                        }
                    },
                    unchanged,
                );
                assert!(!kept(&p), "{committed:?}: the bytes go with their baseline");
                assert_ne!(
                    std::fs::read_to_string(p.dest.join(HOOK_PAYLOADS)).expect("bytes"),
                    std::fs::read_to_string(
                        sources_root()
                            .join("fixtures/dsh/tool-run-recorded")
                            .join(HOOK_PAYLOADS)
                    )
                    .expect("committed"),
                    "{committed:?}: the run's renamed bytes replaced the committed ones"
                );
            }
        }

        #[test]
        fn the_workspace_becomes_its_own_repository_or_fails() {
            let d = tempfile::tempdir().expect("tempdir");
            let ws = d.path().join("ws");
            std::fs::create_dir(&ws).expect("mkdir");
            plant_note(&ws).expect("plant");
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
            let root = std::env::temp_dir();
            assert!(
                workspace_refusal(&root.join("proj")).is_some(),
                "its parent is the operator's"
            );
            let tail = capture_workspace_tail();
            let why = workspace_refusal(&tail).expect("a relative path with the right tail");
            assert!(
                why.contains("absolute"),
                "refused for being relative: {why}"
            );
            assert!(
                why.contains(&format!("end in {}:", tail.display())),
                "names the tail it checks, never a root any absolute one may have: {why}"
            );
            assert_eq!(workspace_refusal(&root.join(&tail)), None);
        }

        fn operator_plan(args: &[&str]) -> CapturePlan {
            let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
            match Invocation::parse_process(&args).expect("a line") {
                Invocation::Capture(plan) => *plan,
                other => panic!("not a capture: {other:?}"),
            }
        }

        #[test]
        fn only_a_hermetic_turn_has_a_budget() {
            let mut raw = operator_plan(&["--out", "x", "kimi", "tool-run", "kimi"]);
            assert_eq!(turn_budget(&raw), None, "an operator's billed raw capture");
            assert_eq!(
                turn_budget(&operator_plan(&["kimi", "tool-run", "kimi"])),
                None
            );
            raw.within = Some(
                pixtuoid_core::harness::plan::AbsPath::try_from(std::env::temp_dir()).expect("abs"),
            );
            assert_eq!(turn_budget(&raw), Some(*TURN_BUDGET.here()));
        }

        #[test]
        fn a_plan_the_host_may_not_run_is_refused_before_it_starts() {
            let committed = operator_plan(&["kimi", "tool-run", "kimi"]);
            assert_eq!(plan_refusal(&committed).is_some(), !cfg!(unix));
            let mut raw = operator_plan(&["--out", "x", "kimi", "tool-run", "kimi"]);
            assert_eq!(plan_refusal(&raw), None, "a raw capture is any host's");
            let abs = |p: PathBuf| pixtuoid_core::harness::plan::AbsPath::try_from(p).expect("abs");
            raw.workspace = Some(abs(std::env::temp_dir().join("proj")));
            assert!(
                plan_refusal(&raw).is_some_and(|why| why.contains("clears its parent")),
                "its parent is the operator's"
            );
            raw.workspace = Some(abs(std::env::temp_dir().join(capture_workspace_tail())));
            assert_eq!(plan_refusal(&raw), None);
        }

        #[test]
        fn a_refused_promotion_leaves_the_fixture_whole_and_says_why() {
            let p = promotable(serde_json::json!({"origin": "recorded"}), |_| {});
            let refused = promote_unless_refused(
                Some("not this host"),
                &p.run,
                &p.root,
                &needles(),
                &mut std::io::sink(),
            );
            assert_eq!(refused.err(), Some("not this host"));
            assert!(
                p.dest.join("old").join("old.jsonl").is_file(),
                "a promotion would have replaced the fixture whole"
            );
            assert!(!p.dest.join(HOOK_PAYLOADS).exists());
        }

        #[test]
        fn a_promotion_on_this_host_is_refused_only_where_the_corpus_is_not_its_to_write() {
            let p = promotable(serde_json::json!({"origin": "recorded"}), |_| {});
            let here = promote_here(&p.run, &p.root, &needles(), &mut std::io::sink());
            assert_eq!(here.is_err(), !cfg!(unix), "{here:?}");
            assert_eq!(p.dest.join(HOOK_PAYLOADS).exists(), cfg!(unix));
        }

        #[test]
        fn only_a_named_seed_seeds() {
            let d = tempfile::tempdir().expect("tempdir");
            let (none, missing) = (d.path().join("none"), d.path().join("missing"));
            std::fs::create_dir(&none).expect("mkdir");
            std::fs::create_dir(&missing).expect("mkdir");
            assert!(fill_workspace(&none, None).is_ok());
            let named = NonEmpty::try_from("/no/such").ok();
            assert!(
                fill_workspace(&missing, named.as_ref()).is_err(),
                "a named seed that is missing fails the copy"
            );
        }

        #[test]
        fn a_seed_that_carries_a_note_does_not_replace_the_planted_one() {
            let d = tempfile::tempdir().expect("tempdir");
            let (seed, ws) = (d.path().join("seed"), d.path().join("ws"));
            std::fs::create_dir(&seed).expect("mkdir");
            std::fs::create_dir(&ws).expect("mkdir");
            std::fs::write(seed.join(NOTE_FILE), "seeded\n").expect("write");
            fill_workspace(
                &ws,
                NonEmpty::try_from(seed.display().to_string()).ok().as_ref(),
            )
            .expect("fill");
            assert!(note_survived(&ws));
        }

        #[test]
        fn the_note_survives_only_while_it_is_there_unchanged() {
            let d = tempfile::tempdir().expect("tempdir");
            assert!(!note_survived(d.path()), "a deleted note did not survive");
            plant_note(d.path()).expect("plant");
            assert!(note_survived(d.path()), "the planted note is the survivor");
            std::fs::write(d.path().join(NOTE_FILE), "other\n").expect("write");
            assert!(!note_survived(d.path()), "a rewritten note did not survive");
        }

        #[test]
        fn the_second_turns_session_is_the_one_the_first_turns_wire_names() {
            let cc = sources_root().join("fixtures/claude-code/ultra-effort-hermetic");
            let payloads: Vec<String> = std::fs::read_to_string(cc.join(HOOK_PAYLOADS))
                .expect("payloads")
                .lines()
                .map(str::to_owned)
                .collect();
            let transcript = pixtuoid_core::harness::transcript_in("claude-code", &cc)
                .expect("the one transcript");
            let named = first_session("claude-code", &payloads, &[]).expect("a hook-named session");
            assert_eq!(
                transcript.file_stem().and_then(|s| s.to_str()),
                Some(named.as_str()),
                "claude-code's hooks name the session its transcript keys"
            );
            // copilot fires no hooks; its session rides its transcript's path.
            let cp = sources_root().join("fixtures/copilot/resume-hermetic");
            let events =
                pixtuoid_core::harness::transcript_in("copilot", &cp).expect("the one transcript");
            let dir = events
                .parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                .expect("the session dir");
            assert_eq!(
                first_session("copilot", &[], std::slice::from_ref(&events)).as_deref(),
                Some(dir)
            );
            assert_eq!(first_session("copilot", &[], &[]), None);
        }

        #[test]
        fn a_root_is_prepared_only_inside_the_sandbox() {
            let sandbox = tempfile::tempdir().expect("tempdir");
            let elsewhere = tempfile::tempdir().expect("tempdir");
            let inside = sandbox.path().join("homes/GROK_HOME/sessions");
            prepare_root(Some(&inside), sandbox.path()).expect("inside");
            assert!(inside.is_dir());

            let outside = elsewhere.path().join("sessions");
            assert!(prepare_root(Some(&outside), sandbox.path()).is_err());
            assert!(!outside.exists(), "a refusal creates nothing");
            assert!(
                prepare_root(None, sandbox.path()).is_ok(),
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

            let mine = root.join("omp/fixtures/permission-hermetic");
            std::fs::create_dir_all(&mine).expect("mkdir");
            std::fs::write(mine.join("provenance.json"), "{}").expect("write");
            assert_eq!(scenario_dest(root, "omp", "permission-hermetic"), mine);
            assert_eq!(
                scenario_dest(root, "hermes", "permission-hermetic"),
                root.join("fixtures/hermes/permission-hermetic")
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
            let needles = identity_needles(
                |k| match k {
                    "user.name" => Some("Ada Lovelace".to_string()),
                    "user.email" => Some("ada@example.org".to_string()),
                    _ => None,
                },
                true,
            );
            assert!(needles.contains("Ada Lovelace"), "{needles:?}");
            assert!(needles.contains("ada@example.org"), "{needles:?}");
            assert!(!identity_needles(|_| None, true).contains("Ada Lovelace"));
        }

        #[test]
        fn a_sandboxed_runs_home_is_not_the_operators_identity() {
            let home = "/home/ada-lovelace";
            let needles = |own_home| {
                temp_env::with_vars(
                    [
                        ("HOME", Some(home)),
                        ("USER", Some("ada")),
                        ("LOGNAME", None),
                    ],
                    || identity_needles(|_| None, own_home),
                )
            };
            assert!(needles(true).contains(home));
            assert!(
                !needles(false).contains(home),
                "the sandbox's home is the run's"
            );
            assert!(needles(false).contains("ada"), "the user passes through");
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
            // agy keys its session on the conversation dir `ag_id_from_path` names.
            let ag =
                Path::new("/sb/.gemini/antigravity-cli/brain/9/.system_generated/logs/t.jsonl");
            assert_eq!(
                placement("antigravity", ag, dest),
                dest.join("9/.system_generated/logs/t.jsonl")
            );
            // Keyed on the whole path, which any placement changes.
            let whole = Path::new("/sb/x/9/t.jsonl");
            assert_eq!(placement("not-a-source", whole, dest), dest.join("t.jsonl"));
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
                   && test "$CLAUDE_CODE_PROBE" = 1 && test "${DRIVER_DIR_ENV}" = "{}" \
                   && test -d "${DRIVER_DIR_ENV}" && echo dsh >"${CLI_ENV}""#,
                driver_dir(sandbox.path()).display()
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
                    wired(&cmd, ws.path(), &wiring, "1.0.0", &env)
                        .and_then(|mut c| c.status())
                        .expect("runs")
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

        #[cfg(unix)]
        #[test]
        fn a_daemon_serves_before_the_cli_runs() {
            let sandbox = tempfile::tempdir().expect("sandbox");
            let ws = tempfile::tempdir().expect("workspace");
            let endpoint = Endpoint::in_dir(sandbox.path(), "capture");
            let ready = ws.path().join("ready");
            // It serves only after a beat, so a probe that never waited fails.
            let serve = format!("sleep 1; touch {}; cat >/dev/null", ready.display());
            let daemon = daemon_of(
                &["sh", "-c", &serve],
                &["test", "-e", &ready.display().to_string()],
                &[],
            );
            let wiring = CliWiring {
                endpoint: &endpoint,
                scratch: sandbox.path(),
            };
            let running = RunningDaemon::start(
                Some(&daemon),
                *DAEMON_BOOT.here(),
                ws.path(),
                &wiring,
                "1.0.0",
                &[],
            )
            .expect("starts")
            .expect("a daemon");
            assert!(ready.exists(), "returned before it served");
            running.stop(*DAEMON_STOP_GRACE.here()).expect("it stops");
        }

        /// Where a re-run of this test binary as the fake daemon keeps its
        /// markers: the env var marks the re-run.
        const FAKE_DAEMON: &str = "PIXTUOID_TEST_FAKE_DAEMON";
        /// A second libtest filter, naming no test, that marks the fake's probe.
        const FAKE_PROBE: &str = "fake-daemon-probe";
        /// What the fake's probe says while the fake does not serve.
        const NOT_YET: &str = "the fake does not serve yet";

        /// `test`'s name as libtest filters it: module path without the crate.
        fn this_test(test: &str) -> String {
            let module = module_path!()
                .split_once("::")
                .map_or(module_path!(), |(_, m)| m);
            format!("{module}::{test}")
        }

        /// This test binary, re-run as `test` playing a daemon, and as its probe,
        /// which passes once the fake wrote its `ready`.
        fn fake_daemon(test: &str) -> DaemonPlan {
            let exe = std::env::current_exe()
                .expect("the test binary")
                .display()
                .to_string();
            let name = this_test(test);
            daemon_of(
                &[&exe, "--exact", &name, "--nocapture"],
                &[&exe, "--exact", &name, FAKE_PROBE, "--nocapture"],
                &[],
            )
        }

        /// A daemon plan from plain words, as a test spells one.
        fn daemon_of(start: &[&str], ready: &[&str], env: &[(&str, &str)]) -> DaemonPlan {
            DaemonPlan {
                start: Argv::resolved(start).expect("words"),
                ready: Argv::resolved(ready).expect("words"),
                env: env
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            }
        }

        /// In a re-run as the fake: its marker directory, and whether it is the
        /// probe, which has taken `probe_takes` and asserted the fake serves by
        /// the time this returns. A fake that `serves` says so before it returns.
        fn as_fake(serves: bool, probe_takes: Duration) -> Option<(PathBuf, bool)> {
            let dir = PathBuf::from(std::env::var_os(FAKE_DAEMON)?);
            let probe = std::env::args().any(|a| a == FAKE_PROBE);
            if probe {
                std::thread::sleep(probe_takes);
                assert!(dir.join("ready").exists(), "{NOT_YET}");
            } else if serves {
                std::fs::write(dir.join("ready"), "").expect("ready");
            }
            Some((dir, probe))
        }

        /// The fake daemon, started within `boot` as the recorder starts one,
        /// in `dir`: its workspace, its scratch and its markers, which outlive it.
        fn start_fake(test: &str, dir: &Path, boot: Duration) -> std::io::Result<RunningDaemon> {
            let endpoint = Endpoint::in_dir(dir, "capture");
            let wiring = CliWiring {
                endpoint: &endpoint,
                scratch: dir,
            };
            let env = [(FAKE_DAEMON.to_owned(), dir.display().to_string())];
            RunningDaemon::start(Some(&fake_daemon(test)), boot, dir, &wiring, "1.0.0", &env)
                .map(|d| d.expect("a daemon"))
        }

        /// Not `cfg(unix)`: the lifeline is the recorder's one stop on every OS.
        #[test]
        fn a_daemon_stops_when_its_stdin_closes() {
            const TEST: &str = "a_daemon_stops_when_its_stdin_closes";
            if let Some((dir, probe)) = as_fake(true, Duration::ZERO) {
                if !probe {
                    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).expect("stdin");
                    // An EOF that came before the recorder stopped it is a
                    // stdin it never held.
                    let when = if dir.join("stopping").exists() {
                        "after stop"
                    } else {
                        "before stop"
                    };
                    std::fs::write(dir.join("stopped"), when).expect("stopped");
                }
                return;
            }
            let markers = tempfile::tempdir().expect("markers");
            let running = start_fake(TEST, markers.path(), *DAEMON_BOOT.here()).expect("starts");
            std::fs::write(markers.path().join("stopping"), "").expect("stopping");
            running
                .stop(*DAEMON_STOP_GRACE.here())
                .expect("it stops on its own");
            assert_eq!(
                std::fs::read_to_string(markers.path().join("stopped"))
                    .ok()
                    .as_deref(),
                Some("after stop"),
                "the daemon saw EOF only once the recorder stopped it"
            );
        }

        #[test]
        fn a_daemons_own_env_reaches_its_spawn_and_not_its_probe() {
            const TEST: &str = "a_daemons_own_env_reaches_its_spawn_and_not_its_probe";
            const OWN: &str = "PIXTUOID_TEST_FAKE_DAEMON_OWN";
            if let Some((dir, probe)) = as_fake(true, Duration::ZERO) {
                if probe {
                    assert!(std::env::var_os(OWN).is_none(), "the probe got it");
                } else {
                    let own = std::env::var_os(OWN).unwrap_or_default();
                    std::fs::write(dir.join("own"), own.as_encoded_bytes()).expect("own");
                    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).expect("stdin");
                }
                return;
            }
            let dir = tempfile::tempdir().expect("dir");
            let endpoint = Endpoint::in_dir(dir.path(), "capture");
            let wiring = CliWiring {
                endpoint: &endpoint,
                scratch: dir.path(),
            };
            let env = [(FAKE_DAEMON.to_owned(), dir.path().display().to_string())];
            let daemon = DaemonPlan {
                env: BTreeMap::from([(OWN.to_owned(), "1".to_owned())]),
                ..fake_daemon(TEST)
            };
            RunningDaemon::start(
                Some(&daemon),
                *DAEMON_BOOT.here(),
                dir.path(),
                &wiring,
                "1.0.0",
                &env,
            )
            .expect("it serves")
            .expect("a daemon")
            .stop(*DAEMON_STOP_GRACE.here())
            .expect("it stops");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("own"))
                    .ok()
                    .as_deref(),
                Some("1")
            );
        }

        #[test]
        fn a_daemon_that_never_serves_carries_both_logs_into_the_error() {
            const TEST: &str = "a_daemon_that_never_serves_carries_both_logs_into_the_error";
            const SAYS: &str = "the fake daemon will not serve";
            if let Some((_, probe)) = as_fake(false, Duration::ZERO) {
                if !probe {
                    println!("{SAYS}");
                    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).expect("stdin");
                }
                return;
            }
            let dir = tempfile::tempdir().expect("dir");
            let why = start_fake(TEST, dir.path(), Duration::from_secs(2))
                .map(|_| ())
                .expect_err("it never serves")
                .to_string();
            assert!(why.contains("never came up"), "{why}");
            assert!(why.contains(SAYS), "its log: {why}");
            assert!(why.contains(NOT_YET), "its probes: {why}");
        }

        #[test]
        fn a_daemon_that_outlives_its_stdin_is_killed_and_fails_the_capture() {
            const TEST: &str = "a_daemon_that_outlives_its_stdin_is_killed_and_fails_the_capture";
            /// Past any grace this test gives it.
            const OUTLIVES: Duration = Duration::from_secs(600);
            const SAYS: &str = "the fake daemon will not stop";
            if let Some((_, probe)) = as_fake(true, Duration::ZERO) {
                if !probe {
                    println!("{SAYS}");
                    std::thread::sleep(OUTLIVES);
                }
                return;
            }
            let markers = tempfile::tempdir().expect("markers");
            let running = start_fake(TEST, markers.path(), *DAEMON_BOOT.here()).expect("starts");
            let grace = Duration::from_secs(1);
            let log = driver_dir(markers.path()).join(DAEMON_LOG);
            let started = std::time::Instant::now();
            let why = running.stop(grace).expect_err("a killed daemon fails");
            assert!(started.elapsed() >= grace, "killed before its grace");
            let why = why.to_string();
            assert!(why.contains(&this_test(TEST)), "names the daemon: {why}");
            assert!(
                why.contains(&format!("{grace:?}")),
                "names the grace: {why}"
            );
            assert!(!why.contains('\n'), "a report's one line: {why}");
            let said = std::fs::read_to_string(&log).expect("its log");
            assert!(said.contains(SAYS), "its log keeps what it said: {said}");
            assert!(
                incomplete(None, Err(std::io::Error::other(why))).is_some(),
                "a capture whose daemon was killed is incomplete"
            );
            assert!(incomplete(None, Ok(())).is_none());
        }

        #[test]
        fn a_capture_whose_tap_stopped_and_daemon_was_killed_names_both() {
            const TAP: &str = "the tap's own reason";
            const KILL: &str = "the daemon's kill";
            let why = incomplete(Some(TAP.into()), Err(std::io::Error::other(KILL)))
                .expect("incomplete")
                .to_string();
            assert!(why.contains(TAP), "{why}");
            assert!(why.contains(KILL), "{why}");
            assert!(!why.contains('\n'), "a report's one line: {why}");
        }

        #[cfg(unix)]
        #[test]
        fn a_daemon_that_exited_is_refused_though_its_probe_answers() {
            let sandbox = tempfile::tempdir().expect("sandbox");
            let ws = tempfile::tempdir().expect("workspace");
            let endpoint = Endpoint::in_dir(sandbox.path(), "capture");
            let daemon = daemon_of(
                &["sh", "-c", "echo out of disk; exit 3"],
                &["sh", "-c", "sleep 1"],
                &[],
            );
            let wiring = CliWiring {
                endpoint: &endpoint,
                scratch: sandbox.path(),
            };
            let why = RunningDaemon::start(
                Some(&daemon),
                *DAEMON_BOOT.here(),
                ws.path(),
                &wiring,
                "1.0.0",
                &[],
            )
            .map(|_| ())
            .expect_err("its probe answered for someone else");
            assert!(why.to_string().contains("exited"), "{why}");
            assert!(why.to_string().contains("out of disk"), "its log: {why}");
        }

        #[test]
        fn a_probe_that_blocks_is_killed_at_the_boot_deadline() {
            const TEST: &str = "a_probe_that_blocks_is_killed_at_the_boot_deadline";
            /// Past any boot this test gives it.
            const BLOCKS: Duration = Duration::from_secs(600);
            if let Some((_, probe)) = as_fake(true, BLOCKS) {
                if !probe {
                    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).expect("stdin");
                }
                return;
            }
            let dir = tempfile::tempdir().expect("dir");
            let boot = Duration::from_secs(2);
            let started = std::time::Instant::now();
            let why = start_fake(TEST, dir.path(), boot)
                .map(|_| ())
                .expect_err("its probe never answered")
                .to_string();
            assert!(why.contains("killed at the boot deadline"), "{why}");
            assert!(started.elapsed() < boot * 2, "the probe outlived its boot");
        }

        /// Where a re-run of this test binary as a fake turn keeps its markers:
        /// the env var marks the re-run.
        const FAKE_TURN: &str = "PIXTUOID_TEST_FAKE_TURN";
        /// A second libtest filter, naming no test, that marks the fake turn's
        /// grandchild.
        const FAKE_GRANDCHILD: &str = "fake-turn-grandchild";
        /// A third, marking a re-run as the recorder running the fake turn.
        const FAKE_RECORDER: &str = "fake-recorder";
        /// Past any budget a test gives a fake turn.
        const OUTLIVES: Duration = Duration::from_secs(600);
        /// How often the fake turn's grandchild says it still runs.
        const BEAT: Duration = Duration::from_millis(100);
        /// The file the grandchild beats into.
        const BEATS: &str = "beats";

        /// This test binary, re-run as `test`'s fake turn with its markers in
        /// `dir`: it starts a grandchild that beats until it is killed, and
        /// outlives any budget.
        fn fake_turn(test: &str, dir: &Path) -> std::process::Command {
            let mut turn =
                std::process::Command::new(std::env::current_exe().expect("the test binary"));
            turn.args(["--exact", &this_test(test), "--nocapture"])
                .env(FAKE_TURN, dir);
            turn
        }

        /// In a re-run as `test`'s fake turn or its grandchild, plays it and
        /// returns its marker dir.
        fn as_fake_turn(test: &str) -> Option<PathBuf> {
            let dir = PathBuf::from(std::env::var_os(FAKE_TURN)?);
            let plays = |role: &str| std::env::args().any(|a| a == role);
            if plays(FAKE_GRANDCHILD) {
                let started = std::time::Instant::now();
                while started.elapsed() < OUTLIVES {
                    let mut beats = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join(BEATS))
                        .expect("beats");
                    beats.write_all(b".").expect("beat");
                    std::thread::sleep(BEAT);
                }
            } else if !plays(FAKE_RECORDER) {
                let mut grandchild = fake_turn(test, &dir)
                    .arg(FAKE_GRANDCHILD)
                    .spawn()
                    .expect("the grandchild");
                std::thread::sleep(OUTLIVES);
                let _ = grandchild.wait();
            }
            Some(dir)
        }

        /// How many beats the grandchild in `dir` has beaten.
        fn beaten(dir: &Path) -> u64 {
            std::fs::metadata(dir.join(BEATS)).map_or(0, |m| m.len())
        }

        #[test]
        fn a_turn_past_its_budget_is_killed_with_its_children() {
            const TEST: &str = "a_turn_past_its_budget_is_killed_with_its_children";
            if as_fake_turn(TEST).is_some() {
                return;
            }
            let dir = tempfile::tempdir().expect("dir");
            let budget = Duration::from_secs(4);
            let started = std::time::Instant::now();
            let end = run_turn(fake_turn(TEST, dir.path()), Some(budget)).expect("it ran");
            assert_eq!(end, TurnEnd::OutOfTime);
            assert!(started.elapsed() < budget * 2, "it outlived its budget");
            let before = beaten(dir.path());
            assert!(before > 0, "the grandchild never ran, so it proves nothing");
            std::thread::sleep(BEAT * 10);
            assert_eq!(
                beaten(dir.path()),
                before,
                "the turn's grandchild outlived it"
            );
        }

        #[cfg(unix)]
        #[test]
        fn a_signal_that_ends_the_recorder_ends_its_turn() {
            const TEST: &str = "a_signal_that_ends_the_recorder_ends_its_turn";
            if let Some(dir) = as_fake_turn(TEST) {
                if std::env::args().any(|a| a == FAKE_RECORDER) {
                    crate::bounded::kill_child_on_signal().expect("its handler");
                    let _ = run_turn(fake_turn(TEST, &dir), Some(OUTLIVES));
                }
                return;
            }
            let dir = tempfile::tempdir().expect("dir");
            let mut recorder = fake_turn(TEST, dir.path())
                .arg(FAKE_RECORDER)
                .spawn()
                .expect("the recorder");
            wait_until("the grandchild", BOOT, || Ok(beaten(dir.path()) > 0)).expect("it beats");
            let pid = rustix::process::Pid::from_child(&recorder);
            rustix::process::kill_process(pid, rustix::process::Signal::INT).expect("Ctrl-C");
            wait_until("the recorder's end", BOOT, || {
                Ok(recorder.try_wait()?.is_some())
            })
            .expect("Ctrl-C ends the recorder");
            let before = beaten(dir.path());
            std::thread::sleep(BEAT * 10);
            assert_eq!(
                beaten(dir.path()),
                before,
                "the turn outlived the recorder Ctrl-C ended"
            );
        }

        #[test]
        fn a_turn_inside_its_budget_ends_as_it_exited() {
            let exited = |code: i32| {
                let mut turn = if cfg!(windows) {
                    std::process::Command::new("cmd")
                } else {
                    std::process::Command::new("sh")
                };
                let flag = if cfg!(windows) { "/C" } else { "-c" };
                turn.args([flag, &format!("exit {code}")]);
                run_turn(turn, Some(*TURN_BUDGET.here())).expect("it ran")
            };
            assert_eq!(exited(0), TurnEnd::Exited(0));
            assert_eq!(exited(3), TurnEnd::Exited(3));
        }

        /// A driver is a Unix shell script.
        #[cfg(unix)]
        #[test]
        fn a_driver_that_gave_up_says_why_in_the_record() {
            const WHY: &str = "the screen stood still";
            let sandbox = tempfile::tempdir().expect("sandbox");
            let ws = tempfile::tempdir().expect("workspace");
            let endpoint = Endpoint::in_dir(sandbox.path(), "capture");
            let wiring = CliWiring {
                endpoint: &endpoint,
                scratch: sandbox.path(),
            };
            let script = format!("printf '{WHY}\\n' >\"${DRIVER_DIR_ENV}/{GAVE_UP_NOTE}\"; exit 3");
            let cmd = ["sh", "-c", &script].map(String::from);
            let turn = wired(&cmd, ws.path(), &wiring, "1.0.0", &[]).expect("wired");
            assert_eq!(
                run_turn(turn, Some(*TURN_BUDGET.here())).expect("it ran"),
                TurnEnd::Exited(3)
            );
            assert_eq!(give_up_reason(sandbox.path()).as_deref(), Some(WHY));
        }

        #[test]
        fn a_slow_probe_answers_inside_the_boot_deadline() {
            const TEST: &str = "a_slow_probe_answers_inside_the_boot_deadline";
            /// A probe's own cold start, as a slow CLI's is.
            const SLOW: Duration = Duration::from_secs(6);
            if let Some((_, probe)) = as_fake(true, SLOW) {
                if !probe {
                    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).expect("stdin");
                }
                return;
            }
            let dir = tempfile::tempdir().expect("dir");
            let started = std::time::Instant::now();
            let running = start_fake(TEST, dir.path(), SLOW * 3).expect("a slow probe answers");
            assert!(started.elapsed() >= SLOW, "{:?}", started.elapsed());
            running.stop(*DAEMON_STOP_GRACE.here()).expect("it stops");
        }

        #[test]
        fn a_windows_daemon_gets_longer_to_boot_and_stop_than_a_unix_one() {
            assert!(DAEMON_BOOT.windows > DAEMON_BOOT.unix, "its cold boot");
            assert!(
                DAEMON_STOP_GRACE.windows > DAEMON_STOP_GRACE.unix,
                "its stop"
            );
        }

        #[test]
        fn only_a_profiled_failing_turn_may_exit_non_zero() {
            let exited = |code, second: Option<i32>| CaptureRecord {
                workspace: PathBuf::from("/ws"),
                cli: "cli".into(),
                version: "1".into(),
                end: TurnEnd::Exited(code),
                give_up_reason: None,
                incomplete: None,
                second_turn: second.map(|code| SecondTurn {
                    end: TurnEnd::Exited(code),
                }),
                note_survived: true,
            };
            assert!(exited_as_scripted(
                "openclaw",
                "model-error",
                &exited(1, None)
            ));
            assert!(!exited_as_scripted(
                "openclaw",
                "tool-run",
                &exited(1, None)
            ));
            assert!(!exited_as_scripted(
                "unprofiled",
                "model-error",
                &exited(1, None)
            ));
            assert!(exited_as_scripted(
                "unprofiled",
                "model-error",
                &exited(0, None)
            ));
            assert!(exited_as_scripted(
                "claude-code",
                "ultra-effort",
                &exited(0, Some(0))
            ));
            assert!(
                !exited_as_scripted("claude-code", "ultra-effort", &exited(0, Some(1))),
                "its second turn is held to the same bar"
            );
        }

        #[test]
        fn a_failed_capture_keeps_only_a_scratch_a_driver_wrote_in() {
            let top_level = |scratch: &Path| {
                std::fs::write(named_cli(scratch), "openclaw\n").expect("cli");
                std::fs::write(scratch.join("capture.sock.lock"), "").expect("lock");
                std::fs::create_dir_all(driver_dir(scratch)).expect("driver dir");
            };
            let bare = tempfile::tempdir().expect("scratch");
            top_level(bare.path());
            let at = bare.path().to_path_buf();
            keep_scratch(bare, None, BTreeSet::new, &mut std::io::sink());
            assert!(!at.exists(), "top-level entries alone go");

            let used = tempfile::tempdir().expect("scratch");
            top_level(used.path());
            let log = driver_dir(used.path()).join("openclaw-agent.log");
            std::fs::write(&log, "refused").expect("log");
            let at = used.path().to_path_buf();
            keep_scratch(used, None, BTreeSet::new, &mut std::io::sink());
            assert!(log.is_file(), "a driver's log stays");
            std::fs::remove_dir_all(&at).expect("cleanup");
        }

        fn driver_logs_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
            let d = tempfile::tempdir().expect("scratch");
            for (name, body) in files {
                let path = driver_dir(d.path()).join(name);
                std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
                std::fs::write(path, body).expect("write");
            }
            d
        }

        fn no_needles() -> BTreeSet<String> {
            BTreeSet::new()
        }

        #[test]
        fn only_a_sandboxed_capture_copies_its_driver_dir_into_its_run_dir() {
            let out = std::env::temp_dir().join("run");
            let line = |w: &[&str]| {
                let args: Vec<String> = w.iter().map(|s| (*s).to_owned()).collect();
                match Invocation::parse_process(&args).expect("a line") {
                    Invocation::Capture(plan) => *plan,
                    other => panic!("not a capture: {other:?}"),
                }
            };
            let out_arg = out.display().to_string();
            let mut raw = line(&["--out", &out_arg, "kimi", "tool-run", "kimi"]);
            assert_eq!(
                driver_destination(&raw),
                None,
                "an operator's billed capture may hold credentials"
            );
            assert_eq!(
                driver_destination(&line(&["kimi", "tool-run", "kimi"])),
                None
            );
            raw.within = Some(
                pixtuoid_core::harness::plan::AbsPath::try_from(std::env::temp_dir())
                    .expect("absolute"),
            );
            assert_eq!(
                driver_destination(&raw),
                Some(std::path::absolute(&out).expect("abs").join(DRIVER_COPY))
            );
        }

        #[test]
        fn a_driver_copy_keeps_the_drivers_layout() {
            let from = driver_logs_with(&[("daemon.log", b"up"), ("homes/a/b.log", b"deep")]);
            let into = tempfile::tempdir().expect("run dir");
            let copy = copy_driver_dir(
                &driver_dir(from.path()),
                into.path(),
                &no_needles(),
                DRIVER_CAP_BYTES,
            )
            .expect("copy");
            assert!(copy.notices.is_empty(), "{:?}", copy.notices);
            assert_eq!(copy.files, 2);
            assert_eq!(
                std::fs::read_to_string(into.path().join("homes/a/b.log")).expect("deep"),
                "deep"
            );
            assert!(into.path().join("daemon.log").is_file());
        }

        #[test]
        fn a_driver_file_naming_the_operator_is_copied_with_the_name_redacted() {
            let from = driver_logs_with(&[
                ("clean.log", b"fine"),
                ("leak.log", b"home is /Users/ada-lovelace/x, ada-lovelace"),
                ("ada-lovelace/inner.log", b"in a dir named for them"),
            ]);
            let into = tempfile::tempdir().expect("run dir");
            let needles = BTreeSet::from(["ada-lovelace".to_owned()]);
            let notices = copy_driver_dir(
                &driver_dir(from.path()),
                into.path(),
                &needles,
                DRIVER_CAP_BYTES,
            )
            .expect("copy")
            .notices;
            assert_eq!(
                std::fs::read_to_string(into.path().join("leak.log")).expect("copied"),
                format!("home is /Users/{REDACTED}/x, {REDACTED}")
            );
            assert!(into.path().join("clean.log").is_file());
            assert!(
                !into.path().join("ada-lovelace").exists(),
                "the path is renamed"
            );
            assert!(into.path().join(REDACTED).join("inner.log").is_file());
            assert!(notices.is_empty(), "{notices:?}");
        }

        #[test]
        fn a_notice_never_shows_the_identity_it_redacted() {
            let from = driver_logs_with(&[
                ("ada-lovelace/blob.bin", &[0xff, 0xfe, 0x00]),
                ("ada-lovelace/b.log", b"1234"),
            ]);
            let into = tempfile::tempdir().expect("run dir");
            let needles = BTreeSet::from(["ada-lovelace".to_owned()]);
            let mut notices = copy_driver_dir(&driver_dir(from.path()), into.path(), &needles, 2)
                .expect("copy")
                .notices;
            notices.extend(
                copy_driver_dir(&driver_dir(from.path()), into.path(), &needles, 1 << 20)
                    .expect("copy")
                    .notices,
            );
            assert!(
                notices.iter().any(|n| n.contains("not text")),
                "{notices:?}"
            );
            assert!(notices.iter().any(|n| n.contains("cap")), "{notices:?}");
            assert!(
                notices.iter().all(|n| !n.contains("ada-lovelace")),
                "{notices:?}"
            );
        }

        #[test]
        fn two_files_that_redact_to_one_path_do_not_overwrite_each_other() {
            let from = driver_logs_with(&[("a-1/x.log", b"one"), ("b-2/x.log", b"two")]);
            let into = tempfile::tempdir().expect("run dir");
            let needles = BTreeSet::from(["a-1".to_owned(), "b-2".to_owned()]);
            let notices = copy_driver_dir(
                &driver_dir(from.path()),
                into.path(),
                &needles,
                DRIVER_CAP_BYTES,
            )
            .expect("copy")
            .notices;
            assert_eq!(
                std::fs::read_to_string(into.path().join(REDACTED).join("x.log")).expect("kept"),
                "one"
            );
            assert!(
                notices.iter().any(|n| n.contains("another file's")),
                "{notices:?}"
            );
        }

        #[test]
        fn a_longer_needle_wins_where_two_start_together() {
            let needles = BTreeSet::from(["ada".to_owned(), "ada-lovelace".to_owned()]);
            assert_eq!(
                redact("ada-lovelace ada", &needles),
                format!("{REDACTED} {REDACTED}")
            );
        }

        #[test]
        fn a_file_that_is_not_text_does_not_end_the_copy() {
            let big = vec![0xffu8; 64];
            let from = driver_logs_with(&[("a.bin", &big), ("b.log", b"1234")]);
            let into = tempfile::tempdir().expect("run dir");
            let notices = copy_driver_dir(&driver_dir(from.path()), into.path(), &no_needles(), 8)
                .expect("copy")
                .notices;
            assert!(into.path().join("b.log").is_file(), "{notices:?}");
        }

        #[test]
        fn a_file_past_the_cap_is_left_out_and_a_later_one_that_fits_is_copied() {
            let from =
                driver_logs_with(&[("a.log", b"1234"), ("b.log", b"5678"), ("c.log", b"90")]);
            let into = tempfile::tempdir().expect("run dir");
            let notices = copy_driver_dir(&driver_dir(from.path()), into.path(), &no_needles(), 6)
                .expect("copy")
                .notices;
            assert!(into.path().join("a.log").is_file());
            assert!(!into.path().join("b.log").exists(), "past the cap");
            assert!(into.path().join("c.log").is_file(), "it skips, not stops");
            assert!(
                notices
                    .iter()
                    .any(|n| n.contains("past the copy's") && n.contains("b.log")),
                "{notices:?}"
            );
        }

        #[test]
        fn a_driver_dir_one_byte_over_the_cap_is_not_copied_whole() {
            let from = driver_logs_with(&[("a.log", b"1234"), ("b.log", b"5678"), ("c.log", b"9")]);
            let into = tempfile::tempdir().expect("run dir");
            let notices = copy_driver_dir(&driver_dir(from.path()), into.path(), &no_needles(), 8)
                .expect("copy")
                .notices;
            assert!(into.path().join("b.log").is_file());
            assert!(!into.path().join("c.log").exists(), "one byte past the cap");
            assert!(notices.iter().any(|n| n.contains("cap")), "{notices:?}");
        }

        /// A path component is written on every OS the tier runs on. An allowlist,
        /// since Windows' list is long: ASCII letters and digits hold none of its
        /// reserved characters, control characters, or a trailing dot or space; and
        /// the reserved device names are checked by name, with or without a digit
        /// (learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file, "Naming
        /// conventions", fetched this session).
        #[test]
        fn the_redaction_marker_is_a_file_name_on_every_os() {
            assert!(!REDACTED.is_empty());
            assert!(
                REDACTED.chars().all(|c| c.is_ascii_alphanumeric()),
                "{REDACTED}"
            );
            let reserved = ["CON", "PRN", "AUX", "NUL"]
                .into_iter()
                .map(String::from)
                .chain(
                    ["COM", "LPT"]
                        .into_iter()
                        .flat_map(|d| (1..=9).map(move |n| format!("{d}{n}"))),
                );
            for name in reserved {
                assert!(!REDACTED.eq_ignore_ascii_case(&name), "{name}");
            }
        }

        #[test]
        fn a_driver_dir_exactly_the_size_of_the_cap_is_copied_whole() {
            let from = driver_logs_with(&[("a.log", b"1234"), ("b.log", b"5678")]);
            let into = tempfile::tempdir().expect("run dir");
            let notices = copy_driver_dir(&driver_dir(from.path()), into.path(), &no_needles(), 8)
                .expect("copy")
                .notices;
            assert!(notices.is_empty(), "{notices:?}");
            assert!(into.path().join("b.log").is_file());
        }

        /// `keep_scratch` over `files`, naming `ada-lovelace`: the run dir it copied
        /// into, what it logged, and whether the scratch was left behind.
        fn kept_scratch_log(files: &[(&str, &[u8])]) -> (tempfile::TempDir, String, bool) {
            let scratch = driver_logs_with(files);
            let at = scratch.path().to_path_buf();
            let into = tempfile::tempdir().expect("run dir");
            let mut log = Vec::new();
            keep_scratch(
                scratch,
                Some(into.path()),
                || BTreeSet::from(["ada-lovelace".to_owned()]),
                &mut log,
            );
            let kept = at.exists();
            if kept {
                std::fs::remove_dir_all(at).expect("cleanup");
            }
            (into, String::from_utf8(log).expect("utf8"), kept)
        }

        #[test]
        fn a_copy_that_fails_keeps_and_names_the_scratch() {
            let scratch = driver_logs_with(&[("a.log", b"fine")]);
            let at = scratch.path().to_path_buf();
            let run = tempfile::tempdir().expect("run dir");
            let blocker = run.path().join("a-file");
            std::fs::write(&blocker, "").expect("write");
            let mut log = Vec::new();
            keep_scratch(
                scratch,
                Some(&blocker.join("driver")),
                BTreeSet::new,
                &mut log,
            );
            let log = String::from_utf8(log).expect("utf8");
            assert!(log.contains("copy failed"), "{log}");
            assert!(log.contains("stay in"), "{log}");
            assert!(at.exists(), "it holds the only copy of the logs");
            std::fs::remove_dir_all(at).expect("cleanup");
        }

        #[test]
        fn a_whole_copy_says_where_it_went_and_does_not_point_at_the_scratch() {
            let (into, log, kept) = kept_scratch_log(&[("daemon.log", b"up as ada-lovelace")]);
            assert_eq!(
                std::fs::read_to_string(into.path().join("daemon.log")).expect("copied"),
                format!("up as {REDACTED}")
            );
            assert!(log.contains("copied into"), "{log}");
            assert!(!log.contains("stay in"), "{log}");
            assert!(!kept, "the originals do not linger unnamed");
        }

        #[test]
        fn an_incomplete_copy_points_at_the_scratch_that_holds_the_rest() {
            let (_, log, kept) = kept_scratch_log(&[("a.log", b"fine"), ("b.bin", &[0xff, 0xfe])]);
            assert!(log.contains("copied into"), "a.log was copied: {log}");
            assert!(log.contains("not text"), "{log}");
            assert!(log.contains("stay in"), "{log}");
            assert!(kept, "it holds the rest");
        }

        #[test]
        fn a_copy_of_nothing_claims_no_destination() {
            let (_, log, kept) = kept_scratch_log(&[("b.bin", &[0xff, 0xfe])]);
            assert!(!log.contains("copied into"), "{log}");
            assert!(log.contains("stay in"), "{log}");
            assert!(kept);
        }

        #[test]
        fn a_sandboxed_runs_identity_leaves_its_home_out() {
            let home = "/home/ada-lovelace";
            let needles =
                temp_env::with_vars([("HOME", Some(home)), ("USER", Some("ada"))], || {
                    operator_identity()
                });
            assert!(!needles.contains(home), "{needles:?}");
            assert!(needles.contains("ada"), "{needles:?}");
        }

        #[test]
        fn every_driver_reads_only_env_the_recorder_sets() {
            let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/lib");
            let mut drivers = 0;
            for entry in std::fs::read_dir(&lib).expect("scripts/lib") {
                let path = entry.expect("an entry").path();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if name.starts_with("capture-") {
                    drivers += 1;
                    let text = std::fs::read_to_string(&path).expect("a driver");
                    assert!(text.contains(CLI_ENV), "{name} names no CLI at ${CLI_ENV}");
                    for named in text.split("PIXTUOID_CAPTURE_").skip(1) {
                        let tail: String = named
                            .chars()
                            .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                            .collect();
                        let var = format!("PIXTUOID_CAPTURE_{tail}");
                        assert!(
                            [CLI_ENV, VERSION_ENV, DRIVER_DIR_ENV].contains(&var.as_str()),
                            "{name} reads ${var}, which the recorder never sets"
                        );
                    }
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
