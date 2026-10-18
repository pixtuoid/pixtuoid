//! Every character the office can write is one the window's faces draw: the
//! screen face all of it, the world face what the office's own signs and
//! names write. Read from the literals of everything that renders text,
//! parsed so a comment, a doc, a pattern, a test or the dev-only `harness`
//! never counts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pixtuoid_scene::cutaway::Face;
use syn::visit::Visit;

/// The source trees whose literals reach the screen, core's decoded labels
/// and details among them.
const RENDERERS: [&str; 4] = [
    "../pixtuoid-core/src",
    "../pixtuoid-scene/src",
    "src/tui",
    "src/floating",
];

/// What world text (the badges, the board, the bubbles, the floor sign) is
/// written by: core's labels and the scene's signs.
const WORLD_WRITERS: [&str; 6] = [
    "../pixtuoid-core/src",
    "../pixtuoid-scene/src/neon_sign.rs",
    "../pixtuoid-scene/src/chitchat.rs",
    "../pixtuoid-scene/src/badge.rs",
    "../pixtuoid-scene/src/display/text.rs",
    "../pixtuoid-scene/src/layout",
];

/// The icons' terminal glyphs: the screen face draws an icon's art by its
/// glyph, and the world names its icons, so nothing writes this file's
/// glyphs as text the gate could check.
const TERMINAL_ONLY: &str = "../pixtuoid-scene/src/display/icon.rs";

/// What world text writes that no badge reaches: core's cap mark, which ends
/// only a label longer than a badge keeps (`badge.rs` asserts it).
const PAST_A_BADGE: [char; 1] = [pixtuoid_core::source::decoder::ELLIPSIS];

/// The characters of a file's string and char literals outside tests, the
/// `harness`, attributes and patterns, and the off-screen modules it declares
/// out of line.
#[derive(Default)]
struct Written {
    chars: Vec<char>,
    off_screen_mods: Vec<String>,
}

impl Written {
    fn of(source: &str) -> Self {
        let file = syn::parse_file(source).expect("source parses");
        let mut found = Self::default();
        found.visit_file(&file);
        found
    }

    /// A macro's arguments are tokens, not syntax: each literal among them
    /// counts.
    fn scan_tokens(&mut self, tokens: proc_macro2::TokenStream) {
        use proc_macro2::TokenTree;
        for tree in tokens {
            match tree {
                TokenTree::Group(g) => self.scan_tokens(g.stream()),
                TokenTree::Literal(lit) => {
                    let text = lit.to_string();
                    if let Ok(s) = syn::parse_str::<syn::LitStr>(&text) {
                        self.chars.extend(s.value().chars());
                    } else if let Ok(c) = syn::parse_str::<syn::LitChar>(&text) {
                        self.chars.push(c.value());
                    }
                }
                _ => {}
            }
        }
    }
}

/// Whether an item compiles only for tests or for core's dev-only `harness`,
/// which no painter links: `#[test]`, or a `cfg` that holds only when one of
/// them does (`test`, `feature = "harness"`, or `all(…)` naming one).
fn off_screen(attrs: &[syn::Attribute]) -> bool {
    fn only_under(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(p) => p.is_ident("test"),
            syn::Meta::NameValue(nv) if nv.path.is_ident("feature") => matches!(
                &nv.value,
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) if s.value() == "harness"
            ),
            syn::Meta::List(l) if l.path.is_ident("all") => l
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|all| all.iter().any(only_under)),
            _ => false,
        }
    }
    attrs.iter().any(|a| {
        a.path().is_ident("test")
            || (a.path().is_ident("cfg")
                && a.parse_args::<syn::Meta>().is_ok_and(|m| only_under(&m)))
    })
}

impl<'ast> Visit<'ast> for Written {
    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !off_screen(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        } else if m.content.is_none() {
            self.off_screen_mods.push(m.ident.to_string());
        }
    }

    fn visit_pat(&mut self, _: &'ast syn::Pat) {}

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if !off_screen(&f.attrs) {
            syn::visit::visit_item_fn(self, f);
        }
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if !off_screen(&i.attrs) {
            syn::visit::visit_item_impl(self, i);
        }
    }

    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
        self.chars.extend(s.value().chars());
    }

    fn visit_lit_char(&mut self, c: &'ast syn::LitChar) {
        self.chars.push(c.value());
    }

    /// A `matches!`'s pattern is no text: only its scrutinee counts.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.is_ident("matches") {
            let scrutinee = mac.tokens.clone().into_iter().take_while(
                |t| !matches!(t, proc_macro2::TokenTree::Punct(p) if p.as_char() == ','),
            );
            self.scan_tokens(scrutinee.collect());
        } else {
            self.scan_tokens(mac.tokens.clone());
        }
    }
}

/// Where `file`'s out-of-line `mod name;` lives, as rustc resolves it: beside
/// a `mod.rs` or `lib.rs`, else in the directory named for the file.
fn module_dir(file: &Path) -> PathBuf {
    let dir = file.parent().expect("a file has a directory");
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("mod" | "lib" | "main") => dir.to_path_buf(),
        Some(stem) => dir.join(stem),
        None => dir.to_path_buf(),
    }
}

/// Whether a written `c` reaches a glyph: one that takes no cell (a control,
/// a bidi mark) draws nothing, and a private-use code point means nothing
/// outside a private agreement (Unicode §23.5), here a painter's in-band
/// marker.
fn drawn(c: char) -> bool {
    let private_use = matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{10ffff}');
    pixtuoid_scene::display::text::cells(c.encode_utf8(&mut [0; 4])) > 0 && !private_use
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("src reads") {
        let path = entry.expect("entry reads").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Each source file under [`RENDERERS`] outside its crate's tests and
/// `harness`, with the characters its literals write.
fn written() -> Vec<(PathBuf, Vec<char>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for tree in RENDERERS {
        rust_files(&root.join(tree), &mut files);
    }
    files.retain(|file| *file != root.join(TERMINAL_ONLY));
    let parsed: Vec<(PathBuf, Written)> = files
        .into_iter()
        .map(|file| {
            let source = std::fs::read_to_string(&file).expect("source reads");
            let found = Written::of(&source);
            (file, found)
        })
        .collect();
    let skipped_dirs: Vec<PathBuf> = parsed
        .iter()
        .flat_map(|(file, found)| {
            let dir = module_dir(file);
            found.off_screen_mods.iter().map(move |name| dir.join(name))
        })
        .collect();
    parsed
        .into_iter()
        .filter(|(file, _)| {
            !skipped_dirs
                .iter()
                .any(|t| file.with_extension("") == *t || file.starts_with(t))
        })
        .map(|(file, found)| (file, found.chars))
        .collect()
}

/// The characters of `files` that `face` draws as tofu, with where each is
/// written.
fn tofu<'a>(
    files: impl IntoIterator<Item = &'a (PathBuf, Vec<char>)>,
    face: Face,
) -> BTreeMap<String, Vec<String>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pack = pixtuoid_scene::pack::load_bundled_pack().expect("the bundled pack loads");
    let mut tofu = BTreeMap::<String, Vec<String>>::new();
    for (file, chars) in files {
        for &c in chars {
            if drawn(c) && !face.draws(c, &pack) {
                let at = file
                    .strip_prefix(root)
                    .unwrap_or(file)
                    .display()
                    .to_string();
                let seen = tofu
                    .entry(format!("{c} U+{:04X}", u32::from(c)))
                    .or_default();
                if !seen.contains(&at) {
                    seen.push(at);
                }
            }
        }
    }
    tofu
}

#[test]
fn the_screen_face_draws_every_character_the_office_writes() {
    let missing = tofu(&written(), Face::Screen);
    assert!(
        missing.is_empty(),
        "the screen face draws these as tofu: {missing:#?}"
    );
}

#[test]
fn the_world_face_draws_every_character_the_signs_and_names_write() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let writers: Vec<PathBuf> = WORLD_WRITERS.iter().map(|w| root.join(w)).collect();
    let world: Vec<(PathBuf, Vec<char>)> = written()
        .into_iter()
        .filter(|(file, _)| writers.iter().any(|w| file.starts_with(w)))
        .map(|(file, chars)| {
            let reached = chars.into_iter().filter(|c| !PAST_A_BADGE.contains(c));
            (file, reached.collect())
        })
        .collect();
    let missing = tofu(&world, Face::World);
    assert!(
        missing.is_empty(),
        "the world face draws these as tofu: {missing:#?}"
    );
}
