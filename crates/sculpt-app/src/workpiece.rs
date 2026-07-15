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

use sculpt_core::{extract_chunk, ChunkCoord, Grid};

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
    // Read by the Layers panel (Track B3, not yet built).
    #[allow(dead_code)]
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
    /// chunks without a separate `Assets<StandardMaterial>` write
    /// pass tangled into chunk lookup. Shared across layers for now
    /// (no per-layer colour yet — not asked for).
    material: Handle<StandardMaterial>,
}

impl Layer {
    /// Mark a chunk coord dirty on *this* layer specifically,
    /// regardless of which layer is active. Used by undo/redo, which
    /// applies an entry to the layer it was recorded against.
    #[inline]
    pub fn mark_dirty(&mut self, key: (u32, u32, u32)) {
        self.dirty.insert(key);
    }

    fn new(id: LayerId, name: impl Into<String>, grid: Grid, material: Handle<StandardMaterial>) -> Self {
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

    // Used by the Layers panel (Track B3, not yet built) for
    // visibility toggles / rename / delete on the active layer.
    #[allow(dead_code)]
    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active]
    }

    #[allow(dead_code)]
    pub fn active_layer_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.active]
    }

    pub fn active_id(&self) -> LayerId {
        self.layers[self.active].id
    }

    /// The active layer's chunk material, cloned so a new layer can
    /// be spawned with a visually-matching colour without reaching
    /// into `Layer`'s private field.
    pub fn active_material(&self) -> Handle<StandardMaterial> {
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
        material: Handle<StandardMaterial>,
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
    /// per-voxel min-union. Used for STL export and `.mudclay` save
    /// until the v3 multi-layer format lands (Track B4): today's
    /// single-grid formats have no way to represent more than one
    /// layer, so exporting/saving must not silently drop whichever
    /// ones aren't active.
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

    /// Mutable access to a specific layer by its stable id,
    /// regardless of which one is active. Used by undo/redo, which
    /// must apply an entry to the layer it was recorded against even
    /// if the user has since switched away from it.
    pub fn layer_by_id_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
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
        }
    }
}

#[derive(Debug)]
pub enum GridSwapError {
    ResolutionMismatch { current: UVec3, incoming: UVec3 },
    VoxelSizeMismatch { current: f32, incoming: f32 },
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
        }
    }
}

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, spawn_workpiece);
    app.add_systems(Update, remesh_dirty_chunks);
}

fn spawn_workpiece(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
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

    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.78, 0.55, 0.42),
        perceptual_roughness: 0.85,
        metallic: 0.0,
        ..default()
    });

    commands.spawn((WorkpieceRoot, Transform::default(), Visibility::default()));

    // No chunk entities spawned up front — every chunk coord starts
    // dirty and `remesh_dirty_chunks` spawns an entity only for the
    // ones that turn out to have geometry (see the module doc).
    let layer = Layer::new(0, "Layer 1", grid, material);

    commands.insert_resource(LayersState {
        layers: vec![layer],
        active: 0,
        next_id: 1,
    });
}

fn build_mesh(extracted: sculpt_core::ExtractedMesh) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, extracted.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, extracted.normals);
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
) {
    for layer in state.layers.iter_mut() {
        if !layer.visible {
            // Hidden: drop every rendered chunk (cheap — they'll
            // respawn correctly if the layer becomes visible again,
            // since nothing here touches `dirty`).
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
            let extracted = extract_chunk(&layer.grid, coord);

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
}
