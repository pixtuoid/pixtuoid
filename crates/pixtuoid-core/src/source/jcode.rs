//! Jcode hook-only source. The shim forwards selected environment fields, never tool inputs.
//!
//! Cites are `1jehuang/jcode` at `v1.0.3`.

use std::path::PathBuf;

use crate::AgentId;
use crate::source::decoder::Hook;
use crate::source::decoder::{
    DecodeResult, display_safe, label_prefix_for, non_empty, required_or_err,
};
use crate::source::registry::{
    ChildEndInference, Envelope, FocusChannel, HookDecoding, Hyphen, SessionField, SourceCaps,
    SourceDescriptor, SourceKind,
};
use crate::source::{AgentEvent, ToolDetail};

/// The Jcode CLI source's registry name.
pub const SOURCE_NAME: &str = "jcode";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    display_name: "Jcode",
    label_prefix: "jc",
    verified_version: "1.0.3",
    version_probe: Some(&["jcode", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: None,
    kind: SourceKind::Agent {
        transcript: None,
        hook: Some(HookDecoding::ClaimsAll {
            envelope: ENVELOPE,
            decode: decode_jcode_hook_payload,
        }),
        caps: SourceCaps {
            // `session_end` fires on a normal close alone (`docs/HOOKS.md:102-103`):
            // a crash marks the session crashed and fires none
            // (`crates/jcode-app-core/src/agent.rs:1238-1255`, `:1274-1287`), and no
            // pid channel watches the shared server.
            exits_can_go_unseen: true,
            // `turn_start` re-emits SessionStart, but a `jcode run --json` turn
            // fires none: `run_once_capture` skips the hook `run_once` fires
            // (`crates/jcode-app-core/src/agent/turn_execution.rs:69-71`, `:84-113`;
            // `src/cli/commands.rs:2655-2656`).
            resurrects_on_prompt: false,
            // It mints no `Task`, so no slot ever delegates.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::None,
            child_hooks_unnamed: false,
            // Its hooks name no turn (`crates/jcode-app-core/src/agent/turn_execution.rs:391-394`,
            // `:413-417`), and each observer is a detached process
            // (`crates/jcode-base/src/hooks.rs:520-554`), so no ordered stream.
            brackets_turns: false,
        },
        // A shared server owns many sessions (`crates/jcode-base/src/hooks.rs:214-217`),
        // so its pid names none of them.
        focus: FocusChannel::Unsupported,
    },
    #[cfg(feature = "native")]
    native: crate::source::registry::Native::NONE,
};

/// Where the payload the shim builds from jcode's hook environment names its event and session.
const ENVELOPE: Envelope = Envelope {
    event_field: "event",
    session_field: SessionField {
        within: None,
        field: "session_id",
    },
};

pub(crate) fn decode_jcode_hook_payload(hook: &Hook<'_>) -> DecodeResult<Option<Vec<AgentEvent>>> {
    let (obj, event) = (hook.obj, hook.event);
    let key = hook.session()?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd = non_empty(obj, "cwd").map(PathBuf::from);
    // The Identity is the one claim that registers a session the office joined
    // late: the activity alone registers nothing. It honors its tombstones.
    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, key, cwd.clone());
    let (claim, activity) = match event {
        // Its registration, and the resurrect path: a swept session walks back in.
        "turn_start" => (
            AgentEvent::SessionStart {
                agent_id,
                source: SOURCE_NAME.into(),
                session_id: key.into(),
                cwd: cwd.unwrap_or_default(),
                parent_id: None,
            },
            None,
        ),
        "pre_tool" => (
            identity(),
            Some(AgentEvent::ToolUseStart {
                agent_id,
                // pre_tool names no call (`crates/jcode-base/src/hooks.rs:579-585`),
                // so no post_tool can pair with it.
                tool_use_id: None,
                detail: Some(ToolDetail::Generic {
                    display: display_safe(required_or_err(
                        SOURCE_NAME,
                        Some(event),
                        "tool_name",
                        |k| non_empty(obj, k),
                    )?),
                }),
                copy_id: None,
            }),
        ),
        "post_tool" => (
            identity(),
            Some(AgentEvent::ToolUseEnd {
                agent_id,
                tool_use_id: None,
            }),
        ),
        "turn_end" => (identity(), Some(AgentEvent::turn_end(agent_id))),
        "session_end" => {
            return Ok(Some(vec![AgentEvent::SessionEnd {
                agent_id,
                as_child: false,
            }]));
        }
        _ => return Ok(None),
    };
    let mut events = vec![
        claim,
        AgentEvent::Rename {
            agent_id,
            label: session_label(key),
        },
    ];
    let model = non_empty(obj, "model").map(display_safe);
    events.extend(AgentEvent::model_info(agent_id, model.as_deref(), None));
    events.extend(activity);
    Ok(Some(events))
}

/// The memorable word of a `session_<word>_<ms>_<hex>` id
/// (`crates/jcode-core/src/id.rs:270`), else the whole id.
fn session_label(id: &str) -> String {
    let name = id
        .strip_prefix("session_")
        .and_then(|rest| rest.split_once('_'))
        .map_or(id, |(name, _)| name);
    display_safe(&crate::state::SlotLabel::join(
        label_prefix_for(SOURCE_NAME),
        name,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::DecodeError;
    use crate::source::{ToolDetail, Transport};
    use crate::{AgentId, Reducer, SceneState};
    use serde_json::{Value, json};
    use std::time::SystemTime;

    fn decode(v: &Value) -> DecodeResult<Vec<AgentEvent>> {
        crate::source::decoder::test_decode(SOURCE_NAME, v)
    }

    fn payload(event: &str, id: &str) -> Value {
        json!({"_pixtuoid_source":"jcode", "event":event, "session_id":id, "cwd":"/repo", "model":"gpt-test", "tool_name":"bash"})
    }

    fn apply(scene: &mut SceneState, reducer: &mut Reducer, event: &str, id: &str) {
        for decoded in decode(&payload(event, id)).unwrap() {
            reducer.apply(scene, decoded, SystemTime::UNIX_EPOCH, Transport::Hook);
        }
    }

    #[test]
    fn registration_and_backfill_carry_names_and_distinct_identity() {
        let mut scene = SceneState::uniform(4);
        let mut reducer = Reducer::new();
        for (event, id, label) in [
            ("turn_start", "session_chipmunk_123_a", "jc·chipmunk"),
            ("pre_tool", "session_spider_123_b", "jc·spider"),
            ("turn_start", "session_t-rex_123_c", "jc·t-rex"),
            ("turn_start", "session_chipmunk_123_d", "jc·chipmunk"),
        ] {
            apply(&mut scene, &mut reducer, event, id);
            let slot = &scene.agents[&AgentId::from_parts(SOURCE_NAME, id)];
            assert_eq!(&*slot.label, label);
            assert_eq!(slot.session_id.as_ref(), id);
            assert_eq!(slot.source.as_ref(), SOURCE_NAME);
            assert!(slot.pid.is_none());
        }
        assert_eq!(scene.agents.len(), 4);
    }

    #[test]
    fn starts_activity_without_inventing_tool_pairing_or_arguments() {
        let mut input = payload("pre_tool", "session_fox_123_a");
        input["tool_use_id"] = json!("post-only-id");
        input["tool_input"] = json!({"command":"private"});
        let events = decode(&input).unwrap();
        assert!(matches!(events[0], AgentEvent::Identity { .. }));
        assert!(events.iter().any(|event| matches!(event,
            AgentEvent::ToolUseStart { tool_use_id: None, detail: Some(ToolDetail::Generic {display}), .. } if display == "bash")));
        assert!(!format!("{events:?}").contains("private"));
        let turn = decode(&payload("turn_start", "session_fox_123_a")).unwrap();
        assert!(matches!(
            turn[0],
            AgentEvent::SessionStart {
                parent_id: None,
                ..
            }
        ));
        assert!(
            !turn
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolUseStart { .. }))
        );
    }

    #[test]
    fn terminal_events_respect_dead_sessions_and_backfill_live_identity() {
        let events = decode(&payload("session_end", "session_fox_123_a")).unwrap();
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::SessionEnd {
                as_child: false,
                ..
            }]
        ));
        for event in ["post_tool", "turn_end"] {
            let mut scene = SceneState::uniform(4);
            let mut reducer = Reducer::new();
            apply(&mut scene, &mut reducer, event, "session_fox_123_a");
            let slot = &scene.agents[&AgentId::from_parts(SOURCE_NAME, "session_fox_123_a")];
            assert_eq!(&*slot.label, "jc·fox");
        }
        let mut scene = SceneState::uniform(4);
        let mut reducer = Reducer::new();
        let id = "session_fox_123_a";
        apply(&mut scene, &mut reducer, "session_end", id);
        apply(&mut scene, &mut reducer, "post_tool", id);
        apply(&mut scene, &mut reducer, "turn_end", id);
        assert!(scene.agents.is_empty());
        apply(&mut scene, &mut reducer, "turn_start", id);
        assert_eq!(scene.agents.len(), 1);
        apply(&mut scene, &mut reducer, "turn_start", id);
        assert_eq!(scene.agents.len(), 1);
        apply(&mut scene, &mut reducer, "session_end", id);
        let agent_id = AgentId::from_parts(SOURCE_NAME, id);
        let ended = scene.agents[&agent_id].exiting_at;
        assert!(ended.is_some());
        for event in ["post_tool", "turn_end"] {
            apply(&mut scene, &mut reducer, event, id);
            assert_eq!(scene.agents[&agent_id].exiting_at, ended);
        }
    }

    #[test]
    fn unknown_and_missing_contract_fields_fail_closed() {
        use crate::source::drift::{DriftKind, breadcrumbed};
        for value in [json!({}), payload("unregistered", "session_fox_123_a")] {
            assert!(decode(&value).is_err(), "{value}");
        }
        let event = "pre_tool";
        for (value, missing) in [
            (json!({"event":event}), "session_id"),
            (json!({"event":event, "session_id":""}), "session_id"),
            (
                json!({"event":event, "session_id":"session_fox_123_a"}),
                "tool_name",
            ),
        ] {
            let mut err = None;
            let logs = crate::test_capture::capture_logs(|| err = decode(&value).err());
            assert!(
                matches!(
                    &err,
                    Some(DecodeError::Missing { context: Some(c), field, .. })
                        if c == event && *field == missing
                ),
                "{value}: {err:?}"
            );
            let fields = [("event", event), ("field", missing)];
            assert!(
                breadcrumbed(&logs, SOURCE_NAME, DriftKind::MissingField, &fields),
                "{fields:?} in\n{logs}"
            );
        }
    }

    #[test]
    fn registered_events_use_the_registered_decoder_and_optional_model() {
        for event in [
            "session_end",
            "turn_start",
            "turn_end",
            "pre_tool",
            "post_tool",
        ] {
            let mut input = payload(event, "session_fox_123_a");
            let decoded = crate::source::decoder::decode_hook_payload(input.clone()).unwrap();
            if event != "session_end" {
                assert!(decoded.iter().any(|event| matches!(event, AgentEvent::ModelInfo {model: Some(model), effort: None, ..} if model == "gpt-test")));
            }
            input.as_object_mut().unwrap().remove("model");
            assert!(
                !decode(&input)
                    .unwrap()
                    .iter()
                    .any(|event| matches!(event, AgentEvent::ModelInfo { .. }))
            );
        }
    }

    #[test]
    fn unknown_names_and_malformed_ids_keep_a_safe_bounded_label() {
        for (id, expected) in [
            ("session_blue-whale_123_random", "jc·blue-whale"),
            ("session_bee_123_random", "jc·bee"),
            ("imported-session", "jc·imported-session"),
            ("session_fox\u{202e}\n_123_random", "jc·fox"),
        ] {
            let events = decode(&payload("turn_start", id)).unwrap();
            assert!(
                events.iter().any(
                    |event| matches!(event, AgentEvent::Rename {label, ..} if label == expected)
                )
            );
        }
        let mut input = payload(
            "pre_tool",
            &format!("session_{}_123_random", "x".repeat(1000)),
        );
        input["tool_name"] = json!(format!("bash\n\u{202e}{}", "x".repeat(1000)));
        input["model"] = json!(format!("gpt\n\u{202e}{}", "x".repeat(1000)));
        for event in decode(&input).unwrap() {
            let display = match event {
                AgentEvent::Rename { label, .. } => label,
                AgentEvent::ModelInfo {
                    model: Some(model), ..
                } => model,
                AgentEvent::ToolUseStart {
                    detail: Some(ToolDetail::Generic { display }),
                    ..
                } => display,
                _ => continue,
            };
            assert!(!display.contains(['\n', '\u{202e}']));
            assert!(display.chars().count() <= crate::source::decoder::MAX_DECODED_FIELD_CHARS + 1);
        }
    }
}
