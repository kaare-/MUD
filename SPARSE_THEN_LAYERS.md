# Sparse SDF, then Layers — Plan

*Decisions locked 2026-07-15. Companion to `DESIGN.md` (philosophy)
and `PLAN.md` (backlog). This doc is the execution plan for the next
architecture work — not a feature dump.*

---

## Verdict

**Do sparse narrow-band SDF first. Do layers second.**

The user goal that unblocks "sausage turning on the worktable" is a
**large shared domain** with cheap empty air — not yet multi-body
identity. Dense 192³ @ 1.5 mm is already a 288 mm cube and ~27 MB per
live field (plus snapshot + labels under Move / Select). Growing that
dense domain to match the visible 400 mm bench grid (~267³) blows past
~70 MB per field; spanning a true worktable for coil/sausage work is
worse. Sparse storage is the lever. Layers then become cheap copies of
*tile sets*, not another 27 MB each.

Layers (same domain / same resolution / shared piece-local frame) are
still the right multi-body model for v1 — see §3. They land after the
`Grid` façade no longer assumes a dense `Vec<f32>`.

---

## Locked product decisions

| # | Decision | Notes |
|---|---|---|
| D1 | **Sparse before layers** | Unlocks whole worktable + empty-bench Add as a primary interaction. |
| D2 | **Layers = same-domain grids**, not per-body transforms | Option A (grid-per-body with own Transform) deferred. |
| D3 | **Insert Primitive → new layer by default** | Fusion only via explicit Merge. Fixes silent union. |
| D4 | **Tools affect active layer only (v1)** | Cutters / wire-cut / smooth / paddle / Add / Remove / Move / Rest / Delete. Multi-active later. |
| D5 | **Visibility toggle per layer** | Ships with the Layers UI phase. |
| D6 | **Merge = min-union into destination, drop source** | Explicit weld; Photoshop "Merge Down" mental model. |
| D7 | **Custom 32³ tile store**, not OpenVDB | Matches existing `CHUNK_SIZE`, Bevy remesh, no FFI. Sparse VDB remains a future option once the façade is stable. |

### Explicitly *not* in this plan

- Per-body independent rotation / placement outside the shared domain
  (real Option A). Flag if coil work later needs pieces parked far off
  the table — that is a different migration.
- Plastic gravity (still its own spike).
- Dual contouring (orthogonal; can follow either track).
- Multi-active layers (future toggle on top of D4).

---

## Why layers-on-dense would be the wrong order

Today: one `Grid` in `SculptWorkpiece`, one `WorkpieceRoot`, brushes /
ray-march / chunk mesh / Move / undo all talk to that grid.

Layers-on-dense would give Insert-doesn't-fuse and a Layers panel, but:

1. Domain stays 288 mm — sausage turning still hits the wall.
2. N dense layers = N × 27 MB (+snapshots). Ten layers ≈ 270 MB before
   sparse. Fine briefly; hostile once the domain grows.
3. The sparse migration then has to touch every layer consumer *and*
   the new LayersState — two migrations instead of one.

Sparse-first keeps a single-layer exterior API during the hard rewrite,
then Layers is mostly "Vec of the new Grid + active index".

---

## Track A — Sparse narrow-band SDF

**Falsifiable question (DESIGN Stage 2):** Can we sculpt across a
worktable-scale domain (≥ visual bench, target **≥ 512 mm** XZ,
Y tall enough for standing forms) at interactive rates on a laptop,
with idle memory dominated by *surface tiles* not air?

**Target numbers (initial):**

| Param | Dense today | Sparse target |
|---|---|---|
| Voxel | 1.5 mm | keep 1.5 mm (unchanged feel) |
| Domain XZ | 288 mm | **512 mm** (matches / exceeds 400 mm View grid) |
| Domain Y | 288 mm | **256–384 mm** (bench at y=0; headroom for standing coil) |
| Tile | n/a (virtual 32³ chunks exist) | **32³ allocated tiles** only near surface / edits |
| Narrow band | implicit full field | keep / prune to ~±3–4 voxels (matches `gravity::BAND`) |
| Empty sample | `f32::MAX/4` or OOB far+ | unallocated tile → far+ |

### A0 — Façade + characterisation *(spike, short)*

Pin the contract so tools barely change:

- Keep `sculpt_core::Grid` as the public type.
- Guarantees tools rely on today: `get` / `set` / `sample` / `gradient`
  / `ray_march` / `DirtyRegion` / `samples()`-or-replacement /
  `restore_samples`-or-replacement / `res` / `origin` / `voxel_size`.
- Audit callers that assume dense layout (`samples().to_vec()`,
  full-res loops). Worst offenders already known:
  - `cutter::apply_wire_cutter` — full-grid pass
  - `components::label_components` — O(N) volume flood
  - `gravity::{translate,rest}` — full `samples()` copy
  - `export::extract_full_mesh` — dense pad buffer
  - `project::{read,write}_project` — v1 dense blob
  - `workpiece::swap_grid` — fixed res + always-spawned chunks

Deliverable: short note in this file or a PR description listing the
replacement iterators (`for_each_allocated_tile`, `for_each_band_voxel`,
`snapshot_region`).

### A1 — Block-sparse storage behind `Grid`

- Internal: `HashMap<TileCoord, Box<[f32; 32³]>>` (or arena of tiles)
  + `res`, `voxel_size`, `origin`, optional band-width.
- `get` / `set`: allocate on write; missing tile reads as far+.
- `sample` / `gradient` / `ray_march`: unchanged public behaviour;
  miss = outside.
- Drop requirement that `samples()` returns a full dense `Vec` —
  replace with:
  - `write_dense_for_tests` / loader path, or
  - region snapshot API used by Move preview and undo.
- Unit tests: sphere fill, brush AABB, missing-tile sample, dirty
  chunk set equals allocated neighbourhood.

**Acceptance:** existing sculpt tools keep compiling against `Grid`;
single-layer app still runs on a domain the size of today's 192³
(behavioural parity) before we grow the domain.

### A2 — Remesh only allocated / dirty tiles

- `SculptWorkpiece` stops pre-spawning all 6×6×6 chunk entities.
- Spawn / despawn Bevy chunk entities when tiles appear / empty out.
- Cap `MAX_CHUNKS_PER_FRAME` stays.
- STL export concatenates chunk extracts (or temporary dense only for
  the tight AABB of allocated tiles) — no full-domain pad when mostly air.

### A3 — Rewrite volume walkers

Priority order (most blocking → least):

1. **Wire cutter** — iterate tiles intersecting the slab AABB + band,
   not `0..res`.
2. **Component labelling** — flood only interior / band voxels;
   storage becomes sparse label map or tile-parallel labels.
3. **Translate / Rest / Move preview** — region or tile snapshots;
   ban full-grid `samples().to_vec()`.
4. **Primitives / brushes** — already AABB-local; mostly free.

### A4 — Grow the domain + bench-first clay

Once A1–A3 land and feel good:

- Raise logical `res` so XZ ≥ 512 mm at 1.5 mm/voxel (~341³ class, but
  only surface tiles allocated).
- Align visual workbench grid with the real sculpt domain (or make
  domain ≥ View grid).
- **Empty-bench Add is a first-class path**, not an edge case:
  Shift+LMB (or plain LMB with Clay when ray misses) deposits on y=0
  anywhere in-domain. Goal: start a sausage/coil without a starter
  sphere if the user clears the table.
- Optional: File → New starts *empty* (no default ball) — product call
  at A4; default ball can remain for first-run comfort.

### A5 — `.mudclay` v2 (sparse tiles)

```text
v2 sketch:
  magic, version=2, flags
  res, voxel_size, origin
  tile_size (=32), tile_count
  repeated: tile_ix, tile_iy, tile_iz, 32³ f32 samples
```

- Reader accepts **v1** (dense) → ingest into sparse Grid (allocate
  only tiles that aren't uniformly far+).
- Writer emits **v2**.
- Keep version mismatch errors strict.

---

## Track B — Layers (same-domain multi-body)

Starts only after Track A leaves `Grid` as a façade over tiles.
Memory per idle empty layer ≈ 0; per occupied layer ≈ its tile set.

### Model

```text
Layer {
  id, name,
  grid: Grid,          // sparse, same res/origin/voxel as siblings
  visible: bool,
  // chunks + dirty live here or in a parallel app struct
}

LayersState {
  layers: Vec<Layer>,
  active: usize,       // tools read/write this one
}
```

Shared: one `WorkpieceRoot` / turntable (all layers spin together).
No per-layer transform in v1 (D2).

### Locked behaviours (from user)

1. **Insert Primitive → always new layer**, auto-activate, switch to
   Move (Move edits the active layer's content in piece-local space —
   today via voxel translate; later could stay that way).
2. **Tools = active layer only** (D4). Later: optional multi-active.
3. **Visibility toggle** per layer (D5).
4. **Merge Down / Merge to active** = `dst[i] = min(dst, src)` over
   tiles (D6), drop source, one undo entry.

### B1 — Single-layer refactor onto `LayersState`

- `SculptWorkpiece` becomes thin wrapper or is replaced by
  `LayersState` with `len == 1`.
- All tools/selectors take `layers.active_mut().grid`.
- Undo entries tagged with `layer_id` (stable id, not index).
- Behaviour identical to today (one layer). No UI yet.

### B2 — Multi-layer create / pick / hide

- Insert Primitive → push layer.
- Selection pick: ray-march **visible** layers, closest hit wins;
  activates that layer (and optionally selects component within).
- Delete layer / delete component within active layer — clarify UX:
  Select+Delete removes component; layer panel Delete removes whole
  layer if empty-or-confirmed.
- Visibility toggles skip hidden layers in ray-march + remesh.

### B3 — Layers UI + Merge

- Small panel: name, active indicator, visibility, delete, Merge Down.
- Merge journals both grids appropriately for undo.
- Status strip shows `Layer N/M · name`.

### B4 — `.mudclay` v3 (layers)

- Repeated layer sections on top of v2 tile encoding.
- v1/v2 load as a **single-layer** scene (back-compat).
- STL: export active only, or visible-union (temp min across visible
  layers into one mesh) — pick default in B4 (lean: **visible union**
  for "what I see is what I print").

---

## Secondary questions (answered for v1)

| Question | Answer |
|---|---|
| Turntable rotates scene or active body? | **Scene** — one `WorkpieceRoot`; all visible layers turn together. |
| Symmetry with multiple layers? | Symmetry plane is **piece-local / shared**; applies only to stamps on the **active** layer (same as tools). |
| Rest on bench? | Active layer only in B1–B2; optional "rest all visible" later. |
| Cut splits into new layer? | **Not auto in v1.** Disconnects stay components inside the active layer (today's Select/Delete). Optional later: "Promote component to layer". |
| Soft-join across layers? | Never automatic. Only Merge. |
| Empty New? | Prefer empty table once A4 lands; starter sphere becomes onboarding optional. |

---

## PR sequence (landable stack)

Prefer stacking on the current Stage-3 tip
(`cursor/stage-3-larger-grid-wirecut-2-6a1e`).

| PR | Track | Scope | Reviewability |
|---|---|---|---|
| **P0** | plan | This doc + `PLAN.md` reorder | docs only — shipped |
| **P1** | A0–A1 | Sparse `Grid` façade, parity tests, still 192³ domain | core-heavy, app thin — shipped |
| **P2** | A2 | Chunk spawn/despawn + remesh allocated only | app remesh |
| **P3** | A3 | Wire cutter + labels + translate/rest sparse walks | core walkers |
| **P4** | A4 | Domain grow + bench-first Add (+ optional empty New) | product-visible |
| **P5** | A5 | `.mudclay` v2 sparse write + v1 read | IO |
| **P6** | B1 | `LayersState`, single layer, undo tags | refactor |
| **P7** | B2 | Insert→new layer, pick across layers, visibility | behaviour |
| **P8** | B3 | Layers panel + Merge | UI |
| **P9** | B4 | `.mudclay` v3 + STL visible-union | IO |

Do **not** combine A4 domain-grow with B-layers in one PR — each has
its own falsifiable check.

---

## Risks / watchouts

1. **Redistancing.** Soft CSG warps the field; today's code already
   lives with a non-perfect SDF. Sparse must not make band holes that
   break `ray_march`. Mitigation: keep a conservative band on write;
   periodic reseed from interior if needed (later).
2. **Wire-cutter quality.** Soft-max ring currently assumes a full
   domain pass — must preserve C¹ ring behaviour on sparse tiles.
3. **Move live preview.** Today snapshots the whole dense grid.
   Must become tile or AABB snapshots or latency returns.
4. **Save size.** v2 should stay simple (raw tiles). Compression can
   wait.
5. **Don't invent OpenVDB mid-flight.** If custom tiles struggle,
   revisit FFI as a *replacement backend* behind the same `Grid` API.

---

## P1 — shipped (2026-07-15)

`sculpt-core`'s `Grid` is now a `HashMap<ChunkCoord, Box<[f32; 32³]>>`
tile store instead of a dense `Vec<f32>`. Behaviour is unchanged:

- `get` / `set` / `sample` / `gradient` / `gradient_at` / `ray_march`
  / `res` / `voxel_size` / `origin` / `position` / `extent` /
  `num_chunks` / `from_sphere` / `empty` — identical signatures and
  results.
- `get_unchecked` keeps its `unsafe fn` signature for API
  compatibility but has no unchecked fast path over a `HashMap`; it
  now just calls `get`.
- **Contract change:** `samples() -> &[f32]` is gone. A sparse store
  has no single contiguous buffer to borrow. It's replaced by
  `to_dense() -> Vec<f32>`, which materialises the full buffer on
  demand. Updated call sites: the `.mudclay` writer
  (`sculpt_core::project::write_project`), `gravity::translate_components`
  (its full-grid snapshot), and the app's Move-tool drag-preview
  snapshot (`move_tool.rs`). These are exactly the operations Track
  A3 will make tile-native; until then they pay a `to_dense()` cost
  they didn't pay before, which is an accepted, documented trade-off
  for this phase — not a behavioural change.
- New tests in `grid.rs`: zero tiles for an empty grid, one write
  allocates exactly one tile, tile boundaries clip correctly at
  non-multiple-of-32 resolutions, `to_dense` / `from_samples`
  round-trip exactly, loading mostly-far-positive data stays sparse,
  and `restore_samples` re-sparsifies a tile that was written then
  reverted (the Move-preview reset case).
- Verified: `cargo test --workspace` (83 tests) and `cargo clippy
  --workspace --all-targets` both clean. Manually smoke-tested the
  running app (Insert Primitive union, Select highlight, Rest on
  bench, `Ctrl+N` clear, `Ctrl+S` save) — saved file size matched
  `48 + 4×192³` bytes exactly, confirming the sparse-to-dense project
  writer round-trips correctly against the live grid.
- Domain is still 192³ — no memory win yet (`from_sphere` fills the
  whole domain with distinct values, so every tile allocates). The
  payoff starts once real "far from any surface" regions exist,
  which is Track A4 (domain grow).

## P2 — shipped (2026-07-15)

`SculptWorkpiece` no longer pre-spawns all 216 (6×6×6) chunk entities
at startup. `remesh_dirty_chunks` now reconciles each dirty chunk's
extracted mesh against its entity: spawn one where geometry newly
appears, despawn it where geometry disappears, update it in place
otherwise. `swap_grid` (project load / New) marks *every* chunk coord
dirty, not just the previously-spawned ones, so both directions
(chunks that go empty, chunks that gain geometry for the first time)
get picked up.

This is a real win **today**, not just future-proofing: a chunk only
gets an entity where the SDF's zero-crossing actually passes through
it, not wherever the grid has data. Verified live (temporary `info!`
instrumentation, removed after use): the starter sphere spawns
**8 of 216** possible chunks — the surface shell, not the solid
interior or the empty exterior. Full lifecycle confirmed end-to-end
in the running app: startup → 8 spawned; `Ctrl+N` → exactly those 8
despawn to 0; `Ctrl+Shift+O` reload → the same 8 coords respawn with
matching geometry (screenshot-verified).

## P3 — mostly shipped (2026-07-15)

Sparsified the three volume walkers that still iterated the full
domain, using two new `Grid` primitives:

- `Grid::allocated_chunk_coords()` — cheap key-set copy, no sample
  data touched.
- `Grid::snapshot_region(min, max) -> RegionSnapshot` — a dense
  snapshot of a sub-box, indexed by the same global voxel coords as
  the grid it came from; reads outside the box return the same
  far-positive sentinel an unallocated tile does.

**Wire cutter** (`cutter::apply_wire_cutter_with_callback`): now
iterates allocated tiles only, not `0..res` on every axis. This is
exact, not approximate — a CSG subtract can only push the field more
positive, never less, so a voxel starting at the far-positive
sentinel is provably unaffected (`soft_max(FAR_POSITIVE, -d_slab, k)`
degenerates to `FAR_POSITIVE` because `corner_k` is a couple of
voxels and the sentinel is astronomically larger than any real
`d_slab`). Verified: cutting a sparse brush-built sphere splits it
correctly and allocates zero new tiles; the existing deep-reshape
regression test (cells several voxels into the remaining piece
getting properly lifted toward the cut plane) still passes unchanged.

**Component labelling** (`components::label_components`): the
seed-scan that used to raster the whole domain now only visits
allocated tiles — a voxel in an unallocated tile is guaranteed empty
by the sparse `Grid` contract, so it can never seed or extend a
component. The flood-fill itself is untouched (`grid.get` already
returns the right answer for neighbours in unallocated tiles, so a
flood correctly stops at a tile boundary with no special-casing).
**Not yet sparsified:** the label array itself (`ids: Vec<ComponentId>`)
is still one dense `res³` buffer. That's fine at the current domain,
but it will need to become sparse too before Track A4 grows the
domain much further — worth doing together with A4, not before it,
since it doesn't matter until the domain actually grows.

**Rest-on-bench / rigid translate** (`gravity::translate_components`,
backs both `rest_components_on_bench` and the Move-tool's typed-apply
path): replaced the full-domain `to_dense()` snapshot with
`snapshot_region(union_min, union_max)` — the same bounding box the
function already computed as everything it could possibly touch or
read from. Verified live in the running app: `Ctrl+G` visibly drops
the (slightly floating) starter sphere onto the bench, `Ctrl+Z`
correctly restores the floating position, and a new regression test
confirms translating one component leaves a distant, uninvolved one
completely untouched.

**Deliberately deferred:** the Move tool's **gizmo-drag** live
preview (`move_tool.rs`) still snapshots/restores the *whole* grid
every frame via `to_dense()` / `restore_samples()`. Bounding that
snapshot safely needs tracking the union of every region the drag
could reach over its *whole* lifetime (drag distance varies frame to
frame, unlike rest-on-bench's one-shot bounded delta) — a bit more
design than the other three, and the existing full-grid path is still
correct, just not sparsified. Left as explicit follow-up rather than
risk a subtly-wrong bounded region under time pressure.

## P3 — completed (2026-07-15)

Closed out the two items deferred above.

**Move-tool gizmo-drag preview** (`move_tool.rs`): replaced the
per-frame full-grid `to_dense()` / `restore_samples()` with a
*growable* region snapshot. New `sculpt_core::touched_region_for_translate`
computes the exact box a single-component translate by a given delta
could read from or write to (the component's own widened bounds,
unioned with that box shifted by the delta) — deliberately narrower
than what `translate_components` snapshots internally for its
multi-component bookkeeping, since a stationary component's SDF is
never actually mutated by a translate and so never needs guarding.
`ActiveDrag` now holds a `RegionSnapshot` sized to that box at delta
zero; `ensure_region_covers` grows it on demand as the drag reaches
farther — restoring the *current* (smaller) snapshot first (safe,
since nothing outside it has ever been written to this drag), then
taking a fresh snapshot over the union of old and newly-required
bounds straight from the now-pristine live grid. Every frame that
doesn't exceed the drag's high-water mark (the common case) pays a
bounds check and nothing else; growing is O(region), never O(domain).
New `Grid::restore_region` is the region-scoped inverse of
`snapshot_region`.

**`ComponentField` label storage**: switched from one dense
`Vec<ComponentId>` sized `res³` to the same
`HashMap<ChunkCoord, tile>` shape `Grid` uses — a component can only
ever occupy voxels inside an allocated `Grid` tile, so the label
field only allocates where labelling actually wrote a non-`EMPTY` id.
This removed the `ids() -> &[ComponentId]` escape hatch entirely (no
consumer needs a dense slice once `id_at` is the only way in), which
also caught and fixed a second full-domain raster scan that had been
hiding in the app layer: `selection.rs`'s `delete_selected_component`
used to walk `0..res` on every axis to find the selected component's
voxels; it now walks `bounds_of(target)` directly.

**Verified:** live in the running app — `Ctrl+G` still drops the
sphere onto the bench correctly (exercises `label_components` +
`bounds_of` + `id_at` through the sparsified path end-to-end); full
test suite (93 tests) green, including a new regression confirming a
5 mm sphere's label field allocates only a handful of tiles on a
192³ grid, nowhere near a dense `res³` buffer.

Track A3 is now done. The `.mudclay` writer is the one place left
that still deliberately materialises a full dense buffer — Track A5
(the v2 on-disk sparse format) is the right place to revisit that,
not before.

## Next step

**Track A4** (grow the domain past 192³ + bench-first Add) is now
unblocked. Alternatively, start **Track B** (layers) — it never
depended on A3 or A4.
