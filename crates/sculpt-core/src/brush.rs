use glam::{UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};

/// Which side of the "displace vs. remove" line this brush sits on for
/// Stage 0. The finger brush supports both modes; the app picks which
/// via input state.
///
/// Stage 0 does *not* do volume-preserving redistribution — that lands
/// in Stage 1. Press is currently a hard CSG subtraction and Pull is a
/// hard CSG union. This is a deliberate simplification so we can
/// evaluate raw interaction feel first.
#[derive(Copy, Clone, Debug)]
pub enum BrushMode {
    /// Carve material away. The brush footprint is subtracted from
    /// the workpiece each step.
    Press,
    /// Add material. The brush footprint is unioned into the workpiece
    /// each step.
    Pull,
}

/// A single-step spherical brush stamp.
///
/// `depth_per_step` is the amount (in mm) that the isosurface moves
/// per application. For a 60 Hz app engaging every frame, a value
/// around 0.5–1.0 mm gives the feel of a firm, deliberate press.
#[derive(Copy, Clone, Debug)]
pub struct SphereBrush {
    pub center: Vec3,
    pub radius: f32,
    pub depth_per_step: f32,
    pub mode: BrushMode,
}

/// Apply one stamp of a spherical brush. Returns the dirty voxel AABB
/// so the caller can queue chunk re-meshing.
///
/// This is intentionally the simplest possible brush op: it walks every
/// voxel in the brush AABB and applies a hard CSG operation. At
/// Stage-0 grid sizes (256³) with realistic brush sizes (10–40 mm)
/// that is a few thousand voxels per step, easily fast enough on the
/// main thread.
pub fn apply_sphere_brush(grid: &mut Grid, brush: &SphereBrush) -> Option<DirtyRegion> {
    // A little margin so we also touch voxels one cell outside the
    // brush footprint — this keeps the SDF continuous across the
    // brush boundary and avoids sharp discontinuities that would make
    // marching cubes render zig-zags along the edge.
    let effective_radius = brush.radius + brush.depth_per_step + grid.voxel_size();

    let vs = grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = grid.res();

    let min_p = (brush.center - Vec3::splat(effective_radius) - grid.origin()) * inv_vs;
    let max_p = (brush.center + Vec3::splat(effective_radius) - grid.origin()) * inv_vs;

    let min = UVec3::new(
        min_p.x.floor().max(0.0) as u32,
        min_p.y.floor().max(0.0) as u32,
        min_p.z.floor().max(0.0) as u32,
    )
    .min(res - UVec3::ONE);
    let max = UVec3::new(
        (max_p.x.ceil() as i32).max(0) as u32,
        (max_p.y.ceil() as i32).max(0) as u32,
        (max_p.z.ceil() as i32).max(0) as u32,
    )
    .min(res);

    if min.x >= max.x || min.y >= max.y || min.z >= max.z {
        return None;
    }

    let r = brush.radius;
    let depth = brush.depth_per_step;
    let c = brush.center;

    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                let old = grid.get(ix, iy, iz);
                let new = match brush.mode {
                    // Press = subtract. New SDF = max(old, -d_brush - depth).
                    // The `- depth` term shifts the effective isosurface
                    // outward each step, so material is carved gradually
                    // rather than in a single hard bite.
                    BrushMode::Press => old.max(-d_brush - depth),
                    // Pull = union. New SDF = min(old, d_brush - depth).
                    // Same idea inverted: the effective brush isosurface
                    // grows outward each step.
                    BrushMode::Pull => old.min(d_brush - depth),
                };
                if new != old {
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    Some(DirtyRegion { min, max })
}
