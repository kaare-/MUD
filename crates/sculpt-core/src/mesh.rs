use fast_surface_nets::ndshape::{ConstShape, ConstShape3u32};
use fast_surface_nets::{surface_nets, SurfaceNetsBuffer};
use glam::{UVec3, Vec3};

use crate::dual_contour::extract_chunk_dc;
use crate::grid::{ChunkCoord, Grid, CHUNK_SIZE};

/// Which isosurface algorithm to use for viewport remesh / export.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum MesherKind {
    /// Naive Surface Nets — fast, smooths sharp features.
    #[default]
    SurfaceNets,
    /// Dual contouring with QEF — better cube corners / cuts.
    DualContouring,
}

impl MesherKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::SurfaceNets => "Surface Nets",
            Self::DualContouring => "Dual Contouring",
        }
    }

    pub fn all() -> [Self; 2] {
        [Self::SurfaceNets, Self::DualContouring]
    }
}

/// Padded per-chunk sample size. Surface Nets needs one voxel of padding
/// on every side so it can see across chunk boundaries. With
/// `CHUNK_SIZE = 32`, each chunk is meshed against a 34³ local sample
/// array. The padding on the negative side is a repeat of the boundary
/// voxel (not a fresh sample) — this keeps chunk seams tight for
/// Stage 0 without a separate "chunk shares +1 halo" bookkeeping layer.
///
/// This is a Stage-0 simplification. Stage 2 will replace this with a
/// proper shared-halo scheme when we move to sparse SDF tiles.
const PAD: u32 = 1;
const SN_SIZE: u32 = CHUNK_SIZE + 2 * PAD;
type ChunkShape = ConstShape3u32<{ SN_SIZE }, { SN_SIZE }, { SN_SIZE }>;

/// A ready-to-upload mesh in piece-local space.
#[derive(Default, Clone)]
pub struct ExtractedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex cavity brightness in `0..=1` (1 = open surface,
    /// lower = crevice). Packed into mesh UV.x for matcap shading.
    pub cavity: Vec<f32>,
    pub indices: Vec<u32>,
}

impl ExtractedMesh {
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }
}

/// Crevice darkening from `smoothed(φ) − φ` (DESIGN.md §3.4).
///
/// Returns a brightness multiplier: `1` on open/convex surface,
/// down toward `0` in pits. Cheap 6-neighbour average of the SDF.
pub fn cavity_brightness(grid: &Grid, p: Vec3) -> f32 {
    let vs = grid.voxel_size();
    let phi = grid.sample(p);
    let h = vs;
    let neighbors = [
        grid.sample(p + Vec3::new(h, 0.0, 0.0)),
        grid.sample(p + Vec3::new(-h, 0.0, 0.0)),
        grid.sample(p + Vec3::new(0.0, h, 0.0)),
        grid.sample(p + Vec3::new(0.0, -h, 0.0)),
        grid.sample(p + Vec3::new(0.0, 0.0, h)),
        grid.sample(p + Vec3::new(0.0, 0.0, -h)),
    ];
    let smooth = neighbors.iter().sum::<f32>() / neighbors.len() as f32;
    // Crevices: neighbourhood more solid → smooth < φ → delta < 0.
    let delta = smooth - phi;
    let occ = ((-delta) / vs).clamp(0.0, 1.0);
    (1.0 - 0.7 * occ).clamp(0.15, 1.0)
}

/// Extract a single chunk's mesh, dispatching on [`MesherKind`].
pub fn extract_chunk_with(grid: &Grid, coord: ChunkCoord, kind: MesherKind) -> ExtractedMesh {
    match kind {
        MesherKind::SurfaceNets => extract_chunk_surface_nets(grid, coord),
        MesherKind::DualContouring => extract_chunk_dc(grid, coord),
    }
}

/// Extract a single chunk with the default Surface Nets mesher.
pub fn extract_chunk(grid: &Grid, coord: ChunkCoord) -> ExtractedMesh {
    extract_chunk_with(grid, coord, MesherKind::SurfaceNets)
}

/// Extract a single chunk's mesh from the grid using Naive Surface Nets.
///
/// The output positions are in piece-local coordinates (mm), the
/// normals are unit length in the same frame, and indices reference
/// positions with clockwise winding matching the crate's convention
/// (which is CCW when viewed from outside — the standard).
fn extract_chunk_surface_nets(grid: &Grid, coord: ChunkCoord) -> ExtractedMesh {
    let res = grid.res();
    let vs = grid.voxel_size();

    let base = UVec3::new(coord.x, coord.y, coord.z) * CHUNK_SIZE;
    // The chunk covers voxels [base, base + CHUNK_SIZE] inclusive on the
    // upper end (we need one extra sample to close the last cell).

    let mut samples = vec![0.0f32; ChunkShape::SIZE as usize];

    // Fill the local sample buffer, clamping to the grid bounds. Voxels
    // outside the grid are set to a large positive value, which reads
    // as "empty air" and produces no surface.
    let outside_value = grid.voxel_size() * 10.0;
    for lz in 0..SN_SIZE {
        for ly in 0..SN_SIZE {
            for lx in 0..SN_SIZE {
                let gx = base.x as i32 + lx as i32 - PAD as i32;
                let gy = base.y as i32 + ly as i32 - PAD as i32;
                let gz = base.z as i32 + lz as i32 - PAD as i32;
                let value = if gx < 0
                    || gy < 0
                    || gz < 0
                    || gx as u32 >= res.x
                    || gy as u32 >= res.y
                    || gz as u32 >= res.z
                {
                    outside_value
                } else {
                    grid.get(gx as u32, gy as u32, gz as u32)
                };
                let li = ChunkShape::linearize([lx, ly, lz]) as usize;
                samples[li] = value;
            }
        }
    }

    let mut buffer = SurfaceNetsBuffer::default();
    surface_nets(
        &samples,
        &ChunkShape {},
        [0, 0, 0],
        [SN_SIZE - 1, SN_SIZE - 1, SN_SIZE - 1],
        &mut buffer,
    );

    if buffer.positions.is_empty() {
        return ExtractedMesh::default();
    }

    // Transform Surface Nets output (in local, unit-voxel space) into
    // piece-local coordinates. The `-PAD as f32` offset compensates for
    // the padding we added at the start of the sample buffer.
    let origin_offset = grid.origin()
        + Vec3::new(
            base.x as f32 - PAD as f32,
            base.y as f32 - PAD as f32,
            base.z as f32 - PAD as f32,
        ) * vs;

    let positions: Vec<[f32; 3]> = buffer
        .positions
        .into_iter()
        .map(|p| {
            let v = origin_offset + Vec3::new(p[0], p[1], p[2]) * vs;
            [v.x, v.y, v.z]
        })
        .collect();

    // Surface Nets emits smoothed normals in local space; those are also
    // valid in piece-local space because we only scaled uniformly.
    let normals = buffer.normals;

    let cavity: Vec<f32> = positions
        .iter()
        .map(|p| cavity_brightness(grid, Vec3::from(*p)))
        .collect();

    ExtractedMesh {
        positions,
        normals,
        cavity,
        indices: buffer.indices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn chunk_mesh_includes_cavity_per_vertex() {
        // 32³ domain = one chunk at CHUNK_SIZE 32; centred sphere fills it.
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mesh = extract_chunk(&g, ChunkCoord::new(0, 0, 0));
        assert!(!mesh.is_empty());
        assert_eq!(mesh.cavity.len(), mesh.positions.len());
        assert!(mesh.cavity.iter().all(|&c| (0.0..=1.0).contains(&c)));
    }

    #[test]
    fn cavity_brightness_is_unit_interval() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let c = cavity_brightness(&g, Vec3::new(16.0, 16.0, 8.0));
        assert!((0.0..=1.0).contains(&c), "cavity={c}");
    }
}
