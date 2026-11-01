use super::*;
use crate::sprite::Frame;
use crate::sprite::error::{LineError, SpriteError};

/// The `mini_pack` fixture: one one-frame piece, the rest filled in.
fn mini_pack() -> Pack {
    load_filled_pack(
        include_str!("fixtures/mini_pack/pack.toml"),
        &[(
            "seated.sprite",
            include_str!("fixtures/mini_pack/seated.sprite"),
        )],
    )
    .expect("mini pack loads")
}

/// `src` as the one sprite file of a pack whose palette is `A`, `B` and `.`.
fn parse(src: &str) -> anyhow::Result<Vec<Frame>> {
    let pack = load_filled_pack(
        "[palette]\n\"A\"=\"#010203\"\n\"B\"=\"#040506\"\n\".\"=\"transparent\"\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n",
        &[("f.sprite", src)],
    )?;
    Ok(pack.piece(Piece::Seated).frames().to_vec())
}

/// The failure names its line, and what a caller sees of it (`{:#}`) names each
/// step once.
#[test]
fn a_bad_pixel_names_its_line_and_its_chain_prints_each_step_once() {
    let err = load_filled_pack(
        "[palette]\n\"A\"=\"#010203\"\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n",
        &[("f.sprite", "@frame 0\nA z")],
    )
    .unwrap_err();
    assert!(
        matches!(
            &err,
            Repr::Decode {
                file,
                source: SpriteError::Line {
                    line: 2,
                    kind: LineError::UnknownKey { key: 'z', .. },
                    ..
                },
                ..
            } if file == "f.sprite"
        ),
        "{err:?}"
    );
    assert_eq!(
        format!("{:#}", anyhow::Error::from(PackError::from(err))),
        "decoding f.sprite: unknown palette key 'z' (line 2)"
    );
}

#[test]
fn a_shape_error_names_the_line_at_fault() {
    let line_of = |sprite: &str| match load_filled_pack(
        "[palette]\n\"A\"=\"#010203\"\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n",
        &[("f.sprite", sprite)],
    )
    .unwrap_err()
    {
        Repr::Decode {
            source: SpriteError::Line { line, .. },
            ..
        } => line,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        line_of("@frame 0\nA A\nA\nA A\n@frame 1\nA A"),
        3,
        "the ragged row"
    );
    assert_eq!(
        line_of("@frame 0\n@mark hat 5 0\nA A\nA A\n"),
        2,
        "the mark outside"
    );
    assert_eq!(
        line_of("@frame 0\nA\n@frame 1\n@frame 2\nA"),
        3,
        "the empty frame"
    );
}

#[test]
fn parses_two_frame_mini_sprite() {
    let frames = parse(include_str!("fixtures/mini.sprite")).unwrap();

    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].width(), 4);
    assert_eq!(frames[0].height(), 2);
    assert_eq!(frames[0].as_slice()[0], Some(Rgb { r: 1, g: 2, b: 3 }));
    assert_eq!(frames[0].as_slice()[1], None);
    assert_eq!(frames[0].as_slice()[2], Some(Rgb { r: 4, g: 5, b: 6 }));
    assert_eq!(frames[0].as_slice()[3], None);
}

#[test]
fn rejects_inconsistent_row_widths() {
    let src = "@frame 0\nA . B .\nA . B";
    let err = parse(src).unwrap_err();
    assert!(format!("{err:#}").contains("row width"), "got: {err:#}");
}

#[test]
fn rejects_empty_source_with_no_frames() {
    let err = parse("").unwrap_err();
    assert!(
        format!("{err:#}").contains("contains no frames"),
        "empty source must bail with 'contains no frames'; got: {err:#}"
    );
}

#[test]
fn rejects_multi_char_pixel_token() {
    let err = parse("@frame 0\nAB . .").unwrap_err();
    assert!(
        format!("{err:#}").contains("single character"),
        "a multi-char pixel token must bail; got: {err:#}"
    );
}

#[test]
fn rejects_frame_block_with_no_rows() {
    // Back-to-back @frame headers: the first block has zero rows.
    let err = parse("@frame 0\n@frame 1\nA").unwrap_err();
    assert!(
        format!("{err:#}").contains("no rows"),
        "an empty frame block must bail with 'frame has no rows'; got: {err:#}"
    );
}

#[test]
fn rejects_palette_key_longer_than_one_char() {
    let pack_toml = "[palette]\n\"AB\"=\"#010203\"\n\
         [animations.seated]\nframes=[\"i.sprite\"]\nframe_ms=100\n";
    let err = anyhow::Error::from(
        load_filled_pack(pack_toml, &[("i.sprite", "@frame 0\nA")]).unwrap_err(),
    );
    assert!(
        format!("{err:#}").contains("exactly one character"),
        "a >1-char palette key must bail; got: {err:#}"
    );
}

#[test]
fn rejects_palette_value_not_six_hex_digits() {
    let pack_toml = "[palette]\n\"A\"=\"#12345\"\n\
         [animations.seated]\nframes=[\"i.sprite\"]\nframe_ms=100\n";
    let err = anyhow::Error::from(
        load_filled_pack(pack_toml, &[("i.sprite", "@frame 0\nA")]).unwrap_err(),
    );
    assert!(
        format!("{err:#}").contains("6 hex digits"),
        "a non-6-hex-digit color must bail; got: {err:#}"
    );
}

#[test]
fn a_single_frame_typing_does_not_load() {
    let err = load_filled_pack(
        "[palette]\n\"A\"=\"#010203\"\n\
         [animations.typing]\nframes=[\"t.sprite\"]\nframe_ms=100\n",
        &[("t.sprite", "@frame 0\nA")],
    )
    .unwrap_err();
    assert!(
        matches!(&err, Repr::TooFewFrames { key, need: 2, have: 1, .. } if key == "typing"),
        "{err:?}"
    );
}

#[test]
fn loads_mini_pack() {
    let pack = mini_pack();
    let seated = pack.piece(Piece::Seated);
    assert_eq!(seated.frame_ms(), 500);
    assert_eq!(seated.frames().len(), 1);
    assert_eq!(seated.first().width(), 4);
}

#[test]
fn an_unregistered_animation_does_not_load() {
    let err = load_filled_pack(
        "[palette]\n\"A\"=\"#010203\"\n\
         [animations.idle]\nframes=[\"i.sprite\"]\nframe_ms=100\n",
        &[("i.sprite", "@frame 0\nA")],
    )
    .unwrap_err();
    assert!(
        matches!(&err, Repr::UnknownAnimation { key, .. } if key == "idle"),
        "{err:?}"
    );
}

#[test]
fn frame_wider_than_u16_max_errors_instead_of_truncating() {
    // The width half of `push_row`'s u16 guard.
    let mut src = String::with_capacity(2 * (u16::MAX as usize + 2) + 16);
    src.push_str("@frame 0\n");
    for _ in 0..=u16::MAX as usize {
        src.push_str("A ");
    }
    src.push('\n');
    let err = parse(&src).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("width") && msg.contains("line"),
        "oversized width must error with line context, got: {msg}"
    );
}

#[test]
fn frame_taller_than_u16_max_errors_instead_of_truncating() {
    // Its row-count half.
    let mut src = String::with_capacity(2 * (u16::MAX as usize + 2) + 16);
    src.push_str("@frame 0\n");
    for _ in 0..=u16::MAX as usize {
        src.push_str("A\n");
    }
    let err = parse(&src).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("rows") && msg.contains("line"),
        "oversized height must error with line context, got: {msg}"
    );
}

fn d(n: u16) -> Density {
    Density::new(n).expect("nonzero")
}

#[test]
fn palette_value_rejects_sign_prefixed_or_non_hex() {
    assert!(parse_palette_value("#+f0102").is_err());
    assert!(parse_palette_value("#-f0102").is_err());
    assert!(parse_palette_value("#abXY12").is_err());
    assert!(parse_palette_value("#Ff0102").unwrap().is_some());
    assert!(parse_palette_value("transparent").unwrap().is_none());
}

/// A pack of `extra` tables over a palette of the seven material keys and
/// a stray `x`; `files` holds the buildings' sprites.
fn city_pack(extra: &str, files: &[(&str, &str)]) -> Result<Pack> {
    load_filled_pack(
        &format!(
            "[palette]\n\".\"=\"transparent\"\n\
             \"F\"=\"#202020\"\n\"f\"=\"#181818\"\n\"R\"=\"#303030\"\n\"W\"=\"#404040\"\n\
             \"M\"=\"#101010\"\n\"D\"=\"#282828\"\n\"L\"=\"#d08050\"\n\"x\"=\"#ffffff\"\n\
             {extra}"
        ),
        files,
    )
}

const CITY: &str = "[city]\nfacade=\"F\"\nshade=\"f\"\nroof=\"R\"\nglass=\"W\"\n\
                    mullion=\"M\"\ndetail=\"D\"\nsign=\"L\"\n";
/// A 2x3 tower: one window of two glass pixels over a wall.
const TOWER: &str = "@frame 0\nR R\nW W\nF f\n";
/// The tower at 2x, drawn with a mullion splitting its window in two.
const TOWER_2X: &str = "@frame 0\nR R R R\nR R R R\nW M W W\nW M W W\nF F f f\nF F f f\n";

#[test]
fn a_building_is_its_base_and_its_density_variants() {
    let pack = city_pack(
        &format!(
            "{CITY}[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[\"mid\", \"near\"]\n\
             [buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\n"
        ),
        &[("t.sprite", TOWER), ("t2.sprite", TOWER_2X)],
    )
    .expect("the city pack loads");
    let tower = pack.buildings().next().expect("the tower");
    assert_eq!((tower.name(), tower.size()), ("tower", (2, 3)));
    assert!(tower.stands_in(CityPlane::Mid) && tower.stands_in(CityPlane::Near));
    let d = |n| Density::new(n).expect("nonzero");
    let base = tower.base();
    assert!(tower.variant(d(1)).is_none(), "the base is no variant");
    assert_eq!(
        base.windows(),
        [vec![(0, 1), (1, 1)]],
        "one run of glass, one window"
    );
    let dense = tower.variant(d(2)).expect("the 2x variant");
    assert_eq!(
        dense.windows().len(),
        2,
        "each density finds its own windows: the mullion splits this one"
    );
    assert!(tower.variant(d(4)).is_none());
    assert_eq!(pack.city_materials().key(Material::Glass), 'W');
}

#[test]
fn a_building_outside_its_rules_is_rejected() {
    let base = "[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[\"near\"]\n";
    let load = |extra: &str, art: &str, dense: &str| {
        city_pack(extra, &[("t.sprite", art), ("t2.sprite", dense)])
    };
    let ok = format!("{CITY}{base}[buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\n");
    assert!(load(&ok, TOWER, TOWER_2X).is_ok());
    let rejected = [
        (
            format!("{CITY}{base}"),
            "@frame 0\nF x\n",
            "a key no material names",
        ),
        (
            format!("{CITY}[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[\"far\"]\n"),
            TOWER,
            "the far plane is the painter's own",
        ),
        (
            format!("{CITY}[buildings.tower]\nsprite=\"t.sprite\"\n"),
            TOWER,
            "a base naming no planes",
        ),
        (
            format!("{CITY}[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[]\n"),
            TOWER,
            "a base standing in no plane",
        ),
        (
            format!("{CITY}[buildings.\"tower@1x\"]\nsprite=\"t.sprite\"\nplanes=[\"near\"]\n"),
            TOWER,
            "a base named like a variant",
        ),
        (
            format!("{CITY}[buildings.\"tower@big\"]\nsprite=\"t.sprite\"\nplanes=[\"near\"]\n"),
            TOWER,
            "a base named with a variant's mark",
        ),
        (
            format!("{CITY}{base}"),
            "@frame 0\nR R\nW W\nF f\n@frame 1\nR R\nW W\nF f\n",
            "a building of two frames",
        ),
        (
            format!("{}{base}", CITY.replace("sign=\"L\"", "sign=\".\"")),
            TOWER,
            "a material drawn in a transparent key",
        ),
        (
            format!("{CITY}[buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\n"),
            TOWER,
            "a variant with no base",
        ),
        (
            format!(
                "{CITY}{base}[buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\nplanes=[\"mid\"]\n"
            ),
            TOWER,
            "a variant naming its own planes",
        ),
        (
            format!("{CITY}{base}[buildings.\"tower@2x\"]\nsprite=\"t.sprite\"\n"),
            TOWER,
            "a variant not twice its base",
        ),
        (
            format!("{}{base}", CITY.replace("sign=\"L\"", "sign=\"F\"")),
            TOWER,
            "two materials in one key",
        ),
        (
            format!("{}{base}", CITY.replace("sign=\"L\"\n", "")),
            TOWER,
            "a material with no key",
        ),
        (
            format!("{CITY}neon=\"L\"\n{base}"),
            TOWER,
            "a key naming no material",
        ),
    ];
    for (extra, art, why) in rejected {
        assert!(load(&extra, art, TOWER_2X).is_err(), "{why}");
    }
}

/// A pack whose `seated` draws `f.sprite`, plus `extra` tables; `frames`
/// holds every file by name. As a caller sees the failure: through
/// `anyhow`, whose `{:#}` walks the chain.
fn hair_pack(extra: &str, frames: &[(&str, &str)]) -> anyhow::Result<Pack> {
    let toml = format!(
        "[palette]\n\".\"=\"transparent\"\n\
         \"H\"=\"#28140a\"\n\"k\"=\"#101010\"\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n{extra}"
    );
    Ok(load_filled_pack(&toml, frames)?)
}

#[test]
fn a_mark_names_a_point_of_its_own_frame() {
    let pack = hair_pack(
        "",
        &[(
            "f.sprite",
            "@frame 0\n@mark head.front 1 0\n@mark cup 0 1\nH H\nH H\n@frame 1\nH H\nH H\n",
        )],
    )
    .expect("loads");
    let seated = pack.piece(Piece::Seated);
    let head = seated.head(0).expect("frame 0's head");
    assert_eq!((head.view, head.x, head.y), (HeadView::Front, 1, 0));
    let names: Vec<_> = seated.marks(0).iter().map(Mark::name).collect();
    assert_eq!(names, ["head.front", "cup"]);
    assert_eq!(seated.head(1), None, "an unmarked frame has no head");
    assert!(seated.marks(1).is_empty());
}

#[test]
fn a_mark_outside_its_frame_or_out_of_its_rules_is_rejected() {
    for bad in [
        "@mark cup 2 0",
        "@mark head.up 0 0",
        "@mark cup 0",
        "@mark Cup 0 0",
        "@mark cup 0 0\n@mark cup 1 0",
        "@mark head.front 0 0\n@mark head.back 1 0",
    ] {
        let err =
            hair_pack("", &[("f.sprite", &format!("@frame 0\n{bad}\nH H\n"))]).expect_err(bad);
        assert!(format!("{err:#}").contains("mark"), "{bad}: {err:#}");
    }
}

#[test]
fn a_hairstyle_loads_its_layers_per_view_at_its_density() {
    let pack = hair_pack(
        "[characters]\noutline=\"k\"\n\
         [hairstyles.\"mop@2x\"]\nfront={ behind=\"b.sprite\", over=\"o.sprite\" }\nback={ over=\"o2.sprite\" }\n",
        &[
            ("f.sprite", "@frame 0\nH\n"),
            ("b.sprite", "@frame 0\n@mark head.front 0 1\nH\nH\n"),
            ("o.sprite", "@frame 0\n@mark head.front 0 0\nH\n"),
            ("o2.sprite", "@frame 0\n@mark head.back 0 0\nH\n"),
        ],
    )
    .expect("loads");
    let styles: Vec<_> = pack.hairstyles().collect();
    assert_eq!(styles.len(), 1);
    let mop = styles[0];
    assert_eq!((mop.name(), mop.density().get()), ("mop", 2));
    assert!(pack.hairstyle("mop", mop.density()).is_some());
    let front = mop.layers(HeadView::Front).expect("a front view");
    assert_eq!(front.behind().and_then(|l| l.head(0)).map(|h| h.y), Some(1));
    assert!(front.over().is_some());
    assert!(
        mop.layers(HeadView::Back)
            .is_some_and(|l| l.behind().is_none())
    );
    assert!(mop.layers(HeadView::Side).is_none());
    assert_eq!(
        pack.character_outline(),
        Rgb {
            r: 16,
            g: 16,
            b: 16
        }
    );
}

#[test]
fn a_hairstyle_out_of_its_rules_is_rejected() {
    let f = ("f.sprite", "@frame 0\nH\n");
    let front = ("o.sprite", "@frame 0\n@mark head.front 0 0\nH\n");
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 5] = [
        (
            "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n",
            &[f, ("o.sprite", "@frame 0\n@mark head.back 0 0\nH\n")],
            "a back head on a front layer",
        ),
        (
            "[hairstyles.mop]\nfront={ over=\"o.sprite\" }\n",
            &[f, front],
            "a style without a density",
        ),
        (
            "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n\
             [hairstyles.\"bun@4x\"]\nfront={ over=\"o.sprite\" }\n",
            &[f, front],
            "styles differing between densities",
        ),
        (
            "[characters]\noutline=\".\"\n",
            &[f],
            "a transparent outline",
        ),
        (
            "[characters]\noutline=\"q\"\n",
            &[f],
            "an outline no palette key names",
        ),
    ];
    for (extra, files, why) in cases {
        assert!(hair_pack(extra, files).is_err(), "{why}");
    }
    assert!(
        hair_pack(
            "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n\
             [hairstyles.\"mop@4x\"]\nfront={ over=\"o.sprite\" }\n",
            &[f, front],
        )
        .is_ok(),
        "one style at two densities"
    );
}

fn ramp_pack(palette: &str, ramps: &str, sprite: &str) -> anyhow::Result<Pack> {
    let toml = format!(
        "[palette]\n{palette}\n\
         [ramps]\n{ramps}\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n"
    );
    Ok(load_filled_pack(&toml, &[("f.sprite", sprite)])?)
}

const HAIR: Rgb = Rgb {
    r: 40,
    g: 20,
    b: 10,
};

#[test]
fn ramp_keys_draw_as_a_step_of_their_base() {
    let pack = ramp_pack(
        "\"H\"=\"#28140a\"",
        "\"h\" = { of = \"H\", level = -1 }",
        "@frame 0\nH h",
    )
    .expect("pack builds");
    let frame = pack.piece(Piece::Seated).first();
    assert_eq!(frame.get(0, 0).copied().flatten(), Some(HAIR));
    assert_eq!(frame.get(1, 0).copied().flatten(), Some(HAIR.ramp(-1)));
}

/// `X` shares `B`'s color and stays; `h` is never named and follows `H`;
/// `.` is transparent and stays so.
#[test]
fn a_recolor_replaces_keys_not_colors() {
    let pack = ramp_pack(
        "\"B\"=\"#2e62cf\"\n\"X\"=\"#2e62cf\"\n\"H\"=\"#28140a\"\n\".\"=\"transparent\"",
        "\"h\" = { of = \"H\", level = -1 }",
        "@frame 0\nB X h .",
    )
    .expect("pack builds");
    let seated = pack.piece(Piece::Seated);
    let (red, blond) = (
        Rgb { r: 200, g: 0, b: 0 },
        Rgb {
            r: 200,
            g: 160,
            b: 80,
        },
    );
    let overrides = [
        ('B', Some(red)),
        ('H', Some(blond)),
        ('.', Some(red)),
        ('Q', None),
    ];
    let out = seated.recolorable_at(0).recolored(&overrides);
    let shirt = pack.palette().get('B').flatten();
    assert_eq!(
        out.as_slice(),
        &[Some(red), shirt, Some(blond.ramp(-1)), None][..]
    );
    assert_eq!(
        seated.recolorable_at(1).recolored(&overrides).as_slice(),
        out.as_slice(),
        "an index past the last frame wraps to the first"
    );
    assert_eq!(
        seated.frames()[0].as_slice(),
        &[shirt, shirt, Some(HAIR.ramp(-1)), None][..],
        "the pack's own colors are untouched"
    );
}

#[test]
fn a_ramp_may_step_as_far_as_the_bound_either_way() {
    for level in [MAX_RAMP_LEVEL, -MAX_RAMP_LEVEL] {
        let ramps = format!("\"h\" = {{ of = \"H\", level = {level} }}");
        ramp_pack("\"H\"=\"#28140a\"", &ramps, "@frame 0\nh").expect(&ramps);
    }
}

#[test]
fn a_ramp_rejects_a_malformed_declaration() {
    for (ramps, needle) in [
        ("\"h\" = { of = \"Q\", level = -1 }", "no opaque color"),
        ("\"h\" = { of = \"P\", level = -1 }", "no opaque color"),
        (
            "\"h\" = { of = \"H\", level = -1 }\n\"k\" = { of = \"h\", level = -1 }",
            "no opaque color",
        ),
        ("\"H2\" = { of = \"H\", level = -1 }", "one character"),
        ("\"X\" = { of = \"H\", level = -1 }", "both"),
        ("\"h\" = { of = \"H\", level = 0 }", "nonzero"),
        ("\"h\" = { of = \"H\", level = 11 }", "within"),
        ("\"h\" = { of = \"H\", level = -11 }", "within"),
        (
            "\"h\" = { of = \"H\", level = -1, typo = 1 }",
            "unknown field",
        ),
    ] {
        let err = ramp_pack(
            "\"H\"=\"#28140a\"\n\"X\"=\"#010203\"\n\"P\"=\"transparent\"",
            ramps,
            "@frame 0\nH",
        )
        .expect_err(ramps);
        assert!(format!("{err:#}").contains(needle), "{ramps}: {err:#}");
    }
}

/// A key that is an ESC or a bidi override comes out escaped in every ramp
/// error, never raw.
#[test]
fn a_control_character_key_is_escaped_in_every_ramp_error() {
    for ramps in [
        "\"\\u001B\" = { of = \"Q\", level = -1 }",
        "\"h\" = { of = \"\\u202E\", level = -1 }",
        "\"\\u001B\\u001B\" = { of = \"H\", level = -1 }",
        "\"\\u001B\" = { of = \"H\", level = 11 }",
    ] {
        let err = ramp_pack("\"H\"=\"#28140a\"", ramps, "@frame 0\nH").expect_err(ramps);
        let msg = format!("{err:#}");
        assert!(
            !msg.contains('\u{1b}') && !msg.contains('\u{202e}'),
            "{ramps}: raw control character in {msg:?}"
        );
    }
}

#[test]
fn a_palette_past_what_a_frame_can_index_is_rejected() {
    // the filler's own keys count toward the capacity
    let keys: String = FILLER_KEYS
        .into_iter()
        .chain('\u{100}'..)
        .take(PALETTE_CAPACITY + 1)
        .map(|k| format!("\"{k}\"=\"#010203\"\n"))
        .collect();
    let err = ramp_pack(&keys, "", "@frame 0\n\u{100}").expect_err("over capacity");
    assert!(format!("{err:#}").contains("at most"), "{err:#}");
    let fits: String = keys
        .lines()
        .take(PALETTE_CAPACITY)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(ramp_pack(&fits, "", "@frame 0\n\u{100}").is_ok());
    let err = ramp_pack(
        &fits,
        "\"h\" = { of = \"\u{100}\", level = -1 }",
        "@frame 0\n\u{100}",
    )
    .expect_err("a ramp is a key too");
    assert!(format!("{err:#}").contains("at most"), "{err:#}");
}

/// A pack of `animations` over the filler, whose frame files are `frames`.
fn try_pack_with_frames(animations: &str, frames: &[(&str, &str)]) -> Result<Pack> {
    load_filled_pack(
        &format!("[palette]\n\"A\"=\"#010203\"\n{animations}"),
        frames,
    )
}

fn pack_with_frames(animations: &str, frames: &[(&str, &str)]) -> Pack {
    try_pack_with_frames(animations, frames).expect("pack builds")
}

fn pack_with(animations: &str) -> Pack {
    pack_with_frames(animations, &[("f.sprite", "@frame 0\nA")])
}

/// `animations` as a manifest `try_pack_with_frames` refuses.
fn refused(animations: &str, frames: &[(&str, &str)]) -> Repr {
    try_pack_with_frames(animations, frames).expect_err("the pack does not load")
}

/// `[animations.<key>]` of `files`, 100 ms a frame.
fn anim(key: &str, files: &[&str]) -> String {
    let files: Vec<_> = files.iter().map(|f| format!("\"{f}\"")).collect();
    format!(
        "[animations.\"{key}\"]\nframes=[{}]\nframe_ms=100\n",
        files.join(", ")
    )
}

/// Each frame [`frames_read_by`] reports is one the pack fails to load
/// without, and each other is one it loads without: the read set is the
/// needed set a leave-one-out load would find.
#[test]
fn the_frames_read_are_the_frames_needed() {
    let toml = fill_pack_manifest(
        "[palette]\n\"A\"=\"#010203\"\n\
         [animations.typing]\nframes=[\"a.sprite\", \"b.sprite\"]\nframe_ms=100\n",
    );
    let frames = [
        ("a.sprite", "@frame 0\nA"),
        ("b.sprite", "@frame 0\nA"),
        ("stray.sprite", "@frame 0\nA"),
        FILLER_SPRITE,
    ];
    let read = frames_read_by(&toml, &frames).expect("the whole set loads");
    assert!(!read.iter().any(|r| r == "stray.sprite"));
    for (name, _) in frames {
        let without: Vec<_> = frames.iter().copied().filter(|&(n, _)| n != name).collect();
        assert_eq!(
            read.iter().any(|r| r == name),
            load_pack_from_strings(&toml, &without).is_err(),
            "{name}"
        );
    }
}

#[test]
fn the_filler_loads_an_empty_manifest() {
    let pack = load_filled_pack("[palette]\n", &[]).expect("every piece is filled in");
    for &piece in Piece::VARIANTS {
        assert!(
            pack.piece(piece).frames().len() >= piece.min_frames(),
            "{piece:?}"
        );
        assert!(pack.variants_of(piece).is_empty(), "{piece:?}");
    }
    for &walk in Walk::VARIANTS {
        assert_eq!(pack.stride(walk).get(), 1, "{walk:?}");
    }
}

#[test]
fn sprite_first_is_its_first_frame() {
    let pack = pack_with_frames(&anim("typing", &["one.sprite", "two.sprite"]), SIZED_FRAMES);
    let typing = pack.piece(Piece::Typing);
    assert!(std::ptr::eq(typing.first(), &typing.frames()[0]));
    assert_eq!(typing.first().width(), 1);
    assert_eq!(typing.frames()[1].width(), 2);
}

#[test]
fn a_pack_lacking_a_piece_does_not_load() {
    use toml::{Table, Value};
    let full: Table = fill_pack_manifest("[palette]\n").parse().expect("toml");
    for &piece in Piece::VARIANTS {
        let mut manifest = full.clone();
        let Some(Value::Table(animations)) = manifest.get_mut("animations") else {
            panic!("the filled manifest has animations");
        };
        animations.remove(piece.name());
        let err = load_pack_from_strings(&manifest.to_string(), &[FILLER_SPRITE])
            .expect_err("a piece is missing");
        assert!(
            matches!(err, Repr::MissingPiece { piece: p } if p == piece),
            "{piece:?}: {err:?}"
        );
    }
}

#[test]
fn an_unknown_animation_does_not_load() {
    for key in ["idle", "dsek@4x", "desk@1x", "typo@64x", "desk@60000x"] {
        let err = refused(&anim(key, &["f.sprite"]), &[("f.sprite", "@frame 0\nA")]);
        assert!(
            matches!(&err, Repr::UnknownAnimation { key: k } if k == key),
            "{key}: {err:?}"
        );
    }
}

#[test]
fn a_pack_without_city_or_characters_does_not_load() {
    for table in ["city", "characters"] {
        let mut manifest: toml::Table = fill_pack_manifest("[palette]\n").parse().expect("toml");
        manifest.remove(table);
        let err = load_pack_from_strings(&manifest.to_string(), &[FILLER_SPRITE])
            .expect_err("a required table is missing");
        assert!(
            matches!(&err, Repr::JsonManifest { message } if message.contains(table)),
            "{table}: {err:?}"
        );
    }
}

/// A manifest's JSON transcode loads, and one the parser or the schema
/// refuses is a `JsonManifest` error that says why.
#[test]
fn a_json_manifest_loads_and_a_bad_one_says_why() {
    let json = transcode(&fill_pack_manifest("[palette]\n"));
    load_pack_from_json(&json, &[FILLER_SPRITE]).expect("the transcode loads");
    let mut unknown: serde_json::Value = serde_json::from_str(&json).expect("json");
    unknown["sprites"] = serde_json::json!({});
    for (bad, why) in [("{".to_owned(), "EOF"), (unknown.to_string(), "sprites")] {
        let err = load_pack_from_json(&bad, &[FILLER_SPRITE]).expect_err("refused");
        assert!(
            matches!(&err.0, Repr::JsonManifest { message } if message.contains(why)),
            "{why}: {err:?}"
        );
    }
}

/// A walk's frames advance by its stride, so one without it does not load.
#[test]
fn a_walk_without_a_stride_does_not_load() {
    for &walk in Walk::VARIANTS {
        let piece = walk.piece();
        let files = vec!["f.sprite"; piece.min_frames()];
        let err = refused(&anim(piece.name(), &files), &[("f.sprite", "@frame 0\nA")]);
        assert!(
            matches!(err, Repr::WalkWithoutStride { walk: p } if p == piece),
            "{piece:?}: {err:?}"
        );
    }
}

#[test]
fn a_short_animation_does_not_load() {
    let frame = [("f.sprite", "@frame 0\nA")];
    for &piece in Piece::VARIANTS {
        let stride = if piece.walk().is_some() {
            "stride=1\n"
        } else {
            ""
        };
        let load = |n: usize| {
            let files = vec!["f.sprite"; n];
            try_pack_with_frames(&format!("{}{stride}", anim(piece.name(), &files)), &frame)
        };
        assert!(load(piece.min_frames()).is_ok(), "{piece:?}");
        let err = load(piece.min_frames() - 1).expect_err("one frame short");
        assert!(
            matches!(
                &err,
                Repr::TooFewFrames { key, need, have }
                    if key == piece.name()
                        && *need == piece.min_frames()
                        && *have == piece.min_frames() - 1
            ),
            "{piece:?}: {err:?}"
        );
    }
}

/// A piece of `door`, `typing` and `seated` is each the fewest frames that
/// registry row names.
#[test]
fn a_door_needs_three_frames_a_typist_two_and_any_other_piece_one() {
    for (name, need) in [("door", 3), ("typing", 2), ("seated", 1)] {
        let have = need - 1;
        let err = refused(
            &anim(name, &vec!["f.sprite"; have]),
            &[("f.sprite", "@frame 0\nA")],
        );
        assert!(
            matches!(&err, Repr::TooFewFrames { key, need: n, have: h }
                if key == name && *n == need && *h == have),
            "{name}: {err:?}"
        );
    }
}

#[test]
fn an_animation_with_no_frames_does_not_load() {
    for name in ["seated", "desk", "cat_sit"] {
        let err = refused(
            &format!("[animations.{name}]\nframes=[]\nframe_ms=100\n"),
            &[],
        );
        assert!(
            matches!(&err, Repr::TooFewFrames { key, need: 1, have: 0 } if key == name),
            "{name}: {err:?}"
        );
    }
}

/// Pins [`Pack::icon`]: each art `[icons]` names loads.
#[test]
fn an_icon_loads_the_art_it_names() {
    let pack = pack_with("[icons]\nstar=\"f.sprite\"\n");
    let star = pack.icon("star").expect("the star loads");
    assert_eq!(star.frames().len(), 1);
    assert_eq!(pack.icon_names().collect::<Vec<_>>(), ["star"]);
}

#[test]
fn an_icon_of_two_frames_is_refused() {
    let err = refused(
        "[icons]\nstar=\"two.sprite\"\n",
        &[("two.sprite", "@frame 0\nA\n@frame 1\nA")],
    );
    assert!(matches!(err, Repr::IconFrames { file } if file == "two.sprite"));
}

/// A walk carries its stride; a stride of nothing is no walk and the pack
/// refuses it.
#[test]
fn a_walk_carries_its_stride_and_refuses_a_zero_one() {
    let pack = pack_with(
        "[animations.walking]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=100\nstride=12\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n",
    );
    assert_eq!(pack.stride(Walk::Walking).get(), 12);
    assert_eq!(
        pack.piece(Piece::Walking).stride().map(|s| s.get()),
        Some(12)
    );
    assert_eq!(pack.piece(Piece::Seated).stride(), None);
    let zero = try_pack_with_frames(
        "[animations.walking]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=100\nstride=0\n",
        &[("f.sprite", "@frame 0\nA")],
    );
    assert!(zero.is_err(), "a zero stride loaded");
}

/// 1x1 and 3x1 base frames, their 2x variants and a 4x of the 1x1; each
/// also misses every other's claim.
const SIZED_FRAMES: &[(&str, &str)] = &[
    ("one.sprite", "@frame 0\nA"),
    ("two.sprite", "@frame 0\nA A\nA A"),
    ("three.sprite", "@frame 0\nA A A"),
    ("six.sprite", "@frame 0\nA A A A A A\nA A A A A A"),
    (
        "four.sprite",
        "@frame 0\nA A A A\nA A A A\nA A A A\nA A A A",
    ),
];

/// Pins [`split_density_variant`]'s one spelling per density.
#[test]
fn a_density_with_a_leading_zero_is_not_a_variant() {
    assert_eq!(split_density_variant("desk@04x"), None);
    let err = refused(
        &format!(
            "{}{}",
            anim("desk", &["one.sprite"]),
            anim("desk@02x", &["two.sprite"])
        ),
        SIZED_FRAMES,
    );
    assert!(
        matches!(&err, Repr::UnknownAnimation { key } if key == "desk@02x"),
        "{err:?}"
    );
}

/// Pins [`Pack::density_variants`]: densest first, each density once.
#[test]
fn density_variants_are_the_densities_the_pack_redraws_densest_first() {
    let pack = pack_with_frames(
        "[animations.typing]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\n\
         [animations.\"typing@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=100\n\
         [animations.\"typing@4x\"]\nframes=[\"four.sprite\", \"four.sprite\"]\nframe_ms=100\n\
         [animations.walking]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\nstride=1\n\
         [animations.\"walking@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=100\n",
        SIZED_FRAMES,
    );
    assert_eq!(pack.density_variants(), vec![d(4), d(2)]);
    assert_eq!(pack.max_density_variant(), d(4));
    assert!(pack_with("").density_variants().is_empty());
}

/// Frame count and size are both part of redrawing a piece; either one
/// wrong refuses the pack.
#[test]
fn a_variant_that_does_not_redraw_its_piece_does_not_load() {
    let typing = |variant: &[&str]| {
        format!(
            "{}{}",
            anim("typing", &["one.sprite", "one.sprite"]),
            anim("typing@2x", variant)
        )
    };
    for (variant, variant_frames, why) in [
        (
            &["two.sprite", "three.sprite"][..],
            2,
            "a frame the wrong size",
        ),
        (&["two.sprite"], 1, "a frame short"),
        (&["three.sprite"], 1, "a frame short and the wrong size"),
        (
            &["two.sprite", "two.sprite", "two.sprite"],
            3,
            "a frame over",
        ),
    ] {
        let err = refused(&typing(variant), SIZED_FRAMES);
        assert!(
            matches!(&err, Repr::VariantDoesNotRedraw {
                key, base_frames: 2, variant_frames: v
            } if key == "typing@2x" && *v == variant_frames),
            "{why}: {err:?}"
        );
    }
    assert!(try_pack_with_frames(&typing(&["two.sprite", "two.sprite"]), SIZED_FRAMES).is_ok());
}

/// Pins the MATCHING base frame, in [`variant_redraws`] and in load's
/// refusal of a variant that fails it.
#[test]
fn each_variant_frame_is_proved_against_the_matching_base_frame() {
    let walking = |variant: &[&str]| {
        format!(
            "{}stride=1\n{}",
            anim("walking", &["one.sprite", "three.sprite"]),
            anim("walking@2x", variant)
        )
    };
    let pack = pack_with_frames(&walking(&["two.sprite", "six.sprite"]), SIZED_FRAMES);
    assert!(variant_redraws(
        pack.piece(Piece::Walking),
        d(2),
        &pack.variants_of(Piece::Walking)[&d(2)]
    ));
    // each frame fits a base frame, but not the matching one
    let err = refused(&walking(&["six.sprite", "two.sprite"]), SIZED_FRAMES);
    assert!(matches!(err, Repr::VariantDoesNotRedraw { .. }), "{err:?}");
}

/// Pins [`DENSITY_VARIANT_SEP`]'s claim: `@4x` of a 2-wide base is 8 wide.
#[test]
fn a_variant_that_lies_about_its_density_does_not_load() {
    let err = refused(
        "[animations.desk]\nframes=[\"one.sprite\"]\nframe_ms=100\n\
         [animations.\"desk@4x\"]\nframes=[\"four.sprite\"]\nframe_ms=100\n",
        &[
            ("one.sprite", "@frame 0\nA A"),
            // 4 wide, not the 8 that `@4x` of a 2-wide base claims.
            ("four.sprite", "@frame 0\nA A A A"),
        ],
    );
    assert!(
        matches!(&err, Repr::VariantDoesNotRedraw { key, .. } if key == "desk@4x"),
        "{err:?}"
    );
}

/// Pins the empty case of the variant's frame count.
#[test]
fn an_empty_density_variant_does_not_load() {
    let err = refused(
        "[animations.desk]\nframes=[\"f.sprite\"]\nframe_ms=100\n\
         [animations.\"desk@4x\"]\nframes=[]\nframe_ms=100\n",
        &[("f.sprite", "@frame 0\nA")],
    );
    assert!(
        matches!(&err, Repr::TooFewFrames { key, need: 1, have: 0 } if key == "desk@4x"),
        "{err:?}"
    );
}

/// Pins [`Pack::max_density_variant`].
#[test]
fn the_packs_max_density_is_the_scale_a_painter_has_to_round_to() {
    let pack = |extra: &str| {
        try_pack_with_frames(
            &format!(
                "[animations.desk]\nframes=[\"one.sprite\"]\nframe_ms=100\n\
                 [animations.\"desk@2x\"]\nframes=[\"two.sprite\"]\nframe_ms=100\n\
                 [animations.plant]\nframes=[\"one.sprite\"]\nframe_ms=100\n{extra}"
            ),
            SIZED_FRAMES,
        )
    };
    let plain = pack_with("");
    assert_eq!(
        plain.max_density_variant(),
        Density::ONE,
        "a pack with no variants must not push a painter off the cell's own scale"
    );
    assert_eq!(
        pack("[animations.\"plant@4x\"]\nframes=[\"four.sprite\"]\nframe_ms=100\n")
            .expect("loads")
            .max_density_variant(),
        d(4)
    );
    assert!(
        pack("[animations.\"typo@64x\"]\nframes=[\"one.sprite\"]\nframe_ms=100\n").is_err(),
        "a variant of an unregistered base must not inflate the pack's density"
    );
    assert!(
        pack("[animations.\"plant@8x\"]\nframes=[\"two.sprite\"]\nframe_ms=100\n").is_err(),
        "a variant no renderer could draw must not round a painter's scale to it"
    );
}

/// Pins [`claimed_variant_size`].
#[test]
fn a_claim_no_frame_can_meet_does_not_load() {
    let row = |w: usize| format!("{}\n", "A ".repeat(w).trim_end());
    let base = format!("@frame 0\n{}", row(40_000));
    let variant = format!("@frame 0\n{0}{0}", row(u16::MAX as usize));
    let err = refused(
        "[animations.desk]\nframes=[\"base.sprite\"]\nframe_ms=100\n\
         [animations.\"desk@2x\"]\nframes=[\"variant.sprite\"]\nframe_ms=100\n",
        &[("base.sprite", &base), ("variant.sprite", &variant)],
    );
    assert!(
        matches!(&err, Repr::VariantDoesNotRedraw { key, .. } if key == "desk@2x"),
        "80_000 wide is claimed, 65_535 is found: {err:?}"
    );
}
