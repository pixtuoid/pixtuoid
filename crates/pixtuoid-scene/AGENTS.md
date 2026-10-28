# pixtuoid-scene — render+simulation engine crate guide

The **backend-agnostic render + simulation engine**: layout geometry,
pose/walk/pathfinding, the frame entry every painter calls (`look::render`),
the color-theme MODEL, pets, chitchat, frame cache, bundled
sprite pack. The three painters (`tui`, `floating`, `pixtuoid-web`) sit on top.
Module map: `ls src/` — each file's `//!` header is its annotation.
Cross-cutting rules: workspace [`AGENTS.md`](../../AGENTS.md).

## Screen-space compass (THE convention — read before reasoning about N/S)

Directions in this crate are **SCREEN-SPACE**, map-style (north = up), NOT
real-world headings. Pin this and stop re-deriving it:

- **North = −y = screen TOP** — the far wall, the floor-to-ceiling windows,
  the city skyline (the north wall band, `layout/compute.rs::top_margin`).
  "Behind" a piece.
- **South = +y = screen BOTTOM** — the near side, the FRONT, toward the
  viewer. This is the z-sort **"south row"** (`placement.rs` pins the z-sort
  row to the box's south row) and the **south-anchored** ground strip
  (`GroundAlign::End`): a sprite's front/base row.
- East = +x (right), West = −x (left).

A piece's approach set is canonical (facing-South) then rotated by live
`Facing` — a `HomeDesk`'s `facing` is the authority; never assume the
canonical set is the live one. (The compass stays screen-space even where
real-world geography disagrees — flipping it would invert the z-sort/"south
row" vocabulary across the crate for zero behavior change.)

## The sprite format

The rules the bundled pack (`sprites/default/`) is drawn to. A pack that breaks
one does not load (`PackError`, and `pack::ArtError` for what the office reads
off its art); the test-only `pack::validate::validate_pack` finds art that loads but is
not as authored, and `the_bundled_pack_passes_its_own_validation` fails on a finding.

Characters are recolored per agent by palette key: whatever a pack draws with `B`
(shirt), `H` (hair), `S` (skin) or `P` (pants) takes each agent's colors, and
every other key keeps the pack's color, even one with the same RGB. A `[ramps]`
entry such as `"h" = { of = "H", level = -1 }` is a key with no color of its own:
`H` one step darker and cooler (a positive level is lighter and warmer), so it
follows whatever color replaces `H`. `of` must name a key with an opaque color in
`[palette]`, and a key is declared in one table, never both.

A pack can also redraw an animation on a denser grid, registered as
`<name>@<N>x` (`desk@4x` is `desk` drawn on a 4x grid). Each frame is exactly
`N` times the size of the matching base frame, and the frame counts match,
or the pack does not load. The classic renderer, the half-block terminal office,
draws the base art. Variants are for the pixel-graphics cutaway,
which the `floating` window and the site's live office paint, and `run` over kitty's
graphics protocol, SIXEL or iTerm2's inline images: it takes the
densest variant whose `N` divides its render scale and draws it as it is — a
variant carries its own front, where a desk's top-down base art gets a front
face derived under it. The recolor keys and `[ramps]` apply at every density.
A variant plays its base's `frame_ms` and `stride`, and validation reports
one that sets them apart (a single-frame base's `frame_ms`, which nothing
steps, excepted); it also reports a looping animation whose
`frame_ms` is not a whole number of the office's beats.

A pet's walk (`cat_walk`, `dog_walk`) is drawn facing east: the renderer
mirrors it when the pet heads west, so a walk drawn facing west walks backwards.

A desk (`desk`, `desk_north`) marks where its cup and token tower stand with `@mark cup <x> <y>` and `@mark tower <x> <y>` on its first frame, and draws its lamp's bulb in palette key `9`, at every density: the lamp's pool centres on those pixels. Without either, the pack does not load.

`desk_north` is `desk` seen from its sitter's side, so it stands its props
mirrored, each mark naming the prop's bottom-right cell instead of its
bottom-left. `desk_front` is drawn over a `desk`'s props on the desk's own
canvas: whatever of the desk stands between the viewer and its sitter's props,
its monitor.

A symbol in text is an icon, `[icons] <name> = "<file>"` under the name
`display::Icon::art` gives it: one frame a line tall, as many cells wide as its
terminal glyph takes less the last cell's gap. Its pixels in key `ι` (`pack::ICON_INK_KEY`), and
that key's ramps, take the text's ink; every other key keeps the pack's
colour. Screen text finds an icon by its glyph, so icons that write one glyph
draw one art. The icon tests pin the size, the ink and the shared art.

A walk (a person's `walking`, `walking_back` and `walking_coffee`, a pet's
`cat_walk` and `dog_walk`, the gateway mascot's `lobster_walk`) takes
`stride = <pixels>`: how far, on the base grid, the walker travels in one full
cycle of its frames. Its frames, and its `@Nx` variants', then step by the
ground covered, not by `frame_ms`. A walk without one does not load.

A frame can name points on itself for the renderer: `@mark <name> <x> <y>` in
its `@frame` block, at column `x` and row `y` from the frame's top-left. A
frame names each mark once. `head.<view>` is its head, with `view` one of
`front`, `back`, `side` or `crown`; a frame has at most one.

A pack can dress its variant characters in hairstyles. A
`[hairstyles."<name>@<N>x"]` table gives any of the views a `behind` layer, drawn
under the body, and an `over` layer, drawn on top. Each layer is a one-frame
sprite marking its own head in that view, and it is laid mark on mark on the
frame's. Every agent wears one of the pack's styles, picked by name from its id,
so a pack ships the same styles at every density it dresses. The layers take
the agent's recolor as the body does. A frame whose view its style leaves out is
drawn bare, as is a variant frame with no head mark, and a layer reaching past
the frame's sides is cut off: validation reports each, and a style
at a density the pack draws no character at.

`[characters] outline = "<key>"` draws one line round every marked variant
frame, bare or dressed: a pack draws its bodies and layers
unoutlined, so no line runs between hair and face. A dressed frame may rise
above its body's box, by the hair and the line over it. Base art is never
dressed or outlined.

A pack also ships the city seen through the office's windows. `[city]` names
the palette key of each of its seven materials — `facade`, `shade`, `roof`,
`glass`, `mullion`, `detail` and `sign` — and a building is drawn in those keys
alone, since its colours come from its depth and the sky rather than the pack.
`[buildings.<name>]` is one building: a one-frame `sprite` and the `planes`
(`"mid"`, `"near"`) it may stand in. `[buildings."<name>@<N>x"]` redraws its
sprite at exactly `N` times the size. A building that breaks these rules fails
the pack's load. Each connected run of `glass` is one window, and more of them
burn as night falls. How tall the city stands is a share of the window, so a
short window shows the tops of the towers and a tall one shows them whole. Every window
looks out on one city, the pack's buildings standing in front of plain blocks
on the horizon.

## The corpus census

`just corpus-all` drives every local transcript through the real render seam;
what it reports and why it doesn't gate: `examples/corpus_check.rs`'s `//!` header.

## When refactoring

Changes to `derive_with_routing`, `WalkState`, or the pixel passes add or
update a frame-by-frame continuity guard (`walk/tests.rs`, `pose/tests.rs`,
the binary's `tui_renderer/harness`) — the flash/teleport/replay regressions
all came back as failing tests first.
