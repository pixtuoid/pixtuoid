//! What each plugin must register and send, written from the Rust authorities
//! into the `plugin-contract.json` its selftest executes it against
//! (`scripts/lib/plugin-harness.mjs`), so neither language reads the other's
//! source text, except dsh's `registers`, which `dsh::plugin_events` scans from
//! its template. `SNAPSHOTS=overwrite just test -p pixtuoid --lib plugin_contract`
//! rewrites it.

use serde_json::{Value, json};

use crate::install::target::{DSH, HERMES, OMP, OPENCLAW, OPENCODE, Target};

/// One plugin's row: its template, the host events it registers, and those it
/// consumes itself and never forwards.
fn row(target: &Target, file: &str) -> Value {
    json!({
        "source": target.core_source,
        "file": file,
        "registers": (target.registers)(),
        "consumes": target.consumes,
    })
}

#[test]
fn the_plugin_contract_is_the_committed_file() {
    let mut opencode = row(&OPENCODE, "opencode_plugin.ts");
    opencode["data_fields"] = json!(pixtuoid_core::source::opencode::DECODED_DATA_FIELDS);
    opencode["turn_key"] = json!(pixtuoid_core::source::opencode::TURN);
    let mut hermes = row(&HERMES, "hermes_plugin.py");
    hermes["fields"] = json!(pixtuoid_core::source::hermes::DECODED_FIELDS);
    hermes["detail_keys"] = json!(pixtuoid_core::source::hermes::DETAIL_KEYS);
    hermes["outside_a_turn"] = json!(pixtuoid_core::source::hermes::OUTSIDE_A_TURN);
    let contract = json!({
        "pid_key": pixtuoid_core::source::decoder::PID_STAMP,
        "source_flag": super::hook_cmd::SOURCE_FLAG.trim(),
        "hook_placeholder": super::merge::HOOK_PLACEHOLDER,
        "plugins": [
            row(&DSH, "dsh_plugin.mjs"),
            hermes,
            row(&OMP, "omp_extension.ts"),
            row(&OPENCLAW, "openclaw_plugin.js"),
            opencode,
        ],
    });
    let generated = serde_json::to_string_pretty(&contract).unwrap() + "\n";
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/install/plugin-contract.json");
    let committed = snapbox::Data::read_from(&path, Some(snapbox::data::DataFormat::Text));
    snapbox::assert_data_eq!(generated, committed.raw());
}
