# MUD — Clay Feel Track

Companion to `DESIGN.md` §2.4 / §4 Stage 1 and `PLAN.md`.

**Thesis:** the platform (SDF, tools chrome, layers, export) is ahead of
the material model. Next work closes the gap to *intention-simulating
clay* — especially true Press / Pull — before more Stage-4 product
features.

**Falsifiable bar (DESIGN Stage 1):** a stranger should describe the
material as “pushing back,” not “carving foam / gluing blobs.”

---

## Priority list (do in order)

| P | Item | Why first | Exit criterion |
|---|------|-----------|----------------|
| **P0** | **Press displace v1** — real `∆V⁻` → recruit → redistance on Clay LMB | Without this, nothing else reads as clay | Pushing a face leaves a rim of conserved volume; soft clay no longer “melts away” |
| **P1** | **Tool vocabulary realign** — Press / Pull / Carve (remove); drop magic-clay as a user story | DESIGN forbids hidden polarity + mid-stroke displace toggles | UI/logs say Press/Pull/Carve; Shift is not “add mode”; magic clay is not a toggle |
| **P2** | **Pull semantics** — recruit-from-surroundings *or* explicit Deposit/coil | Current “Pull” is soft union Add | Pull thins the neighbourhood; Deposit is named and separate if we keep coil-building |
| **P3** | **Paddle → displace** | DESIGN: paddle displaces; today it carves a disk | Pressing a flat squeezes a rim, doesn’t delete a cylinder of clay |
| **P4** | **One sharp remove tool** (knife *or* scraper) | Proves per-tool Displace vs Remove beside Press | Cut/shave removes volume; no recruitment; dual-contour edges stay crisp |
| **P5** | **Polish that serves feel** — tilt→orientation, ghost = tool SDF, pressure→engagement depth on Press | Amplifies P0–P4; doesn’t replace them | Directional tools lean with stylus; ghost matches stamp |

Anything below P5 stays in `PLAN.md` / deferred unless it unblocks P0–P4.

---

## P0 — Press displace v1 (spike → ship)

### Current behaviour (baseline)

Clay LMB → `BrushMode::Press` → soft/hard subtract CSG + optional rim
bulge (`brush.rs`). No volume measurement, no conserved redistribute,
no redistancing. Names are historical: Press ≈ remove, Pull ≈ add.

### Target algorithm (DESIGN §2.4, minimal)

Per stamp while Press is engaged:

1. Snapshot φ in the brush AABB (or use pre-mutation undo samples).
2. Apply primary edit (depress / soft intersection with tool SDF).
3. Measure `∆V⁻` = volume that crossed into air this stamp
   (`∫ max(0, φ_after − φ_before)` over cells that left the solid, or
   equivalent voxel count × `voxel_size³`).
4. If `∆V⁻ > ε`, redistribute `+∆V⁻` into a **recruitment kernel**:
   surface band around the contact, weighted by sideways / behind the
   press direction (reuse the existing bulge bias as a weight prior).
5. Local redistance / fast sweep in the dirty region so φ stays a
   distance field.
6. Mark chunks dirty as today.

Removal tools (cookie, wire, future knife) **skip** steps 4–5’s
recruitment (edit only).

### Spike checklist (answer before polishing)

- `[ ]` Can we measure `∆V` cheaply enough at 1.5 mm / interactive rates?
- `[ ]` Does a simple weighted surface band beat the current bulge
  heuristic in a side-by-side “push the side of a blob” test?
- `[ ]` Does skipping redistance break the next stamp / mesher badly
  enough that redistance is mandatory in v1?
- `[ ]` Symmetry: recruit on both sides of the mirror plane.

### Non-goals for P0

- Full Poisson solve on the surface (band weights are enough for v1).
- Thumb / roller / multi-finger.
- Perfect global volume conservation (local stamp conservation is the bar).

### Suggested files

- `crates/sculpt-core/src/brush.rs` — Press path + `∆V` / recruit
- New helpers: volume measure + redistance (keep Bevy-free in core)
- `crates/sculpt-app/src/sculpt.rs` — wire depth/pressure into engagement
- Tests: conserved volume within tolerance on a unit sphere press;
  removal tools still lose volume

---

## P1 — Vocabulary & interaction realign

Do **after** P0 proves displace, so rename isn’t lipstick on CSG.

| Today | Target |
|-------|--------|
| Tool: Add/Remove | Tool: **Press** (displace) |
| Shift+LMB Add | **Pull** (P2) or separate **Deposit** |
| Magic clay toggle | Gone as UX; displace is Press’s nature |
| LMB carve foam | Separate **Carve** / Remove brush *or* only cutters remove |

Shortcuts proposal (adjust when implementing):

- `1` Press (default)
- Shift held or `1` dual-mode → Pull once P2 lands
- Carve on its own palette slot if we keep a spherical remove

Update README / HUD copy to workshop words (DESIGN §5).

---

## P2 — Pull vs Deposit

**Decision gate after P0:**

- **A — True Pull:** inverse of Press — grow outward, recruit *from*
  surrounding surface (thins nearby clay). Matches DESIGN “finger-pull.”
- **B — Deposit stays:** keep soft-union coil/bench build as **Deposit**
  (or Coil), clearly not displace. Press/Pull are the finger pair;
  Deposit is workshop “add a snake of clay.”

Recommendation: **both** eventually — Pull = A, Deposit = today’s Add
for empty-bench / coil workflows. Ship A first if volume math from P0
inverts cleanly; otherwise ship B rename immediately so we stop lying.

---

## P3 — Paddle displace

Same `∆V` + recruit pipeline as Press, tool SDF = disk / half-space
footprint. Primary edit flattens; rim receives volume. Workbench clip
unchanged.

---

## P4 — One removal tool

Pick **knife** (thin box / wedge cut along stroke) *or* **scraper**
(blade profile shave). Must:

- Remove with **no** recruitment
- Read as a physical ghost (profile extrusion)
- Look sharp under Dual Contouring

Cookie + wire already prove remove; this proves *hand-tool* remove next
to Press so the mental model sticks.

---

## P5 — Feel amplifiers (only after P0–P2)

1. Pen **tilt → tool orientation** for knife/scraper/paddle.
2. Ghost preview = actual tool SDF footprint (not only sphere/prism).
3. Pressure → **engagement depth** of Press (already partly wired to
   advance); retune once displace exists.
4. Reference images on the workbench (PLAN Stage 4) — useful, not clay.

---

## Explicitly later / don’t cut in front of P0–P4

- More matcaps, bookmarks, recent-files polish
- Automatic weld-on-contact
- Per-piece transforms / full scene graph
- Scraper blade library / loop shape params (need the tools first)
- Colour, VR, collab, print services (DESIGN defer)

---

## How to run this track

1. Spike P0 on a branch off the current tip; keep Surface Nets for speed
   while iterating; DC optional for evaluating sharp rims.
2. Land P0 behind a short A/B if needed (`displace_v2` pref) then make
   it default and delete the bulge-only path.
3. P1 rename in the same PR as “displace is default” or immediately after.
4. P2 decision note in this file (A/B) before coding Pull.
5. Update `PLAN.md` checkboxes as each P lands; keep this file as the
   ordered track.

---

## Success snapshot

When P0–P2 are done, the product pitch matches the code:

> Press clay and it moves. Pull clay and it comes with you.
> Cutters and knives take clay away. The workbench and turntable
> stay out of the way.

Until then, keep calling today’s Clay tool Add/Remove in docs — it’s
honest.
