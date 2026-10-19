//! The NAME-KEYED delegation family: sources whose subagent dispatch is a
//! tool literally called `task` — `subagent` for dsh and opencode. They claim the Task detail by tool NAME and
//! deliberately NOT by the presence of a `subagent_type` key, so a
//! model-authored argument on an ordinary tool cannot spoof a delegation, seed
//! `active_tasks`, and cascade a real child out on drain.

use std::path::PathBuf;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::{AgentEvent, ToolDetail};

fn lines(cli: &str, name: &str) -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("delegation/fixtures")
            .join(cli)
            .join(name),
    )
}

fn copilot_transcript() -> PathBuf {
    super::captures::transcript_in(
        &super::captures::sources_root().join("delegation/fixtures/copilot"),
    )
}

fn copilot_events() -> Vec<AgentEvent> {
    let transcript = copilot_transcript();
    let d = Drive::transcript_at("copilot", &transcript)
        .expect("copilot has a line decoder")
        .lines(super::captures::fixture_lines(&transcript));
    d.assert_clean("copilot delegation transcript");
    d.events
}

/// The parent's: omp writes each child inside a same-named DIR beside it.
fn omp_transcript() -> PathBuf {
    let dir = super::captures::sources_root().join("delegation/fixtures/omp");
    let mut parents: Vec<_> = super::captures::transcripts_in(&dir)
        .into_iter()
        .filter(|t| t.parent() == Some(dir.as_path()))
        .collect();
    assert_eq!(parents.len(), 1, "one parent transcript");
    parents.remove(0)
}

fn omp_events() -> Vec<AgentEvent> {
    let transcript = omp_transcript();
    let d = Drive::transcript_at("omp", &transcript)
        .expect("omp has a line decoder")
        .lines(super::captures::fixture_lines(&transcript));
    d.assert_clean("omp delegation transcript");
    d.events
}

fn dsh_events() -> Vec<AgentEvent> {
    let d = Drive::hooks().lines(lines("dsh", "hook-payloads.jsonl"));
    d.assert_clean("dsh delegation hooks");
    d.events
}

/// Its hermetic profile records it under opencode's own module.
fn opencode_lines() -> Vec<String> {
    super::captures::fixture_lines(
        &super::captures::sources_root()
            .join("opencode/fixtures/subagent-hermetic/hook-payloads.jsonl"),
    )
}

fn opencode_events() -> Vec<AgentEvent> {
    let d = Drive::hooks().lines(opencode_lines());
    d.assert_clean("opencode delegation hooks");
    d.events
}

fn tasks(evs: &[AgentEvent]) -> usize {
    evs.iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ActivityStart {
                    detail: Some(ToolDetail::Task),
                    ..
                }
            )
        })
        .count()
}

#[test]
fn every_name_keyed_source_mints_exactly_one_task_from_its_capture() {
    for (cli, evs) in [
        ("copilot", copilot_events()),
        ("omp", omp_events()),
        ("dsh", dsh_events()),
        ("opencode", opencode_events()),
    ] {
        assert_eq!(
            tasks(&evs),
            1,
            "{cli}: the one `task` call in the capture must be the one Task detail"
        );
    }
}

#[test]
fn the_child_is_in_band_for_some_of_them_and_a_separate_file_for_others() {
    // Not cosmetic: where the child is ANNOUNCED in the parent's own stream, the
    // capture is two sprites and cannot sit under the conformance one-AgentId
    // rule. omp announces nothing — its child is a sibling transcript the
    // watcher discovers — so its parent capture is a single agent.
    for (cli, evs, in_band) in [
        ("copilot", copilot_events(), true),
        ("omp", omp_events(), false),
        ("dsh", dsh_events(), true),
        ("opencode", opencode_events(), true),
    ] {
        let ids: std::collections::BTreeSet<_> = evs.iter().map(AgentEvent::agent_id).collect();
        assert_eq!(
            ids.len() >= 2,
            in_band,
            "{cli}: in-band child announcement is {in_band}, got {} agent(s)",
            ids.len()
        );
    }
}

/// The call id sitting beside the tool named by `dispatch` in the SAME raw
/// object.
fn task_call_ids(raw: &[String], dispatch: &str) -> std::collections::BTreeSet<String> {
    fn walk(v: &serde_json::Value, dispatch: &str, out: &mut std::collections::BTreeSet<String>) {
        match v {
            serde_json::Value::Object(o) => {
                let names = ["tool", "toolName", "name"];
                let ids = ["toolCallId", "callID", "callId", "call_id", "id"];
                if names
                    .iter()
                    .any(|k| o.get(*k).and_then(|n| n.as_str()) == Some(dispatch))
                    && let Some(id) = ids.iter().find_map(|k| o.get(*k).and_then(|i| i.as_str()))
                {
                    out.insert(id.to_string());
                }
                o.values().for_each(|c| walk(c, dispatch, out));
            }
            serde_json::Value::Array(a) => a.iter().for_each(|c| walk(c, dispatch, out)),
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    for l in raw {
        walk(
            &serde_json::from_str(l).expect("valid capture json"),
            dispatch,
            &mut out,
        );
    }
    out
}

#[test]
fn the_task_detail_lands_on_the_call_actually_named_task() {
    // It has to BIND: counting one Task and one ordinary tool passes just as
    // well when the decoder tags the bash call and leaves the dispatch bare.
    for (cli, evs, raw, name) in [
        (
            "copilot",
            copilot_events(),
            super::captures::fixture_lines(&copilot_transcript()),
            "task",
        ),
        (
            "omp",
            omp_events(),
            super::captures::fixture_lines(&omp_transcript()),
            "task",
        ),
        (
            "dsh",
            dsh_events(),
            lines("dsh", "hook-payloads.jsonl"),
            "subagent",
        ),
        ("opencode", opencode_events(), opencode_lines(), "subagent"),
    ] {
        let dispatch = task_call_ids(&raw, name);
        assert!(
            !dispatch.is_empty(),
            "{cli}: the capture must name a `{name}` call, else this rule is untested"
        );
        let starts: Vec<(&Option<String>, bool)> = evs
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ActivityStart {
                    tool_use_id,
                    detail,
                    ..
                } => Some((tool_use_id, matches!(detail, Some(ToolDetail::Task)))),
                _ => None,
            })
            .collect();
        assert!(
            starts.len() > tasks(&evs),
            "{cli}: the capture must also hold ordinary tools, else the rule is untested"
        );
        for (id, is_task) in &starts {
            let named_task = id.as_deref().is_some_and(|i| dispatch.contains(i));
            assert_eq!(
                *is_task, named_task,
                "{cli}: the Task detail must sit on the dispatch call ({dispatch:?}) and \
                 on no other — start {id:?} claims Task={is_task}"
            );
        }
    }
}
