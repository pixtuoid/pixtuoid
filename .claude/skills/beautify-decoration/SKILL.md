---
name: beautify-decoration
description: "Iterates on the visual identity of a top-down pixel-art decoration (sprite + layout integration) in pixtuoid. Use when redesigning an existing decoration (pantry, lounge, meeting room, cubicle decor) or adding a new one. Captures the rebuild trap, the visual-verification loop, resolution constraints, sprite-format pitfalls, and the layout-integration checklist."
---

# beautify-decoration

A repo-specific iteration loop for visually redesigning a decoration in `pixtuoid`. Follow it when the user says "beautify X" or "make Y look better": it skips rebuild traps and visual-design dead ends the codebase alone doesn't show. The sprite format itself is the [scene guide](../../../crates/pixtuoid-scene/AGENTS.md#the-sprite-format)'s.

## When to use

- Redesigning an existing decoration sprite (pantry, lounge, meeting, cubicle decor)
- Adding a new fixture (pendant lamp, water cooler, chalkboard, etc.)
- The user says "items look too small / don't read like X / blend together"
- After a sprite edit, when "I don't see any change"

## The visual-iteration loop

1. Edit the sprite or the layout.
2. `cargo build --release --example snapshot`
3. `./target/release/examples/snapshot /tmp/snap.png`
4. Crop the render, either way:
   - `uv run --locked -- python3 scripts/crop-snapshot.py /tmp/snap.png --scale 3 -q <quadrant>`; its `QUADRANTS` table is the zone map.
   - Skip the quadrant guessing and the Python step: `snapshot --crop-furniture <piece>` (`crates/pixtuoid/examples/snapshot/encode.rs` lists the pieces) or `--crop-agent <label|session>` renders a `CROP_WINDOW` already centred on the target.
5. Read the cropped PNG, critique it, and go back to step 1.
6. When the [self-critique checklist](#self-critique-checklist--before-sending-a-render-to-the-user) passes, send the crop to the user with a short caption. The user is the final judge of "does it look like a fridge / coffee machine / etc."
7. `cargo build --release --workspace` to rebuild the live binary too. Never skip it: step 2 does not rebuild the main binary, so `./target/release/pixtuoid run` shows no sprite change until the workspace is rebuilt. Skipping it is how "I changed the sprite but nothing happened in the live TUI" bugs get filed.
8. Name each rejected design and why it lost in the PR body. Never skip it: a sprite change's PR body, which becomes its squash message on main, gives a one-line reason for each rejected design, as the org rules' "why this approach", so a later editor doesn't retry the same dead ends.

## Sharp edges

### 1. The rebuild trap

- `cargo build --release --workspace` does not rebuild examples. Iterate on `examples/snapshot` with `cargo build --release --example snapshot`.
- `crates/pixtuoid-scene/build.rs` embeds every `.sprite` in `sprites/default/` at compile time, and a sprite edit, add or remove rebuilds it.
- If unsure, verify with `strings target/release/examples/snapshot | grep "<some unique string from your sprite>"`.

### 2. Snapshot size gates the large sprite variants

- `examples/snapshot` defaults to its `COLS`×`ROWS` cells, and that default clears every gate below; don't shrink it while iterating.
- Several layouts (pantry, corridor appliances) have variants conditional on room dimensions. Corridor items (vending machine, printer) appear only when the cubicle aisle clears `VENDING_MIN_AISLE_*` / `PRINTER_MIN_AISLE_*` (`layout/compute.rs`).
- The large `pantry` counter needs the left column to fit `PANTRY_COUNTER_LARGE_W` plus margin (the `pantry_counter_size` pick in `layout/compute.rs`); below that, `pantry_small.sprite` is used (`layout::pantry_counter_anim`).

### 3. Resolution budget

- A terminal cell shows one pixel column and two pixel rows (half-block ▀): a 1x sprite is as many cells wide as it has pixels, and half as many tall.
- Subzones narrower than about 5 display cells blur into pixel noise; users can't read them.
- Sub-pixel detail (a 1-cell handle, a 1-cell stripe) is invisible. Iterate on silhouette and color identity, not pixel polish.
- Budget subzones against the sprite's width in cells: the pantry's 32-wide counter read at three zones (8/10/10), not six. Drop items; don't shrink them.

### 4. Identity mistakes that look identical to each other

Symptoms of weak identity:

- **Transparent body (`.`)**: the wall color shows through, weakening the silhouette. Use a solid fill color for appliances.
- **All-dark appliances**: a row of `M`-bodied items reads as "row of dark boxes." Give each appliance a distinct base color (e.g., `w` white fridge against `M` dark coffee machine + `M` dark microwave with `q` glass).
- **Symmetric H-frame on a white box** → reads as washing machine, not fridge. Use asymmetric handles (single-side handle, or center-French-door pair).
- **Cyan + blue dispenser dots next to each other** → reads as cyan-cyan because `b` is dark and gets dim. Space them out or use `c` + `r`.

### 5. Sprite-format pitfalls

- Every row in a `.sprite` file has exactly the same number of space-separated cells. Off-by-one is the most common bug.
  - Verify with `awk '/^@/{next}/^#/{next}NF{print NR": "NF}' crates/pixtuoid-scene/sprites/default/foo.sprite`: all NF values must match.
  - Or visualize packed rows: `awk '/^@/{next}/^#/{next}NF{for(i=1;i<=NF;i++)printf "%s",$i;print " ["NF"]"}' foo.sprite`.
- Reuse existing palette keys when possible; new keys go in the `[palette]` section of `crates/pixtuoid-scene/sprites/default/pack.toml`.
- A sprite whose header carries the provenance line (every `foo@Nx.sprite`, and the 1x pieces the generator owns) is drawn by `scripts/gen-art.py`: redraw it there and run `just gen-art`. `just gen-art-check` fails on a hand edit.

### 6. Layout integration checklist

When a sprite changes size:

1. Update the decoration's footprint in the `furniture_def(Furniture)` geometry table in `crates/pixtuoid-scene/src/layout/decor.rs`, the single source of footprint and visual: `mask::build_walkable_mask` (waypoints via `approach::obstacle_footprint`), `approach.rs` and the z-sort read it. Never hardcode a `(w, h)` at the mask stamp site; it would diverge from the table that `stand_point`/approach and render-centering read.
2. A non-waypoint obstacle (plant, wall decor, pod decor) is stamped from its `FurnitureDef` row too, via `furniture_def(kind.furniture()).footprint`, not an inline literal, so the same table edit covers it.
3. Run `just test -p pixtuoid-scene`. The `walkable_is_one_connected_region` check (`layout/placement_sweep.rs`, run by `every_swept_office_holds_its_invariants`) catches mask/sprite mismatches: it sweeps buffer sizes × seeds and asserts every walkable pixel is reachable from the door threshold. `narrow_band_connectivity_boundary_scan` reruns the same check at the step-1 widths that discrete grid skips.
4. If the connectivity test fails at a small buffer (`SWEEP_SIZES` starts at the minimum layout size), the sprite is too big for that pantry. Add a `_small` variant and a conditional pick; the `PantryRoom::counter_size` field and `SceneLayout::pantry_counter_size()` show the pattern.
5. If you added a variant, register both `foo.sprite` and `foo_small.sprite` in `crates/pixtuoid-scene/sprites/default/pack.toml`: an unregistered sprite fails `every_bundled_sprite_is_a_frame_the_pack_loads`.
6. If the piece has a density variant (`foo@Nx.sprite`, drawn by `scripts/gen-art.py`), resize it there too, with its 1x where the generator owns that: a variant not exactly N× its base fails `the_bundled_pack_passes_its_own_validation`.

## Self-critique checklist — before sending a render to the user

Run this checklist before each render you send in a beautify loop, and state each row's result (✅/⚠️/❌) in the message. Fix any ❌ first; call out any ⚠️ you ship so the user sees the trade-off.

| Check | What it means |
|---|---|
| Stranger-ID | If a stranger saw this with no context, would they identify each new element as the intended thing? Name each element explicitly. |
| Visually differs | Diff is noticeable, not a sub-pixel tweak. If hash-identical to last attempt, you didn't actually rebuild. |
| Subzone width | Each new sub-element is at least 5 display cells wide (§3). |
| Color distinctness | New elements use colors distinct from immediate neighbours. |
| Alive | The asset ships what makes it alive — idle frames (`frame_ms`), a state-driven change, a response to weather or window light, or variety across agents — named in the PR body. |
| `just test` | The connectivity tests pass (§6 step 3). |
| `--debug-walkable` | Rendered the walkable debug layer and visually checked no narrow / isolated walkable pockets near the new element. |

## Adding a new decoration

1. Sketch the design as a list of cells per row (count exactly).
2. Pick a palette: reuse `pack.toml` keys; add new ones only if necessary.
3. Write the `.sprite` file; verify row widths with the awk command in §5.
4. Add a `Piece::Foo` variant (scene `pack/piece.rs`) and the `[animations.foo]` block to `pack.toml` (`build.rs` embeds the file itself): a pack without it, or a key no piece names, does not load.
5. Decide where it lives in the layout: add a `Point` placement in `SceneLayout::compute`.
6. Give it a `Furniture` variant and a `furniture_def` row (§6 step 1).
   - A new plant, wall-decor or pod-decor kind is then stamped by its collection's loop in `mask::build_walkable_mask`.
   - A one-off piece also needs a `MaskObstacles` field and its own `stamp_ground` from that row, like `fish_tank` (or add a waypoint kind if it's interactive).
7. Add a `DrawableKind::Foo` variant and a `paint_drawable` arm if z-sorting matters.
8. Run the connectivity tests (§6 step 3).
9. Snapshot and iterate.
