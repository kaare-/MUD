# Gravity settle — spike design

Status: **drop → tip → arch-sag → volumetric splat**.

## Goal

Tunable gravity that reads as clay:

1. Floating lumps **crash onto the workbench**.
2. Tall unstable pieces **tip** (90° lay-down) when soft enough (≥ 0.35).
3. Long thin branches **arch-sag** toward the bench — even at low
   softness (0.01–0.1) — forming an arch from tip to rooted base.
4. Soft clay **splats** into a thick mound (not a 1-voxel sheet).

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

### 2 — Tip (softness ≥ 0.35)
90° lay-down when height ≫ footprint, then re-drop.

### 3 — Arch sag (any softness > 0)
Per component, BFS from bench-touching solids. Sag only voxels that are
**horizontally far** from the bench footprint (true cantilevers /
branches) — not sphere crowns. Tip drop `� from the bench footprint (true cantilevers /
branches) — not sphere crowns. Tip drop `∝ softness · t²` where `t` is
normalised support-graph distance. Reseal 1-voxel gaps so the arch
doesn’t shatter into crumbs. **No column packing** (that snapped arches).

### 4 — Splat (softness ≥ 0.25)
Bench-pack → target mound height from volume → spread + equalise →
despike → SDF rewrite.

### Write gate
Solid→SDF rewrite runs **only** if tip, sag, or splat changed geometry.

## UX

| | |
|---|---|
| Menu | `Sculpt → Settle (gravity)…` |
| Hotkey | `Ctrl+Shift+G` |
| Softness | `0` drop only · low = arch sag · high = tip + splat |

## Implementation

`crates/sculpt-core/src/plastic.rs`
