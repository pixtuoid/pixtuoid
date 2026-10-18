//! The upstream drift watch: a name a source declares it reads
//! ([`pixtuoid_core::source::upstream`]) has not vanished upstream, and one it
//! waits on has not appeared. The
//! `drift_watch` example runs it for upstream-drift.yml, which files the report
//! as a tracking issue; a name ships renamed with no notice (`Task` → `Agent`
//! silently disabled subagent suppression), so this is how we hear first.

mod net;
mod read;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Duration;

use pixtuoid_core::source::registry::REGISTRY;
use pixtuoid_core::source::upstream::{
    At, Doc, Family, Finding, Read, Release, Repo, Siblings, Watch, When, Yield,
};
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::install::target::TARGETS;

use net::Platform;
use read::{Names, Unread};

/// One stalled host must not hold the whole sweep.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Only a release tarball earns more.
const RELEASE_ASSET_TIMEOUT: Duration = Duration::from_secs(120);

const RAW_URL: &str = "https://raw.githubusercontent.com";
const API_URL: &str = "https://api.github.com/repos";
const NPM_URL: &str = "https://registry.npmjs.org";

/// Why a fetch returned nothing. Neither is ever a vanish.
#[derive(Debug, PartialEq, Eq)]
enum FetchError {
    /// Our pin is gone, so the watch is dark for that document: `why` is the
    /// status upstream answered, or what its release lacks.
    Gone { why: String },
    /// May pass on a retry.
    Transient(String),
}

/// The network, behind a seam so the engine's tests never spawn `curl` or
/// `tar`.
trait Fetch {
    /// The body at `url`.
    ///
    /// # Errors
    ///
    /// [`FetchError::Gone`] on a status that means the pin is wrong, else
    /// [`FetchError::Transient`].
    fn get(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, FetchError>;
    /// `member` out of a gzipped tarball; `None` when it has none.
    ///
    /// # Errors
    ///
    /// [`FetchError::Transient`] when the extractor cannot run or fails.
    fn untar(&self, tarball: &[u8], member: &str) -> Result<Option<Vec<u8>>, FetchError>;
}

/// One bucket per DISPOSITION, because each calls for a different action:
/// `breaking` fix the decoder or the registration, `review` adopt, ignore or re-verify,
/// `blind` repin by hand,
/// `errors` transient. Collapsing `blind` into `breaking` is what made #793
/// report phantom renames.
#[derive(Debug, Default)]
pub struct Report {
    breaking: Vec<String>,
    review: Vec<String>,
    blind: Vec<String>,
    errors: Vec<String>,
}

impl Report {
    fn add_breaking(&mut self, line: String) {
        self.breaking.push(line);
    }

    fn add_review(&mut self, line: String) {
        self.review.push(line);
    }

    fn add_error(&mut self, line: String) {
        self.errors.push(line);
    }

    /// A lookup missed. Composed here so the disclaimer always ships with it;
    /// `ours` when the unreadable thing is ours, since the default wording
    /// blames upstream, which nothing verified.
    fn add_blind(&mut self, what: &str, at: &str, consequence: &str, ours: bool) {
        let (cause, action) = if ours {
            (
                "Our own declaration or checker is stale; nothing upstream was consulted.",
                "Fix ours — this says nothing about upstream.",
            )
        } else {
            (
                "Upstream may have moved or reshaped it, or our pin/parser may be stale; \
                 this is NOT evidence that upstream changed.",
                "Re-verify upstream by hand, then repin — do NOT change a decoder on this alone.",
            )
        };
        self.blind.push(format!(
            "could not verify {what} at {at} — {cause} {consequence} {action}"
        ));
    }

    /// The H1, which IS the issue title: upstream-drift.yml reads it back off
    /// the rendered report. Ordered by disposition, so the issue list carries
    /// the strongest finding.
    fn title(&self) -> &'static str {
        if !self.breaking.is_empty() {
            "Upstream CLI wire-format drift detected"
        } else if !self.review.is_empty() {
            "Upstream changes to review"
        } else if !self.blind.is_empty() {
            "Upstream drift watch could not verify — repin needed"
        } else {
            "Upstream wire-format watch: no drift"
        }
    }

    pub fn render(&self) -> String {
        let mut out = vec![format!("# {}", self.title()), String::new()];
        let mut section = |head: &str, intro: Option<&str>, lines: &[String]| {
            if lines.is_empty() {
                return;
            }
            out.push(head.to_owned());
            out.push(String::new());
            if let Some(intro) = intro {
                out.push(intro.to_owned());
                out.push(String::new());
            }
            out.extend(lines.iter().map(|l| format!("- {l}")));
            out.push(String::new());
        };
        section(
            "## ⛔ Verified upstream change — events will silently drop",
            Some(
                "Each line was read off the upstream surface that OWNS the name: the \
                 declaration is present and the name is gone. Fix what the line names: \
                 the decoder, or the hook we register.",
            ),
            &self.breaking,
        );
        section("## 🔎 Upstream changes to review", None, &self.review);
        section(
            "## 🩺 Probe could NOT verify — this is not evidence of upstream change",
            Some(
                "A lookup missed, so all the watch knows is that **its own probe failed**. \
                 A stale pin, a moved declaration, or a refactor that left a re-export \
                 facade at the old path all land here, and every check riding the \
                 affected surface was SKIPPED rather than reported as drift. **Verify \
                 upstream by hand and repin — do NOT change a decoder on these lines \
                 alone** (#793 did, against working env vars).",
            ),
            &self.blind,
        );
        section(
            "## ⚠️ Could not verify (transient network/HTTP — not drift)",
            None,
            &self.errors,
        );
        if self.breaking.is_empty()
            && self.review.is_empty()
            && self.blind.is_empty()
            && self.errors.is_empty()
        {
            out.push(
                "✅ No drift. Nothing we read has vanished and nothing we wait on has appeared."
                    .to_owned(),
            );
        }
        out.join("\n")
    }

    /// 0 no findings · 1 actionable (the workflow opens an issue) · 2 transient
    /// only. `blind` is actionable: the watch is dark until someone repins, and
    /// a dark watch that exits 0 goes green (#454).
    pub fn exit_code(&self) -> u8 {
        if !(self.breaking.is_empty() && self.review.is_empty() && self.blind.is_empty()) {
            1
        } else if !self.errors.is_empty() {
            2
        } else {
            0
        }
    }
}

/// The ref of a [`Repo`] a run reads its documents at.
#[derive(Debug, Clone)]
enum Ref {
    Tag(String),
    Branch(String),
}

impl Ref {
    fn name(&self) -> &str {
        match self {
            Self::Tag(name) | Self::Branch(name) => name,
        }
    }

    /// Spelled whole for raw.githubusercontent.com: a tag and a branch may
    /// share a name.
    fn qualified(&self) -> String {
        match self {
            Self::Tag(tag) => format!("refs/tags/{tag}"),
            Self::Branch(branch) => format!("refs/heads/{branch}"),
        }
    }
}

/// Where one declared document is read from.
enum Located {
    Raw {
        url: String,
    },
    Release {
        repo: &'static str,
        tag: String,
        asset: &'static str,
        member: &'static str,
        sums: &'static str,
    },
}

/// One run's documents: each is upstream's answer for the whole run, fetched
/// once however many watches read it, and a missed anchor is filed once, as
/// is each repository's ref.
struct Fetched<'f> {
    fetch: &'f dyn Fetch,
    refs: HashMap<&'static str, Option<Ref>>,
    texts: HashMap<String, Option<String>>,
    unanchored: HashSet<(String, &'static str)>,
}

impl<'f> Fetched<'f> {
    fn new(fetch: &'f dyn Fetch) -> Self {
        Self {
            fetch,
            refs: HashMap::new(),
            texts: HashMap::new(),
            unanchored: HashSet::new(),
        }
    }

    /// `at`'s [`Located`], and the label the report names it by; `None` once
    /// its repository's ref has filed why it could not resolve.
    fn locate(&mut self, at: &At, report: &mut Report) -> Option<(Located, String)> {
        Some(match *at {
            At::Path { repo, path } => {
                let at_ref = self.resolve(repo, report)?;
                (
                    Located::Raw {
                        url: format!("{RAW_URL}/{}/{}/{path}", repo.name, at_ref.qualified()),
                    },
                    format!("{}@{}:{path}", repo.name, at_ref.name()),
                )
            }
            At::ReleaseAsset {
                repo,
                asset,
                member,
                sums,
            } => {
                let tag = self.resolve(repo, report)?.name().to_owned();
                let label = format!("{}@{tag} ({asset}: {member})", repo.name);
                (
                    Located::Release {
                        repo: repo.name,
                        tag,
                        asset,
                        member,
                        sums,
                    },
                    label,
                )
            }
            At::Url(url) => (
                Located::Raw {
                    url: url.to_owned(),
                },
                url.trim_start_matches("https://").to_owned(),
            ),
        })
    }

    /// The ref of `repo` users run, asked once per run.
    fn resolve(&mut self, repo: &'static Repo, report: &mut Report) -> Option<Ref> {
        if let Some(r) = self.refs.get(repo.name) {
            return r.clone();
        }
        let resolved = match self.resolve_now(repo) {
            Ok(r) => Some(r),
            Err(FetchError::Gone { why }) => {
                report.add_blind(
                    &format!("which ref of `{}` users run", repo.name),
                    &format!("{API_URL}/{}", repo.name),
                    &format!("{why}. Every watch reading it was SKIPPED."),
                    false,
                );
                None
            }
            Err(FetchError::Transient(e)) => {
                report.add_error(format!("{}: {e}", repo.name));
                None
            }
        };
        if let (Some(_), Release::Branch) = (&resolved, &repo.release) {
            match self.json_str(
                &format!("{API_URL}/{}/releases/latest", repo.name),
                "tag_name",
            ) {
                Ok(tag) => report.add_review(format!(
                    "`{}` now publishes `{}`, and its documents are still read at its default \
                     branch. Choose its `Release`.",
                    repo.name,
                    code_span(&tag)
                )),
                Err(FetchError::Gone { .. }) => {}
                Err(FetchError::Transient(e)) => report.add_error(format!("{}: {e}", repo.name)),
            }
        }
        self.refs.insert(repo.name, resolved.clone());
        resolved
    }

    /// A moved repository is never followed: [`Repo::name`] says why.
    fn resolve_now(&self, repo: &Repo) -> Result<Ref, FetchError> {
        let api = format!("{API_URL}/{}", repo.name);
        let meta = self.json(&api)?;
        let full_name = field(&meta, &api, "full_name")?;
        if !full_name.eq_ignore_ascii_case(repo.name) {
            return Err(FetchError::Gone {
                why: format!("It moved to `{}`: repin its `Repo`", code_span(&full_name)),
            });
        }
        let tag = |tag: String, missing: String| match self
            .fetch
            .get(&format!("{api}/git/ref/tags/{tag}"), FETCH_TIMEOUT)
        {
            Ok(_) => Ok(Ref::Tag(tag)),
            Err(FetchError::Gone { .. }) => Err(FetchError::Gone { why: missing }),
            Err(e) => Err(e),
        };
        match repo.release {
            Release::GithubLatest => {
                match self.json_str(&format!("{api}/releases/latest"), "tag_name") {
                    Err(FetchError::Gone { why }) => Err(FetchError::Gone {
                        why: format!("No release resolves for it (its latest release: {why})"),
                    }),
                    got => got.map(Ref::Tag),
                }
            }
            Release::Npm {
                package,
                tag_prefix,
            } => {
                let version = self.json_str(&format!("{NPM_URL}/{package}/latest"), "version")?;
                let want = format!("{tag_prefix}{version}");
                let missing = format!(
                    "No release resolves for it: npm's `{package}` is at `{}`, which it does not \
                     tag `{}`",
                    code_span(&version),
                    code_span(&want)
                );
                tag(want, missing)
            }
            Release::Pinned { tag: pinned } => tag(
                pinned.to_owned(),
                format!("Its pinned tag `{pinned}` is gone"),
            ),
            Release::Branch => field(&meta, &api, "default_branch").map(Ref::Branch),
        }
    }

    fn json(&self, url: &str) -> Result<serde_json::Value, FetchError> {
        let body = self.fetch.get(url, FETCH_TIMEOUT)?;
        serde_json::from_slice(&body)
            .map_err(|e| FetchError::Transient(format!("{url} is no JSON document ({e})")))
    }

    fn json_str(&self, url: &str, key: &str) -> Result<String, FetchError> {
        field(&self.json(url)?, url, key)
    }

    fn text(&mut self, located: &Located, label: &str, report: &mut Report) -> Option<String> {
        if let Some(t) = self.texts.get(label) {
            return t.clone();
        }
        let got = match located {
            Located::Raw { url } => self
                .fetch
                .get(url, FETCH_TIMEOUT)
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            Located::Release {
                repo,
                tag,
                asset,
                member,
                sums,
            } => self.release_member(repo, tag, asset, member, sums),
        };
        let text = match got {
            Ok(t) => Some(t),
            Err(FetchError::Gone { why }) => {
                report.add_blind(
                    "the document",
                    &format!("{label} ({why})"),
                    "Every watch reading it was SKIPPED.",
                    false,
                );
                None
            }
            Err(FetchError::Transient(e)) => {
                report.add_error(format!("{label}: {e}"));
                None
            }
        };
        self.texts.insert(label.to_owned(), text.clone());
        text
    }

    /// The release's own sums file must vouch for the tarball before any of
    /// it is read.
    fn release_member(
        &self,
        repo: &str,
        tag: &str,
        asset: &str,
        member: &str,
        sums: &str,
    ) -> Result<String, FetchError> {
        let base = format!("https://github.com/{repo}/releases/download/{tag}/");
        let name = asset.replace("{version}", tag.trim_start_matches('v'));
        let tarball = self
            .fetch
            .get(&format!("{base}{name}"), RELEASE_ASSET_TIMEOUT)?;
        let listed =
            String::from_utf8_lossy(&self.fetch.get(&format!("{base}{sums}"), FETCH_TIMEOUT)?)
                .into_owned();
        let digest = sha256_hex(&tarball);
        let vouched = listed.lines().any(|l| {
            let mut cols = l.split_whitespace();
            cols.next() == Some(digest.as_str()) && cols.next() == Some(name.as_str())
        });
        if !vouched {
            return Err(FetchError::Transient(format!(
                "{name} does not match {tag}'s {sums}"
            )));
        }
        let body = self
            .fetch
            .untar(&tarball, member)?
            .ok_or_else(|| FetchError::Gone {
                why: format!("no {member} in {name}"),
            })?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// The text, once the document proves it is the one that declares the
    /// names: a fetch landing on the wrong content skips every watch riding
    /// it as probe health.
    fn anchored(
        &mut self,
        located: &Located,
        label: &str,
        doc: &Doc,
        report: &mut Report,
    ) -> Option<String> {
        let text = self.text(located, label, report)?;
        let anchor = match Regex::new(doc.anchor) {
            Ok(re) => re,
            Err(e) => {
                report.add_blind(
                    &format!("the anchor of {label} ({e})"),
                    label,
                    "Every watch reading it was SKIPPED.",
                    true,
                );
                return None;
            }
        };
        if anchor.is_match(&text) {
            return Some(text);
        }
        if self.unanchored.insert((label.to_owned(), doc.anchor)) {
            report.add_blind(
                &format!("{label}: the document no longer contains {}", doc.owns),
                label,
                "It still fetches, so the probe landed on the wrong content — most likely \
                 a stale pin (a moved declaration, or a re-export facade left at the old \
                 path) rather than an upstream rename. Every watch riding this document \
                 was SKIPPED, NOT reported as drift.",
                false,
            );
        }
        None
    }
}

/// `key`'s string in `v`, the JSON document at `url`.
fn field(v: &serde_json::Value, url: &str, key: &str) -> Result<String, FetchError> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| FetchError::Transient(format!("{url} names no `{key}`")))
}

fn check_watch(source: &str, w: &Watch, fetched: &mut Fetched, report: &mut Report) {
    // All or nothing: a name may live in any one of them, so a partial read
    // would report it gone. Each miss has already filed its own line.
    let mut located = Vec::new();
    for doc in w.docs {
        located.push(
            fetched
                .locate(&doc.at, report)
                .map(|(l, label)| (l, label, doc)),
        );
    }
    let Some(located) = located.into_iter().collect::<Option<Vec<_>>>() else {
        return;
    };
    let mut texts = Vec::new();
    for (l, label, doc) in &located {
        texts.push(fetched.anchored(l, label, doc, report));
    }
    let Some(texts) = texts.into_iter().collect::<Option<Vec<String>>>() else {
        return;
    };
    let at = located
        .iter()
        .map(|(_, label, _)| label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let ours: BTreeSet<&str> = (w.ours)().into_iter().collect();
    let read_from: Vec<(&str, &str)> = located
        .iter()
        .zip(&texts)
        .map(|((_, label, _), text)| (label.as_str(), text.as_str()))
        .collect();
    let Some(read::Declared {
        names: upstream,
        partial,
    }) = read_docs(w, source, &read_from, &ours, report)
    else {
        return;
    };
    let (says, file): (&str, fn(&mut Report, String)) = match w.finding {
        Finding::Breaking(says) => (says, Report::add_breaking),
        Finding::Review(says) => (says, Report::add_review),
    };
    match w.when {
        When::Vanishes => {
            let verb = match w.read.yields() {
                Yield::One { verb } => Some(verb),
                Yield::Declaration | Yield::Family | Yield::Ours | Yield::Fields => None,
            };
            let now = verb.map_or_else(String::new, |verb| {
                format!(
                    " Upstream {verb} `{}` there now.",
                    upstream
                        .iter()
                        .map(|n| code_span(n))
                        .collect::<Vec<_>>()
                        .join("`, `")
                )
            });
            for name in ours.iter().filter(|n| !upstream.contains(**n)) {
                // Out of the read's sight, so no evidence it vanished; a fact
                // of a field the read sees is not.
                if let Some(why) = partial
                    .as_ref()
                    .filter(|_| !upstream.contains(read::field_of(name)))
                {
                    report.add_blind(
                        &format!("`{name}` ({source}: `{}`)", w.id),
                        &at,
                        &format!(
                            "The reader read a partial declaration ({why}), so it may be \
                             declared out of sight and `{}` did not judge it.",
                            w.id
                        ),
                        false,
                    );
                    continue;
                }
                file(
                    report,
                    format!(
                        "`{name}` ({source}: `{}`) is no longer declared in {at} — {says}.{now}",
                        w.id
                    ),
                );
            }
        }
        When::Appears => {
            for name in ours.iter().filter(|n| upstream.contains(**n)) {
                file(
                    report,
                    format!(
                        "{at} now declares `{name}` ({source}: `{}`) — {says}.",
                        w.id
                    ),
                );
            }
        }
    }
    if let Some(sib) = &w.siblings {
        check_siblings(w.id, sib, &ours, &upstream, &at, report);
    }
}

/// Whether fewer names than ours means the reader degraded: only on a vanish
/// watch, since a shrunk parse cannot invent an appearance.
fn floored(w: &Watch) -> bool {
    matches!(w.when, When::Vanishes) && w.read.yields().floored()
}

/// What the floor counts of `names`: a `TsFields` read's `field?` and
/// `field:"lit"` are facts beside a field, not fields.
fn floor_count<'a>(read: &Read, names: impl Iterator<Item = &'a str>) -> usize {
    match read.yields() {
        Yield::Fields => names.filter(|n| read::is_bare(n)).count(),
        Yield::Declaration | Yield::Family | Yield::Ours | Yield::One { .. } => names.count(),
    }
}

/// Upstream's names off every `(label, text)` document of `w`, and why the
/// read is partial; `None` once it has filed why it could not. Every
/// document must yield, as every one must fetch: a union read off the rest
/// would hide what changed in the one that did not.
fn read_docs(
    w: &Watch,
    source: &str,
    docs: &[(&str, &str)],
    ours: &BTreeSet<&str>,
    report: &mut Report,
) -> Option<read::Declared> {
    let mut upstream = Names::new();
    let mut partial = Vec::new();
    for (label, text) in docs {
        let read = match w.read {
            Read::Each { pattern } => read::each(text, pattern, ours).map(Into::into),
            Read::RustStr { item } => read::rust_str(text, item).map(Into::into),
            Read::RustArms { name, arm } => read::rust_arms(text, name, arm).map(Into::into),
            Read::RustEnum { name } => read::rust_enum(text, name).map(Into::into),
            Read::RustStruct { name } => read::rust_struct(text, name).map(Into::into),
            Read::TsFields { name } => read::ts_fields(text, name),
            Read::TsMembers { of } => read::ts_members(text, of),
            Read::TsReturn { function } => read::ts_return(text, function).map(Into::into),
            Read::Matches { pattern } => read::matches(text, pattern).map(Into::into),
            Read::MdTable { header, under } => read::md_table(text, header, under).map(Into::into),
            Read::MdHeadings { under } => read::md_headings(text, under).map(Into::into),
            Read::PySet { decl } => read::py_set(text, decl).map(Into::into),
            Read::RustMacroRows { name } => read::rust_macro_rows(text, name).map(Into::into),
            Read::TsConstArray { name } => read::ts_const_array(text, name).map(Into::into),
            Read::TsConst { name } => read::ts_const(text, name).map(Into::into),
            Read::JsonUnion { def, prop } => read::json_union(text, def, prop).map(Into::into),
            Read::JsonKeys => read::json_keys(text).map(Into::into),
        };
        match read {
            Ok(read::Declared {
                names,
                partial: why,
            }) => {
                upstream.extend(names);
                partial.extend(why.map(|why| format!("{label}: {why}")));
            }
            Err(Unread::Ours(e)) => {
                report.add_blind(
                    &format!("`{}`'s pattern or item spec ({e})", w.id),
                    source,
                    &format!("`{}` was SKIPPED.", w.id),
                    true,
                );
                return None;
            }
            Err(Unread::Missing(what)) => {
                report.add_blind(
                    &format!("the names `{}` reads", w.id),
                    label,
                    &format!("The reader found {what}, so `{}` was SKIPPED.", w.id),
                    false,
                );
                return None;
            }
        }
    }
    let (got, want) = (
        floor_count(&w.read, upstream.iter().map(String::as_str)),
        floor_count(&w.read, ours.iter().copied()),
    );
    if floored(w) && got < want {
        let at: Vec<&str> = docs.iter().map(|(label, _)| *label).collect();
        report.add_blind(
            &format!("the names `{}` reads", w.id),
            &at.join(", "),
            &format!(
                "The reader returned {got} names (floor {want}), so `{}` was SKIPPED.",
                w.id
            ),
            false,
        );
        return None;
    }
    Some(read::Declared {
        names: upstream,
        partial: (!partial.is_empty()).then(|| partial.join("; ")),
    })
}

/// Run only once every document of the watch was read: a missed one would
/// read as upstream deleting each ignored row.
fn check_siblings(
    id: &str,
    sib: &Siblings,
    ours: &BTreeSet<&str>,
    upstream: &Names,
    at: &str,
    report: &mut Report,
) {
    let first_word = |n: &str| n.split('_').next().unwrap_or(n).to_owned();
    let last_word = |n: &str| n.rsplit('_').next().unwrap_or(n).to_owned();
    let words = |word: &dyn Fn(&str) -> String| -> BTreeSet<String> {
        ours.iter()
            .filter(|o| o.contains('_'))
            .map(|o| word(o))
            .collect()
    };
    let in_family: Box<dyn Fn(&str) -> bool> = match sib.family {
        Family::All => Box::new(|_| true),
        Family::Prefix(p) => Box::new(move |n: &str| n.starts_with(p)),
        Family::FirstWord => {
            let kin = words(&first_word);
            Box::new(move |n: &str| kin.contains(&first_word(n)))
        }
        Family::LastWord => {
            let kin = words(&last_word);
            Box::new(move |n: &str| kin.contains(&last_word(n)))
        }
    };
    let ignored: BTreeSet<&str> = sib.ignored.iter().map(|(name, _why)| *name).collect();
    for name in upstream.iter().map(String::as_str) {
        if in_family(name) && !ours.contains(name) && !ignored.contains(name) {
            report.add_review(format!(
                "{at} declares `{}` beside the names `{id}` reads, and nothing decodes it. \
                 Decode it, or add it to that watch's ignored siblings with the reason.",
                code_span(name)
            ));
        }
    }
    for name in ignored.iter().filter(|n| !upstream.contains(**n)) {
        report.add_review(format!(
            "`{id}` still ignores `{name}`, which {at} no longer declares — upstream removed \
             it. Delete the row: it documents a decision about a name that is gone, and \
             would silently exempt it if upstream reused it."
        ));
    }
}

/// An upstream name as the inside of a Markdown code span: the report becomes
/// an issue body and a terminal's output, so a backtick would end the span and
/// a control or bidi character would drive what the reader sees.
fn code_span(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '`' && !crate::is_terminal_unsafe(*c))
        .collect()
}

/// Every watch the registered sources and the install targets declare, over the
/// network.
pub fn run(report: &mut Report) {
    check_all(&Platform, report);
}

/// Every declared watch with the source it reads for: the decoders' off the
/// registry, then the installers' off their targets.
fn declared() -> impl Iterator<Item = (&'static str, &'static Watch)> {
    REGISTRY
        .iter()
        .flat_map(|d| d.upstream.watches().map(move |w| (d.name, w)))
        .chain(
            TARGETS
                .iter()
                .flat_map(|t| t.upstream.iter().map(move |w| (t.core_source, w))),
        )
}

fn check_all(fetch: &dyn Fetch, report: &mut Report) {
    let mut fetched = Fetched::new(fetch);
    for (source, w) in declared() {
        check_watch(source, w, &mut fetched, report);
    }
}

/// `bytes`' SHA-256 as a sums file lists it: sha2's digest has no `LowerHex`.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

#[cfg(test)]
mod tests;
