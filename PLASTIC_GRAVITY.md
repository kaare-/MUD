# Gravity settle — spike design

Status: **drop → throttle-up sag → tip → volumetric splat**.

## Goal

Tunable gravity that reads as clay:

1. Floating lumps **crash onto the workbench**.
2. Forms **sag first** — even at high softness — with gravity
   easing in over several passes (not full tip/splat onset).
3. Long thin branches **arch-sag** toward the bench at low
   softness (0.01–0.1), forming an arch from tip to rooted base.
4. Tall stalks that survive sag may **tip** (angle eases in above
   softness ≈ 0.55).
5. Soft clay **splats** into a thick mound (softness ≥ 0.5), blended
   toward the target height — not an instant pancake.

Critical: light settle must **not** binary-rewrite the SDF when nothing
cantilevered moved (that was the surface “erosion” look).

## Falsifiers

1. Airborne blob lands at `iy = 0` at softness `0`.
2. Long horizontal branch at softness `≈ 0.05`: tip lowers, stays one
   component (arch), root on bench.
3. Grounded blob with no cantilevers at softness `0.01`: surface
   unchanged vs a pure drop.
4. Soft tower tips/splats; soft sphere becomes a thick splat.
5. Stiff fat blob barely moves.

## Algorithm

### 1 — Drop
Rigid rest (`Ctrl+G` backbone).

### 2 — Sag first (any softness > 0)
Progressive arch/stalk sag. Pass strengths are **increments** that sum
to softness (ease-in curve), so early passes bow gently and later
passes add the rest — never re-apply a full drop each pass.

Per component, BFS from bench-touching solids. Sag voxels that are
**horizontally far** from the bench footprint (true cantilevers /
branches), or tall upright stalks without an overhang. Reseal 1-voxel
gaps so arches don’t shatter. **No column packing**.

### 3 — Tip (softness ≥ 0.55)
Only if still needle-tall after sag. Rotation angle eases with
`(softness - 0.55) / 0.45` (full 90° only at softness 1).

### 4 — Splat (softness ≥ 0.5)
Bench-pack → blend current peak toward a volume-based mound height →
spread + equalise → despike → SDF rewrite.

### Write gate
Solid→SDF rewrite runs **only** if tip, sag, or splat changed geometry.

## UX

| | |
|---|---|
| Menu | `Sculpt → Settle (gravity)…` |
| Hotkey | `Ctrl+Shift+G` |
| Softness | `0` drop only · low/mid = progressive sag · high = tip + splat |

## Implementation

`crates/sculpt-core/src/plastic.rs`
