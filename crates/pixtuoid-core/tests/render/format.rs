use pixtuoid_core::sprite::error::{LineError, PackError, SpriteError};
use pixtuoid_core::sprite::format::{Pack, Piece, load_filled_pack};
use pixtuoid_core::sprite::{Frame, Rgb};

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

/// A caller can match the failure, and `{:#}` names each step of it once.
#[test]
fn a_bad_pixel_is_matchable_and_its_chain_prints_each_step_once() {
    let err = load_filled_pack(
        "[palette]\n\"A\"=\"#010203\"\n\
         [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n",
        &[("f.sprite", "@frame 0\nA z")],
    )
    .unwrap_err();
    assert!(
        matches!(
            &err,
            PackError::Decode {
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
        format!("{:#}", anyhow::Error::from(err)),
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
        PackError::Decode {
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
    let src = std::fs::read_to_string("tests/render/fixtures/mini.sprite").unwrap();
    let frames = parse(&src).unwrap();

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
        matches!(&err, PackError::TooFewFrames { key, need: 2, have: 1, .. } if key == "typing"),
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
        matches!(&err, PackError::UnknownAnimation { key, .. } if key == "idle"),
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
