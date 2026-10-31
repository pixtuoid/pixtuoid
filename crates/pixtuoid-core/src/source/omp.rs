//! Oh My Pi (`omp`, omp.sh) source. HYBRID: the durable authority is the
//! session transcript (`<sessions-root>/<encoded-cwd>/<ts>_<uuid>.jsonl`), read
//! by one `JsonlWatcher` per root: `omp_sessions_dir` owns every axis of the
//! primary root, `omp_profile_sessions_dirs` adds every other profile's.
//! Subagents persist as SEPARATE nested files (`<parent-stem>/<taskId>.jsonl`),
//! parent-linked by path. omp has no shell-hook seam — its hooks are in-process
//! TS extension modules — so the hook plane is a pixtuoid-owned bridge
//! extension (`pixtuoid/src/install/omp_extension.ts`, auto-discovered from the
//! agent dir's `extensions/`) whose payloads `decode_omp_hook_payload` claims
//! (#951). It forwards what the transcript can never carry: pre-persist
//! presence, empty-session shutdown, the approval wait, and the process's own
//! `_pid`. Both transports key on the sessionFile stem chain
//! ([`omp_id_from_path`]), so they mint ONE AgentId per session, as grok's do;
//! under `omp --no-extensions`, or with the bridge broken, the transcript alone
//! carries the session. Wire shape: upstream `packages/coding-agent/src/session/`
//! (transcript) and `src/extensibility/` (extension events).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::source::decoder::DecodeResult as Result;
use serde_json::Value;

use crate::AgentId;
use crate::source::decoder::{
    MAX_DECODED_FIELD_CHARS, accept_all_paths, ellipsize, extract_top_level_cwd,
};
use crate::source::registry::{
    ChildEndInference, FocusChannel, HookDecoding, HookObject, Hyphen, SourceCaps,
    SourceDescriptor, SourceKind, Transcript,
};
use crate::source::{AgentEvent, RequestKeys, ToolDetail};

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::OmpSource;

/// The Oh My Pi (omp) source's registry name (its `SourceDescriptor.name`).
pub const SOURCE_NAME: &str = "omp";

pub(crate) const DESCRIPTOR: SourceDescriptor = SourceDescriptor {
    name: SOURCE_NAME,
    label_prefix: "om",
    verified_version: "18.14.1",
    version_probe: Some(&["omp", "--version"]),
    version_hyphen: Hyphen::Prerelease,
    home_env: Some("PI_CODING_AGENT_DIR"),
    kind: SourceKind::Agent {
        transcript: Some(Transcript {
            line_decoder: decode_omp_line,
            // The nested stem CHAIN, so a subagent task id can't collide across
            // sessions.
            id_from_path: omp_id_from_path,
            path_filter: accept_all_paths,
            cwd_extractor: extract_top_level_cwd,
            parent_extractor: None,
        }),
        hook: Some(HookDecoding::ClaimsAll(decode_omp_hook_payload)),
        caps: SourceCaps {
            // The `session_exit` entry is appended + flushed on every clean
            // teardown incl. SIGINT/SIGTERM (and the bridge fires
            // `session_shutdown` even for empty sessions the entry skips).
            // SIGKILL: with the bridge, `HookPidWatch` ends the stamped pid's
            // sessions promptly; without it, the stale-sweep.
            has_exit_signal: true,
            // The header `SessionStart` decodes once per transcript life. A
            // `--resume` fires the bridge's `session_start` on the SAME id
            // (`omp/fixtures/resume-hermetic`), so an ended
            // session CAN walk back in — through the SessionStart arm's
            // ordinary parentless re-registration, not this prompt-resurrect
            // flag.
            resurrects_on_prompt: false,
            // The `task` dispatch emits a toolCall/toolResult pair on the parent
            // AND the child persists its own parent-linked transcript.
            delegations_are_hook_silent: false,
            child_end_inference: ChildEndInference::TaskDrain,
            child_hooks_unnamed: false,
            brackets_turns: false,
        },
        // The bridge extension runs IN-PROCESS (upstream: extensions share
        // the runtime), so its stamped `process.pid` IS omp's own —
        // PluginStamp trust. The router stamps the kernel start marker; the
        // click-time compare refuses a recycled pid (#527). The flip also
        // puts omp on `HookPidWatch` (prompt exits) and makes a bridge-only,
        // never-persisted session focusable from birth. Stamp-less shapes —
        // `--no-extensions`, a session predating the bridge install — still
        // focus through its `NATIVE` focus probe, which `resolve_pid` reads
        // with no channel gate.
        focus: FocusChannel::PluginStamp,
    },
    #[cfg(feature = "native")]
    native: native::NATIVE,
};

/// omp's SESSIONS root, mirroring `dirs.ts` — deliberately NOT
/// `omp_agent_dir().join("sessions")`, because the XDG redirect FLATTENS the
/// `agent/` segment away (`$XDG_DATA_HOME/omp/sessions`). Every directory var is
/// read through the `.env` overlay first (`with_omp_dotenv`): upstream applies
/// those files and then REBUILDS this resolver, so a var set only in `~/.env`
/// moves the sessions dir and process env alone would watch an empty directory.
#[cfg(feature = "native")]
pub(crate) fn omp_sessions_dir() -> PathBuf {
    let env = overlaid_env();
    resolve_omp_sessions_dir(
        &env,
        cfg!(any(target_os = "linux", target_os = "macos")),
        &|p| p.exists(),
    )
}

/// omp's ACTIVE agent directory, the root its extension loader scans (its
/// `EXTENSIONS_SUBDIR` child). Same `.env` overlay as
/// `omp_sessions_dir`, same profile/override precedence, but NO XDG
/// flatten: upstream's `getAgentDir()` returns `dirs.agentDir` un-redirected
/// (`dirs.ts` applies XDG only per-category via `agentSubdir`), so a
/// migrated-XDG user's extensions still load from here. Exported for the
/// install target — a second resolver copy is the #880 drift class.
pub fn omp_agent_dir() -> PathBuf {
    let env = overlaid_env();
    resolve_omp_agent_dir(&env)
}

/// The subdirectory omp's loader scans for extension modules: upstream joins
/// it onto each config dir.
pub(crate) const EXTENSIONS_SUBDIR: &str = "extensions";

/// Where the bridge extension installs: [`omp_agent_dir`] +
/// `EXTENSIONS_SUBDIR` (private: the name still greps; rustdoc denies links
/// to non-public items).
pub fn omp_extensions_dir() -> PathBuf {
    omp_agent_dir().join(EXTENSIONS_SUBDIR)
}

/// The process env under the `.env` overlay. The read is deliberately
/// UNBOUNDED, unlike `source/`'s per-refresh probe reads: a cap could truncate
/// a key upstream's own `parseEnvFile` honors.
fn overlaid_env() -> OmpEnv {
    with_omp_dotenv(&OmpEnv::from_process(), &|p| {
        std::fs::read_to_string(p).ok()
    })
}

/// The process environment omp's resolver reads, injected so every arm — the
/// Windows one, the XDG one, the profile ones — unit-tests on any host.
#[derive(Clone)]
struct OmpEnv {
    home: Option<PathBuf>,
    config_dir_name: Option<String>,
    omp_profile: Option<String>,
    pi_profile: Option<String>,
    /// `PI_PROFILE` as the OVERRIDE resolver sees it, which is not always what
    /// profile SELECTION sees: upstream freezes `activeProfile` at module load,
    /// but `resolveActiveAgentDirOverride` re-reads `PI_PROFILE` LIVE — after
    /// the `.env` overlay. Equal to `pi_profile` until `with_omp_dotenv` runs.
    pi_profile_live: Option<String>,
    /// PATH-valued, so `PathBuf` — a `String` here would drop a legal non-UTF-8
    /// override at the read. The four above are NAMES, not paths.
    agent_dir: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
}

impl OmpEnv {
    fn from_process() -> Self {
        let var = crate::platform::text_env;
        Self {
            home: crate::platform::user_home_opt(),
            config_dir_name: var("PI_CONFIG_DIR"),
            omp_profile: var("OMP_PROFILE"),
            pi_profile: var("PI_PROFILE"),
            pi_profile_live: var("PI_PROFILE"),
            agent_dir: crate::platform::path_env("PI_CODING_AGENT_DIR"),
            xdg_data_home: crate::platform::path_env("XDG_DATA_HOME"),
        }
    }
}

/// Node's `path.join` for the ONE place the difference bites: a ROOTED second
/// segment, which Node appends while Rust's `Path::join` lets it REPLACE the base
/// — and omp joins `PI_CONFIG_DIR` under a home it never escapes upstream. The
/// PREFIX matters as much, and only on Windows: `C:\srv\omp` stays bound to
/// `<home>\srv\omp`, not Node's literal, uncreatable `<home>\C:\srv\omp`.
fn node_join(base: &Path, segment: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for c in Path::new(segment).components() {
        match c {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `normalizeProfileName`: trimmed; empty or `"default"` is the implicit default
/// profile. An invalid name THROWS upstream, but every path we observe catches
/// it and proceeds with the default — so an invalid name is `None` here too.
fn normalize_profile_name(raw: Option<&str>) -> Option<String> {
    let n = raw?.trim();
    if n.is_empty() || n == "default" {
        return None;
    }
    // `^[a-z0-9][a-z0-9._-]{0,63}$`, plus the "." / ".." / trailing-dot bans.
    let ok_len = (1..=64).contains(&n.chars().count());
    let mut chars = n.chars();
    let head_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let tail_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !ok_len || !head_ok || !tail_ok || n.ends_with('.') {
        return None;
    }
    // Windows reserved device basenames, extension included, case-insensitive.
    let stem = n.split('.').next().unwrap_or(n).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    (!reserved).then(|| n.to_string())
}

/// `resolveProfileEnv`: `OMP_PROFILE` wins whenever PRESENT — an empty value
/// selects the default profile rather than falling through to `PI_PROFILE`.
fn resolve_profile(env: &OmpEnv) -> Option<String> {
    match env.omp_profile.as_deref() {
        Some(omp) => normalize_profile_name(Some(omp)),
        None => normalize_profile_name(env.pi_profile.as_deref()),
    }
}

/// The fixed path segments of a profile's tree, named once — `dirs.ts` joins
/// these literals, and a silent rename upstream is the drift class
/// `EXTENSIONS_SUBDIR` already guards its own segment against.
const PROFILES_SUBDIR: &str = "profiles";
/// The agent-dir segment under a config root (flattened away by the XDG arm).
const AGENT_SUBDIR: &str = "agent";

/// `getProfileConfigRoot` — `<home>/<PI_CONFIG_DIR|.omp>[/profiles/<profile>]`.
fn omp_config_root(env: &OmpEnv, profile: Option<&str>) -> Option<PathBuf> {
    let name = env
        .config_dir_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(".omp");
    let root = node_join(env.home.as_deref()?, name);
    Some(match profile {
        Some(p) => root.join(PROFILES_SUBDIR).join(p),
        None => root,
    })
}

/// `DirResolver`'s `agentDir`, plus the `isDefault` flag the XDG arm gates on.
/// Precedence: a named profile derives its own agent dir and IGNORES the override;
/// otherwise `PI_CODING_AGENT_DIR` wins, except when it equals the
/// `PI_PROFILE`-derived dir — `resolvePreProfileAgentDir` drops that value, since a
/// parent's `setProfile` exported it rather than the user choosing it.
fn resolve_omp_agent_dir_parts(env: &OmpEnv) -> Option<(PathBuf, bool, Option<String>)> {
    let profile = resolve_profile(env);
    let default_agent = omp_config_root(env, profile.as_deref())?.join(AGENT_SUBDIR);
    if profile.is_some() {
        return Some((default_agent, true, profile));
    }
    let override_dir = env.agent_dir.clone().filter(|v| {
        let derived = normalize_profile_name(env.pi_profile_live.as_deref())
            .and_then(|p| omp_config_root(env, Some(&p)))
            .map(|r| r.join(AGENT_SUBDIR));
        derived.is_none_or(|d| *v != d)
    });
    match override_dir {
        Some(v) => Some((
            crate::platform::warn_if_relative_override("PI_CODING_AGENT_DIR", v),
            false,
            profile,
        )),
        None => Some((default_agent, true, profile)),
    }
}

fn resolve_omp_agent_dir(env: &OmpEnv) -> PathBuf {
    resolve_omp_agent_dir_parts(env)
        .map(|(d, _, _)| d)
        // Keeps the pre-#880 shape: an unresolvable home is already `/tmp`-rooted.
        .unwrap_or_else(|| crate::platform::user_home().join(".omp").join(AGENT_SUBDIR))
}

/// `getSessionsDir()` = `agentSubdir(undefined, "sessions", "data")`.
#[cfg(feature = "native")]
fn resolve_omp_sessions_dir(
    env: &OmpEnv,
    xdg_platform: bool,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let Some((agent_dir, is_default, profile)) = resolve_omp_agent_dir_parts(env) else {
        return resolve_omp_agent_dir(env).join("sessions");
    };
    let xdg = (xdg_platform && is_default)
        .then(|| xdg_app_root(env.xdg_data_home.as_deref(), profile.as_deref(), exists))
        .flatten();
    // The flatten lives here: with XDG the base REPLACES `<…>/agent`, it does
    // not sit under it.
    xdg.unwrap_or(agent_dir).join("sessions")
}

/// Sessions dirs of every OTHER profile — the roots the primary
/// [`omp_sessions_dir`] resolution does not cover. Enumerated from the
/// `profiles/` child of the base config root, each derived with the SAME
/// precedence as the selected profile (its own `agent` dir; the XDG arm
/// flattens `agent/` away where its candidate exists). The env-selected
/// profile is excluded: its dir IS the primary root. Sorted so the watcher
/// set is deterministic. Only the SELECTED profile's `.env` files overlay the
/// env (upstream freezes the resolver per process the same way), so another
/// profile's own `.env` relocations are unseen here.
#[cfg(feature = "native")]
pub(crate) fn omp_profile_sessions_dirs() -> Vec<PathBuf> {
    let env = overlaid_env();
    omp_profile_sessions_dirs_resolved(
        &env,
        cfg!(any(target_os = "linux", target_os = "macos")),
        &|p| p.exists(),
        &|profiles| {
            let Ok(entries) = std::fs::read_dir(profiles) else {
                return Vec::new();
            };
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        },
    )
}

/// The dir [`omp_profile_sessions_dirs`] enumerates, so each new entry in it
/// is a new profile.
#[cfg(feature = "native")]
pub(crate) fn omp_profiles_dir() -> Option<PathBuf> {
    let env = overlaid_env();
    profiles_dir(&env)
}

#[cfg(feature = "native")]
fn profiles_dir(env: &OmpEnv) -> Option<PathBuf> {
    Some(omp_config_root(env, None)?.join(PROFILES_SUBDIR))
}

#[cfg(feature = "native")]
fn omp_profile_sessions_dirs_resolved(
    env: &OmpEnv,
    xdg_platform: bool,
    exists: &dyn Fn(&Path) -> bool,
    list_profiles: &dyn Fn(&Path) -> Vec<String>,
) -> Vec<PathBuf> {
    let Some(profiles) = profiles_dir(env) else {
        return Vec::new();
    };
    let primary = resolve_omp_sessions_dir(env, xdg_platform, exists);
    // Through the same validator omp applies: a dir failing its name regex (or
    // a stray `default`) is one omp can never select, so watching it would
    // follow a root omp never writes.
    let mut names: Vec<String> = list_profiles(&profiles)
        .iter()
        .filter_map(|n| normalize_profile_name(Some(n)))
        .collect();
    names.sort();
    names
        .iter()
        .filter_map(|p| {
            let agent = omp_config_root(env, Some(p))?.join(AGENT_SUBDIR);
            let xdg = xdg_platform
                .then(|| xdg_app_root(env.xdg_data_home.as_deref(), Some(p), exists))
                .flatten();
            Some(xdg.unwrap_or(agent).join("sessions"))
        })
        .filter(|d| *d != primary)
        .collect()
}

/// `resolveIf`: `$XDG_DATA_HOME/omp` (or its `profiles/<name>` child) when that
/// directory EXISTS — the existence gate is what keeps a not-yet-migrated user on
/// the home-rooted layout. Linux and macOS only, and only for a DEFAULT agent dir.
/// The file's own header claims there is no existence check; its code calls
/// `fs.existsSync`.
#[cfg(feature = "native")]
fn xdg_app_root(
    xdg_data_home: Option<&Path>,
    profile: Option<&str>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let app_root = node_join(xdg_data_home?, "omp");
    let candidate = match profile {
        Some(p) => app_root.join(PROFILES_SUBDIR).join(p),
        None => app_root,
    };
    exists(&candidate).then_some(candidate)
}

/// Overlay omp's `.env` files onto `env`, mirroring `env.ts`: located with the
/// resolver frozen at module load (hence the PRE-overlay `env`), then the resolver
/// is REBUILT from the merged env. Shell wins over every file, the first file to
/// define a key wins, and profile SELECTION is deliberately NOT overlaid — upstream
/// reuses the frozen profile. Upstream's `$CWD/.env` is the FIRST and
/// strongest of its four files (`env.ts` iterates project→agent→config→home,
/// first-wins, and Bun autoloads the cwd dotenv before the resolver freezes),
/// unreachable out-of-process because pixtuoid never knows which directory
/// omp was launched from.
fn with_omp_dotenv(env: &OmpEnv, read: &dyn Fn(&Path) -> Option<String>) -> OmpEnv {
    let agent_dir = resolve_omp_agent_dir_parts(env).map(|(dir, _, _)| dir);
    let config_root = omp_config_root(env, resolve_profile(env).as_deref());
    let files = [
        agent_dir.map(|d| d.join(".env")),
        config_root.map(|r| r.join(".env")),
        env.home.as_deref().map(|h| h.join(".env")),
    ];

    let mut out = env.clone();
    for (key, value) in files
        .into_iter()
        .flatten()
        .filter_map(|p| read(&p))
        .flat_map(parse_omp_env_file)
    {
        // The falsy fill test (`!Bun.env[key]`) splits by kind: a NAME keeps whatever
        // the shell had, while `path_env` already resolved a blank PATH to `None`.
        match key.as_str() {
            "PI_CONFIG_DIR" => fill_name(&mut out.config_dir_name, value),
            "PI_PROFILE" => fill_name(&mut out.pi_profile_live, value),
            "PI_CODING_AGENT_DIR" => fill_path(&mut out.agent_dir, value),
            "XDG_DATA_HOME" => fill_path(&mut out.xdg_data_home, value),
            _ => {}
        }
    }
    out
}

/// Fill a NAME slot the shell left unset or blank (upstream's falsy test).
fn fill_name(slot: &mut Option<String>, value: String) {
    if slot.as_deref().is_none_or(|s| s.trim().is_empty()) {
        *slot = Some(value);
    }
}

/// Fill a PATH slot the shell left unset. `platform::path_env` already mapped a
/// blank value to `None`, so absence IS the falsy test here.
fn fill_path(slot: &mut Option<PathBuf>, value: String) {
    if slot.is_none() && !value.trim().is_empty() {
        *slot = Some(PathBuf::from(value));
    }
}

/// Upstream `parseEnvFile`. The `OMP_<X>` → `PI_<X>` aliasing runs AFTER the whole
/// file is read because upstream lets it OVERRIDE an explicit `PI_` key.
fn parse_omp_env_file(text: String) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = text
        .lines()
        .filter_map(|l| parse_omp_env_line(l).map(|(k, v)| (k.to_owned(), v)))
        .collect();
    let aliases: Vec<_> = out
        .iter()
        .filter_map(|(k, v)| Some((format!("PI_{}", k.strip_prefix("OMP_")?), v.clone())))
        .collect();
    out.extend(aliases);
    out
}

/// Upstream `parseEnvLine`: an optional `export` prefix, `#` comments (full-line
/// and inline after whitespace), and single/double/backtick quoting inside which a
/// `#` stays literal. A NUL-bearing value is dropped (`isSafeEnvValue`).
fn parse_omp_env_line(line: &str) -> Option<(&str, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (head, rest) = trimmed.split_once('=')?;
    let head = head.trim();
    let key = match head.strip_prefix("export") {
        Some(tail) if tail.starts_with([' ', '\t']) => tail.trim(),
        _ => head,
    };
    if !is_valid_env_name(key) {
        return None;
    }

    let raw = rest.trim_start_matches([' ', '\t']);
    let value = match raw.chars().next() {
        Some(quote @ ('"' | '\'' | '`')) => {
            let body = &raw[quote.len_utf8()..];
            // The first quote NOT escaped by a preceding `\` closes the value; an
            // unterminated one runs to end of line, as upstream's does.
            let close = body
                .match_indices(quote)
                .find(|&(at, _)| body.as_bytes()[..at].last() != Some(&b'\\'))
                .map(|(at, _)| at);
            close.map_or(body, |at| &body[..at]).to_owned()
        }
        // Unquoted: an inline comment starts at the whitespace preceding its `#`.
        _ => raw
            .char_indices()
            .find(|&(i, c)| matches!(c, ' ' | '\t') && raw[i + 1..].starts_with('#'))
            .map_or(raw, |(i, _)| &raw[..i])
            .trim_end()
            .to_owned(),
    };
    (!value.contains('\0')).then_some((key, value))
}

/// Upstream `isValidEnvName`: the strict POSIX shell-identifier shape, so a dotenv
/// key that no shell could export never reaches the resolver.
fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Does a path component look like a ROOT session file stem
/// (`${fileSafeTimestamp}_${uuid}`)? Subagent stems are task ids (`Alpha`,
/// `GoodWolf`) and never date-shaped. The `T` check is case-insensitive: on
/// Windows the per-line decoder receives the `normalize_path_key`'d path
/// (LOWERCASED), so requiring an upper-case `T` breaks the whole stem chain.
fn looks_like_session_stem(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 20
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10].eq_ignore_ascii_case(&b'T')
        && s.contains('_')
}

/// The stem chain from the root session down to this transcript, e.g.
/// `["<ts>_<uuid>", "Alpha", "Child"]` for `…/<ts>_<uuid>/Alpha/Child.jsonl`; a
/// root transcript is `[stem]`. PURE and case-preserving, so the fixture-fed
/// conformance goldens stay platform-invariant — the Windows case-fold belongs at
/// the WATCHER seam (`walk.rs`, and the probe's own boundary), never here.
fn stem_chain(path: &Path) -> Vec<String> {
    let own = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut chain = vec![own];
    let mut cur = path.parent();
    while let Some(dir) = cur {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            break;
        };
        if looks_like_session_stem(name) {
            chain.push(name.to_string());
            break;
        }
        // Bound the climb at omp's layout boundaries: above the watched root a
        // date-shaped dir (`~/backups/2026-…_snap/`) would fake a subagent chain.
        if name == "sessions" || name.starts_with('-') {
            break;
        }
        // A task-id dir is indistinguishable from a foreign one until the root stem
        // shows up above it, so collect speculatively and discard below.
        chain.push(name.to_string());
        cur = dir.parent();
    }
    if chain.last().is_some_and(|top| looks_like_session_stem(top)) {
        chain.reverse();
        chain
    } else {
        chain.truncate(1);
        chain
    }
}

/// AgentId key: the root stem for a root transcript; the `/`-joined stem chain for
/// a (nested) subagent, so `Alpha` under two different sessions never collides.
/// Also the `session_id` [`decode_omp_line`] mints, so its `SessionStart` and the
/// watcher's first-sight one agree on one key.
pub fn omp_id_from_path(path: &Path) -> String {
    stem_chain(path).join("/")
}

/// The parent's key (the chain minus the last segment); `None` for a root. The
/// `task` tool persists each child as a SEPARATE file
/// `<parent-path-minus-.jsonl>/<taskId>.jsonl`, recursively — the linkage is that
/// PATH NESTING, never a header field (a child header carries no `parentSession`).
pub(crate) fn omp_parent_key_from_path(path: &Path) -> Option<String> {
    let chain = stem_chain(path);
    (chain.len() > 1).then(|| chain[..chain.len() - 1].join("/"))
}

/// The header entry (line 2; line 1 is a fixed-width `title` slot that omp
/// rewrites IN PLACE — pwrite at offset 0, and the tail cursor sits past it, so it
/// is never re-read; legacy files lack the slot). Fork, branch, version migration
/// and tool-output pruning REWRITE the file atomically (temp + rename → new inode)
/// and the watcher re-stats by path, so a rewrite reads as a fresh transcript.
const SESSION: &str = "session";

/// A turn entry. `role:"assistant"` content carries `toolCall` blocks
/// (`{id,name,arguments}`); `role:"toolResult"` closes one by `toolCallId`. Its
/// `usage.input` EXCLUDES the cache share (`totalTokens` = input + output +
/// cacheRead + cacheWrite), so fresh spend is input + cacheWrite + output — cache
/// READS are re-served context, not new spend.
const MESSAGE: &str = "message";

/// The `custom` entry envelope. Its `customType:"tool_execution_start"` duplicates
/// each toolCall right before execution and is deliberately NOT decoded — the same
/// `tool_use_id` would double-count `tool_call_count`.
const CUSTOM: &str = "custom";

/// A `--thinking` pin change, forwarded RAW: omp's vocabulary already contains
/// `burn::MAX_EFFORTS` verbatim, so translating would be a second vocabulary to
/// keep in sync. Its `configured` field is the user's PIN, not the turn's level.
const THINKING_LEVEL_CHANGE: &str = "thinking_level_change";
const TITLE: &str = "title";
const TITLE_CHANGE: &str = "title_change";
/// The title payload key read by both first-sight and live carriers. It happens
/// to match the `title` entry type today; keep the authorities separate so one
/// upstream rename cannot silently rewrite both surfaces.
const TITLE_FIELD: &str = "title";

/// The `customType` marking a clean teardown — the ONLY structural end omp writes.
/// The guard falls through to `_ => vec![]`: rename it upstream and no omp
/// TRANSCRIPT ever ends cleanly, with no breadcrumb. Appended on every clean
/// teardown, SIGINT/SIGTERM included; its reason/kind is
/// ignored, because every kind ("normal"|"signal"|"fatal"|"process_exit") IS an
/// end. Skipped when the session never produced an assistant message (the
/// bridge's `session_shutdown` ends that one outright), and SIGKILL writes
/// nothing — with the bridge `HookPidWatch` catches it; without, the
/// stale-sweep.
const SESSION_EXIT: &str = "session_exit";

/// The turn role carrying tool calls. Its `model` is read BARE, never the
/// provider-prefixed `model_change` form, so `TOP_MODELS` prefix matching sees the
/// vocabulary CC/codex/copilot emit. `model_change` itself stays undecoded: every
/// assistant message re-stamps the bare `model` anyway — one turn's lag.
const ROLE_ASSISTANT: &str = "assistant";

const ROLE_TOOL_RESULT: &str = "toolResult";

/// The assistant-content block a tool call arrives in. Its `id` is a REQUIRED
/// pairing key on a block we decode, so an absent one is upstream drift rather than
/// a line we ignore — breadcrumb, then drop. An unkeyable `toolResult` is the same
/// case: it could never close its Start, leaking Running forever.
const BLOCK_TOOL_CALL: &str = "toolCall";

/// The tool that BLOCKS on human input: its `ToolUseStart` makes the ask's own
/// tool_use_id a member of the reducer's action request, so the answer's
/// `toolResult` resolves the Wait. The request takes as its member the call
/// shown when it is raised, so ask pairs are appended LAST: each ask's [`AgentEvent::UserActionRequest`]
/// directly follows its own start, never a sibling's.
const TOOL_ASK: &str = "ask";

/// `prefix·<text>` at the label cap — the ONE place omp assembles a
/// content-derived label, shared by the subagent-stem deriver and the session
/// title, so neither can bypass the bound that keeps an untrusted string out of
/// the painter and the headless summary. The prefix is read from the registry,
/// never hardcoded (invariant #3).
fn omp_label(source: &str, text: &str) -> String {
    let prefix = crate::source::decoder::label_prefix_for(source);
    format!("{prefix}·{}", ellipsize(text, MAX_DECODED_FIELD_CHARS))
}

/// omp label: a NESTED transcript is a subagent, and its file STEM is the
/// human-authored `tasks[].name` the parent dispatched it under (`Alpha`,
/// `OmpWireFormat`) — which the default deriver throws away. omp subagents run
/// IN-PROCESS, so a child's `session` header repeats the parent's `cwd`
/// verbatim: `prefix·<cwd-basename>` renders a whole delegation fan-out as N
/// identical labels, separated only by the disambiguation suffix. A ROOT keeps
/// the shared cwd derivation as its FLOOR — superseded by the session title,
/// which reaches first sight through [`omp_head_title`] and the live stream
/// through the decoder's `title_change` arm.
// Rides `derive_prefixed_label`'s gate: the watcher wiring is `native`, and the
// registry conformance test is the only other caller.
#[cfg(any(feature = "native", test))]
pub(crate) fn omp_derive_label(path: &Path, source: &str, cwd: &Path) -> String {
    if omp_parent_key_from_path(path).is_some()
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        return omp_label(source, stem);
    }
    crate::source::decoder::derive_prefixed_label(source, cwd)
}

/// The session title off ONE head line, for the first-sight label. omp writes
/// the title into a fixed-width line-1 slot precisely so it is readable without
/// the transcript — which is what makes it the right carrier here: when the
/// backlog is skipped (an oversized root, a revival's tail-only read) the
/// decoder's `title` arm never runs, and the label would fall back to the cwd
/// basename every concurrent session in a repo shares.
///
/// EMPTY is not a title: omp writes the slot at birth and fills it later, and
/// leaves SUBAGENT titles empty forever — `None` keeps the stem/cwd deriver.
// Same gate as `omp_derive_label`: the only non-test caller is the `native`
// watcher wiring.
#[cfg(any(feature = "native", test))]
pub(crate) fn omp_head_title(v: &Value) -> Option<String> {
    let obj = v.as_object()?;
    matches!(obj.get("type").and_then(|t| t.as_str()), Some(TITLE)).then_some(())?;
    let title = obj.get(TITLE_FIELD).and_then(|t| t.as_str())?;
    (!title.is_empty()).then(|| omp_label(SOURCE_NAME, title))
}
/// Decode one omp session JSONL line into zero or more `AgentEvent`s.
/// Unknown entry types / roles and malformed shapes return `vec![]` — the
/// upstream loader is itself lenient (`parseJsonlLenient`).
pub fn decode_omp_line(transcript_path: &str, source: &str, v: Value) -> Vec<AgentEvent> {
    let path = Path::new(transcript_path);
    let acting = AgentId::from_parts(source, &omp_id_from_path(path));
    let Some(obj) = v.as_object() else {
        return vec![];
    };
    let kind = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");

    match kind {
        SESSION => {
            let cwd = obj.get("cwd").and_then(|c| c.as_str()).unwrap_or_else(|| {
                crate::source::drift::missing_field(source, "session", "cwd");
                ""
            });
            let parent_id = omp_parent_key_from_path(path).map(|k| AgentId::from_parts(source, &k));
            vec![AgentEvent::SessionStart {
                agent_id: acting,
                source: source.to_string(),
                session_id: omp_id_from_path(path),
                cwd: PathBuf::from(cwd),
                parent_id,
            }]
        }
        MESSAGE => {
            let Some(msg) = obj.get("message") else {
                return vec![];
            };
            match msg.get("role").and_then(|r| r.as_str()) {
                Some(ROLE_ASSISTANT) => {
                    let mut out = Vec::new();
                    if let Some(model) = msg
                        .get("model")
                        .and_then(|m| m.as_str())
                        .filter(|m| !m.is_empty())
                    {
                        out.push(AgentEvent::ModelInfo {
                            agent_id: acting,
                            model: Some(ellipsize(model, MAX_DECODED_FIELD_CHARS)),
                            effort: None,
                        });
                    }
                    if let Some(usage) = msg.get("usage").and_then(|u| u.as_object()) {
                        let field = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                        let fresh = field("input")
                            .saturating_add(field("cacheWrite"))
                            .saturating_add(field("output"));
                        if fresh > 0 {
                            out.push(AgentEvent::Usage {
                                agent_id: acting,
                                fresh_tokens: fresh,
                            });
                        }
                    }
                    let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
                        return out;
                    };
                    for b in blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some(BLOCK_TOOL_CALL))
                    {
                        let Some(id) = b.get("id").and_then(|i| i.as_str()) else {
                            crate::source::drift::missing_field(source, BLOCK_TOOL_CALL, "id");
                            continue;
                        };
                        let name = b.get("name").and_then(|n| n.as_str()).unwrap_or_else(|| {
                            crate::source::drift::missing_field(source, BLOCK_TOOL_CALL, "name");
                            ""
                        });
                        out.push(AgentEvent::ToolUseStart {
                            agent_id: acting,
                            tool_use_id: Some(id.to_string()),
                            detail: Some(omp_tool_detail(name, b.get("arguments"))),
                        });
                        if name == TOOL_ASK {
                            out.push(AgentEvent::user_action_request(
                                acting,
                                Some(omp_ask_reason(b.get("arguments"))),
                                RequestKeys {
                                    tool_use_id: Some(id.to_string()),
                                    ..RequestKeys::default()
                                },
                            ));
                        }
                    }
                    out
                }
                Some(ROLE_TOOL_RESULT) => {
                    let Some(tool_call_id) = msg.get("toolCallId").and_then(|i| i.as_str()) else {
                        crate::source::drift::missing_field(source, "toolResult", "toolCallId");
                        return vec![];
                    };
                    vec![AgentEvent::ToolUseEnd {
                        agent_id: acting,
                        tool_use_id: Some(tool_call_id.to_string()),
                    }]
                }
                // user / developer / bashExecution / … — not sprite-visible.
                _ => vec![],
            }
        }
        CUSTOM if obj.get("customType").and_then(|c| c.as_str()) == Some(SESSION_EXIT) => {
            vec![AgentEvent::SessionEnd {
                agent_id: acting,
                as_child: omp_parent_key_from_path(path).is_some(),
            }]
        }
        THINKING_LEVEL_CHANGE => match obj.get("thinkingLevel").and_then(|l| l.as_str()) {
            Some(level) if !level.is_empty() => vec![AgentEvent::ModelInfo {
                agent_id: acting,
                model: None,
                effort: Some(ellipsize(level, MAX_DECODED_FIELD_CHARS)),
            }],
            _ => vec![],
        },
        // The only human-readable name a ROOT transcript has. An unguarded Rename
        // would blank it and wipe every subagent's dispatch-name label — the empty
        // slot's WHY is on `omp_head_title`.
        TITLE | TITLE_CHANGE => match obj.get(TITLE_FIELD).and_then(|t| t.as_str()) {
            Some(title) if !title.is_empty() => vec![AgentEvent::Rename {
                agent_id: acting,
                label: omp_label(source, title),
            }],
            _ => vec![],
        },
        // Not sprite-visible (`model_change` among them — see `ROLE_ASSISTANT`).
        _ => vec![],
    }
}

/// omp's tool-detail dispatch. The subagent dispatch is the `task` tool,
/// detected by NAME only: `arguments` are model-authored, so a hallucinated
/// `subagent_type` key must not flip an ordinary tool to Delegating.
fn omp_tool_detail(tool: &str, args: Option<&Value>) -> ToolDetail {
    if tool == "task" {
        return ToolDetail::Task;
    }
    // `i` is omp's mandated per-call INTENT, so it sits LAST, below every concrete
    // target — a path beats a paraphrase of that path. It earns its place on the
    // tools with no keyed target at all (`edit`, `todo`, `job`, `hub`, …).
    const KEYS: &[&str] = &["command", "path", "pattern", "query", "i"];
    crate::source::decoder::generic_keyed_detail(tool, args, KEYS)
}

/// The [`AgentEvent::UserActionRequest`] reason for an `ask` round: the first question's text, falling
/// back to the call's intent (`arguments.i`), then the bare tool name.
fn omp_ask_reason(args: Option<&Value>) -> String {
    args.and_then(|a| {
        a.get("questions")
            .and_then(|q| q.as_array())
            .and_then(|q| q.first())
            .and_then(|q| q.get("question"))
            .and_then(|q| q.as_str())
            .or_else(|| a.get("i").and_then(|i| i.as_str()))
    })
    .map(|t| ellipsize(t, MAX_DECODED_FIELD_CHARS))
    .unwrap_or_else(|| "ask".to_string())
}

// --- extension-bridge hook payloads (#951) ---------------------------------

/// The bridge extension's lifecycle event names, verbatim from omp's
/// extension API (`extensibility/shared-events.ts`), forwarded by
/// `install/omp_extension.ts`.
const HOOK_SESSION_START: &str = "session_start";
const HOOK_SESSION_SWITCH: &str = "session_switch";
const HOOK_SESSION_BRANCH: &str = "session_branch";
const HOOK_SESSION_SHUTDOWN: &str = "session_shutdown";
/// The approval pair (`extensibility/extensions/types.ts`) — the state the
/// transcript can never carry: omp does not persist approval waits.
const HOOK_APPROVAL_REQUESTED: &str = "tool_approval_requested";
const HOOK_APPROVAL_RESOLVED: &str = "tool_approval_resolved";

/// The hook event types this decoder turns into events, pinned to the dispatch arms by
/// `every_exported_set_is_exactly_its_dispatch_arms` and to the TS
/// extension's registrations by its selftest, through `the_plugin_contract_is_the_committed_file`.
#[cfg(any(test, feature = "harness"))]
#[doc(hidden)]
pub const DECODED_HOOK_EVENTS: &[&str] = &[
    HOOK_SESSION_START,
    HOOK_SESSION_SWITCH,
    HOOK_SESSION_BRANCH,
    HOOK_SESSION_SHUTDOWN,
    HOOK_APPROVAL_REQUESTED,
    HOOK_APPROVAL_RESOLVED,
];

/// Decode one bridge-extension payload (already routed here by
/// `_pixtuoid_source == "omp"`). pixtuoid authors the envelope
/// (`install/omp_extension.ts`), so an unknown `type` is foreign or a stale
/// installed extension — a drift breadcrumb and a benign skip, never a bail:
/// the hook plane log-and-continues (workspace convention) and a stale
/// bridge is a supported state `verify_schema` reports.
///
/// Identity: `sessionFile` (the ALLOCATED path — omp exposes it before the
/// lazy persist materializes the JSONL) through the watcher's own
/// `normalize_path_key` fold then [`omp_id_from_path`], so both transports
/// mint ONE AgentId per session, nested task children included. `sessionId`
/// (the bare header UUID, a keyspace no stem can collide with — stems carry
/// the date shape and `_`) covers `--no-session`, where no transcript will
/// ever exist to coalesce with.
///
/// A switch/branch means this omp process now drives the CURRENT file: the
/// previous one gets its End without waiting for the stale sweep, and the
/// upstream `reason` (`"new" | "resume" | "fork"`) is deliberately dropped —
/// the End+Start pair keys on `previousSessionFile` alone, identically for
/// all three.
///
/// The start/switch/branch arms trail an [`AgentEvent::Identity`]: the router
/// stamps pids onto Identity events only, and the focus stamp must arm from
/// BIRTH to cover a bridge-only (never-persisted) session — the exit watch
/// needs no help, `SessionStart` is already a bind target.
///
/// # Errors
///
/// Never: a malformed or unknown payload decodes to `vec![]`; the `Result` is the hook-decoder signature.
pub(crate) fn decode_omp_hook_payload(obj: &HookObject) -> Result<Vec<AgentEvent>> {
    let Some(ty) = obj.get("type").and_then(|t| t.as_str()) else {
        crate::source::drift::missing_field(SOURCE_NAME, "hook", "type");
        return Ok(vec![]);
    };
    let (key, parent_key) = match omp_hook_session_key(obj) {
        Some(k) => k,
        None => {
            crate::source::drift::missing_field(SOURCE_NAME, ty, "sessionFile");
            return Ok(vec![]);
        }
    };
    let agent_id = AgentId::from_parts(SOURCE_NAME, &key);
    let cwd = || {
        obj.get("cwd")
            .and_then(|c| c.as_str())
            .map(std::path::PathBuf::from)
    };
    let session_start = || AgentEvent::SessionStart {
        agent_id,
        source: SOURCE_NAME.to_string(),
        session_id: key.clone(),
        cwd: cwd().unwrap_or_default(),
        parent_id: parent_key
            .as_deref()
            .map(|p| AgentId::from_parts(SOURCE_NAME, p)),
    };
    let identity = || AgentEvent::identity(agent_id, SOURCE_NAME, key.clone(), cwd());

    match ty {
        // The trailing `identity()` is the focus-pid carrier (the declaration
        // doc owns the story).
        HOOK_SESSION_START => Ok(vec![session_start(), identity()]),
        // previous == current is the in-place branch REWRITE — an End+Start
        // pair there would walk the sprite out for its own rewrite.
        HOOK_SESSION_SWITCH | HOOK_SESSION_BRANCH => {
            let mut evs = Vec::new();
            if let Some(prev) = obj
                .get("previousSessionFile")
                .and_then(|p| p.as_str())
                .filter(|p| !p.is_empty())
            {
                let prev_key = omp_id_from_path(Path::new(&crate::id::normalize_path_key(prev)));
                if prev_key != key {
                    evs.push(AgentEvent::SessionEnd {
                        agent_id: AgentId::from_parts(SOURCE_NAME, &prev_key),
                        as_child: omp_parent_key_from_path(Path::new(
                            &crate::id::normalize_path_key(prev),
                        ))
                        .is_some(),
                    });
                }
            }
            evs.push(session_start());
            evs.push(identity());
            Ok(evs)
        }
        HOOK_SESSION_SHUTDOWN => Ok(vec![AgentEvent::SessionEnd {
            agent_id,
            as_child: parent_key.is_some(),
        }]),
        HOOK_APPROVAL_REQUESTED | HOOK_APPROVAL_RESOLVED => {
            let Some(tool_call_id) = obj.get("toolCallId").and_then(|t| t.as_str()) else {
                crate::source::drift::missing_field(SOURCE_NAME, ty, "toolCallId");
                return Ok(vec![]);
            };
            let tool = obj.get("toolName").and_then(|t| t.as_str()).unwrap_or("");
            let event = if ty == HOOK_APPROVAL_REQUESTED {
                AgentEvent::user_action_request(
                    agent_id,
                    Some(omp_approval_reason(obj, tool)),
                    RequestKeys {
                        tool_use_id: Some(tool_call_id.to_string()),
                        ..RequestKeys::default()
                    },
                )
            } else if obj.get("approved").and_then(|a| a.as_bool()) == Some(true) {
                // The resume. copilot's approve-emits-nothing shape doesn't fit
                // omp: the transcript wrote this call BEFORE the request, so
                // only this Start can lift the wait.
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id: Some(tool_call_id.to_string()),
                    detail: Some(omp_tool_detail(tool, None)),
                }
            } else {
                // Denial: resolves the gated wait now; the transcript's
                // isError toolResult later is the deduped/no-op backstop.
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id: Some(tool_call_id.to_string()),
                }
            };
            Ok(vec![identity(), event])
        }
        _ => {
            crate::source::drift::unknown_event(SOURCE_NAME, ty);
            Ok(vec![])
        }
    }
}

/// The session key (and parent key) for one hook payload: the folded
/// `sessionFile` stem chain, else the bare `sessionId` UUID.
fn omp_hook_session_key(obj: &serde_json::Map<String, Value>) -> Option<(String, Option<String>)> {
    if let Some(file) = obj
        .get("sessionFile")
        .and_then(|f| f.as_str())
        .filter(|f| !f.is_empty())
    {
        let folded = crate::id::normalize_path_key(file);
        return Some((
            omp_id_from_path(Path::new(&folded)),
            omp_parent_key_from_path(Path::new(&folded)),
        ));
    }
    obj.get("sessionId")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| (s.to_string(), None))
}

/// The [`AgentEvent::UserActionRequest`] reason for an approval gate: the wire's `reason` when it says
/// anything (a plain bash gate sends none — probe-verified at this source's
/// `verified_version`), else the tool name, else the literal gate.
fn omp_approval_reason(obj: &serde_json::Map<String, Value>, tool: &str) -> String {
    let wire = obj
        .get("reason")
        .and_then(|r| r.as_str())
        .filter(|r| !r.is_empty());
    match wire {
        Some(r) => ellipsize(r, MAX_DECODED_FIELD_CHARS),
        None if !tool.is_empty() => ellipsize(tool, MAX_DECODED_FIELD_CHARS),
        None => "approval".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::decoder::test_obj;
    use serde_json::json;

    const ROOT: &str = "/home/u/.omp/agent/sessions/-dev-proj/2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001.jsonl";
    const ROOT_KEY: &str = "2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001";
    const CHILD: &str = "/home/u/.omp/agent/sessions/-dev-proj/2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001/Alpha.jsonl";
    const GRANDCHILD: &str = "/home/u/.omp/agent/sessions/-dev-proj/2026-07-09T08-00-00-000Z_0197f0aa-0000-7000-8000-000000000001/Alpha/GoodWolf.jsonl";

    /// Each entry type reaches a real arm. Guarded arms get the payload each
    /// needs to emit; a bare guarded type falls through like any unread type.
    #[test]
    fn each_entry_type_reaches_an_arm() {
        let drive = |ty: &str| {
            let line = match ty {
                CUSTOM => json!({"type": ty, "id": "x", "parentId": null,
                                 "timestamp": "t", "customType": "session_exit"}),
                MESSAGE => json!({"type": ty, "id": "x", "parentId": null, "timestamp": "t",
                                  "message": {"role": "assistant", "timestamp": 1,
                                              "model": "anthropic/claude-opus-4-5"}}),
                THINKING_LEVEL_CHANGE => json!({"type": ty, "id": "x", "parentId": null,
                                                "timestamp": "t", "thinkingLevel": "high"}),
                TITLE | TITLE_CHANGE => json!({"type": ty, "id": "x", "parentId": null,
                                               "timestamp": "t", "title": "Named session"}),
                _ => json!({"type": ty, "version": 3, "id": "x", "timestamp": "t",
                            "cwd": "/home/u/proj"}),
            };
            let mut evs = 0;
            let logs = crate::test_capture::capture_logs(|| {
                evs = decode_omp_line(ROOT, SOURCE_NAME, line).len();
            });
            evs > 0 || logs.contains(crate::source::drift::TARGET)
        };
        for ty in [
            SESSION,
            MESSAGE,
            CUSTOM,
            THINKING_LEVEL_CHANGE,
            TITLE,
            TITLE_CHANGE,
        ] {
            assert!(drive(ty), "{ty} must reach a real arm");
        }
        assert!(!drive("checkpoint"), "an unread entry type reaches neither");
    }

    /// The length gate is EXCLUSIVE: `<19-char ts>_` is the longest stem that still
    /// carries no uuid. A subagent task id can never reach the date shape, so the
    /// `_` separator is what makes a stem a root. The `T` check stays
    /// case-insensitive for the lowercased Windows key.
    #[test]
    fn a_root_stem_needs_both_the_date_shape_and_the_uuid_separator() {
        assert!(!looks_like_session_stem("2026-07-16T12-00-05_"));
        assert!(looks_like_session_stem("2026-07-16T12-00-05_abc"));
        assert!(!looks_like_session_stem("2026-07-16T12-00-05-999999"));
        assert!(looks_like_session_stem("2026-07-16t12-00-05_abc"));
    }

    fn root() -> AgentId {
        AgentId::from_parts(SOURCE_NAME, ROOT_KEY)
    }
    fn decode_at(path: &str, line: &str) -> Vec<AgentEvent> {
        decode_omp_line(path, SOURCE_NAME, serde_json::from_str(line).unwrap())
    }
    fn decode(line: &str) -> Vec<AgentEvent> {
        decode_at(ROOT, line)
    }

    #[test]
    fn id_from_path_is_the_stem_for_a_root_and_the_chain_for_subagents() {
        assert_eq!(omp_id_from_path(Path::new(ROOT)), ROOT_KEY);
        assert_eq!(
            omp_id_from_path(Path::new(CHILD)),
            format!("{ROOT_KEY}/Alpha")
        );
        assert_eq!(
            omp_id_from_path(Path::new(GRANDCHILD)),
            format!("{ROOT_KEY}/Alpha/GoodWolf")
        );
    }

    #[test]
    fn parent_key_links_each_level_to_the_one_above() {
        assert_eq!(omp_parent_key_from_path(Path::new(ROOT)), None);
        assert_eq!(
            omp_parent_key_from_path(Path::new(CHILD)).as_deref(),
            Some(ROOT_KEY)
        );
        assert_eq!(
            omp_parent_key_from_path(Path::new(GRANDCHILD)),
            Some(format!("{ROOT_KEY}/Alpha"))
        );
    }

    #[test]
    fn same_task_id_under_two_sessions_keys_distinctly() {
        let other = "/home/u/.omp/agent/sessions/-dev-proj/2026-07-09T09-00-00-000Z_0197f0bb-0000-7000-8000-000000000002/Alpha.jsonl";
        assert_ne!(
            omp_id_from_path(Path::new(CHILD)),
            omp_id_from_path(Path::new(other))
        );
    }

    #[test]
    fn stem_chain_survives_the_windows_case_fold() {
        let folded = "c:/users/u/.omp/agent/sessions/-dev-proj/2026-07-09t08-00-00-000z_0197f0aa-0000-7000-8000-000000000001/alpha.jsonl";
        assert_eq!(
            omp_id_from_path(Path::new(folded)),
            "2026-07-09t08-00-00-000z_0197f0aa-0000-7000-8000-000000000001/alpha",
            "a lowercased timestamp must still read as a session stem"
        );
        assert_eq!(
            omp_parent_key_from_path(Path::new(folded)).as_deref(),
            Some("2026-07-09t08-00-00-000z_0197f0aa-0000-7000-8000-000000000001"),
            "the parent link must survive the fold"
        );
    }

    #[test]
    fn date_shaped_dirs_above_the_sessions_root_do_not_misclassify() {
        let p = format!(
            "/home/u/backups/2026-01-01T00-00-00-000Z_snap/agent/sessions/-dev-proj/{stem}.jsonl",
            stem = ROOT_KEY
        );
        assert_eq!(omp_id_from_path(Path::new(&p)), ROOT_KEY);
        assert_eq!(omp_parent_key_from_path(Path::new(&p)), None);
    }

    /// omp subagents run IN-PROCESS and inherit the parent's `cwd`, so the
    /// default cwd-basename deriver renders a fan-out as N identical labels.
    #[test]
    fn subagent_labels_come_from_the_task_name_not_the_shared_cwd() {
        let cwd = Path::new("/home/u/proj");
        let child = format!("/home/u/.omp/agent/sessions/-dev-proj/{ROOT_KEY}/OmpWireFormat.jsonl");
        assert_eq!(
            omp_derive_label(Path::new(&child), SOURCE_NAME, cwd),
            "om·OmpWireFormat",
            "a subagent is named by the task it was dispatched under"
        );
        // Siblings sharing that cwd must NOT collapse onto one label.
        let sibling =
            format!("/home/u/.omp/agent/sessions/-dev-proj/{ROOT_KEY}/OmpConfigDirs.jsonl");
        assert_ne!(
            omp_derive_label(Path::new(&sibling), SOURCE_NAME, cwd),
            omp_derive_label(Path::new(&child), SOURCE_NAME, cwd)
        );
        // A ROOT's own stem is a timestamp+uuid, so the cwd stays the better name.
        assert_eq!(
            omp_derive_label(Path::new(ROOT), SOURCE_NAME, cwd),
            "om·proj"
        );
        // The stem is MODEL-authored, so it rides the same cap as every other
        // content-derived label.
        let long = "N".repeat(MAX_DECODED_FIELD_CHARS * 4);
        let capped = omp_derive_label(
            Path::new(&format!(
                "/home/u/.omp/agent/sessions/-dev-proj/{ROOT_KEY}/{long}.jsonl"
            )),
            SOURCE_NAME,
            cwd,
        );
        assert_eq!(
            capped.chars().count(),
            "om·".chars().count() + MAX_DECODED_FIELD_CHARS + 1
        );
        assert!(capped.ends_with('…'));
    }

    /// Byte-real (omp 17.2.9): the line-1 slot and the appended rename carry
    /// the same `title` field, and concurrent sessions in ONE repo are the case
    /// that makes this matter — they all share a cwd basename.
    #[test]
    fn a_non_empty_session_title_renames_the_slot_and_an_empty_one_never_does() {
        let slot = r#"{"type":"title","v":1,"title":"Raise PR for working branch","source":"auto","updatedAt":"t","pad":"   "}"#;
        match &decode(slot)[..] {
            [AgentEvent::Rename { agent_id, label }] => {
                assert_eq!(*agent_id, root());
                assert_eq!(label, "om·Raise PR for working branch");
            }
            other => panic!("expected one Rename, got {other:?}"),
        }
        let changed = r#"{"type":"title_change","id":"x","parentId":"p","timestamp":"t","title":"Implement auto-refresh","source":"auto","previousTitle":"Auto-detect changes","trigger":"auto"}"#;
        match &decode(changed)[..] {
            [AgentEvent::Rename { label, .. }] => assert_eq!(label, "om·Implement auto-refresh"),
            other => panic!("expected one Rename, got {other:?}"),
        }
        // The slot exists from birth and omp leaves SUBAGENT titles empty
        // forever — renaming to a blank would wipe the deriver's label.
        for blank in [
            r#"{"type":"title","v":1,"title":"","source":null,"updatedAt":"t","pad":"   "}"#,
            r#"{"type":"title","v":1,"source":"auto","updatedAt":"t","pad":"   "}"#,
            r#"{"type":"title_change","id":"x","parentId":null,"timestamp":"t","title":null}"#,
        ] {
            assert!(decode(blank).is_empty(), "expected no events for {blank}");
        }
        // Auto-generated from model output, so it rides the label cap.
        let long = "T".repeat(MAX_DECODED_FIELD_CHARS * 3);
        let line =
            format!(r#"{{"type":"title_change","id":"x","timestamp":"t","title":"{long}"}}"#);
        match &decode(&line)[..] {
            [AgentEvent::Rename { label, .. }] => {
                assert_eq!(
                    label.chars().count(),
                    "om·".chars().count() + MAX_DECODED_FIELD_CHARS + 1
                );
                assert!(label.ends_with('…'));
            }
            other => panic!("expected one Rename, got {other:?}"),
        }
    }

    /// The first-sight carrier of the same title. Most root transcripts are
    /// already past `MAX_PENDING_BYTES` when first seen, and that path decodes
    /// NOTHING — so without this the decoder's `title` arm never runs for the
    /// common case and the root keeps the shared cwd basename.
    #[test]
    fn head_title_names_a_root_at_first_sight_but_never_on_an_empty_slot() {
        let head = |s: &str| omp_head_title(&serde_json::from_str(s).unwrap());
        assert_eq!(
            head(r#"{"type":"title","v":1,"title":"Raise PR for working branch","pad":"   "}"#),
            Some("om·Raise PR for working branch".to_string())
        );
        // Empty/missing slot, and the OTHER entry types that carry a `title`
        // key: only the line-1 slot is the head carrier, and a subagent's slot
        // stays empty forever — either would clobber a better label.
        for none in [
            r#"{"type":"title","v":1,"title":"","pad":"   "}"#,
            r#"{"type":"title","v":1,"pad":"   "}"#,
            r#"{"type":"title_change","id":"x","title":"Implement auto-refresh"}"#,
            r#"{"type":"session","version":3,"cwd":"/home/u/proj"}"#,
            r#"["not an object"]"#,
        ] {
            assert_eq!(head(none), None, "expected no head title for {none}");
        }
        // Model-authored, so it rides the same cap as every decoded field.
        let long = "T".repeat(MAX_DECODED_FIELD_CHARS * 3);
        let capped = head(&format!(r#"{{"type":"title","v":1,"title":"{long}"}}"#)).unwrap();
        assert_eq!(
            capped.chars().count(),
            "om·".chars().count() + MAX_DECODED_FIELD_CHARS + 1
        );
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn both_title_readers_use_the_title_field() {
        fn title_entry(kind: &str, field: &str) -> Value {
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), Value::String(kind.to_string()));
            obj.insert(
                field.to_string(),
                Value::String("Name this session".to_string()),
            );
            Value::Object(obj)
        }

        let field = TITLE_FIELD;
        assert_eq!(
            omp_head_title(&title_entry(TITLE, field)).as_deref(),
            Some("om·Name this session")
        );
        assert!(matches!(
            decode_omp_line(ROOT, SOURCE_NAME, title_entry(TITLE_CHANGE, field))
                .as_slice(),
            [AgentEvent::Rename { label, .. }] if label == "om·Name this session"
        ));

        let renamed = "renamedTitle";
        assert!(omp_head_title(&title_entry(TITLE, renamed)).is_none());
        assert!(decode_omp_line(ROOT, SOURCE_NAME, title_entry(TITLE_CHANGE, renamed)).is_empty());
    }

    #[test]
    fn subagent_header_registers_child_parented_to_the_root() {
        let line = r#"{"type":"session","version":3,"id":"0197f0cc-0000-7000-8000-000000000003","timestamp":"2026-07-09T08:01:00.000Z","cwd":"/home/u/proj"}"#;
        match &decode_at(CHILD, line)[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    parent_id,
                    ..
                },
            ] => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, &format!("{ROOT_KEY}/Alpha"))
                );
                assert_eq!(*parent_id, Some(root()));
            }
            other => panic!("expected one parented SessionStart, got {other:?}"),
        }
    }

    /// Every message-level name reaches a real arm. Each is an equality guard
    /// whose miss is SILENT — a renamed role decodes the turn to nothing, a
    /// renamed block type finds no tool calls, a renamed `ask` strands the RequiresAction
    /// gate.
    #[test]
    fn each_message_level_name_reaches_an_arm() {
        let turn = |role: &str, block: &str, tool: &str| {
            decode_omp_line(
                ROOT,
                SOURCE_NAME,
                json!({"type": "message", "id": "m1", "parentId": null, "timestamp": "t",
                       "message": {"role": role, "timestamp": 1,
                                   "content": [{"type": block, "id": "t1", "name": tool}]}}),
            )
            .len()
        };
        let good = turn(ROLE_ASSISTANT, BLOCK_TOOL_CALL, TOOL_ASK);
        assert!(good > 0, "the message vocabulary decodes a turn");
        for (role, block, tool, what) in [
            ("pxd", BLOCK_TOOL_CALL, TOOL_ASK, "role"),
            (ROLE_ASSISTANT, "pxd", TOOL_ASK, "block type"),
        ] {
            assert!(
                turn(role, block, tool) < good,
                "a renamed {what} must decode to less, not the same",
            );
        }

        // The ask arm's OWN probe: an ask-named call must gate a UserActionRequest, and a
        // renamed ask must not — start-only, never the gate.
        let waits = |tool: &str| {
            decode_omp_line(
                ROOT,
                SOURCE_NAME,
                json!({"type": "message", "id": "m2", "parentId": null, "timestamp": "t",
                       "message": {"role": ROLE_ASSISTANT, "timestamp": 1,
                                   "content": [{"type": BLOCK_TOOL_CALL, "id": "t2",
                                                "name": tool}]}}),
            )
            .iter()
            .any(|e| matches!(e, AgentEvent::UserActionRequest { .. }))
        };
        assert!(waits(TOOL_ASK), "an ask call must gate a UserActionRequest");
        assert!(
            !waits("pxd_renamed_ask"),
            "a renamed ask must not gate a UserActionRequest"
        );
    }

    #[test]
    fn session_exit_in_a_subagent_file_ends_the_child_as_child() {
        let line = r#"{"type":"custom","id":"a1b2c3d4","parentId":null,"timestamp":"t","customType":"session_exit","data":{"reason":"task complete","kind":"normal","recordedAt":"t"}}"#;
        match &decode_at(CHILD, line)[..] {
            [AgentEvent::SessionEnd { agent_id, as_child }] => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts(SOURCE_NAME, &format!("{ROOT_KEY}/Alpha"))
                );
                assert!(*as_child);
            }
            other => panic!("expected child SessionEnd, got {other:?}"),
        }
    }

    #[test]
    fn assistant_usage_becomes_a_fresh_token_observation() {
        let line = r#"{"type":"message","id":"m1","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[],"usage":{"input":122,"output":1491,"cacheRead":1000,"cacheWrite":1000,"totalTokens":3613},"timestamp":1720512000000}}"#;
        let evs = decode(line);
        assert!(
            evs.iter().any(|e| matches!(
                e,
                AgentEvent::Usage {
                    fresh_tokens: 2613,
                    ..
                }
            )),
            "expected fresh=2613 (cacheRead excluded), got {evs:?}"
        );
        let line = r#"{"type":"message","id":"m2","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[],"usage":{"input":0,"output":0,"cacheRead":500,"cacheWrite":0,"totalTokens":500},"timestamp":1720512000000}}"#;
        assert!(
            !decode(line)
                .iter()
                .any(|e| matches!(e, AgentEvent::Usage { .. })),
            "cache-read-only reading must be silent"
        );
    }

    /// Byte-real: omp mandates a per-call `i` intent, and its `edit`/`todo`
    /// tools carry NO path-like argument, so they would render as a bare verb
    /// without `i` as the last-resort target.
    #[test]
    fn intent_is_the_last_resort_target_and_never_outranks_a_concrete_one() {
        let display_of = |args: &str| {
            let line = format!(
                r#"{{"type":"message","id":"m1","parentId":null,"timestamp":"t","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"t1","name":"edit","arguments":{args}}}],"timestamp":1}}}}"#
            );
            match &decode(&line)[..] {
                [
                    AgentEvent::ToolUseStart {
                        detail: Some(ToolDetail::Generic { display }),
                        ..
                    },
                ] => display.clone(),
                other => panic!("expected one ToolUseStart, got {other:?}"),
            }
        };
        assert_eq!(
            display_of(r#"{"i":"Adding the palette color","input":"…"}"#),
            "edit: Adding the palette color",
            "a keyless tool falls back to its intent"
        );
        assert_eq!(
            display_of(r#"{"path":"src/burn.rs","i":"Adding the palette color"}"#),
            "edit: src/burn.rs",
            "a CONCRETE target outranks the paraphrase"
        );
        assert_eq!(
            display_of(r#"{"input":"…"}"#),
            "edit",
            "no target and no intent stays a bare verb"
        );
        // The intent rides the same target cap as every other key, so a
        // model that writes an essay can't blow out the sprite's detail line.
        let long = "T".repeat(200);
        let capped = display_of(&format!(r#"{{"i":"{long}"}}"#));
        let target = capped.strip_prefix("edit: ").expect("has a target suffix");
        assert_eq!(
            target.chars().count(),
            crate::source::decoder::MAX_TOOL_TARGET_CHARS + 1
        );
        assert!(target.ends_with('…'));
    }

    #[test]
    fn assistant_message_surfaces_model_info_for_the_burn_tier() {
        let line = r#"{"type":"message","id":"m1","parentId":null,"timestamp":"t","message":{"role":"assistant","provider":"kimi-code","model":"kimi-for-coding","content":[{"type":"toolCall","id":"t1","name":"bash","arguments":{"command":"ls"}}],"timestamp":1}}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ModelInfo {
                    agent_id,
                    model: Some(model),
                    effort: None,
                },
                AgentEvent::ToolUseStart { .. },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(model.as_str(), "kimi-for-coding");
            }
            other => panic!("expected ModelInfo then ToolUseStart, got {other:?}"),
        }
        let text_only = r#"{"type":"message","id":"m2","parentId":null,"timestamp":"t","message":{"role":"assistant","provider":"anthropic","model":"claude-fable-5","content":[{"type":"text","text":"done"}],"timestamp":2}}"#;
        match &decode(text_only)[..] {
            [AgentEvent::ModelInfo { model: Some(m), .. }] => {
                assert_eq!(m.as_str(), "claude-fable-5");
            }
            other => panic!("expected one ModelInfo, got {other:?}"),
        }
        let empty = r#"{"type":"message","id":"m3","timestamp":"t","message":{"role":"assistant","model":"","content":[],"timestamp":3}}"#;
        assert!(decode(empty).is_empty());
    }

    /// Byte-real shape, anchored by the recorded `omp/fixtures/tool-run-hermetic`:
    /// `configured` is the user's PIN and is `null` whenever the pin is "auto",
    /// while `thinkingLevel` is what the turn actually ran at — only the latter
    /// is an effort observation.
    #[test]
    fn thinking_level_change_is_an_effort_observation_that_spares_the_model() {
        let line = r#"{"type":"thinking_level_change","id":"3576fccd","parentId":"db62fa97","timestamp":"2026-06-23T12:22:24.469Z","thinkingLevel":"xhigh","configured":null}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ModelInfo {
                    agent_id,
                    model: None,
                    effort: Some(effort),
                },
            ] => {
                assert_eq!(*agent_id, root());
                // Forwarded RAW: `burn::MAX_EFFORTS` already contains "xhigh"
                assert_eq!(effort.as_str(), "xhigh");
            }
            other => panic!("expected one model-free ModelInfo, got {other:?}"),
        }
        // A pin the user set to a DIFFERENT level must not leak in — the
        // running level wins.
        let pinned = r#"{"type":"thinking_level_change","id":"a","parentId":null,"timestamp":"t","thinkingLevel":"max","configured":"xhigh"}"#;
        match &decode(pinned)[..] {
            [
                AgentEvent::ModelInfo {
                    effort: Some(e), ..
                },
            ] => assert_eq!(e.as_str(), "max"),
            other => panic!("expected the RUNNING level, got {other:?}"),
        }
        // Absent/blank stays silent rather than stamping an empty effort, which
        // would blank an already-observed one for a whole TTL.
        for quiet in [
            r#"{"type":"thinking_level_change","id":"a","parentId":null,"timestamp":"t","configured":"max"}"#,
            r#"{"type":"thinking_level_change","id":"a","parentId":null,"timestamp":"t","thinkingLevel":""}"#,
            r#"{"type":"thinking_level_change","id":"a","parentId":null,"timestamp":"t","thinkingLevel":3}"#,
        ] {
            assert!(decode(quiet).is_empty(), "expected no events for {quiet}");
        }
    }

    #[test]
    fn task_dispatch_is_delegating() {
        let line = r#"{"type":"message","id":"m3","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"t3","name":"task","arguments":{"task":"fix the flaky test","id":"Alpha"}}],"timestamp":1}}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    detail: Some(d), ..
                },
            ] => assert!(d.is_task(), "task tool must be Delegating, got {d:?}"),
            other => panic!("expected Delegating ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn spoofed_subagent_type_arg_does_not_make_a_task() {
        let line = r#"{"type":"message","id":"m4","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"t4","name":"read","arguments":{"path":"x.rs","subagent_type":null}}],"timestamp":1}}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    detail: Some(d), ..
                },
            ] => assert!(
                !d.is_task(),
                "a spoofed subagent_type arg must stay Generic, got {d:?}"
            ),
            other => panic!("expected Generic ToolUseStart, got {other:?}"),
        }
    }

    #[test]
    fn ask_call_starts_activity_then_waits_on_the_question() {
        let line = r#"{"type":"message","id":"m7","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"tool_ASK1","name":"ask","arguments":{"i":"Resolving packages/ui collision","questions":[{"id":"ui_collision","question":"packages/ui already exists. What should happen?","options":[{"label":"Replace"},{"label":"Merge"}]}]}}],"timestamp":1}}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id,
                    ..
                },
                AgentEvent::UserActionRequest {
                    agent_id: wid,
                    reason,
                    tool_use_id: asked,
                    ..
                },
            ] => {
                assert_eq!(*agent_id, root());
                assert_eq!(*wid, root());
                assert_eq!(tool_use_id.as_deref(), Some("tool_ASK1"));
                assert_eq!(asked, tool_use_id, "the question names its own call");
                assert!(
                    reason
                        .as_deref()
                        .is_some_and(|r| r.contains("packages/ui already exists")),
                    "reason carries the question text, got {reason:?}"
                );
            }
            other => panic!("expected ToolUseStart then UserActionRequest, got {other:?}"),
        }
    }

    #[test]
    fn ask_reason_falls_back_to_intent_then_bare_name() {
        let intent = r#"{"type":"message","id":"m8","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"tool_ASK2","name":"ask","arguments":{"i":"Confirming scope"}}],"timestamp":1}}"#;
        match &decode(intent)[..] {
            [_, AgentEvent::UserActionRequest { reason, .. }] => {
                assert_eq!(reason.as_deref(), Some("Confirming scope"))
            }
            other => panic!("expected Start+UserActionRequest, got {other:?}"),
        }
        let bare = r#"{"type":"message","id":"m9","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"tool_ASK3","name":"ask"}],"timestamp":1}}"#;
        match &decode(bare)[..] {
            [_, AgentEvent::UserActionRequest { reason, .. }] => {
                assert_eq!(reason.as_deref(), Some("ask"))
            }
            other => panic!("expected Start+UserActionRequest, got {other:?}"),
        }
    }

    #[test]
    fn an_ask_among_parallel_calls_names_its_own() {
        let line = r#"{"type":"message","id":"mB","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"tool_ASK5","name":"ask","arguments":{"i":"Confirming scope"}},{"type":"toolCall","id":"t7","name":"bash","arguments":{"command":"cargo check"}}],"timestamp":1}}"#;
        match &decode(line)[..] {
            [
                AgentEvent::ToolUseStart {
                    tool_use_id: ask, ..
                },
                AgentEvent::UserActionRequest {
                    tool_use_id: asked, ..
                },
                AgentEvent::ToolUseStart {
                    tool_use_id: bash, ..
                },
            ] => {
                assert_eq!(ask.as_deref(), Some("tool_ASK5"));
                assert_eq!(asked, ask);
                assert_eq!(bash.as_deref(), Some("t7"));
            }
            other => panic!(
                "expected the ask's Start+UserActionRequest, then bash's Start, got {other:?}"
            ),
        }
    }

    #[test]
    fn tool_call_without_id_is_dropped_and_without_name_still_starts() {
        let no_id = r#"{"type":"message","id":"m5","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{}}],"timestamp":1}}"#;
        let out = crate::test_capture::capture_logs(|| {
            assert!(decode(no_id).is_empty(), "un-keyable toolCall → no event");
        });
        for needle in [crate::source::drift::TARGET, "missing_field", "toolCall"] {
            assert!(
                out.contains(needle),
                "no id breadcrumb: missing {needle:?}\n{out}"
            );
        }
        let no_name = r#"{"type":"message","id":"m6","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","id":"t6","arguments":{}}],"timestamp":1}}"#;
        let out = crate::test_capture::capture_logs(|| match &decode(no_name)[..] {
            [
                AgentEvent::ToolUseStart {
                    tool_use_id,
                    detail: Some(d),
                    ..
                },
            ] => {
                assert_eq!(tool_use_id.as_deref(), Some("t6"));
                assert!(!d.is_task());
            }
            other => panic!("expected one ToolUseStart, got {other:?}"),
        });
        for needle in [crate::source::drift::TARGET, "missing_field", "toolCall"] {
            assert!(
                out.contains(needle),
                "no name breadcrumb: missing {needle:?}\n{out}"
            );
        }
    }

    #[test]
    fn toolresult_without_id_drops_with_a_drift_breadcrumb() {
        let no_id = r#"{"type":"message","id":"m7","parentId":null,"timestamp":"t","message":{"role":"toolResult","toolName":"read","content":[],"timestamp":1}}"#;
        let out = crate::test_capture::capture_logs(|| {
            assert!(decode(no_id).is_empty(), "un-keyable toolResult → no event");
        });
        for needle in [crate::source::drift::TARGET, "missing_field", "toolResult"] {
            assert!(
                out.contains(needle),
                "no toolResult breadcrumb: missing {needle:?}\n{out}"
            );
        }
    }

    #[test]
    fn tool_execution_start_custom_entry_is_deliberately_ignored() {
        let line = r#"{"type":"custom","id":"c1","parentId":null,"timestamp":"t","customType":"tool_execution_start","data":{"toolCallId":"toolu_01AAA","toolName":"read","startedAt":"t"}}"#;
        assert!(decode(line).is_empty());
    }

    #[test]
    fn non_lifecycle_entries_and_malformed_lines_are_ignored_not_panicked() {
        for line in [
            // Titles decode to a Rename only when NON-empty; the birth-state
            // empty slot is the non-lifecycle case (owned in full by
            // `a_non_empty_session_title_renames_the_slot_and_an_empty_one_never_does`).
            r#"{"type":"title","v":1,"title":"","source":null,"updatedAt":"t","pad":"   "}"#,
            r#"{"type":"model_change","id":"x","parentId":null,"timestamp":"t","model":"anthropic/claude-opus-4-5"}"#,
            r#"{"type":"compaction","id":"x","parentId":null,"timestamp":"t","summary":"…","firstKeptEntryId":"y","tokensBefore":1}"#,
            r#"{"type":"session_init","id":"x","parentId":null,"timestamp":"t","systemPrompt":"…","task":"…","tools":[]}"#,
            r#"{"type":"message","id":"x","parentId":null,"timestamp":"t","message":{"role":"user","content":"hi","timestamp":1}}"#,
            r#"{"type":"message","id":"x","parentId":null,"timestamp":"t","message":{"role":"bashExecution","command":"ls","output":"","exitCode":0,"timestamp":1}}"#,
            r#"{"type":"message","id":"x","parentId":null,"timestamp":"t"}"#,
            r#"{"type":"custom","id":"x","parentId":null,"timestamp":"t","customType":"memory_write","data":{}}"#,
        ] {
            assert!(decode(line).is_empty(), "expected no events for {line}");
        }
        assert!(decode_omp_line(ROOT, SOURCE_NAME, json!("not an object")).is_empty());
        assert!(decode_omp_line(ROOT, SOURCE_NAME, json!(["array"])).is_empty());
    }

    #[test]
    fn no_entry_type_breadcrumbs_however_new() {
        for line in [
            r#"{"type":"quantum_entry","id":"x","parentId":null,"timestamp":"t"}"#,
            r#"{"type":"title","v":1,"title":"","source":null,"updatedAt":"t","pad":"   "}"#,
            r#"{"type":"model_change","id":"x","parentId":null,"timestamp":"t","model":"m"}"#,
            r#"{"type":"compaction","id":"x","parentId":null,"timestamp":"t","summary":"…"}"#,
            r#"{"type":"custom","id":"x","parentId":null,"timestamp":"t","customType":"memory_write","data":{}}"#,
            r#"{"type":"thinking_level_change","id":"x","parentId":null,"timestamp":"t"}"#,
            r#"{"type":"credential_pin","id":"x","parentId":null,"timestamp":"t","provider":"anthropic","hash":"h"}"#,
            r#"{"type":"reset_boundary","id":"x","parentId":null,"timestamp":"t"}"#,
        ] {
            let quiet = crate::test_capture::capture_logs(|| {
                assert!(decode(line).is_empty());
            });
            assert!(
                !quiet.contains(crate::source::drift::TARGET),
                "an entry we read nothing from must stay out of the drift log:\n{quiet}"
            );
        }
    }

    #[test]
    fn session_header_without_cwd_registers_with_empty_cwd() {
        let line = r#"{"type":"session","version":3,"id":"0197","timestamp":"t"}"#;
        match &decode(line)[..] {
            [AgentEvent::SessionStart { cwd, .. }] => {
                assert_eq!(cwd, Path::new(""), "missing cwd → empty path fallback");
            }
            other => panic!("expected one SessionStart, got {other:?}"),
        }
    }

    /// `PI_CONFIG_DIR` is user-controlled, so the invariant is that NO value escapes
    /// home. Its TEETH are Windows-CI-only, and deliberately so: the drive-letter
    /// escape exists only there (`C:\\srv` parses as Prefix+RootDir and replaces the
    /// base), while on Unix that string is one ordinary component that never could —
    /// so a green macOS run does NOT prove this (the `windows-test` class).
    #[test]
    fn node_join_never_lets_a_rooted_segment_escape_the_base() {
        let base = Path::new("/home/u");
        for hostile in [
            "/srv/omp",
            r"C:\srv\omp",
            r"C:srv",
            r"\srv",
            r"\\?\C:\srv",
            "plain",
        ] {
            let got = node_join(base, hostile);
            assert!(
                got.starts_with(base),
                "{hostile:?} escaped the base: {got:?}"
            );
        }
    }

    /// Every arm injected, so the XDG and Windows-reserved-name branches run on
    /// any host.
    #[test]
    fn omp_sessions_dir_mirrors_every_axis_of_the_upstream_resolver() {
        let base = |f: &dyn Fn(&mut OmpEnv)| {
            let mut e = OmpEnv {
                home: Some("/home/u".into()),
                config_dir_name: None,
                omp_profile: None,
                pi_profile: None,
                pi_profile_live: None,
                agent_dir: None,
                xdg_data_home: None,
            };
            f(&mut e);
            // `from_process` reads both from the same var; they diverge only
            // after the `.env` overlay, which these axis cases don't exercise.
            if e.pi_profile_live.is_none() {
                e.pi_profile_live.clone_from(&e.pi_profile);
            }
            e
        };
        let never = |_: &Path| false;
        let sessions = |e: &OmpEnv| resolve_omp_sessions_dir(e, true, &never);

        assert_eq!(
            sessions(&base(&|_| {})),
            PathBuf::from("/home/u/.omp/agent/sessions")
        );

        // PI_CONFIG_DIR is a NAME under home...
        assert_eq!(
            sessions(&base(&|e| e.config_dir_name = Some(".myomp".into()))),
            PathBuf::from("/home/u/.myomp/agent/sessions")
        );
        // ...and an ABSOLUTE value does NOT escape home, because upstream joins
        // it Node-style. Rust's own `Path::join` would return `/srv/omp/...`.
        assert_eq!(
            sessions(&base(&|e| e.config_dir_name = Some("/srv/omp".into()))),
            PathBuf::from("/home/u/srv/omp/agent/sessions"),
            "an absolute PI_CONFIG_DIR must stay bound under home"
        );

        // Profiles.
        assert_eq!(
            sessions(&base(&|e| e.omp_profile = Some("work".into()))),
            PathBuf::from("/home/u/.omp/profiles/work/agent/sessions")
        );
        assert_eq!(
            sessions(&base(&|e| e.pi_profile = Some("work".into()))),
            PathBuf::from("/home/u/.omp/profiles/work/agent/sessions"),
            "PI_PROFILE is the legacy fallback when OMP_PROFILE is ABSENT"
        );
        assert_eq!(
            sessions(&base(&|e| {
                e.omp_profile = Some(String::new());
                e.pi_profile = Some("work".into());
            })),
            PathBuf::from("/home/u/.omp/agent/sessions"),
            "an EMPTY OMP_PROFILE explicitly selects default — it does not fall through"
        );
        for bad in ["CON", "com1.txt", "Work", "..", "trailing.", "-lead"] {
            assert_eq!(
                sessions(&base(&|e| e.omp_profile = Some(bad.into()))),
                PathBuf::from("/home/u/.omp/agent/sessions"),
                "invalid profile {bad:?} degrades to default, it does not build a path"
            );
        }

        // PI_CODING_AGENT_DIR, and the profile-derived drop rule.
        assert_eq!(
            sessions(&base(&|e| e.agent_dir = Some("/custom/agent".into()))),
            PathBuf::from("/custom/agent/sessions")
        );
        assert_eq!(
            sessions(&base(&|e| {
                e.omp_profile = Some("work".into());
                e.agent_dir = Some("/custom/agent".into());
            })),
            PathBuf::from("/home/u/.omp/profiles/work/agent/sessions"),
            "a named profile derives its own agent dir; the override is ignored"
        );
        assert_eq!(
            sessions(&base(&|e| {
                e.omp_profile = Some(String::new());
                e.pi_profile = Some("work".into());
                e.agent_dir = Some("/home/u/.omp/profiles/work/agent".into());
            })),
            PathBuf::from("/home/u/.omp/agent/sessions"),
            "a PI_PROFILE-derived override is DROPPED, not adopted as the default baseline"
        );
    }

    /// Write the bridge to a flattened path and omp never loads it while
    /// verify reads healthy (#951); `omp_agent_dir` owns the mechanism.
    #[test]
    fn the_agent_dir_ignores_xdg_where_the_sessions_dir_flattens() {
        let env = OmpEnv {
            home: Some("/home/u".into()),
            config_dir_name: None,
            omp_profile: None,
            pi_profile: None,
            pi_profile_live: None,
            agent_dir: None,
            xdg_data_home: Some("/xdg".into()),
        };
        assert_eq!(
            resolve_omp_sessions_dir(&env, true, &|p| p == Path::new("/xdg/omp")),
            PathBuf::from("/xdg/omp/sessions"),
            "precondition: this env DOES flatten the sessions dir"
        );
        assert_eq!(
            resolve_omp_agent_dir(&env),
            PathBuf::from("/home/u/.omp/agent")
        );
        let overridden = OmpEnv {
            agent_dir: Some("/custom/agent".into()),
            ..env.clone()
        };
        assert_eq!(
            resolve_omp_agent_dir(&overridden),
            PathBuf::from("/custom/agent"),
            "PI_CODING_AGENT_DIR moves the extensions root"
        );
        let profiled = OmpEnv {
            omp_profile: Some("work".into()),
            ..env
        };
        assert_eq!(
            resolve_omp_agent_dir(&profiled),
            PathBuf::from("/home/u/.omp/profiles/work/agent"),
            "a named profile derives its own agent dir"
        );
    }

    #[test]
    fn profile_enumeration_derives_each_profiles_sessions_dir() {
        let env = OmpEnv {
            home: Some("/home/u".into()),
            config_dir_name: None,
            omp_profile: None,
            pi_profile: None,
            pi_profile_live: None,
            agent_dir: None,
            xdg_data_home: Some("/xdg".into()),
        };
        let list = |p: &Path| {
            assert_eq!(
                p,
                Path::new("/home/u/.omp/profiles"),
                "enumerates the profiles dir"
            );
            vec!["play".to_string(), "work".to_string()]
        };
        // XDG exists only for `work`, so `work` gets the flattened shape and
        // `play` the standard profile agent shape.
        let exists = |p: &Path| p == Path::new("/xdg/omp/profiles/work");
        assert_eq!(
            omp_profile_sessions_dirs_resolved(&env, true, &exists, &list),
            vec![
                PathBuf::from("/home/u/.omp/profiles/play/agent/sessions"),
                PathBuf::from("/xdg/omp/profiles/work/sessions"),
            ]
        );
    }

    #[test]
    fn profile_enumeration_skips_the_selected_profile_and_survives_no_profiles_dir() {
        let mut env = OmpEnv {
            home: Some("/home/u".into()),
            config_dir_name: None,
            omp_profile: Some("work".into()),
            pi_profile: None,
            pi_profile_live: None,
            agent_dir: None,
            xdg_data_home: None,
        };
        let none = |_: &Path| false;
        // `work` is the PRIMARY root (env-selected); only `other` is extra.
        let list = |_: &Path| vec!["work".to_string(), "other".to_string()];
        assert_eq!(
            omp_profile_sessions_dirs_resolved(&env, false, &none, &list),
            vec![PathBuf::from("/home/u/.omp/profiles/other/agent/sessions")]
        );
        // Names omp's own validator refuses — a stray `default` dir, a case
        // violation — never become roots (the watcher would materialize them).
        let junk = |_: &Path| {
            vec![
                "default".to_string(),
                "Work".to_string(),
                "other".to_string(),
            ]
        };
        assert_eq!(
            omp_profile_sessions_dirs_resolved(&env, false, &none, &junk),
            vec![PathBuf::from("/home/u/.omp/profiles/other/agent/sessions")]
        );
        // No profiles dir at all -> nothing to add.
        let empty = |_: &Path| Vec::new();
        assert_eq!(
            omp_profile_sessions_dirs_resolved(&env, false, &none, &empty),
            Vec::<PathBuf>::new()
        );
        // An unresolvable home enumerates nothing rather than panicking.
        env.home = None;
        assert_eq!(
            omp_profile_sessions_dirs_resolved(&env, false, &none, &list),
            Vec::<PathBuf>::new()
        );
    }

    /// XDG changes the SHAPE, not the prefix: it replaces the base.
    #[test]
    fn omp_xdg_flattens_the_agent_segment_and_only_when_the_dir_exists() {
        let env = |xdg: Option<&str>, profile: Option<&str>, agent: Option<&str>| OmpEnv {
            home: Some("/home/u".into()),
            config_dir_name: None,
            omp_profile: profile.map(str::to_string),
            pi_profile: None,
            pi_profile_live: None,
            agent_dir: agent.map(PathBuf::from),
            xdg_data_home: xdg.map(PathBuf::from),
        };
        let exists = |want: &'static str| move |p: &Path| p == Path::new(want);
        let home_default = PathBuf::from("/home/u/.omp/agent/sessions");

        assert_eq!(
            resolve_omp_sessions_dir(&env(Some("/xdg"), None, None), true, &exists("/xdg/omp")),
            PathBuf::from("/xdg/omp/sessions"),
            "the agent/ segment is FLATTENED, not prefixed"
        );
        assert_eq!(
            resolve_omp_sessions_dir(&env(Some("/xdg"), None, None), true, &|_| false),
            home_default,
            "an absent $XDG_DATA_HOME/omp keeps the home layout — the existence gate"
        );
        assert_eq!(
            resolve_omp_sessions_dir(&env(Some("/xdg"), None, None), false, &exists("/xdg/omp")),
            home_default,
            "XDG is linux/darwin only"
        );
        assert_eq!(
            resolve_omp_sessions_dir(
                &env(Some("/xdg"), None, Some("/custom/agent")),
                true,
                &exists("/xdg/omp")
            ),
            PathBuf::from("/custom/agent/sessions"),
            "an agent-dir override makes isDefault false, which disables XDG"
        );
        assert_eq!(
            resolve_omp_sessions_dir(
                &env(Some("/xdg"), Some("work"), None),
                true,
                &exists("/xdg/omp/profiles/work")
            ),
            PathBuf::from("/xdg/omp/profiles/work/sessions"),
            "a profile keys the XDG choice on the PROFILE path, never the app root"
        );
        assert_eq!(
            resolve_omp_sessions_dir(
                &env(Some("/xdg"), Some("work"), None),
                true,
                &exists("/xdg/omp")
            ),
            PathBuf::from("/home/u/.omp/profiles/work/agent/sessions"),
            "the base app root existing is NOT enough for a named profile"
        );
        assert_eq!(
            resolve_omp_sessions_dir(&env(Some(""), None, None), true, &exists("/xdg/omp")),
            home_default,
            "a blank XDG_DATA_HOME never yields an ABSOLUTE xdg root, so the probe misses"
        );
    }

    /// [`resolve_omp_sessions_dir`] with omp's `.env` overlay applied first.
    /// `files` maps an absolute `.env` path to its contents; a path absent from
    /// it does not exist, so the empty table is the no-files baseline.
    fn dotenv_sessions(
        vars: &[(&str, &str)],
        files: &[(&str, &str)],
        existing_xdg: Option<&str>,
    ) -> PathBuf {
        let get = |k: &str| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        };
        // Production reads PATH slots through `platform::path_env`, which maps a
        // blank to `None` — collapse here too, or the yields case tests a fiction.
        let get_path = |k: &str| get(k).filter(|v| !v.trim().is_empty()).map(PathBuf::from);
        let env = OmpEnv {
            home: Some("/home/u".into()),
            config_dir_name: get("PI_CONFIG_DIR"),
            omp_profile: get("OMP_PROFILE"),
            pi_profile: get("PI_PROFILE"),
            pi_profile_live: get("PI_PROFILE"),
            agent_dir: get_path("PI_CODING_AGENT_DIR"),
            xdg_data_home: get_path("XDG_DATA_HOME"),
        };
        let overlaid = with_omp_dotenv(&env, &|p| {
            files
                .iter()
                .find(|(n, _)| Path::new(n) == p)
                .map(|(_, c)| (*c).to_owned())
        });
        resolve_omp_sessions_dir(&overlaid, true, &|p| {
            existing_xdg.is_some_and(|x| p == Path::new(x))
        })
    }

    /// The `.env` overlay's directory axes — `with_omp_dotenv` owns why a
    /// file-only var still moves this root. The PATH half of the falsy fill
    /// test is settled earlier, by `path_env` at the READ, and pinned end to
    /// end by `omp_sessions_dir_honors_non_empty_env_override`.
    #[test]
    fn a_dotenv_directory_var_moves_the_sessions_dir_the_way_omp_does() {
        const HOME_ENV: &str = "/home/u/.env";
        const CONFIG_ENV: &str = "/home/u/.omp/.env";
        const AGENT_ENV: &str = "/home/u/.omp/agent/.env";
        let default = Path::new("/home/u/.omp/agent/sessions");

        assert_eq!(
            dotenv_sessions(&[], &[], None),
            default,
            "no files, no change"
        );
        assert_eq!(
            dotenv_sessions(&[], &[(HOME_ENV, "PI_CODING_AGENT_DIR=/data/omp")], None),
            Path::new("/data/omp/sessions")
        );
        // The shell wins over every file — upstream fills only an unset key…
        assert_eq!(
            dotenv_sessions(
                &[("PI_CODING_AGENT_DIR", "/shell/agent")],
                &[(HOME_ENV, "PI_CODING_AGENT_DIR=/data/omp")],
                None
            ),
            Path::new("/shell/agent/sessions")
        );
        // …and its test is FALSY. A NAME var reaches the overlay with the blank
        // still on it, so an exported `PI_CONFIG_DIR=` must still yield here.
        assert_eq!(
            dotenv_sessions(
                &[("PI_CONFIG_DIR", "")],
                &[(HOME_ENV, "PI_CONFIG_DIR=.pi")],
                None
            ),
            Path::new("/home/u/.pi/agent/sessions"),
            "a set-but-blank NAME yields to a file, as upstream's falsy test does"
        );
        // The other direction: a blank value IN a file is not a definition, so it
        // never claims the first-to-define slot from a later file.
        assert_eq!(
            dotenv_sessions(
                &[],
                &[
                    (AGENT_ENV, "PI_CODING_AGENT_DIR="),
                    (HOME_ENV, "PI_CODING_AGENT_DIR=/from/home"),
                ],
                None
            ),
            Path::new("/from/home/sessions"),
            "a blank file value does not claim the key from a later file"
        );
        // Among files the FIRST to define a key wins: agent, config root, home.
        let all = [
            (AGENT_ENV, "PI_CODING_AGENT_DIR=/from/agent"),
            (CONFIG_ENV, "PI_CODING_AGENT_DIR=/from/config"),
            (HOME_ENV, "PI_CODING_AGENT_DIR=/from/home"),
        ];
        assert_eq!(
            dotenv_sessions(&[], &all, None),
            Path::new("/from/agent/sessions")
        );
        assert_eq!(
            dotenv_sessions(&[], &all[1..], None),
            Path::new("/from/config/sessions")
        );
        assert_eq!(
            dotenv_sessions(&[], &all[2..], None),
            Path::new("/from/home/sessions")
        );
        assert_eq!(
            dotenv_sessions(&[], &[(HOME_ENV, "PI_CONFIG_DIR=.pi")], None),
            Path::new("/home/u/.pi/agent/sessions")
        );
        assert_eq!(
            dotenv_sessions(
                &[],
                &[(HOME_ENV, "XDG_DATA_HOME=/home/u/.local/share")],
                Some("/home/u/.local/share/omp")
            ),
            Path::new("/home/u/.local/share/omp/sessions")
        );
        // Files are located with the PRE-overlay dirs, so a named profile's own
        // tree is where omp looks — upstream reads them before its rebuild too.
        assert_eq!(
            dotenv_sessions(
                &[("OMP_PROFILE", "work")],
                &[(
                    "/home/u/.omp/profiles/work/agent/.env",
                    "XDG_DATA_HOME=/home/u/.local/share"
                )],
                Some("/home/u/.local/share/omp/profiles/work")
            ),
            Path::new("/home/u/.local/share/omp/profiles/work/sessions")
        );
    }

    /// The overlay is DIRECTORY vars only: upstream's rebuild reuses the profile
    /// frozen at module load, so a file-borne profile moves nothing there and
    /// honoring one here would invent a tree omp never selected.
    #[test]
    fn a_dotenv_profile_is_ignored_because_upstream_freezes_the_profile() {
        assert_eq!(
            dotenv_sessions(
                &[],
                &[("/home/u/.env", "OMP_PROFILE=work\nPI_PROFILE=work")],
                None
            ),
            Path::new("/home/u/.omp/agent/sessions")
        );
    }

    /// The files are located from the PRE-overlay env, because upstream reads
    /// them with the resolver `dirs.ts` froze at module load and only THEN calls
    /// `refreshDirsFromEnv()`. Recomputing the search from the merged env would
    /// read a `.env` omp never opened — so a home file that moves `PI_CONFIG_DIR`
    /// must NOT move where the config-root file is looked for.
    #[test]
    fn dotenv_files_are_located_before_the_overlay_can_move_them() {
        assert_eq!(
            dotenv_sessions(
                &[],
                &[
                    ("/home/u/.env", "PI_CONFIG_DIR=.other"),
                    ("/home/u/.omp/.env", "PI_CODING_AGENT_DIR=/from/pre"),
                    ("/home/u/.other/.env", "PI_CODING_AGENT_DIR=/from/post"),
                ],
                None
            ),
            Path::new("/from/pre/sessions"),
            "the pre-overlay config root is the one whose `.env` is read"
        );
    }

    /// …but `PI_PROFILE` still reaches the agent-dir DROP check, because
    /// upstream's `resolveActiveAgentDirOverride` re-reads it LIVE while
    /// `activeProfile` stays frozen. A `.env` carrying a parent's leftovers
    /// must therefore drop the override without moving the profile path.
    #[test]
    fn a_dotenv_pi_profile_still_drops_a_profile_derived_agent_dir() {
        const HOME_ENV: &str = "/home/u/.env";
        const LEFTOVERS: &str =
            "PI_PROFILE=work\nPI_CODING_AGENT_DIR=/home/u/.omp/profiles/work/agent";
        assert_eq!(
            dotenv_sessions(&[], &[(HOME_ENV, LEFTOVERS)], None),
            Path::new("/home/u/.omp/agent/sessions"),
            "an inherited profile-derived override is dropped, not honoured"
        );
        // The SELECTION half stays frozen: the profile path itself must not move.
        assert_eq!(
            dotenv_sessions(&[], &[(HOME_ENV, "PI_PROFILE=work")], None),
            Path::new("/home/u/.omp/agent/sessions")
        );
        // A GENUINE override still survives the same file.
        assert_eq!(
            dotenv_sessions(
                &[],
                &[(
                    HOME_ENV,
                    "PI_PROFILE=work\nPI_CODING_AGENT_DIR=/custom/agent"
                )],
                None
            ),
            Path::new("/custom/agent/sessions")
        );
        // And the shell's own PI_PROFILE still wins over the file's.
        assert_eq!(
            dotenv_sessions(&[("PI_PROFILE", "other")], &[(HOME_ENV, LEFTOVERS)], None),
            Path::new("/home/u/.omp/profiles/other/agent/sessions"),
            "a shell profile selects its own tree and the override is disabled"
        );
    }

    /// Upstream's `OMP_<X>` → `PI_<X>` alias, which OVERRIDES an explicit `PI_`
    /// key in the same file — hence the pass over the WHOLE file, not per line.
    #[test]
    fn a_dotenv_omp_prefixed_key_aliases_and_overrides_its_pi_twin() {
        assert_eq!(
            dotenv_sessions(
                &[],
                &[("/home/u/.env", "OMP_CODING_AGENT_DIR=/aliased")],
                None
            ),
            Path::new("/aliased/sessions")
        );
        assert_eq!(
            dotenv_sessions(
                &[],
                &[(
                    "/home/u/.env",
                    "PI_CODING_AGENT_DIR=/explicit\nOMP_CODING_AGENT_DIR=/aliased"
                )],
                None
            ),
            Path::new("/aliased/sessions")
        );
    }

    /// Upstream `parseEnvLine`, whose semantics a naive `split('=')` gets wrong
    /// in both directions: each case is a value omp itself would read.
    #[test]
    fn dotenv_line_parsing_matches_upstream_bun_semantics() {
        let got = |line: &str| parse_omp_env_line(line).map(|(k, v)| (k.to_owned(), v));
        let kv = |k: &str, v: &str| Some((k.to_owned(), v.to_owned()));

        assert_eq!(got("A=1"), kv("A", "1"));
        assert_eq!(got("  export  A = 1 "), kv("A", "1"));
        assert_eq!(got("A="), kv("A", ""));
        // `export` needs its separator, or it is simply part of the name.
        assert_eq!(got("exportA=1"), kv("exportA", "1"));
        for skipped in ["", "   ", "# A=1", "NOEQUALS", "1BAD=x", "A-B=x", "=x"] {
            assert_eq!(got(skipped), None, "{skipped:?} is not a dotenv assignment");
        }
        assert_eq!(got("A=hello # trailing"), kv("A", "hello"));
        assert_eq!(got("A=hello#nospace"), kv("A", "hello#nospace"));
        // `trimEnd`: untrimmed, `.pi ` names a directory omp never created.
        assert_eq!(got("A=.pi  # note"), kv("A", ".pi"));
        assert_eq!(got("A=.pi   "), kv("A", ".pi"));
        assert_eq!(got(r#"A="hello # keep""#), kv("A", "hello # keep"));
        assert_eq!(got("A='hello # keep'"), kv("A", "hello # keep"));
        assert_eq!(got("A=`hello # keep`"), kv("A", "hello # keep"));
        // An ESCAPED quote does not close the value, and upstream keeps the slash.
        assert_eq!(got(r#"A="esc\"aped""#), kv("A", r#"esc\"aped"#));
        // An EMPTY quoted value closes at index 0 — nothing precedes it to inspect.
        for empty in [r#"A="""#, "A=''", "A=``"] {
            assert_eq!(got(empty), kv("A", ""), "{empty:?} is an empty value");
        }
        assert_eq!(got(r#"A="unterminated"#), kv("A", "unterminated"));
        // Multi-byte either side of the escape pins the quote-scan index arithmetic.
        assert_eq!(got(r#"A="café\"ﬁn" # x"#), kv("A", r#"café\"ﬁn"#));
        assert_eq!(got("A=café # x"), kv("A", "café"));
        // CRLF: upstream trims the `\r`, our `.lines()` strips it — same value.
        assert_eq!(got("A=1\r"), kv("A", "1"));
        // `isSafeEnvValue`: a NUL would corrupt the C string of a real spawn.
        assert_eq!(got("A=a\0b"), None);
    }

    /// Live-env twin of the injected matrix: proves the PRODUCTION fn reads
    /// the process env, not merely that the pure core computes from injected
    /// values.
    #[test]
    fn omp_sessions_dir_honors_non_empty_env_override() {
        temp_env::with_var("PI_CODING_AGENT_DIR", Some("/custom/agent"), || {
            assert_eq!(
                omp_sessions_dir(),
                PathBuf::from("/custom/agent").join("sessions")
            );
        });

        for blank in ["", "   "] {
            let dflt = temp_env::with_var("PI_CODING_AGENT_DIR", Some(blank), omp_sessions_dir);
            assert!(
                dflt.ends_with(Path::new(".omp/agent/sessions")),
                "blank override {blank:?} → ~/.omp/agent fallback, got {dflt:?}"
            );
        }

        temp_env::with_var_unset("PI_CODING_AGENT_DIR", || {
            assert!(omp_sessions_dir().ends_with(Path::new(".omp/agent/sessions")));
        });
    }

    // -- extension-bridge hook payloads (#951) ------------------------------

    const HOOK_ROOT: &str = "/h/.omp/agent/sessions/-repo/2026-08-31T06-00-52-863Z_01a05668-057f-7559-8fed-f28ff062e3ca.jsonl";
    /// The RAW wire stem, for building inputs; expectations derive through
    /// [`hook_key`] instead — the decoder folds, so a literal expectation
    /// holds on Unix and reds only in `windows-test` (the #520 class).
    const HOOK_ROOT_KEY: &str = "2026-08-31T06-00-52-863Z_01a05668-057f-7559-8fed-f28ff062e3ca";

    /// A raw wire path's session key, through the decoder's own fold.
    fn hook_key(raw: &str) -> String {
        omp_id_from_path(Path::new(&crate::id::normalize_path_key(raw)))
    }

    /// The folded key as an AgentId, the shape every expectation compares.
    fn hook_id(raw: &str) -> AgentId {
        AgentId::from_parts("omp", &hook_key(raw))
    }

    fn hook(v: serde_json::Value) -> Vec<AgentEvent> {
        decode_omp_hook_payload(test_obj(&v)).unwrap()
    }

    #[test]
    fn hook_session_start_registers_with_path_keyed_identity() {
        let evs = hook(serde_json::json!({
            "type": "session_start", "sessionFile": HOOK_ROOT,
            "sessionId": "01a05668-057f-7559-8fed-f28ff062e3ca", "cwd": "/repo",
        }));
        match &evs[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    source,
                    session_id,
                    cwd,
                    parent_id,
                },
                AgentEvent::Identity { pid: None, .. },
            ] => {
                assert_eq!(*agent_id, hook_id(HOOK_ROOT));
                assert_eq!(source, "omp");
                assert_eq!(session_id, &hook_key(HOOK_ROOT));
                assert_eq!(cwd, &std::path::PathBuf::from("/repo"));
                assert_eq!(*parent_id, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hook_session_start_for_a_nested_task_file_links_the_parent() {
        let child = format!("/h/.omp/agent/sessions/-repo/{HOOK_ROOT_KEY}/Alpha.jsonl");
        let evs = hook(serde_json::json!({
            "type": "session_start", "sessionFile": child,
            "sessionId": "01a05668-0f13-7438-82b1-d239cb124270", "cwd": "/repo",
        }));
        match &evs[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    session_id,
                    parent_id,
                    ..
                },
                AgentEvent::Identity { .. },
            ] => {
                let key = hook_key(&child);
                assert_eq!(*agent_id, AgentId::from_parts("omp", &key));
                assert_eq!(session_id, &key);
                assert_eq!(*parent_id, Some(hook_id(HOOK_ROOT)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hook_session_start_without_a_file_keys_on_the_bare_uuid() {
        let evs = hook(serde_json::json!({
            "type": "session_start",
            "sessionId": "01a05668-057f-7559-8fed-f28ff062e3ca", "cwd": "/repo",
        }));
        match &evs[..] {
            [
                AgentEvent::SessionStart {
                    agent_id,
                    session_id,
                    ..
                },
                AgentEvent::Identity { .. },
            ] => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts("omp", "01a05668-057f-7559-8fed-f28ff062e3ca")
                );
                assert_eq!(session_id, "01a05668-057f-7559-8fed-f28ff062e3ca");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hook_shutdown_ends_the_session_and_a_nested_one_as_child() {
        let evs = hook(serde_json::json!({
            "type": "session_shutdown", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
        }));
        assert!(matches!(
            &evs[..],
            [AgentEvent::SessionEnd {
                as_child: false,
                ..
            }]
        ));
        let child = format!("/h/.omp/agent/sessions/-repo/{HOOK_ROOT_KEY}/Alpha.jsonl");
        let evs = hook(serde_json::json!({
            "type": "session_shutdown", "sessionFile": child,
            "sessionId": "x", "cwd": "/repo",
        }));
        assert!(matches!(
            &evs[..],
            [AgentEvent::SessionEnd { as_child: true, .. }]
        ));
    }

    #[test]
    fn hook_switch_ends_the_previous_session_and_starts_the_current() {
        let prev = "/h/.omp/agent/sessions/-repo/2026-08-30T01-00-00-000Z_01a00000-0000-7000-8000-000000000009.jsonl";
        let evs = hook(serde_json::json!({
            "type": "session_switch", "sessionFile": HOOK_ROOT,
            "previousSessionFile": prev,
            "sessionId": "01a05668-057f-7559-8fed-f28ff062e3ca", "cwd": "/repo",
        }));
        match &evs[..] {
            [
                AgentEvent::SessionEnd {
                    agent_id: ended,
                    as_child: false,
                },
                AgentEvent::SessionStart {
                    agent_id: started, ..
                },
                AgentEvent::Identity { .. },
            ] => {
                assert_eq!(*ended, hook_id(prev));
                assert_eq!(*started, hook_id(HOOK_ROOT));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hook_branch_onto_the_same_path_emits_no_end() {
        // An in-place branch REWRITES the file: previous == current.
        let evs = hook(serde_json::json!({
            "type": "session_branch", "sessionFile": HOOK_ROOT,
            "previousSessionFile": HOOK_ROOT,
            "sessionId": "01a05668-057f-7559-8fed-f28ff062e3ca", "cwd": "/repo",
        }));
        assert!(
            matches!(
                &evs[..],
                [AgentEvent::SessionStart { .. }, AgentEvent::Identity { .. }]
            ),
            "same-path branch must not End: {evs:?}"
        );
    }

    #[test]
    fn hook_approval_requested_waits_on_the_named_call_with_identity_ahead() {
        let evs = hook(serde_json::json!({
            "type": "tool_approval_requested", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
            "toolCallId": "call_1", "toolName": "bash",
        }));
        match &evs[..] {
            [
                AgentEvent::Identity { agent_id: iid, .. },
                AgentEvent::UserActionRequest {
                    agent_id,
                    reason: Some(reason),
                    tool_use_id,
                    ..
                },
            ] => {
                assert_eq!(iid, agent_id);
                assert_eq!(*agent_id, hook_id(HOOK_ROOT));
                assert_eq!(reason, "bash", "empty reason falls back to the tool name");
                assert_eq!(tool_use_id.as_deref(), Some("call_1"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        // A wire reason wins over the tool-name fallback.
        let evs = hook(serde_json::json!({
            "type": "tool_approval_requested", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
            "toolCallId": "call_1", "toolName": "bash", "reason": "outside workspace",
        }));
        match &evs[..] {
            [_, AgentEvent::UserActionRequest { reason, .. }] => {
                assert_eq!(reason.as_deref(), Some("outside workspace"))
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hook_approval_resolved_approved_resumes_the_call() {
        let evs = hook(serde_json::json!({
            "type": "tool_approval_resolved", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
            "toolCallId": "call_1", "toolName": "bash", "approved": true,
        }));
        match &evs[..] {
            [
                AgentEvent::Identity { .. },
                AgentEvent::ToolUseStart {
                    agent_id,
                    tool_use_id,
                    detail,
                },
            ] => {
                assert_eq!(*agent_id, hook_id(HOOK_ROOT));
                assert_eq!(tool_use_id.as_deref(), Some("call_1"));
                assert_eq!(detail.as_ref().map(|d| d.display()), Some("bash"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hook_approval_resolved_denied_ends_the_call() {
        let evs = hook(serde_json::json!({
            "type": "tool_approval_resolved", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
            "toolCallId": "call_1", "toolName": "bash", "approved": false,
            "reason": "denied by user",
        }));
        match &evs[..] {
            [
                AgentEvent::Identity { .. },
                AgentEvent::ToolUseEnd {
                    agent_id,
                    tool_use_id,
                },
            ] => {
                assert_eq!(*agent_id, hook_id(HOOK_ROOT));
                assert_eq!(tool_use_id.as_deref(), Some("call_1"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_hook_type_decodes_to_nothing() {
        assert!(
            hook(serde_json::json!({
                "type": "credential_disabled", "sessionFile": HOOK_ROOT,
                "sessionId": "x", "cwd": "/repo",
            }))
            .is_empty()
        );
        assert!(hook(serde_json::json!({"no_type": true})).is_empty());
    }

    #[test]
    fn a_task_approval_resume_carries_the_task_detail() {
        let evs = hook(serde_json::json!({
            "type": "tool_approval_resolved", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
            "toolCallId": "call_t", "toolName": "task", "approved": true,
        }));
        match &evs[..] {
            [_, AgentEvent::ToolUseStart { detail, .. }] => {
                assert!(detail.as_ref().is_some_and(|d| d.is_task()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_stamped_omp_payload_routes_to_this_decoder_through_the_registry() {
        // Red until the registry row carries `HookDecoding::ClaimsAll`: without
        // it the payload falls through to the CC-shaped shared arms, which
        // find no `hook_event_name` and decode nothing.
        let evs = crate::source::decoder::decode_hook_payload(serde_json::json!({
            "_pixtuoid_source": "omp",
            "type": "session_start", "sessionFile": HOOK_ROOT,
            "sessionId": "x", "cwd": "/repo",
        }))
        .unwrap();
        assert!(
            matches!(&evs[..], [AgentEvent::SessionStart { agent_id, .. }, AgentEvent::Identity { .. }]
                if *agent_id == hook_id(HOOK_ROOT)),
            "expected the ClaimsAll route: {evs:?}"
        );
    }

    #[test]
    fn a_raw_windows_hook_path_folds_to_the_watcher_key() {
        // The hook path arrives RAW from the TS extension; the decoder must
        // apply the same seam fold the JSONL watcher applies, or the two
        // transports mint two AgentIds for one session (PR #520 lesson).
        let raw = r"C:\Users\Dev\.omp\agent\sessions\-repo\2026-08-31T06-00-52-863Z_01A05668-057F-7559-8FED-F28FF062E3CA.jsonl";
        let folded = crate::id::normalize_path_key(raw);
        let evs = hook(serde_json::json!({
            "type": "session_start", "sessionFile": raw,
            "sessionId": "x", "cwd": "/repo",
        }));
        match &evs[..] {
            [
                AgentEvent::SessionStart { agent_id, .. },
                AgentEvent::Identity { .. },
            ] => {
                assert_eq!(
                    *agent_id,
                    AgentId::from_parts("omp", &omp_id_from_path(Path::new(&folded)))
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
