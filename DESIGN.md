# MUD — Design Notes for a Digital Clay Modeller

*A discussion document, not a specification. The point is to interrogate the
brief before writing code.*

---

## 0. TL;DR

- The philosophy — *simulate sculptural intention, not physical reality* — is
  strong and internally consistent. Its main hidden cost is that "magic clay"
  has to be **defined** rather than **simulated**: every ambiguity a real
  material resolves for free has to be resolved by a design decision.
- The right internal representation for the described interaction is almost
  certainly a **sparse narrow-band signed distance field** (VDB-like), with a
  **surface mesh extracted on demand** for rendering. Meshes-only and
  particles-only both fail specific criteria in the brief.
- Volume preservation, seamless merging, and cutting are cheap in an SDF-based
  representation and expensive in a mesh-based one — this is the single
  biggest architectural lever.
- The novelty must be tightly confined. Everything outside the sculpting
  viewport should be as boring as possible.
- A meaningful prototype is small: one tool, one representation, one camera,
  one turntable hotkey, no undo. If that prototype does not already feel good,
  no amount of features will save the product.

---

## 1. Critique of the philosophy

### 1.1 What the brief gets right

- **A single, sharp thesis.** "Simulate intention, not reality" is the kind
  of guiding sentence a team can actually steer by. It gives a decision
  procedure for every future feature request: does this improve sculptural
  intention, or does it add realism no one asked for?
- **Concentrating novelty.** By insisting that camera, menus, shortcuts, and
  file handling stay conventional, the brief protects the user's cognitive
  budget for the one place novelty is welcome. This mirrors what worked for
  Figma (novelty in collaboration, conventional everywhere else) and what
  broke for many well-intentioned 3D tools (novelty everywhere → nothing
  transfers).
- **Two-tier tools (immediate / advanced).** This is the same pattern as
  Blender geometry nodes vs. modifier presets, Figma components vs. variants,
  Photoshop filters vs. filter parameters. It's a proven way to keep
  beginners fluent without capping expert ceilings.
- **Physical tools with geometry.** Treating a scraper as a *shape* that
  interacts with the volume, rather than as a *brush kernel with parameters*,
  is unusual and — if it works — is probably the most memorable interaction
  idea in the brief. It rewards spatial reasoning over parameter tweaking.
- **The workbench as a tool.** This is genuinely fresh. Most sculpting apps
  render a floor for orientation only. Making it collide, brace, and flatten
  is a small change with disproportionate payoff.
- **Turntable without inertia.** Refusing to simulate the parts of a wheel
  that a real potter has to fight is a nice concrete example of the whole
  philosophy. It also has a real ergonomic case: you can hold a scraper
  still while the *world* moves, which is genuinely hard in every existing
  3D tool.

### 1.2 Where the brief is under-specified

None of these are objections. They are the design decisions the philosophy
punts on and someone has to make.

1. **Whose intuition are we modelling?** Coil-builders, throwers,
   figurative sculptors, mould-makers, and digital ZBrush users all have
   different assumptions about what clay "wants". The brief implicitly
   privileges the *press/pull/scrape/cut* subtractive-additive workshop
   sculptor. That's fine, but it needs to be stated: the app is opinionated,
   and users from other traditions will bounce.
2. **What is "volume preserved" a rule about?** Volume preservation as a
   headline feature interacts with:
   - the loop tool (which *removes* material)
   - the scraper (also removes)
   - cookie cutter subtraction (removes)
   - pressing (should *displace*, not remove)
   The user needs a clear mental model of when material is displaced and
   when it's removed. This is not a physics question — it's a UX question,
   and we should answer it *per tool*, not globally.
3. **Input hardware.** The brief doesn't say. Mouse-only sculpting is
   possible but never fluent. A pen tablet with pressure and hover is
   effectively required for the experience the brief describes. VR and
   haptics are optional endgame; they should not drive early architecture.
4. **Output pipeline.** What is a finished piece *for*? 3D printing (STL),
   rendering (glTF/OBJ), or just personal satisfaction (screenshot)? Each
   implies different constraints (watertightness, scale accuracy, UVs).
5. **Colour, texture, decoration.** The brief is silent. I would
   deliberately defer these — decorated pottery is a whole second app — but
   the representation should not preclude them.
6. **Multi-piece scenes.** The moment you allow cuts, you have multiple
   pieces. That means selection, grouping, hide/show, alignment — a small
   scene-graph creeps in even if you didn't want one.
7. **Symmetry.** Not mentioned, but beginners rely on it heavily. Live
   mirror-plane symmetry is table stakes; the moment you have it, it
   interacts with cutting, turntable, and undo.
8. **Failure modes.** What happens when the user tries something impossible
   (cut a piece into 200 slivers, sculpt a feature thinner than a voxel)?
   Magic clay has to gracefully degrade or the illusion breaks.

### 1.3 Hidden complexity the brief undersells

- **"Convincing forms within minutes"** hides a lot of scaffolding:
  starter primitives, sensible default tool sizes, sensible default
  workbench height, symmetry on by default, undo that works instantly,
  and a matcap shader that flatters bad geometry. The interaction is only
  half the reason people succeed early — the other half is defaults.
- **"Tools are physical objects with geometry"** implies collision detection
  between arbitrary rigid tool geometry and the deforming volume, at
  interactive rates. This is the single most computationally serious
  commitment in the whole brief.
- **"Cuts create editable pieces"** is a topological operation. Meshes hate
  these. SDFs make them cheap but not free — you still need to relabel
  connected components and manage identities across undo.
- **"Separate pieces merge seamlessly"** is the inverse: it's the *easy*
  case for SDFs (`min` of two fields is a smooth union) but the confusing
  case for the user, who now needs a mental model of when two pieces are
  "one thing" versus "two things touching". This is not a rendering
  question — it's an identity question.
- **The turntable is deceptively simple.** It sounds like just a keyframe
  of `object.rotation.y`. But it interacts with:
  - undo (do we undo the sculpting done during rotation, or the rotation
    itself, or both?)
  - the workbench (does the workbench spin with the piece? — I'd argue no)
  - symmetry planes (do they follow the piece or the world? — piece)
  - camera (probably stays fixed)
  - recording of tool paths for reproducible edits (they must be stored
    in piece-local coordinates)
- **Performance is a UX feature.** For "an experienced sculptor stops
  thinking about software", edit-to-pixel latency must be < 16 ms and
  ideally < 10 ms. That is the hard technical bar. Everything else is
  optional.

---

## 2. Internal representation

The single most consequential decision. I'll evaluate five candidates against
the specific claims in the brief, not against generic 3D-modelling criteria.

### 2.1 Criteria (derived directly from the brief)

| # | Criterion | Weight |
|---|---|---|
| C1 | Continuous volume (no visible polygonal artefacts) | high |
| C2 | Seamless merging on contact | high |
| C3 | Cuts producing separately editable pieces | high |
| C4 | Pressing displaces material rather than deleting it | high |
| C5 | Tool geometry directly shapes the result | high |
| C6 | Sharp features when the user asks for them (knife, scraper) | medium |
| C7 | Instant local edits at 60+ fps | high |
| C8 | Reasonable memory at usable resolution | medium |
| C9 | Undo/redo without full snapshots | medium |
| C10 | Export to a printable/renderable mesh | medium |

### 2.2 Candidates

**A. Triangle mesh with dynamic tessellation** (ZBrush Sculptris / Blender
Dyntopo / SculptGL style).
- C1: good visually, bad structurally (topology becomes noise).
- C2: awful — booleaning meshes at interactive rates is fragile.
- C3: painful — requires robust mesh cutting, always a source of bugs.
- C4: hard — volume is implicit; enforcing displacement requires
  per-vertex mass and constraint solving.
- C5: mediocre — tool geometry has to be projected onto the surface.
- C6: excellent — sharp features are trivial.
- C7: excellent for small edits, poor after heavy remeshing.
- C8: excellent.
- C9: hard — mesh diffs are large and topology changes make journalling
  awkward.
- C10: trivial (it *is* a mesh).

**B. Dense uniform SDF grid.**
- C1: excellent.
- C2: trivial (`min` of two SDFs is a smooth union).
- C3: easy (label connected components after cut).
- C4: natural (edits are field displacements; volume is measurable and
  redistributable).
- C5: excellent — tools are rigid SDF stamps.
- C6: only if you use dual contouring / Hermite data; otherwise mushy.
- C7: good for small edits, painful at high resolutions because memory
  is O(N³).
- C8: bad. A 512³ float grid is 512 MB.
- C9: fine (chunk-level diffs).
- C10: needs mesh extraction (marching cubes / dual contouring).

**C. Sparse narrow-band SDF (VDB / OpenVDB / NanoVDB).**
- Same wins as B, plus:
- C8: excellent — memory scales with surface area, not volume. 2048³
  effective resolution is realistic on a laptop.
- C7: excellent — edits touch only tiles near the surface.
- Additional complexity: tile management, but well-solved.

**D. Position-based dynamics / particles / MPM.**
- C1: mediocre — surface is implicit in particle distribution.
- C2: good.
- C3: hard — cutting particles is easy, but keeping a coherent surface is
  not.
- C4: excellent — explicit volume constraints are one of PBD's strengths.
- C5: mediocre — collisions with arbitrary geometry are expensive.
- C6: bad — sharp features require dense particle counts.
- C7: acceptable at moderate particle counts.
- C8: acceptable.
- C9: hard — particle state is large.
- C10: painful — surface reconstruction (Zhu-Bridson etc.) is another
  whole system.

**E. Tetrahedral mesh + FEM.**
- Great physics, wrong problem. Retetrahedralisation under large
  deformation is one of the ugliest problems in graphics. Rejected for
  interaction-quality reasons alone.

### 2.3 Recommendation

**Sparse narrow-band SDF (option C) as the source of truth, with an
incrementally-extracted triangle mesh for rendering.**

Rationale, mapped to the brief:

- *"clay is continuous"* → an SDF is an infinitely resolved field.
- *"clay has volume"* → volume is `∫₁_{φ<0} dV`, cheap to sum per tile.
- *"clay deforms rather than stretching polygons"* → editing is a field
  operation, not a vertex operation. There are no polygons in the
  representation to stretch.
- *"pushed, pulled, compressed"* → each is a local field operation
  (positive displacement, negative displacement, redistribution).
- *"separate pieces merge seamlessly"* → `φ_union = min(φ_a, φ_b)`. If we
  want it soft, `smin(φ_a, φ_b, k)` (Inigo Quilez's smooth-min). If we
  want them to *stay* separate on contact, we tag tiles by object ID and
  refuse to merge across IDs unless the user asks.
- *"cuts create editable pieces"* → intersect with a half-space to remove
  material; run a connected-components label to detect newly-separated
  regions; assign fresh object IDs.
- *"tools leave characteristic marks"* → tools are represented as SDFs
  themselves (or as procedural signed-distance functions if analytic —
  e.g. a knife blade is a thin box, a wire is a capsule, a cookie cutter
  is an extruded 2D SDF). The engagement of the tool with the workpiece
  is `workpiece = op(workpiece_φ, tool_φ_at_current_pose)`.
- *"pressing displaces material instead of deleting it"* → the tool stamp
  is used to *depress* the surface (negative displacement of `φ` inside
  the tool), while a companion positive displacement is applied to a
  ring of nearby tiles to conserve volume. See §2.4.

### 2.4 "Magic clay" as a well-defined algorithm

For each tool engagement over a small time step:

1. Compute the tool's rigid SDF in the workpiece's local frame.
2. Compute the *removed volume* `∆V⁻`: the mass of `φ` cells that crossed
   the zero isosurface into the tool's interior this step.
3. Apply the *primary edit*: the tool stamp modifies `φ` inside its
   footprint.
4. If the tool is a *displacement tool* (finger, thumb, paddle, roller),
   redistribute `+∆V⁻` across a "recruitment kernel" surrounding the
   contact patch. The kernel is a weighted band on the surface of the
   workpiece, weighted by the tool's motion vector and by surface normal
   alignment. Implementation: solve a small local Poisson-like update
   that adds material where the recruitment kernel is largest, subject
   to a smoothness prior.
5. If the tool is a *removal tool* (knife, wire, loop, scraper, cookie
   cutter in subtract mode), skip step 4. The material is gone.
6. Re-normalise `φ` in the affected tiles (fast sweeping / redistancing).
7. Mark affected tiles as dirty for the mesher.

This gives a small, principled set of *tool semantics*:
- **Displace:** edit + redistribute (finger, thumb, paddle, roller).
- **Remove:** edit only (knife, wire, loop, scraper, cookie-cutter-cut).
- **Add:** inverse edit — pull material outward, redistributing from the
  surrounding surface (finger-pull mode, or "smear").
- **Smooth:** local mollification of `φ` (Gaussian blur, clamped near
  sharp features via a curvature test).
- **Merge:** `smin` union on contact when both operands are tagged
  compatibly.
- **Cut:** split by half-space; run label pass.

The `∆V⁻` bookkeeping is what makes the app feel like clay. Without it,
SDF sculpting feels like carving foam.

### 2.5 Sharp features

Dual contouring with Hermite data (Ju, Losasso, Schaefer, Warren 2002)
solves the "SDF makes everything mushy" problem. When a tile contains a
crease or corner, the QEF solver places the vertex exactly on the crease.
For a knife cut, that means the cut looks like a cut, not a rounded slot.
For a scraper mark, the flat is flat.

Practically: dual contouring at the surface tiles, with Hermite normals
sampled from the analytic tool SDFs at the moment of edit. This means
the *tool's own crispness* survives into the mesh.

### 2.6 What about hybrids?

A hybrid that came up in the brief: SDF core + PBD skin for local elastic
response. I would explicitly *not* build this in the prototype. Elastic
skin is exactly the "physical realism" the brief rejects. If we later
discover users want dough-like squish, we can add a shape-matching
constraint layer over the SDF, but only if a user need forces it.

---

## 3. Software architecture

Deliberately conservative outside the sculpting engine. The novelty budget
is spent on the geometry core.

```
┌──────────────────────────────────────────────────────────────┐
│  Application shell                                            │
│    - main window, docked panels, menus, shortcuts             │
│    - project files, recent files, autosave                    │
│    - camera controller (orbit/pan/zoom)                       │
│    - viewport(s)                                              │
├──────────────────────────────────────────────────────────────┤
│  Interaction layer                                            │
│    - input router (mouse/pen/keyboard/tablet)                 │
│    - tool selection & tool state                              │
│    - turntable state (angular velocity, axis)                 │
│    - workbench state (position, orientation)                  │
│    - symmetry state                                           │
│    - stroke recorder (feeds undo journal)                     │
├──────────────────────────────────────────────────────────────┤
│  Tool layer                                                   │
│    - Tool interface:                                          │
│        rigid geometry (analytic SDF or mesh→SDF)              │
│        engage(motion, pressure, dt) → EditOp                  │
│        parameters (basic / advanced)                          │
│    - concrete tools: finger, thumb, paddle, knife, wire,      │
│      loop, roller, scraper, cookie cutter, workbench          │
├──────────────────────────────────────────────────────────────┤
│  Geometry engine (the "sculpting core")                       │
│    - sparse SDF storage (tiled narrow band)                   │
│    - edit dispatcher (applies EditOps to tiles)               │
│    - volume bookkeeping & redistribution                      │
│    - redistancing / normalisation                             │
│    - connected-components labelling & object identity         │
│    - undo journal (edit ops + periodic checkpoints)           │
├──────────────────────────────────────────────────────────────┤
│  Meshing & rendering                                          │
│    - incremental dual contouring on dirty tiles               │
│    - matcap / ambient-occlusion shading                       │
│    - tool preview (ghosted tool geometry at cursor)           │
│    - workbench visuals                                        │
│    - turntable indicator                                      │
├──────────────────────────────────────────────────────────────┤
│  Platform / IO                                                │
│    - windowing, GPU (Vulkan/Metal/WebGPU/OpenGL)              │
│    - file format (native SDF+journal, exports: STL/OBJ/glTF)  │
│    - tablet / pen APIs (Wintab, Ink, Cocoa NSEvent, Pointer)  │
└──────────────────────────────────────────────────────────────┘
```

### 3.1 Data flow of a single stroke

1. Pen down. Input router creates a *Stroke* object bound to the current
   tool and current object.
2. Each input sample (position, pressure, tilt, timestamp) is transformed
   into workpiece-local coordinates (accounting for turntable rotation).
3. The tool's `engage()` produces one or more `EditOp`s describing the
   local field change.
4. The edit dispatcher applies each `EditOp` to the affected tiles,
   updates volume bookkeeping, and appends the op to the undo journal.
5. Dirty tiles are queued for redistancing and re-meshing.
6. The mesher rebuilds surface tiles asynchronously (or synchronously if
   the queue is small); rendering displays the new surface.
7. Pen up. The Stroke is closed and marked as a single undo unit.

### 3.2 Undo/redo

- Store an **edit journal**: an ordered list of `EditOp`s with their
  parameters and RNG seeds (if any tool uses noise).
- Periodically snapshot the SDF tiles that have been modified since the
  last checkpoint (~every 32 strokes, or every N MB of dirty data).
- Undo to a point = restore the nearest earlier snapshot of affected
  tiles, then replay journal forward to the target point.
- Redo = replay forward.
- Because tiles are sparse, snapshots are small (only touched tiles).
- Strokes group into single undo units by default; power users get
  per-op undo via a modifier.

### 3.3 Turntable, workbench, symmetry — coordinate frames

Three frames:

- **World** — camera, lights, screen-space UI.
- **Workbench** — attached to the turntable. Rotates when the user
  holds the turntable hotkeys.
- **Piece-local** — attached to each object on the workbench. Cuts
  create new objects with their own piece-local frames.

Tools operate in **piece-local** space. This has three good consequences:

- The undo journal records tool paths in piece-local coordinates. Replaying
  a stroke after the turntable rotated gives the same result on the piece.
- Symmetry planes live in piece-local coordinates, so mirror behaves
  intuitively when you spin the piece.
- The workbench collides with the piece via its own SDF, in workbench
  space; the piece's SDF is transformed into workbench space for that
  collision.

### 3.4 Rendering

- **Matcap shading** for the sculpt surface. Matcaps flatter bad
  geometry, run on any hardware, and are what ZBrush and Blender's
  sculpt mode both use for exactly this reason.
- **Ambient occlusion** (SSAO or a baked cavity term from the SDF, which
  is actually cheap: cavity ≈ smoothed(`φ`) − `φ`). Cavity is very
  flattering on sculpts.
- **Ghost preview** of the tool geometry at the cursor position, drawn
  translucent, so the user always sees the shape they're about to press
  in with.
- **Workbench** rendered as a plain matte surface with a subtle grid,
  slight rim shadow where the piece touches it.
- **Turntable indicator** a thin arc on the workbench edge showing
  current angular velocity, so the user has a persistent sense of
  rotation without a distracting HUD.

### 3.5 Threading

- Main thread: input, UI, camera, tool engagement, edit journalling.
- Worker pool: redistancing, meshing, ambient occlusion.
- GPU: shading, tile upload, optional GPU-accelerated stamping for very
  large brushes.

A useful rule: **edits are synchronous, meshing is asynchronous.** The
user's cursor never has to wait for the mesher. If the mesh is one frame
stale, that's fine — the ghost tool preview already tells the user what
they're doing.

### 3.6 Language / stack

Not the point of this document, but for planning:

- Core geometry engine in a systems language (Rust or C++). This is
  the hot loop.
- UI in whatever gets a viewport up fastest on the target platforms. If
  desktop-first: egui/Slint (Rust) or Qt (C++). If web-first: WebGPU +
  a native shell (Tauri) later.
- GPU shading via WebGPU or Vulkan for portability.
- OpenVDB / NanoVDB is a candidate for the sparse SDF store; it's
  battle-tested, GPU-friendly (NanoVDB), and permissively licensed.
- A custom tiled SDF is also viable and cheaper to reason about — we
  don't need most of VDB's features. Trade-off: NanoVDB gets you further
  faster, custom gets you tighter fit.

---

## 4. Staged roadmap

Each stage ends with a **falsifiable question**. If the answer is no,
that stage failed and we should reconsider before adding more.

### Stage 0 — "Does the interaction feel right at all?"

Scope:
- One representation: **dense** SDF grid, ~256³, single object.
- One tool: spherical finger brush, push and pull (modifier toggle).
- One camera: orbit/pan/zoom, mouse only.
- Dual contouring rendering with a matcap.
- Turntable: hold `R` to rotate right, `L` for left, at a fixed
  angular velocity. No inertia.
- No undo yet. Save/load a raw grid to disk.
- No workbench yet.

Test: hand a laptop to someone who has never sculpted digitally. Ask them
to make a mug shape. Do they succeed in five minutes without instruction?

If yes → the core loop is real. Proceed.
If no → the problem is in the tool feel, not in the missing features.
Iterate on the finger brush before doing anything else.

### Stage 1 — "Does magic clay behave like clay?"

Scope:
- Volume-preserving redistribution (§2.4).
- Two more tools: **paddle** (planar contact stamp) and **smooth**.
- Undo via edit journal + periodic snapshots.
- The **workbench**: an infinite SDF plane that collides. Pressing the
  piece against it flattens it. The piece rests on it under gravity —
  where "gravity" here just means "we snap the lowest surface point
  to the workbench when nothing else acts on it". Not simulated
  physics; a nudge.
- Matcap library (5–10 presets).

Test: give the app to two people. Person A has sculpted with real clay,
person B has not. Both should describe the material as "pushing back" and
neither should describe it as "melting" or "cutting away".

### Stage 2 — "Can the app support topology changes without feeling fragile?"

Scope:
- **Cookie cutter**, immediate mode, with 6 primitive shapes.
- **Wire cutter**: planar cut → new objects labelled and pickable.
- **Merge on contact**: two overlapping objects can be welded (explicit
  action, not automatic — see §5).
- **Symmetry**: one mirror plane in piece-local space, live.
- Migrate to **sparse narrow-band SDF** storage (VDB or custom) to
  unlock ≥ 1024³ effective resolution.

Test: cut a lump in half, sculpt each half separately, then merge them
back. This must not corrupt the geometry, must not lose fine details, and
must not desync the undo journal.

### Stage 3 — "Are the tools expressive enough?"

Scope:
- Full immediate-mode tool set: finger, thumb, paddle, knife, wire
  cutter, loop, roller, scraper, cookie cutter.
- Advanced-mode parameters where they earn their keep:
  - Cookie cutter: symmetry count, corner radius, wave amplitude/
    frequency, edge modulation function.
  - Scraper: blade profile (2D SDF).
  - Loop: loop shape.
- **Pen tablet** support (Wintab / Ink / native APIs).
- Pressure → depth of engagement. Tilt → tool orientation for the
  scraper and knife.

Test: publish a gallery challenge — "make a teacup, a face, and an
abstract vase". Watch users. If they reach for menus mid-sculpt, the
tool set is wrong or the shortcuts are wrong.

### Stage 4 — "Is it a product?"

Scope:
- STL / OBJ / glTF export. Watertightness check on export.
- Native project format: sparse SDF tiles + journal + scene metadata.
- Recent files, autosave, crash recovery.
- Reference images pinned to the workbench.
- Multi-piece scenes with hide/show and per-piece transforms.
- Camera bookmarks. Turntable with configurable axis (not just world Y,
  because the piece might have been tilted).
- Snap-to-workbench for the base of a piece.
- Optional: haptic pen support for users who have one, but do not
  design around it.

### Stage 5 and beyond — deliberately vague

Only if earlier stages worked. Candidates:
- Colour/decoration mode (own document, own decisions).
- Procedural / scripted cutters and tools (advanced tier of the
  two-tier philosophy).
- Collaborative sculpting.
- VR mode.
- Tablet/iPad port.
- Print service integration.

I would not commit to any of these until Stage 4 has shipped and been
used by real people for real projects.

---

## 5. Design decisions that need to be made early

These are the choices where deferring is expensive because they leak
into every subsequent decision.

1. **Displacement vs. removal is per-tool, not global.** The finger
   displaces. The knife removes. The scraper removes (that's what a
   scraper does — it shaves off high spots). The paddle displaces. This
   should be visible in the tool's icon or documentation, but not a
   toggle the user has to think about mid-stroke.
2. **Objects have identity.** Two touching objects are not the same
   object. Merging is an explicit user action ("weld"). This costs
   a little UX friction and saves an enormous amount of confusion.
3. **Turntable rotates the piece, not the world.** Camera and lighting
   stay put. This makes the app feel like a workbench, not a video game.
4. **Undo is stroke-granular by default.** Ctrl+Z undoes a whole
   pen-down-to-pen-up stroke. Power users can shift-Ctrl-Z for
   per-op undo.
5. **Symmetry is a piece-local property, on by default for the starter
   ball.** Turn it off with one shortcut. Beginners get symmetric forms
   for free.
6. **The workbench is always present.** No "no floor" mode. The
   workbench is part of the metaphor.
7. **Native units are physical.** Millimetres, not scene units. This
   matters the moment someone tries to 3D-print a piece.
8. **Coordinate frames are explicit** in the API and in the file format
   (world / workbench / piece-local). Cheap to design in, expensive to
   retrofit.
9. **Meshes are outputs, not sources of truth.** Importing an OBJ
   converts it to SDF on load. There is no "edit polygons" mode.
   This is a defining choice — it forecloses some flexibility (users
   can't do retopology in-app) and gains everything the brief asks for.
10. **No modes hidden in modifier keys.** Every tool state should be
    visible in the cursor and the toolbar. The novelty is in the tools,
    not in the shortcut arcana.

---

## 6. Interaction / UX notes

- **Cursor is the tool.** The ghost preview of the tool geometry at
  the cursor is more important than a crosshair. If the tool is a
  cookie cutter, the user sees the cutter floating over the surface.
- **Hover ≠ engage.** Pen or mouse over the workpiece without pressure
  shows the ghost. Pressure (or mouse-down) engages. This mirrors real
  hand-to-clay interaction.
- **Pressure = depth**, not size. Size is chosen ahead of the stroke.
  This is opposite of what many painting apps do, and it is the right
  choice for sculpting because it gives the user a stable "reach" while
  allowing subtlety of touch.
- **Tilt = orientation** for tools with a directional geometry (scraper,
  knife, paddle). The finger and thumb are radially symmetric so tilt
  is ignored.
- **Turntable hotkeys are held**, not tapped. Tap-to-toggle would be
  a footgun — the piece would keep spinning while you sculpt and it
  would feel possessed. Holding is unambiguous.
- **Speed of the turntable** is a setting, with reasonable defaults
  (say, one revolution in 4 seconds). Power users get finer control
  via a modifier (shift-hold = slow, alt-hold = fast).
- **Reset viewpoint** is a hotkey (F, following Blender's convention
  for "frame selected"). The novelty budget is protected; conventional
  shortcuts do conventional things.
- **Snap-to-symmetric-primitive** for the starting shape. Start every
  new file with a centered ball on the workbench, not an empty scene.

---

## 7. Prior work worth reading

Not exhaustive, but the papers and products that most directly
inform this design.

**Volume sculpting and SDF sculpting**

- Galyean, T.A. & Hughes, J.F. *Sculpting: An Interactive Volumetric
  Modeling Technique.* SIGGRAPH 1991. The direct ancestor of every
  volumetric sculpting tool that followed.
- Wang, S.W. & Kaufman, A.E. *Volume Sculpting.* Interactive 3D 1995.
- Perry, R.N. & Frisken, S.F. *Kizamu: A System for Sculpting Digital
  Characters.* SIGGRAPH 2001. Philosophically the closest prior work
  to this brief; uses adaptively sampled distance fields.
- Frisken, S.F. et al. *Adaptively Sampled Distance Fields.* SIGGRAPH
  2000. Foundational for octree SDFs.
- Bærentzen, J.A. *Octree-based Volume Sculpting.* Late Breaking Hot
  Topics IEEE Vis 1998.
- Museth, K. *VDB: High-Resolution Sparse Volumes with Dynamic
  Topology.* ACM TOG 2013. The sparse volume structure to consider.
- Museth, K. *NanoVDB.* SIGGRAPH 2021 Talks. GPU-friendly VDB.

**Mesh extraction**

- Lorensen, W. & Cline, H. *Marching Cubes.* SIGGRAPH 1987.
- Ju, T., Losasso, F., Schaefer, S., Warren, J. *Dual Contouring of
  Hermite Data.* SIGGRAPH 2002. Enables sharp features from SDFs.
- Schaefer, S. & Warren, J. *Dual Marching Cubes.* IEEE Vis 2004.

**SDF operations and tool geometry**

- Quilez, I. Various writings on smooth minimum, SDF primitives, and
  domain repetition. Not academic but influential in the demoscene /
  Shadertoy community; directly applicable to procedural tool profiles.
- Second Order. *Claybook.* Game entirely built on animated SDFs;
  proof by demonstration that SDF-first interactive geometry works.

**Sculpting UX and commercial tools**

- Pixologic ZBrush — decades of interaction design worth studying,
  particularly the pen dynamics, brush anchoring, and lazy mouse.
- Sculptris — the "add material" flow and dynamic tessellation are
  a good reference for beginner-friendliness.
- 3D-Coat — voxel and surface mode coexistence; a cautionary tale for
  the complexity that arises when you support both.
- Meshmixer — boolean and stamp tools are close in spirit to the
  cookie cutter concept.
- SensAble FreeForm / ClayTools — the seminal commercial haptic
  sculpting system. Instructive on how much haptics actually change
  the experience (answer: less than you'd think, but not nothing).
- SculptGL — open source, small enough to read.

**Deformable simulation (for context, not adoption)**

- Müller, M., Heidelberger, B., Hennix, M., Ratcliff, J. *Position
  Based Dynamics.* J. Vis. Comm. and Image Rep. 2007.
- Macklin, M., Müller, M. *A Constraint-Based Formulation of Stable
  Neo-Hookean Materials.* SCA 2021.
- Jiang, C. et al. *The Material Point Method for Simulating Continuum
  Materials.* SIGGRAPH 2016 course.

**Sketching and beginner-friendly modelling**

- Igarashi, T., Matsuoka, S., Tanaka, H. *Teddy: A Sketching Interface
  for 3D Freeform Design.* SIGGRAPH 1999. The gold standard for
  low-friction 3D modelling for beginners.
- Nealen, A., Igarashi, T., Sorkine, O., Alexa, M. *FiberMesh:
  Designing Freeform Surfaces with 3D Curves.* SIGGRAPH 2007.

**Interaction design**

- Buxton, W. *Sketching User Experiences.* On why prototyping the
  interaction matters more than prototyping the features.
- Victor, B. *A Brief Rant on the Future of Interaction Design.* On
  respecting the hand.
- Kay, A. *"Simple things should be simple, complex things should be
  possible."* The one sentence that best captures the two-tier tool
  philosophy in §1.

---

## 8. Concrete next step

If we agree on the shape of this document, the smallest useful thing to
build next is Stage 0 from §4: one grid, one brush, one camera, one
turntable hotkey, no undo. That is a two-to-four-week prototype for one
person, and it will settle 80% of the arguments about whether the
philosophy holds up under a mouse.

Everything in Stages 1–4 depends on Stage 0 feeling good. If it doesn't,
we iterate on the finger brush before we do anything else.
