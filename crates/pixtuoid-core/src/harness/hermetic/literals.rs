//! The literal census: every string a decoder compares, indexes, matches or
//! holds in a const is one its source's scenario fixtures hold, one its profile
//! owes (`not_yet_literals`, or its `not_yet` / `not_yet_fields`), or an
//! exemption with its reason. `verify::verify_source` then needs each value a
//! fixture holds to arrive at its kind and pointer, and each `Exact` key in
//! some run, so a CLI that renamed either fails though every path arrives.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

#[cfg(test)]
use super::Profile;

/// How a decoder matches a literal against what it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Match {
    /// A key or a string value equal to it.
    Exact,
    /// One that starts with it (`starts_with`, `strip_prefix`).
    Prefix,
    /// One that ends with it (`ends_with`, `strip_suffix`).
    Suffix,
    /// One that holds it (`contains`, `split_once`).
    Within,
}

impl Match {
    /// The mode a method that takes the literal reads it in.
    fn of_method(method: &str) -> Option<Self> {
        Some(match method {
            "get" | "get_mut" | "contains_key" | "remove" => Self::Exact,
            "starts_with" | "strip_prefix" => Self::Prefix,
            "ends_with" | "strip_suffix" => Self::Suffix,
            "contains" | "split_once" => Self::Within,
            _ => return None,
        })
    }
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
    /// Whether `s`, a key or a string value, is one this literal matches.
    pub(crate) fn matches(&self, s: &str) -> bool {
        let t = self.text.as_str();
        match self.how {
            Match::Exact => s == t,
            Match::Prefix => s.starts_with(t),
            Match::Suffix => s.ends_with(t),
            Match::Within => s.contains(t),
        }
    }

    /// Whose wire it is.
    pub(crate) fn owner(&self) -> Owner {
        if let Some((_, owner)) = OWNERS.iter().find(|(m, _)| *m == self.module) {
            return *owner;
        }
        let head = self.module.split('/').next().unwrap_or_default();
        let name = head.trim_end_matches(".rs").replace('_', "-");
        crate::source::registry::registered_source_names()
            .find(|s| *s == name)
            .map_or(Owner::Unowned, Owner::Source)
    }

    /// Whether it is `source`'s wire.
    pub(crate) fn is_of(&self, source: &str) -> bool {
        self.owner().sources().is_some_and(|s| s.contains(&source))
    }
}

/// Whose wire a module's literals are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    /// The source whose wire it is.
    Source(&'static str),
    /// Every source's: a helper each decoder calls.
    Every,
    /// No CLI's, and why.
    Nobody(&'static str),
    /// Neither named for a source nor in [`OWNERS`], which the census refuses.
    Unowned,
}

impl Owner {
    /// The sources whose wire it is; `None` for a module no wire reaches.
    pub(crate) fn sources(self) -> Option<Vec<&'static str>> {
        match self {
            Self::Source(s) => Some(vec![s]),
            Self::Every => Some(crate::source::registry::registered_source_names().collect()),
            Self::Nobody(_) | Self::Unowned => None,
        }
    }
}

/// Each module not named for its source, by its path under `src/source`.
const OWNERS: &[(&str, Owner)] = &[
    // The ACP standard, which grok speaks (`acp.rs`'s header).
    ("acp.rs", Owner::Source("grok")),
    ("cc_probe.rs", Owner::Source("claude-code")),
    ("decoder.rs", Owner::Every),
    ("mod.rs", Owner::Every),
    ("admit.rs", Owner::Nobody(PATH_AXIS)),
    ("jsonl/mod.rs", Owner::Nobody(PATH_AXIS)),
    ("jsonl/health.rs", Owner::Nobody(PATH_AXIS)),
    ("jsonl/liveness.rs", Owner::Nobody(PATH_AXIS)),
    ("jsonl/unclaim.rs", Owner::Nobody(PATH_AXIS)),
    ("jsonl/walk.rs", Owner::Nobody(PATH_AXIS)),
    ("drift.rs", Owner::Nobody("the breadcrumb's own log fields")),
    ("hook/mod.rs", Owner::Nobody(OUR_SHIM)),
    ("hook/router.rs", Owner::Nobody(OUR_SHIM)),
    ("hook/unix.rs", Owner::Nobody(OUR_SHIM)),
    ("hook/windows.rs", Owner::Nobody(OUR_SHIM)),
    ("hook/pid_watch.rs", Owner::Nobody(OUR_SHIM)),
    (
        "registry.rs",
        Owner::Nobody("the roster: ids, labels, pins and home variables"),
    ),
];

/// Why a path or environment name needs no fixture.
pub(crate) const PATH_AXIS: &str = "the transcript tree's resolver axis (#880)";
/// Why our own shim's names need none: no upstream renames them.
const OUR_SHIM: &str = "our shim's transport and stamps";

/// The decoder modules' root.
fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source")
}

/// The text of each [`Match::Exact`] literal `source`'s own modules read, not
/// a helper every decoder shares.
///
/// # Errors
///
/// If [`population`] fails.
pub(crate) fn own_exact(source: &str) -> std::io::Result<BTreeSet<String>> {
    Ok(population()?
        .into_iter()
        .filter(|l| l.how == Match::Exact && matches!(l.owner(), Owner::Source(s) if s == source))
        .map(|l| l.text)
        .collect())
}

/// Every literal the modules under `src/source` read, walked from `mod.rs`
/// through each `mod` they declare, whatever its platform or feature cfg;
/// test-only items skipped.
///
/// # Errors
///
/// If a module cannot be read or does not parse.
pub(crate) fn population() -> std::io::Result<BTreeSet<Literal>> {
    let root = source_root();
    let mut out = BTreeSet::new();
    let mut pending = vec![root.join("mod.rs")];
    while let Some(path) = pending.pop() {
        let module = crate::harness::slashed_under(&root, &path)?;
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
        out.extend(collect(&module, &file));
    }
    Ok(out)
}

/// Whether `attrs` hold a `cfg` only a test build meets: `test`, or an
/// `all(..)` that requires it. `any(test, ..)` is not: the harness compiles it.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    fn requires_test(m: &syn::Meta) -> bool {
        match m {
            syn::Meta::Path(p) => p.is_ident("test"),
            syn::Meta::List(l) if l.path.is_ident("all") => l
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|ms| ms.iter().any(requires_test)),
            _ => false,
        }
    }
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| requires_test(&m))
    })
}

/// The literals `file`, the module `module`, reads, each in the mode
/// [`Collect::resolve`] gives it.
pub(crate) fn collect(module: &str, file: &syn::File) -> BTreeSet<Literal> {
    let mut c = Collect {
        module: module.to_owned(),
        item: String::new(),
        out: BTreeSet::new(),
        uses: BTreeSet::new(),
        bound: Vec::new(),
        inline: Vec::new(),
    };
    c.visit_file(file);
    c.resolve()
}

struct Collect {
    module: String,
    item: String,
    out: BTreeSet<Literal>,
    /// Each const or inline array a method reads in a mode other than
    /// [`Match::Exact`].
    uses: BTreeSet<(String, Match)>,
    /// Closure parameters bound to the const or inline array whose members
    /// they iterate.
    bound: Vec<(String, String)>,
    /// Each inline array a method chain iterates (`[..].iter()`), as its
    /// `[..]#n` name, the item it sits in and its string members.
    inline: Vec<(String, String, Vec<String>)>,
}

impl Collect {
    /// Each const's and inline array's string members in every mode `uses`
    /// holds for them, else [`Match::Exact`]; every other literal as the walk
    /// added it.
    fn resolve(mut self) -> BTreeSet<Literal> {
        let modes = |uses: &BTreeSet<(String, Match)>, of: &str| {
            let m: Vec<Match> = uses
                .iter()
                .filter(|(t, _)| t == of)
                .map(|(_, how)| *how)
                .collect();
            if m.is_empty() { vec![Match::Exact] } else { m }
        };
        let targets: BTreeSet<String> = self.uses.iter().map(|(t, _)| t.clone()).collect();
        for name in targets {
            let members: Vec<Literal> = self
                .out
                .iter()
                .filter(|l| l.item == name && l.how == Match::Exact)
                .cloned()
                .collect();
            for l in members {
                self.out.remove(&l);
                for how in modes(&self.uses, &name) {
                    self.out.insert(Literal { how, ..l.clone() });
                }
            }
        }
        for (name, item, texts) in std::mem::take(&mut self.inline) {
            self.item = item;
            for how in modes(&self.uses, &name) {
                for text in &texts {
                    self.add(text.clone(), how);
                }
            }
        }
        self.out
    }

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

    /// The literal `e` is, through `Some(..)`, `&..`, `*..` and parentheses.
    fn lit(e: &syn::Expr) -> Option<String> {
        match e {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            syn::Expr::Reference(r) => Self::lit(&r.expr),
            syn::Expr::Paren(p) => Self::lit(&p.expr),
            syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => Self::lit(&u.expr),
            syn::Expr::Call(c)
                if c.args.len() == 1
                    && matches!(&*c.func, syn::Expr::Path(p) if p.path.is_ident("Some")) =>
            {
                Self::lit(&c.args[0])
            }
            _ => None,
        }
    }

    /// The const `e` names, directly or through a closure parameter bound to
    /// one.
    fn const_of(&self, e: &syn::Expr) -> Option<String> {
        let e = match e {
            syn::Expr::Reference(r) => &*r.expr,
            e => e,
        };
        let syn::Expr::Path(p) = e else { return None };
        let name = p.path.get_ident()?.to_string();
        if let Some((_, c)) = self.bound.iter().rev().find(|(param, _)| *param == name) {
            return Some(c.clone());
        }
        is_const_ident(&name).then_some(name)
    }

    /// Every string literal `e` holds, for a const's value.
    fn lits_in(&mut self, e: &syn::Expr) {
        struct All<'b>(&'b mut Collect);
        impl<'ast> Visit<'ast> for All<'_> {
            fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
                self.0.add(s.value(), Match::Exact);
            }
        }
        All(self).visit_expr(e);
    }
}

impl<'ast> Visit<'ast> for Collect {
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

    /// A string a local accessor takes (`field(obj, "id")`, a closure's
    /// `n("inputTokens")`) is a key it reads. A path call's is not: a
    /// breadcrumb (`drift::missing_field`) or an error only repeats the key.
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if matches!(&*c.func, syn::Expr::Path(p) if p.path.get_ident().is_some_and(|i| i != "Some"))
        {
            for arg in &c.args {
                if let Some(s) = Self::lit(arg) {
                    self.add(s, Match::Exact);
                }
            }
        }
        syn::visit::visit_expr_call(self, c);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let method = m.method.to_string();
        if let Some(how) = Match::of_method(&method)
            && let Some(first) = m.args.first()
        {
            if let Some(s) = Self::lit(first) {
                self.add(s, how);
            } else if how != Match::Exact
                && let Some(c) = self.const_of(first)
            {
                self.uses.insert((c, how));
            }
        }
        if matches!(method.as_str(), "pointer" | "pointer_mut")
            && let Some(s) = m.args.first().and_then(Self::lit)
        {
            for seg in s.split('/') {
                self.add(seg.to_owned(), Match::Exact);
            }
        }
        // `ROLES.iter().any(|r| s.ends_with(r))`: `r` stands for each member,
        // and so it does for an inline array's (`["a", "b"].iter()`).
        let iterated = match &*m.receiver {
            syn::Expr::MethodCall(inner) if inner.method == "iter" => match &*inner.receiver {
                syn::Expr::Array(a) => {
                    let name = format!("[..]#{}", self.inline.len());
                    let texts = a.elems.iter().filter_map(Self::lit).collect();
                    self.inline.push((name.clone(), self.item.clone(), texts));
                    Some(name)
                }
                r => self.const_of(r),
            },
            _ => None,
        };
        self.visit_expr(&m.receiver);
        for arg in &m.args {
            let param = match (arg, &iterated) {
                (syn::Expr::Closure(c), Some(target)) => match c.inputs.first() {
                    Some(syn::Pat::Ident(p)) => Some((p.ident.to_string(), target.clone())),
                    _ => None,
                },
                _ => None,
            };
            let pushed = param.is_some();
            self.bound.extend(param);
            self.visit_expr(arg);
            if pushed {
                self.bound.pop();
            }
        }
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
        } else if let Ok(args) = m.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        ) {
            // `Ok(vec![..])` and its kin: their arguments are code like any
            // other. A logging or format macro's message sits in no read.
            for arg in &args {
                self.visit_expr(arg);
            }
        }
    }
}

/// What one source owes at rest, as its profile's three lists say.
#[cfg(test)]
struct Owing<'a> {
    source: &'a str,
    literals: &'a [&'a str],
    hooks: &'a [&'a str],
    fields: &'a [(&'a str, &'a str)],
}

#[cfg(test)]
impl<'a> Owing<'a> {
    fn of(p: &'a Profile) -> Self {
        Self {
            source: p.source,
            literals: p.not_yet_literals,
            hooks: p.not_yet,
            fields: p.not_yet_fields,
        }
    }

    /// Whether any of the three lists owes `text`.
    fn owes(&self, text: &str) -> bool {
        self.literals.contains(&text) || self.elsewhere(text)
    }

    /// Whether `not_yet` or `not_yet_fields` owes `text`, each census of which
    /// shrinks its own list.
    fn elsewhere(&self, text: &str) -> bool {
        self.hooks.iter().any(|h| one_hook(h, text))
            || self.fields.iter().any(|(kind, path)| {
                kind.split_once(':').is_some_and(|(_, k)| k == text)
                    || path.split('/').any(|s| s == text)
            })
    }
}

/// Whether `a` and `b` name one hook in two spellings: `not_yet`'s
/// (`PostToolUseFailure`) and a decoder's wire one (`post_tool_use_failure`).
#[cfg(test)]
fn one_hook(a: &str, b: &str) -> bool {
    let spelled = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    spelled(a) == spelled(b)
}

/// Whether `s` is spelled as a const is (`SCREAMING_SNAKE`).
fn is_const_ident(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
}

/// What a `syn::File`'s `fn name` dispatches on in its first `match on`: the
/// consts its arms name, and any string literal an arm or its guard spells.
#[cfg(test)]
fn dispatch_arms(file: &syn::File, name: &str, on: &str) -> (BTreeSet<String>, Vec<String>) {
    struct Arms<'n> {
        on: &'n str,
        consts: BTreeSet<String>,
        bare: Vec<String>,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Arms<'_> {
        fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
            let scrutinee = matches!(&*m.expr, syn::Expr::Path(p) if p.path.is_ident(self.on));
            if !scrutinee || std::mem::replace(&mut self.found, true) {
                return syn::visit::visit_expr_match(self, m);
            }
            for arm in &m.arms {
                struct Pats<'b, 'n>(&'b mut Arms<'n>);
                impl<'ast> Visit<'ast> for Pats<'_, '_> {
                    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
                        self.0.consts.insert(p.ident.to_string());
                    }
                    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
                        if let Some(i) = p.path.get_ident() {
                            self.0.consts.insert(i.to_string());
                        }
                    }
                    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
                        self.0.bare.push(s.value());
                    }
                }
                // The pattern holds its guard, whose literal is a bare one too.
                Pats(self).visit_pat(&arm.pat);
            }
        }
    }
    let f = file
        .items
        .iter()
        .find_map(|i| match i {
            syn::Item::Fn(f) if f.sig.ident == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fn {name}"));
    let mut arms = Arms {
        on,
        consts: BTreeSet::new(),
        bare: Vec::new(),
        found: false,
    };
    arms.visit_block(&f.block);
    arms.consts.retain(|c| is_const_ident(c));
    (arms.consts, arms.bare)
}

/// The const idents `file`'s `const name` lists.
#[cfg(test)]
fn listed(file: &syn::File, name: &str) -> BTreeSet<String> {
    struct Idents(BTreeSet<String>);
    impl<'ast> Visit<'ast> for Idents {
        fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
            if let Some(i) = p.path.get_ident() {
                self.0.insert(i.to_string());
            }
        }
    }
    let c = file
        .items
        .iter()
        .find_map(|i| match i {
            syn::Item::Const(c) if c.ident == name => Some(c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no const {name}"));
    let mut ids = Idents(BTreeSet::new());
    ids.visit_expr(&c.expr);
    ids.0
}

/// One literal a fixture need not hold, as its module, what it names and why.
#[cfg(test)]
pub(crate) struct Exempt {
    module: &'static str,
    key: Key,
    why: &'static str,
}

/// What an [`Exempt`] row excuses.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// One literal.
    Literal(&'static str),
    /// One literal on the item and arm of `of`, which a fixture must hold or
    /// a list owe.
    Sibling {
        text: &'static str,
        of: &'static str,
    },
    /// Every literal an item holds that no fixture does.
    Item(&'static str),
}

/// Every problem with the census at rest, over `population` and each source's
/// `held` keys and values: a literal no fixture holds, owed or exempted; a
/// `not_yet_literals` entry a fixture holds, no decoder reads, or another list
/// owes; an [`Exempt`] row that excuses nothing or whose sibling nothing
/// holds or owes; and a module with no owner.
#[cfg(test)]
fn problems(
    population: &BTreeSet<Literal>,
    held: &BTreeMap<&str, BTreeSet<String>>,
    owing: &[Owing<'_>],
    exempt: &[Exempt],
    absent: &BTreeSet<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut excused = BTreeSet::new();
    let mut read_owed = BTreeSet::new();
    let holds = |source: &str, l: &Literal| {
        held.get(source)
            .is_some_and(|t| t.iter().any(|s| l.matches(s)))
    };
    for l in population {
        let owner = l.owner();
        if owner == Owner::Unowned {
            out.push(format!("{}: a module with no owner (`OWNERS`)", l.module));
            continue;
        }
        let Some(sources) = owner.sources() else {
            continue;
        };
        let mine: Vec<&Owing<'_>> = owing
            .iter()
            .filter(|o| sources.contains(&o.source))
            .collect();
        for o in &mine {
            if o.literals.contains(&l.text.as_str()) {
                read_owed.insert((o.source, l.text.clone()));
                if holds(o.source, l) {
                    out.push(format!(
                        "{}: `{}` is in a fixture: it leaves not_yet_literals",
                        o.source, l.text
                    ));
                }
            }
        }
        if sources.iter().any(|s| holds(s, l)) {
            continue;
        }
        let row = exempt.iter().find(|e| {
            e.module == l.module
                && match e.key {
                    Key::Literal(t) | Key::Sibling { text: t, .. } => t == l.text,
                    Key::Item(i) => i == l.item,
                }
        });
        if let Some(e) = row {
            excused.insert((e.module, e.key));
        }
        if let Some(Key::Sibling { of, .. }) = row.map(|e| e.key) {
            let warranted = population.iter().any(|w| {
                w.module == l.module
                    && w.item == l.item
                    && w.text == of
                    && (sources.iter().any(|s| holds(s, w)) || mine.iter().any(|o| o.owes(of)))
            });
            if !warranted {
                out.push(format!(
                    "{} {}: `{}` is exempt as a sibling of `{of}`, which no fixture of {} \
                     holds there and no list owes",
                    l.module,
                    l.item,
                    l.text,
                    sources.join("/")
                ));
            }
        }
        let owed = mine.iter().any(|o| o.owes(&l.text));
        if !owed && row.is_none() && !sources.iter().all(|s| absent.contains(s)) {
            out.push(format!(
                "{} {}: `{}` ({:?}) is in no fixture of {}: record a scenario that \
                 reaches it, owe it in not_yet_literals, or exempt it",
                l.module,
                l.item,
                l.text,
                l.how,
                sources.join("/")
            ));
        }
    }
    for o in owing {
        for owed in o.literals {
            if o.elsewhere(owed) {
                out.push(format!(
                    "{}: not_yet_literals names `{owed}`, which another owed list owes",
                    o.source
                ));
            } else if !read_owed.contains(&(o.source, (*owed).to_owned())) {
                out.push(format!(
                    "{}: not_yet_literals names `{owed}`, which no decoder of it reads",
                    o.source
                ));
            }
        }
    }
    for e in exempt {
        if !excused.contains(&(e.module, e.key)) {
            out.push(format!(
                "{}: the exemption of {:?} ({}) excuses no literal a fixture lacks",
                e.module, e.key, e.why
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::hermetic::{NOT_HERMETIC, PROFILES};

    /// Why a tool-argument name a label shows needs no fixture.
    const LABEL_DETAIL: &str = "a label's detail: a rename shows the bare tool name";
    /// Why a value an arm matches only to decode it to nothing needs no
    /// fixture: a rename breadcrumbs, and no state hangs on it.
    const IGNORE_ARM: &str = "an ignore arm: it decodes to nothing either way";
    // The loss list the owner signed on 2026-10-19 ("全部签字"): a literal no
    // scenario can reach, each for the reason its const names. A scenario that
    // does reach one makes its row excuse nothing, which the census refuses.
    const LOSS_APPROVE: &str = "loss list: the tier only refuses, so no run approves";
    const LOSS_SIBLING: &str =
        "loss list: a sibling on one arm of a name a scenario reaches or its profile owes";
    const LOSS_LEGACY: &str = "loss list: only a Legacy-history codex rollout carries it, \
         which only a thread store without paginated lists gets, as `EM_RESUME` cites; \
         the tier's rollouts are Paginated";
    const LOSS_NO_EMITTER: &str = "loss list: codex writes it only for a model's tool_search \
         call (`codex-rs/core/src/tools/spec_plan.rs:392-399` at rust-v0.163.0), which no \
         aimock fixture block makes (`src/types.ts:423-435` at v1.45.1)";
    const LOSS_CACHE_WRITE: &str = "loss list: aimock's Messages usage carries no cache \
         writes, as claude-code's `cache_creation_input_tokens` row cites";
    /// Why agy's input steps need none: their line decodes to nothing, so the
    /// strip blanks it whole, and renamed, one only ends a call no step began.
    const AG_INPUT_STEP: &str = "an agy input step: its line decodes to nothing either way";
    /// Why a literal the tail scan reads needs no fixture.
    const TAIL_READ: &str = "read off the tail, outside the decode the strip probes, \
         so a fixture's line holds it blanked";

    const fn item(module: &'static str, name: &'static str, why: &'static str) -> Exempt {
        Exempt {
            module,
            key: Key::Item(name),
            why,
        }
    }

    const fn literal(module: &'static str, text: &'static str, why: &'static str) -> Exempt {
        Exempt {
            module,
            key: Key::Literal(text),
            why,
        }
    }

    const fn sibling(module: &'static str, text: &'static str, of: &'static str) -> Exempt {
        Exempt {
            module,
            key: Key::Sibling { text, of },
            why: LOSS_SIBLING,
        }
    }

    const EXEMPT: &[Exempt] = &[
        item("antigravity.rs", "CLI_APP_DATA_DIR", PATH_AXIS),
        item("antigravity.rs", "TRANSCRIPT", PATH_AXIS),
        item("antigravity.rs", "KEYS", LABEL_DETAIL),
        item("cc_probe.rs", "cc_sessions_dir", PATH_AXIS),
        item("cc_probe.rs", "live_cc_session_ids", PATH_AXIS),
        item(
            "cc_probe.rs",
            "parse_registry_entry",
            "CC's session registry file, which no capture records",
        ),
        item("claude_code.rs", "admits_transcript", PATH_AXIS),
        item("claude_code.rs", "subagent_session", PATH_AXIS),
        item("claude_code.rs", "ACTIVITY_TYPES", TAIL_READ),
        item("claude_code.rs", "cc_session_ended", TAIL_READ),
        literal(
            "claude_code.rs",
            "<synthetic>",
            "CC's placeholder model: a rename shows a cosmetic label until a real model arrives",
        ),
        item(
            "claude_code.rs",
            "ULTRA_EXIT_LABEL",
            "a label we write, not one CC sends",
        ),
        item("codewhale.rs", "KEYS", LABEL_DETAIL),
        item(
            "codewhale.rs",
            "DELEGATING_ACTIONS",
            "an agent call naming one decodes as one naming none, so the strip blanks it",
        ),
        item("copilot.rs", "KEYS", LABEL_DETAIL),
        literal(
            "decoder.rs",
            "cursor_version",
            "cursor's, which runs in no hermetic tier (owner, 2026-10-13)",
        ),
        item("decoder.rs", "describe_tool_target", LABEL_DETAIL),
        item("decoder.rs", "SUBAGENTS_DIR", PATH_AXIS),
        item("codex/native.rs", "is_rollout_filename", PATH_AXIS),
        item("grok.rs", "is_updates_jsonl", PATH_AXIS),
        item("grok/native.rs", "grok_probe_root_resolved", PATH_AXIS),
        item("grok.rs", "KEYS", LABEL_DETAIL),
        literal("grok.rs", "idle_prompt", IGNORE_ARM),
        literal("grok.rs", "task_complete", IGNORE_ARM),
        item("hermes.rs", "KEYS", LABEL_DETAIL),
        item("hermes.rs", "resolve_hermes_home", PATH_AXIS),
        item("hermes.rs", "expand_home", PATH_AXIS),
        item("hermes.rs", "python_home", PATH_AXIS),
        item("omp.rs", "AGENT_SUBDIR", PATH_AXIS),
        item("omp.rs", "EXTENSIONS_SUBDIR", PATH_AXIS),
        item("omp.rs", "PROFILES_SUBDIR", PATH_AXIS),
        item("omp.rs", "stem_chain", PATH_AXIS),
        item("omp.rs", "normalize_profile_name", PATH_AXIS),
        item("omp.rs", "parse_omp_env_line", PATH_AXIS),
        item("omp.rs", "with_omp_dotenv", PATH_AXIS),
        item("omp.rs", "from_process", PATH_AXIS),
        item("omp.rs", "parse_omp_env_file", PATH_AXIS),
        item("omp/native.rs", "omp_recognize", PATH_AXIS),
        item("omp.rs", "KEYS", LABEL_DETAIL),
        item("reasonix.rs", "KEYS", LABEL_DETAIL),
        literal("copilot.rs", "approved", LOSS_APPROVE),
        literal("dsh.rs", "allowed-once", LOSS_APPROVE),
        sibling("dsh.rs", "subagent_fork", "subagent"),
        sibling("reasonix.rs", "read_only_task", "explore"),
        sibling("reasonix.rs", "parallel_tasks", "explore"),
        sibling("reasonix.rs", "locate", "explore"),
        sibling("reasonix.rs", "research", "explore"),
        sibling("reasonix.rs", "review", "explore"),
        sibling("reasonix.rs", "security_review", "explore"),
        sibling("antigravity.rs", "ask_permission", "ask_question"),
        literal("codex.rs", "patch_apply_end", LOSS_LEGACY),
        literal("codex.rs", "web_search_end", LOSS_LEGACY),
        literal("codex.rs", "tool_search_call", LOSS_NO_EMITTER),
        literal("codex.rs", "tool_search_output", LOSS_NO_EMITTER),
        literal("dsh.rs", "cacheWriteTokens", LOSS_CACHE_WRITE),
        literal("antigravity.rs", "CONVERSATION_HISTORY", AG_INPUT_STEP),
        literal("antigravity.rs", "USER_INPUT", AG_INPUT_STEP),
    ];

    /// Each profile's scenario-fixture keys and values.
    fn held() -> BTreeMap<&'static str, BTreeSet<String>> {
        PROFILES
            .iter()
            .map(|p| {
                let reads = super::super::verify::scenario_reads(p, p.scenarios.iter())
                    .expect("the fixtures read");
                (p.source, reads.keys_and_values())
            })
            .collect()
    }

    #[test]
    fn every_literal_a_decoder_reads_is_recorded_owed_or_exempt() {
        let absent = NOT_HERMETIC.iter().map(|(s, _)| *s).collect();
        let owing: Vec<Owing<'_>> = PROFILES.iter().map(Owing::of).collect();
        let found = problems(
            &population().expect("the decoders parse"),
            &held(),
            &owing,
            EXEMPT,
            &absent,
        );
        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    fn lit(module: &str, item: &str, text: &str) -> Literal {
        Literal {
            module: module.into(),
            item: item.into(),
            text: text.into(),
            how: Match::Exact,
        }
    }

    /// The census of `population` against codex owing `owed` literals alone,
    /// its fixtures holding `held`.
    fn census(
        population: &[Literal],
        held: &[&str],
        owed: &[&str],
        exempt: &[Exempt],
    ) -> Vec<String> {
        let held = BTreeMap::from([("codex", held.iter().map(|s| (*s).to_owned()).collect())]);
        let owing = Owing {
            source: "codex",
            literals: owed,
            hooks: &[],
            fields: &[],
        };
        problems(
            &population.iter().cloned().collect(),
            &held,
            &[owing],
            exempt,
            &BTreeSet::new(),
        )
    }

    #[test]
    fn a_literal_no_fixture_holds_and_nothing_owes_is_a_problem() {
        let found = census(&[lit("codex.rs", "decode", "turn_renamed")], &[], &[], &[]);
        assert!(
            found.iter().any(|f| f.contains("`turn_renamed`")),
            "{found:?}"
        );
        let held = census(
            &[lit("codex.rs", "decode", "turn_renamed")],
            &["turn_renamed"],
            &[],
            &[],
        );
        assert!(
            !held.iter().any(|f| f.contains("`turn_renamed`")),
            "{held:?}"
        );
    }

    #[test]
    fn an_owed_literal_a_fixture_holds_leaves_the_list() {
        let owed = "turn_renamed";
        let found = census(&[lit("codex.rs", "decode", owed)], &[owed], &[owed], &[]);
        assert!(
            found.iter().any(|f| f.contains("leaves not_yet_literals")),
            "{found:?}"
        );
    }

    #[test]
    fn an_owed_literal_no_decoder_reads_is_stale() {
        let found = census(&[], &[], &["turn_renamed"], &[]);
        assert!(
            found.iter().any(|f| f.contains("no decoder of it reads")),
            "{found:?}"
        );
    }

    #[test]
    fn an_exemption_that_excuses_nothing_is_stale() {
        let row = [literal("codex.rs", "gone", "test")];
        let found = census(&[], &[], &[], &row);
        assert!(
            found.iter().any(|f| f.contains("excuses no literal")),
            "{found:?}"
        );
    }

    #[test]
    fn a_literal_owed_twice_is_refused_and_one_owed_elsewhere_is_owed() {
        let owing = |literals: &'static [&'static str]| Owing {
            source: "codex",
            literals,
            hooks: &["StopFailure"],
            fields: &[("tx:event_msg", "/payload/turn_renamed")],
        };
        let held = BTreeMap::from([("codex", BTreeSet::new())]);
        let run = |o: Owing<'_>, population: &[Literal]| {
            problems(
                &population.iter().cloned().collect(),
                &held,
                &[o],
                &[],
                &BTreeSet::new(),
            )
        };
        let twice = run(
            owing(&["stop_failure"]),
            &[lit("codex.rs", "decode", "stop_failure")],
        );
        assert!(
            twice
                .iter()
                .any(|f| f.contains("which another owed list owes")),
            "{twice:?}"
        );
        let once = run(owing(&[]), &[lit("codex.rs", "decode", "turn_renamed")]);
        assert!(
            once.is_empty(),
            "a field owed elsewhere owes its literal: {once:?}"
        );
    }

    #[test]
    fn a_sibling_exemption_needs_its_arm_held_or_owed() {
        let row = [sibling("codex.rs", "a_sibling", "the_arm")];
        let refused = |held: &[&str], owed: &[&str], arm_item: &str| {
            let population = [
                lit("codex.rs", "decode", "a_sibling"),
                lit("codex.rs", arm_item, "the_arm"),
            ];
            census(&population, held, owed, &row)
                .iter()
                .any(|f| f.contains("exempt as a sibling of `the_arm`"))
        };
        assert!(refused(&[], &[], "decode"));
        assert!(!refused(&["the_arm"], &[], "decode"));
        assert!(!refused(&[], &["the_arm"], "decode"));
        assert!(
            refused(&["the_arm"], &[], "elsewhere"),
            "another item's arm"
        );
    }

    #[test]
    fn a_module_with_no_owner_is_a_problem() {
        let found = census(&[lit("nobody_knows.rs", "f", "x")], &[], &[], &[]);
        assert!(
            found.iter().any(|f| f.contains("a module with no owner")),
            "{found:?}"
        );
    }

    #[test]
    fn a_hook_owed_in_another_spelling_is_owed_once() {
        assert!(one_hook("PostToolUseFailure", "post_tool_use_failure"));
        assert!(one_hook(
            "session.execution.failed",
            "SessionExecutionFailed"
        ));
        assert!(!one_hook("Stop", "StopFailure"));
    }

    #[test]
    fn the_scan_reads_each_place_a_decoder_names_a_string() {
        let file = syn::parse_file(
            r#"
        const KIND: &str = "kind_const";
        const ROLES: &[&str] = &[":planner"];
        const MARK: &str = ":sub:";
        const BOTH: &str = "both_ends";
        fn decode(v: &Value, s: &str, field: impl Fn(&str) -> Option<&str>) {
            let _ = v["indexed"] == *"compared" || s == "rhs" || Some("some") == v.as_str();
            let _ = v.get("got").and_then(|x| x.pointer("/a/b"));
            let _ = field("helper") ;
            let _ = drift::missing_field("breadcrumb");
            let _ = s.strip_prefix("pre-");
            let _ = s.split_once(MARK);
            let _ = ROLES.iter().any(|r| s.ends_with(r));
            let _ = ["inline_a", "inline_b"].iter().find_map(|k| v.get(k));
            let _ = [":inline_tail"].iter().any(|r| s.ends_with(r));
            let _ = [TAIL].iter().any(|r| s.ends_with(r));
            let _ = ["after_bare"].iter().find_map(|k| v.get(k));
            let _ = s.starts_with(BOTH) || s.ends_with(BOTH);
            let _ = matches!(s, "m1" | "m2");
            let _ = vec![field("in_vec")];
            match s { "arm" => {}, _ => {} }
        }
        #[cfg(test)]
        fn skipped() { let _ = v["test_only"]; }
        #[cfg(all(test, unix))]
        mod also_skipped { fn f(v: &Value) { let _ = v["unix_test_only"]; } }
"#,
        )
        .expect("parses");
        let all = collect("x.rs", &file);
        assert!(
            all.iter()
                .any(|l| l.text == "inline_a" && l.item == "decode"),
            "an inline array's literal sits in its item: {all:?}"
        );
        let both: BTreeSet<Match> = all
            .iter()
            .filter(|l| l.text == "both_ends")
            .map(|l| l.how)
            .collect();
        assert_eq!(both, BTreeSet::from([Match::Prefix, Match::Suffix]));
        let got: BTreeMap<String, Match> = all.into_iter().map(|l| (l.text, l.how)).collect();
        for exact in [
            "kind_const",
            "indexed",
            "compared",
            "rhs",
            "some",
            "got",
            "a",
            "b",
            "helper",
            "in_vec",
            "m1",
            "m2",
            "arm",
            "inline_a",
            "inline_b",
            "after_bare",
        ] {
            assert_eq!(got.get(exact), Some(&Match::Exact), "{exact}: {got:?}");
        }
        assert_eq!(got.get("pre-"), Some(&Match::Prefix));
        assert_eq!(got.get(":sub:"), Some(&Match::Within));
        assert_eq!(got.get(":planner"), Some(&Match::Suffix));
        assert_eq!(got.get(":inline_tail"), Some(&Match::Suffix));
        for absent in ["breadcrumb", "test_only", "unix_test_only"] {
            assert!(!got.contains_key(absent), "{absent}: {got:?}");
        }
    }

    /// An export a plugin installer reads is exactly its decoder's dispatch
    /// arms, each a const, so the plugin forwards every event the decoder
    /// handles and nothing a bare literal could hide.
    #[test]
    fn every_exported_set_is_exactly_its_dispatch_arms() {
        for (module, dispatch, on, export) in [
            (
                "opencode.rs",
                "decode_oc_hook_payload",
                "event",
                "DECODED_EVENTS",
            ),
            (
                "omp.rs",
                "decode_omp_hook_payload",
                "ty",
                "DECODED_HOOK_EVENTS",
            ),
            (
                "hermes.rs",
                "decode_hermes_hook_payload",
                "event",
                "DECODED_EVENTS",
            ),
        ] {
            let src = std::fs::read_to_string(source_root().join(module)).expect("readable");
            let file = syn::parse_file(&src).expect("parses");
            let (arms, bare) = dispatch_arms(&file, dispatch, on);
            assert!(
                bare.is_empty(),
                "{module}: an arm spells {bare:?}: name it as a const"
            );
            assert_eq!(arms, listed(&file, export), "{module}: arms vs {export}");
        }
    }
}
