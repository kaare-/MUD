use glam::{UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};

/// Which side of the "displace vs. remove" line this brush sits on.
///
/// Stage 0 does *not* do volume-preserving redistribution — that
/// lands in Stage 1. Press is a hard CSG subtraction; Pull is a hard
/// CSG union. This is a deliberate simplification so we can evaluate
/// raw interaction feel first.
#[derive(Copy, Clone, Debug)]
pub enum BrushMode {
    /// Carve material away. The brush footprint is subtracted from
    /// the workpiece.
    Press,
    /// Add material. The brush footprint is unioned into the workpiece.
    Pull,
}

/// A single-step spherical brush stamp.
///
/// One call to `apply_sphere_brush` performs *one* hard CSG operation
/// at the given position. Progressive-feeling strokes are produced by
/// the caller: for a Press, offset `center` a bit further along the
/// surface normal each frame the button is held; for a Pull, offset
/// it back toward the viewer. See `sculpt-app` for the exact policy.
#[derive(Copy, Clone, Debug)]
pub struct SphereBrush {
    pub center: Vec3,
    pub radius: f32,
    pub mode: BrushMode,
}

/// Apply one stamp of a spherical brush. Returns the dirty voxel AABB
/// so the caller can queue chunk re-meshing.
pub fn apply_sphere_brush(grid: &mut Grid, brush: &SphereBrush) -> Option<DirtyRegion> {
    // AABB = brush footprint + one voxel of margin. The margin ensures
    // marching cubes / surface nets see a continuous field across the
    // brush boundary and doesn't hiccup at the exact edge of the stamp.
    let margin = grid.voxel_size();
    let effective_radius = brush.radius + margin;

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
    let c = brush.center;

    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                let old = grid.get(ix, iy, iz);
                let new = match brush.mode {
                    // Subtract the brush from the workpiece: A minus B
                    // in SDF land is max(A, -B). Where -B > A, we're
                    // inside the brush (which is now empty air).
                    BrushMode::Press => old.max(-d_brush),
                    // Union the brush with the workpiece: A ∪ B in SDF
                    // land is min(A, B). Where B < A, we're inside the
                    // brush (which is now filled with clay).
                    BrushMode::Pull => old.min(d_brush),
                };
                if new != old {
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    Some(DirtyRegion { min, max })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    #[test]
    fn press_removes_material_inside_brush() {
        let g_res = glam::UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(16.0, 16.0, 16.0), 8.0);
        // Before: centre of the workpiece is inside (negative SDF).
        assert!(g.sample(Vec3::new(16.0, 16.0, 16.0)) < 0.0);
        let brush = SphereBrush {
            center: Vec3::new(16.0, 16.0, 16.0),
            radius: 20.0,
            mode: BrushMode::Press,
        };
        let dirty = apply_sphere_brush(&mut g, &brush);
        assert!(dirty.is_some());
        // After: same point should now be strongly outside.
        assert!(g.sample(Vec3::new(16.0, 16.0, 16.0)) > 15.0);
    }

    #[test]
    fn pull_adds_material_outside_brush() {
        let g_res = glam::UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(8.0, 16.0, 16.0), 4.0);
        // A point at x=20 is outside the initial workpiece.
        assert!(g.sample(Vec3::new(20.0, 16.0, 16.0)) > 0.0);
        // Pull with a brush at x=20 large enough to enclose that point.
        let brush = SphereBrush {
            center: Vec3::new(20.0, 16.0, 16.0),
            radius: 4.0,
            mode: BrushMode::Pull,
        };
        let _ = apply_sphere_brush(&mut g, &brush);
        // After: same point should now be inside.
        assert!(g.sample(Vec3::new(20.0, 16.0, 16.0)) < 0.0);
    }

    #[test]
    fn press_outside_workpiece_is_a_noop_on_the_surface() {
        let g_res = glam::UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(8.0, 16.0, 16.0), 4.0);
        let before = g.sample(Vec3::new(8.0, 16.0, 16.0));
        // Press way off in empty space — should leave the piece untouched.
        let brush = SphereBrush {
            center: Vec3::new(28.0, 28.0, 28.0),
            radius: 2.0,
            mode: BrushMode::Press,
        };
        let _ = apply_sphere_brush(&mut g, &brush);
        let after = g.sample(Vec3::new(8.0, 16.0, 16.0));
        assert!((before - after).abs() < 1e-4);
    }
}
