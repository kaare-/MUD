# Plastic gravity — spike design

Status: **spike in progress** (Track A/B foundations landed).
Not FEM. Not continuous simulation. One-shot settle on the active layer.

## Goal

Give clay a controllable “gradual setting” under its own weight:
tall thin forms slump and puddle outward; fat resting blobs barely
move. Slider `plasticity ∈ [0, 1]` = elastic (noop) → soft yielding.

Falsifiers for the spike:

1. A thin tower on the bench loses height and gains base width at
   `plasticity = 1`.
2. The same tower is nearly unchanged at `plasticity = 0`.
3. A short fat hemisphere barely moves at `plasticity = 1`.
4. Volume of solid voxels stays within ~15% of the pre-settle count
   (approximate conservation — good enough for a spike).
5. Narrow-band meshing does not crumple (band rewritten with the
   solid, same discipline as rigid rest).

## Non-goals (spike)

- FEM / MPM / PBD / elastic rebound
- Continuous always-on simulation
- Rest-pose rotation / tipping
- Multi-layer settle in one op (active layer only, same as tools)
- Exact volume-preserving Poisson redistribution
- Full SDF redistancing

## Stress proxy (spike)

Column overburden, not FEM curvature:

- For each `(ix, iz)` column, `height` = count of solid (`φ < 0`) voxels.
- Stable height `max_stable = 4 + (1 − plasticity) × 36` voxels.
- A column **yields** when `height > max_stable`.

This is a discrete stand-in for “vertical load × unsupported height”.
A later pass can swap in a mean-curvature × load surface proxy
without changing the UX.

## Yield response (one iteration)

For each yielding column:

1. **Peel** up to `1 + 3×plasticity` solid voxels from the top
   (`φ → +voxel_size`).
2. **Plant** the same count into the shortest neighbouring columns
   at the base (`φ → −voxel_size`), flaring the footprint.
3. Paint a tiny positive halo around new plants so the band stays
   meshable; lightly mollify **positive** band cells only (never
   blur solid — that dissolves volume).

Repeat `iterations` times (default 8). Journal every changed voxel
once (first pre-value) → **one undo stroke** for the whole settle.

## UX

| | |
|---|---|
| Menu | `Sculpt → Settle (plastic)…` |
| Hotkey | `Ctrl+Shift+G` (rigid Rest stays `Ctrl+G`) |
| Control | Plasticity slider on a small dialog (default 0.7) |
| Scope | All components on the **active** layer |
| Mode | One-shot burst → single undo |

## Relationship to rigid Rest

`Ctrl+G` remains a rigid −Y translate (no deformation). Plastic
settle is a separate op: local SDF surgery. Users can Rest then
Settle, or Settle floaters (material still flows down onto the
bench clamp).

## Implementation map

- `crates/sculpt-core/src/plastic.rs` — core settle + tests
- `crates/sculpt-app/src/gravity.rs` — hotkey / action / undo
- `actions` / `ui` — menu + plasticity dialog
