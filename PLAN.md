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

- `[x]` Dense SDF grid at 128³, 1.5 mm voxel, chunked meshing.
- `[x]` Orbit / pan / zoom camera, Q / E turntable.
- `[x]` Workbench floor (y = 0) as a hard clip.
- `[x]` Clay tool (Add / Remove) with soft CSG (magic clay).
- `[x]` Cookie cutters (circle, square, hex, star).
- `[x]` Wire cutter (LMB drag → planar slab cut).
- `[x]` Smooth brush, paddle.
- `[x]` Mirror symmetry (piece-local X = 0).
- `[x]` Undo / redo (stroke-granular, `Shift+Ctrl+Z` per-op reserved).
- `[x]` `.mudclay` save / load, Save-As and Open dialogs (in-app).
- `[x]` STL export to CWD (`Ctrl+E`).
- `[x]` Egui menus + tool palette + status strip.
- `[x]` Hover ghost preview matching the stamp centre.
- `[x]` Ray march robust against warped SDFs (front-face lock).
- `[x]` Add/Remove UX: soft join, tangent-plane lock, side-column
  rings, no camera stalks, no top-down 45° climb.
- `[x]` Turntable slowed; sculpt syncs to the live piece transform.
- `[x]` Stroke lifecycle survives release-over-UI and load / tool
  switch.

---

## Immediate backlog (user pass, 2026-07-14)

Ordered roughly easiest → hardest. Each item includes what it needs.

1. `[x]` **Export STL via dialog.** — `Ctrl+Shift+E` opens Save As-style
   dialog, `Ctrl+E` still instant-exports to CWD.

2. `[x]` **Clear worktable.** — `Ctrl+N` / `File → New`. Swaps in an
   empty grid, clears undo + live stroke.

3. `[x]` **Add material on an empty worktable.** — Shift+LMB on the
   empty bench projects the ray onto y = 0 and deposits a blob
   sitting on the bench. Every other tool still bails on a miss.

4. `[x]` **Primitives menu.** — `Shift+N` / `File → Insert Primitive…`.
   Sphere / Cube / Cylinder / Torus with a size slider; unioned into
   the current grid, journaled as one undo stroke.

5. `[x]` **Selection tool: pick tiny bits.** Option A (single-grid
   component labels) shipped. New `Select` tool (`9`); LMB picks the
   connected piece under the cursor. `Delete` / `Backspace` removes
   the selected piece as a single undo stroke. `A` toggles
   **active-only** — sculpt stamps skip when the hit doesn't fall on
   the selected piece. HUD shows `Sel #N: X vx · Y mm³` + Delete
   button + Active-only toggle. Labels are recomputed lazily
   (invalidated after any stroke / undo / redo / Insert / New /
   Load). Option B (multi-piece scene) still deferred.

6. `[x]` **Rigid gravity (rest).** `File → Rest pieces on bench`
   (`Ctrl+G`). Every floating connected component is translated
   along −Y so its lowest voxel lands at `iy = 0`. No plastic
   deformation, no inter-component collision (overlapping landing
   zones union naturally through min-SDF). Journaled as one undo
   stroke via `sculpt_core::rest_components_on_bench`. Rotation to
   a minimum-face rest pose is *not* implemented — components keep
   their orientation. Continuous "gravity on" mode is deferred: it
   would fight active sculpting.

7. `[ ]` **Plastic gravity (gradual setting).** *(spike required)*
   - Slider 0 → 1 = elastic → yielding clay.
   - Approximation: iterative descent of the SDF surface under a
     stress proxy (e.g. curvature × vertical load), with a plastic
     yield threshold. Not FEM.
   - Almost certainly its own design document before writing code.

---

## Design-doc obligations still owed

From `DESIGN.md`'s staged roadmap:

- `[ ]` **Sparse narrow-band SDF store** (Stage 2 → not migrated yet;
  still on dense 128³). Effective resolution ceiling for detail work.
- `[ ]` **Dual contouring** for sharp features. Marching cubes today
  rounds every corner.
- `[ ]` **Merge on contact / weld.** Explicit user action; needs
  component identity from (5).
- `[ ]` **Matcap library + cavity shading.** One matcap right now;
  cavity term (`smoothed(φ) − φ`) is cheap.
- `[ ]` **Multi-piece scene** with hide / show, per-piece transforms.
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
