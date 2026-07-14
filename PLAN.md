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

1. `[ ]` **Export STL via dialog.**
   - Reuse the Save-As dialog pattern from `project.rs`.
   - New action `ShowExportStlDialog` + `ExportStlTo(path)`.
   - Suggest `mud-sculpt-<timestamp>.stl` in CWD; validate `.stl`
     extension.

2. `[ ]` **Clear worktable.**
   - `File → New` (and `Ctrl+N`) emits `AppAction::NewWorkpiece`.
   - Resets grid to empty (all outside), clears undo history and any
     live stroke via `SculptStroke::discard_live()`.
   - Confirm prompt only if grid is non-trivial (skip when already
     empty).

3. `[ ]` **Add material on an empty worktable.**
   - Currently Clay Add requires a ray-march hit.
   - When the ray misses, project the cursor onto a horizontal plane
     at `workbench_y + tool.size` (or the ray's closest point to the
     bench) and stamp a small sphere there, clipped to `y ≥ 0`.
   - Face-on paint-plane lock still applies to the first stamp.

4. `[ ]` **Primitives menu.**
   - `File → Insert Primitive` (also `Shift+N`): sphere / box /
     cylinder / cone / torus, plus a `Size (mm)` field.
   - Adds to the current grid via SDF union (not a hard grid reset).
   - Pairs naturally with (2) and (3) — start empty, drop a shape,
     sculpt it.

5. `[ ]` **Selection tool: pick tiny bits.** *(spike first)*
   - Two viable designs; pick before coding:
     - **A. Component labels.** Flood-fill connected `φ < 0` regions
       into an ID field. Selection = "the component under the
       cursor". Active-only edits gate stamps by `id_at(voxel) ==
       selected`. Delete = mark those voxels `+∞`.
     - **B. Multi-piece.** Separate SDF grids in the scene. Cutters /
       wire produce new pieces. Selection is a scene-graph concept.
     - Option A is smaller and lands sooner; Option B is where
       DESIGN.md §5.2 already points ("touching ≠ merged").
   - Deliverables regardless: `Ctrl+A` toggle "active-only",
     `Delete` removes the selected component, HUD shows current
     selection.

6. `[ ]` **Rigid gravity (setting 1).** *(depends on 5)*
   - After a cut, drop each connected component as a rigid body
     onto the bench (y = 0). Clay is infinitely stiff — only the
     transform changes.
   - Simple resting: translate each component so its lowest point
     touches y = 0; optionally rotate to its convex-hull minimum
     face. No inter-component collision at first.
   - `View → Gravity: Off / Rest`.

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
