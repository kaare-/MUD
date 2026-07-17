//! The sculpting subject: one or more layers sharing a domain.
//!
//! **Track B (`PLAN.md` / `SPARSE_THEN_LAYERS.md`):** a [`Layer`]
//! owns its own SDF grid, chunk-entity map, and dirty-chunk queue.
//! [`LayersState`] is the resource holding every layer plus which one
//! is *active* — the one tools read and write. All layers share the
//! same domain, resolution, and origin (no per-layer transform), and
//! are children of the same [`WorkpieceRoot`] entity, so the
//! turntable rotates every layer together with zero extra code.
//!
//! B1 (this pass) keeps exactly one layer — every tool, selection,
//! undo entry, and save/load path already goes through
//! [`LayersState::grid`] / [`grid_mut`](LayersState::grid_mut) /
//! [`mark_dirty`](LayersState::mark_dirty) instead of a bare `grid`
//! field, so adding a second layer later (B2: Insert Primitive → new
//! layer) is "push onto `layers`", not another app-wide refactor.
//!
//! **Track A2 (`PLAN.md` / `SPARSE_THEN_LAYERS.md`):** chunk entities
//! are spawned lazily and despawned when they go empty, instead of
//! eagerly pre-spawning every chunk in the domain at startup. This is
//! a real win even at a modest domain: a chunk only gets an entity
//! where the *surface* actually passes through it, not wherever the
//! grid has data — the starter sphere's shell touches only a small
//! fraction of the possible chunks, so most of them are never
//! spawned at all. It matters even more at the current 11×11×11 = 1331
//! chunk domain (Track A4): pre-spawning thousands of chunks that
//! will never hold geometry would be wasteful at that scale.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use glam::{UVec3, Vec3 as GVec3};

use sculpt_core::{extract_chunk_with, ChunkCoord, Grid};

use crate::settings::AppSettings;

use crate::actions::AppAction;
use crate::matcap::{MatcapMaterial, MatcapState};
use crate::selection::Selection;
use crate::undo::{DeleteLayerEntry, SculptStroke, UndoHistory};

/// Effective grid resolution per axis. **352³ at 1.5 mm/voxel gives a
/// 528 mm domain** (Track A4, `SPARSE_THEN_LAYERS.md`) — bigger than
/// the 400 mm `View → Workbench grid` overlay, and comfortably past
/// the ≥512 mm XZ target: room for a coil piece built up from an
/// empty bench (`File > New`, then `Shift+LMB` on the bare bench),
/// not just a single two-handed lump.
///
/// This only affords going ~1.8× past the old 192³ / 288 mm domain
/// (worst case dense-equivalent footprint ~174 MB) because Tracks
/// A1–A3 landed first: `Grid` only allocates the 32³ tiles a shape
/// actually touches (a starter sphere's shell is a handful of tiles
/// out of the 11×11×11 = 1331 possible, not all of them), chunk
/// entities are spawned only where there's geometry, and every
/// volume walker (wire cutter, component labels, rigid translate,
/// the Move-tool live preview) is scoped to the region it actually
/// touches instead of raster-scanning the whole domain. None of that
/// scales with `RES` directly, so growing this constant is now cheap
/// for typical pieces — it only costs what the user actually builds.
///
/// Kept cubic (same `RES` on every axis) for simplicity: this already
/// satisfies both the XZ footprint target and gives generous Y
/// headroom for a standing piece, without introducing an anisotropic
/// domain shape `Grid::from_sphere`, the workbench-origin math below,
/// and `ray_march`'s iteration budget would all need to account for.
const RES: u32 = 352;
const VOXEL_MM: f32 = 1.5;

/// Cap on chunks re-meshed per frame. Prevents big edits (large brush
/// or many chunks straddled) from turning into visible hitches. A
/// backlog just re-meshes over subsequent frames.
const MAX_CHUNKS_PER_FRAME: usize = 32;

#[derive(Component)]
pub struct WorkpieceRoot;

#[derive(Component)]
#[allow(dead_code)]
pub struct ChunkEntity {
    // Held for later stages (e.g. re-meshing systems that iterate
    // entities instead of the resource's HashMap).
    pub coord: ChunkCoord,
}

/// Stable identifier for a [`Layer`], independent of its position in
/// [`LayersState::layers`]. Undo entries are tagged with this (not an
/// index) so they still resolve correctly if layers are reordered,
/// merged, or deleted between recording and undo — none of which
/// happen yet in B1, but the tag costs nothing to carry now and saves
/// a re-plumbing pass in B2/B3.
pub type LayerId = u32;

/// One sculptable layer: its own SDF grid, its own chunk-entity map
/// and dirty queue, and (from B2 on) a name / visibility flag for the
/// Layers panel. All layers share domain, resolution, origin, and the
/// same [`WorkpieceRoot`] parent — see the module doc.
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub grid: Grid,
    /// Chunk coord → spawned entity, for chunks that currently have
    /// an active mesh. Sparse: a coord with no geometry has no
    /// entry, not an entity with an empty mesh. See the module-level
    /// Track A2 note.
    chunks: HashMap<(u32, u32, u32), Entity>,
    dirty: HashSet<(u32, u32, u32)>,
    /// Material every chunk entity in this layer is spawned with.
    /// Cached here so `remesh_dirty_chunks` can spawn newly-occupied
    /// chunks without a separate materials write pass. Shared across
    /// layers — one matcap look for the whole stack.
    material: Handle<MatcapMaterial>,
}

/// Detached copy of a layer's durable state (id / name / visibility /
/// material / SDF). Used by undo for Delete Layer and Merge Down so
/// the layer can be restored without keeping live chunk entities.
#[derive(Clone)]
pub struct LayerSnapshot {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub material: Handle<MatcapMaterial>,
    pub grid: Grid,
}

impl LayerSnapshot {
    fn from_layer(layer: &Layer) -> Self {
        Self {
            id: layer.id,
            name: layer.name.clone(),
            visible: layer.visible,
            material: layer.material.clone(),
            grid: layer.grid.clone(),
        }
    }
}

impl Layer {
    /// Mark a chunk coord dirty on *this* layer specifically,
    /// regardless of which layer is active. Used by undo/redo, which
    /// applies an entry to the layer it was recorded against.
    #[inline]
    pub fn mark_dirty(&mut self, key: (u32, u32, u32)) {
        self.dirty.insert(key);
    }

    fn new(id: LayerId, name: impl Into<String>, grid: Grid, material: Handle<MatcapMaterial>) -> Self {
        let num_chunks = grid.num_chunks();
        let mut dirty = HashSet::new();
        for cz in 0..num_chunks.z {
            for cy in 0..num_chunks.y {
                for cx in 0..num_chunks.x {
                    dirty.insert((cx, cy, cz));
                }
            }
        }
        Self {
            id,
            name: name.into(),
            visible: true,
            grid,
            chunks: HashMap::new(),
            dirty,
            material,
        }
    }
}

/// Every sculptable layer plus which one is active. Tools, selection,
/// undo, and save/load all read and write through
/// [`grid`](Self::grid) / [`grid_mut`](Self::grid_mut) /
/// [`mark_dirty`](Self::mark_dirty) rather than reaching into a
/// specific layer directly, so none of them need to change when B2
/// adds a second layer.
#[derive(Resource)]
pub struct LayersState {
    layers: Vec<Layer>,
    active: usize,
    next_id: LayerId,
    /// Chunk entities belonging to layers that were just removed
    /// (delete / merge). Drained and despawned at the start of
    /// [`remesh_dirty_chunks`] so structural edits don't need their
    /// own `Commands` borrow tangled into every caller.
    pending_despawn: Vec<Entity>,
}

impl LayersState {
    /// The active layer's grid. Every sculpt tool reads through this.
    #[inline]
    pub fn grid(&self) -> &Grid {
        &self.layers[self.active].grid
    }

    /// The active layer's grid, mutably. Every sculpt tool writes
    /// through this.
    #[inline]
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.layers[self.active].grid
    }

    /// Mark a chunk coord dirty on the *active* layer, so the mesher
    /// re-extracts it (bounded by [`MAX_CHUNKS_PER_FRAME`] per
    /// frame). The one-line replacement for the old bare
    /// `workpiece.dirty.insert(key)` — every sculpt tool already goes
    /// through this after touching `grid_mut()`.
    #[inline]
    pub fn mark_dirty(&mut self, key: (u32, u32, u32)) {
        self.layers[self.active].dirty.insert(key);
    }

    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active]
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn layer_mut(&mut self, idx: usize) -> Option<&mut Layer> {
        self.layers.get_mut(idx)
    }

    pub fn active_id(&self) -> LayerId {
        self.layers[self.active].id
    }

    /// The active layer's chunk material, cloned so a new layer can
    /// be spawned with a visually-matching colour without reaching
    /// into `Layer`'s private field.
    pub fn active_material(&self) -> Handle<MatcapMaterial> {
        self.layers[self.active].material.clone()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Switch the active layer by index. No-op if out of range.
    pub fn set_active_index(&mut self, idx: usize) {
        if idx < self.layers.len() {
            self.active = idx;
        }
    }

    /// Create a new layer (starting from `grid`), make it active, and
    /// return its stable id. Used by Insert Primitive (`PLAN.md`
    /// Track B2 — "Insert Primitive → always a new layer" so a fresh
    /// primitive can never silently fuse with existing material;
    /// fusion only happens via an explicit Merge Down, B3).
    pub fn push_new_layer(
        &mut self,
        grid: Grid,
        material: Handle<MatcapMaterial>,
        name: impl Into<String>,
    ) -> LayerId {
        let id = self.next_id;
        self.next_id += 1;
        self.layers.push(Layer::new(id, name, grid, material));
        self.active = self.layers.len() - 1;
        id
    }

    /// Ray-march every *visible* layer and report the one with the
    /// closest hit (index into `layers`, plus the hit point), or
    /// `None` if the ray misses every visible layer's surface.
    /// Lets a click reach — and, via [`set_active_index`], activate —
    /// whatever the user is actually pointing at, not just whichever
    /// layer happens to be active already.
    pub fn ray_march_visible(&self, origin: GVec3, dir: GVec3, max_dist: f32) -> Option<(usize, GVec3)> {
        let mut best: Option<(usize, GVec3, f32)> = None;
        for (i, layer) in self.layers.iter().enumerate() {
            if !layer.visible {
                continue;
            }
            if let Some(hit) = layer.grid.ray_march(origin, dir, max_dist) {
                let d = (hit - origin).length();
                let closer = best.as_ref().map(|&(_, _, bd)| d < bd).unwrap_or(true);
                if closer {
                    best = Some((i, hit, d));
                }
            }
        }
        best.map(|(i, hit, _)| (i, hit))
    }

    /// Flatten every *visible* layer's material into one grid via
    /// per-voxel min-union. Used for STL export ("what I see is what
    /// I print"). `.mudclay` v3 saves layers intact; this is no
    /// longer on the save path.
    pub fn visible_union_grid(&self) -> Grid {
        let domain = &self.layers[self.active].grid;
        let mut out = Grid::empty(domain.res(), domain.voxel_size(), domain.origin());
        for layer in &self.layers {
            if layer.visible {
                out.union_from(&layer.grid);
            }
        }
        out
    }

    /// Replace the entire layer stack from a loaded `.mudclay` scene.
    /// Queues every live chunk entity for despawn, then rebuilds
    /// layers from `layers` (id / name / visible / grid). Requires the
    /// incoming domain to match the current one (same restriction as
    /// [`Self::swap_active_grid`]).
    pub fn replace_from_scene(
        &mut self,
        layers: Vec<(LayerId, String, bool, Grid)>,
        active: usize,
    ) -> Result<(), GridSwapError> {
        if layers.is_empty() {
            return Err(GridSwapError::EmptyScene);
        }
        if active >= layers.len() {
            return Err(GridSwapError::EmptyScene);
        }
        let material = self.layers[self.active].material.clone();
        let current = &self.layers[self.active].grid;
        let res = layers[0].3.res();
        let voxel_size = layers[0].3.voxel_size();
        let origin = layers[0].3.origin();
        if res != current.res() {
            return Err(GridSwapError::ResolutionMismatch {
                current: current.res(),
                incoming: res,
            });
        }
        if (voxel_size - current.voxel_size()).abs() > 1e-4 {
            return Err(GridSwapError::VoxelSizeMismatch {
                current: current.voxel_size(),
                incoming: voxel_size,
            });
        }
        for (_, _, _, grid) in &layers {
            if grid.res() != res
                || (grid.voxel_size() - voxel_size).abs() > 1e-4
                || grid.origin() != origin
            {
                return Err(GridSwapError::ResolutionMismatch {
                    current: res,
                    incoming: grid.res(),
                });
            }
        }

        for mut layer in self.layers.drain(..) {
            for (_, entity) in layer.chunks.drain() {
                self.pending_despawn.push(entity);
            }
        }

        let mut next_id = 1u32;
        for (id, name, visible, grid) in layers {
            let mut layer = Layer {
                id,
                name,
                visible,
                grid,
                chunks: HashMap::new(),
                dirty: HashSet::new(),
                material: material.clone(),
            };
            for coord in layer.grid.allocated_chunk_coords() {
                layer.dirty.insert((coord.x, coord.y, coord.z));
            }
            next_id = next_id.max(id.saturating_add(1));
            self.layers.push(layer);
        }
        self.active = active;
        self.next_id = next_id;
        Ok(())
    }

    /// Mutable access to a specific layer by its stable id,
    /// regardless of which one is active. Used by undo/redo, which
    /// must apply an entry to the layer it was recorded against even
    /// if the user has since switched away from it.
    pub fn layer_by_id_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// Mark every allocated chunk on every visible layer dirty so the
    /// next remesh pass rebuilds geometry (e.g. after switching
    /// [`MesherKind`] in Preferences).
    pub fn redirty_all_meshes(&mut self) {
        for layer in self.layers.iter_mut() {
            if !layer.visible {
                continue;
            }
            for c in layer.grid.allocated_chunk_coords() {
                layer.dirty.insert((c.x, c.y, c.z));
            }
        }
    }

    /// Flip a layer's visibility. Hidden layers skip remesh / pick /
    /// export flatten. Returns `false` if `idx` is out of range.
    ///
    /// Showing a layer again marks every allocated chunk dirty so
    /// [`remesh_dirty_chunks`] can respawn meshes that were despawned
    /// while the layer was hidden.
    pub fn set_visible(&mut self, idx: usize, visible: bool) -> bool {
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if layer.visible == visible {
            return true;
        }
        layer.visible = visible;
        if visible {
            for c in layer.grid.allocated_chunk_coords() {
                layer.dirty.insert((c.x, c.y, c.z));
            }
        }
        true
    }

    /// Rename a layer. Empty / whitespace-only names are rejected so
    /// the panel never shows a blank row. Returns `false` if `idx`
    /// is out of range or the name is empty after trim.
    pub fn rename_layer(&mut self, idx: usize, name: impl Into<String>) -> bool {
        let name = name.into();
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return false;
        }
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        layer.name = trimmed.to_string();
        true
    }

    /// Remove a layer by index. Refuses to delete the last remaining
    /// layer (clear the worktable instead). Returns a snapshot for
    /// undo, plus the active index from before the delete. Live chunk
    /// entities are queued on [`Self::pending_despawn`].
    pub fn delete_layer(&mut self, idx: usize) -> Option<(LayerSnapshot, usize)> {
        if self.layers.len() <= 1 || idx >= self.layers.len() {
            return None;
        }
        let previous_active = self.active;
        let snapshot = LayerSnapshot::from_layer(&self.layers[idx]);
        self.remove_layer_at(idx);
        Some((snapshot, previous_active))
    }

    /// Drop a layer without snapshotting (redo of Delete / Merge).
    /// Same last-layer guard as [`Self::delete_layer`].
    pub fn drop_layer_at(&mut self, idx: usize) -> bool {
        if self.layers.len() <= 1 || idx >= self.layers.len() {
            return false;
        }
        self.remove_layer_at(idx);
        true
    }

    /// Photoshop-style Merge Down: union the active layer into the
    /// layer immediately below it (`active - 1`), then drop the
    /// source. Returns `None` when the active layer is already the
    /// bottom of the stack. The returned [`MergeDownRecord`] journals
    /// both the dest voxel delta and a full source snapshot so undo
    /// can restore the pre-merge scene.
    pub fn merge_down_active(&mut self) -> Option<MergeDownRecord> {
        let src_index = self.active;
        if src_index == 0 || src_index >= self.layers.len() {
            return None;
        }
        let dst_index = src_index - 1;
        let src_snapshot = LayerSnapshot::from_layer(&self.layers[src_index]);
        let dst_id = self.layers[dst_index].id;
        let src_grid = self.layers[src_index].grid.clone();

        // Capture only voxels where the source actually wins the min
        // — that's the sparse delta undo needs to reverse.
        let mut voxels = Vec::new();
        let mut pre = Vec::new();
        let mut dirty_chunks = HashSet::new();
        for coord in src_grid.allocated_chunk_coords() {
            dirty_chunks.insert((coord.x, coord.y, coord.z));
            let base = coord.voxel_min();
            let max = coord.voxel_max(src_grid.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        let s = src_grid.get(ix, iy, iz);
                        let d = self.layers[dst_index].grid.get(ix, iy, iz);
                        if s < d {
                            voxels.push((ix, iy, iz));
                            pre.push(d);
                        }
                    }
                }
            }
        }

        self.layers[dst_index].grid.union_from(&src_grid);
        let post: Vec<f32> = voxels
            .iter()
            .map(|&(x, y, z)| self.layers[dst_index].grid.get(x, y, z))
            .collect();
        for key in &dirty_chunks {
            self.layers[dst_index].dirty.insert(*key);
        }

        // Drop the source (entities → pending_despawn) and activate
        // the destination — the merged result is what the user now
        // wants to sculpt.
        self.remove_layer_at(src_index);
        self.active = dst_index;

        Some(MergeDownRecord {
            src_index,
            dst_index,
            src: src_snapshot,
            dst_id,
            voxels,
            pre,
            post,
            dirty_chunks: dirty_chunks.into_iter().collect(),
        })
    }

    /// Re-insert a previously-removed layer at `idx` (used by undo of
    /// Delete / Merge Down). Chunk entities start empty; allocated
    /// tiles are marked dirty so remesh respawns them.
    pub fn restore_layer_at(&mut self, idx: usize, snap: LayerSnapshot) {
        let idx = idx.min(self.layers.len());
        let mut layer = Layer {
            id: snap.id,
            name: snap.name,
            visible: snap.visible,
            grid: snap.grid,
            chunks: HashMap::new(),
            dirty: HashSet::new(),
            material: snap.material,
        };
        for coord in layer.grid.allocated_chunk_coords() {
            layer.dirty.insert((coord.x, coord.y, coord.z));
        }
        self.next_id = self.next_id.max(snap.id.saturating_add(1));
        self.layers.insert(idx, layer);
        if self.active >= idx {
            // Indices at/after the insert shift up; keep pointing at
            // the same logical layer unless the caller overrides.
            self.active += 1;
        }
    }

    /// Reset to a single empty layer (File → New). Despawns every
    /// other layer's chunks and clears the survivor's SDF.
    pub fn reset_to_empty_single_layer(&mut self) {
        while self.layers.len() > 1 {
            self.remove_layer_at(self.layers.len() - 1);
        }
        self.active = 0;
        let empty = Grid::empty(
            self.layers[0].grid.res(),
            self.layers[0].grid.voxel_size(),
            self.layers[0].grid.origin(),
        );
        let _ = self.swap_active_grid(empty);
        self.layers[0].name = "Layer 1".into();
        self.layers[0].visible = true;
    }

    /// Remove layer `idx`, queue its chunk entities for despawn, and
    /// clamp `active` onto a surviving neighbour.
    fn remove_layer_at(&mut self, idx: usize) {
        let mut layer = self.layers.remove(idx);
        for (_, entity) in layer.chunks.drain() {
            self.pending_despawn.push(entity);
        }
        if self.layers.is_empty() {
            // Should be unreachable — callers refuse to delete the
            // last layer — but keep `active` sane if it ever happens.
            self.active = 0;
            return;
        }
        if self.active > idx {
            self.active -= 1;
        } else if self.active == idx {
            self.active = idx.saturating_sub(1).min(self.layers.len() - 1);
        } else if self.active >= self.layers.len() {
            self.active = self.layers.len() - 1;
        }
    }

    /// Replace the *active* layer's SDF grid, e.g. after loading a
    /// project file or clearing the worktable. Fails if the new
    /// grid's dimensions don't match the existing chunk layout, since
    /// re-tiling would require spawning and despawning entities from
    /// a system that doesn't own `Commands`. Callers should check the
    /// current `grid().res()` + `voxel_size()` before offering the
    /// swap.
    ///
    /// Marks *every possible* chunk coord on this layer dirty — not
    /// just the ones with a currently-spawned entity — so the mesher
    /// both re-checks chunks that might now be empty (and despawns
    /// them) and discovers chunks that might now have geometry for
    /// the first time (and spawns them). Rebuilds over the next few
    /// frames (bounded by [`MAX_CHUNKS_PER_FRAME`]).
    pub fn swap_active_grid(&mut self, new_grid: Grid) -> Result<(), GridSwapError> {
        let layer = &mut self.layers[self.active];
        if new_grid.res() != layer.grid.res() {
            return Err(GridSwapError::ResolutionMismatch {
                current: layer.grid.res(),
                incoming: new_grid.res(),
            });
        }
        if (new_grid.voxel_size() - layer.grid.voxel_size()).abs() > 1e-4 {
            return Err(GridSwapError::VoxelSizeMismatch {
                current: layer.grid.voxel_size(),
                incoming: new_grid.voxel_size(),
            });
        }
        let num_chunks = new_grid.num_chunks();
        layer.grid = new_grid;
        layer.dirty.clear();
        for cz in 0..num_chunks.z {
            for cy in 0..num_chunks.y {
                for cx in 0..num_chunks.x {
                    layer.dirty.insert((cx, cy, cz));
                }
            }
        }
        Ok(())
    }
}

/// Journal payload for Merge Down — built by
/// [`LayersState::merge_down_active`] so the voxel walk stays next
/// to `union_from`, then stored in [`crate::undo`].
pub struct MergeDownRecord {
    pub src_index: usize,
    pub dst_index: usize,
    pub src: LayerSnapshot,
    pub dst_id: LayerId,
    pub voxels: Vec<(u32, u32, u32)>,
    pub pre: Vec<f32>,
    pub post: Vec<f32>,
    pub dirty_chunks: Vec<(u32, u32, u32)>,
}

#[cfg(test)]
impl LayersState {
    /// Construct a single-layer state directly from a `Grid`, for
    /// tests that need to exercise `LayersState`-consuming logic
    /// (e.g. `move_tool::apply_move`) without spinning up a full
    /// Bevy `App`. `Handle::default()` is a fine stand-in material —
    /// nothing under test renders anything.
    pub fn new_for_test(grid: Grid) -> Self {
        Self {
            layers: vec![Layer::new(0, "Layer 1", grid, Handle::default())],
            active: 0,
            next_id: 1,
            pending_despawn: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum GridSwapError {
    ResolutionMismatch { current: UVec3, incoming: UVec3 },
    VoxelSizeMismatch { current: f32, incoming: f32 },
    /// Load / replace refused an empty layer list (or a bad active index).
    EmptyScene,
}

impl std::fmt::Display for GridSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GridSwapError::ResolutionMismatch { current, incoming } => write!(
                f,
                "grid resolution mismatch: current {}x{}x{}, incoming {}x{}x{}",
                current.x, current.y, current.z, incoming.x, incoming.y, incoming.z,
            ),
            GridSwapError::VoxelSizeMismatch { current, incoming } => write!(
                f,
                "voxel size mismatch: current {current} mm, incoming {incoming} mm",
            ),
            GridSwapError::EmptyScene => {
                write!(f, "invalid project scene (no layers or bad active index)")
            }
        }
    }
}

pub fn plugin(app: &mut App) {
    app.add_systems(
        Startup,
        spawn_workpiece.after(crate::matcap::setup_matcaps),
    );
    app.add_systems(Update, (handle_layer_actions, remesh_dirty_chunks).chain());
}

/// Layers panel actions (Track B3): activate, visibility, rename,
/// delete, Merge Down. Structural edits discard any in-flight stroke
/// and push a journal entry so Ctrl+Z restores the pre-op scene.
fn handle_layer_actions(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<LayersState>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut selection: ResMut<Selection>,
) {
    for action in events.read() {
        match action {
            AppAction::SetActiveLayer(idx) => {
                let before = workpiece.active_index();
                workpiece.set_active_index(*idx);
                if workpiece.active_index() != before {
                    selection.picked_voxel = None;
                    selection.invalidate_labels();
                    stroke.discard_live();
                }
            }
            AppAction::SetLayerVisible(idx, visible) => {
                if workpiece.set_visible(*idx, *visible) {
                    selection.invalidate_labels();
                }
            }
            AppAction::DeleteLayer(idx) => {
                if let Some((snapshot, previous_active)) = workpiece.delete_layer(*idx) {
                    stroke.discard_live();
                    history.push_delete_layer(DeleteLayerEntry {
                        index: *idx,
                        previous_active,
                        layer: snapshot,
                    });
                    selection.picked_voxel = None;
                    selection.invalidate_labels();
                    info!(
                        "deleted layer {} ({} remaining)",
                        idx,
                        workpiece.layer_count()
                    );
                }
            }
            AppAction::MergeDown => {
                if let Some(record) = workpiece.merge_down_active() {
                    stroke.discard_live();
                    history.push_merge_down(record);
                    selection.picked_voxel = None;
                    selection.invalidate_labels();
                    info!(
                        "merged down → {} layer(s), active '{}'",
                        workpiece.layer_count(),
                        workpiece.active_layer().name
                    );
                }
            }
            _ => {}
        }
    }
}

fn spawn_workpiece(mut commands: Commands, matcaps: Res<MatcapState>) {
    // Grid is centered on X/Z. Y=0 is the workbench, so the grid's Y
    // origin is also 0 — the piece rests on the workbench.
    let extent = RES as f32 * VOXEL_MM;
    let origin = GVec3::new(-extent * 0.5, 0.0, -extent * 0.5);
    let grid = Grid::from_sphere(
        UVec3::splat(RES),
        VOXEL_MM,
        origin,
        // Starter primitive: ball centered above the workbench.
        GVec3::new(0.0, 45.0, 0.0),
        40.0,
    );

    commands.spawn((WorkpieceRoot, Transform::default(), Visibility::default()));

    // No chunk entities spawned up front — every chunk coord starts
    // dirty and `remesh_dirty_chunks` spawns an entity only for the
    // ones that turn out to have geometry (see the module doc).
    let layer = Layer::new(0, "Layer 1", grid, matcaps.material.clone());

    commands.insert_resource(LayersState {
        layers: vec![layer],
        active: 0,
        next_id: 1,
        pending_despawn: Vec::new(),
    });
}

fn build_mesh(extracted: sculpt_core::ExtractedMesh) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    let uvs: Vec<[f32; 2]> = extracted.cavity.iter().map(|&c| [c, 0.0]).collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, extracted.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, extracted.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(extracted.indices));
    mesh
}

/// Re-extract every dirty chunk across every layer (bounded by
/// [`MAX_CHUNKS_PER_FRAME`] *per layer*, per frame) and reconcile its
/// entity against the result:
/// - geometry appears where there was none → spawn a chunk entity;
/// - geometry disappears → despawn it;
/// - geometry changes shape → update the existing mesh in place.
/// - a hidden layer's chunks are despawned wholesale and skipped
///   while hidden, so an invisible layer costs nothing to render.
///   [`LayersState::set_visible`] re-dirties allocated chunks when a
///   layer is shown again so those meshes respawn without needing a
///   sculpt edit.
///
/// A chunk that goes dirty again later (any edit touching it calls
/// `DirtyRegion::touched_chunks`, independent of whether it currently
/// has an entity) is picked back up here the same way, so a despawned
/// chunk respawns correctly the next time it gets material.
fn remesh_dirty_chunks(
    mut state: ResMut<LayersState>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    q_meshes: Query<&Mesh3d>,
    q_root: Query<Entity, With<WorkpieceRoot>>,
    settings: Res<AppSettings>,
) {
    // Structural edits (delete / merge) queue orphaned chunk entities
    // here so they don't leak under WorkpieceRoot.
    for entity in state.pending_despawn.drain(..) {
        commands.entity(entity).despawn();
    }

    for layer in state.layers.iter_mut() {
        if !layer.visible {
            // Hidden: drop every rendered chunk. Showing the layer
            // again re-dirties allocated coords in `set_visible`.
            if !layer.chunks.is_empty() {
                for (_, entity) in layer.chunks.drain() {
                    commands.entity(entity).despawn();
                }
            }
            continue;
        }
        if layer.dirty.is_empty() {
            continue;
        }

        // Snapshot the set of keys we'll process this frame.
        let batch: Vec<_> = layer
            .dirty
            .iter()
            .cloned()
            .take(MAX_CHUNKS_PER_FRAME)
            .collect();

        for key in &batch {
            layer.dirty.remove(key);
        }

        for key in batch {
            let coord = ChunkCoord::new(key.0, key.1, key.2);
            let extracted = extract_chunk_with(&layer.grid, coord, settings.mesher);

            if extracted.is_empty() {
                if let Some(entity) = layer.chunks.remove(&key) {
                    commands.entity(entity).despawn();
                }
                continue;
            }

            if let Some(&entity) = layer.chunks.get(&key) {
                // Existing chunk, geometry changed shape: update in place.
                if let Ok(mesh3d) = q_meshes.get(entity) {
                    meshes.insert(mesh3d.0.id(), build_mesh(extracted));
                }
            } else {
                // Newly-occupied chunk: spawn it under the workpiece root.
                let Ok(root) = q_root.get_single() else {
                    continue;
                };
                let mesh_handle = meshes.add(build_mesh(extracted));
                let entity = commands
                    .spawn((
                        Mesh3d(mesh_handle),
                        MeshMaterial3d(layer.material.clone()),
                        Transform::default(),
                        Visibility::default(),
                        ChunkEntity { coord },
                    ))
                    .id();
                commands.entity(root).add_child(entity);
                layer.chunks.insert(key, entity);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3 as GVec3;

    fn sphere(centre: GVec3) -> Grid {
        Grid::from_sphere(UVec3::new(64, 64, 64), 1.0, GVec3::ZERO, centre, 6.0)
    }

    /// End-to-end test of Track B2's "Insert Primitive → new layer"
    /// plus the interim save/export path ("PLAN.md" Track B2/B4):
    /// two layers must both survive `visible_union_grid`, and a
    /// hidden layer must not contribute to it.
    #[test]
    fn push_new_layer_keeps_both_layers_and_union_preserves_both() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        assert_eq!(state.layer_count(), 1);
        assert_eq!(state.active_index(), 0);

        let second = sphere(GVec3::new(48.0, 48.0, 48.0));
        let id = state.push_new_layer(second, Handle::default(), "Layer 2");
        assert_eq!(state.layer_count(), 2);
        // Inserting always activates the new layer.
        assert_eq!(state.active_index(), 1);
        assert_eq!(state.active_id(), id);

        // The active layer's own grid only has the second sphere.
        assert!(state.grid().sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0);
        assert!(state.grid().sample(GVec3::new(16.0, 16.0, 16.0)) > 0.0);

        // The flattened union has both.
        let flattened = state.visible_union_grid();
        assert!(flattened.sample(GVec3::new(16.0, 16.0, 16.0)) < 0.0, "first layer's sphere must survive the union");
        assert!(flattened.sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0, "second layer's sphere must survive the union");
    }

    #[test]
    fn hidden_layer_is_excluded_from_visible_union() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        state.push_new_layer(sphere(GVec3::new(48.0, 48.0, 48.0)), Handle::default(), "Layer 2");
        state.layers[0].visible = false;

        let flattened = state.visible_union_grid();
        assert!(
            flattened.sample(GVec3::new(16.0, 16.0, 16.0)) > 0.0,
            "hidden layer's material must not appear in the union"
        );
        assert!(flattened.sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0);
    }

    #[test]
    fn ray_march_visible_finds_the_closest_hit_across_layers() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 32.0, 32.0)));
        state.push_new_layer(sphere(GVec3::new(48.0, 32.0, 32.0)), Handle::default(), "Layer 2");
        // Active layer is now index 1 (the far sphere). A ray from the
        // -X side should hit the *near* sphere (layer 0) first, even
        // though it isn't active.
        let hit = state.ray_march_visible(GVec3::new(-10.0, 32.0, 32.0), GVec3::X, 200.0);
        let (idx, point) = hit.expect("ray should hit the near sphere");
        assert_eq!(idx, 0, "closest hit should be the near sphere's layer, not the active one");
        assert!((point.x - 10.0).abs() < 1.5, "hit should land on the near sphere, x~10, got {point:?}");
    }

    #[test]
    fn ray_march_visible_skips_hidden_layers() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 32.0, 32.0)));
        state.push_new_layer(sphere(GVec3::new(48.0, 32.0, 32.0)), Handle::default(), "Layer 2");
        state.layers[0].visible = false;
        let hit = state.ray_march_visible(GVec3::new(-10.0, 32.0, 32.0), GVec3::X, 200.0);
        let (idx, _) = hit.expect("ray should hit the far (visible) sphere");
        assert_eq!(idx, 1, "hidden layer must be skipped even though it's physically closer");
    }

    #[test]
    fn merge_down_unions_into_below_and_drops_source() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        state.push_new_layer(sphere(GVec3::new(48.0, 48.0, 48.0)), Handle::default(), "Layer 2");
        assert_eq!(state.active_index(), 1);

        let record = state
            .merge_down_active()
            .expect("active layer 1 should merge into layer 0");
        assert_eq!(record.src_index, 1);
        assert_eq!(record.dst_index, 0);
        assert_eq!(state.layer_count(), 1);
        assert_eq!(state.active_index(), 0);
        // Both spheres now live on the surviving layer.
        assert!(state.grid().sample(GVec3::new(16.0, 16.0, 16.0)) < 0.0);
        assert!(state.grid().sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0);
    }

    #[test]
    fn merge_down_on_bottom_layer_is_noop() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        assert!(state.merge_down_active().is_none());
        assert_eq!(state.layer_count(), 1);
    }

    #[test]
    fn delete_layer_refuses_last_and_restores_via_snapshot() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        assert!(state.delete_layer(0).is_none(), "cannot delete the last layer");

        state.push_new_layer(sphere(GVec3::new(48.0, 48.0, 48.0)), Handle::default(), "Layer 2");
        let (snap, prev_active) = state.delete_layer(1).expect("second layer is deletable");
        assert_eq!(prev_active, 1);
        assert_eq!(state.layer_count(), 1);
        assert_eq!(snap.name, "Layer 2");
        assert!(snap.grid.sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0);

        state.restore_layer_at(1, snap);
        state.set_active_index(prev_active);
        assert_eq!(state.layer_count(), 2);
        assert_eq!(state.active_index(), 1);
        assert!(state.grid().sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0);
    }

    #[test]
    fn reset_to_empty_single_layer_drops_extras() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        state.push_new_layer(sphere(GVec3::new(48.0, 48.0, 48.0)), Handle::default(), "Layer 2");
        state.reset_to_empty_single_layer();
        assert_eq!(state.layer_count(), 1);
        assert_eq!(state.active_index(), 0);
        assert_eq!(state.active_layer().name, "Layer 1");
        assert!(
            state.grid().allocated_tile_count() == 0,
            "cleared worktable must be empty"
        );
    }

    #[test]
    fn rename_and_visibility_helpers() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        assert!(state.rename_layer(0, "Base"));
        assert_eq!(state.active_layer().name, "Base");
        assert!(!state.rename_layer(0, "   "), "whitespace-only names rejected");
        assert!(state.set_visible(0, false));
        assert!(!state.layers()[0].visible);
    }

    #[test]
    fn showing_a_hidden_layer_redirties_allocated_chunks() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        // Simulate a settled remesh: no dirty keys, but the layer still
        // has voxel data in allocated tiles.
        state.layers[0].dirty.clear();
        assert!(state.set_visible(0, false));
        assert!(
            state.layers[0].dirty.is_empty(),
            "hiding must not invent dirty work"
        );
        assert!(state.set_visible(0, true));
        let allocated = state.layers[0].grid.allocated_chunk_coords().len();
        assert!(allocated > 0, "test sphere must allocate tiles");
        assert_eq!(
            state.layers[0].dirty.len(),
            allocated,
            "showing again must dirty every allocated chunk so remesh respawns meshes"
        );
    }

    #[test]
    fn replace_from_scene_restores_multi_layer_stack() {
        let mut state = LayersState::new_for_test(sphere(GVec3::new(16.0, 16.0, 16.0)));
        state.push_new_layer(sphere(GVec3::new(48.0, 48.0, 48.0)), Handle::default(), "L2");
        state.set_visible(0, false);

        let layers: Vec<_> = state
            .layers()
            .iter()
            .map(|l| (l.id, l.name.clone(), l.visible, l.grid.clone()))
            .collect();
        let active = state.active_index();

        // Blow away and reload.
        let mut fresh = LayersState::new_for_test(Grid::empty(
            UVec3::new(64, 64, 64),
            1.0,
            GVec3::ZERO,
        ));
        fresh
            .replace_from_scene(layers, active)
            .expect("same domain");
        assert_eq!(fresh.layer_count(), 2);
        assert_eq!(fresh.active_index(), 1);
        assert!(!fresh.layers()[0].visible);
        assert_eq!(fresh.layers()[1].name, "L2");
        assert!(fresh.grid().sample(GVec3::new(48.0, 48.0, 48.0)) < 0.0);
    }
}
