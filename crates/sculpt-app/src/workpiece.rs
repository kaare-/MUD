//! The sculpting subject.
//!
//! The workpiece owns:
//! - a dense SDF grid (source of truth),
//! - a set of chunk entities (Bevy renderables extracted from the grid),
//! - a dirty-chunk queue.
//!
//! One `WorkpieceRoot` entity carries the piece-local transform (the
//! turntable rotation is applied here). Each chunk is a child entity
//! whose mesh vertices are already in piece-local space, so its own
//! transform stays identity.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use glam::{UVec3, Vec3 as GVec3};

use sculpt_core::{extract_chunk, ChunkCoord, Grid, CHUNK_SIZE};

/// Effective grid resolution per axis. 192^3 at 1.5 mm/voxel gives a
/// **288 mm domain** — big enough for a chunky two-handed piece,
/// still cheap enough that Move-drag preview stays interactive.
///
/// Memory footprint at 192³: grid 27 MB (f32), snapshot 27 MB
/// during a drag, component labels 27 MB when live. Full re-mesh
/// takes ~120 ms (bounded by our 32-chunks-per-frame cap for
/// smoothness during large edits).
///
/// The Stage-2 sparse-SDF migration in `PLAN.md` is still the
/// answer for going much larger; this bump is what we can afford
/// on a dense grid without hurting interactivity.
const RES: u32 = 192;
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

#[derive(Resource)]
pub struct SculptWorkpiece {
    pub grid: Grid,
    pub chunks: HashMap<(u32, u32, u32), Entity>,
    pub dirty: HashSet<(u32, u32, u32)>,
}

impl SculptWorkpiece {
    /// Replace the workpiece's SDF grid, e.g. after loading a project
    /// file. Fails if the new grid's dimensions don't match the
    /// existing chunk layout, since re-tiling would require spawning
    /// and despawning entities from a system that doesn't own
    /// Commands. Callers should check the current `grid.res()` +
    /// `voxel_size()` before offering the swap.
    ///
    /// Marks every chunk dirty so the mesher rebuilds the whole
    /// workpiece over the next few frames (bounded by
    /// [`MAX_CHUNKS_PER_FRAME`]).
    pub fn swap_grid(&mut self, new_grid: Grid) -> Result<(), GridSwapError> {
        if new_grid.res() != self.grid.res() {
            return Err(GridSwapError::ResolutionMismatch {
                current: self.grid.res(),
                incoming: new_grid.res(),
            });
        }
        if (new_grid.voxel_size() - self.grid.voxel_size()).abs() > 1e-4 {
            return Err(GridSwapError::VoxelSizeMismatch {
                current: self.grid.voxel_size(),
                incoming: new_grid.voxel_size(),
            });
        }
        self.grid = new_grid;
        self.dirty.clear();
        for &key in self.chunks.keys() {
            self.dirty.insert(key);
        }
        Ok(())
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
    mut meshes: ResMut<Assets<Mesh>>,
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

    let root = commands
        .spawn((WorkpieceRoot, Transform::default(), Visibility::default()))
        .id();

    let num_chunks = RES.div_ceil(CHUNK_SIZE);
    let mut chunks = HashMap::new();
    let mut dirty = HashSet::new();

    for cz in 0..num_chunks {
        for cy in 0..num_chunks {
            for cx in 0..num_chunks {
                let key = (cx, cy, cz);
                let mesh_handle = meshes.add(empty_mesh());
                let entity = commands
                    .spawn((
                        Mesh3d(mesh_handle),
                        MeshMaterial3d(material.clone()),
                        Transform::default(),
                        Visibility::default(),
                        ChunkEntity {
                            coord: ChunkCoord::new(cx, cy, cz),
                        },
                    ))
                    .id();
                commands.entity(root).add_child(entity);
                chunks.insert(key, entity);
                dirty.insert(key);
            }
        }
    }

    commands.insert_resource(SculptWorkpiece {
        grid,
        chunks,
        dirty,
    });
}

fn empty_mesh() -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
    mesh.insert_indices(Indices::U32(Vec::new()));
    mesh
}

fn remesh_dirty_chunks(
    mut workpiece: ResMut<SculptWorkpiece>,
    mut meshes: ResMut<Assets<Mesh>>,
    q_meshes: Query<&Mesh3d>,
) {
    if workpiece.dirty.is_empty() {
        return;
    }

    // Snapshot the set of keys we'll process this frame.
    let batch: Vec<_> = workpiece
        .dirty
        .iter()
        .cloned()
        .take(MAX_CHUNKS_PER_FRAME)
        .collect();

    for key in &batch {
        workpiece.dirty.remove(key);
    }

    for key in batch {
        let coord = ChunkCoord::new(key.0, key.1, key.2);
        let Some(&entity) = workpiece.chunks.get(&key) else {
            continue;
        };
        let Ok(mesh3d) = q_meshes.get(entity) else {
            continue;
        };

        let extracted = extract_chunk(&workpiece.grid, coord);

        let new_mesh = if extracted.is_empty() {
            empty_mesh()
        } else {
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, extracted.positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, extracted.normals);
            mesh.insert_indices(Indices::U32(extracted.indices));
            mesh
        };

        meshes.insert(mesh3d.0.id(), new_mesh);
    }
}
