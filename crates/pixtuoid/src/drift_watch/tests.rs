use std::cell::RefCell;
use std::collections::HashMap;

use super::*;

/// The network as a table: a URL it lacks is a transient failure, so a test
/// states every document it serves.
#[derive(Default)]
struct Served {
    bodies: HashMap<String, Result<Vec<u8>, u16>>,
    tarballs: HashMap<String, Vec<u8>>,
    calls: RefCell<Vec<(String, Duration)>>,
}

impl Served {
    fn with(mut self, url: &str, body: &str) -> Self {
        self.bodies
            .insert(url.to_owned(), Ok(body.as_bytes().to_vec()));
        self
    }

    fn gone(mut self, url: &str, status: u16) -> Self {
        self.bodies.insert(url.to_owned(), Err(status));
        self
    }

    /// `repo` as the resolver finds it: under its own name, its default
    /// branch `main`, its latest release `v1.0.0`, npm's `latest` `1.0.0`, and
    /// every tag present; a [`Release::Branch`] repository publishes none.
    fn resolving(self, repo: &Repo) -> Self {
        let api = format!("{API_URL}/{}", repo.name);
        let found = self.with(
            &api,
            &format!(
                r#"{{"full_name": "{}", "default_branch": "main"}}"#,
                repo.name
            ),
        );
        match repo.release {
            Release::GithubLatest => found.with(
                &format!("{api}/releases/latest"),
                r#"{"tag_name": "v1.0.0"}"#,
            ),
            Release::Npm {
                package,
                tag_prefix,
            } => found
                .with(
                    &format!("{NPM_URL}/{package}/latest"),
                    r#"{"version": "1.0.0"}"#,
                )
                .with(&format!("{api}/git/ref/tags/{tag_prefix}1.0.0"), "{}"),
            Release::Pinned { tag } => found.with(&format!("{api}/git/ref/tags/{tag}"), "{}"),
            Release::Branch => found.gone(&format!("{api}/releases/latest"), 404),
        }
    }

    /// `repo` resolving to its latest release `v1.0.0`, whose vouched tarball
    /// `asset` holds `text` at `member`.
    fn release(self, repo: &Repo, asset: &str, member: &str, sums: &str, text: &str) -> Self {
        self.resolving(repo)
            .release_files(repo.name, "v1.0.0", asset, member, sums, text)
    }

    fn release_files(
        mut self,
        repo: &str,
        tag: &str,
        asset: &str,
        member: &str,
        sums: &str,
        text: &str,
    ) -> Self {
        let base = format!("https://github.com/{repo}/releases/download/{tag}/");
        let name = asset.replace("{version}", tag.trim_start_matches('v'));
        let tarball = format!("TARBALL {repo} {name}");
        let digest = sha256_hex(tarball.as_bytes());
        self.tarballs
            .insert(format!("{tarball}#{member}"), text.as_bytes().to_vec());
        self.with(&format!("{base}{name}"), &tarball)
            .with(&format!("{base}{sums}"), &format!("{digest}  {name}\n"))
    }

    fn fetched(&self, url: &str) -> usize {
        self.calls.borrow().iter().filter(|(u, _)| u == url).count()
    }
}

impl Fetch for Served {
    fn get(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, FetchError> {
        self.calls.borrow_mut().push((url.to_owned(), timeout));
        match self.bodies.get(url) {
            Some(Ok(b)) => Ok(b.clone()),
            Some(Err(status)) => Err(FetchError::Gone {
                why: format!("HTTP {status}"),
            }),
            None => Err(FetchError::Transient(format!("not served: {url}"))),
        }
    }

    fn untar(&self, tarball: &[u8], member: &str) -> Result<Option<Vec<u8>>, FetchError> {
        let key = format!("{}#{member}", String::from_utf8_lossy(tarball));
        Ok(self.tarballs.get(&key).cloned())
    }
}

/// The repository most tests read, at its default branch.
const OR: Repo = Repo {
    name: "o/r",
    release: Release::Branch,
};

/// The network answering every question [`OR`] raises.
fn served() -> Served {
    Served::default().resolving(&OR)
}

fn raw(path: &str) -> String {
    format!("{RAW_URL}/o/r/refs/heads/main/{path}")
}

const ANCHORED: &str = r"(?m)^enum Events \{";

const fn doc(path: &'static str) -> Doc {
    OR.doc(path, ANCHORED, "`Events`")
}

const fn each(docs: &'static [Doc], pattern: &'static str) -> Watch {
    Watch {
        id: "t.each",
        ours: || vec!["a.b", "c"],
        docs,
        read: Read::Each { pattern },
        when: When::Vanishes,
        finding: Finding::Breaking("no sprite"),
        siblings: None,
    }
}

fn check(served: &Served, w: &Watch) -> Report {
    let mut report = Report::default();
    let mut fetched = Fetched::new(served);
    check_watch("src", w, &mut fetched, &mut report);
    report
}

#[test]
fn a_vanished_name_is_breaking_and_a_present_one_is_silent() {
    const W: Watch = each(&[doc("e.ts")], r#""{name}""#);
    let both = served().with(&raw("e.ts"), "enum Events {\n\"a.b\" \"c\"");
    let r = check(&both, &W);
    assert!(r.breaking.is_empty() && r.blind.is_empty(), "{r:?}");

    // `a.b` is escaped: a regex `.` would let `axb` stand in for it.
    let renamed = served().with(&raw("e.ts"), "enum Events {\n\"axb\" \"c\"");
    let r = check(&renamed, &W);
    assert_eq!(r.breaking.len(), 1, "{r:?}");
    assert!(
        !r.breaking[0].contains("there now"),
        "only a `RustStr` read names upstream's value"
    );
    assert!(
        r.breaking[0].contains("`a.b`") && r.breaking[0].contains("no sprite"),
        "{r:?}"
    );
}

#[test]
fn a_multi_document_watch_is_all_or_nothing() {
    const W: Watch = each(&[doc("1.ts"), doc("2.ts")], r#""{name}""#);
    // `c` lives only in the second half, so reading the first alone would
    // report it gone when the second merely failed to fetch.
    let half = served()
        .with(&raw("1.ts"), "enum Events {\n\"a.b\"")
        .gone(&raw("2.ts"), 404);
    let r = check(&half, &W);
    assert!(
        r.breaking.is_empty(),
        "a fetch failure is never a vanish: {r:?}"
    );
    assert_eq!(r.blind.len(), 1, "a gone pin is probe health: {r:?}");
    assert!(r.blind[0].contains("HTTP 404"), "{r:?}");

    let flaky = served().with(&raw("1.ts"), "enum Events {\n\"a.b\"");
    let r = check(&flaky, &W);
    assert!(r.breaking.is_empty() && r.blind.is_empty(), "{r:?}");
    assert_eq!(
        r.errors.len(),
        1,
        "a transient failure is an error line: {r:?}"
    );
}

#[test]
fn a_missing_anchor_is_probe_health_filed_once_per_document() {
    const W1: Watch = each(&[doc("e.ts")], r#""{name}""#);
    const W2: Watch = each(&[doc("e.ts")], r#"'{name}'"#);
    // A re-export facade: it fetches, and declares nothing.
    let facade = served().with(&raw("e.ts"), "export * from './events';");
    let mut report = Report::default();
    let mut fetched = Fetched::new(&facade);
    check_watch("src", &W1, &mut fetched, &mut report);
    check_watch("src", &W2, &mut fetched, &mut report);
    assert!(report.breaking.is_empty(), "{report:?}");
    assert_eq!(report.blind.len(), 1, "{report:?}");
    assert!(
        report.blind[0].contains("NOT evidence that upstream changed"),
        "{report:?}"
    );
    assert_eq!(
        facade.fetched(&raw("e.ts")),
        1,
        "a document is fetched once per run"
    );
}

#[test]
fn a_reader_that_finds_no_declaration_is_probe_health() {
    const W: Watch = Watch {
        id: "t.union",
        ours: || vec!["c"],
        docs: &[Doc {
            at: At::Path {
                repo: &OR,
                path: "s.json",
            },
            anchor: r#""Events""#,
            owns: "`Events`",
        }],
        read: Read::JsonUnion {
            def: "Events",
            prop: None,
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    // Anchored, and the union moved out from under the reader.
    let moved = served().with(&raw("s.json"), r#"{"Events": 1}"#);
    let r = check(&moved, &W);
    assert!(r.breaking.is_empty() && r.blind.len() == 1, "{r:?}");
    assert!(r.blind[0].contains("found no `Events` union"), "{r:?}");
}

/// A union read off the documents that parsed would hide whatever changed in
/// the one that did not, however many names the others still declare.
#[test]
fn one_unreadable_document_skips_the_whole_union() {
    const UNION: Doc = Doc {
        at: At::Path {
            repo: &OR,
            path: "a.json",
        },
        anchor: r#""Events""#,
        owns: "`Events`",
    };
    const W: Watch = Watch {
        id: "t.two",
        ours: || vec!["c"],
        docs: &[
            UNION,
            Doc {
                at: At::Path {
                    repo: &OR,
                    path: "b.json",
                },
                ..UNION
            },
        ],
        read: Read::JsonUnion {
            def: "Events",
            prop: None,
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let served = served()
        .with(
            &raw("a.json"),
            r#"{"$defs": {"Events": {"oneOf": [{"const": "c"}]}}}"#,
        )
        .with(&raw("b.json"), r#"{"Events": "reshaped"}"#);
    let r = check(&served, &W);
    assert!(r.breaking.is_empty() && r.blind.len() == 1, "{r:?}");
    assert!(
        r.blind[0].contains("b.json"),
        "the line names the document: {r:?}"
    );
}

/// A pattern or item spec of ours that cannot compile is ours to fix: the
/// upstream wording would send the maintainer to repin a working pin.
#[test]
fn a_broken_spec_of_ours_blames_ours() {
    const EACH: Watch = each(&[doc("e.ts")], r"({name}");
    const ARMS: Watch = Watch {
        id: "t.arms",
        ours: || vec!["start"],
        docs: &[doc("e.ts")],
        read: Read::RustArms {
            name: "parse",
            arm: "(",
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    const STR: Watch = Watch {
        id: "t.str",
        ours: || vec!["x"],
        docs: &[doc("e.ts")],
        read: Read::RustStr { item: "<<>" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    for w in [&EACH, &ARMS, &STR] {
        let r = check(&served().with(&raw("e.ts"), "enum Events {\n"), w);
        assert!(r.breaking.is_empty() && r.blind.len() == 1, "{r:?}");
        assert!(r.blind[0].contains("Our own declaration"), "{r:?}");
        assert!(
            r.blind[0].contains(&format!("`{}`'s pattern or item spec", w.id)),
            "{r:?}"
        );
        assert!(
            r.blind[0].contains(&format!("`{}` was SKIPPED", w.id)),
            "{r:?}"
        );
    }
}

/// A document is upstream's answer for the whole run, a failure included:
/// re-asking per watch would file the same dark pin once per watch and wait
/// out each timeout.
#[test]
fn a_document_is_fetched_once_per_run_and_a_failure_filed_once() {
    const W1: Watch = each(&[doc("e.ts")], r#""{name}""#);
    const W2: Watch = each(&[doc("e.ts")], r#"'{name}'"#);
    let text = "enum Events {\n  \"a.b\", \"c\", 'a.b', 'c'\n}\n";
    // (served, its blind lines, its error lines)
    for (served, blind, errors) in [
        (served().gone(&raw("e.ts"), 404), 1, 0),
        (served(), 0, 1),
        (served().with(&raw("e.ts"), text), 0, 0),
    ] {
        let mut report = Report::default();
        let mut fetched = Fetched::new(&served);
        check_watch("src", &W1, &mut fetched, &mut report);
        check_watch("src", &W2, &mut fetched, &mut report);
        assert_eq!(
            (
                report.blind.len(),
                report.errors.len(),
                report.breaking.len()
            ),
            (blind, errors, 0),
            "{report:?}"
        );
        assert_eq!(served.fetched(&raw("e.ts")), 1);
        assert!(
            served
                .calls
                .borrow()
                .iter()
                .all(|(_, t)| *t == FETCH_TIMEOUT)
        );
    }
}

#[test]
fn a_rust_str_read_names_what_upstream_declares_now() {
    const W: Watch = Watch {
        id: "t.str",
        ours: || vec!["agent"],
        docs: &[doc("t.rs")],
        read: Read::RustStr { item: "NAME" },
        when: When::Vanishes,
        finding: Finding::Breaking("no delegation"),
        siblings: None,
    };
    let renamed = served().with(&raw("t.rs"), "enum Events {}\nconst NAME: &str = \"task\";");
    let r = check(&renamed, &W);
    assert_eq!(r.breaking.len(), 1, "{r:?}");
    assert!(r.breaking[0].contains("declares `task` there now"), "{r:?}");
}

#[test]
fn a_ts_const_read_names_what_upstream_declares_now() {
    const W: Watch = Watch {
        id: "t.const",
        ours: || vec!["18789"],
        docs: &[doc("t.ts")],
        read: Read::TsConst { name: "PORT" },
        when: When::Vanishes,
        finding: Finding::Breaking("stale port"),
        siblings: None,
    };
    let reformatted = served().with(
        &raw("t.ts"),
        "enum Events {}\nexport const PORT: number =\n  18_789;\n",
    );
    let r = check(&reformatted, &W);
    assert!(r.breaking.is_empty() && r.blind.is_empty(), "{r:?}");
    let bumped = served().with(&raw("t.ts"), "enum Events {}\nexport const PORT = 18790;\n");
    let r = check(&bumped, &W);
    assert_eq!(r.breaking.len(), 1, "{r:?}");
    assert!(
        r.breaking[0].contains("declares `18790` there now"),
        "{r:?}"
    );
}

/// An early-return branch is the change: the whole list is upstream's answer.
#[test]
fn a_ts_return_read_names_what_upstream_returns_now() {
    const W: Watch = Watch {
        id: "t.return",
        ours: || vec!["dirs.agentDir"],
        docs: &[doc("t.ts")],
        read: Read::TsReturn { function: "f" },
        when: When::Vanishes,
        finding: Finding::Review("re-verify"),
        siblings: None,
    };
    let redirected = served().with(
        &raw("t.ts"),
        "enum Events {}\nexport function f() {\n  if (x) return y;\n  return dirs.agentDir;\n}\n",
    );
    let r = check(&redirected, &W);
    assert_eq!(r.review.len(), 1, "{r:?}");
    assert!(
        r.review[0].contains("returns `y; dirs.agentDir` there now"),
        "{r:?}"
    );
}

/// A name a read could not see the whole declaration for, through a base
/// type or a computed key, may be declared out of its sight: blind, never a
/// vanish.
#[test]
fn a_name_missing_from_a_partial_read_is_blind() {
    const W: Watch = Watch {
        id: "t.fields",
        ours: || vec!["a", "b"],
        docs: &[doc("t.ts")],
        read: Read::TsFields { name: "X" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let page = |decl: &str| served().with(&raw("t.ts"), &format!("enum Events {{}}\n{decl}\n"));
    let partial = check(
        &page("export interface X extends Base { a: string; c: string }"),
        &W,
    );
    assert!(partial.breaking.is_empty(), "{partial:?}");
    assert!(
        partial.blind.len() == 1
            && partial.blind[0].contains("`b`")
            && partial.blind[0].contains("extends another type"),
        "{partial:?}"
    );
    let whole = check(&page("export interface X { a: string; c: string }"), &W);
    assert!(
        whole.breaking.len() == 1 && whole.breaking[0].contains("`b`"),
        "{whole:?}"
    );
}

/// A partial read judges a fact of a field it sees: no base type adds a
/// literal back. A fact whose field it cannot see stays blind.
#[test]
fn a_partial_read_judges_a_fact_by_whether_its_field_is_seen() {
    const W: Watch = Watch {
        id: "t.facts",
        ours: || vec!["a:\"x\"", "b:\"y\""],
        docs: &[doc("t.ts")],
        read: Read::TsFields { name: "X" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let narrowed = served().with(
        &raw("t.ts"),
        "enum Events {}\nexport type X = Base & { a: \"y\" };\n",
    );
    let r = check(&narrowed, &W);
    assert!(
        r.blind.len() == 1 && r.blind[0].contains(r#"`b:"y"`"#),
        "{r:?}"
    );
    assert!(
        r.breaking.len() == 1 && r.breaking[0].contains(r#"`a:"x"`"#),
        "{r:?}"
    );
}

/// A later complete document leaves an earlier partial one's reason standing.
#[test]
fn a_partial_document_keeps_a_union_read_partial() {
    const W: Watch = Watch {
        id: "t.members",
        ours: || vec!["a", "b"],
        docs: &[doc("a.ts"), doc("b.ts")],
        read: Read::TsMembers { of: &["X"] },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let served = served()
        .with(
            &raw("a.ts"),
            "enum Events {}\ninterface X extends Base { a: string }\n",
        )
        .with(
            &raw("b.ts"),
            "enum Events {}\ninterface X { a: string; c: string }\n",
        );
    let r = check(&served, &W);
    assert!(r.breaking.is_empty(), "{r:?}");
    assert!(
        r.blind.len() == 1
            && r.blind[0].contains("`b`")
            && r.blind[0].contains("a.ts: `interface X` extends another type"),
        "{r:?}"
    );
}

/// The floor counts fields, never the facts beside them: a read that kept
/// `a` and `a?` kept one field of the two ours names.
#[test]
fn a_ts_fields_floor_counts_fields_not_their_facts() {
    const W: Watch = Watch {
        id: "t.fields",
        ours: || vec!["a", "b"],
        docs: &[doc("t.ts")],
        read: Read::TsFields { name: "X" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let truncated = served().with(
        &raw("t.ts"),
        "enum Events {}\nexport interface X { a?: string }\n",
    );
    let r = check(&truncated, &W);
    assert!(r.breaking.is_empty(), "{r:?}");
    assert!(
        r.blind.len() == 1 && r.blind[0].contains("floor 2"),
        "{r:?}"
    );
}

#[test]
fn the_floor_skips_a_degraded_parse_but_not_a_family_reader() {
    const SCHEMA: &str = r#"{"$defs": {"Events": {"oneOf": [{"const": "c"}]}}}"#;
    const DOC: Doc = Doc {
        at: At::Path {
            repo: &OR,
            path: "s.json",
        },
        anchor: r#""Events""#,
        owns: "`Events`",
    };
    const UNION: Watch = Watch {
        id: "t.union",
        ours: || vec!["a.b", "c"],
        docs: &[DOC],
        read: Read::JsonUnion {
            def: "Events",
            prop: None,
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    // One name back where we read two: the reader degraded, nothing is renamed.
    let r = check(&served().with(&raw("s.json"), SCHEMA), &UNION);
    assert!(r.breaking.is_empty() && r.blind.len() == 1, "{r:?}");
    assert!(r.blind[0].contains("floor 2"), "{r:?}");

    // An enum read is floored alike: one variant back where we read two.
    const ENUM: Watch = Watch {
        id: "t.enum",
        ours: || vec!["a_b", "c"],
        docs: &[doc("e.rs")],
        read: Read::RustEnum { name: "Item" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let one_variant =
        "enum Events {}\n#[serde(rename_all = \"snake_case\")]\nenum Item {\n    C,\n}\n";
    let r = check(&served().with(&raw("e.rs"), one_variant), &ENUM);
    assert!(r.breaking.is_empty() && r.blind.len() == 1, "{r:?}");
    assert!(r.blind[0].contains("floor 2"), "{r:?}");

    // A family reader yields exactly the family, so its shrink is the finding.
    const ARMS: Watch = Watch {
        id: "t.arms",
        ours: || vec!["start", "wait"],
        docs: &[doc("a.rs")],
        read: Read::RustArms {
            name: "parse",
            arm: r"Ok\(Go\)",
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let src = "enum Events {}\nfn parse() { match s { \"start\" => Ok(Go), _ => Err(()) } }";
    let r = check(&served().with(&raw("a.rs"), src), &ARMS);
    assert!(r.blind.is_empty(), "{r:?}");
    assert!(
        r.breaking.len() == 1 && r.breaking[0].contains("`wait`"),
        "{r:?}"
    );
}

#[test]
fn a_sibling_in_the_family_is_reviewed_and_a_stale_ignore_is_reported() {
    const SCHEMA: &str = r#"{"$defs": {"Events": {"oneOf": [
        {"const": "tool_call"}, {"const": "tool_call_update"},
        {"const": "tool_call_cancelled"}, {"const": "tool_call_deferred"},
        {"const": "tool_call_`](https://x)\u202e"},
        {"const": "plan"}, {"const": "agent_message"}]}}}"#;
    const W: Watch = Watch {
        id: "t.sib",
        ours: || vec!["tool_call", "tool_call_update"],
        docs: &[Doc {
            at: At::Path {
                repo: &OR,
                path: "s.json",
            },
            anchor: r#""Events""#,
            owns: "`Events`",
        }],
        read: Read::JsonUnion {
            def: "Events",
            prop: None,
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: Some(Siblings {
            family: Family::Prefix("tool_call"),
            ignored: &[
                ("tool_call_deferred", "decided"),
                ("tool_call_progress", "gone upstream"),
            ],
        }),
    };
    let r = check(&served().with(&raw("s.json"), SCHEMA), &W);
    assert!(r.breaking.is_empty() && r.blind.is_empty(), "{r:?}");
    assert_eq!(r.review.len(), 3, "{r:?}");
    // A fetched name stays inside its code span, its controls dropped.
    assert!(
        r.review
            .iter()
            .any(|l| l.contains("`tool_call_](https://x)`") && !l.contains('\u{202e}')),
        "{r:?}"
    );
    assert!(
        r.review.iter().any(|l| l.contains("`tool_call_cancelled`")),
        "{r:?}"
    );
    assert!(
        r.review
            .iter()
            .any(|l| l.contains("still ignores `tool_call_progress`")),
        "{r:?}"
    );
    assert!(
        !r.review.iter().any(|l| l.contains("`tool_call_deferred`")),
        "an ignored sibling goes quiet: {r:?}"
    );
    // A stranger to the family is mere growth.
    assert!(!r.review.iter().any(|l| l.contains("`plan`")), "{r:?}");
}

#[test]
fn a_release_member_is_read_only_once_its_sums_vouch_for_it() {
    const MEMBER: &str = "pkg/schema.json";
    const RELEASED: Repo = Repo {
        name: "o/r",
        release: Release::GithubLatest,
    };
    const W: Watch = Watch {
        id: "t.asset",
        ours: || vec!["a.b"],
        docs: &[Doc {
            at: At::ReleaseAsset {
                repo: &RELEASED,
                asset: "cli-{version}-linux.tgz",
                member: MEMBER,
                sums: "SUMS.txt",
            },
            anchor: r#""Events""#,
            owns: "`Events`",
        }],
        read: Read::JsonKeys,
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let schema = r#"{"Events": 1, "properties": {"a.b": {}}}"#;
    let base = "https://github.com/o/r/releases/download/v1.0.0/";
    let tarball = format!("{base}cli-1.0.0-linux.tgz");
    let ok = served().release(
        &RELEASED,
        "cli-{version}-linux.tgz",
        MEMBER,
        "SUMS.txt",
        schema,
    );
    let r = check(&ok, &W);
    assert!(
        r.breaking.is_empty() && r.blind.is_empty() && r.errors.is_empty(),
        "{r:?}"
    );
    let long: Vec<_> = ok
        .calls
        .borrow()
        .iter()
        .filter(|(_, t)| *t == RELEASE_ASSET_TIMEOUT)
        .map(|(u, _)| u.clone())
        .collect();
    assert_eq!(long, [tarball], "only the tarball waits long");

    let unvouched = served()
        .release(
            &RELEASED,
            "cli-{version}-linux.tgz",
            MEMBER,
            "SUMS.txt",
            schema,
        )
        .with(&format!("{base}SUMS.txt"), "0000  cli-1.0.0-linux.tgz\n");
    let r = check(&unvouched, &W);
    assert!(
        r.blind.is_empty() && r.errors.len() == 1,
        "a sums miss is retried: {r:?}"
    );
    // The right digest vouches only for the name it is listed under.
    let digest = sha256_hex(b"TARBALL o/r cli-1.0.0-linux.tgz");
    let misnamed = served()
        .release(
            &RELEASED,
            "cli-{version}-linux.tgz",
            MEMBER,
            "SUMS.txt",
            schema,
        )
        .with(
            &format!("{base}SUMS.txt"),
            &format!("{digest}  other.tgz\n"),
        );
    let r = check(&misnamed, &W);
    assert!(r.errors.len() == 1, "{r:?}");

    let mut moved = served().release(
        &RELEASED,
        "cli-{version}-linux.tgz",
        MEMBER,
        "SUMS.txt",
        schema,
    );
    moved.tarballs.clear();
    let r = check(&moved, &W);
    assert!(
        r.errors.is_empty() && r.blind.len() == 1,
        "a missing member darkens the pin: {r:?}"
    );
    assert!(
        r.blind[0].contains("no pkg/schema.json in cli-1.0.0-linux.tgz")
            && !r.blind[0].contains("HTTP"),
        "no request 404'd, so the line says what is missing: {r:?}"
    );

    let tagless = served()
        .release(
            &RELEASED,
            "cli-{version}-linux.tgz",
            MEMBER,
            "SUMS.txt",
            schema,
        )
        .with(&format!("{API_URL}/o/r/releases/latest"), "{}");
    let r = check(&tagless, &W);
    assert!(
        r.blind.is_empty() && r.errors.len() == 1,
        "a tagless reply is retried: {r:?}"
    );
}

/// FIPS 180-2's "abc" vector, whose bytes below 0x10 catch a dropped pad.
#[test]
fn a_digest_reads_as_a_sums_file_lists_it() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Each [`Release`] is read at the ref users run, named in the report.
#[test]
fn each_release_resolves_to_the_ref_users_run() {
    const LATEST: Repo = Repo {
        name: "o/latest",
        release: Release::GithubLatest,
    };
    const NPM: Repo = Repo {
        name: "o/npm",
        release: Release::Npm {
            package: "@o/cli",
            tag_prefix: "cli-v",
        },
    };
    const PINNED: Repo = Repo {
        name: "o/pinned",
        release: Release::Pinned { tag: "v0.1.0" },
    };
    let found = |repo: &Repo| {
        Served::default().with(
            &format!("{API_URL}/{}", repo.name),
            &format!(
                r#"{{"full_name": "{}", "default_branch": "trunk"}}"#,
                repo.name
            ),
        )
    };
    for (repo, served, at_ref) in [
        (
            &LATEST,
            found(&LATEST).with(
                &format!("{API_URL}/o/latest/releases/latest"),
                r#"{"tag_name": "v2.0.0"}"#,
            ),
            "refs/tags/v2.0.0",
        ),
        (
            &NPM,
            found(&NPM)
                .with(
                    &format!("{NPM_URL}/@o/cli/latest"),
                    r#"{"version": "3.1.0"}"#,
                )
                .with(&format!("{API_URL}/o/npm/git/ref/tags/cli-v3.1.0"), "{}"),
            "refs/tags/cli-v3.1.0",
        ),
        (
            &PINNED,
            found(&PINNED).with(&format!("{API_URL}/o/pinned/git/ref/tags/v0.1.0"), "{}"),
            "refs/tags/v0.1.0",
        ),
        (
            &OR,
            found(&OR).gone(&format!("{API_URL}/o/r/releases/latest"), 404),
            "refs/heads/trunk",
        ),
    ] {
        let mut report = Report::default();
        let located = Fetched::new(&served).locate(&repo.doc("e.ts", "x", "x").at, &mut report);
        let Some((Located::Raw { url }, label)) = located else {
            panic!("{}: unresolved: {report:?}", repo.name);
        };
        assert_eq!(url, format!("{RAW_URL}/{}/{at_ref}/e.ts", repo.name));
        let short = at_ref.rsplit('/').next().unwrap_or_default();
        assert_eq!(label, format!("{}@{short}:e.ts", repo.name));
        assert!(
            report.blind.is_empty() && report.errors.is_empty() && report.review.is_empty(),
            "{report:?}"
        );
    }
}

/// A moved repository is never followed: its documents stay unread until it
/// is repinned, and only a case change is the same name.
#[test]
fn a_moved_repository_is_probe_health_until_repinned() {
    const W: Watch = each(&[doc("e.ts")], r#""{name}""#);
    let text = "enum Events {\n\"a.b\" \"c\"";
    let moved = served()
        .with(
            &format!("{API_URL}/o/r"),
            r#"{"full_name": "n/r", "default_branch": "main"}"#,
        )
        .with(&raw("e.ts"), text);
    let r = check(&moved, &W);
    assert!(
        r.breaking.is_empty() && r.errors.is_empty() && r.blind.len() == 1,
        "{r:?}"
    );
    assert!(r.blind[0].contains("moved to `n/r`"), "{r:?}");
    assert_eq!(moved.fetched(&raw("e.ts")), 0, "{r:?}");

    let recased = served()
        .with(
            &format!("{API_URL}/o/r"),
            r#"{"full_name": "O/R", "default_branch": "main"}"#,
        )
        .with(&raw("e.ts"), text);
    let r = check(&recased, &W);
    assert!(r.blind.is_empty() && r.errors.is_empty(), "{r:?}");
}

/// A release policy that resolves no tag is probe health, named per cause.
#[test]
fn a_release_that_resolves_no_tag_is_probe_health() {
    const LATEST: Repo = Repo {
        name: "o/r",
        release: Release::GithubLatest,
    };
    const NPM: Repo = Repo {
        name: "o/r",
        release: Release::Npm {
            package: "x",
            tag_prefix: "v",
        },
    };
    const PINNED: Repo = Repo {
        name: "o/r",
        release: Release::Pinned { tag: "v0.1.0" },
    };
    for (repo, served, says) in [
        (
            &LATEST,
            served().gone(&format!("{API_URL}/o/r/releases/latest"), 404),
            "No release resolves for it",
        ),
        (
            &NPM,
            served()
                .with(&format!("{NPM_URL}/x/latest"), r#"{"version": "1.2.0"}"#)
                .gone(&format!("{API_URL}/o/r/git/ref/tags/v1.2.0"), 404),
            "which it does not tag `v1.2.0`",
        ),
        (
            &PINNED,
            served().gone(&format!("{API_URL}/o/r/git/ref/tags/v0.1.0"), 404),
            "Its pinned tag `v0.1.0` is gone",
        ),
    ] {
        let mut report = Report::default();
        let mut fetched = Fetched::new(&served);
        let at = repo.doc("e.ts", "x", "x").at;
        assert!(fetched.locate(&at, &mut report).is_none());
        assert!(fetched.locate(&at, &mut report).is_none());
        assert!(
            report.errors.is_empty() && report.blind.len() == 1,
            "filed once per run: {report:?}"
        );
        assert!(report.blind[0].contains(says), "{report:?}");
    }
}

/// A repository read at its default branch is reviewed the run it first
/// publishes a release, and resolved once however many documents it holds.
#[test]
fn a_branch_repository_that_starts_releasing_is_reviewed() {
    const W1: Watch = each(&[doc("e.ts")], r#""{name}""#);
    const W2: Watch = each(&[doc("f.ts")], r#""{name}""#);
    let text = "enum Events {\n\"a.b\" \"c\"";
    let served = served()
        .with(
            &format!("{API_URL}/o/r/releases/latest"),
            r#"{"tag_name": "v0.1.0"}"#,
        )
        .with(&raw("e.ts"), text)
        .with(&raw("f.ts"), text);
    let mut report = Report::default();
    let mut fetched = Fetched::new(&served);
    check_watch("src", &W1, &mut fetched, &mut report);
    check_watch("src", &W2, &mut fetched, &mut report);
    assert!(
        report.breaking.is_empty() && report.blind.is_empty() && report.errors.is_empty(),
        "{report:?}"
    );
    assert_eq!(report.review.len(), 1, "{report:?}");
    assert!(
        report.review[0].contains("`o/r` now publishes `v0.1.0`"),
        "{report:?}"
    );
    assert_eq!(served.fetched(&format!("{API_URL}/o/r")), 1);
}

/// A declared pattern as text it matches, `{name}` and its one capture group
/// standing for `name`, an alternation for its first branch. A construct it
/// cannot render fails the test, which keeps declared patterns to shapes a
/// reader can tell names apart in.
fn render(pattern: &str, name: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        // A `{n}` or `{n,m}` count repeats what came before it to its least.
        let count = rest
            .strip_prefix('{')
            .and_then(|r| r.split_once('}'))
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == ','));
        if let Some((n, _)) = count {
            let least: usize = n.split(',').next().unwrap_or("1").parse().unwrap_or(1);
            let last = out.chars().last().unwrap_or_default();
            out.extend(std::iter::repeat_n(last, least.saturating_sub(1)));
            rest = &rest[n.len() + 2..];
            continue;
        }
        let (emit, used) = if let Some(alts) = rest.strip_prefix("(?:") {
            let end = alts
                .find(')')
                .unwrap_or_else(|| panic!("unclosed group in {pattern}"));
            let first = alts[..end].split('|').next().unwrap_or_default();
            (render(first, name), 3 + end + 1)
        } else {
            let fixed: &[(&str, &str)] = &[
                ("{name}", name),
                (r"(\w+)", name),
                (r#"([^"]+)"#, name),
                (r"\w+", "w"),
                ("(?m)", ""),
                ("^", "\n"),
                (r"\s*", ""),
                (r"\s+", " "),
                (r"\b", ""),
            ];
            match fixed.iter().find(|(p, _)| rest.starts_with(p)) {
                Some((p, e)) => ((*e).to_owned(), p.len()),
                None => {
                    let c = rest.chars().next().unwrap_or_default();
                    if let Some(escaped) = rest.strip_prefix('\\') {
                        let e = escaped.chars().next().unwrap_or_default();
                        assert!(
                            e.is_ascii_punctuation(),
                            "cannot render `\\{e}` in {pattern}"
                        );
                        (e.to_string(), 1 + e.len_utf8())
                    } else {
                        assert!(
                            !"[]()*+?|.$".contains(c),
                            "cannot render `{c}` in {pattern}"
                        );
                        (c.to_string(), c.len_utf8())
                    }
                }
            }
        };
        out.push_str(&emit);
        rest = &rest[used..];
    }
    out
}

#[test]
fn rendering_a_pattern_yields_text_it_matches() {
    for (pattern, name) in [
        (r#""x-method"\s*:\s*"{name}""#, "session/update"),
        (r"Schema\.Literal\({name}\)", "a.b"),
        (r"(?m)^export const \b{name} = \{", "Event"),
        (r#"(?m)^version = "([^"]+)""#, "0.11.4"),
    ] {
        let text = render(pattern, name);
        let re = Regex::new(&pattern.replace("{name}", &regex::escape(name))).expect("compiles");
        assert!(re.is_match(&text), "{pattern} renders {text:?}");
    }
}

/// A page declaring `names` the way `w` reads them, its anchors included.
fn page(w: &Watch, names: &[&str]) -> String {
    let anchors: Vec<String> = w.docs.iter().map(|doc| render(doc.anchor, "")).collect();
    match w.read {
        Read::Each { pattern } => {
            let decls: Vec<String> = names.iter().map(|n| render(pattern, n)).collect();
            format!("{}\n{}\n", anchors.join("\n"), decls.join("\n"))
        }
        Read::RustArms { name, arm } => {
            let arms: String = names
                .iter()
                .map(|n| format!("\"{n}\" => {},\n", render(arm, "")))
                .collect();
            after_decl(
                w,
                format!("fn {name}() {{ match a {{\n{arms} _ => Err(()) }} }}"),
            )
        }
        Read::RustStr { item } => {
            let value = names.first().copied().unwrap_or_default();
            let decl = match item.strip_prefix('<').and_then(|i| i.split_once(">::")) {
                Some((ty_trait, method)) => {
                    let (ty, tr) = ty_trait
                        .split_once(" as ")
                        .expect("`<Type as Trait>::method`");
                    format!(
                        "impl {tr} for {ty} {{\n    fn {method}(&self) -> &'static str {{\n        \"{value}\"\n    }}\n}}"
                    )
                }
                None => format!("const {item}: &str = \"{value}\";"),
            };
            after_decl(w, decl)
        }
        // The declaration first: an anchor that names the enum would otherwise
        // be read as its opening, attributes and all.
        Read::RustEnum { name } => {
            let variants: String = names
                .iter()
                .map(|n| format!("    {},\n", pascal(n)))
                .collect();
            after_decl(
                w,
                format!(
                    "#[serde(tag = \"type\", rename_all = \"snake_case\")]\npub enum {name} {{\n{variants}}}"
                ),
            )
        }
        Read::RustStruct { name } => {
            let fields: String = names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    format!("    #[serde(rename = \"{n}\", default)]\n    pub pxd_{i}: Vec<u8>,\n")
                })
                .collect();
            after_decl(w, format!("pub struct {name} {{\n{fields}}}"))
        }
        Read::TsConstArray { name } => {
            let members: String = names.iter().map(|n| format!("  \"{n}\",\n")).collect();
            after_decl(w, format!("const {name} = [\n{members}] as const;"))
        }
        // A number reads as one, and anything else, a vanished value's empty
        // one included, as a string.
        Read::TsConst { name } => {
            let value = names.first().copied().unwrap_or_default();
            let literal = if value.parse::<f64>().is_ok() {
                value.to_owned()
            } else {
                format!("\"{value}\"")
            };
            after_decl(w, format!("export const {name} = {literal};"))
        }
        // A field of its own besides: an empty interface is no declaration
        // the reader reads, and an `Appears` page names none of ours.
        Read::TsFields { name } => {
            let fields: String = names.iter().map(|n| ts_field(n)).collect();
            after_decl(
                w,
                format!("export interface {name} {{\n  pxd: string;\n{fields}}}"),
            )
        }
        Read::TsMembers { of } => {
            let members: String = names
                .iter()
                .map(|n| format!("  \"{n}\": string;\n"))
                .collect();
            after_decl(w, format!("export interface {} {{\n{members}}}", of[0]))
        }
        Read::TsReturn { function } => {
            let value = names.first().copied().unwrap_or_default();
            after_decl(
                w,
                format!("export function {function}() {{\n  return {value};\n}}"),
            )
        }
        // A JSON document's anchor renders as a key, so each one opens a member.
        Read::JsonUnion { def, prop } => {
            let members: Vec<serde_json::Value> = names
                .iter()
                .map(|n| match prop {
                    Some(p) => serde_json::json!({"properties": {p: {"const": n}}}),
                    None => serde_json::json!({"const": n}),
                })
                .collect();
            let defs = serde_json::json!({ def: {"oneOf": members} });
            let keys: String = anchors.iter().map(|a| format!("{a} 0, ")).collect();
            format!("{{{keys}\"$defs\": {defs}}}")
        }
        Read::JsonKeys => {
            let props: serde_json::Map<String, serde_json::Value> = names
                .iter()
                .map(|n| ((*n).to_owned(), serde_json::json!({})))
                .collect();
            let keys: String = anchors.iter().map(|a| format!("{a} 0, ")).collect();
            format!(
                "{{{keys}\"properties\": {}}}",
                serde_json::Value::Object(props)
            )
        }
        Read::Matches { pattern } => {
            let decls: Vec<String> = names.iter().map(|n| render(pattern, n)).collect();
            format!("{}\n{}\n", anchors.join("\n"), decls.join("\n"))
        }
        Read::MdTable { header, under } => {
            let rows: String = names.iter().map(|n| format!("| `{n}` | x |\n")).collect();
            after_decl(
                w,
                format!("## {under}\n| {header} | When |\n|---|---|\n{rows}"),
            )
        }
        Read::MdHeadings { under } => {
            let heads: String = names.iter().map(|n| format!("### {n}\n")).collect();
            after_decl(w, format!("## {under}\n{heads}"))
        }
        Read::PySet { decl } => {
            let members: String = names.iter().map(|n| format!("    \"{n}\",\n")).collect();
            // A `frozenset(` opener closes its call after the set.
            let close = if decl.ends_with(r"\(") { ")" } else { "" };
            format!(
                "{}\n{} {{\n{members}}}{close}\n",
                anchors.join("\n"),
                render(decl, "")
            )
        }
        Read::RustMacroRows { name } => {
            let rows: String = names
                .iter()
                .map(|n| format!("    {n} {{ alias: \"x\" }},\n"))
                .collect();
            after_decl(w, format!("{name}! {{\n{rows}}}"))
        }
    }
}

/// `decl`, then each of `w`'s anchors it does not already carry: rendering a
/// declaration's own head again would declare it twice. In Rust and TypeScript
/// source the anchors sit in a block comment, which the anchor check still sees
/// and the parser skips; Markdown has none, so its anchors follow as plain lines.
fn after_decl(w: &Watch, decl: String) -> String {
    let rest: Vec<String> = w
        .docs
        .iter()
        .filter(|doc| !Regex::new(doc.anchor).expect("compiles").is_match(&decl))
        .map(|doc| render(doc.anchor, ""))
        .collect();
    if rest.is_empty() || matches!(w.read, Read::MdTable { .. } | Read::MdHeadings { .. }) {
        format!("{decl}\n{}\n", rest.join("\n"))
    } else {
        format!("{decl}\n/*\n{}\n*/\n", rest.join("\n"))
    }
}

/// The variant a snake_case wire name derives from.
fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// The field that yields TsFields fact `n`: `f?` an optional field, `f:"lit"`
/// a literal type, a decoy's suffix kept inside the quotes.
fn ts_field(n: &str) -> String {
    let (key, optional, literal) = match (n.split_once(":\""), n.strip_suffix('?')) {
        (Some((f, lit)), _) => (f, "", format!("\"{}\"", lit.replace('"', ""))),
        (None, Some(f)) => (f, "?", "string".to_owned()),
        (None, None) => (n, "", "string".to_owned()),
    };
    format!("  \"{key}\"{optional}: {literal};\n")
}

/// `name` where `w`'s reader must not find it: another item of the reader's
/// kind. A plain-text pattern's strictness cannot be read off the pattern under
/// test, so `a_declaration_form_is_not_satisfied_by_prose` pins it where a
/// declaration names the prose it must reject, and a JSON reader reads its
/// whole document by design.
fn near_miss(w: &Watch, name: &str) -> String {
    match w.read {
        Read::RustStr { .. } => format!("const PXD_OTHER: &str = \"{name}\";"),
        Read::TsConst { .. } => format!("export const PXD_OTHER = \"{name}\";"),
        Read::RustArms { arm, .. } => format!(
            "fn pxd_other() {{ match a {{ \"{name}\" => {}, _ => Err(()) }} }}",
            render(arm, "")
        ),
        Read::RustEnum { .. } => format!(
            "#[serde(rename_all = \"snake_case\")]\nenum PxdOther {{\n    {},\n}}",
            pascal(name)
        ),
        Read::RustStruct { .. } => {
            format!(
                "pub struct PxdOther {{\n    #[serde(rename = \"{name}\")]\n    pub pxd: u8,\n}}"
            )
        }
        Read::TsFields { .. } => format!("export interface PxdOther {{\n{}}}", ts_field(name)),
        Read::TsMembers { .. } => {
            format!("export interface PxdOther {{\n  \"{name}\": string;\n}}")
        }
        Read::TsReturn { .. } => format!("function pxdOther() {{\n  return {name};\n}}"),
        Read::TsConstArray { .. } => {
            format!("const PXD_OTHER = [\n  \"{name}\",\n];\nif (n === \"{name}\") {{}}\n")
        }
        Read::MdTable { header, .. } => {
            format!("## PxdOther\n| {header} | When |\n|---|---|\n| `{name}` | x |\n")
        }
        Read::MdHeadings { .. } => format!("## PxdOther\n### {name}\n"),
        Read::PySet { .. } => format!("PXD_OTHER = {{\"{name}\"}}\n"),
        Read::RustMacroRows { .. } => format!("pxd_other! {{\n    {name} {{ }},\n}}\n"),
        Read::Each { .. } | Read::Matches { .. } | Read::JsonUnion { .. } | Read::JsonKeys => {
            String::new()
        }
    }
}

/// `text` as every document `w` reads, where the engine itself locates it.
fn serve(w: &Watch, text: &str) -> Served {
    let resolving = w
        .docs
        .iter()
        .fold(Served::default(), |s, doc| match doc.at {
            At::Path { repo, .. } | At::ReleaseAsset { repo, .. } => s.resolving(repo),
            At::Url(_) => s,
        });
    let mut report = Report::default();
    let mut fetched = Fetched::new(&resolving);
    let located: Vec<Located> = w
        .docs
        .iter()
        .map(|doc| {
            let (l, _) = fetched
                .locate(&doc.at, &mut report)
                .expect("the fake resolves it");
            l
        })
        .collect();
    let served = located.into_iter().fold(resolving, |s, l| match l {
        Located::Raw { url } => s.with(&url, text),
        Located::Release {
            repo,
            tag,
            asset,
            member,
            sums,
        } => s.release_files(repo, &tag, asset, member, sums, text),
    });
    served.calls.borrow_mut().clear();
    served
}

/// A watch read in its declaration form must not take the name's mention
/// elsewhere for it: each prose shape here is the one its declaration's
/// comment warns of.
#[test]
fn a_declaration_form_is_not_satisfied_by_prose() {
    for (id, prose) in [
        ("omp.hook_events", r#"/** Emitted as "{name}". */"#),
        (
            "opencode.hook_events",
            r#"expect(event.type).toBe("{name}")"#,
        ),
        ("omp.entry_types", "// a {name} entry, written once"),
        ("codex.escalation", "/// set `{name}` to escalate"),
    ] {
        let w = pixtuoid_core::source::registry::declared_watch(id)
            .unwrap_or_else(|| panic!("{id} is declared"));
        let ours = (w.ours)();
        let victim = ours[0];
        let text = format!(
            "{}\n{}\n",
            page(w, &ours[1..]),
            prose.replace("{name}", victim)
        );
        let mut report = Report::default();
        check_watch("src", w, &mut Fetched::new(&serve(w, &text)), &mut report);
        assert!(
            report
                .breaking
                .iter()
                .any(|l| l.contains(&format!("`{victim}`"))),
            "{id}: prose kept `{victim}` alive: {report:?}"
        );
    }
}

/// #941's gate, generic: every watch fires on its change and stays silent
/// otherwise. A vanished name leaves a decoy in its own shape and stays in
/// [`near_miss`]'s positions, so a pattern or reader loose enough to find
/// either cannot pass.
#[test]
fn every_watch_fires_on_its_change_and_stays_silent_otherwise() {
    let mut proven = 0;
    for (source, w) in declared() {
        {
            let ours = (w.ours)();
            let victim = ours[0];
            let run_on = |names: &[&str], absent: Option<&str>| {
                let miss = absent.map(|n| near_miss(w, n)).unwrap_or_default();
                let served = serve(w, &format!("{}\n{miss}\n", page(w, names)));
                let mut report = Report::default();
                let mut fetched = Fetched::new(&served);
                check_watch(source, w, &mut fetched, &mut report);
                report
            };
            let fired = |r: &Report| {
                let filed = match w.finding {
                    Finding::Breaking(_) => &r.breaking,
                    Finding::Review(_) => &r.review,
                };
                filed.iter().any(|l| l.contains(&format!("`{victim}`")))
            };
            let decoy = format!("{victim}_pxd");
            let (quiet, changed) = match w.when {
                When::Vanishes => {
                    let mut kept: Vec<&str> = ours[1..].to_vec();
                    kept.push(&decoy);
                    (run_on(&ours, None), run_on(&kept, Some(victim)))
                }
                When::Appears => (run_on(&[], Some(victim)), run_on(&[victim], None)),
            };
            assert!(
                quiet.breaking.is_empty() && quiet.blind.is_empty() && !fired(&quiet),
                "{}: an unchanged document stays silent: {quiet:?}",
                w.id
            );
            assert!(
                fired(&changed),
                "{}: a changed `{victim}` fires: {changed:?}",
                w.id
            );
            proven += 1;
        }
    }
    assert!(proven > 0, "the registry declares watches");
}

#[test]
fn the_report_title_carries_the_strongest_disposition_and_the_exit_code() {
    for file in [Report::add_breaking, Report::add_review] {
        let mut one = Report::default();
        file(&mut one, "L".to_owned());
        assert_eq!(one.exit_code(), 1);
    }
    let mut r = Report::default();
    assert_eq!(
        (r.title(), r.exit_code()),
        ("Upstream wire-format watch: no drift", 0)
    );
    assert!(r.render().contains("✅ No drift"));
    r.add_error("E".to_owned());
    assert_eq!(r.exit_code(), 2, "transient alone is warn-only");
    r.add_blind("W", "AT", "C", false);
    assert_eq!(
        (r.title(), r.exit_code()),
        ("Upstream drift watch could not verify — repin needed", 1),
        "a dark watch is actionable (#454)"
    );
    r.add_review("R".to_owned());
    assert_eq!(r.title(), "Upstream changes to review");
    r.add_breaking("B".to_owned());
    assert_eq!(r.title(), "Upstream CLI wire-format drift detected");
    let out = r.render();
    assert!(
        out.starts_with("# Upstream CLI wire-format drift detected\n"),
        "{out}"
    );
    let at = |s: &str| out.find(s).unwrap_or(usize::MAX);
    assert!(
        at("- B") < at("- R") && at("- R") < at("- could not") && at("- could not") < at("- E")
    );
}

/// What a shipped template reads is what its watches read. omp, both ways: a
/// read added without its watch, or a watch outliving its read, fails here.
/// OpenClaw and dsh, one way: each field, literal and id their consts name is
/// still read, compared or forwarded; a new read is not detected. dsh's event
/// watch reads its scan itself.
#[test]
fn each_template_read_is_watched() {
    use crate::install::{dsh, omp, openclaw};
    let bare = |watches: &[Watch]| -> BTreeSet<&str> {
        watches
            .iter()
            .flat_map(|w| (w.ours)())
            .filter(|n| read::is_bare(n))
            .collect()
    };
    for field in dsh::TOOL_RESULT_FIELDS {
        let read = format!("d.message?.{field}");
        assert!(
            dsh::PLUGIN_TEMPLATE.contains(&read),
            "the dsh plugin no longer reads `{read}`"
        );
    }
    let omp_reads: BTreeSet<&str> = omp::extension_reads().into_iter().collect();
    assert!(!omp_reads.is_empty(), "the template scan reads nothing");
    assert_eq!(omp_reads, bare(omp::UPSTREAM));
    let plugin_reads = |read: &str| {
        openclaw::PLUGIN_TEMPLATE.match_indices(read).any(|(i, _)| {
            !openclaw::PLUGIN_TEMPLATE[i + read.len()..]
                .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        })
    };
    let fields = [openclaw::AGENT_END_FIELDS, openclaw::MODEL_CALL_BASE_FIELDS].concat();
    for field in fields {
        assert!(
            plugin_reads(&format!("ev.{field}")),
            "the plugin no longer reads `{field}`"
        );
    }
    for field in openclaw::PORT_FIELDS {
        for read in [format!("ev.{field}"), format!("ctx.{field}")] {
            assert!(plugin_reads(&read), "the plugin no longer reads `{read}`");
        }
    }
    let allow = openclaw::PLUGIN_TEMPLATE
        .split_once("const ALLOW = [")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map_or("", |(list, _)| list);
    for id in openclaw::RUN_KEY_FIELDS {
        assert!(
            allow.contains(&format!("\"{id}\"")),
            "the plugin no longer forwards `{id}`"
        );
    }
    for fact in openclaw::MODEL_CALL_ENDED_FACTS {
        let (field, literal) = fact.split_once(':').expect("a `field:\"lit\"` fact");
        assert!(
            ["===", "!=="].iter().any(|op| openclaw::PLUGIN_TEMPLATE
                .contains(&format!("ev.{field} {op} {literal}"))),
            "the plugin no longer compares `{field}` against {literal}"
        );
    }
}

/// Every CLI is watched where its dependence is declared: an `Installer`
/// row's target carries watches, and a target with none leans on its source's
/// own. An empty `Watched` list fails core's `no_source_is_watched_by_an_empty_list`.
#[test]
fn every_source_and_target_is_watched_or_closed() {
    use pixtuoid_core::source::upstream::Upstream;
    for d in REGISTRY {
        if matches!(d.upstream, Upstream::Installer) {
            assert!(
                TARGETS
                    .iter()
                    .any(|t| t.core_source == d.name && !t.upstream.is_empty()),
                "{} is `Installer`, but no install target of it declares a watch",
                d.name
            );
        }
    }
    for t in TARGETS {
        let source = REGISTRY
            .iter()
            .find(|d| d.name == t.core_source)
            .expect("every target names a registered source");
        assert!(
            !t.upstream.is_empty() || matches!(source.upstream, Upstream::Watched(_)),
            "{} declares no watch, and its source declares none of its own",
            t.name
        );
    }
}

/// The installers' watches answer to the rules the registry's do, in one id
/// space with them.
#[test]
fn every_declared_watch_is_runnable_and_its_id_unique() {
    let mut ids = std::collections::BTreeSet::new();
    for (_, w) in declared() {
        assert!(ids.insert(w.id), "watch id {} is declared twice", w.id);
        assert_eq!(w.unrunnable(), None, "{}", w.id);
    }
}

/// Every `.rs` under `dir`, recursively: a module directory would otherwise
/// hide its files from the census that exists to see them.
fn rs_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("the dir is readable") {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            out.extend(rs_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

/// Every hook a target's artifact subscribes to is carried by a vanish watch,
/// the target's or its source's: a hook upstream renamed would leave the
/// registration inert with nothing to say so. A field watch never carries a
/// hook name, so a field named like one cannot stand in for it.
#[test]
fn every_registered_hook_is_watched() {
    for t in TARGETS {
        let hooks = (t.registers)();
        assert!(!hooks.is_empty(), "{} registers no hook", t.name);
        let source = REGISTRY
            .iter()
            .find(|d| d.name == t.core_source)
            .unwrap_or_else(|| panic!("{} installs for no registered source", t.name));
        let carried: BTreeSet<&str> = t
            .upstream
            .iter()
            .chain(source.upstream.watches())
            .filter(|w| matches!(w.when, When::Vanishes))
            .filter(|w| match w.read.yields() {
                Yield::Fields => false,
                Yield::Declaration | Yield::Family | Yield::Ours | Yield::One { .. } => true,
            })
            .flat_map(|w| (w.ours)())
            .collect();
        for hook in hooks {
            assert!(
                carried.contains(hook),
                "{} registers `{hook}`, but no vanish watch it or {} declares carries it",
                t.name,
                t.core_source,
            );
        }
    }
}

/// No anchor matches a name it guards: such an anchor vanishes WITH the names
/// on the very rename it exists to tell apart from a stale pin. A `Matches`
/// pattern captures the name it reads. A pattern of ours that does not compile
/// is a blind line, which `every_watch_fires_on_its_change_and_stays_silent_otherwise`
/// fails on.
#[test]
fn no_anchor_is_a_guarded_name_and_every_matches_pattern_captures() {
    let mut watches = 0;
    for (_, w) in declared() {
        watches += 1;
        let ours = (w.ours)();
        for doc in w.docs {
            let anchor = Regex::new(doc.anchor).unwrap_or_else(|e| panic!("{}: {e}", w.id));
            for name in &ours {
                assert!(
                    !anchor.is_match(name),
                    "{}: the anchor matches `{name}`",
                    w.id
                );
            }
        }
        if let Read::Matches { pattern } = w.read {
            let re = Regex::new(pattern).unwrap_or_else(|e| panic!("{}: {e}", w.id));
            assert!(
                re.captures_len() > 1,
                "{}: `{pattern}` captures no name",
                w.id
            );
        }
    }
    assert!(watches > 0, "the registry declares watches");
}

/// One repository, whichever crate reads it, is read at one ref.
#[test]
fn one_repository_has_one_release_policy() {
    let mut seen: HashMap<String, &Repo> = HashMap::new();
    for (_, w) in declared() {
        for doc in w.docs {
            let (At::Path { repo, .. } | At::ReleaseAsset { repo, .. }) = doc.at else {
                continue;
            };
            let first = *seen.entry(repo.name.to_ascii_lowercase()).or_insert(repo);
            assert_eq!(
                first, repo,
                "`{}` is declared twice, differently",
                repo.name
            );
        }
    }
    assert!(!seen.is_empty(), "the roster reads repositories");
}

#[test]
fn a_fetched_name_cannot_leave_its_code_span() {
    assert_eq!(
        code_span("tool_call`](https://x)\nmore\u{202e}"),
        "tool_call](https://x)more"
    );
}

/// The heading is the instruction: a line filed under the wrong one sends a
/// reader to edit a decoder that works (#793).
#[test]
fn each_line_renders_under_its_own_heading() {
    let mut r = Report::default();
    r.add_breaking("VERIFIED-LINE".to_owned());
    r.add_blind("PROBE-LINE", "AT", "C", false);
    let out = r.render();
    let at = |s: &str| out.find(s).unwrap_or(usize::MAX);
    let order = [
        at("## ⛔ Verified upstream change"),
        at("VERIFIED-LINE"),
        at("## 🩺 Probe could NOT verify"),
        at("PROBE-LINE"),
    ];
    assert!(order.is_sorted() && order[3] < usize::MAX, "{out}");
    assert!(out.contains("do NOT change a decoder"), "{out}");

    let mut blind = Report::default();
    blind.add_blind("PROBE-ONLY", "AT", "C", false);
    let out = blind.render();
    assert!(out.contains("Probe could NOT verify") && !out.contains("Verified upstream change"));
}

/// upstream-drift.yml titles the issue with the report's H1 rather than its
/// own copy of these strings: a reworded copy would title a confirmed rename
/// "could not verify".
#[test]
fn the_workflow_titles_the_issue_from_the_report_h1() {
    let wf = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows/upstream-drift.yml"
    ))
    .expect("the workflow is readable");
    assert!(wf.contains("head -1 drift-report.md") && wf.contains("s/^# //"));
    let mut r = Report::default();
    let mut titles = vec![r.title()];
    r.add_blind("W", "AT", "C", false);
    titles.push(r.title());
    r.add_review("R".to_owned());
    titles.push(r.title());
    r.add_breaking("B".to_owned());
    titles.push(r.title());
    for t in titles {
        assert!(!wf.contains(t), "the workflow keeps a copy of {t:?}");
    }
}

/// A family reader yields only the family, so everything it yields beyond ours
/// is a sibling.
#[test]
fn every_name_a_family_reader_yields_beyond_ours_is_a_sibling() {
    const W: Watch = Watch {
        id: "t.all",
        ours: || vec!["start"],
        docs: &[doc("a.rs")],
        read: Read::RustArms {
            name: "parse",
            arm: r"Ok\(Go\)",
        },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: Some(Siblings {
            family: Family::All,
            ignored: &[],
        }),
    };
    let src =
        "enum Events {}\nfn parse() { match s { \"start\" | \"begin\" => Ok(Go), _ => Err(()) } }";
    let r = check(&served().with(&raw("a.rs"), src), &W);
    assert!(r.breaking.is_empty() && r.blind.is_empty(), "{r:?}");
    assert!(
        r.review.len() == 1 && r.review[0].contains("`begin`"),
        "{r:?}"
    );
}

/// An appearance is the finding when a premise rests on a name's absence, and
/// a review finding files under review, never as breaking.
#[test]
fn an_appearing_name_files_its_finding_where_its_severity_says() {
    const PAGE: &str = "https://docs.example/hooks.md";
    const W: Watch = Watch {
        id: "t.appears",
        ours: || vec!["session_end", "procStart"],
        docs: &[Doc {
            at: At::Url(PAGE),
            anchor: r"(?m)^# Hooks reference",
            owns: "the hooks page",
        }],
        read: Read::Each { pattern: "{name}" },
        when: When::Appears,
        finding: Finding::Review("adopt it"),
        siblings: None,
    };
    let absent = served().with(PAGE, "# Hooks reference\nSessionEnd fires last.");
    let r = check(&absent, &W);
    assert!(
        r.breaking.is_empty() && r.review.is_empty() && r.blind.is_empty(),
        "{r:?}"
    );
    let landed = served().with(PAGE, "# Hooks reference\nA `session_end` record.");
    let r = check(&landed, &W);
    assert!(
        r.breaking.is_empty(),
        "a review finding is never breaking: {r:?}"
    );
    assert_eq!(r.review.len(), 1, "{r:?}");
    assert!(
        r.review[0].contains("docs.example/hooks.md now declares `session_end`")
            && r.review[0].contains("adopt it"),
        "the page is named without its scheme: {r:?}"
    );
}

/// A word family is read off ours: `custom_tool_call` is a sibling of
/// `function_call` by its last word, `subagent_progress` of `subagent_spawned`
/// by its first, and a name of ours with no `_` founds no family.
#[test]
fn a_word_family_is_the_word_ours_share() {
    const DOC: Doc = Doc {
        at: At::Path {
            repo: &OR,
            path: "e.rs",
        },
        anchor: r"pub enum Item\b",
        owns: "`Item`",
    };
    const LAST: Watch = Watch {
        id: "t.last",
        ours: || vec!["function_call", "message"],
        docs: &[DOC],
        read: Read::RustEnum { name: "Item" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: Some(Siblings {
            family: Family::LastWord,
            ignored: &[],
        }),
    };
    const FIRST: Watch = Watch {
        id: "t.first",
        ours: || vec!["subagent_spawned", "message"],
        siblings: Some(Siblings {
            family: Family::FirstWord,
            ignored: &[],
        }),
        ..LAST
    };
    let item = "#[serde(rename_all = \"snake_case\")]\npub enum Item {\n    FunctionCall,\n    CustomToolCall,\n    SubagentSpawned,\n    SubagentProgress,\n    Message,\n    UserMessage,\n    MessageDelta,\n}\n";
    for (w, sibling) in [(&LAST, "custom_tool_call"), (&FIRST, "subagent_progress")] {
        let r = check(&served().with(&raw("e.rs"), item), w);
        assert!(r.breaking.is_empty() && r.blind.is_empty(), "{r:?}");
        assert!(
            r.review.len() == 1 && r.review[0].contains(&format!("`{sibling}`")),
            "{}: {r:?}",
            w.id
        );
    }
}

/// An enum serialized under another spelling is unread, never read wrong: its
/// snake_case names would all report as gone.
#[test]
fn an_enum_off_snake_case_is_probe_health() {
    const W: Watch = Watch {
        id: "t.enum",
        ours: || vec!["event_msg"],
        docs: &[doc("e.rs")],
        read: Read::RustEnum { name: "Item" },
        when: When::Vanishes,
        finding: Finding::Breaking("x"),
        siblings: None,
    };
    let camel =
        "enum Events {}\n#[serde(rename_all = \"camelCase\")]\npub enum Item {\n    EventMsg,\n}\n";
    let r = check(&served().with(&raw("e.rs"), camel), &W);
    assert!(r.breaking.is_empty() && r.blind.len() == 1, "{r:?}");
    assert!(
        r.blind[0].contains("found `enum Item` without `rename_all = \"snake_case\"`"),
        "{r:?}"
    );
}

/// Every wire literal a decoder or a native exit gate equality-compares is carried by a declared
/// vanish watch — a source's or an installer's, so it lives here, where both
/// rosters are — or exempted below with its adjudicated reason (#943's gate).
///
/// Accepted loosenesses, adjudicated on #943: membership is
/// global across sets, so a cross-source spelling collision passes; a
/// literal behind a non-exported const escapes the scan; and the scan is
/// `==`/`!=`-shaped only — a MATCH ARM pattern, a `.starts_with` prefix
/// gate, or a `matches!` is outside its population. Export such
/// vocabularies instead.
/// Stale exemptions fail the OTHER direction.
#[test]
fn every_equality_compared_wire_literal_is_accounted_for() {
    // (path under `src/source`, literal, why no upstream watch is owed)
    const EXEMPT: &[(&str, &str, &str)] = &[
        (
            "antigravity.rs",
            "PLANNER_RESPONSE",
            "capture-verified step type; a rename lands in the \
             non-PLANNER_RESPONSE + tool_calls breadcrumb (antigravity.rs)",
        ),
        (
            "antigravity.rs",
            "ask_permission",
            "unverified reverse-engineered tool name (antigravity.rs comment); \
             no watchable upstream, silent on rename — accepted",
        ),
        ("antigravity.rs", "ask_question", "same as ask_permission"),
        (
            "admit.rs",
            "jsonl",
            "file-extension guard; resolver/path axis (#880)",
        ),
        (
            "cc_probe.rs",
            "json",
            "file-extension guard; resolver/path axis (#880)",
        ),
        (
            "cc_probe.rs",
            "projects",
            "dirname guard; resolver/path axis (#880)",
        ),
        (
            "claude_code.rs",
            "<synthetic>",
            "CC's placeholder model value; a rename degrades to a cosmetic \
             flame label until the next real model string arrives",
        ),
        (
            "claude_code.rs",
            "tool_use",
            "Messages-API block type; no CC doc declares it apart from \
             `tool_use_id`, and the hook transport carries the same activity \
             — headless-only degradation, corpus-census detectable",
        ),
        ("claude_code.rs", "tool_result", "same as tool_use"),
        (
            "antigravity.rs",
            "USER_INPUT",
            "no watchable upstream doc; the decoder breadcrumbs unknown \
             vocabulary (the runtime detector)",
        ),
        (
            "antigravity.rs",
            "CONVERSATION_HISTORY",
            "same as USER_INPUT",
        ),
        (
            "claude_code.rs",
            "attachment",
            "undocumented transcript surface; its marker KINDS are \
             appearance-watched by `claude_code.effort_attachments`",
        ),
        (
            "copilot.rs",
            "task",
            "name-only BY DESIGN (subagent_type is spoofable, see \
             copilot_tool_detail) — and unwatchable: the schema types \
             toolName as a free string and never declares tool-name values",
        ),
        (
            "cursor.rs",
            "Task",
            "detection is semantic (subagent_type, capture-verified), so a \
             rename degrades to a Generic tool, never a missed delegation",
        ),
        (
            "grok.rs",
            "spawn_subagent",
            "semantic subagent_type fallback exists on the same arm",
        ),
        (
            "omp.rs",
            "default",
            "resolver/path axis, deliberately unwatched (#880)",
        ),
        (
            "omp.rs",
            "sessions",
            "resolver/path axis, deliberately unwatched (#880)",
        ),
        (
            "omp.rs",
            "task",
            "children register via PATH NESTING regardless (module doc); the \
             name only styles the parent's pose",
        ),
        (
            "codex/native.rs",
            "jsonl",
            "resolver/path axis, deliberately unwatched (#880): a session-file path \
             component, not a wire name",
        ),
        (
            "codex/native.rs",
            "sessions",
            "same as codex/native.rs jsonl",
        ),
        (
            "grok/native.rs",
            "sessions",
            "same as codex/native.rs jsonl",
        ),
        ("omp/native.rs", "jsonl", "same as codex/native.rs jsonl"),
        ("omp/native.rs", "sessions", "same as codex/native.rs jsonl"),
    ];

    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut carried: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // An appearance watch fires only when a name lands, so it cannot see one
    // renamed: only the vanish watches' names are carried.
    for (_, w) in declared().filter(|(_, w)| matches!(w.when, When::Vanishes)) {
        carried.extend((w.ours)().into_iter().map(str::to_owned));
    }
    let sources: std::collections::BTreeSet<&str> =
        pixtuoid_core::source::registry::registered_source_names().collect();

    let root = crates.join("pixtuoid-core/src/source");
    let paths = rs_files(&root);

    let mut found: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut unaccounted: Vec<String> = Vec::new();
    for path in paths {
        let file = path
            .strip_prefix(&root)
            .expect("under the census root")
            .to_str()
            .expect("a utf-8 path")
            .replace('\\', "/");
        // mod/tests are the runtime axis, deliberately unwatched (#880);
        // drift/registry are the breadcrumb infra itself. A native resolver is
        // read: its exit gate compares the decoder's wire names.
        if ["mod.rs", "tests.rs", "drift.rs", "registry.rs"]
            .iter()
            .any(|f| path.file_name().is_some_and(|n| n == *f))
        {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("the module is readable");
        let prod = src
            .split("#[cfg(test)]\nmod ")
            .next()
            .expect("split never yields zero parts");
        for line in prod.lines() {
            // Comments first: a doc example quoting `== "x"` is not a read.
            let code = line.split("//").next().unwrap_or("");
            for pat in ["== Some(\"", "== \"", "!= Some(\"", "!= \""] {
                for (i, _) in code.match_indices(pat) {
                    let rest = &code[i + pat.len()..];
                    let Some(end) = rest.find('\"') else { continue };
                    let lit = &rest[..end];
                    found.insert((file.clone(), lit.to_owned()));
                    let ok = carried.contains(lit)
                        || sources.contains(lit)
                        || lit.starts_with('.')
                        || lit.ends_with(".jsonl")
                        || EXEMPT.iter().any(|(f, l, _)| *f == file && *l == lit);
                    if !ok {
                        unaccounted.push(format!("{file}: == \"{lit}\""));
                    }
                }
            }
        }
    }
    assert!(
        unaccounted.is_empty(),
        "a decoder equality-compares wire literals the drift surface never \
         carries — watch them (a `Watch` in the source's or installer's \
         `UPSTREAM`), breadcrumb the miss, or exempt them here with the \
         adjudicated reason:\n  {}",
        unaccounted.join("\n  "),
    );
    for (file, lit, _) in EXEMPT {
        assert!(
            found.contains(&((*file).to_owned(), (*lit).to_owned())),
            "the exemption for {file} == \"{lit}\" outlived the \
             comparison it excused — delete the row",
        );
    }
}
