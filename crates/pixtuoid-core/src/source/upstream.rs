//! What a source depends on UPSTREAM, declared beside the names it reads: the
//! documents that declare those names, how to read them, and which change to
//! one is a finding, with what it costs a user or what a maintainer
//! re-verifies. pixtuoid's `drift_watch` checks them against upstream.

/// Whether a source is watched upstream, and through what.
#[derive(Debug)]
pub enum Upstream {
    /// One slice of watches per upstream project the source reads.
    Watched(&'static [&'static [Watch]]),
    /// Its install target's watches carry what it depends on: it decodes the
    /// hook set its installer registers.
    Installer,
    /// Nothing upstream publishes declares what the source reads.
    Closed { why: &'static str },
}

impl Upstream {
    /// Every watch the source itself declares.
    pub fn watches(&self) -> impl Iterator<Item = &'static Watch> + use<> {
        let projects: &'static [&'static [Watch]] = match *self {
            Self::Watched(projects) => projects,
            Self::Installer | Self::Closed { .. } => &[],
        };
        projects.iter().flat_map(|watches| watches.iter())
    }
}

/// An upstream repository and the ref of it users run. Each is one CLI's
/// `REPO`, and one repository has one release policy across the crates that
/// read it (`one_repository_has_one_release_policy`).
#[derive(Debug, PartialEq, Eq)]
pub struct Repo {
    /// `owner/name` as GitHub spells it now: a renamed repository answers
    /// through a redirect only until its old name is reused
    /// (docs.github.com/en/repositories/creating-and-managing-repositories/renaming-a-repository),
    /// so a move is probe health until repinned, never a rename upstream (#793).
    pub name: &'static str,
    pub release: Release,
}

impl Repo {
    /// The document at `path` of this repository.
    pub const fn doc(
        &'static self,
        path: &'static str,
        anchor: &'static str,
        owns: &'static str,
    ) -> Doc {
        Doc {
            at: At::Path { repo: self, path },
            anchor,
            owns,
        }
    }
}

/// Which ref of a [`Repo`] its documents are read at: only the latest released
/// version of each agent CLI is supported, so a default branch would judge
/// what no user runs.
#[derive(Debug, PartialEq, Eq)]
pub enum Release {
    /// The tag of its latest GitHub release.
    GithubLatest,
    /// The tag `{tag_prefix}{version}` of `package`'s npm `latest` version,
    /// for a repository whose GitHub releases are not the CLI users install.
    Npm {
        package: &'static str,
        tag_prefix: &'static str,
    },
    /// One tag, for an upstream users run at the version another CLI locks.
    Pinned { tag: &'static str },
    /// Its default branch, for a repository that publishes no release.
    Branch,
}

/// A document upstream declares names in.
#[derive(Debug)]
pub struct Doc {
    pub at: At,
    /// Matches the declaration that OWNS the names, never one of them: a name
    /// as its own anchor passes vacuously, and a fetch landing on a re-export
    /// facade then reads as a rename (#793).
    pub anchor: &'static str,
    /// What `anchor` matches, as the report names it.
    pub owns: &'static str,
}

/// Where a [`Doc`] is fetched from.
#[derive(Debug)]
pub enum At {
    /// A file of `repo`'s tree at the ref its [`Release`] resolves to.
    Path {
        repo: &'static Repo,
        path: &'static str,
    },
    /// `member` of `repo`'s latest release tarball `asset` (`{version}` = its
    /// tag, `v`-stripped), admitted only when the release's `sums` file vouches
    /// for it.
    ReleaseAsset {
        repo: &'static Repo,
        asset: &'static str,
        member: &'static str,
        sums: &'static str,
    },
    /// A page a closed-source CLI publishes as its contract.
    Url(&'static str),
}

/// How a document yields upstream's names.
#[derive(Debug)]
pub enum Read {
    /// A name is declared where `pattern` matches, `{name}` standing for it,
    /// regex-escaped.
    Each { pattern: &'static str },
    /// The string literal the ONE Rust `item` evaluates to: a const by name,
    /// or a method as `<Type as Trait>::method`, its body's tail.
    RustStr { item: &'static str },
    /// The string patterns of `fn name`'s match arms whose result matches `arm`.
    RustArms {
        name: &'static str,
        arm: &'static str,
    },
    /// Enum `name`'s serde wire names: each top-level variant's `rename`, else
    /// its ident in snake_case. Read only under `rename_all = "snake_case"`,
    /// the one spelling it derives.
    RustEnum { name: &'static str },
    /// The serde wire names of the ONE `struct name`'s fields.
    RustStruct { name: &'static str },
    /// The top-level fields of the ONE interface or type alias `name`: each
    /// field, `field?` when optional, and `field:"lit"` per string literal its
    /// type admits.
    TsFields { name: &'static str },
    /// Every member key of the interfaces and classes named `of`, merged
    /// across a document's declarations as TS merges augmentations.
    TsMembers { of: &'static [&'static str] },
    /// Every `return` of the ONE `function name`, in order, as one name.
    TsReturn { function: &'static str },
    /// Every first group of `pattern`: `Each` turned round, so the floor can
    /// tell a degraded parse from a rename.
    Matches { pattern: &'static str },
    /// The backticked first-column names of the first Markdown table in the
    /// section of the ONE heading `under` whose header row begins with
    /// `header`; other tables repeat names in other columns.
    MdTable {
        header: &'static str,
        under: &'static str,
    },
    /// The one-word titles of the direct subsections of the ONE heading
    /// `under`; a page's other sections carry one-word headings of their own.
    MdHeadings { under: &'static str },
    /// The string members of the `{…}` opened after the ONE match of `decl`,
    /// comments first.
    PySet { decl: &'static str },
    /// The row headers of the ONE `name! { … }` invocation.
    RustMacroRows { name: &'static str },
    /// The string elements of the ONE `const name = [ … ]`.
    TsConstArray { name: &'static str },
    /// The literal the ONE `const name` initializes to: a string's value, or a
    /// number's.
    TsConst { name: &'static str },
    /// JSON Schema: each member of `def`'s `oneOf`/`anyOf` union, as the
    /// const of its `prop` property, or its own const when `prop` is `None`.
    JsonUnion {
        def: &'static str,
        prop: Option<&'static str>,
    },
    /// JSON Schema: every `properties` key, at any depth.
    JsonKeys,
}

impl Read {
    /// What it yields of upstream's declaration. Every judgement that depends
    /// on the reader asks this, so a new reader is classified here once.
    #[deny(clippy::wildcard_enum_match_arm)]
    pub const fn yields(&self) -> Yield {
        match self {
            Self::Each { .. } => Yield::Ours,
            Self::RustStr { .. } | Self::TsConst { .. } => Yield::One { verb: "declares" },
            Self::TsReturn { .. } => Yield::One { verb: "returns" },
            Self::RustArms { .. } => Yield::Family,
            Self::TsFields { .. } => Yield::Fields,
            Self::RustEnum { .. }
            | Self::RustStruct { .. }
            | Self::TsMembers { .. }
            | Self::Matches { .. }
            | Self::MdTable { .. }
            | Self::MdHeadings { .. }
            | Self::PySet { .. }
            | Self::RustMacroRows { .. }
            | Self::TsConstArray { .. }
            | Self::JsonUnion { .. }
            | Self::JsonKeys => Yield::Declaration,
        }
    }
}

/// What a [`Read`] yields of upstream's declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Yield {
    /// The whole declaration a watch's names sit in.
    Declaration,
    /// Only the family a watch reads, never the rest beside it.
    Family,
    /// Only the names of ours it finds.
    Ours,
    /// The one value an item has, which a report says upstream now `verb`.
    One { verb: &'static str },
    /// A type's fields, each beside its `field?` and `field:"lit"` facts.
    Fields,
}

#[deny(clippy::wildcard_enum_match_arm)]
impl Yield {
    /// Whether fewer names than ours can be a degraded parse rather than most
    /// of ours renamed at once (#929): only a read of the whole declaration
    /// shrinks by parsing less, while the rest yield a subset by design.
    pub const fn floored(self) -> bool {
        match self {
            Self::Declaration | Self::Fields => true,
            Self::Family | Self::Ours | Self::One { .. } => false,
        }
    }

    /// Whether it is one value, which only one name of ours can watch.
    pub(crate) const fn is_one(self) -> bool {
        match self {
            Self::One { .. } => true,
            Self::Declaration | Self::Family | Self::Ours | Self::Fields => false,
        }
    }

    /// Why a sibling sweep cannot run off this yield. `all` says the sweep's
    /// family is [`Family::All`], which only a yield confined to the family serves.
    pub(crate) const fn sibling_refusal(self, all: bool) -> Option<&'static str> {
        match self {
            Self::Ours | Self::One { .. } => Some("sweeps siblings off a reader that yields none"),
            Self::Fields => {
                Some("sweeps siblings off a reader whose `field?` and `field:\"lit\"` are no names")
            }
            Self::Declaration if all => {
                Some("sweeps `Family::All` off a reader that yields more than the family")
            }
            Self::Declaration | Self::Family => None,
        }
    }
}

/// Names we read, or wait on, whose vanishing or appearing upstream is a
/// finding.
#[derive(Debug)]
pub struct Watch {
    /// The report's key for this watch; unique across sources.
    pub id: &'static str,
    pub ours: fn() -> Vec<&'static str>,
    /// Read as ONE document, since a name may be declared in any of them: all
    /// must fetch, or none is checked.
    pub docs: &'static [Doc],
    pub read: Read,
    pub when: When,
    pub finding: Finding,
    pub siblings: Option<Siblings>,
}

impl Watch {
    /// Why a checker cannot run this watch, if it cannot: a blank name, or an
    /// `Each` pattern without `{name}`, matches whatever upstream says; a
    /// release asset hangs off a GitHub release; a [`Yield::One`] read yields
    /// one name; and a sibling sweep needs a vanish watch whose yield
    /// [`Yield::sibling_refusal`] admits.
    pub fn unrunnable(&self) -> Option<String> {
        let ours = (self.ours)();
        let yields = self.read.yields();
        let why = if ours.is_empty() || ours.iter().any(|n| n.is_empty()) {
            "watches an empty set or a blank name".to_owned()
        } else if self.docs.is_empty() {
            "reads no document".to_owned()
        } else if matches!(self.read, Read::Each { pattern } if !pattern.contains("{name}")) {
            "has an `Each` pattern that names no name".to_owned()
        } else if self.docs.iter().any(|d| {
            matches!(d.at, At::ReleaseAsset { repo, .. }
                if !matches!(repo.release, Release::GithubLatest))
        }) {
            "reads a release asset off a repository that resolves no GitHub release".to_owned()
        } else if yields.is_one() && ours.len() != 1 {
            format!("reads one name off upstream but watches {}", ours.len())
        } else if let Some(sib) = &self.siblings {
            if !matches!(self.when, When::Vanishes) {
                "sweeps siblings on a watch that does not fire on a vanish".to_owned()
            } else {
                yields
                    .sibling_refusal(matches!(sib.family, Family::All))?
                    .to_owned()
            }
        } else {
            return None;
        };
        Some(why)
    }
}

/// Which change to one of a watch's names is the finding.
#[derive(Debug)]
pub enum When {
    /// Upstream stops declaring it.
    Vanishes,
    /// Upstream starts declaring it: a premise we rely on broke, or a surface
    /// we wait for landed.
    Appears,
}

/// How hard a finding lands, with what it costs a user or what a maintainer
/// re-verifies.
#[derive(Debug)]
pub enum Finding {
    /// Users are broken now.
    Breaking(&'static str),
    /// Nothing is known broken; a maintainer re-verifies.
    Review(&'static str),
}

/// Upstream names beside ours, in a family we already read, that no decoder
/// reads: a sibling (`custom_tool_call` beside `function_call`, #933) is where
/// a missed decode hides, while mere growth is not.
#[derive(Debug)]
pub struct Siblings {
    pub family: Family,
    /// Siblings deliberately not decoded, each with its reason. A row whose
    /// name upstream dropped is reported: it would silently exempt a reuse.
    pub ignored: &'static [(&'static str, &'static str)],
}

/// Which of upstream's names are siblings of ours.
#[derive(Debug)]
pub enum Family {
    /// Starts with this.
    Prefix(&'static str),
    /// Shares its word before the first `_` with one of ours that has one.
    FirstWord,
    /// Shares its word after the last `_` with one of ours that has one.
    LastWord,
    /// Every name the reader yields, which reads only the family already.
    All,
}

/// `bytes`' SHA-256 as a sums file lists it: sha2's digest has no `LowerHex`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Whether a release's `sums` file lists `bytes`' digest for `name`.
pub fn vouches(sums: &str, name: &str, bytes: &[u8]) -> bool {
    let digest = sha256_hex(bytes);
    sums.lines().any(|l| {
        let mut cols = l.split_whitespace();
        cols.next() == Some(digest.as_str()) && cols.next() == Some(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::registry::REGISTRY;

    /// FIPS 180-2's "abc" vector, whose bytes below 0x10 catch a dropped pad.
    #[test]
    fn a_digest_reads_as_a_sums_file_lists_it() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_sums_file_vouches_for_its_own_line_alone() {
        let sums = format!(
            "{}  omp-linux-x64-baseline\n{}  omp-linux-x64\n{}  omp-darwin-arm64\n",
            sha256_hex(b"baseline"),
            sha256_hex(b"linux"),
            sha256_hex(b"darwin")
        );
        assert!(vouches(&sums, "omp-linux-x64", b"linux"));
        assert!(
            !vouches(&sums, "omp-linux-x64", b"darwin"),
            "another's digest"
        );
        assert!(
            !vouches(&sums, "omp-linux-x64", b"baseline"),
            "a longer name's digest"
        );
        assert!(!vouches(&sums, "omp-linux-x64", b"tampered"));
        assert!(!vouches(&sums, "omp-windows-x64.exe", b"linux"), "unlisted");
    }

    /// A closed source says why, since nothing upstream will.
    #[test]
    fn every_closed_source_says_why() {
        for d in REGISTRY {
            if let Upstream::Closed { why } = d.upstream {
                assert!(
                    !why.trim().is_empty(),
                    "{} is closed with no reason",
                    d.name
                );
            }
        }
    }

    /// An emptied list fails here, in core, rather than passing a census its
    /// installer's watches satisfy.
    #[test]
    fn no_source_is_watched_by_an_empty_list() {
        for d in REGISTRY {
            if let Upstream::Watched(projects) = d.upstream {
                assert!(
                    !projects.is_empty() && projects.iter().all(|p| !p.is_empty()),
                    "{} is `Watched` by an empty list",
                    d.name
                );
            }
        }
    }

    /// Each rule refuses the watch it exists for; the roster is held to them by
    /// the binary's `every_declared_watch_is_runnable_and_its_id_unique`.
    #[test]
    fn unrunnable_names_each_watch_a_checker_cannot_run() {
        const OK: Watch = Watch {
            id: "t.ok",
            ours: || vec!["a", "b"],
            docs: &[Doc {
                at: At::Url("https://example.com/x.md"),
                anchor: "x",
                owns: "x",
            }],
            read: Read::Each {
                pattern: r"\b{name}\b",
            },
            when: When::Vanishes,
            finding: Finding::Breaking("x"),
            siblings: None,
        };
        const ALL: Siblings = Siblings {
            family: Family::All,
            ignored: &[],
        };
        const UNRELEASED: Repo = Repo {
            name: "o/r",
            release: Release::Branch,
        };
        let arms = Read::RustArms {
            name: "f",
            arm: "x",
        };
        let cases = [
            (
                Watch {
                    ours: || vec![],
                    ..OK
                },
                "watches an empty set or a blank name",
            ),
            (
                Watch {
                    ours: || vec![""],
                    ..OK
                },
                "watches an empty set or a blank name",
            ),
            (Watch { docs: &[], ..OK }, "reads no document"),
            (
                Watch {
                    docs: &[Doc {
                        at: At::ReleaseAsset {
                            repo: &UNRELEASED,
                            asset: "a",
                            member: "m",
                            sums: "s",
                        },
                        anchor: "x",
                        owns: "x",
                    }],
                    ..OK
                },
                "reads a release asset off a repository that resolves no GitHub release",
            ),
            (
                Watch {
                    read: Read::Each { pattern: "x" },
                    ..OK
                },
                "has an `Each` pattern that names no name",
            ),
            (
                Watch {
                    read: Read::RustStr { item: "X" },
                    ..OK
                },
                "reads one name off upstream but watches 2",
            ),
            (
                Watch {
                    ours: || vec!["a"],
                    read: Read::RustStr { item: "X" },
                    siblings: Some(ALL),
                    ..OK
                },
                "sweeps siblings off a reader that yields none",
            ),
            (
                Watch {
                    read: Read::TsReturn { function: "f" },
                    ..OK
                },
                "reads one name off upstream but watches 2",
            ),
            (
                Watch {
                    ours: || vec!["a"],
                    read: Read::TsReturn { function: "f" },
                    siblings: Some(ALL),
                    ..OK
                },
                "sweeps siblings off a reader that yields none",
            ),
            (
                Watch {
                    read: Read::TsFields { name: "T" },
                    siblings: Some(ALL),
                    ..OK
                },
                "sweeps siblings off a reader whose `field?` and `field:\"lit\"` are no names",
            ),
            (
                Watch {
                    read: arms,
                    when: When::Appears,
                    siblings: Some(ALL),
                    ..OK
                },
                "sweeps siblings on a watch that does not fire on a vanish",
            ),
            (
                Watch {
                    siblings: Some(ALL),
                    ..OK
                },
                "sweeps siblings off a reader that yields none",
            ),
            (
                Watch {
                    read: Read::MdTable {
                        header: "E",
                        under: "S",
                    },
                    siblings: Some(ALL),
                    ..OK
                },
                "sweeps `Family::All` off a reader that yields more than the family",
            ),
        ];
        assert_eq!(OK.unrunnable(), None);
        let family = Watch {
            read: Read::RustArms {
                name: "f",
                arm: "x",
            },
            siblings: Some(ALL),
            ..OK
        };
        assert_eq!(
            family.unrunnable(),
            None,
            "a family reader sweeps `Family::All`"
        );
        for (w, why) in cases {
            assert_eq!(w.unrunnable().as_deref(), Some(why));
        }
    }

    /// Every dispatch arm in `file` names a CONST, and those consts are exactly
    /// the ones `export` declares.
    ///
    /// A drift watch carries the const in its source's `UPSTREAM`, so it
    /// follows a RENAME. It cannot see an ADDITION: an arm
    /// written as a bare literal decodes a name no watch carries, and every
    /// other gate stays green — so a bare literal is rejected outright.
    fn assert_arms_match_export(file: &str, dispatch: &str, export: &str) {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/source")
                .join(file),
        )
        .expect("the module is readable");

        let after = src
            .split_once(dispatch)
            .unwrap_or_else(|| panic!("{file}: no `{dispatch}` dispatch"))
            .1;
        let body = &after[..after.find("\n}\n").unwrap_or(after.len())];
        // The bare-literal ban runs at EVERY depth, not just the level the export
        // is collected from: a nested `match` (acp's on `status`) would otherwise
        // hide wire values behind an indent the collector skips. Naming them is
        // what puts them in front of a reviewer and lets them be exported.
        for line in body.lines() {
            let Some((head, _)) = line.split_once("=>") else {
                continue;
            };
            // ANY `"` in the head, not just a leading or `(`-prefixed one: the
            // shape that escaped was a guard, `g if g == "wire_name" =>`, which
            // dispatches on a name no watch carries while `armed` stays
            // equal to the export because the token is not SCREAMING_SNAKE.
            assert!(
                !head.contains('"'),
                "{file}: arm `{}` in {export}'s dispatch reads a bare wire literal — \
                 name it as a const so a drift watch can carry it",
                head.trim(),
            );
        }
        let mut armed: Vec<&str> = Vec::new();
        for line in body.lines() {
            if !line.contains("=>")
                || !line.starts_with("        ")
                || line.starts_with("         ")
            {
                continue;
            }
            for tok in line
                .split("=>")
                .next()
                .unwrap_or("")
                .split('|')
                .map(str::trim)
            {
                if tok != "_"
                    && !tok.is_empty()
                    && tok
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
                {
                    armed.push(tok);
                }
            }
        }

        let decl = src
            .split_once(&format!("{export}: &[&str] = &["))
            .unwrap_or_else(|| panic!("{file}: no {export} declaration"))
            .1;
        let mut declared: Vec<&str> = decl[..decl.find("];").unwrap_or(0)]
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();

        armed.sort_unstable();
        armed.dedup();
        declared.sort_unstable();
        assert_eq!(armed, declared, "{file}: dispatch arms vs {export}");
    }

    /// One row per decoder whose export is a match dispatch. Every OTHER
    /// `DECODED_*` export names the test that pins it instead — a new one gets
    /// neither by default, and the census below is what refuses it.
    #[test]
    fn every_exported_set_is_exactly_its_dispatch_arms() {
        let rows = [
            (
                "acp.rs",
                "match str_field(\"sessionUpdate\").unwrap_or(\"\") {",
                "DECODED_TAGS",
            ),
            ("copilot.rs", "let out = match kind {", "DECODED_KINDS"),
            ("opencode.rs", "match event {", "DECODED_EVENTS"),
            ("omp.rs", "match ty {", "DECODED_HOOK_EVENTS"),
            (
                "claude_code.rs",
                ".and_then(|t| t.as_str())?\n    {",
                "DECODED_EFFORT_ATTACHMENTS",
            ),
        ];
        for (file, dispatch, export) in rows {
            assert_arms_match_export(file, dispatch, export);
        }

        // Each entry is the const and the test that pins it, so a renamed test
        // cannot leave a row still claiming coverage.
        const PINNED_ELSEWHERE: &[(&str, &str)] = &[
            (
                "DECODED_TERMINAL_STATUSES",
                "tool_call_starts_and_terminal_update_ends_keyed_by_tool_call_id",
            ),
            (
                "DECODED_TYPES",
                "the_decoded_set_is_exactly_what_the_arms_match",
            ),
            (
                "DECODED_FIELDS",
                "the_field_set_is_exactly_what_the_decoder_reads",
            ),
            ("DECODED_DATA_FIELDS", "every_decoded_data_field_is_read"),
            (
                "DECODED_DISPATCH_NAMES",
                "the_dispatch_name_set_is_exactly_what_the_fallback_matches",
            ),
            (
                "DECODED_ENTRY_TYPES",
                "the_decoded_entry_type_set_is_exactly_what_the_arms_match",
            ),
            ("DECODED_EXIT_MARKER", "session_exit_ends_root_not_as_child"),
            ("DECODED_ESCALATION", "escalated_function_call_is_waiting"),
            (
                "DECODED_MESSAGE_VOCAB",
                "the_exported_message_vocabulary_is_exactly_what_the_arms_match",
            ),
            (
                "DECODED_TITLE_FIELDS",
                "the_exported_title_field_set_is_exactly_what_both_readers_use",
            ),
            // grok's arms nest inside the method arm, so the shared dispatch
            // scanner above cannot reach them.
            (
                "DECODED_XAI_TAGS",
                "the_exported_xai_tag_set_is_exactly_what_the_arms_match",
            ),
        ];
        // Recursive: a const in a module directory would otherwise be
        // invisible to the census that exists to see it.
        fn collect(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("the source dir is readable") {
                let path = entry.expect("a readable entry").path();
                if path.is_dir() {
                    collect(&path, out);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("the module is readable");
                for line in src.lines() {
                    let Some((_, rest)) = line.split_once("const DECODED_") else {
                        continue;
                    };
                    if let Some((name, _)) = rest.split_once(':') {
                        out.push((format!("DECODED_{name}"), src.clone()));
                    }
                }
            }
        }
        let mut hits: Vec<(String, String)> = Vec::new();
        collect(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source"),
            &mut hits,
        );
        for (name, src) in &hits {
            if let Some((_, test)) = PINNED_ELSEWHERE.iter().find(|(n, _)| n == name) {
                let body = src
                    .split_once(&format!("fn {test}("))
                    .map(|(_, rest)| rest)
                    .unwrap_or("");
                assert!(
                    !body.is_empty(),
                    "{name} claims to be pinned by {test}, which its module no longer \
                     declares — the exemption is now an unbacked claim of coverage",
                );
                assert!(
                    body.split("\n    fn ").next().unwrap_or("").contains(name),
                    "{test} exists but never names {name}, so the exemption claims a \
                     coverage the test does not provide — have it read the const",
                );
            }
        }
        let mut found: Vec<String> = hits.into_iter().map(|(n, _)| n).collect();
        let mut covered: Vec<String> = rows
            .iter()
            .map(|(_, _, export)| (*export).to_owned())
            .chain(PINNED_ELSEWHERE.iter().map(|(e, _)| (*e).to_owned()))
            .collect();
        found.sort_unstable();
        found.dedup();
        covered.sort_unstable();
        covered.dedup();
        assert_eq!(
            found, covered,
            "a DECODED_* export needs a row above or a named pin in PINNED_ELSEWHERE — \
             or, the other direction, a row here outlived the const it named"
        );
    }
}
