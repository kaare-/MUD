# MUD — Plan / Backlog

Companion to `DESIGN.md` (which is the philosophy). This file tracks
what has shipped, what is next, and what is deferred. Update as work
lands. Kept short on purpose.

The user is the 3D-game-native non-artist described in `DESIGN.md`
§10. Every backlog item is scored against: *does it help produce a
convincing lump of clay, quickly*.

---

## Legend

- `[x]` shipped
- `[~]` in flight
- `[ ]` planned
- `(spike)` needs a design pass before implementation
- `(defer)` explicitly not now

---

## Shipped so far (Stages 0–3 + Add/Remove pass)

- `[x]` Sparse-tile SDF grid at 352³, 1.5 mm voxel (528 mm work
  volume — see `SPARSE_THEN_LAYERS.md` Tracks A1–A4), chunked
  meshing with chunk entities spawned only where there's geometry.
- `[x]` Orbit / pan / zoom camera, Q / E turntable.
- `[x]` Workbench floor (y = 0) as a hard clip.
- `[x]` Clay tool (Add / Remove) with soft CSG (magic clay).
- `[x]` Cookie cutters (circle, square, hex, star).
- `[x]` Wire cutter (LMB drag → planar slab cut). Slab CSG iterates
  only allocated tiles (Track A3) and uses a **soft-max at the
  corner** where the cut plane meets the outer surface: the
  resulting SDF is C¹
  around the ring instead of kinked, so surface-nets can't
  average a saw-tooth edge across the cut. Slab is 3 mm (two
  voxels) so the mesher sees a real gradient across the gap.
- `[x]` Smooth brush, paddle.
- `[x]` Mirror symmetry (piece-local X = 0).
- `[x]` Undo / redo (stroke-granular, `Shift+Ctrl+Z` per-op reserved).
- `[x]` `.mudclay` save / load, Save-As and Open dialogs (in-app).
- `[x]` STL export to CWD (`Ctrl+E`).
- `[x]` Egui menus + tool palette + status strip.
- `[x]` Hover ghost preview matching the stamp centre; ghost stays
  visible (dimmed) during a stroke so the brush footprint reads
  while dragging.
- `[x]` Ray march robust against warped SDFs (front-face lock).
- `[x]` Add/Remove UX: soft join, tangent-plane lock, side-column
  rings, no camera stalks, no top-down 45° climb.
- `[x]` Turntable slowed; sculpt syncs to the live piece transform.
- `[x]` Stroke lifecycle survives release-over-UI and load / tool
  switch.

---

## Immediate backlog (user pass, 2026-07-14)

Ordered roughly easiest → hardest. Each item includes what it needs.

1. `[x]` **Export STL via dialog.** — `Ctrl+E` and `File → Export STL…`
   both open the Export-STL-As dialog; there is no divergent
   "quick auto-name" path any more.

2. `[x]` **Clear worktable.** — `Ctrl+N` / `File → New`. Swaps in an
   empty grid, clears undo + live stroke.

3. `[x]` **Add material on an empty worktable.** — Shift+LMB on the
   empty bench projects the ray onto y = 0 and deposits a blob
   sitting on the bench. Every other tool still bails on a miss.
   Top-view turntable coils latch `bench_paint` for the stroke so
   later stamps stay at bench height instead of tip-chasing toward
   the camera once the ray hits the previous bead.
   *(Domain still 288 mm — worktable-scale Add is Track A in
   `SPARSE_THEN_LAYERS.md`.)*

4. `[x]` **Primitives menu.** — `Shift+N` / `File → Insert Primitive…`.
   Sphere / Cube / Cylinder / Torus with a size slider; unioned into
   the current grid, journaled as one undo stroke.
   *(Silent fuse → fixed by Track B: Insert creates a new layer.)*

5. `[x]` **Selection tool: pick tiny bits.** Option A (single-grid
   component labels) shipped. New `Select` tool (`9`); LMB picks the
   connected piece under the cursor. `Delete` / `Backspace` removes
   the selected piece as a single undo stroke. `A` toggles
   **active-only** — sculpt stamps skip when the hit doesn't fall on
   the selected piece. HUD shows `Sel #N: X vx · Y mm³` + Delete
   button + Active-only toggle. **Selected piece is boxed** by a
   yellow wireframe hugging its voxel-space AABB (turns with the
   piece). Labels are recomputed lazily (invalidated after any
   stroke / undo / redo / Insert / New / Load). Option B (multi-piece
   scene) → superseded by **Layers** in `SPARSE_THEN_LAYERS.md`.

6. `[x]` **Rigid gravity (rest).** `File → Rest pieces on bench`
   (`Ctrl+G`). Every floating connected component is translated
   along −Y so its lowest voxel lands at `iy = 0`. No plastic
   deformation, no inter-component collision (overlapping landing
   zones union naturally through min-SDF). Journaled as one undo
   stroke via `sculpt_core::rest_components_on_bench`. Rotation to
   a minimum-face rest pose is *not* implemented — components keep
   their orientation. **The shift now moves the full narrow band
   around each interior**, not just the `φ < 0` cells: the old
   interior-only shift sheared the SDF at the boundary and made
   the surface look crumpled after a rest.

7. `[x]` **Move body / primitive.** New `Move` tool (`0`) plus an
   XYZ mm widget (top-right) **and a 3D axis gizmo**: three
   arrows (red X, green Y, blue Z) anchored on the selected
   piece. Drag an arrow for a live-preview translation along
   that axis; the widget numbers update in real time; the
   piece commits on mouse release as a single undo entry. Users
   who prefer typing can enter numbers into the widget and
   click Apply. All moves snap to whole voxels via
   `sculpt_core::translate_component`. Insert Primitive
   auto-selects the new piece and switches into Move.

8. `[x]` **View menu.** `View → Workbench grid` toggles a 400 mm
   translucent grid on the bench. `View → Perspective / Top /
   Bottom / Front / Back / Left / Right` snap the orbit camera
   to a face-on preset; distance and target are preserved.

9. `[x]` **Gravity settle.** See `PLASTIC_GRAVITY.md`.
   `Sculpt → Settle (gravity)…` (`Ctrl+Shift+G`): (1) rigid-drop
   every floating lump to the workbench, (2) soft clay sandpile-
   collapses tall columns down onto the bench (not a surface peel).
   Softness 0 = drop only; 1 = pancake. One undo stroke. Follow-ups:
   elastic bow curves, multi-layer, volume-perfect redistribution.

10. `[x]` **Settings menus + workbench grid default on.**
    `Edit → Tool parameters…` (size / advance / smooth / magic clay /
    symmetry) and `Edit → Preferences…` (grid, turntable period,
    default settle plasticity). Workbench grid starts visible.

11. `[x]` **Layer visibility remesh fix.** Hiding despawns chunk
    meshes; showing again re-dirties allocated chunks so they
    respawn (previously stayed invisible until the next sculpt).

---

## Next architecture track (locked 2026-07-15)

Full plan: **`SPARSE_THEN_LAYERS.md`**.

**Order: sparse SDF first, then same-domain layers.**
Rationale: unlock a worktable-scale domain so empty-bench Add /
coil-sausage work is real; layers then cost tile sets, not N× dense
grids. Per-body transforms (old Option A) stay deferred.

Locked product calls:

- Insert Primitive → **new layer by default**; Merge is explicit.
- Tools → **active layer only** in v1 (multi-active later).
- Per-layer **visibility** toggle with the Layers UI.
- Custom **32³ tile** store behind `Grid` (not OpenVDB yet).

Suggested PR stack (see doc for acceptance checks):

1. `[x]` **P0** — this plan (docs).
2. `[x]` **P1** — sparse `Grid` façade, parity at current 192³.
3. `[x]` **P2** — chunk entities spawn/despawn with geometry, not
   pre-spawned for the whole domain (8/216 chunks live for the
   starter sphere — a real entity-count win already).
4. `[x]` **P3** — sparse-native wire cutter, component labelling
   (scan *and* storage), rigid-translate/rest-on-bench, and the
   Move-tool gizmo-drag preview (growable region snapshot) all off
   the full-domain path. Also fixed a second full-domain scan found
   along the way in the app's delete-selection path.
5. `[x]` **P4** — domain grown to 352³ (528 mm, ≥ the 400 mm View
   grid). Bench-first Add needed no code changes (already fully
   parameterised on `grid.res()`); found and fixed a related bug
   while verifying — `ray_march`'s iteration cap was a fixed
   constant sized for the old domain, silently truncating reach
   below the `max_dist` callers already ask for.
6. `[x]` **P5** — `.mudclay` v2 sparse tiles (v1 read OK). Shipped
   together with P9: writer emits v3; reader accepts v1/v2/v3.
7. `[x]` **P6** — `LayersState { layers, active }` replaces the bare
   `SculptWorkpiece` grid, single layer, behaviour unchanged.
8. `[x]` **P7** — Insert Primitive → new layer; cross-layer pick
   (`ray_march_visible`) activates whatever the user clicks on;
   STL still flattens visible layers (`Grid::union_from`).
9. `[x]` **P8** — Layers panel (name / visibility / delete) + Merge
   Down. Status strip shows `Layer N/M · name`. Delete and Merge
   Down are undoable; File → New resets to one empty layer.
10. `[x]` **P9** — `.mudclay` v3 multi-layer sections. Save keeps
    layer boundaries; load restores the stack. STL export remains
    visible-union.

---

## Design-doc obligations still owed

From `DESIGN.md`'s staged roadmap:

- `[~]` **Sparse narrow-band SDF store** — now Track A in
  `SPARSE_THEN_LAYERS.md` (still dense 192³ until P1). Effective
  resolution / domain ceiling for detail work.
- `[ ]` **Dual contouring** for sharp features. Marching cubes /
  surface nets today round every corner.
- `[~]` **Merge on contact / weld.** Explicit user action — Track B
  Merge Down (`min` union). Not automatic on contact.
- `[ ]` **Matcap library + cavity shading.** One matcap right now;
  cavity term (`smoothed(φ) − φ`) is cheap.
- `[~]` **Multi-piece scene** — Track B Layers (same-domain, hide /
  show). Per-piece transforms still deferred.
- `[ ]` **Reference images pinned to the workbench** (Stage 4).
- `[ ]` **Autosave / crash recovery** (Stage 4).
- `[ ]` **Watertightness check on export.**
- `[ ]` **Camera bookmarks.**
- `[ ]` **Recent files.**
- `[ ]` **Snap-to-workbench** for a piece's base.
- `[ ]` **Advanced-mode tool parameters**: scraper blade profile,
  loop shape, cookie-cutter corner-radius / wave modulation.
- `[ ]` **Pen tablet support** (Stage 3 target: pressure = depth,
  tilt = orientation for directional tools).

---

## Deferred (Stage 5+ / research)

- `(defer)` Colour / decoration mode.
- `(defer)` `.mudprofile` asset format + shareable profile library.
- `(defer)` Scripted / procedural cutters (advanced tier of the
  two-tier philosophy).
- `(defer)` Collaborative sculpting.
- `(defer)` VR / haptics.
- `(defer)` iPad / tablet ports.
- `(defer)` Print-service integration.

---

## Notes on scope discipline

- Every new tool should score against `DESIGN.md` §5 (per-tool
  displacement vs. removal, workshop vocabulary, no hidden modes).
- Keep the novelty budget on the sculpting viewport. Menus, dialogs,
  and shortcuts stay boring and conventional.
- The staged roadmap in `DESIGN.md` §4 has a **falsifiable question**
  per stage. Answer it before adding more features in that stage.
