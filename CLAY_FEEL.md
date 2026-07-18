# MUD — Clay Feel Track

Companion to `DESIGN.md` §2.4 / §4 Stage 1 and `PLAN.md`.

**Thesis:** the platform (SDF, tools chrome, layers, export) is ahead of
the material model. Next work adds *true* Press / Pull displace tools
beside the existing Clay Add/Remove — not by replacing it.

**Product call (locked 2026-07-17):**

- **Keep Add / Remove as it is** — soft CSG, magic-clay bulge, Shift+LMB
  add, empty-bench coils, side-column rings. It stays the fast “blob /
  carve / deposit” tool (`ToolKind::Clay`, palette `1`).
- **Add new Press and Pull tools** — DESIGN §2.4 volume redistribution
  (`∆V` → recruit → redistance). Workshop finger tools, separate palette
  entries and shortcuts.

**Falsifiable bar:** with Press selected, a stranger should say the
material “pushes back,” not “carves foam.” Add/Remove can keep feeling
like deposit / eraser — that’s fine; it’s a different job.

---

## Priority list (do in order)

| P | Item | Why first | Exit criterion |
|---|------|-----------|----------------|
| **P0** `[x]` | **New Press tool** — `∆V⁻` → recruit → redistance; LMB depresses | Core clay thesis; Add/Remove unchanged | Pressing a face leaves a conserved rim; volume roughly held |
| **P1** `[x]` | **New Pull tool** — inverse: grow outward, thin the neighbourhood | Completes the finger pair | Pull raises a bump and draws from surroundings |
| **P2** `[x]` | **Palette / UX** — slots, shortcuts, ghosts, HUD hints for Press/Pull | Discoverable without touching Add/Remove | Distinct ghost tints; `1` Add/Remove, `P` Press, `L` Pull, `K` Knife |
| **P3** `[x]` | **Paddle → displace** | DESIGN paddle displaces | Flat press squeezes a rim |
| **P4** `[x]` | **One sharp remove tool** (knife) | Hand-tool remove beside Press | Cut/shave removes volume; no recruitment |
| **P5** | **Feel amplifiers** — tilt→orientation, pressure→Press depth, tool-SDF ghosts | Amplifies P0–P1 | Stylus lean / depth feel right on Press/Pull |

Anything below P5 stays in `PLAN.md` / deferred unless it unblocks P0–P1.

---

## Tool map (target)

| Tool | Role | Volume |
|------|------|--------|
| **Add/Remove** (existing) | Deposit blobs / soft carve / coils | Not conserved (CSG ± magic bulge) |
| **Press** (new) | Finger push — displace into the surface | Conserved via rim recruit |
| **Pull** (new) | Finger pull — displace outward | Conserved via neighbourhood draw |
| Cookie / wire (/ knife) | Remove | Gone — no recruit |
| Smooth | Mollify φ | N/A |
| Paddle | Flat press — prefer displace later (P3) | Today: remove disk |

Add/Remove keeps `M` magic-clay and Shift polarity. Press/Pull do **not**
reuse that toggle; displace is their nature.

---

## P0 — New Press tool (spike → ship)

### Baseline left alone

`ToolKind::Clay` + `brush.rs` `BrushMode::Press`/`Pull` paths stay. Do
not rewrite Add/Remove to “become” Press.

### New tool sketch

- New `ToolKind::Press` (name TBD in code; UI label **Press**).
- Continuous LMB (like Clay/Smooth/Paddle).
- Sphere (or soft finger) footprint; `advance_per_step` / pen pressure =
  engagement depth.
- Core API: e.g. `apply_press_displace` (new), not a flag on
  `SphereBrush` that changes Add/Remove.

### Algorithm (DESIGN §2.4, minimal)

Per stamp:

1. Snapshot φ in the brush AABB.
2. Primary edit — depress surface (soft intersection with tool SDF).
3. Measure `∆V⁻` (volume that left the solid this stamp).
4. If `∆V⁻ > ε`, redistribute `+∆V⁻` into a **recruitment kernel**:
   surface band around contact, weighted sideways / behind the press
   direction.
5. Local redistance / fast sweep in the dirty region.
6. Mark chunks dirty; undo journals pre-mutation samples as today.

Cookie / wire / Add-Remove **never** call the recruit step.

### Spike checklist

- `[x]` `∆V` via occupancy sum — cheap at local AABB / interactive rates.
- `[x]` Weighted rim recruit + unit tests (depress, conserve, side rim).
- `[x]` Local Jacobi redistance (4 iters) in v1 dirty halo.
- `[x]` Symmetry: Press goes through the same mirror `apply_at` path.

### Non-goals for P0

- Changing Add/Remove behaviour or shortcuts.
- Full surface Poisson solve.
- Thumb / roller / multi-finger.
- Perfect global volume conservation (per-stamp local is the bar).

### Suggested files

- New core module or `brush.rs` sibling: press displace + volume +
  redistance helpers (Bevy-free).
- `sculpt-app`: `ToolKind::Press`, palette entry, `apply_at` / preview /
  size knobs shared where sensible.
- Tests: volume within tolerance on a sphere press; Clay Add/Remove
  regression suite still green unchanged.

---

## P1 — New Pull tool

Inverse of Press:

1. Primary edit grows the surface outward under the finger.
2. Measure `∆V⁺` gained in the contact.
3. Remove that volume from a surrounding surface recruitment band
   (thin the neighbourhood).
4. Redistance locally.

Same size / pressure / symmetry wiring as Press. Empty-bench Pull is a
no-op or soft fail — **Deposit stays on Add/Remove** (Shift+LMB on the
bench).

---

## P2 — Palette / UX

Ship with or right after P0/P1 so the tools aren’t hidden.

Suggested layout (adjust when implementing; **do not steal `1`**):

| Key | Tool |
|-----|------|
| `1` | Add/Remove (unchanged) |
| new | **Press** |
| new | **Pull** |
| … | existing cutters / wire / smooth / paddle / select / move |

Options if the digit row is full: shift Press/Pull onto a second row,
toolbar-only, or renumber less-used tools — decide at implement time.
HUD hint: “Press displaces · Add/Remove deposits or carves.”

Ghost: sphere (or finger) at engagement depth, distinct tint from
Add/Remove so the two jobs don’t look identical.

---

## P3 — Paddle displace `[x]`

Same `∆V` pipeline as Press; tool SDF = disk ∩ half-space flatten,
then annular rim recruit + local redistance. Ghost shares Press amber.

---

## P4 — One removal tool

Knife (thin box / wedge along stroke) *or* scraper (blade shave):

- Remove, **no** recruitment
- Physical ghost
- Sharp under Dual Contouring

Cookie + wire already remove; this is the hand-tool counterpart next
to Press.

---

## P5 — Feel amplifiers (after P0–P1)

1. Pen tilt → orientation for knife/scraper/paddle.
2. Ghost = tool SDF footprint.
3. Pressure → Press/Pull engagement depth (retune; Add/Remove can keep
   current pressure→advance mapping).
4. Reference images — useful, not clay.

---

## Explicitly later / don’t cut in front of P0–P1

- Rewriting or renaming Add/Remove
- Killing the magic-clay toggle on Add/Remove
- More matcap / bookmark polish
- Automatic weld-on-contact
- Per-piece transforms
- Colour, VR, collab, print services

---

## How to run this track

1. Spike P0 as a **new** tool beside Clay; A/B in-app: same blob, Press
   vs Add/Remove LMB.
2. Land Press when the Stage-1 “pushes back” test passes.
3. P1 Pull reuses Press’s measure/recruit/redistance helpers (inverted).
4. P2 palette polish can land in the Press PR or immediately after.
5. Update `PLAN.md` checkboxes as each P lands.

---

## Success snapshot

> **Add/Remove** builds and carves quickly (coils, soft CSG).
> **Press** moves clay. **Pull** draws clay. Cutters take clay away.

Two jobs, two tools — no need to make one brush pretend to be both.
