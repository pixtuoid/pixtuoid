# pixtuoid-scene — render+simulation engine crate guide

The **backend-agnostic render and simulation engine**: layout geometry,
pose/walk/pathfinding, the frame entry every painter calls (`look::render`),
the color-theme model, pets, chitchat, the frame cache, the sprite format and
bundled pack, and the grid/walkable mask. The three painters (`tui`,
`floating`, `pixtuoid-web`) sit on top. Module map: `ls src/`; each file's
`//!` header annotates it. Cross-cutting rules: the workspace
[`AGENTS.md`](../../AGENTS.md).

## Screen-space compass

Directions in this crate are screen-space, map-style (north is up), never
real-world headings:

- **North** is −y, the screen top: the far wall, the floor-to-ceiling windows
  and the city skyline (the wall band `layout/compute.rs::top_margin` sizes).
  "Behind" a piece is north of it.
- **South** is +y, the screen bottom: the near side, the front, toward the
  viewer. A sprite's front/base row is its "south row": the z-sort row
  (`layout/placement.rs`'s `sort_row_at_is_the_sprite_south_row_for_every_pivot`)
  and where a south-anchored ground strip (`GroundAlign::End`) sits.
- **East** is +x (right); **west** is −x (left).
- A piece's approach set is canonical, written facing south, then rotated by
  its live `Facing`. Read the live facing from `HomeDesk::facing`; never assume the
  canonical set is the live one.
- Keep the compass screen-space even where real-world geography disagrees:
  flipping it would invert the z-sort's "south row" vocabulary across the
  crate for no change in behavior.

## The sprite format

The rules the bundled pack (`sprites/default/`) is drawn to.

- A pack that breaks one does not load: `PackError`, which also covers what
  the office reads off its art.
- Art that loads but is not as authored is a finding of the test-only
  `pack::validate::validate_pack`; `the_bundled_pack_passes_its_own_validation`
  fails on any finding.

### Recolor

- Character art is recolored per agent by palette key: whatever a pack draws
  in `character::colors`'s `SHIRT_KEY`, `HAIR_KEY`, `SKIN_KEY` or `PANTS_KEY`
  takes each agent's colors. Every other key keeps the pack's color, even one
  with the same RGB.
- A `[ramps]` entry such as `"h" = { of = "H", level = -1 }` is a key with no
  color of its own: `H` one step darker and cooler (a positive level is
  lighter and warmer), so it follows whatever color `H` takes.
- A ramp's `of` names a key with an opaque color in `[palette]`.
- A key is declared in one table, `[palette]` or `[ramps]`, never both.

### Density variants

- A pack can redraw an animation on a denser grid, registered as
  `<name>@<N>x` (`desk@4x` is `desk` drawn on a 4x grid).
- Each variant frame is exactly `N` times the size of its base frame, and the
  frame counts match, or the pack does not load.
- The classic renderer draws the base art, at any scale.
- Variants are for the pixel-graphics cutaway, which paints the `floating`
  window, the site's live office, and `run` over a terminal graphics protocol
  (the binary's `graphics::ImageProtocol`). It takes the densest variant whose
  `N` divides its render scale (`pack::density::densest`) and draws it without
  a derived front: a variant carries its own front, where a desk's top-down
  base art gets a front face derived under it.
- The recolor keys and `[ramps]` apply at every density.
- A variant plays its base's `frame_ms` and `stride`. Validation reports a
  variant that sets either apart, except a single-frame base's `frame_ms`,
  which nothing steps.
- Validation also reports a looping animation whose `frame_ms` is not a whole
  number of the office's beats (`anim::FULL_TICK_MS`).

### Desks

- A desk (each `pack::Desk`'s `piece()`) marks where its cup and token tower stand with `@mark cup <x> <y>` and `@mark tower <x> <y>` on its first frame, and draws its lamp's bulb in palette key `9`, at every density. The lamp's pool centres on those pixels.
- A pack whose desk lacks a mark or the bulb does not load.
- `desk_north` is `desk` seen from its sitter's side, so it stands its props
  mirrored: each mark names the prop's bottom-right cell instead of its
  bottom-left.
- `desk_front` is drawn over a `desk`'s props, on the desk's own canvas: the
  part of the desk that stands between the viewer and its sitter's props, its
  monitor.

### Icons

- A symbol in text is an icon: `[icons] <name> = "<file>"`, under the name
  `display::Icon::art` gives it.
- Its art is one frame, one text line tall (`display::text::LINE_H`), and as
  wide as the cells its terminal glyph takes, less the gap column after the
  last cell (each cell is `display::text::ADVANCE` wide, its gap included).
- Its pixels in `pack::ICON_INK_KEY`, and that key's ramps, take the text's
  ink; every other key keeps the pack's colour.
- Screen text finds an icon by its glyph, so icons that write one glyph draw
  one art.
- The icon tests pin the size, the ink and the shared art.

### Walks

- A walk, each piece `pack::Walk` lists (`Walk::piece`, whose `Piece::name`
  is its `[animations]` key), takes `stride = <pixels>`: how far, on the base
  grid, the walker travels in one full cycle of its frames.
- Its frames, and its `@Nx` variants', then step by the ground covered, not by
  `frame_ms`.
- A walk without a stride does not load.
- A pet's walk is drawn facing east: the renderer
  mirrors it when the pet heads west, so a walk drawn facing west walks
  backwards.

### Marks

- A frame names points on itself for the renderer with `@mark <name> <x> <y>`
  in its `@frame` block, at column `x` and row `y` from the frame's top-left.
- A frame names each mark once.
- `head.<view>`, where `<view>` is a `sprite::HeadView::name()`, at most one
  per frame, is its brow: where its hair is laid mark on mark and a subagent's
  cap sets its brim.

### Hairstyles and outline

- A pack can dress its variant characters in hairstyles. A
  `[hairstyles."<name>@<N>x"]` table gives any of the views a `behind` layer,
  drawn under the body, and an `over` layer, drawn on top.
- Each layer is a one-frame sprite marking its own head in that view, laid
  mark on mark on the frame's.
- Every agent wears one of the pack's styles, picked by name from its id, so a
  pack ships the same styles at every density it dresses.
- The layers take the agent's recolor, as the body does.
- Validation reports each of these:
  - a frame whose view its style leaves out, drawn bare;
  - a variant frame with no head mark, drawn bare;
  - a layer reaching past the frame's sides, which is cut off;
  - a style at a density the pack draws no character at.
- `[characters] outline = "<key>"` draws one line round every marked variant
  frame, bare or dressed. A pack draws its bodies and layers unoutlined, so no
  line runs between hair and face.
- A dressed frame may rise above its body's box, by the hair and the line over
  it.
- Base art is never dressed or outlined.

### City

- A pack also ships the city seen through the office's windows.
- `[city]` names a palette key for each `pack::Material`, under the name
  `Material::name()` gives it. A building is drawn in those keys alone, since
  its colours come from its depth and the sky rather than the pack.
- `[buildings.<name>]` is one building: a one-frame `sprite` and the `planes`
  it may stand in, each a `pack::CityPlane` by the name
  `CityPlane::from_name` reads. `[buildings."<name>@<N>x"]` redraws its sprite
  at exactly `N` times the size.
- A building that breaks these rules fails the pack's load.
- Each connected run of `Material::Glass` is one window, and more of them
  burn as night falls.
- How tall the city stands is a share of the window, so a short window shows
  the tops of the towers and a tall one shows them whole.
- Every window looks out on one city: the pack's buildings standing in front
  of plain blocks on the horizon.

## When refactoring

- A change to `derive_with_routing`, `WalkState` or a pixel pass
  (`pixel_painter`, `cutaway/paint.rs`) adds or updates a frame-by-frame
  continuity guard: `walk/tests.rs`, `pose/tests.rs`, or the binary's
  `tui/tui_renderer/harness`. Each past flash, teleport and replay regression
  came back as a failing guard first.
