# Gravity settle — spike design

Status: **rewritten as gravity** (not a surface filter).

## Goal

Tunable gravity on the active layer:

- Floating lumps **crash onto the workbench**.
- Tall thin stalks **collapse / pile down** under soft clay.
- Stiff clay mostly keeps its shape after the drop.
- Softness slider `plasticity ∈ [0, 1]` = drop-only → soft pancake.

Falsifiers:

1. An airborne blob lands with its lowest solid at `iy = 0`, even at
   softness `0`.
2. A thin tower on the bench loses substantial height and gains base
   width at softness `1`.
3. The same grounded tower is unchanged at softness `0`.
4. A soft sphere on the bench loses height and widens (pancake).
5. A stiff fat blob (`softness ≈ 0.2`) barely squashes.
6. Solid voxel count stays within ~15% (approximate conservation).

## Non-goals (spike)

- FEM / MPM / PBD / elastic rebound / continuous sim
- True elastic bow curves (collapse + pile stands in for sag)
- Rest-pose rotation / tipping
- Multi-layer settle in one op
- Exact volume-preserving Poisson redistribution

## Algorithm

### Phase 1 — Drop

Same rigid rest as `Ctrl+G`: every connected component translates by
`−min_iy` so it touches the workbench. Narrow band moves with the
interior (see `gravity.rs`).

### Phase 2 — Squash (softness > 0)

1. Count solid voxels per `(ix, iz)` column (gaps fall out when packed).
2. Stable height `max_stable = 2 + (1 − softness)² × 48`.
3. Sandpile: while any column exceeds `max_stable`, move one voxel of
   count onto the shortest neighbour (iteration-budgeted).
4. Rewrite the region as packed columns from `iy = 0` upward and
   rebuild a 3-voxel narrow band from the solid mask.

This is bulk mass moving **down onto the bench**, not peeling the
surface sideways.

## UX

| | |
|---|---|
| Menu | `Sculpt → Settle (gravity)…` |
| Hotkey | `Ctrl+Shift+G` (rigid Rest stays `Ctrl+G`) |
| Control | Softness slider (default from Preferences) |
| Scope | Active layer · one undo stroke |

## Relationship to rigid Rest

`Ctrl+G` = phase 1 only (no squash). Settle always includes phase 1,
then optionally phase 2.

## Implementation map

- `crates/sculpt-core/src/plastic.rs` — drop + sandpile squash + tests
- `crates/sculpt-app/src/gravity.rs` — hotkey / action / undo
- `actions` / `ui` — menu + softness dialog
