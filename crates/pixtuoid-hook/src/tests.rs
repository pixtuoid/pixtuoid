use super::*;
use serde_json::json;

#[test]
fn stdin_cap_plus_headroom_equals_the_pipe_quota() {
    assert_eq!(STDIN_CAP + STAMP_HEADROOM, 1 << 20);
}

#[test]
fn stamp_headroom_covers_worst_case_stamps() {
    let mut p = json!({});
    let map = p.as_object_mut().unwrap();
    // 64 is far longer than any registered CLI name — headroom for custom ones.
    enrich_payload(
        map,
        Some("x".repeat(64)),
        Some("x".repeat(64)),
        u64::MAX,
        || Some(u32::MAX),
    );
    let stamped = serde_json::to_vec(&p).unwrap();
    // minus the bare `{}` baseline, plus the trailing '\n' main appends.
    let overhead = (stamped.len() - 2 + 1) as u64;
    assert!(
        overhead <= STAMP_HEADROOM,
        "stamps ({overhead}B) must fit within STAMP_HEADROOM ({STAMP_HEADROOM}B)"
    );
}

#[test]
fn stamps_parent_pid_under_pid_when_absent() {
    let mut p = json!({ "hook_event_name": "Stop" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, Some("hermes".into()), None, 1, || Some(4242));
    assert_eq!(map["_pid"], json!(4242u32));
}

/// An upstream `_pid` is kept AND the resolver is never run — on Windows that
/// resolver is a process-table walk, so a plugin that stamped its own pid must
/// not pay for one.
#[test]
fn an_upstream_pid_is_kept_and_the_resolver_never_runs() {
    let mut p = json!({ "_pid": 777 });
    let map = p.as_object_mut().unwrap();
    let mut resolved = false;
    enrich_payload(map, Some("opencode".into()), None, 1, || {
        resolved = true;
        Some(4242)
    });
    assert_eq!(map["_pid"], json!(777), "upstream value kept");
    assert!(
        !resolved,
        "the walk must not run when _pid is already present"
    );
}

#[test]
fn now_ms_is_a_real_wall_clock_reading() {
    // 1_704_067_200_000 is 2024-01-01 in ms.
    assert!(now_ms() > 1_704_067_200_000);
}

#[test]
fn stamps_cli_source_under_private_key_and_leaves_public_source_untouched() {
    let mut p = json!({ "hook_event_name": "SessionStart", "source": "startup" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, Some("claude-code".into()), None, 123, || None);
    assert_eq!(map["_pixtuoid_source"], json!("claude-code"));
    assert_eq!(map["source"], json!("startup"), "public reason untouched");
    assert_eq!(map["_shim_ts_ms"], json!(123u64));
}

#[test]
fn no_source_env_omits_private_key_so_decoder_defaults_to_claude() {
    let mut p = json!({ "hook_event_name": "Stop" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, None, None, 1, || None);
    assert!(map.get("_pixtuoid_source").is_none());
}

#[test]
fn empty_source_env_is_ignored() {
    // Seeded with an inbound key: the empty-source path must strip it too,
    // not merely decline to insert.
    let mut p = json!({ "_pixtuoid_source": "codex" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, Some(String::new()), None, 1, || None);
    assert!(map.get("_pixtuoid_source").is_none());
}

#[test]
fn inbound_spoofed_private_key_is_stripped_when_no_source_resolves() {
    let mut p = json!({ "hook_event_name": "Stop", "_pixtuoid_source": "codex" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, None, None, 1, || None);
    assert!(
        map.get("_pixtuoid_source").is_none(),
        "inbound spoofed key must be stripped, not passed through"
    );
}

#[test]
fn inbound_spoofed_private_key_is_overwritten_when_source_resolves() {
    let mut p = json!({ "_pixtuoid_source": "codex" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, Some("reasonix".into()), None, 1, || None);
    assert_eq!(map["_pixtuoid_source"], json!("reasonix"));
}

#[test]
fn a_hook_event_from_argv_is_stamped_and_an_inbound_one_is_stripped() {
    let mut p = json!({ "conversationId": "c", "_pixtuoid_event": "Stop" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, None, Some("PreToolUse".into()), 1, || None);
    assert_eq!(map["_pixtuoid_event"], json!("PreToolUse"));

    let mut p = json!({ "_pixtuoid_event": "Stop" });
    let map = p.as_object_mut().unwrap();
    enrich_payload(map, None, None, 1, || None);
    assert!(map.get("_pixtuoid_event").is_none());
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn source_from_argv_reads_space_form() {
    assert_eq!(
        source_from_argv(&argv(&["pixtuoid-hook", "--source", "codex"])),
        Some("codex".into())
    );
}

#[test]
fn source_from_argv_reads_equals_form() {
    assert_eq!(
        source_from_argv(&argv(&["pixtuoid-hook", "--source=codex"])),
        Some("codex".into())
    );
}

#[test]
fn source_from_argv_absent_is_none() {
    assert_eq!(source_from_argv(&argv(&["pixtuoid-hook"])), None);
}

#[test]
fn source_from_argv_missing_value_is_none() {
    assert_eq!(
        source_from_argv(&argv(&["pixtuoid-hook", "--source"])),
        None
    );
}

#[test]
fn source_from_argv_empty_value_is_none() {
    assert_eq!(
        source_from_argv(&argv(&["pixtuoid-hook", "--source", ""])),
        None
    );
    assert_eq!(
        source_from_argv(&argv(&["pixtuoid-hook", "--source="])),
        None
    );
}

#[test]
fn event_from_argv_reads_both_forms_and_rejects_empty() {
    assert_eq!(
        event_from_argv(&argv(&[
            "pixtuoid-hook",
            "--source",
            "codewhale",
            "--event",
            "session_start"
        ])),
        Some("session_start".into())
    );
    assert_eq!(
        event_from_argv(&argv(&["pixtuoid-hook", "--event=tool_call_before"])),
        Some("tool_call_before".into())
    );
    assert_eq!(event_from_argv(&argv(&["pixtuoid-hook"])), None);
    assert_eq!(event_from_argv(&argv(&["pixtuoid-hook", "--event"])), None);
    assert_eq!(
        event_from_argv(&argv(&["pixtuoid-hook", "--event", ""])),
        None
    );
    assert_eq!(event_from_argv(&argv(&["pixtuoid-hook", "--event="])), None);
}

#[test]
fn hook_event_from_argv_reads_both_forms_and_never_the_env_mode_flag() {
    assert_eq!(
        hook_event_from_argv(&argv(&[
            "pixtuoid-hook",
            "--source",
            "antigravity",
            "--hook-event",
            "PreToolUse"
        ])),
        Some("PreToolUse".into())
    );
    assert_eq!(
        hook_event_from_argv(&argv(&["pixtuoid-hook", "--hook-event=Stop"])),
        Some("Stop".into())
    );
    assert_eq!(
        hook_event_from_argv(&argv(&["pixtuoid-hook", "--hook-event", ""])),
        None
    );
    assert_eq!(
        hook_event_from_argv(&argv(&["pixtuoid-hook", "--event", "session_start"])),
        None
    );
    assert_eq!(
        event_from_argv(&argv(&["pixtuoid-hook", "--hook-event", "Stop"])),
        None,
        "a stdin payload's event name must never switch on env mode"
    );
}

#[test]
fn env_payload_folds_codewhale_env_into_the_envelope() {
    let env: std::collections::HashMap<&str, &str> = [
        ("DEEPSEEK_WORKSPACE", "/repo"),
        ("DEEPSEEK_TOOL_NAME", "exec_shell"),
        ("DEEPSEEK_TOOL_ARGS", r#"{"command":"ls -la"}"#),
    ]
    .into_iter()
    .collect();
    let map = env_payload_from("tool_call_before", None, |k| env.get(k).map(OsString::from));
    assert_eq!(map["event"], json!("tool_call_before"));
    assert_eq!(map["cwd"], json!("/repo"));
    assert_eq!(map["tool"], json!("exec_shell"));
    assert_eq!(map["tool_args"], json!(r#"{"command":"ls -la"}"#));
}

/// env-mode's envelope still carries `_pid` — via the ONE stamping site, which
/// is what keeps a Windows ancestor walk from running twice per hook. Composed
/// rather than asserted on `env_payload_from` alone: that fn stamping its own
/// pid is exactly the duplicate this pins against.
#[test]
fn env_mode_gets_its_pid_from_the_single_enrich_site() {
    let mut map = env_payload_from("tool_call_before", None, |_| None);
    assert!(!map.contains_key("_pid"), "not stamped at envelope build");
    enrich_payload(&mut map, None, None, 0, || Some(4321));
    assert_eq!(map["_pid"], json!(4321), "stamped once, by enrich_payload");
}

#[test]
fn env_payload_omits_missing_and_empty_env() {
    let env: std::collections::HashMap<&str, &str> =
        [("DEEPSEEK_WORKSPACE", "/repo"), ("DEEPSEEK_TOOL_NAME", "")]
            .into_iter()
            .collect();
    let map = env_payload_from("session_start", None, |k| env.get(k).map(OsString::from));
    assert_eq!(map["cwd"], json!("/repo"));
    assert!(
        !map.contains_key("tool"),
        "empty DEEPSEEK_TOOL_NAME must be omitted"
    );
    assert!(
        !map.contains_key("tool_args"),
        "absent tool_args must be omitted"
    );
    assert!(
        !map.contains_key("_pid"),
        "enrich_payload alone stamps _pid"
    );
    assert_eq!(map.len(), 2, "exactly event + cwd");
}

#[test]
fn env_payload_caps_oversized_fields_at_a_char_boundary() {
    // Multi-byte fixture: a byte-slice cap would split a UTF-8 scalar.
    let huge = "é".repeat(ENV_FIELD_CAP); // ~2·CAP bytes, well over the cap
    let env: std::collections::HashMap<&str, String> = [
        ("DEEPSEEK_WORKSPACE", "/repo".to_string()),
        ("DEEPSEEK_TOOL_ARGS", huge),
    ]
    .into_iter()
    .collect();
    let map = env_payload_from("tool_call_before", None, |k| env.get(k).map(OsString::from));
    let args = map["tool_args"].as_str().unwrap();
    assert!(
        args.len() <= ENV_FIELD_CAP,
        "tool_args must be capped to <= {ENV_FIELD_CAP} bytes, got {}",
        args.len()
    );
    assert!(
        args.len() > ENV_FIELD_CAP - 4,
        "cap should truncate NEAR the limit (last char boundary), not collapse"
    );
    assert!(
        args.chars().all(|c| c == 'é'),
        "no mid-scalar split → still valid é runs"
    );
    assert_eq!(
        map["cwd"],
        json!("/repo"),
        "the AgentId key (a real path) is untouched"
    );
}

#[test]
fn cap_never_exceeds_the_bound_when_a_multibyte_scalar_straddles_it() {
    // A 4-byte scalar STARTING inside the cap but ENDING past it. The é fixture
    // above can't catch this: 2-byte chars over an even cap always end exactly
    // ON a boundary.
    let mut val = "a".repeat(ENV_FIELD_CAP - 1);
    val.push('\u{1D11E}');
    let capped = cap_env_field(val);
    assert!(
        capped.len() <= ENV_FIELD_CAP,
        "cap is a hard byte ceiling, got {} > {ENV_FIELD_CAP}",
        capped.len()
    );
    assert_eq!(
        capped.len(),
        ENV_FIELD_CAP - 1,
        "floor to the last char boundary at or below the cap"
    );
    assert!(capped.chars().all(|c| c == 'a'), "the straddler is dropped");
}

#[test]
fn env_payload_falls_back_to_cwd_when_workspace_unset() {
    // A fresh `codewhale` without `-C` has no DEEPSEEK_WORKSPACE at
    // session_start.
    let no_ws: std::collections::HashMap<&str, String> =
        [("DEEPSEEK_TOOL_NAME", "exec_shell".to_string())]
            .into_iter()
            .collect();
    let map = env_payload_from("session_start", Some("/proj/here".to_string()), |k| {
        no_ws.get(k).map(OsString::from)
    });
    assert_eq!(
        map["cwd"],
        json!("/proj/here"),
        "cwd must fall back to the hook child's working dir when DEEPSEEK_WORKSPACE is unset"
    );

    let ws: std::collections::HashMap<&str, String> = [("DEEPSEEK_WORKSPACE", "/ws".to_string())]
        .into_iter()
        .collect();
    let map = env_payload_from("session_start", Some("/proj/here".to_string()), |k| {
        ws.get(k).map(OsString::from)
    });
    assert_eq!(
        map["cwd"],
        json!("/ws"),
        "DEEPSEEK_WORKSPACE wins over the fallback"
    );

    let map = env_payload_from("session_start", None, |_| None);
    assert!(
        !map.contains_key("cwd"),
        "no workspace and no cwd fallback → no cwd field"
    );
}

#[cfg(unix)]
#[test]
fn default_socket_path_branches() {
    temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/0"), || {
        temp_env::with_var("PIXTUOID_SOCKET", Some("/explicit/path.sock"), || {
            assert_eq!(
                default_socket_path(),
                std::path::Path::new("/explicit/path.sock")
            );
        });

        // Set-but-empty/whitespace = unset (the #172 RUST_LOG policy).
        for blank in ["", "   "] {
            temp_env::with_var("PIXTUOID_SOCKET", Some(blank), || {
                assert_eq!(
                    default_socket_path(),
                    std::path::Path::new("/run/user/0/pixtuoid.sock")
                );
            });
        }
    });

    temp_env::with_var_unset("PIXTUOID_SOCKET", || {
        temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/1000"), || {
            assert_eq!(
                default_socket_path(),
                std::path::Path::new("/run/user/1000/pixtuoid.sock")
            );
        });

        // A relative XDG_RUNTIME_DIR counts as unset per the XDG absolute-only spec.
        // Safety: getuid is always safe on Unix.
        let uid = unsafe { libc::getuid() };
        let tmp_fallback = format!("/tmp/pixtuoid-{uid}/pixtuoid.sock");
        for invalid in ["", "   ", "relative/run"] {
            temp_env::with_var("XDG_RUNTIME_DIR", Some(invalid), || {
                assert_eq!(default_socket_path(), tmp_fallback);
            });
        }

        // #485: the per-user 0700 SUBDIR, not a flat squattable name.
        temp_env::with_var_unset("XDG_RUNTIME_DIR", || {
            assert_eq!(
                default_socket_path(),
                format!("/tmp/pixtuoid-{uid}/pixtuoid.sock")
            );
        });
    });
}

#[cfg(unix)]
#[test]
fn owned_tmp_socket_dir_matches_only_the_tmp_fallback() {
    // Safety: getuid is always safe on Unix.
    let uid = unsafe { libc::getuid() };
    let owned = std::path::PathBuf::from(format!("/tmp/pixtuoid-{uid}"));
    assert_eq!(
        paths::owned_tmp_socket_dir(std::path::Path::new(&format!(
            "/tmp/pixtuoid-{uid}/pixtuoid.sock"
        ))),
        Some(owned),
        "the /tmp fallback endpoint resolves its owned dir"
    );
    assert_eq!(
        paths::owned_tmp_socket_dir(std::path::Path::new("/run/user/1000/pixtuoid.sock")),
        None,
        "XDG_RUNTIME_DIR is systemd's, not ours to police"
    );
    assert_eq!(
        paths::owned_tmp_socket_dir(std::path::Path::new("/explicit/path.sock")),
        None,
        "an explicit PIXTUOID_SOCKET override is the user's, not ours"
    );
    // A different uid's tmp dir is not ours either.
    assert_eq!(
        paths::owned_tmp_socket_dir(std::path::Path::new(&format!(
            "/tmp/pixtuoid-{}/pixtuoid.sock",
            uid + 1
        ))),
        None
    );
}

#[cfg(windows)]
#[test]
fn default_socket_path_branches_windows() {
    temp_env::with_var("PIXTUOID_SOCKET", Some(r"\\.\pipe\explicit"), || {
        assert_eq!(
            default_socket_path(),
            std::path::Path::new(r"\\.\pipe\explicit")
        );
    });

    temp_env::with_var("USERNAME", Some("ada"), || {
        // Set-but-empty/whitespace = unset (the #172 RUST_LOG policy).
        for blank in ["", "   "] {
            temp_env::with_var("PIXTUOID_SOCKET", Some(blank), || {
                assert_eq!(
                    default_socket_path(),
                    std::path::Path::new(r"\\.\pipe\pixtuoid-ada")
                );
            });
        }
    });

    temp_env::with_var_unset("PIXTUOID_SOCKET", || {
        temp_env::with_var("USERNAME", Some("ada"), || {
            assert_eq!(
                default_socket_path(),
                std::path::Path::new(r"\\.\pipe\pixtuoid-ada")
            );
        });

        // DOMAIN\user form is sanitized (backslashes are illegal in pipe names).
        temp_env::with_var("USERNAME", Some(r"CORP\alice"), || {
            assert_eq!(
                default_socket_path(),
                std::path::Path::new(r"\\.\pipe\pixtuoid-CORP-alice")
            );
        });

        temp_env::with_var_unset("USERNAME", || {
            assert_eq!(
                default_socket_path(),
                std::path::Path::new(r"\\.\pipe\pixtuoid-default")
            );
        });
    });
}
