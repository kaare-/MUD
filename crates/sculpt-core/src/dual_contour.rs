//! Dual contouring mesh extraction.
//!
//! Surface Nets (the default mesher) rounds sharp features. Dual
//! contouring places one vertex per occupied cell by solving a small
//! QEF against Hermite data (edge intersection + SDF normal), then
//! emits a quad per sign-changing primal edge. Cube corners and
//! knife cuts stay sharper at the cost of a bit more CPU.

use glam::{UVec3, Vec3};

use crate::grid::{ChunkCoord, Grid, CHUNK_SIZE};
use crate::mesh::{cavity_brightness, ExtractedMesh};

/// Padded sample extent — same halo as Surface Nets so chunk seams
/// stay consistent when users switch meshers mid-session.
const PAD: u32 = 1;
const DC_SIZE: u32 = CHUNK_SIZE + 2 * PAD;

/// Corner offsets of a unit cell (x, y, z) in {0,1}³.
const CORNERS: [[u32; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [1, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [0, 1, 1],
    [1, 1, 1],
];

/// Twelve edges of the unit cell as (corner_a, corner_b).
const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (1, 3),
    (1, 5),
    (2, 3),
    (2, 6),
    (3, 7),
    (4, 5),
    (4, 6),
    (5, 7),
    (6, 7),
];

#[inline]
fn sample_idx(lx: u32, ly: u32, lz: u32, sx: u32, sy: u32) -> usize {
    (lx + ly * sx + lz * sx * sy) as usize
}

/// Extract one chunk with dual contouring.
pub fn extract_chunk_dc(grid: &Grid, coord: ChunkCoord) -> ExtractedMesh {
    let res = grid.res();
    let vs = grid.voxel_size();
    let base = UVec3::new(coord.x, coord.y, coord.z) * CHUNK_SIZE;
    let outside = vs * 10.0;

    let mut samples = vec![outside; (DC_SIZE * DC_SIZE * DC_SIZE) as usize];
    for lz in 0..DC_SIZE {
        for ly in 0..DC_SIZE {
            for lx in 0..DC_SIZE {
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
                    outside
                } else {
                    grid.get(gx as u32, gy as u32, gz as u32)
                };
                samples[sample_idx(lx, ly, lz, DC_SIZE, DC_SIZE)] = value;
            }
        }
    }

    let origin_offset = grid.origin()
        + Vec3::new(
            base.x as f32 - PAD as f32,
            base.y as f32 - PAD as f32,
            base.z as f32 - PAD as f32,
        ) * vs;

    extract_dc_from_samples(&samples, DC_SIZE, DC_SIZE, DC_SIZE, vs, origin_offset, grid)
}

/// Dual-contour the entire grid (for STL export). Same halo strategy
/// as [`crate::export::extract_full_mesh`].
pub fn extract_full_mesh_dc(grid: &Grid) -> ExtractedMesh {
    let res = grid.res();
    let vs = grid.voxel_size();
    let halo = 1u32;
    let sx = res.x + 2 * halo;
    let sy = res.y + 2 * halo;
    let sz = res.z + 2 * halo;
    let outside = vs * 10.0;
    let mut samples = vec![outside; (sx * sy * sz) as usize];

    for gz in 0..res.z {
        for gy in 0..res.y {
            for gx in 0..res.x {
                let lx = gx + halo;
                let ly = gy + halo;
                let lz = gz + halo;
                samples[sample_idx(lx, ly, lz, sx, sy)] = grid.get(gx, gy, gz);
            }
        }
    }

    let origin_offset = grid.origin() - Vec3::splat(halo as f32) * vs;
    extract_dc_from_samples(&samples, sx, sy, sz, vs, origin_offset, grid)
}

fn extract_dc_from_samples(
    samples: &[f32],
    sx: u32,
    sy: u32,
    sz: u32,
    vs: f32,
    origin_offset: Vec3,
    grid: &Grid,
) -> ExtractedMesh {
    // Cell count along each axis (samples are corners).
    let cx = sx.saturating_sub(1);
    let cy = sy.saturating_sub(1);
    let cz = sz.saturating_sub(1);
    if cx == 0 || cy == 0 || cz == 0 {
        return ExtractedMesh::default();
    }

    // Vertex index per cell, or u32::MAX if empty.
    let cell_count = (cx * cy * cz) as usize;
    let mut cell_vert = vec![u32::MAX; cell_count];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();

    for iz in 0..cz {
        for iy in 0..cy {
            for ix in 0..cx {
                let mut corners = [0.0f32; 8];
                let mut any_neg = false;
                let mut any_pos = false;
                for (ci, c) in CORNERS.iter().enumerate() {
                    let v = samples[sample_idx(ix + c[0], iy + c[1], iz + c[2], sx, sy)];
                    corners[ci] = v;
                    if v < 0.0 {
                        any_neg = true;
                    } else {
                        any_pos = true;
                    }
                }
                if !(any_neg && any_pos) {
                    continue;
                }

                let mut mass = Vec3::ZERO;
                let mut mass_n = 0u32;
                // QEF: ATA x = ATb
                let mut ata = [[0.0f32; 3]; 3];
                let mut atb = [0.0f32; 3];

                for &(a, b) in &EDGES {
                    let va = corners[a];
                    let vb = corners[b];
                    if (va < 0.0) == (vb < 0.0) {
                        continue;
                    }
                    let t = if (vb - va).abs() < 1e-8 {
                        0.5
                    } else {
                        (0.0 - va) / (vb - va)
                    }
                    .clamp(0.0, 1.0);
                    let ca = CORNERS[a];
                    let cb = CORNERS[b];
                    let local = Vec3::new(
                        ix as f32 + ca[0] as f32 + t * (cb[0] as f32 - ca[0] as f32),
                        iy as f32 + ca[1] as f32 + t * (cb[1] as f32 - ca[1] as f32),
                        iz as f32 + ca[2] as f32 + t * (cb[2] as f32 - ca[2] as f32),
                    );
                    let world = origin_offset + local * vs;
                    let n = sdf_normal(grid, world);
                    mass += local;
                    mass_n += 1;

                    // Accumulate n nᵀ and n (n·p)
                    let np = n.dot(local);
                    for r in 0..3 {
                        atb[r] += n[r] * np;
                        for c in 0..3 {
                            ata[r][c] += n[r] * n[c];
                        }
                    }
                }

                if mass_n == 0 {
                    continue;
                }
                let mass_point = mass / mass_n as f32;
                let mut vertex = solve_qef(&ata, &atb, mass_point);
                // Keep the vertex inside a slightly expanded cell so
                // dual quads don't fold across distant cells.
                vertex.x = vertex.x.clamp(ix as f32 - 0.25, ix as f32 + 1.25);
                vertex.y = vertex.y.clamp(iy as f32 - 0.25, iy as f32 + 1.25);
                vertex.z = vertex.z.clamp(iz as f32 - 0.25, iz as f32 + 1.25);

                let world = origin_offset + vertex * vs;
                let n = sdf_normal(grid, world);
                let vi = positions.len() as u32;
                positions.push([world.x, world.y, world.z]);
                normals.push([n.x, n.y, n.z]);
                let cell_i = (ix + iy * cx + iz * cx * cy) as usize;
                cell_vert[cell_i] = vi;
            }
        }
    }

    if positions.is_empty() {
        return ExtractedMesh::default();
    }

    let mut indices: Vec<u32> = Vec::new();

    // Emit a dual quad for every sign-changing primal edge whose four
    // surrounding cells all have vertices.
    // X-edges: (ix,iy,iz) → (ix+1,iy,iz)
    for iz in 1..cz {
        for iy in 1..cy {
            for ix in 0..cx {
                let v0 = samples[sample_idx(ix, iy, iz, sx, sy)];
                let v1 = samples[sample_idx(ix + 1, iy, iz, sx, sy)];
                if (v0 < 0.0) == (v1 < 0.0) {
                    continue;
                }
                let c00 = cell_index(ix, iy - 1, iz - 1, cx, cy);
                let c10 = cell_index(ix, iy, iz - 1, cx, cy);
                let c11 = cell_index(ix, iy, iz, cx, cy);
                let c01 = cell_index(ix, iy - 1, iz, cx, cy);
                emit_quad(
                    &cell_vert,
                    &mut indices,
                    [c00, c10, c11, c01],
                    v0 >= 0.0,
                );
            }
        }
    }
    // Y-edges
    for iz in 1..cz {
        for iy in 0..cy {
            for ix in 1..cx {
                let v0 = samples[sample_idx(ix, iy, iz, sx, sy)];
                let v1 = samples[sample_idx(ix, iy + 1, iz, sx, sy)];
                if (v0 < 0.0) == (v1 < 0.0) {
                    continue;
                }
                let c00 = cell_index(ix - 1, iy, iz - 1, cx, cy);
                let c10 = cell_index(ix, iy, iz - 1, cx, cy);
                let c11 = cell_index(ix, iy, iz, cx, cy);
                let c01 = cell_index(ix - 1, iy, iz, cx, cy);
                emit_quad(
                    &cell_vert,
                    &mut indices,
                    [c00, c10, c11, c01],
                    v0 >= 0.0,
                );
            }
        }
    }
    // Z-edges
    for iz in 0..cz {
        for iy in 1..cy {
            for ix in 1..cx {
                let v0 = samples[sample_idx(ix, iy, iz, sx, sy)];
                let v1 = samples[sample_idx(ix, iy, iz + 1, sx, sy)];
                if (v0 < 0.0) == (v1 < 0.0) {
                    continue;
                }
                let c00 = cell_index(ix - 1, iy - 1, iz, cx, cy);
                let c10 = cell_index(ix, iy - 1, iz, cx, cy);
                let c11 = cell_index(ix, iy, iz, cx, cy);
                let c01 = cell_index(ix - 1, iy, iz, cx, cy);
                emit_quad(
                    &cell_vert,
                    &mut indices,
                    [c00, c10, c11, c01],
                    v0 >= 0.0,
                );
            }
        }
    }

    let cavity: Vec<f32> = positions
        .iter()
        .map(|p| cavity_brightness(grid, Vec3::from(*p)))
        .collect();

    ExtractedMesh {
        positions,
        normals,
        cavity,
        indices,
    }
}

#[inline]
fn cell_index(ix: u32, iy: u32, iz: u32, cx: u32, cy: u32) -> usize {
    (ix + iy * cx + iz * cx * cy) as usize
}

fn emit_quad(cell_vert: &[u32], indices: &mut Vec<u32>, cells: [usize; 4], flip: bool) {
    let mut verts = [0u32; 4];
    for (i, &c) in cells.iter().enumerate() {
        let v = cell_vert[c];
        if v == u32::MAX {
            return;
        }
        verts[i] = v;
    }
    // Two triangles. `flip` chooses winding so the outward normal
    // points toward positive SDF (air).
    if flip {
        indices.extend_from_slice(&[verts[0], verts[1], verts[2], verts[0], verts[2], verts[3]]);
    } else {
        indices.extend_from_slice(&[verts[0], verts[2], verts[1], verts[0], verts[3], verts[2]]);
    }
}

fn sdf_normal(grid: &Grid, p: Vec3) -> Vec3 {
    let g = grid.gradient_at(p);
    let len = g.length();
    if len > 1e-6 {
        g / len
    } else {
        Vec3::Y
    }
}

/// Solve ATA x = ATb with a mass-point fallback when singular.
fn solve_qef(ata: &[[f32; 3]; 3], atb: &[f32; 3], mass: Vec3) -> Vec3 {
    // Tiny ridge so perfectly flat Hermite sets still invert.
    let mut a = *ata;
    for i in 0..3 {
        a[i][i] += 1e-4;
    }
    if let Some(x) = solve3(&a, atb) {
        Vec3::new(x[0], x[1], x[2])
    } else {
        mass
    }
}

fn solve3(a: &[[f32; 3]; 3], b: &[f32; 3]) -> Option<[f32; 3]> {
    // Gaussian elimination with partial pivoting.
    let mut m = [
        [a[0][0], a[0][1], a[0][2], b[0]],
        [a[1][0], a[1][1], a[1][2], b[1]],
        [a[2][0], a[2][1], a[2][2], b[2]],
    ];
    for col in 0..3 {
        let mut pivot = col;
        let mut best = m[col][col].abs();
        for r in (col + 1)..3 {
            let v = m[r][col].abs();
            if v > best {
                best = v;
                pivot = r;
            }
        }
        if best < 1e-8 {
            return None;
        }
        if pivot != col {
            m.swap(col, pivot);
        }
        let diag = m[col][col];
        for c in col..4 {
            m[col][c] /= diag;
        }
        for r in 0..3 {
            if r == col {
                continue;
            }
            let f = m[r][col];
            for c in col..4 {
                m[r][c] -= f * m[col][c];
            }
        }
    }
    Some([m[0][3], m[1][3], m[2][3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn dc_chunk_meshing_sphere_is_nonempty() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mesh = extract_chunk_dc(&g, ChunkCoord::new(0, 0, 0));
        assert!(!mesh.is_empty());
        assert_eq!(mesh.positions.len(), mesh.normals.len());
        assert_eq!(mesh.positions.len(), mesh.cavity.len());
        assert!(mesh.indices.len() >= 3);
        assert_eq!(mesh.indices.len() % 3, 0);
    }

    #[test]
    fn dc_full_mesh_covers_sphere() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mesh = extract_full_mesh_dc(&g);
        assert!(!mesh.is_empty());
        // Rough volume check: vertices should sit near radius 8.
        let mut r_sum = 0.0;
        let center = Vec3::new(16.0, 16.0, 16.0);
        for p in &mesh.positions {
            r_sum += (Vec3::from(*p) - center).length();
        }
        let r_mean = r_sum / mesh.positions.len() as f32;
        assert!(
            (r_mean - 8.0).abs() < 1.5,
            "mean radius should be ~8, got {r_mean}"
        );
    }

    #[test]
    fn qef_recovers_plane_intersection() {
        // Single plane x = 0.5 through the cell — QEF should land near x=0.5.
        let n = Vec3::X;
        let p = Vec3::new(0.5, 0.5, 0.5);
        let mut ata = [[0.0f32; 3]; 3];
        let mut atb = [0.0f32; 3];
        let np = n.dot(p);
        for r in 0..3 {
            atb[r] += n[r] * np;
            for c in 0..3 {
                ata[r][c] += n[r] * n[c];
            }
        }
        let x = solve_qef(&ata, &atb, Vec3::splat(0.5));
        assert!((x.x - 0.5).abs() < 0.05, "got {x:?}");
    }
}
