//! Jcode hook-only source. The shim forwards selected environment fields, never tool inputs.

use crate::AgentId;
use crate::source::decoder::{DecodeError, DecodeResult, display_safe, label_prefix_for};
use crate::source::{AgentEvent, ToolDetail, drift};
use serde_json::Value;

/// The Jcode CLI source's registry name.
pub const SOURCE_NAME: &str = "jcode";

pub(crate) fn decode_jcode_hook_payload(v: &Value) -> DecodeResult<Vec<AgentEvent>> {
    let obj = v
        .as_object()
        .ok_or_else(|| DecodeError::not_an_object(SOURCE_NAME))?;
    let required = |field| {
        obj.get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                drift::missing_field(SOURCE_NAME, "hook", field);
                DecodeError::missing(SOURCE_NAME, field)
            })
    };
    let event = required("event")?;
    let key = required("session_id")?;
    let agent_id = AgentId::from_parts(SOURCE_NAME, key);
    let cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let mut events = match event {
        "session_start" | "turn_start" => vec![AgentEvent::SessionStart {
            agent_id,
            source: SOURCE_NAME.into(),
            session_id: key.into(),
            cwd: cwd.unwrap_or_default(),
            parent_id: None,
        }],
        // ActivityEnd is proof-of-life in the shared reducer too. Identity honors
        // its tombstones while avoiding a nameless slot when the office joins late.
        "pre_tool" | "post_tool" | "turn_end" => {
            vec![AgentEvent::identity(agent_id, SOURCE_NAME, key, cwd)]
        }
        "session_end" => {
            return Ok(vec![AgentEvent::SessionEnd {
                agent_id,
                as_child: false,
            }]);
        }
        other => {
            drift::unknown_event(SOURCE_NAME, other);
            return Err(DecodeError::unsupported(SOURCE_NAME, other));
        }
    };
    events.push(AgentEvent::Rename {
        agent_id,
        label: session_label(key),
    });
    if let Some(model) = obj
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        events.push(AgentEvent::ModelInfo {
            agent_id,
            model: Some(display_safe(model)),
            effort: None,
        });
    }
    match event {
        "turn_start" | "pre_tool" => events.push(AgentEvent::ActivityStart {
            agent_id,
            // pre_tool has no call ID; post_tool's ID cannot pair with it.
            tool_use_id: None,
            detail: if event == "pre_tool" {
                Some(ToolDetail::Generic {
                    display: display_safe(required("tool_name")?),
                })
            } else {
                None
            },
        }),
        "post_tool" | "turn_end" => events.push(AgentEvent::ActivityEnd {
            agent_id,
            tool_use_id: None,
        }),
        _ => {}
    }
    Ok(events)
}

fn session_label(id: &str) -> String {
    let name = id
        .strip_prefix("session_")
        .and_then(|rest| rest.split_once('_'))
        .map_or(id, |(name, _)| name);
    display_safe(&format!("{}·{name}", label_prefix_for(SOURCE_NAME)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{ToolDetail, Transport, registry};
    use crate::{AgentId, Reducer, SceneState};
    use serde_json::json;
    use std::time::SystemTime;

    fn payload(event: &str, id: &str) -> Value {
        json!({"_pixtuoid_source":"jcode", "event":event, "session_id":id, "cwd":"/repo", "model":"gpt-test", "tool_name":"bash"})
    }

    fn apply(scene: &mut SceneState, reducer: &mut Reducer, event: &str, id: &str) {
        for decoded in decode_jcode_hook_payload(&payload(event, id)).unwrap() {
            reducer.apply(scene, decoded, SystemTime::UNIX_EPOCH, Transport::Hook);
        }
    }

    #[test]
    fn registration_and_backfill_carry_names_and_distinct_identity() {
        let mut scene = SceneState::uniform(4);
        let mut reducer = Reducer::new();
        for (event, id, label) in [
            ("session_start", "session_chipmunk_123_a", "jc·chipmunk"),
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
        assert_eq!(
            registry::descriptor_for(SOURCE_NAME).unwrap().label_prefix,
            "jc"
        );
    }

    #[test]
    fn starts_activity_without_inventing_tool_pairing_or_arguments() {
        let mut input = payload("pre_tool", "session_fox_123_a");
        input["tool_use_id"] = json!("post-only-id");
        input["_pid"] = json!(42);
        input["tool_input"] = json!({"command":"private"});
        let events = decode_jcode_hook_payload(&input).unwrap();
        assert!(matches!(events[0], AgentEvent::Identity { pid: None, .. }));
        assert!(events.iter().any(|event| matches!(event,
            AgentEvent::ActivityStart { tool_use_id: None, detail: Some(ToolDetail::Generic {display}), .. } if display == "bash")));
        assert!(!format!("{events:?}").contains("private"));
        let turn = decode_jcode_hook_payload(&payload("turn_start", "session_fox_123_a")).unwrap();
        assert!(matches!(
            turn[0],
            AgentEvent::SessionStart {
                parent_id: None,
                ..
            }
        ));
        assert!(turn.iter().any(|event| matches!(
            event,
            AgentEvent::ActivityStart {
                tool_use_id: None,
                detail: None,
                ..
            }
        )));
    }

    #[test]
    fn terminal_events_respect_dead_sessions_and_backfill_live_identity() {
        let events =
            decode_jcode_hook_payload(&payload("session_end", "session_fox_123_a")).unwrap();
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
        for value in [
            Value::Null,
            json!({}),
            json!({"event":"pre_tool"}),
            json!({"event":"pre_tool", "session_id":""}),
            json!({"event":"pre_tool", "session_id":"session_fox_123_a"}),
            payload("unregistered", "session_fox_123_a"),
        ] {
            assert!(decode_jcode_hook_payload(&value).is_err(), "{value}");
        }
    }

    #[test]
    fn registered_events_use_the_registered_decoder_and_optional_model() {
        for event in [
            "session_start",
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
                !decode_jcode_hook_payload(&input)
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
            let events = decode_jcode_hook_payload(&payload("session_start", id)).unwrap();
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
        for event in decode_jcode_hook_payload(&input).unwrap() {
            let display = match event {
                AgentEvent::Rename { label, .. } => label,
                AgentEvent::ModelInfo {
                    model: Some(model), ..
                } => model,
                AgentEvent::ActivityStart {
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
