use super::*;
use crate::install::target::{CLAUDE, CODEX, MergeOutcome, OPENCLAW, Primary, REASONIX, Target};

/// Claude's owned hooks file without its plugin registration or presence probe,
/// for the tests of the primary-write path itself.
const CLAUDE_FILE: Target = Target {
    host: None,
    presence_probe: || false,
    ..CLAUDE
};

static FAKE: Target = Target {
    name: "fake",
    core_source: "fake",
    display_name: "Fake",
    default_config_path: || Ok(std::path::PathBuf::from("/nonexistent/fake")),
    hook_command: |_, _| Ok("x".into()),
    primary: Primary::Config {
        merge_install: |c, _| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        merge_uninstall: |c| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        verify_schema: |_| crate::install::verify::SchemaParse::broken("test fake"),
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: || false,
    post_install_hint: None,
    host: None,
    #[cfg(feature = "upstream")]
    upstream: &[],
    registers: Vec::new,
    consumes: &[],
};

// A fn-pointer `default_config_path` can't capture a TempDir, so FAKE2/FAKE_DIR
// use a fixed temp path; the PID suffix keeps concurrent runs from racing on it.
fn fake2_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("pixtuoid-test-fake2-{}.toml", std::process::id()))
}

fn fake_dir_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("pixtuoid-test-fake-dir-{}", std::process::id()))
}

// FAKE2's merge_uninstall reports `changed` iff the content is non-empty, so
// has_hooks can be driven through both arms by controlling the on-disk content.
static FAKE2: Target = Target {
    name: "fake2",
    core_source: "fake2",
    display_name: "Fake2",
    default_config_path: || Ok(fake2_config_path()),
    hook_command: |_, _| Ok("x".into()),
    primary: Primary::Config {
        merge_install: |c, _| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        merge_uninstall: |c| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: !c.trim().is_empty(),
            })
        },
        verify_schema: |_| crate::install::verify::SchemaParse::broken("test fake"),
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: || false,
    post_install_hint: None,
    host: None,
    #[cfg(feature = "upstream")]
    upstream: &[],
    registers: Vec::new,
    consumes: &[],
};

// FAKE_DIR's config path is created as a DIRECTORY, so read_config errors.
static FAKE_DIR: Target = Target {
    name: "fakedir",
    core_source: "fakedir",
    display_name: "FakeDir",
    default_config_path: || Ok(fake_dir_config_path()),
    hook_command: |_, _| Ok("x".into()),
    primary: Primary::Config {
        merge_install: |c, _| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        merge_uninstall: |c| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        verify_schema: |_| crate::install::verify::SchemaParse::broken("test fake"),
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: || false,
    post_install_hint: None,
    host: None,
    #[cfg(feature = "upstream")]
    upstream: &[],
    registers: Vec::new,
    consumes: &[],
};

// FAKE_NO_HOME's default_config_path Errs — the no-home-dir arm.
static FAKE_NO_HOME: Target = Target {
    name: "fakenohome",
    core_source: "fakenohome",
    display_name: "FakeNoHome",
    default_config_path: || Err(anyhow::anyhow!("no home dir")),
    hook_command: |_, _| Ok("x".into()),
    primary: Primary::Config {
        merge_install: |c, _| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        merge_uninstall: |c| {
            Ok(MergeOutcome {
                content: c.to_string(),
                changed: false,
            })
        },
        verify_schema: |_| crate::install::verify::SchemaParse::broken("test fake"),
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: || false,
    post_install_hint: None,
    host: None,
    #[cfg(feature = "upstream")]
    upstream: &[],
    registers: Vec::new,
    consumes: &[],
};

/// The files a target owns beside its config, `None` for every other target.
fn beside_of(t: &Target) -> Option<target::RenderBeside> {
    match t.primary {
        Primary::Config { beside, .. } => beside,
        Primary::Owned { .. } => None,
    }
}

/// `/x/hook` is DRIVE-RELATIVE on Windows, where the absolutization rewrites it.
fn abs_fixture(unix: &str, windows: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(windows)
    } else {
        PathBuf::from(unix)
    }
}

#[test]
fn resolve_hook_binary_env_override_wins() {
    let p = abs_fixture("/x/hook", r"C:\x\hook");
    let got = resolve_hook_binary_from(&CLAUDE_FILE, Some(p.clone()), || {
        panic!("locate must not be called when PIXTUOID_HOOK is set")
    });
    assert_eq!(got.unwrap(), (p, true));
}

#[cfg(unix)]
#[test]
fn resolve_hook_binary_claude_falls_back_to_bare_name_when_unresolvable() {
    // Routed through the injected seam (env_hook: None) so an ambient
    // PIXTUOID_HOOK on the dev machine can't short-circuit the staged failure.
    let got = resolve_hook_binary_from(&CLAUDE_FILE, None, || {
        Err(anyhow::anyhow!("could not locate"))
    });
    assert_eq!(got.unwrap(), (PathBuf::from("pixtuoid-hook"), false));
}

// The bare-name fallback is a unix-only contract: the Windows exec form embeds
// the absolute path, so an unresolvable binary is fatal there.
#[cfg(windows)]
#[test]
fn resolve_hook_binary_claude_errors_when_unresolvable_on_windows() {
    let got = resolve_hook_binary_from(&CLAUDE_FILE, None, || {
        Err(anyhow::anyhow!("could not locate"))
    });
    assert!(got.is_err(), "exec form requires a real resolved .exe");
}

#[test]
fn resolve_hook_binary_codex_errors_when_unresolvable() {
    let got = resolve_hook_binary_from(&CODEX, None, || Err(anyhow::anyhow!("could not locate")));
    assert!(got.is_err());
}

#[test]
fn resolve_hook_binary_absolutizes_a_relative_env_override() {
    // An embedded relative path would resolve against the CLI's cwd at hook time
    // and silently never fire from other dirs.
    let (got, explicit) = resolve_hook_binary_from(
        &CODEX,
        Some(PathBuf::from("target/debug/pixtuoid-hook")),
        || unreachable!("the env override must win over locate"),
    )
    .unwrap();
    assert!(
        got.is_absolute(),
        "expected absolutized env path, got {got:?}"
    );
    assert!(got.ends_with("target/debug/pixtuoid-hook"));
    assert!(explicit);
}

#[test]
fn resolve_hook_binary_no_overrides_uses_locate() {
    let located = abs_fixture("/located/hook", r"C:\located\hook");
    let expect = located.clone();
    let got = resolve_hook_binary_from(&CLAUDE_FILE, None, || Ok(located));
    assert_eq!(got.unwrap(), (expect, false));
}

#[test]
fn empty_env_override_counts_as_unset_at_the_live_read() {
    // path_env is the live seam install_target reads PIXTUOID_HOOK
    // through: empty/whitespace must read as unset, or "" becomes the command.
    let read = |v| {
        temp_env::with_var("PIXTUOID_HOOK", Some(v), || {
            pixtuoid_core::platform::path_env("PIXTUOID_HOOK")
        })
    };
    assert_eq!(read(""), None);
    assert_eq!(read("   "), None);
    assert_eq!(read("/real/hook"), Some("/real/hook".into()));
}

#[test]
fn is_drive_relative_only_matches_prefix_without_root() {
    use std::path::Path;
    #[cfg(windows)]
    {
        assert!(is_drive_relative(Path::new(r"C:rel\hook.exe")));
        assert!(!is_drive_relative(Path::new(r"C:\abs\hook.exe")));
        assert!(!is_drive_relative(Path::new(r"rel\hook.exe")));
        // Rooted-no-prefix (`\x\hook`) IS handled by join (it keeps the cwd's
        // drive), so it must not trip the hard error.
        assert!(!is_drive_relative(Path::new(r"\rooted\hook.exe")));
    }
    // Unix has no path prefixes — `C:foo` is an ordinary relative path there.
    #[cfg(unix)]
    assert!(!is_drive_relative(Path::new("C:foo.exe")));
}

// For drive-relative `C:foo.exe` (prefix, no root) is_relative() is true but
// `cwd.join` no-ops, so the "absolutized" embed would still resolve against a
// per-drive cwd at hook time — hence the hard error.
#[cfg(windows)]
#[test]
fn resolve_hook_binary_rejects_a_drive_relative_env_override() {
    let err = resolve_hook_binary_from(&CODEX, Some(PathBuf::from(r"C:rel\hook.exe")), || {
        unreachable!("the env override must win")
    })
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("PIXTUOID_HOOK")
            && msg.contains("drive-relative")
            && msg.contains("absolute path"),
        "the error must name the seam that supplied the bad path: {msg}"
    );
}

#[test]
fn has_hooks_empty_config_is_false() {
    assert!(!has_hooks(&FAKE, None));
}

#[test]
fn has_hooks_unreadable_config_is_true() {
    let dir = fake_dir_config_path();
    let _ = std::fs::remove_file(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(has_hooks(&FAKE_DIR, None));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A disconnect from before opencode 2 support left a stub opencode 2 lists as
/// a failed plugin; launch rewrites it, and only it, while `has_hooks` keeps
/// reading it disconnected.
#[test]
fn a_retired_stub_is_rewritten_and_stays_disconnected() {
    let t = &crate::install::target::OPENCODE;
    let Primary::Owned {
        retired: Some(stub),
        ..
    } = &t.primary
    else {
        panic!("opencode declares its retired stub");
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("plugins").join("pixtuoid.ts");

    assert!(!heal_retired_stub(t, Some(cfg.clone())).unwrap());
    assert!(
        !cfg.parent().unwrap().exists(),
        "no dir or lock is created beside a CLI the user may not have"
    );

    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, stub.was).unwrap();
    assert!(!has_hooks(t, Some(cfg.clone())));
    assert!(heal_retired_stub(t, Some(cfg.clone())).unwrap());
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), stub.now);
    assert!(!has_hooks(t, Some(cfg.clone())));
    assert!(!heal_retired_stub(t, Some(cfg.clone())).unwrap());

    let foreign = "export default { id: \"mine\", setup() {} }\n";
    std::fs::write(&cfg, foreign).unwrap();
    assert!(!heal_retired_stub(t, Some(cfg.clone())).unwrap());
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), foreign);
}

#[test]
fn has_hooks_changed_vs_unchanged_arms() {
    let path = fake2_config_path();
    std::fs::write(&path, "model = \"x\"\n").unwrap();
    assert!(has_hooks(&FAKE2, None));
    // Whitespace-only content trims to empty, so the empty arm answers and the
    // changed arm is never reached.
    std::fs::write(&path, "   \n").unwrap();
    assert!(!has_hooks(&FAKE2, None));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn verify_target_and_has_hooks_handle_unresolvable_config_path() {
    assert!(
        !has_hooks(&FAKE_NO_HOME, None),
        "no resolvable config path → no hooks"
    );
    let v = verify_target(&FAKE_NO_HOME, None);
    assert!(!v.is_sound());
    assert_eq!(
        v.issues,
        vec!["no config path resolves (no home dir)".to_string()]
    );
    assert!(v.notes.is_empty(), "the early return emits no notes");
}

#[test]
fn install_target_claude_writes_the_hook_command() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    std::fs::write(&cfg, "{}\n").unwrap();

    install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .is_some_and(|c| c.contains("pixtuoid-hook"))
    );

    install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();
}

#[test]
fn install_target_fails_fast_while_the_config_lock_is_held() {
    // Lock BEFORE read: even the up-to-date no-op path, which never reaches the
    // write, can't safely read/decide mid-flight of another writer.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();

    let _guard = io::lock_config(&cfg).unwrap();
    let err = install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap_err();
    assert!(err.to_string().contains("could not lock"), "got: {err:#}");
}

#[test]
fn uninstall_target_fails_fast_while_the_config_lock_is_held() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();

    let _guard = io::lock_config(&cfg).unwrap();
    let err = uninstall_target(&CLAUDE_FILE, Some(cfg.clone())).unwrap_err();
    assert!(err.to_string().contains("could not lock"), "got: {err:#}");
}

#[test]
fn install_target_reports_installed_then_up_to_date() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    std::fs::write(&cfg, "{}\n").unwrap();

    let r = install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();
    assert!(matches!(r.outcome, InstallOutcome::Installed));

    let r2 = install_target_from(
        &CLAUDE_FILE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();
    assert!(matches!(r2.outcome, InstallOutcome::AlreadyUpToDate));
    // A semantic no-op never rewrites a merged config: a hand-formatted file keeps
    // its bytes. An owned primary is ours whole, so the property is a merge's.
    let merged = tmp.path().join("hooks.json");
    let cursor = || {
        install_target_from(
            &crate::install::target::CURSOR,
            Some(merged.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap()
    };
    cursor();
    let formatted = std::fs::read_to_string(&merged)
        .unwrap()
        .replace("\n", "\n\n");
    std::fs::write(&merged, &formatted).unwrap();
    cursor();
    assert_eq!(std::fs::read_to_string(&merged).unwrap(), formatted);
}

#[test]
fn install_target_explicit_hook_suppresses_path_warning() {
    // An explicit PIXTUOID_HOOK embeds the absolute path, so PATH resolution never
    // happens and the expectation is deterministic — unlike the no-hook case.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    let r = install_target_from(
        &CLAUDE_FILE,
        Some(cfg),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();
    assert!(!r.path_warning);
}

#[test]
fn uninstall_target_reports_removed_then_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "model = \"x\"\n").unwrap();

    let r = uninstall_target(&FAKE2, Some(cfg.clone())).unwrap();
    assert!(matches!(r.outcome, UninstallOutcome::Removed));

    // An absent config is decided BEFORE locking, so there are no side effects.
    let missing = tmp.path().join("missing").join("settings.json");
    let r2 = uninstall_target(&CLAUDE_FILE, Some(missing.clone())).unwrap();
    assert!(matches!(r2.outcome, UninstallOutcome::NothingToRemove));
    assert!(
        !missing.parent().unwrap().exists(),
        "a no-op uninstall leaves no dirs"
    );
}

#[test]
fn install_target_round_trips_every_registered_target() {
    // OpenClaw's plugin dir resolves from openclaw_state_dir(), NOT the config
    // override, so a temp home keeps this off the real ~/.openclaw.
    let oc_home = tempfile::TempDir::new().unwrap();
    // Same class: dsh's plugin file resolves from $DSH_HOME.
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            with_fake_hosts(|_| {
                for t in target::TARGETS {
                    let tmp = tempfile::TempDir::new().unwrap();
                    let cfg = plugin_hooks_path(tmp.path());
                    let hook = || Some(PathBuf::from("/fake/pixtuoid-hook"));

                    let r = install_target_from(t, Some(cfg.clone()), hook()).unwrap();
                    assert!(
                        matches!(r.outcome, InstallOutcome::Installed),
                        "{}: first install must write hooks",
                        t.name
                    );
                    assert!(cfg.exists(), "{}: install wrote a config", t.name);

                    let r2 = install_target_from(t, Some(cfg.clone()), hook()).unwrap();
                    assert!(
                        matches!(r2.outcome, InstallOutcome::AlreadyUpToDate),
                        "{}: re-install must be a no-op (sentinel idempotency)",
                        t.name
                    );

                    let u = uninstall_target(t, Some(cfg.clone())).unwrap();
                    assert!(
                        matches!(u.outcome, UninstallOutcome::Removed),
                        "{}: uninstall must remove the managed entries",
                        t.name
                    );
                    let u2 = uninstall_target(t, Some(cfg.clone())).unwrap();
                    assert!(
                        matches!(u2.outcome, UninstallOutcome::NothingToRemove),
                        "{}: re-uninstall must find nothing to remove",
                        t.name
                    );
                }
            });
        },
    );
}

#[test]
fn openclaw_is_present_is_false_before_then_true_after_install() {
    // OPENCLAW_STATE_DIR points at a NON-EXISTENT dir so the probe starts FALSE.
    let oc_home = tempfile::TempDir::new().unwrap();
    let state = oc_home.path().join("ocstate"); // not yet created
    temp_env::with_var("OPENCLAW_STATE_DIR", Some(&state), || {
        assert!(
            !(OPENCLAW.presence_probe)(),
            "OpenClaw must be undetected before install (empty isolated state dir)"
        );

        let exe = std::env::current_exe().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("openclaw.json");
        install_target_from(&OPENCLAW, Some(cfg), Some(exe)).unwrap();

        assert!(
            (OPENCLAW.presence_probe)(),
            "install must create the state dir the presence probe detects \
         (detect⇄install symmetry — else installed-but-invisible)"
        );
    });
}

#[test]
fn uninstall_preserves_the_config_file_even_when_it_merges_to_empty() {
    // CodeWhale installed ALONE: the config holds only our managed entries, so
    // uninstall un-merges to an effectively EMPTY TOML doc — the exact case where a
    // naive "delete if empty" would lose the user's file.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");

    let r = install_target_from(
        &target::CODEWHALE,
        Some(cfg.clone()),
        Some(PathBuf::from("/fake/pixtuoid-hook")),
    )
    .unwrap();
    assert!(matches!(r.outcome, InstallOutcome::Installed));
    assert!(cfg.exists());

    let u = uninstall_target(&target::CODEWHALE, Some(cfg.clone())).unwrap();
    assert!(
        matches!(u.outcome, UninstallOutcome::Removed),
        "uninstall must have removed the managed entry (the merge produced a change)"
    );
    assert!(
        cfg.exists(),
        "uninstall must PRESERVE the config file (un-merge, never delete)"
    );
    let content = io::read_config(&cfg).unwrap();
    assert!(
        !content.contains(SENTINEL_KEY),
        "the managed hook entry must be un-merged out: {content:?}"
    );
}

#[test]
fn install_on_a_malformed_config_errors_without_rewriting() {
    for (t, malformed) in [
        (&target::CODEWHALE, "this is = = not valid toml [[["),
        (&crate::install::target::CURSOR, "{ not valid json,,, "),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("cfg");
        std::fs::write(&cfg, malformed).unwrap();
        let before = std::fs::read_to_string(&cfg).unwrap();

        let err = install_target_from(
            t,
            Some(cfg.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap_err();
        // "refusing to overwrite" is what proves the error came from the parse
        // step and not a downstream write failure.
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to overwrite"),
            "{}: the error must come from the parse guard, got: {msg}",
            t.name
        );

        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            before,
            "{}: a malformed config must NOT be rewritten/truncated",
            t.name
        );
    }
}

#[test]
fn install_on_a_malformed_config_leaves_no_orphan_beside_files() {
    // A present-but-malformed config must bail BEFORE the files beside it are
    // written, else a partial install strands orphan plugin files.
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            let tmp = tempfile::TempDir::new().unwrap();
            let cfg = tmp.path().join("openclaw.json");
            std::fs::write(&cfg, "{ not valid json,,, ").unwrap();
            let before = std::fs::read_to_string(&cfg).unwrap();

            let err = install_target_from(
                &OPENCLAW,
                Some(cfg.clone()),
                Some(PathBuf::from("/fake/pixtuoid-hook")),
            )
            .unwrap_err();
            // OpenClaw's parse guard words itself differently (its config is JSON5, so a
            // document our strict parser rejects may be perfectly valid).
            assert!(
                format!("{err:#}").contains("will not rewrite the file"),
                "the bail must come from the parse guard, got: {err:#}"
            );
            assert_eq!(std::fs::read_to_string(&cfg).unwrap(), before);
            assert!(
                !oc_home.path().join("plugins").exists(),
                "a malformed-config bail must not leave orphan plugin artifacts on disk"
            );
        },
    );
}

#[test]
fn verify_target_is_sound_after_a_real_install_for_every_target() {
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            let exe = std::env::current_exe().unwrap(); // a real, executable file
            with_fake_hosts(|_| {
                for &t in target::TARGETS {
                    let tmp = tempfile::TempDir::new().unwrap();
                    let cfg = plugin_hooks_path(tmp.path());
                    install_target_from(t, Some(cfg.clone()), Some(exe.clone())).unwrap();
                    let v = verify_target(t, Some(cfg));
                    assert!(
                        v.is_sound(),
                        "{}: a fresh install must verify sound, got issues {:?}",
                        t.name,
                        v.issues
                    );
                }
            });
        },
    );
}

#[test]
fn verify_target_flags_a_missing_shim_binary() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    let ghost = tmp.path().join("ghost-pixtuoid-hook");
    install_target_from(&CLAUDE_FILE, Some(cfg.clone()), Some(ghost)).unwrap();
    let v = verify_target(&CLAUDE_FILE, Some(cfg));
    assert!(!v.is_sound());
    assert!(
        v.issues.iter().any(|i| i.contains("shim binary missing")),
        "{:?}",
        v.issues
    );
}

#[test]
fn verify_target_flags_an_empty_config_as_not_installed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    std::fs::write(&cfg, "   \n").unwrap();
    let v = verify_target(&CLAUDE_FILE, Some(cfg));
    assert!(!v.is_sound());
    assert!(
        v.issues.iter().any(|i| i.contains("config is empty")),
        "{:?}",
        v.issues
    );
}

// A DIRECTORY exists, so read_config's missing-file early-Ok doesn't apply.
#[test]
fn verify_target_flags_an_unreadable_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("cfgdir");
    std::fs::create_dir_all(&dir).unwrap();
    let v = verify_target(&CLAUDE_FILE, Some(dir));
    assert!(!v.is_sound());
    assert!(
        v.issues.iter().any(|i| i.contains("config unreadable")),
        "{:?}",
        v.issues
    );
}

// CODEWHALE embeds an ABSOLUTE shim path, so a shim file present with no exec bits
// reaches the not-executable arm rather than the missing-shim one.
#[cfg(unix)]
#[test]
fn verify_target_flags_a_non_executable_shim() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    let shim = tmp.path().join("hook");
    std::fs::write(&shim, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644)).unwrap();

    install_target_from(&target::CODEWHALE, Some(cfg.clone()), Some(shim)).unwrap();
    let v = verify_target(&target::CODEWHALE, Some(cfg));
    assert!(!v.is_sound());
    assert!(
        v.issues
            .iter()
            .any(|i| i.contains("shim binary not executable")),
        "{:?}",
        v.issues
    );
}

// INVARIANT (#387): a config can verify clean while the plugin FILES the runtime
// actually loads are missing or clobbered — the silent-dead class doctor exists
// to catch. Loops EVERY target with files `beside` its config and deletes each
// one, so a new code-shipping path in `install_target` with
// no matching check in `verify_target` fails here.
#[test]
fn verify_target_hard_flags_a_missing_beside_file_for_every_beside_target() {
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            let exe = std::env::current_exe().unwrap();
            let mut covered = 0;
            for &t in target::TARGETS {
                let Some(make) = beside_of(t) else {
                    continue;
                };
                let tmp = tempfile::TempDir::new().unwrap();
                let cfg = tmp.path().join("config");
                install_target_from(t, Some(cfg.clone()), Some(exe.clone())).unwrap();
                assert!(
                    verify_target(t, Some(cfg.clone())).is_sound(),
                    "{}: a fresh install must verify sound",
                    t.name
                );
                for (p, _) in make(exe.to_str().unwrap()).unwrap() {
                    let _ = std::fs::remove_file(&p).or_else(|_| std::fs::remove_dir_all(&p));
                }
                let v = verify_target(t, Some(cfg));
                // Form-agnostic on purpose: the INVARIANT is a hard issue naming the
                // artifacts, not a fixed sentence.
                assert!(
                    !v.is_sound()
                        && v.issues
                            .iter()
                            .any(|i| i.contains("artifact") && i.contains("missing")),
                    "{}: a missing code artifact must be a HARD verify issue (the silent-dead \
             invariant) — got {:?}",
                    t.name,
                    v.issues
                );
                covered += 1;
            }
            assert!(
                covered >= 1,
                "expected at least one target with files `beside` its config (OpenClaw, dsh) — did the registry change?"
            );
        },
    );
}

/// EVERY owned file, not just the one that bakes a shim path, and any byte of it — a
/// comment included: an OpenClaw upgrader whose manifest or `package.json` predated the
/// upgrade once got existence-only checking and a green doctor.
#[test]
fn verify_target_flags_any_byte_drift_in_every_owned_file() {
    with_owned_homes(|| {
        with_fake_hosts(|_| {
            let mut covered = 0;
            for &t in target::TARGETS {
                let Some(files) = t.primary.owned_files() else {
                    continue;
                };
                let tmp = tempfile::TempDir::new().unwrap();
                let cfg = plugin_hooks_path(tmp.path());
                let paths: Vec<PathBuf> = files
                    .render(&cfg, io::HOOK_BIN)
                    .unwrap()
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect();
                for p in paths {
                    install_target_from(
                        t,
                        Some(cfg.clone()),
                        Some(PathBuf::from("/fake/pixtuoid-hook")),
                    )
                    .unwrap();
                    let body = std::fs::read_to_string(&p).unwrap();
                    std::fs::write(&p, drift_edit(&p, &body)).unwrap();
                    let v = verify_target(t, Some(cfg.clone()));
                    assert!(
                        v.issues.iter().any(|i| i
                            .contains("differs from the plugin this pixtuoid ships")
                            && i.contains("a launch rewrites it")),
                        "{}: a drifted {} must be a HARD verify issue — got {:?}",
                        t.name,
                        p.display(),
                        v.issues
                    );
                    std::fs::write(&p, &body).unwrap();
                    covered += 1;
                }
            }
            assert!(
                covered > 0,
                "no target owns a file — did the registry change?"
            );
        })
    });
}

/// The collapse itself, which the sweep above deliberately cannot see: it accepts
/// BOTH forms, so nothing there pins the shortening.
#[test]
fn same_dir_artifact_misses_collapse_to_one_short_line_scattered_ones_do_not() {
    let dir = std::path::Path::new("/o/plugins/pixtuoid");

    assert!(missing_artifact_issue(&[]).is_empty());

    let one = missing_artifact_issue(&[dir.join("index.js")]);
    assert_eq!(one.len(), 1);
    assert!(
        one[0].starts_with("plugin artifact missing:") && one[0].contains("index.js"),
        "a lone miss names its own path — got {one:?}"
    );

    let same: Vec<PathBuf> = ["index.js", "package.json", "pixtuoid-hook"]
        .iter()
        .map(|n| dir.join(n))
        .collect();
    let collapsed = missing_artifact_issue(&same);
    assert_eq!(collapsed.len(), 1, "same-dir misses are ONE fact");
    let line = &collapsed[0];
    assert!(
        line.starts_with("3 plugin artifacts missing from ")
            && line.contains("/o/plugins/pixtuoid"),
        "the collapsed line counts them and names the dir ONCE — got {line}"
    );
    for n in ["index.js", "package.json", "pixtuoid-hook"] {
        assert!(line.contains(n), "{n} must still be named — got {line}");
    }
    let dir_str = dir.to_string_lossy().into_owned();
    assert_eq!(
        line.matches(&dir_str).count(),
        1,
        "the shared dir appears exactly once — got {line}"
    );
    let per_path: Vec<String> = same
        .iter()
        .map(|p| format!("plugin artifact missing: {}", p.display()))
        .collect();
    assert_eq!(
        per_path.iter().filter(|i| i.contains(&dir_str)).count(),
        same.len(),
        "the form being replaced repeats it per path (that is the cost)"
    );
    assert!(
        line.chars().count() < per_path.iter().map(|i| i.chars().count()).sum::<usize>(),
        "and the one line is shorter than the {} it replaces — got {} chars",
        same.len(),
        line.chars().count()
    );

    let scattered = vec![dir.join("index.js"), PathBuf::from("/elsewhere/hook")];
    let scattered_issues = missing_artifact_issue(&scattered);
    assert_eq!(
        scattered_issues.len(),
        2,
        "scattered misses stay one line each"
    );
    assert!(
        scattered_issues
            .iter()
            .all(|i| i.starts_with("plugin artifact missing:")),
        "got {scattered_issues:?}"
    );
}

/// WHY `has_hooks`' empty-config guard is only a fast path: every target's
/// uninstall merge already reports an empty document as unchanged. A future target
/// that claimed `changed` for an empty config would read as INSTALLED without the
/// guard, making it load-bearing — and reds here instead.
#[test]
fn no_targets_uninstall_merge_claims_a_change_on_an_empty_config() {
    for t in crate::install::target::TARGETS {
        let changed = (t.primary.uninstall())("").map(|o| o.changed);
        assert!(
            matches!(changed, Ok(false)),
            "{}: an empty config bears no hooks to remove — got {changed:?}",
            t.name
        );
    }
}

// The silent-dead class the EXISTENCE stat above is blind to: every artifact is
// present and the plugin loads, but the shim path BAKED INTO the entry module
// points at a binary that moved. The plugin swallows spawn errors by design, so
// the mascot never appears while doctor reports the source healthy.
#[test]
fn verify_target_hard_flags_a_moved_baked_shim_for_every_beside_target() {
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            let mut covered = 0;
            for &t in target::TARGETS {
                if beside_of(t).is_none() {
                    continue;
                }
                let shim_dir = tempfile::TempDir::new().unwrap();
                let shim = shim_dir.path().join("pixtuoid-hook");
                std::fs::copy(std::env::current_exe().unwrap(), &shim).unwrap();
                let tmp = tempfile::TempDir::new().unwrap();
                let cfg = tmp.path().join("config");
                install_target_from(t, Some(cfg.clone()), Some(shim.clone())).unwrap();
                assert!(
                    verify_target(t, Some(cfg.clone())).is_sound(),
                    "{}: a fresh install with a real shim must verify sound",
                    t.name
                );
                std::fs::remove_file(&shim).unwrap();
                let v = verify_target(t, Some(cfg));
                assert!(
                    !v.is_sound() && v.issues.iter().any(|i| i.contains("shim binary missing")),
                    "{}: a moved baked shim must be a HARD verify issue — got {:?} / notes {:?}",
                    t.name,
                    v.issues,
                    v.notes
                );
                covered += 1;
            }
            assert!(
                covered >= 1,
                "expected at least one target with files `beside` its config (OpenClaw, dsh) — did the registry change?"
            );
        },
    );
}

#[test]
fn reinstall_heals_a_deleted_beside_file_even_on_a_config_no_op() {
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            let exe = std::env::current_exe().unwrap();
            let mut covered = 0;
            for &t in target::TARGETS {
                let Some(make) = beside_of(t) else {
                    continue;
                };
                let tmp = tempfile::TempDir::new().unwrap();
                let cfg = tmp.path().join("config");
                install_target_from(t, Some(cfg.clone()), Some(exe.clone())).unwrap();

                let (victim, want) = make(exe.to_str().unwrap())
                    .unwrap()
                    .into_iter()
                    .next()
                    .unwrap();
                std::fs::remove_file(&victim).unwrap();
                assert!(
                    !victim.exists(),
                    "{}: precondition — artifact deleted",
                    t.name
                );

                let r = install_target_from(t, Some(cfg.clone()), Some(exe.clone())).unwrap();
                assert!(
                    matches!(r.outcome, InstallOutcome::AlreadyUpToDate),
                    "{}: config already current — the heal must fire despite the no-op",
                    t.name
                );
                // `write_beside` is not gated on the primary's `changed`; gating it would
                // leave a deleted file gone on a no-op re-install.
                assert!(
                    victim.exists(),
                    "{}: a no-op re-install must re-create the deleted plugin file",
                    t.name
                );
                assert_eq!(
                    std::fs::read_to_string(&victim).unwrap(),
                    want,
                    "{}: the healed artifact must carry the correct baked content",
                    t.name
                );

                std::fs::write(&victim, format!("{want}\n")).unwrap();
                install_target_from(t, Some(cfg.clone()), Some(exe.clone())).unwrap();
                assert_eq!(
                    std::fs::read_to_string(&victim).unwrap(),
                    want,
                    "{}: a re-install must refresh a drifted plugin file",
                    t.name
                );
                let stamp = write_stamp(&victim);
                install_target_from(t, Some(cfg), Some(exe.clone())).unwrap();
                assert_eq!(
                    write_stamp(&victim),
                    stamp,
                    "{}: a re-install must not rewrite an identical plugin file",
                    t.name
                );
                covered += 1;
            }
            assert!(
                covered >= 1,
                "expected at least one target with files `beside` its config (OpenClaw, dsh) — did the registry change?"
            );
        },
    );
}

#[test]
fn verify_target_flags_a_missing_event() {
    let exe = std::env::current_exe().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    install_target_from(&CLAUDE_FILE, Some(cfg.clone()), Some(exe)).unwrap();
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    v["hooks"].as_object_mut().unwrap().remove("SessionEnd");
    std::fs::write(&cfg, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let res = verify_target(&CLAUDE_FILE, Some(cfg));
    assert!(!res.is_sound());
    assert!(
        res.issues
            .iter()
            .any(|i| i.contains("missing hook entries") && i.contains("SessionEnd")),
        "{:?}",
        res.issues
    );
}

// After a DISCONNECT the doctor/health logic must NOT spuriously flag "broken".
// The protection is the `has_hooks` gate every caller applies.
#[test]
fn a_disconnected_source_is_gated_out_of_the_broken_check() {
    let exe = std::env::current_exe().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("settings.json");
    install_target_from(&CLAUDE_FILE, Some(cfg.clone()), Some(exe)).unwrap();
    uninstall_target(&CLAUDE_FILE, Some(cfg.clone())).unwrap();
    let content = io::read_config(&cfg).unwrap();
    assert!(
        !(CLAUDE.primary.uninstall())(&content).unwrap().changed,
        "uninstalled config must report no managed hooks (the has_hooks gate)"
    );
    assert!(
        !verify_target(&CLAUDE_FILE, Some(cfg)).is_sound(),
        "ungated verify of an uninstalled config is broken — the gate is what protects it"
    );
}

#[test]
fn verify_target_flags_codewhale_disabled() {
    // CodeWhale gates ALL hooks on [hooks].enabled, so false-with-entries is a
    // silent-dead the sentinel/event-set checks would miss.
    let exe = std::env::current_exe().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    install_target_from(&target::CODEWHALE, Some(cfg.clone()), Some(exe)).unwrap();
    let content = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("enabled = true", "enabled = false");
    std::fs::write(&cfg, content).unwrap();
    let v = verify_target(&target::CODEWHALE, Some(cfg));
    assert!(!v.is_sound());
    assert!(
        v.issues.iter().any(|i| i.contains("enabled = false")),
        "{:?}",
        v.issues
    );
}

#[test]
fn json_value_equality_ignores_key_order_under_preserve_order() {
    // `preserve_order` swaps serde_json's `Map` to IndexMap so a merge re-emits the
    // user's key order. `Value: PartialEq` must therefore stay order-INDEPENDENT:
    // every Config target's `changed` flag is a semantic diff, so order-SENSITIVE equality
    // would report `changed` for a mere re-order — rewriting the file and
    // flipping `has_hooks` on every single connect.
    let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":3}}"#).unwrap();
    let b: serde_json::Value = serde_json::from_str(r#"{"a":{"x":3,"y":2},"b":1}"#).unwrap();
    assert_eq!(a, b, "object equality must not depend on key order");
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        r#"{"b":1,"a":{"y":2,"x":3}}"#,
        "the user's key order must survive the round-trip"
    );
    assert_ne!(
        serde_json::json!([1, 2]),
        serde_json::json!([2, 1]),
        "array order is data and must still compare unequal"
    );
}

#[test]
fn a_config_we_cannot_parse_but_never_wrote_is_not_reported_as_installed() {
    // The JSON5 trap: OpenClaw's config is legal JSON5, so a user's comment makes
    // our strict merge Err — and a conservative `unwrap_or(true)` then claims hooks
    // are INSTALLED for someone who never connected, whom doctor then tells to
    // reconnect (an unsatisfiable remedy the merge refuses by design). The honest
    // fallback asks whether the document mentions us at all.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("openclaw.json");

    // Valid JSON5, strict-JSON-rejected, and NOT ours.
    std::fs::write(
        &cfg,
        "{\n  // my gateway notes\n  \"gateway\": { \"port\": 18789 },\n}\n",
    )
    .unwrap();
    assert!(
        !has_hooks(&target::OPENCLAW, Some(cfg.clone())),
        "a config we cannot parse and never wrote must not count as installed"
    );

    std::fs::write(
        &cfg,
        "{\n  // mine\n  \"plugins\": { \"entries\": { \"pixtuoid\": { \"enabled\": true } } },\n}\n",
    )
    .unwrap();
    assert!(
        has_hooks(&target::OPENCLAW, Some(cfg.clone())),
        "an unparseable config that names us still bears hooks"
    );

    let dir_cfg = tmp.path().join("as-a-dir.json");
    std::fs::create_dir(&dir_cfg).unwrap();
    assert!(
        has_hooks(&target::OPENCLAW, Some(dir_cfg)),
        "an unreadable config keeps the conservative default"
    );
}

#[test]
fn every_target_that_writes_a_config_names_us_in_it() {
    // The invariant `has_hooks`'s unparseable-config fallback rests on: a config we
    // wrote mentions us, so a substring probe answers "is this ours?" when the parse
    // fails. The fixture shim must therefore be named `pixtuoid-hook`, as in prod.
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        || {
            let tmpdir = tempfile::TempDir::new().unwrap();
            let hook = tmpdir.path().join("pixtuoid-hook");
            std::fs::write(&hook, b"#!/bin/sh\n").unwrap();
            with_fake_hosts(|_| {
                for t in crate::install::TARGETS {
                    let tmp = tempfile::TempDir::new().unwrap();
                    let cfg = plugin_hooks_path(tmp.path());
                    install_target_from(t, Some(cfg.clone()), Some(hook.clone()))
                        .unwrap_or_else(|e| panic!("{}: install failed: {e:#}", t.name));
                    let content = std::fs::read_to_string(&cfg)
                        .unwrap_or_else(|e| panic!("{}: config unreadable: {e}", t.name));
                    assert!(
                        super::config_mentions_us(&content),
                        "{}: a config we wrote must satisfy the PRODUCTION fallback predicate",
                        t.name
                    );
                    assert!(
                        has_hooks(t, Some(cfg)),
                        "{}: has_hooks must see the install it just wrote",
                        t.name
                    );
                }
            });
        },
    );
}

/// The fallback's ONE marker-carrier that is not the shim path: kimi ships no
/// `_pixtuoid` sentinel, so where the embedded path happens not to name us its only
/// trace is the UPPERCASE `PIXTUOID_SOURCE=kimi` — which is why `config_mentions_us`
/// folds case. Unix-only by construction: Windows uses the bare exec form, so there
/// a marker-less path leaves no trace and the probe answers "not ours" (safe — it
/// under-reports on a CORRUPT config rather than inventing "install broken").
#[cfg(unix)]
#[test]
fn kimis_uppercase_env_marker_alone_satisfies_the_fallback_probe() {
    let tmp = tempfile::TempDir::new().unwrap();
    // A shim path that deliberately does NOT contain our name, so the env prefix is
    // the only carrier left; without the case fold this reds.
    let hook = tmp.path().join("HOOK-shim");
    std::fs::write(&hook, b"#!/bin/sh\n").unwrap();
    let cfg = tmp.path().join("kimi-cfg");
    install_target_from(&target::KIMI, Some(cfg.clone()), Some(hook)).expect("kimi install");
    let content = std::fs::read_to_string(&cfg).expect("config readable");
    assert!(
        content.contains("PIXTUOID_SOURCE=kimi"),
        "kimi's only marker here is the env prefix: {content}"
    );
    assert!(
        !content.contains(PLUGIN_MENTION),
        "fixture must not leak the lowercase form, or it cannot pin the fold"
    );
    assert!(
        super::config_mentions_us(&content),
        "the fallback must recognise the UPPERCASE marker"
    );
}

/// A fake of every plugin host's CLI first on PATH, each with a temp home, so no test
/// reaches the developer's own. Each logs `<cli> <args>` to `calls.log` beside it and
/// keeps its state where the real CLI does.
pub(crate) fn with_fake_hosts<R>(f: impl FnOnce(&std::path::Path) -> R) -> R {
    with_fake_hosts_failing(None, f)
}

/// [`with_fake_hosts`], whose fakes exit 1 on the exact call `fail` (`<cli> <args>`).
fn with_fake_hosts_failing<R>(fail: Option<&str>, f: impl FnOnce(&std::path::Path) -> R) -> R {
    let dir = tempfile::TempDir::new().unwrap();
    for (cli, _unix, _windows) in FAKE_HOSTS {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin = dir.path().join(cli);
            std::fs::write(&bin, _unix).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        std::fs::write(dir.path().join(format!("{cli}.cmd")), _windows).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(dir.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let claude = dir.path().join("claude-config");
    let codex = dir.path().join("codex-home");
    let reasonix = dir.path().join("reasonix-home");
    for home in [&claude, &codex, &reasonix] {
        std::fs::create_dir_all(home).unwrap();
    }
    temp_env::with_vars(
        [
            ("PATH", Some(path.as_os_str())),
            ("CLAUDE_CONFIG_DIR", Some(claude.as_os_str())),
            ("CODEX_HOME", Some(codex.as_os_str())),
            ("REASONIX_HOME", Some(reasonix.as_os_str())),
            ("FAKE_HOST_FAIL", fail.map(std::ffi::OsStr::new)),
        ],
        || f(dir.path()),
    )
}

/// Each fake: its command, then its sh and cmd scripts.
const FAKE_HOSTS: &[(&str, &str, &str)] = &[
    ("claude", FAKE_CLAUDE_SH, FAKE_CLAUDE_CMD),
    ("codex", FAKE_CODEX_SH, FAKE_CODEX_CMD),
    ("reasonix", FAKE_REASONIX_SH, FAKE_REASONIX_CMD),
];

const FAKE_CLAUDE_SH: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "claude $*" >> "$d/calls.log"
[ "claude $*" = "${FAKE_HOST_FAIL-}" ] && exit 1
case "$*" in
"plugin marketplace add "*) echo '[{"name":"pixtuoid"}]' > "$d/marketplaces.json" ;;
"plugin install pixtuoid@pixtuoid") [ -f "$CLAUDE_CONFIG_DIR/settings.json" ] || echo '{"enabledPlugins":{"pixtuoid@pixtuoid":true}}' > "$CLAUDE_CONFIG_DIR/settings.json" ;;
"plugin marketplace remove pixtuoid") echo '[]' > "$d/marketplaces.json" ;;
"plugin marketplace list --json") cat "$d/marketplaces.json" 2>/dev/null || echo '[]' ;;
esac
"#;

const FAKE_CLAUDE_CMD: &str = "@echo off\r
set \"d=%~dp0\"\r
>>\"%d%calls.log\" echo claude %~1 %~2 %~3 %~4\r
if defined FAKE_HOST_FAIL if \"claude %*\"==\"%FAKE_HOST_FAIL%\" exit /b 1\r
if \"%1 %2 %3\"==\"plugin marketplace add\" (>\"%d%marketplaces.json\" echo [{\"name\":\"pixtuoid\"}]& exit /b 0)\r
if \"%1 %2\"==\"plugin install\" (if not exist \"%CLAUDE_CONFIG_DIR%\\settings.json\" (>\"%CLAUDE_CONFIG_DIR%\\settings.json\" echo {\"enabledPlugins\":{\"pixtuoid@pixtuoid\":true}})& exit /b 0)\r
if \"%1 %2 %3\"==\"plugin marketplace remove\" (>\"%d%marketplaces.json\" echo []& exit /b 0)\r
if \"%1 %2 %3\"==\"plugin marketplace list\" (if exist \"%d%marketplaces.json\" (type \"%d%marketplaces.json\") else (echo []))& exit /b 0\r
exit /b 0\r
";

/// Codex records both registrations in its config.toml, which [`codex_config`] reads.
const FAKE_CODEX_SH: &str = r#"#!/bin/sh
d=$(dirname "$0")
c="$CODEX_HOME/config.toml"
cache="$CODEX_HOME/plugins/cache/pixtuoid/pixtuoid/local"
echo "codex $*" >> "$d/calls.log"
[ "codex $*" = "${FAKE_HOST_FAIL-}" ] && exit 1
case "$*" in
"plugin marketplace add "*) echo "$4" > "$d/codex-root"; grep -qxF '[marketplaces.pixtuoid]' "$c" 2>/dev/null || printf "[marketplaces.pixtuoid]\nsource_type = 'local'\nsource = '%s'\n" "$4" >> "$c" ;;
"plugin add pixtuoid@pixtuoid") rm -rf "$cache"; mkdir -p "$cache"; for p in "$(cat "$d/codex-root")"/*/; do cp -R "$p". "$cache"; done; grep -qxF "[plugins.'pixtuoid@pixtuoid']" "$c" 2>/dev/null || printf "[plugins.'pixtuoid@pixtuoid']\nenabled = true\n" >> "$c" ;;
"plugin remove pixtuoid@pixtuoid") rm -rf "$cache"; grep -vxF -e "[plugins.'pixtuoid@pixtuoid']" -e 'enabled = true' "$c" > "$c.tmp"; mv "$c.tmp" "$c" ;;
"plugin marketplace remove pixtuoid") grep -v -e '^\[marketplaces\.pixtuoid\]$' -e '^source_type = ' -e '^source = ' "$c" > "$c.tmp"; mv "$c.tmp" "$c" ;;
esac
"#;

const FAKE_CODEX_CMD: &str = "@echo off\r
set \"d=%~dp0\"\r
set \"c=%CODEX_HOME%\\config.toml\"\r
set \"cache=%CODEX_HOME%\\plugins\\cache\\pixtuoid\\pixtuoid\\local\"\r
set \"root=\"\r
if exist \"%d%codex-root\" set /p root=<\"%d%codex-root\"\r
>>\"%d%calls.log\" echo codex %~1 %~2 %~3 %~4\r
if defined FAKE_HOST_FAIL if \"codex %*\"==\"%FAKE_HOST_FAIL%\" exit /b 1\r
if \"%1 %2 %3\"==\"plugin marketplace add\" (>\"%d%codex-root\" echo %~4)\r
if \"%1 %2 %3\"==\"plugin marketplace add\" ((findstr /l /x /c:\"[marketplaces.pixtuoid]\" \"%c%\" >nul 2>&1 || (>>\"%c%\" echo [marketplaces.pixtuoid]& >>\"%c%\" echo source_type = 'local'& >>\"%c%\" echo source = '%~4'))& exit /b 0)\r
if \"%1 %2 %3\"==\"plugin marketplace remove\" ((findstr /l /v /b /c:\"[marketplaces.pixtuoid]\" /c:\"source_type = \" /c:\"source = \" \"%c%\" >\"%c%.tmp\"& move /y \"%c%.tmp\" \"%c%\" >nul)& exit /b 0)\r
if \"%1 %2\"==\"plugin add\" if not defined root exit /b 1\r
if \"%1 %2\"==\"plugin add\" (if exist \"%cache%\" rmdir /s /q \"%cache%\")\r
if \"%1 %2\"==\"plugin add\" (for /d %%p in (\"%root%\\*\") do xcopy /e /i /y /q \"%%p\" \"%cache%\" >nul)\r
if \"%1 %2\"==\"plugin add\" ((findstr /l /x /c:\"[plugins.'pixtuoid@pixtuoid']\" \"%c%\" >nul 2>&1 || (>>\"%c%\" echo [plugins.'pixtuoid@pixtuoid']& >>\"%c%\" echo enabled = true))& exit /b 0)\r
if \"%1 %2\"==\"plugin remove\" (if exist \"%cache%\" rmdir /s /q \"%cache%\")\r
if \"%1 %2\"==\"plugin remove\" ((findstr /l /v /x /c:\"[plugins.'pixtuoid@pixtuoid']\" /c:\"enabled = true\" \"%c%\" >\"%c%.tmp\"& move /y \"%c%.tmp\" \"%c%\" >nul)& exit /b 0)\r
exit /b 0\r
";

/// Reasonix records an install in its plugin-packages.json, which
/// [`reasonix_packages`] reads. It fails on its first two args alone, since an
/// install's path arg is quoted differently by each OS's spawn.
const FAKE_REASONIX_SH: &str = r#"#!/bin/sh
d=$(dirname "$0")
s="$REASONIX_HOME/plugin-packages.json"
echo "reasonix $*" >> "$d/calls.log"
[ "reasonix $1 $2" = "${FAKE_HOST_FAIL-}" ] && exit 1
case "$*" in
"plugin install "*" --link --replace --yes") echo '{"version":1,"plugins":[{"name":"pixtuoid","root":"linked","enabled":true}]}' > "$s" ;;
"plugin remove pixtuoid --yes") echo '{"version":1,"plugins":[]}' > "$s" ;;
esac
"#;

const FAKE_REASONIX_CMD: &str = "@echo off\r
set \"d=%~dp0\"\r
set \"s=%REASONIX_HOME%\\plugin-packages.json\"\r
>>\"%d%calls.log\" echo reasonix %~1 %~2 %~3 %~4 %~5 %~6\r
if defined FAKE_HOST_FAIL if \"reasonix %1 %2\"==\"%FAKE_HOST_FAIL%\" exit /b 1\r
if \"%1 %2\"==\"plugin install\" (>\"%s%\" echo {\"version\":1,\"plugins\":[{\"name\":\"pixtuoid\",\"root\":\"linked\",\"enabled\":true}]}& exit /b 0)\r
if \"%1 %2\"==\"plugin remove\" (>\"%s%\" echo {\"version\":1,\"plugins\":[]}& exit /b 0)\r
exit /b 0\r
";

/// The plugin-packages.json Reasonix keeps, under the fake's `REASONIX_HOME`.
fn reasonix_packages() -> PathBuf {
    PathBuf::from(std::env::var_os("REASONIX_HOME").expect("inside with_fake_hosts"))
        .join("plugin-packages.json")
}

/// The config.toml Codex reads, under the fake's `CODEX_HOME`.
fn codex_config() -> PathBuf {
    PathBuf::from(std::env::var_os("CODEX_HOME").expect("inside with_fake_hosts"))
        .join("config.toml")
}

/// The settings.json Claude Code reads, under the fake's `CLAUDE_CONFIG_DIR`.
fn cc_settings() -> PathBuf {
    PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").expect("inside with_fake_hosts"))
        .join("settings.json")
}

/// A hooks file `<marketplace>/<plugin>/hooks/hooks.json` deep inside `tmp`, so a
/// host target's manifests land in the test's own dir.
fn plugin_hooks_path(tmp: &std::path::Path) -> PathBuf {
    tmp.join("marketplace")
        .join("plugin")
        .join("hooks")
        .join("hooks.json")
}

#[test]
fn claude_install_registers_the_plugin() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        // An executable that exists, so verify has only the plugin to judge.
        let hook = Some(std::env::current_exe().unwrap());
        let r = install_target_from(&CLAUDE, Some(hooks.clone()), hook.clone()).unwrap();
        assert!(matches!(r.outcome, InstallOutcome::Installed));

        let root = tmp.path().join("marketplace");
        assert!(root.join(".claude-plugin/marketplace.json").is_file());
        assert!(root.join("plugin/.claude-plugin/plugin.json").is_file());
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert!(
            calls.contains(&format!("plugin marketplace add {}", root.display()))
                && calls.contains("plugin install pixtuoid@pixtuoid"),
            "calls: {calls}"
        );
        assert!(has_hooks(&CLAUDE, Some(hooks.clone())));
        // What `claude plugin install` records at user scope.
        std::fs::write(
            cc_settings(),
            r#"{"theme":"dark","enabledPlugins":{"pixtuoid@pixtuoid":true}}"#,
        )
        .unwrap();
        let v = verify_target(&CLAUDE, Some(hooks.clone()));
        assert!(v.is_sound(), "{:?}", v.issues);

        // A re-install with nothing to change still re-registers, which heals a
        // plugin the user removed in Claude Code.
        let r2 = install_target_from(&CLAUDE, Some(hooks), hook).unwrap();
        assert!(matches!(r2.outcome, InstallOutcome::AlreadyUpToDate));
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert_eq!(
            calls.matches("plugin install pixtuoid@pixtuoid").count(),
            2,
            "calls: {calls}"
        );
    });
}

/// A 0.19.0 install merged a sentinel group per event into Claude Code's own
/// settings.json; left beside the plugin, every event would fire twice.
#[test]
fn claude_connect_strips_the_hooks_an_older_pixtuoid_merged_into_its_settings() {
    with_fake_hosts(|_| {
        let user = serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": "notify-send done" }],
        });
        let legacy = serde_json::json!({
            SENTINEL_KEY: true,
            "matcher": ".*",
            "hooks": [{ "type": "command", "command": "PIXTUOID_SOURCE=claude-code 'pixtuoid-hook'" }],
        });
        std::fs::write(
            cc_settings(),
            serde_json::json!({ "theme": "dark", "hooks": { "Stop": [user, legacy] } }).to_string(),
        )
        .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CLAUDE,
            Some(hooks),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cc_settings()).unwrap()).unwrap();
        assert_eq!(settings["hooks"]["Stop"], serde_json::json!([user]));
        assert_eq!(settings["theme"], "dark");
    });
}

#[test]
fn claude_verify_flags_an_unregistered_plugin() {
    with_fake_hosts(|_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CLAUDE,
            Some(hooks.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        // No `enabledPlugins` entry, as if the user disabled it.
        std::fs::write(cc_settings(), "{}").unwrap();
        let v = verify_target(&CLAUDE, Some(hooks));
        assert!(
            v.issues
                .iter()
                .any(|i| i.contains("not installed and enabled")),
            "{:?}",
            v.issues
        );
    });
}

#[test]
fn claude_uninstall_removes_the_marketplace_only_when_listed() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CLAUDE,
            Some(hooks.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let u = uninstall_target(&CLAUDE, Some(hooks.clone())).unwrap();
        assert!(matches!(u.outcome, UninstallOutcome::Removed));
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert_eq!(
            calls.matches("plugin marketplace remove pixtuoid").count(),
            1
        );
        assert!(
            !tmp.path().join("marketplace").exists(),
            "the files register wrote go with the plugin"
        );

        // Nothing registered now: a second uninstall lists and stops.
        let u2 = uninstall_target(&CLAUDE, Some(hooks)).unwrap();
        assert!(matches!(u2.outcome, UninstallOutcome::NothingToRemove));
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert_eq!(
            calls.matches("plugin marketplace remove pixtuoid").count(),
            1,
            "calls: {calls}"
        );
    });
}

#[test]
fn a_disconnect_that_deregisters_a_stranded_plugin_reports_a_removal() {
    with_fake_hosts(|_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CLAUDE,
            Some(hooks.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        // An earlier disconnect already emptied every hooks file but left the
        // plugin registered (no `claude` then).
        uninstall_target(&CLAUDE_FILE, Some(hooks.clone())).unwrap();
        let u = uninstall_target(&CLAUDE, Some(hooks)).unwrap();
        assert!(matches!(u.outcome, UninstallOutcome::Removed));
        assert!(u.plugin_left_registered.is_none());
    });
}

#[test]
fn a_custom_config_never_deletes_a_marketplace_that_is_not_ours() {
    with_fake_hosts(|_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let manifest = tmp
            .path()
            .join("marketplace/.claude-plugin/marketplace.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, r#"{"name":"someone-else"}"#).unwrap();
        uninstall_target(&CLAUDE, Some(hooks)).unwrap();
        assert!(manifest.exists());
    });
}

#[test]
fn claude_uninstall_without_claude_on_path_flags_only_a_registered_plugin() {
    let empty = tempfile::TempDir::new().unwrap();
    let config = empty.path().join("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    temp_env::with_vars(
        [
            ("PATH", Some(empty.path().as_os_str())),
            ("CLAUDE_CONFIG_DIR", Some(config.as_os_str())),
        ],
        || {
            let tmp = tempfile::TempDir::new().unwrap();
            let hooks = plugin_hooks_path(tmp.path());
            let settings = config.join("settings.json");
            // No settings.json reads as never registered.
            assert!(
                uninstall_target(&CLAUDE, Some(hooks.clone()))
                    .unwrap()
                    .plugin_left_registered
                    .is_none()
            );
            std::fs::write(&settings, "{}").unwrap();
            assert!(
                uninstall_target(&CLAUDE, Some(hooks.clone()))
                    .unwrap()
                    .plugin_left_registered
                    .is_none()
            );
            std::fs::write(
                &settings,
                r#"{"enabledPlugins":{"pixtuoid@pixtuoid":true}}"#,
            )
            .unwrap();
            assert!(
                uninstall_target(&CLAUDE, Some(hooks.clone()))
                    .unwrap()
                    .plugin_left_registered
                    .is_some()
            );
            // Disabled in Claude Code is still installed: its marketplace stays.
            std::fs::write(
                &settings,
                r#"{"enabledPlugins":{"pixtuoid@pixtuoid":false}}"#,
            )
            .unwrap();
            assert!(
                uninstall_target(&CLAUDE, Some(hooks))
                    .unwrap()
                    .plugin_left_registered
                    .is_some()
            );
        },
    );
}

#[test]
fn claude_install_fails_when_registering_fails_and_leaves_no_hooks() {
    with_fake_hosts_failing(Some("claude plugin install pixtuoid@pixtuoid"), |_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
        assert!(install_target_from(&CLAUDE, Some(hooks.clone()), hook).is_err());
        assert!(
            !has_hooks(&CLAUDE, Some(hooks)),
            "a failed connect must not read as connected"
        );
    });
}

#[test]
fn a_failed_registration_restores_the_hooks_it_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let hooks = plugin_hooks_path(tmp.path());
    let old = Some(PathBuf::from("/fake/old/pixtuoid-hook"));
    with_fake_hosts(|_| install_target_from(&CLAUDE, Some(hooks.clone()), old.clone()).unwrap());
    let before = std::fs::read_to_string(&hooks).unwrap();
    with_fake_hosts_failing(Some("claude plugin install pixtuoid@pixtuoid"), |_| {
        assert!(install_target_from(&CLAUDE, Some(hooks.clone()), old).is_err());
        assert_eq!(
            std::fs::read_to_string(&hooks).unwrap(),
            before,
            "a re-registration that wrote nothing takes nothing back"
        );
        let new = Some(PathBuf::from("/fake/new/pixtuoid-hook"));
        assert!(install_target_from(&CLAUDE, Some(hooks.clone()), new).is_err());
        assert_eq!(
            std::fs::read_to_string(&hooks).unwrap(),
            before,
            "an update that failed to register leaves the hooks it found"
        );
    });
}

#[test]
fn claude_uninstall_leaves_no_live_hooks_when_deregistering_fails() {
    with_fake_hosts_failing(Some("claude plugin marketplace list --json"), |_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
        install_target_from(&CLAUDE, Some(hooks.clone()), hook).unwrap();
        assert!(uninstall_target(&CLAUDE, Some(hooks.clone())).is_err());
        assert!(!has_hooks(&CLAUDE, Some(hooks)), "the plugin is left inert");
    });
}

/// A user's own Codex hook beside one an older pixtuoid merged into config.toml.
const CODEX_USER_HOOK: &str =
    "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"notify-send done\"\n";
const CODEX_LEGACY_HOOK: &str = "[[hooks.PreToolUse]]\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"PIXTUOID_SOURCE=codex '/old/pixtuoid-hook'\"\ntimeout = 5\nstatusMessage = \"pixtuoid visualizer\"\n_pixtuoid = true\n";

#[test]
fn codex_connect_registers_the_plugin_and_strips_the_hooks_an_older_pixtuoid_merged() {
    with_fake_hosts(|fake| {
        std::fs::write(
            codex_config(),
            format!("{CODEX_USER_HOOK}{CODEX_LEGACY_HOOK}"),
        )
        .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(std::env::current_exe().unwrap());
        let r = install_target_from(&CODEX, Some(hooks.clone()), hook.clone()).unwrap();
        assert!(matches!(r.outcome, InstallOutcome::Installed));

        let root = tmp.path().join("marketplace");
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert!(
            calls.contains(&format!("codex plugin marketplace add {}", root.display()))
                && calls.contains("codex plugin add pixtuoid@pixtuoid"),
            "calls: {calls}"
        );
        let marketplace: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".agents/plugins/marketplace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            marketplace["plugins"][0]["source"],
            serde_json::json!({ "source": "local", "path": "./plugin" })
        );
        let config = std::fs::read_to_string(codex_config()).unwrap();
        assert!(
            config.starts_with(CODEX_USER_HOOK) && !config.contains(SENTINEL_KEY),
            "the user's hook stays byte for byte and ours goes: {config}"
        );
        assert!(has_hooks(&CODEX, Some(hooks.clone())));
        let v = verify_target(&CODEX, Some(hooks.clone()));
        assert!(v.is_sound(), "{:?}", v.issues);

        // Nothing it writes carries the release, so no upgrade alone changes what
        // Codex copied and runs.
        for f in [root.join("plugin/.codex-plugin/plugin.json"), hooks.clone()] {
            let body = std::fs::read_to_string(&f).unwrap();
            assert!(
                !body.contains(env!("CARGO_PKG_VERSION")),
                "{}: {body}",
                f.display()
            );
        }

        let r2 = install_target_from(&CODEX, Some(hooks), hook).unwrap();
        assert!(matches!(r2.outcome, InstallOutcome::AlreadyUpToDate));
    });
}

#[test]
fn codex_verify_flags_a_disabled_plugin() {
    with_fake_hosts(|_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CODEX,
            Some(hooks.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let config = std::fs::read_to_string(codex_config()).unwrap();
        std::fs::write(
            codex_config(),
            config.replace("enabled = true", "enabled = false"),
        )
        .unwrap();
        let v = verify_target(&CODEX, Some(hooks));
        assert!(
            v.issues
                .iter()
                .any(|i| i.contains("not installed and enabled")),
            "{:?}",
            v.issues
        );
    });
}

/// Codex runs its cached copy, so a copy that differs from the owned hooks file is
/// what doctor names and what a re-copy fixes.
#[test]
fn codex_doctor_names_a_stale_copy_until_it_is_re_copied() {
    with_fake_hosts(|_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(std::env::current_exe().unwrap());
        install_target_from(&CODEX, Some(hooks.clone()), hook).unwrap();
        let copy = CODEX.host.and_then(|h| h.copy.as_ref()).unwrap();
        assert!(!(copy.stale)(&hooks).unwrap(), "connect copies the plugin");

        let cached =
            codex_config().with_file_name("plugins/cache/pixtuoid/pixtuoid/local/hooks/hooks.json");
        std::fs::write(&cached, "{}\n").unwrap();
        assert!((copy.stale)(&hooks).unwrap());
        let v = verify_target(&CODEX, Some(hooks.clone()));
        assert!(
            v.issues.iter().any(|i| i.contains("runs an older copy")),
            "{:?}",
            v.issues
        );

        (copy.refresh)().unwrap();
        assert!(!(copy.stale)(&hooks).unwrap());
        assert!(verify_target(&CODEX, Some(hooks)).is_sound());
    });
}

/// Codex refuses to re-add a marketplace name from another source, so a connect
/// after pixtuoid's config dir moved removes the old one first, and only then.
#[test]
fn codex_connect_re_adds_a_marketplace_codex_records_from_a_moved_root() {
    with_fake_hosts(|fake| {
        let old = tempfile::TempDir::new().unwrap();
        std::fs::write(
            codex_config(),
            format!(
                "[marketplaces.pixtuoid]\nsource_type = 'local'\nsource = '{}'\n",
                old.path().display()
            ),
        )
        .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
        install_target_from(&CODEX, Some(hooks.clone()), hook.clone()).unwrap();
        let removes = || {
            std::fs::read_to_string(fake.join("calls.log"))
                .unwrap()
                .matches("codex plugin marketplace remove pixtuoid")
                .count()
        };
        assert_eq!(removes(), 1);
        install_target_from(&CODEX, Some(hooks), hook).unwrap();
        assert_eq!(removes(), 1, "the same root is re-added in place");
    });
}

/// A host CLI off PATH fails a connect before it touches the legacy config, so the
/// hooks an older pixtuoid merged keep firing.
#[test]
fn a_connect_without_the_host_cli_leaves_the_legacy_hooks() {
    with_fake_hosts(|fake| {
        std::fs::write(codex_config(), CODEX_LEGACY_HOOK).unwrap();
        std::fs::remove_file(fake.join(if cfg!(windows) { "codex.cmd" } else { "codex" })).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
        let err = temp_env::with_var("PATH", Some(fake), || {
            install_target_from(&CODEX, Some(hooks.clone()), hook).unwrap_err()
        });
        assert!(format!("{err:#}").contains("not on PATH"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(codex_config()).unwrap(),
            CODEX_LEGACY_HOOK
        );
        assert!(!has_hooks(&CODEX, Some(hooks)));
    });
}

/// A legacy config the strip cannot parse fails the disconnect, but only after the
/// plugin is deregistered.
#[test]
fn a_disconnect_removes_the_plugin_before_an_unparsable_legacy_config_fails_it() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = reasonix_manifest_path(tmp.path());
        install_target_from(
            &REASONIX,
            Some(manifest.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        std::fs::write(reasonix_settings(), "{not json").unwrap();
        assert!(uninstall_target(&REASONIX, Some(manifest)).is_err());
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert!(
            calls.contains("reasonix plugin remove pixtuoid --yes"),
            "calls: {calls}"
        );
    });
}

/// Each host reads its own state file: absent is not registered, and one it cannot
/// parse is an error rather than a guess.
#[test]
fn a_host_state_file_absent_reads_unregistered_and_unparsable_errs() {
    with_fake_hosts(|_| {
        for (t, state, unparsable) in [
            (&CODEX, codex_config(), "not = = toml [[["),
            (&REASONIX, reasonix_packages(), "{not json"),
        ] {
            let is_registered = t.host.unwrap().is_registered;
            assert!(!is_registered().unwrap(), "{}: absent", t.name);
            std::fs::write(&state, unparsable).unwrap();
            assert!(is_registered().is_err(), "{}: unparsable", t.name);
        }
        std::fs::write(
            reasonix_packages(),
            r#"{"version":1,"plugins":[{"name":"other","enabled":true}]}"#,
        )
        .unwrap();
        assert!(!(REASONIX.host.unwrap().is_registered)().unwrap());
    });
}

#[test]
fn codex_disconnect_removes_the_plugin_and_its_marketplace_through_codex() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CODEX,
            Some(hooks.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let u = uninstall_target(&CODEX, Some(hooks.clone())).unwrap();
        assert!(matches!(u.outcome, UninstallOutcome::Removed));
        assert!(!tmp.path().join("marketplace").exists());
        let removes = |calls: &str| {
            (
                calls
                    .matches("codex plugin remove pixtuoid@pixtuoid")
                    .count(),
                calls
                    .matches("codex plugin marketplace remove pixtuoid")
                    .count(),
            )
        };
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert_eq!(removes(&calls), (1, 1), "calls: {calls}");

        // Nothing registered now: a second disconnect starts no CLI.
        let u2 = uninstall_target(&CODEX, Some(hooks)).unwrap();
        assert!(matches!(u2.outcome, UninstallOutcome::NothingToRemove));
        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert_eq!(removes(&calls), (1, 1), "calls: {calls}");
    });
}

#[test]
fn codex_disconnect_strips_the_hooks_an_older_pixtuoid_merged_without_a_plugin() {
    with_fake_hosts(|fake| {
        std::fs::write(codex_config(), CODEX_LEGACY_HOOK).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let u = uninstall_target(&CODEX, Some(plugin_hooks_path(tmp.path()))).unwrap();
        assert!(matches!(u.outcome, UninstallOutcome::Removed));
        assert!(
            !std::fs::read_to_string(codex_config())
                .unwrap()
                .contains(SENTINEL_KEY)
        );
        assert!(
            !fake.join("calls.log").exists(),
            "no CLI for nothing registered"
        );
    });
}

#[test]
fn codex_disconnect_without_codex_on_path_leaves_a_registered_plugin() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        install_target_from(
            &CODEX,
            Some(hooks.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        std::fs::remove_file(fake.join(if cfg!(windows) { "codex.cmd" } else { "codex" })).unwrap();
        let u = temp_env::with_var("PATH", Some(fake), || {
            uninstall_target(&CODEX, Some(hooks.clone())).unwrap()
        });
        assert!(u.plugin_left_registered.is_some());
        assert!(
            tmp.path()
                .join("marketplace/.agents/plugins/marketplace.json")
                .exists(),
            "a marketplace Codex still lists keeps its manifest"
        );
    });
}

/// The legacy strip runs before the plugin registers, so a registration that fails
/// leaves neither firing: stripping after would leave the copy and the legacy
/// hooks both firing whenever the strip failed.
#[test]
fn codex_connect_fails_when_adding_the_plugin_fails_and_leaves_no_hooks() {
    with_fake_hosts_failing(Some("codex plugin add pixtuoid@pixtuoid"), |_| {
        std::fs::write(codex_config(), CODEX_LEGACY_HOOK).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = plugin_hooks_path(tmp.path());
        let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
        assert!(install_target_from(&CODEX, Some(hooks.clone()), hook).is_err());
        assert!(!has_hooks(&CODEX, Some(hooks)));
        assert!(
            !std::fs::read_to_string(codex_config())
                .unwrap()
                .contains(SENTINEL_KEY)
        );
    });
}

/// A legacy config that no longer parses is never rewritten: the connect fails
/// before the strip touches it, and before any registration.
#[test]
fn a_connect_over_an_unparsable_legacy_config_errs_and_rewrites_nothing() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        for (t, legacy, primary, register) in [
            (
                &CODEX,
                codex_config(),
                plugin_hooks_path(tmp.path()),
                "codex plugin",
            ),
            (
                &REASONIX,
                reasonix_settings(),
                reasonix_manifest_path(tmp.path()),
                "reasonix plugin",
            ),
        ] {
            std::fs::write(&legacy, "{ not = = valid [[[").unwrap();
            let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
            assert!(
                install_target_from(t, Some(primary.clone()), hook).is_err(),
                "{}",
                t.name
            );
            assert_eq!(
                std::fs::read_to_string(&legacy).unwrap(),
                "{ not = = valid [[[",
                "{}",
                t.name
            );
            assert!(!has_hooks(t, Some(primary)), "{}", t.name);
            let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap_or_default();
            assert!(!calls.contains(register), "{}: {calls}", t.name);
        }
    });
}

/// Codex is detected from its own config.toml, never from the plugin pixtuoid
/// writes: an empty Codex home is not Codex, and a connect alone cannot detect it.
#[test]
fn codex_is_detected_from_its_own_config_only() {
    let codex_home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("CODEX_HOME", Some(codex_home.path())),
            ("XDG_CONFIG_HOME", Some(xdg.path())),
        ],
        || {
            assert!(!(CODEX.presence_probe)());
            let ours = (CODEX.default_config_path)().unwrap();
            std::fs::create_dir_all(ours.parent().unwrap()).unwrap();
            std::fs::write(&ours, "{}").unwrap();
            assert!(
                !(CODEX.presence_probe)(),
                "the plugin pixtuoid writes says nothing"
            );
            std::fs::write(codex_home.path().join("config.toml"), "").unwrap();
            assert!((CODEX.presence_probe)());
        },
    );
}

/// `<root>/pixtuoid/reasonix-plugin.json` inside `tmp`.
fn reasonix_manifest_path(tmp: &std::path::Path) -> PathBuf {
    tmp.join("root")
        .join("pixtuoid")
        .join("reasonix-plugin.json")
}

fn reasonix_settings() -> PathBuf {
    reasonix_packages().with_file_name("settings.json")
}

#[test]
fn reasonix_connect_links_the_plugin_and_strips_the_hooks_an_older_pixtuoid_merged() {
    with_fake_hosts(|fake| {
        let user = serde_json::json!({ "command": "notify-send done" });
        let legacy = serde_json::json!({ SENTINEL_KEY: true, "command": "PIXTUOID_SOURCE=reasonix '/old/pixtuoid-hook'" });
        std::fs::write(
            reasonix_settings(),
            serde_json::json!({ "hooks": { "Stop": [user, legacy] } }).to_string(),
        )
        .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = reasonix_manifest_path(tmp.path());
        let hook = Some(std::env::current_exe().unwrap());
        let r = install_target_from(&REASONIX, Some(manifest.clone()), hook.clone()).unwrap();
        assert!(matches!(r.outcome, InstallOutcome::Installed));

        let calls = std::fs::read_to_string(fake.join("calls.log")).unwrap();
        assert!(
            calls.contains(&format!(
                "reasonix plugin install {} --link --replace --yes",
                manifest.parent().unwrap().display()
            )),
            "calls: {calls}"
        );
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(reasonix_settings()).unwrap()).unwrap();
        assert_eq!(settings["hooks"]["Stop"], serde_json::json!([user]));
        assert!(has_hooks(&REASONIX, Some(manifest.clone())));
        let v = verify_target(&REASONIX, Some(manifest.clone()));
        assert!(v.is_sound(), "{:?}", v.issues);

        let r2 = install_target_from(&REASONIX, Some(manifest), hook).unwrap();
        assert!(matches!(r2.outcome, InstallOutcome::AlreadyUpToDate));
    });
}

#[test]
fn reasonix_verify_flags_a_disabled_plugin() {
    with_fake_hosts(|_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = reasonix_manifest_path(tmp.path());
        install_target_from(
            &REASONIX,
            Some(manifest.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let state = std::fs::read_to_string(reasonix_packages()).unwrap();
        std::fs::write(
            reasonix_packages(),
            state.replace("\"enabled\":true", "\"enabled\":false"),
        )
        .unwrap();
        let v = verify_target(&REASONIX, Some(manifest));
        assert!(
            v.issues
                .iter()
                .any(|i| i.contains("not installed and enabled")),
            "{:?}",
            v.issues
        );
    });
}

#[test]
fn reasonix_disconnect_removes_the_plugin_through_reasonix_and_deletes_its_dir() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = reasonix_manifest_path(tmp.path());
        install_target_from(
            &REASONIX,
            Some(manifest.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let u = uninstall_target(&REASONIX, Some(manifest.clone())).unwrap();
        assert!(matches!(u.outcome, UninstallOutcome::Removed));
        assert!(!tmp.path().join("root").exists());
        let removes = || {
            std::fs::read_to_string(fake.join("calls.log"))
                .unwrap()
                .matches("reasonix plugin remove pixtuoid --yes")
                .count()
        };
        assert_eq!(removes(), 1);

        let u2 = uninstall_target(&REASONIX, Some(manifest)).unwrap();
        assert!(matches!(u2.outcome, UninstallOutcome::NothingToRemove));
        assert_eq!(removes(), 1, "nothing registered starts no CLI");
    });
}

#[test]
fn reasonix_disconnect_without_reasonix_on_path_leaves_a_registered_plugin_loadable() {
    with_fake_hosts(|fake| {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = reasonix_manifest_path(tmp.path());
        install_target_from(
            &REASONIX,
            Some(manifest.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        std::fs::remove_file(fake.join(if cfg!(windows) {
            "reasonix.cmd"
        } else {
            "reasonix"
        }))
        .unwrap();
        let u = temp_env::with_var("PATH", Some(fake), || {
            uninstall_target(&REASONIX, Some(manifest.clone())).unwrap()
        });
        assert!(u.plugin_left_registered.is_some());
        let left =
            crate::install::reasonix::verify_schema(&std::fs::read_to_string(&manifest).unwrap());
        assert_eq!(
            left.shim,
            verify::ShimRef::Unknown,
            "the linked manifest stays, with no hooks"
        );
    });
}

#[test]
fn reasonix_connect_fails_when_linking_fails_and_leaves_no_hooks() {
    with_fake_hosts_failing(Some("reasonix plugin install"), |_| {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = reasonix_manifest_path(tmp.path());
        let hook = Some(PathBuf::from("/fake/pixtuoid-hook"));
        assert!(install_target_from(&REASONIX, Some(manifest.clone()), hook).is_err());
        assert!(!has_hooks(&REASONIX, Some(manifest)));
    });
}

/// Redirects every CLI state resolver an owned file renders under (OpenClaw's plugin
/// dir, dsh's plugin file) at temp dirs for `f`, so no test writes a real plugin.
fn with_owned_homes<R>(f: impl FnOnce() -> R) -> R {
    let oc_home = tempfile::TempDir::new().unwrap();
    let dsh_home = tempfile::TempDir::new().unwrap();
    temp_env::with_vars(
        [
            ("OPENCLAW_STATE_DIR", Some(oc_home.path())),
            ("DSH_HOME", Some(dsh_home.path())),
        ],
        f,
    )
}

/// `owned_state` and `converge` re-render with the shim an install baked, read back out
/// of the FIRST owned file — so that read must round-trip every hook shape a target
/// writes: an explicit path, a PATH-resolved bare name, a path with a space.
#[test]
fn every_owned_set_re_renders_byte_identical_from_the_shim_its_first_file_bakes() {
    with_owned_homes(|| {
        let shims = [
            abs_fixture("/opt/a dir/pixtuoid-hook", r"C:\a dir\pixtuoid-hook.exe"),
            abs_fixture("/opt/bin/pixtuoid-hook", r"C:\bin\pixtuoid-hook.exe"),
        ];
        let primary = std::env::temp_dir().join("primary");
        let mut covered = 0;
        for t in target::TARGETS {
            let Some(files) = t.primary.owned_files() else {
                continue;
            };
            let mut asserted = 0;
            for shim in &shims {
                for explicit in [true, false] {
                    let hook = match (t.hook_command)(shim, explicit) {
                        Ok(hook) => hook,
                        // A command cmd.exe runs refuses a spaced path at connect,
                        // so it is never written.
                        Err(e) if cfg!(windows) && format!("{e:#}").contains("cmd.exe") => {
                            continue;
                        }
                        Err(e) => panic!("{}: {e:#}", t.name),
                    };
                    let rendered = files.render(&primary, &hook).unwrap();
                    let back = files.installed_shim(&rendered[0].1);
                    let rehooked = rehook(t.hook_command, &back)
                        .unwrap()
                        .expect("a rendered file gives back its shim");
                    assert_eq!(
                        files.render(&primary, &rehooked).unwrap(),
                        rendered,
                        "{} ({shim:?}, explicit: {explicit})",
                        t.name
                    );
                    asserted += 1;
                }
            }
            assert!(asserted > 0, "{}: no hook shape round-tripped", t.name);
            covered += 1;
        }
        assert!(
            covered > 0,
            "no target owns a file — did the registry change?"
        );
    });
}

/// The verdict is EXACT: any byte an installed file holds that this binary's render
/// with the same shim does not — a comment, a trailing newline — reads `Differs`.
#[test]
fn owned_state_is_current_after_install_and_differs_after_any_byte_edit() {
    with_owned_homes(|| {
        with_fake_hosts(|_| {
            for t in target::TARGETS {
                let Some(files) = t.primary.owned_files() else {
                    continue;
                };
                let tmp = tempfile::TempDir::new().unwrap();
                let cfg = plugin_hooks_path(tmp.path());
                install_target_from(
                    t,
                    Some(cfg.clone()),
                    Some(PathBuf::from("/fake/pixtuoid-hook")),
                )
                .unwrap();
                let state = owned_state(files, t.hook_command, &cfg).unwrap();
                assert!(
                    matches!(state, OwnedState::Current),
                    "{}: {state:?}",
                    t.name
                );

                let entry = files.render(&cfg, io::HOOK_BIN).unwrap().remove(0).0;
                let body = io::read_config(&entry).unwrap();
                std::fs::write(&entry, drift_edit(&entry, &body)).unwrap();
                match owned_state(files, t.hook_command, &cfg).unwrap() {
                    OwnedState::Differs(d) => assert_eq!(
                        d.iter().map(|(p, _)| p).collect::<Vec<_>>(),
                        [&entry],
                        "{}",
                        t.name
                    ),
                    other => panic!("{}: a one-byte edit read {other:?}", t.name),
                }

                std::fs::write(&entry, "").unwrap();
                let state = owned_state(files, t.hook_command, &cfg).unwrap();
                assert!(
                    matches!(state, OwnedState::Unknown),
                    "{}: {state:?}",
                    t.name
                );
            }
        })
    });
}

/// A drift edit to an owned file: a whole-line comment wherever the format has one,
/// a trailing newline in JSON, which has none.
fn drift_edit(p: &std::path::Path, body: &str) -> String {
    if p.extension().is_some_and(|e| e == "json") {
        format!("{body}\n")
    } else {
        format!("// a comment no release shipped\n{body}")
    }
}

/// `(inode, mtime)` on Unix, `mtime` elsewhere: what a rewrite always changes.
fn write_stamp(p: &std::path::Path) -> (u64, std::time::SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&m);
    #[cfg(not(unix))]
    let ino = 0;
    (ino, m.modified().unwrap())
}

/// Convergence rewrites only what differs, with the shim the install baked, and
/// never touches the config a target merges into: a current install keeps its
/// inode and mtime, a stale file comes back exactly as connect wrote it however
/// `PIXTUOID_HOOK` is set now, and the user's own config edit survives byte for byte.
#[test]
fn converge_rewrites_only_stale_owned_files_with_the_installed_shim() {
    with_owned_homes(|| {
        with_fake_hosts(|fake| {
            let host_calls = || std::fs::read_to_string(fake.join("calls.log")).unwrap_or_default();
            for t in target::TARGETS {
                let Some(files) = t.primary.owned_files() else {
                    continue;
                };
                let tmp = tempfile::TempDir::new().unwrap();
                let cfg = plugin_hooks_path(tmp.path());
                let shim = Some(abs_fixture(
                    "/opt/one/pixtuoid-hook",
                    r"C:\one\pixtuoid-hook.exe",
                ));
                install_target_from(t, Some(cfg.clone()), shim).unwrap();
                let calls = host_calls();
                let entry = files.render(&cfg, io::HOOK_BIN).unwrap().remove(0).0;
                let installed = io::read_config(&entry).unwrap();

                let stamp = write_stamp(&entry);
                assert!(
                    converge(t, Some(cfg.clone())).unwrap().rewritten.is_empty(),
                    "{}",
                    t.name
                );
                assert_eq!(
                    write_stamp(&entry),
                    stamp,
                    "{}: a current file was rewritten",
                    t.name
                );

                std::fs::write(&entry, format!("{installed}\n")).unwrap();
                let user_config = matches!(t.primary, Primary::Config { .. }).then(|| {
                    let edited = format!("{}\n", io::read_config(&cfg).unwrap());
                    std::fs::write(&cfg, &edited).unwrap();
                    edited
                });
                let other = abs_fixture("/opt/two/pixtuoid-hook", r"C:\two\pixtuoid-hook.exe");
                let rewritten = temp_env::with_var(io::HOOK_OVERRIDE_ENV, Some(&other), || {
                    converge(t, Some(cfg.clone())).unwrap().rewritten
                });
                assert_eq!(rewritten, std::slice::from_ref(&entry), "{}", t.name);
                assert_eq!(io::read_config(&entry).unwrap(), installed, "{}", t.name);
                assert_eq!(
                    host_calls(),
                    calls,
                    "{}: converge ran the host's CLI",
                    t.name
                );
                if let Some(edited) = user_config {
                    assert_eq!(
                        io::read_config(&cfg).unwrap(),
                        edited,
                        "{}: config touched",
                        t.name
                    );
                }
            }
        })
    });
}

/// A user who disabled the plugin through OpenClaw's own CLI keeps it disabled:
/// convergence writes the plugin dir, never openclaw.json.
#[test]
fn converge_never_re_enables_an_openclaw_plugin_the_user_disabled() {
    with_owned_homes(|| {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("openclaw.json");
        install_target_from(
            &OPENCLAW,
            Some(cfg.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let disabled = io::read_config(&cfg)
            .unwrap()
            .replace("\"enabled\": true", "\"enabled\": false");
        std::fs::write(&cfg, &disabled).unwrap();
        let files = OPENCLAW.primary.owned_files().unwrap();
        let entry = files.render(&cfg, io::HOOK_BIN).unwrap().remove(0).0;
        std::fs::write(
            &entry,
            "// an older plugin\nconst HOOK_PATH = \"/fake/pixtuoid-hook\";\n",
        )
        .unwrap();

        assert_eq!(
            converge(&OPENCLAW, Some(cfg.clone())).unwrap().rewritten,
            [entry]
        );
        assert_eq!(io::read_config(&cfg).unwrap(), disabled);
    });
}

/// A file another pixtuoid holds the lock on is left alone and reported, while the
/// target's other files still converge: OpenClaw's `index.js` refreshed with its
/// `package.json` locked must still reach the launch's restart notice.
#[test]
fn converge_skips_a_locked_file_and_still_reports_the_rest() {
    with_owned_homes(|| {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("openclaw.json");
        install_target_from(
            &OPENCLAW,
            Some(cfg.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let files = (OPENCLAW.primary.owned_files().unwrap())
            .render(&cfg, io::HOOK_BIN)
            .unwrap();
        let (entry, locked) = (files[0].0.clone(), files[2].0.clone());
        for p in [&entry, &locked] {
            let body = io::read_config(p).unwrap();
            std::fs::write(p, format!("{body}\n")).unwrap();
        }
        let stale = io::read_config(&locked).unwrap();

        let _held = io::lock_config(&locked).unwrap();
        let done = converge(&OPENCLAW, Some(cfg.clone())).unwrap();
        assert_eq!(done.rewritten, [entry]);
        assert_eq!(
            done.failed.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            [&locked]
        );
        assert_eq!(io::read_config(&locked).unwrap(), stale);
    });
}

/// An older pixtuoid's hooks.json, two registrations short, is rewritten whole by a
/// re-install: the owned primary is compared with this binary's render, not merged.
#[test]
fn reinstall_adds_newly_registered_events_to_an_older_claude_hooks_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("hooks.json");
    let hook = || Some(PathBuf::from("/fake/pixtuoid-hook"));
    install_target_from(&CLAUDE_FILE, Some(cfg.clone()), hook()).unwrap();
    let mut older: serde_json::Value =
        serde_json::from_str(&io::read_config(&cfg).unwrap()).unwrap();
    for ev in ["SubagentStart", "SubagentStop"] {
        older["hooks"].as_object_mut().unwrap().remove(ev);
    }
    std::fs::write(&cfg, serde_json::to_string_pretty(&older).unwrap()).unwrap();

    let r = install_target_from(&CLAUDE_FILE, Some(cfg.clone()), hook()).unwrap();
    assert!(matches!(r.outcome, InstallOutcome::Installed));
    let v: serde_json::Value = serde_json::from_str(&io::read_config(&cfg).unwrap()).unwrap();
    for ev in crate::install::claude::EVENTS {
        assert!(
            v["hooks"][*ev][0]["hooks"][0]["command"].is_string(),
            "event {ev} must be installed after the upgrade re-run"
        );
    }
}

/// A moved shim is re-baked into every owned primary: its re-install compares the
/// file with this binary's render at the new path, never a sentinel.
#[test]
fn reinstall_re_bakes_a_moved_shim_into_every_owned_primary() {
    with_fake_hosts(|_| {
        let mut covered = 0;
        for &t in target::TARGETS {
            let Primary::Owned { render, .. } = t.primary else {
                continue;
            };
            let tmp = tempfile::TempDir::new().unwrap();
            let cfg = plugin_hooks_path(tmp.path());
            let moved = abs_fixture("/opt/b/pixtuoid-hook", r"C:\b\pixtuoid-hook.exe");
            let first = abs_fixture("/opt/a/pixtuoid-hook", r"C:\a\pixtuoid-hook.exe");
            install_target_from(t, Some(cfg.clone()), Some(first)).unwrap();
            let r = install_target_from(t, Some(cfg.clone()), Some(moved.clone())).unwrap();
            assert!(matches!(r.outcome, InstallOutcome::Installed), "{}", t.name);
            let want = render(&(t.hook_command)(&moved, true).unwrap()).unwrap();
            assert_eq!(io::read_config(&cfg).unwrap(), want, "{}", t.name);
            covered += 1;
        }
        assert!(
            covered > 0,
            "no target owns its primary — did the registry change?"
        );
    });
}

/// A disconnected owned primary holds a stub with no shim to read back, so a launch
/// that converges it anyway (a stale connected set) leaves the stub alone.
#[test]
fn converge_leaves_a_disconnected_owned_primary_alone() {
    with_fake_hosts(|_| {
        for &t in target::TARGETS {
            if !matches!(t.primary, Primary::Owned { .. }) {
                continue;
            }
            let tmp = tempfile::TempDir::new().unwrap();
            let cfg = plugin_hooks_path(tmp.path());
            install_target_from(
                t,
                Some(cfg.clone()),
                Some(PathBuf::from("/fake/pixtuoid-hook")),
            )
            .unwrap();
            uninstall_target(t, Some(cfg.clone())).unwrap();
            let stub = io::read_config(&cfg).unwrap();
            assert!(
                converge(t, Some(cfg.clone())).unwrap().rewritten.is_empty(),
                "{}",
                t.name
            );
            assert_eq!(io::read_config(&cfg).unwrap(), stub, "{}", t.name);
        }
    });
}

/// The file that bakes the shim is what convergence re-renders from: a sibling gone
/// missing is written back, but with the entry gone there is no shim to bake, so
/// nothing is written and the reconnect doctor names is the repair.
#[test]
fn converge_heals_a_missing_sibling_but_not_a_missing_entry() {
    with_owned_homes(|| {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("openclaw.json");
        install_target_from(
            &OPENCLAW,
            Some(cfg.clone()),
            Some(PathBuf::from("/fake/pixtuoid-hook")),
        )
        .unwrap();
        let files = (OPENCLAW.primary.owned_files().unwrap())
            .render(&cfg, io::HOOK_BIN)
            .unwrap();
        let (entry, sibling) = (&files[0].0, &files[1].0);

        std::fs::remove_file(sibling).unwrap();
        assert_eq!(
            converge(&OPENCLAW, Some(cfg.clone())).unwrap().rewritten,
            std::slice::from_ref(sibling)
        );
        assert!(sibling.exists());

        std::fs::remove_file(entry).unwrap();
        std::fs::write(sibling, "{}").unwrap();
        assert!(
            converge(&OPENCLAW, Some(cfg.clone()))
                .unwrap()
                .rewritten
                .is_empty()
        );
        assert!(!entry.exists());
    });
}

/// A hooks.json from before the `PIXTUOID_SOURCE=` stamp ran the bare shim name; the
/// shim stays where PATH finds it, while the command takes this build's shape.
#[cfg(unix)]
#[test]
fn converge_stamps_a_pre_stamp_claude_hooks_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("hooks.json");
    std::fs::write(
        &cfg,
        crate::install::claude::render_hooks(io::HOOK_BIN).unwrap(),
    )
    .unwrap();

    assert_eq!(
        converge(&CLAUDE_FILE, Some(cfg.clone())).unwrap().rewritten,
        std::slice::from_ref(&cfg)
    );
    let stamped = (CLAUDE.hook_command)(std::path::Path::new(io::HOOK_BIN), false).unwrap();
    assert_eq!(
        io::read_config(&cfg).unwrap(),
        crate::install::claude::render_hooks(&stamped).unwrap()
    );
}

/// A beside entry that no longer gives back a shim (truncated, hand-edited) is HARD:
/// a launch cannot re-render it, so only a reconnect brings the plugin back.
#[test]
fn verify_target_hard_flags_a_beside_entry_with_no_readable_shim() {
    with_owned_homes(|| {
        let mut covered = 0;
        for &t in target::TARGETS {
            let Some(render) = beside_of(t) else {
                continue;
            };
            let tmp = tempfile::TempDir::new().unwrap();
            let cfg = tmp.path().join("config");
            install_target_from(
                t,
                Some(cfg.clone()),
                Some(PathBuf::from("/fake/pixtuoid-hook")),
            )
            .unwrap();
            let entry = render(io::HOOK_BIN).unwrap().remove(0).0;
            std::fs::write(&entry, "").unwrap();
            let v = verify_target(t, Some(cfg));
            assert!(
                v.issues
                    .iter()
                    .any(|i| i.contains("could not read the baked shim path")),
                "{}: {:?}",
                t.name,
                v.issues
            );
            covered += 1;
        }
        assert!(
            covered > 0,
            "no target has beside files — did the registry change?"
        );
    });
}

/// A deleted sibling is a missing artifact, not drift: the launch writes it back,
/// so doctor must not also call it a file that "differs".
#[test]
fn verify_target_reports_a_deleted_sibling_as_missing_not_as_drift() {
    with_owned_homes(|| {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("openclaw.json");
        let shim = std::env::current_exe().unwrap();
        install_target_from(&OPENCLAW, Some(cfg.clone()), Some(shim)).unwrap();
        let sibling = OPENCLAW
            .primary
            .owned_files()
            .unwrap()
            .render(&cfg, io::HOOK_BIN)
            .unwrap()
            .remove(1)
            .0;
        std::fs::remove_file(&sibling).unwrap();
        let v = verify_target(&OPENCLAW, Some(cfg));
        let name = sibling.file_name().unwrap().to_string_lossy();
        assert!(
            v.issues
                .iter()
                .any(|i| i.contains("plugin artifact missing") && i.contains(&*name)),
            "{:?}",
            v.issues
        );
        assert!(
            !v.issues
                .iter()
                .any(|i| i.contains("differs from the plugin")),
            "{:?}",
            v.issues
        );
    });
}
