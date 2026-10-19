//! The value census: every string literal a decoder compares, indexes or
//! dispatches on is one its source's scenario fixtures hold, one the profile
//! owes (`not_yet_values`), or an [`EXEMPT`] row. A run then carries each one
//! the fixtures of its scenarios hold ([`unreached`]), so a CLI that renamed
//! a value fails though every field path still arrives.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use syn::visit::Visit;

use super::Profile;
use crate::harness::captures::jsonl_files_under;

/// How a decoder matches a literal against what it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Match {
    /// A key or a string value equal to it.
    Exact,
    /// One that starts with it (`starts_with`, `strip_prefix`).
    Prefix,
    /// One that ends with it (`ends_with`, `strip_suffix`).
    Suffix,
    /// One that holds it (`contains`).
    Within,
}

/// One literal a decoder reads, by the module under `src/source` it is in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Literal {
    /// The module's path under `src/source`, `/`-separated.
    pub(crate) module: String,
    /// The const, static or fn it is in.
    pub(crate) item: String,
    pub(crate) text: String,
    pub(crate) how: Match,
}

impl Literal {
    /// Whether `tokens` hold a key or value this literal matches.
    pub(crate) fn in_tokens(&self, tokens: &BTreeSet<String>) -> bool {
        let t = self.text.as_str();
        match self.how {
            Match::Exact => tokens.contains(t),
            Match::Prefix => tokens.iter().any(|k| k.starts_with(t)),
            Match::Suffix => tokens.iter().any(|k| k.ends_with(t)),
            Match::Within => tokens.iter().any(|k| k.contains(t)),
        }
    }

    /// The sources whose wire it is, or why its module is no CLI's.
    ///
    /// # Errors
    ///
    /// If its module is one [`OWNERS`] says no wire reaches, or neither names
    /// a source nor has a row there ([`NO_OWNER`], which the census refuses).
    pub(crate) fn sources(&self) -> Result<Vec<&'static str>, &'static str> {
        let head = self.module.split('/').next().unwrap_or_default();
        let last = self.module.rsplit('/').next().unwrap_or_default();
        let owner = [self.module.as_str(), head, last]
            .into_iter()
            .find_map(|key| OWNERS.iter().find(|(m, _)| *m == key));
        if let Some((_, owner)) = owner {
            return match owner {
                Owner::Of(sources) => Ok(sources.to_vec()),
                Owner::Every => Ok(crate::source::registry::registered_source_names().collect()),
                Owner::Nobody(why) => Err(why),
            };
        }
        let name = head.trim_end_matches(".rs").replace('_', "-");
        crate::source::registry::registered_source_names()
            .find(|s| *s == name)
            .map(|s| vec![s])
            .ok_or(NO_OWNER)
    }
}

/// Why a literal has no source: its module is in no [`OWNERS`] row.
pub(crate) const NO_OWNER: &str = "a module neither named for a source nor in OWNERS";

/// Whose wire a module's literals are, for one not named for its source.
enum Owner {
    /// These sources'.
    Of(&'static [&'static str]),
    /// Every source's: a helper each decoder calls.
    Every,
    /// No CLI's, and why.
    Nobody(&'static str),
}

/// Each module not named for its source, by its path under `src/source`,
/// its first component, or its last.
const OWNERS: &[(&str, Owner)] = &[
    // The ACP standard, which grok speaks (`acp.rs`'s header).
    ("acp.rs", Owner::Of(&["grok"])),
    ("cc_probe.rs", Owner::Of(&["claude-code"])),
    ("decoder.rs", Owner::Every),
    ("mod.rs", Owner::Every),
    (
        "admit.rs",
        Owner::Nobody("the transcript tree's resolver axis (#880)"),
    ),
    (
        "jsonl",
        Owner::Nobody("the transcript tree's resolver axis (#880)"),
    ),
    (
        "native.rs",
        Owner::Nobody("the transcript tree's resolver axis (#880)"),
    ),
    ("drift.rs", Owner::Nobody("the breadcrumb's own log fields")),
    (
        "hook",
        Owner::Nobody("our shim's transport and stamps, which no upstream renames"),
    ),
    (
        "registry.rs",
        Owner::Nobody("the roster: ids, labels, pins and home variables"),
    ),
];

/// The decoder modules' root.
fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source")
}

/// Every literal the production modules under `src/source` read: those the
/// module tree declares outside `#[cfg(test)]`, its `#[cfg(test)]` items
/// skipped.
///
/// # Errors
///
/// If a module cannot be read or does not parse.
pub(crate) fn population() -> std::io::Result<BTreeSet<Literal>> {
    let root = source_root();
    let mut out = BTreeSet::new();
    let mut pending = vec![root.join("mod.rs")];
    while let Some(path) = pending.pop() {
        let module = module_name(&root, &path);
        let file = syn::parse_file(&std::fs::read_to_string(&path)?).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{module}: {e}"))
        })?;
        // `foo.rs`'s children live in `foo/`, `mod.rs`'s beside it.
        let dir = if path.file_name().is_some_and(|n| n == "mod.rs") {
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        } else {
            path.with_extension("")
        };
        for item in &file.items {
            if let syn::Item::Mod(m) = item
                && m.content.is_none()
                && !is_test(&m.attrs)
            {
                let name = m.ident.to_string();
                let flat = dir.join(format!("{name}.rs"));
                pending.push(if flat.exists() {
                    flat
                } else {
                    dir.join(name).join("mod.rs")
                });
            }
        }
        Collect {
            module,
            item: String::new(),
            out: &mut out,
        }
        .visit_file(&file);
    }
    Ok(out)
}

fn module_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether `attrs` hold `#[cfg(test)]`.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.parse_args::<syn::Meta>()
                .is_ok_and(|m| m.path().is_ident("test"))
    })
}

struct Collect<'a> {
    module: String,
    item: String,
    out: &'a mut BTreeSet<Literal>,
}

impl Collect<'_> {
    fn add(&mut self, text: String, how: Match) {
        // A source's own id is ours, stamped by our shim: no upstream renames it.
        if !text.is_empty()
            && !crate::source::registry::registered_source_names().any(|s| s == text)
        {
            self.out.insert(Literal {
                module: self.module.clone(),
                item: self.item.clone(),
                text,
                how,
            });
        }
    }

    /// Visit `f` as the item `name`.
    fn within(&mut self, name: String, f: impl FnOnce(&mut Self)) {
        let outer = std::mem::replace(&mut self.item, name);
        f(self);
        self.item = outer;
    }

    /// The literal `e` is, through `Some(..)`, `&..` and parentheses.
    fn lit(e: &syn::Expr) -> Option<String> {
        match e {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            syn::Expr::Reference(r) => Self::lit(&r.expr),
            syn::Expr::Paren(p) => Self::lit(&p.expr),
            syn::Expr::Call(c)
                if c.args.len() == 1
                    && matches!(&*c.func, syn::Expr::Path(p) if p.path.is_ident("Some")) =>
            {
                Self::lit(&c.args[0])
            }
            _ => None,
        }
    }

    /// Every string literal `e` holds, for a const's value.
    fn lits_in(&mut self, e: &syn::Expr) {
        struct All<'b, 'c>(&'b mut Collect<'c>);
        impl<'ast> Visit<'ast> for All<'_, '_> {
            fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
                self.0.add(s.value(), Match::Exact);
            }
        }
        All(self).visit_expr(e);
    }
}

impl<'ast> Visit<'ast> for Collect<'_> {
    fn visit_item(&mut self, i: &'ast syn::Item) {
        let attrs = match i {
            syn::Item::Const(c) => &c.attrs,
            syn::Item::Fn(f) => &f.attrs,
            syn::Item::Mod(m) => &m.attrs,
            syn::Item::Impl(m) => &m.attrs,
            syn::Item::Static(s) => &s.attrs,
            _ => return syn::visit::visit_item(self, i),
        };
        if !is_test(attrs) {
            syn::visit::visit_item(self, i);
        }
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if !is_test(&f.attrs) {
            self.within(f.sig.ident.to_string(), |v| {
                syn::visit::visit_impl_item_fn(v, f);
            });
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.within(f.sig.ident.to_string(), |v| syn::visit::visit_item_fn(v, f));
    }

    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        self.within(c.ident.to_string(), |v| v.lits_in(&c.expr));
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        self.within(s.ident.to_string(), |v| v.lits_in(&s.expr));
    }

    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        if matches!(b.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) {
            for side in [&*b.left, &*b.right] {
                if let Some(s) = Self::lit(side) {
                    self.add(s, Match::Exact);
                }
            }
        }
        syn::visit::visit_expr_binary(self, b);
    }

    fn visit_expr_index(&mut self, i: &'ast syn::ExprIndex) {
        if let Some(s) = Self::lit(&i.index) {
            self.add(s, Match::Exact);
        }
        syn::visit::visit_expr_index(self, i);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let first = m.args.first().and_then(Self::lit);
        match (m.method.to_string().as_str(), first) {
            ("get" | "get_mut" | "contains_key" | "remove", Some(s)) => self.add(s, Match::Exact),
            ("starts_with" | "strip_prefix", Some(s)) => self.add(s, Match::Prefix),
            ("ends_with" | "strip_suffix", Some(s)) => self.add(s, Match::Suffix),
            ("contains", Some(s)) => self.add(s, Match::Within),
            ("pointer" | "pointer_mut", Some(s)) => {
                for seg in s.split('/') {
                    self.add(seg.to_owned(), Match::Exact);
                }
            }
            _ => {}
        }
        syn::visit::visit_expr_method_call(self, m);
    }

    fn visit_pat(&mut self, p: &'ast syn::Pat) {
        if let syn::Pat::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = p
        {
            self.add(s.value(), Match::Exact);
        }
        syn::visit::visit_pat(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        // `matches!(expr, pat)`: its pattern is a dispatch like a match arm's.
        if m.path.is_ident("matches") {
            let parsed = m.parse_body_with(|input: syn::parse::ParseStream<'_>| {
                let e: syn::Expr = input.parse()?;
                input.parse::<syn::Token![,]>()?;
                let p = syn::Pat::parse_multi_with_leading_vert(input)?;
                // A guard or a trailing comma: not a pattern.
                input.step(|c| {
                    let mut rest = *c;
                    while let Some((_, next)) = rest.token_tree() {
                        rest = next;
                    }
                    Ok(((), rest))
                })?;
                Ok((e, p))
            });
            if let Ok((e, p)) = parsed {
                self.visit_expr(&e);
                self.visit_pat(&p);
            }
        }
    }
}

/// Every key, chain of nested keys (`location.directory`) and string value in
/// `v`, a string that holds JSON descended too (codex's `arguments`).
pub(crate) fn tokens(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                chains(k.clone(), x, out);
                tokens(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| tokens(x, out)),
        Value::String(s) => {
            if s.starts_with(['{', '['])
                && let Ok(inner) = serde_json::from_str::<Value>(s)
            {
                tokens(&inner, out);
            }
            out.insert(s.clone());
        }
        _ => {}
    }
}

/// `key` and each chain of keys below it, `.`-joined.
fn chains(key: String, v: &Value, out: &mut BTreeSet<String>) {
    if let Value::Object(m) = v {
        for (k, x) in m {
            chains(format!("{key}.{k}"), x, out);
        }
    }
    out.insert(key);
}

/// Every token the `.jsonl` lines under `dir` hold.
///
/// # Errors
///
/// If a file cannot be read.
pub(crate) fn tokens_under(dir: &Path, out: &mut BTreeSet<String>) -> std::io::Result<()> {
    for file in jsonl_files_under(dir) {
        for line in std::fs::read_to_string(file)?.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                tokens(&v, out);
            }
        }
    }
    Ok(())
}

/// The literals of `population` that are `source`'s wire.
pub(crate) fn of_source<'a>(
    population: &'a BTreeSet<Literal>,
    source: &'a str,
) -> impl Iterator<Item = &'a Literal> {
    population
        .iter()
        .filter(move |l| l.sources().is_ok_and(|s| s.contains(&source)))
}

/// What a source's runs owe: each literal of `population` its committed
/// fixtures hold that no run carried, and each `not_yet_values` one a run did.
pub(crate) fn unreached(
    p: &Profile,
    population: &BTreeSet<Literal>,
    committed: &BTreeSet<String>,
    carried: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut missing = Vec::new();
    let mut stale = Vec::new();
    for l in of_source(population, p.source) {
        if l.in_tokens(committed) && !l.in_tokens(carried) {
            missing.push(format!(
                "`{}` ({}): a decoder reads it, a committed capture of {} has it, \
                 and no run of {} carried it",
                l.text, l.module, p.source, p.source
            ));
        }
    }
    for owed in p.not_yet_values {
        if of_source(population, p.source).any(|l| l.text == *owed && l.in_tokens(carried)) {
            stale.push(format!(
                "`{owed}` was carried: it leaves the profile's not_yet_values"
            ));
        }
    }
    (missing, stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::hermetic::{NOT_HERMETIC, profile};
    use crate::harness::sources_root;

    /// Literals no fixture need hold, as `(module, literal or the item that
    /// holds it, why)`.
    const EXEMPT: &[(&str, &str, &str)] = &[
        ("antigravity.rs", "CLI_APP_DATA_DIR", PATH_AXIS),
        ("antigravity.rs", "TRANSCRIPT", PATH_AXIS),
        ("antigravity.rs", "KEYS", LABEL_DETAIL),
        (
            "antigravity.rs",
            "decode_ag_line",
            "no upstream to record against: the decoder breadcrumbs unknown vocabulary",
        ),
        (
            "antigravity.rs",
            "decode_ag_tool_call",
            "unverified reverse-engineered tool names, silent on a rename: accepted (#943)",
        ),
        ("cc_probe.rs", "cc_sessions_dir", PATH_AXIS),
        ("cc_probe.rs", "live_cc_session_ids", PATH_AXIS),
        (
            "cc_probe.rs",
            "parse_registry_entry",
            "CC's session registry file, which no capture records",
        ),
        ("claude_code.rs", "admits_transcript", PATH_AXIS),
        ("claude_code.rs", "subagent_session", PATH_AXIS),
        ("claude_code.rs", "ACTIVITY_TYPES", TAIL_READ),
        ("claude_code.rs", "cc_session_ended", TAIL_READ),
        (
            "claude_code.rs",
            "<synthetic>",
            "CC's placeholder model: a rename shows a cosmetic label until a real model arrives",
        ),
        (
            "claude_code.rs",
            "ULTRA_EXIT_LABEL",
            "a label we write, not one CC sends",
        ),
        (
            "copilot.rs",
            "tokenDetails",
            "loss list item 3 (owner, 2026-10-19): only GitHub's billing response carries it",
        ),
        ("copilot.rs", "KEYS", LABEL_DETAIL),
        (
            "decoder.rs",
            "decodes_to_nothing",
            "cursor's, which runs in no hermetic tier (owner, 2026-10-13)",
        ),
        ("decoder.rs", "describe_tool_target", LABEL_DETAIL),
        ("decoder.rs", "SUBAGENTS_DIR", PATH_AXIS),
        ("grok.rs", "is_updates_jsonl", PATH_AXIS),
        ("grok.rs", "KEYS", LABEL_DETAIL),
        (
            "grok.rs",
            "XAI_MODEL_AUTO_SWITCHED",
            "loss list item 3 (owner, 2026-10-13): only a provider fallback sends it",
        ),
        ("hermes.rs", "KEYS", LABEL_DETAIL),
        ("omp.rs", "AGENT_SUBDIR", PATH_AXIS),
        ("omp.rs", "EXTENSIONS_SUBDIR", PATH_AXIS),
        ("omp.rs", "PROFILES_SUBDIR", PATH_AXIS),
        ("omp.rs", "stem_chain", PATH_AXIS),
        ("omp.rs", "normalize_profile_name", PATH_AXIS),
        ("omp.rs", "parse_omp_env_line", PATH_AXIS),
        ("omp.rs", "with_omp_dotenv", PATH_AXIS),
        ("omp.rs", "KEYS", LABEL_DETAIL),
        ("reasonix.rs", "KEYS", LABEL_DETAIL),
    ];

    /// Why a path or environment name needs no fixture.
    const PATH_AXIS: &str = "the transcript tree's resolver axis (#880)";
    /// Why a tool-argument name a label shows needs no fixture.
    const LABEL_DETAIL: &str = "a label's detail: a rename shows the bare tool name";
    /// Why a literal the tail scan reads needs no fixture.
    const TAIL_READ: &str = "read off the tail, outside the decode the strip probes, \
         so a fixture's line holds it blanked";

    /// The tokens `p`'s scenario fixtures hold, each a recording `--record` wrote.
    ///
    /// # Errors
    ///
    /// If a fixture file cannot be read.
    fn fixture_tokens(p: &Profile) -> std::io::Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for s in p.scenarios {
            tokens_under(&sources_root().join(s.fixture), &mut out)?;
        }
        Ok(out)
    }

    /// Whether `p` owes `text` elsewhere than `not_yet_values`: a hook its
    /// `not_yet` names, or a kind or field its `not_yet_fields` names, each census
    /// of which shrinks its own list.
    fn owed_elsewhere(p: &Profile, text: &str) -> bool {
        p.not_yet.contains(&text)
            || p.not_yet_fields.iter().any(|(kind, path)| {
                kind.split_once(':').is_some_and(|(_, k)| k == text)
                    || path.rsplit('/').next() == Some(text)
            })
    }

    /// Every problem with the census at rest: a literal no fixture holds, owed or
    /// exempted; a `not_yet_values` entry a fixture now holds or no decoder reads;
    /// and an `EXEMPT` row that excuses nothing.
    ///
    /// # Errors
    ///
    /// If the decoders or a fixture cannot be read.
    fn census_problems() -> std::io::Result<Vec<String>> {
        let population = population()?;
        let mut held = std::collections::BTreeMap::new();
        for p in crate::harness::hermetic::PROFILES {
            held.insert(p.source, fixture_tokens(p)?);
        }
        let absent: BTreeSet<&str> = NOT_HERMETIC.iter().map(|(s, _)| *s).collect();
        let mut problems = Vec::new();
        let mut excused = BTreeSet::new();
        let mut read_owed = BTreeSet::new();
        for l in &population {
            let sources = match l.sources() {
                Ok(sources) => sources,
                Err(NO_OWNER) => {
                    problems.push(format!("{}: {NO_OWNER}", l.module));
                    continue;
                }
                Err(_) => continue,
            };
            let profiles: Vec<&Profile> = sources.iter().filter_map(|s| profile(s)).collect();
            let reached = sources
                .iter()
                .any(|s| held.get(s).is_some_and(|t| l.in_tokens(t)));
            for p in &profiles {
                if p.not_yet_values.contains(&l.text.as_str()) {
                    read_owed.insert((p.source, l.text.clone()));
                    if held.get(p.source).is_some_and(|t| l.in_tokens(t)) {
                        problems.push(format!(
                            "{}: `{}` is in a fixture: it leaves not_yet_values",
                            p.source, l.text
                        ));
                    }
                }
            }
            if reached {
                continue;
            }
            let owed = profiles
                .iter()
                .any(|p| p.not_yet_values.contains(&l.text.as_str()) || owed_elsewhere(p, &l.text));
            let exempt = EXEMPT
                .iter()
                .find(|(m, key, _)| *m == l.module && (*key == l.text || *key == l.item));
            if let Some((m, key, _)) = exempt {
                excused.insert((*m, *key));
            }
            if !owed && exempt.is_none() && !sources.iter().all(|s| absent.contains(s)) {
                problems.push(format!(
                    "{} {}: `{}` ({:?}) is in no fixture of {}: record a scenario that \
                     reaches it, owe it in not_yet_values, or exempt it",
                    l.module,
                    l.item,
                    l.text,
                    l.how,
                    sources.join("/")
                ));
            }
        }
        for p in crate::harness::hermetic::PROFILES {
            for owed in p.not_yet_values {
                if !read_owed.contains(&(p.source, (*owed).to_owned())) {
                    problems.push(format!(
                        "{}: not_yet_values names `{owed}`, which no decoder of it reads",
                        p.source
                    ));
                }
            }
        }
        for (m, key, _) in EXEMPT {
            if !excused.contains(&(*m, *key)) {
                problems.push(format!(
                    "{m}: EXEMPT names `{key}`, which excuses no literal a fixture lacks"
                ));
            }
        }
        Ok(problems)
    }

    #[test]
    fn every_literal_a_decoder_reads_is_recorded_owed_or_exempt() {
        let problems = census_problems().expect("the decoders and fixtures read");
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
