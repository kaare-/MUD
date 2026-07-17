# Gravity settle — spike design

Status: **drop + elastic bow + soft squash**.

## Goal

Tunable gravity on the active layer:

- Floating lumps **crash onto the workbench**.
- Tall thin stalks **bow** (elastic lean) under mid softness.
- Soft clay can still **collapse / pancake** onto the bench.
- Softness slider `plasticity ∈ [0, 1]` = drop-only → bow + pancake.

Falsifiers:

1. An airborne blob lands with its lowest solid at `iy = 0`, even at
   softness `0`.
2. A thin tower at mid softness (`≈ 0.35`) keeps most of its height
   but its tip COM shifts sideways (bow).
3. The same tower loses substantial height and gains base width at
   softness `1`.
4. Softness `0` leaves a grounded tower unchanged.
5. A soft sphere on the bench loses height and widens (pancake).
6. A stiff fat blob (`softness ≈ 0.2`) barely squashes / does not bow.
7. Solid voxel count stays within ~15% (approximate conservation).

## Non-goals (spike)

- FEM / MPM / PBD / continuous sim / spring rebound
- Rest-pose rotation / tipping onto a face
- Multi-layer settle in one op
- Exact volume-preserving Poisson redistribution

## Algorithm

### Phase 1 — Drop

Same rigid rest as `Ctrl+G`: every connected component translates by
`−min_iy` so it touches the workbench.

### Phase 2 — Elastic bow (softness > 0)

For each **slender** component (`height / footprint ≥ 1.35`):

1. Pick a lean direction (existing top-vs-base lean, else `+X`).
2. Shear solid voxels laterally with `shift = amp · (y / H)²`.
3. `amp` scales with softness × aspect; capped so consecutive rows
   stay roughly connected.
4. Close internal column gaps without crushing cantilevers to the floor.

Fat / squat forms skip this phase.

### Phase 3 — Squash (softness > 0)

1. Stable height `max_stable = 2 + (1 − softness)² × 48`.
2. Sandpile while any column’s **solid count** or **peak height**
   exceeds `max_stable` (peak matters after bow — tip cantilevers are
   often 1-voxel columns high above the bench).
3. Rewrite the touched region from the solid mask and rebuild a
   3-voxel narrow band.

## UX

| | |
|---|---|
| Menu | `Sculpt → Settle (gravity)…` |
| Hotkey | `Ctrl+Shift+G` (rigid Rest stays `Ctrl+G`) |
| Control | Softness slider (default from Preferences) |
| Scope | Active layer · one undo stroke |

## Relationship to rigid Rest

`Ctrl+G` = phase 1 only. Settle always includes phase 1, then bow +
squash when softness > 0.

## Implementation map

- `crates/sculpt-core/src/plastic.rs` — drop + bow + sandpile + tests
- `crates/sculpt-app/src/gravity.rs` — hotkey / action / undo
- `actions` / `ui` — menu + softness dialog
