//! Export the workpiece to external formats.
//!
//! Right now: **binary STL**, the lingua franca of desktop 3D printing.
//! Every slicer (Cura, Bambu Studio, PrusaSlicer, OrcaSlicer,
//! Simplify3D, Chitubox) reads binary STL; every CAD tool that reads
//! meshes at all reads STL. It's a bad format (no shared vertices, no
//! units, no colour, no history) but a universal one — the exact
//! trade-off you want for "get my sculpt onto the print bed."
//!
//! # Two-step export
//!
//! 1. `extract_full_mesh(grid)` runs Surface Nets over the entire
//!    grid in one pass, with a 1-voxel outside-value halo so the
//!    surface closes cleanly at the domain boundary. This produces a
//!    single indexed mesh with no chunk seams (contrast the app's
//!    per-chunk `extract_chunk`, whose overlapping padding double-
//!    generates geometry at chunk borders — fine for real-time
//!    rendering, wrong for STL where slicers spatial-hash by exact
//!    position and can leave hairline gaps).
//!
//! 2. `write_stl_binary(mesh, writer)` walks the indexed mesh and
//!    writes one STL facet per triangle, computing the face normal
//!    from the triangle rather than trusting the smoothed per-vertex
//!    normals Surface Nets emits. STL uses face normals, not vertex
//!    normals, and the convention slicers expect is right-hand-rule
//!    from CCW-when-viewed-from-outside vertex order.
//!
//! # Coordinate system
//!
//! Piece-local space is Y-up (Bevy convention: X-right, Y-up,
//! Z-toward-camera). Every desktop slicer expects **Z-up** with the
//! print bed at Z=0. We rotate on export: `(x, y, z)_piece →
//! (x, -z, y)_stl`. This is a rigid rotation about X so winding is
//! preserved. Consequence: the workbench (piece-local Y=0) lands at
//! STL Z=0, i.e. the piece sits directly on the print bed. No manual
//! reorientation in the slicer required.
//!
//! If the caller wants raw Y-up for some other pipeline (Blender,
//! game engines that stayed Y-up), pass `Orientation::YupAsIs` to
//! the writer.

use std::collections::HashMap;
use std::io::{self, Write};

use fast_surface_nets::ndshape::RuntimeShape;
use fast_surface_nets::{surface_nets, SurfaceNetsBuffer};
use glam::Vec3;

use crate::dual_contour::extract_full_mesh_dc;
use crate::grid::Grid;
use crate::mesh::{cavity_brightness, ExtractedMesh, MesherKind};

/// Result of an index-edge watertightness check on an [`ExtractedMesh`].
///
/// A mesh is considered watertight when every undirected edge is shared
/// by exactly two triangles (no open boundaries, no non-manifold fans).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatertightReport {
    pub verts: u32,
    pub edges: u32,
    pub faces: u32,
    pub open_edges: u32,
    pub nonmanifold_edges: u32,
    /// Euler characteristic `V − E + F` (info only — multi-shell
    /// closed meshes can be watertight with χ ≠ 2).
    pub euler: i32,
}

impl WatertightReport {
    pub fn is_watertight(self) -> bool {
        self.faces > 0 && self.open_edges == 0 && self.nonmanifold_edges == 0
    }
}

/// Count open / non-manifold edges on an indexed triangle mesh.
///
/// Uses vertex **indices** (not float positions), so it matches the
/// shared-vertex Surface Nets mesh we export from — not the duplicated
/// triangle-soup STL on disk.
pub fn check_watertight(mesh: &ExtractedMesh) -> WatertightReport {
    let faces = (mesh.indices.len() / 3) as u32;
    let verts = mesh.positions.len() as u32;
    if faces == 0 {
        return WatertightReport {
            verts,
            edges: 0,
            faces: 0,
            open_edges: 0,
            nonmanifold_edges: 0,
            euler: verts as i32,
        };
    }

    let mut edge_count: HashMap<(u32, u32), u32> = HashMap::new();
    for tri in mesh.indices.chunks_exact(3) {
        let a = tri[0];
        let b = tri[1];
        let c = tri[2];
        for (i, j) in [(a, b), (b, c), (c, a)] {
            let key = if i <= j { (i, j) } else { (j, i) };
            *edge_count.entry(key).or_default() += 1;
        }
    }
    let edges = edge_count.len() as u32;
    let mut open_edges = 0u32;
    let mut nonmanifold_edges = 0u32;
    for count in edge_count.values() {
        match *count {
            1 => open_edges += 1,
            2 => {}
            _ => nonmanifold_edges += 1,
        }
    }
    WatertightReport {
        verts,
        edges,
        faces,
        open_edges,
        nonmanifold_edges,
        euler: verts as i32 - edges as i32 + faces as i32,
    }
}

/// One voxel of positive-SDF padding around the grid. Surface Nets
/// walks cells (each cell covers 2×2×2 samples), so with the halo we
/// guarantee the algorithm sees "outside" everywhere at the domain
/// boundary and closes the mesh instead of leaving open cells.
const HALO: u32 = 1;

/// Coordinate system chosen for the exported mesh.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// (x, y, z) → (x, -z, y). Piece-local Y-up rotated 90° about X
    /// so it becomes Z-up. Print-bed friendly: the workbench (Y=0)
    /// lands at Z=0, so the piece sits on the bed.
    Zup,
    /// (x, y, z) → (x, y, z). Preserve piece-local Y-up as-is.
    /// Useful for Blender or a Y-up game engine downstream.
    YupAsIs,
}

impl Orientation {
    #[inline]
    fn transform(self, v: [f32; 3]) -> [f32; 3] {
        match self {
            Orientation::Zup => [v[0], -v[2], v[1]],
            Orientation::YupAsIs => v,
        }
    }
}

/// Extract the full grid as one indexed mesh (no chunk seams).
///
/// Dispatches on [`MesherKind`]. Surface Nets is the historical
/// default; Dual Contouring keeps sharper cube corners for export.
pub fn extract_full_mesh(grid: &Grid) -> ExtractedMesh {
    extract_full_mesh_with(grid, MesherKind::SurfaceNets)
}

/// Like [`extract_full_mesh`] but picks the isosurface algorithm.
pub fn extract_full_mesh_with(grid: &Grid, kind: MesherKind) -> ExtractedMesh {
    match kind {
        MesherKind::SurfaceNets => extract_full_mesh_surface_nets(grid),
        MesherKind::DualContouring => extract_full_mesh_dc(grid),
    }
}

fn extract_full_mesh_surface_nets(grid: &Grid) -> ExtractedMesh {
    let res = grid.res();
    let vs = grid.voxel_size();
    let padded = [res.x + 2 * HALO, res.y + 2 * HALO, res.z + 2 * HALO];
    let shape = RuntimeShape::<u32, 3>::new(padded);

    // Positive "outside air" value — large enough that Surface Nets
    // never places a vertex inside the halo. Ten voxel-widths is
    // comfortably beyond the surface-nets narrow band.
    let outside_value = vs * 10.0;
    let n = (padded[0] * padded[1] * padded[2]) as usize;
    let mut samples = vec![outside_value; n];

    for gz in 0..res.z {
        for gy in 0..res.y {
            for gx in 0..res.x {
                let lx = gx + HALO;
                let ly = gy + HALO;
                let lz = gz + HALO;
                let idx = (lx
                    + ly * padded[0]
                    + lz * padded[0] * padded[1]) as usize;
                samples[idx] = grid.get(gx, gy, gz);
            }
        }
    }

    let mut buffer = SurfaceNetsBuffer::default();
    surface_nets(
        &samples,
        &shape,
        [0, 0, 0],
        [padded[0] - 1, padded[1] - 1, padded[2] - 1],
        &mut buffer,
    );

    if buffer.positions.is_empty() {
        return ExtractedMesh::default();
    }

    // Surface Nets emits positions in unit-voxel space within the
    // padded local buffer. Translate back into piece-local mm by
    // subtracting the halo offset and multiplying by voxel size.
    let origin = grid.origin();
    let halo_offset = Vec3::splat(HALO as f32) * vs;
    let positions: Vec<[f32; 3]> = buffer
        .positions
        .into_iter()
        .map(|p| {
            let v = origin - halo_offset + Vec3::new(p[0], p[1], p[2]) * vs;
            [v.x, v.y, v.z]
        })
        .collect();

    let cavity: Vec<f32> = positions
        .iter()
        .map(|p| cavity_brightness(grid, Vec3::from(*p)))
        .collect();

    ExtractedMesh {
        positions,
        normals: buffer.normals,
        cavity,
        indices: buffer.indices,
    }
}

/// Number of bytes a binary-STL file will occupy for the given mesh.
/// Handy for pre-sizing buffers or reporting file size in the log.
///
/// STL binary layout: 80-byte header + 4-byte triangle count +
/// (12-byte face normal + 3×12-byte vertex + 2-byte attribute) per
/// triangle = 50 bytes per triangle.
pub fn stl_binary_size(mesh: &ExtractedMesh) -> usize {
    let tri_count = mesh.indices.len() / 3;
    80 + 4 + tri_count * 50
}

/// Write `mesh` as a binary STL file to `writer`.
///
/// STL is triangle-soup — no shared vertices, one face normal per
/// triangle. We compute the normal from the actual vertex positions
/// via the right-hand rule; the vertex-normals Surface Nets provides
/// are per-vertex smoothed values and would give incorrect facet
/// normals for slicers doing shell-thickness or overhang analysis.
///
/// `orientation` selects the exported coordinate system.
pub fn write_stl_binary<W: Write>(
    mesh: &ExtractedMesh,
    orientation: Orientation,
    writer: &mut W,
) -> io::Result<()> {
    let tri_count = (mesh.indices.len() / 3) as u32;

    // 80-byte header. Some viewers show ASCII in it; a short label
    // helps if the file is ever inspected. Zero-pad the rest.
    let mut header = [0u8; 80];
    let label = b"MUD sculpt binary STL";
    header[..label.len()].copy_from_slice(label);
    writer.write_all(&header)?;
    writer.write_all(&tri_count.to_le_bytes())?;

    for tri in mesh.indices.chunks_exact(3) {
        let ia = tri[0] as usize;
        let ib = tri[1] as usize;
        let ic = tri[2] as usize;
        let a = orientation.transform(mesh.positions[ia]);
        let b = orientation.transform(mesh.positions[ib]);
        let c = orientation.transform(mesh.positions[ic]);

        let av = Vec3::from(a);
        let bv = Vec3::from(b);
        let cv = Vec3::from(c);
        // Right-hand rule normal from CCW-viewed-from-outside winding.
        let n = (bv - av).cross(cv - av).normalize_or_zero();

        write_f32_triplet(writer, [n.x, n.y, n.z])?;
        write_f32_triplet(writer, a)?;
        write_f32_triplet(writer, b)?;
        write_f32_triplet(writer, c)?;
        // 2-byte attribute count. Zero per convention.
        writer.write_all(&[0u8, 0u8])?;
    }

    writer.flush()?;
    Ok(())
}

fn write_f32_triplet<W: Write>(writer: &mut W, v: [f32; 3]) -> io::Result<()> {
    writer.write_all(&v[0].to_le_bytes())?;
    writer.write_all(&v[1].to_le_bytes())?;
    writer.write_all(&v[2].to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec3;

    #[test]
    fn extract_full_mesh_of_sphere_produces_triangles() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mesh = extract_full_mesh(&g);
        assert!(!mesh.is_empty(), "sphere should produce a mesh");
        assert_eq!(mesh.indices.len() % 3, 0, "indices divide into triangles");
        // A 32^3 grid with a mid-sized sphere should give at least a
        // few hundred triangles; exact count depends on surface-nets
        // internals so we don't assert it, just sanity-check that
        // there is a substantial surface.
        assert!(mesh.indices.len() > 300);
    }

    #[test]
    fn sphere_export_mesh_is_watertight() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mesh = extract_full_mesh(&g);
        let report = check_watertight(&mesh);
        assert!(
            report.is_watertight(),
            "closed sphere should be watertight: {report:?}"
        );
    }

    #[test]
    fn single_triangle_has_open_edges() {
        let mesh = ExtractedMesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            cavity: vec![1.0; 3],
            indices: vec![0, 1, 2],
        };
        let report = check_watertight(&mesh);
        assert!(!report.is_watertight());
        assert_eq!(report.open_edges, 3);
        assert_eq!(report.nonmanifold_edges, 0);
    }

    #[test]
    fn edge_shared_by_three_triangles_is_nonmanifold() {
        // Three triangles around a common edge 0–1.
        let mesh = ExtractedMesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.5, 1.0, 0.0],
                [0.5, 0.0, 1.0],
                [0.5, 0.0, -1.0],
            ],
            normals: vec![[0.0, 1.0, 0.0]; 5],
            cavity: vec![1.0; 5],
            indices: vec![0, 1, 2, 0, 1, 3, 0, 1, 4],
        };
        let report = check_watertight(&mesh);
        assert!(!report.is_watertight());
        assert!(
            report.nonmanifold_edges >= 1,
            "edge 0-1 shared by 3 tris: {report:?}"
        );
    }

    #[test]
    fn extract_full_mesh_of_empty_grid_returns_empty() {
        // "Empty" = all-positive SDF: no surface anywhere.
        let mut g = Grid::empty(UVec3::new(16, 16, 16), 1.0, Vec3::ZERO);
        for iz in 0..16 {
            for iy in 0..16 {
                for ix in 0..16 {
                    g.set(ix, iy, iz, 1.0);
                }
            }
        }
        let mesh = extract_full_mesh(&g);
        assert!(mesh.is_empty());
    }

    #[test]
    fn stl_binary_header_and_triangle_count_are_correct() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mesh = extract_full_mesh(&g);
        let expected_size = stl_binary_size(&mesh);
        let mut out = Vec::new();
        write_stl_binary(&mesh, Orientation::Zup, &mut out).unwrap();
        assert_eq!(out.len(), expected_size);

        // Read back the triangle count.
        let mut count_bytes = [0u8; 4];
        count_bytes.copy_from_slice(&out[80..84]);
        let count = u32::from_le_bytes(count_bytes) as usize;
        assert_eq!(count, mesh.indices.len() / 3);
    }

    #[test]
    fn zup_orientation_puts_workbench_at_z_zero() {
        // In piece-local, y=0 is the workbench. After the Y→Z-up
        // rotation, workbench-height should be z=0.
        //
        // Sphere sitting on the workbench: centre y=8, radius 8. The
        // bottom of the sphere touches y=0. After rotation, that
        // touches z=0.
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::new(-16.0, 0.0, -16.0),
            Vec3::new(0.0, 8.0, 0.0),
            8.0,
        );
        let mesh = extract_full_mesh(&g);
        let mut min_z_stl = f32::INFINITY;
        for p in &mesh.positions {
            let t = Orientation::Zup.transform(*p);
            min_z_stl = min_z_stl.min(t[2]);
        }
        // The lowest STL Z should be ~0 (piece bottom on the bed).
        // Allow half a voxel for surface-nets vertex placement error.
        assert!(
            min_z_stl.abs() < 1.0,
            "piece should rest on the bed after Z-up rotation: min_z={min_z_stl}",
        );
    }

    #[test]
    fn yup_orientation_is_identity() {
        let v = [1.5, -2.5, 3.5];
        assert_eq!(Orientation::YupAsIs.transform(v), v);
    }

    #[test]
    fn stl_face_normals_point_outward_for_a_sphere() {
        // Every triangle of a sphere should have a face normal whose
        // dot with (centroid - sphere_centre) is positive — pointing
        // radially outward. This catches winding-order bugs that
        // would leave the slicer thinking the surface is inside-out.
        let centre = Vec3::new(16.0, 16.0, 16.0);
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            centre,
            8.0,
        );
        let mesh = extract_full_mesh(&g);
        let mut inward = 0usize;
        for tri in mesh.indices.chunks_exact(3) {
            let a = Vec3::from(mesh.positions[tri[0] as usize]);
            let b = Vec3::from(mesh.positions[tri[1] as usize]);
            let c = Vec3::from(mesh.positions[tri[2] as usize]);
            let n = (b - a).cross(c - a).normalize_or_zero();
            let centroid = (a + b + c) / 3.0;
            let radial = (centroid - centre).normalize_or_zero();
            if n.dot(radial) < 0.0 {
                inward += 1;
            }
        }
        // Perfect zero is unrealistic (a handful of tris near cell
        // boundaries can have near-zero normals), but the vast
        // majority should face outward.
        let total = mesh.indices.len() / 3;
        let bad_ratio = inward as f32 / total as f32;
        assert!(
            bad_ratio < 0.02,
            "too many inward-facing triangles: {inward}/{total} = {:.2}%",
            bad_ratio * 100.0,
        );
    }
}
