# Gravity settle — spike design

Status: **drop → tip → bow → volumetric splat**.

## Goal

Tunable gravity on the active layer that reads as clay, not erosion:

1. Floating lumps **crash onto the workbench**.
2. Tall unstable pieces **tip over** (90° lay-down) when soft enough.
3. Remaining slender stalks **bow** (quadratic shear).
4. Soft clay **splats** into a thick mound (never a 1-voxel sheet).

Softness `plasticity ∈ [0, 1]`: drop-only → tip/bow → thick splat.

## Falsifiers

1. Airborne blob lands at `iy = 0` even at softness `0`.
2. Soft tower tips and/or splats — loses height, footprint stays wide,
   max column height ≥ 4 (thick mound).
3. Mid-soft slender stalk tips or bows (does not stay a rigid needle).
4. Soft sphere becomes a shorter thick splat.
5. Stiff fat blob (`≈ 0.12`) barely moves.
6. Volume stays within ~25% (despike may trim needles).

## Non-goals

- FEM / MPM / PBD / continuous sim
- Perfect rest-pose onto an arbitrary face (tip is a 90° lay-down)
- Multi-layer settle in one op

## Algorithm

### 1 — Drop
Rigid rest (`Ctrl+G` backbone).

### 2 — Tip
If height/footprint is large and softness ≥ 0.2: rotate 90° about Z
through the centroid so height folds into X, then re-drop to `iy = 0`.

### 3 — Bow
Still-slender upright leftovers: lateral `shift = amp · (y/H)²`.

### 4 — Splat (softness ≥ 0.25)
1. Bench-pack every column.
2. Target mound height from `cbrt(volume)` × softness (min 4 voxels).
3. Spread overflow into neighbours / rings.
4. Laplacian equalise the height field → dome, not spikes.
5. Despike (drop 0–1 neighbour needles).
6. Rewrite SDF from the solid mask with a clean distance band.

## UX

| | |
|---|---|
| Menu | `Sculpt → Settle (gravity)…` |
| Hotkey | `Ctrl+Shift+G` |
| Control | Softness slider |
| Scope | Active layer · one undo stroke |

## Implementation

- `crates/sculpt-core/src/plastic.rs`
- `crates/sculpt-app/src/gravity.rs` / `ui.rs`
