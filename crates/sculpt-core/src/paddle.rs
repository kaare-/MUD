//! Paddle tool — a flat contact-stamp for making flats.
//!
//! Physically the paddle is a rigid flat plate you press against clay
//! to squash a region into a plane. Digitally we model it as a disk-
//! shaped half-space cut: pick a plane (a `center` and outward `normal`
//! pointing away from the workpiece) and a disk `radius`; any workpiece
//! material within the disk *and* on the paddle side of the plane is
//! removed. Material outside the disk is untouched, so the flat is
//! bounded — you don't accidentally shear the whole piece.
//!
//! Progressive-feel behaviour: the caller offsets the `center` a bit
//! deeper into the surface each frame the button is held, so the
//! flat grows in one direction until the plane is buried. Same pattern
//! as the clay tool's advance-per-step trick.
//!
//! Paddle is a *removal* tool (DESIGN §5 decision 1) in this MVP.
//! Volume-preserving displacement for the paddle (material squeezes
//! out around the flat rim, matching real clay) is a natural follow-
//! up but deferred to keep this PR focused.

use glam::{UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};

/// A single-step paddle stamp.
///
/// - `center` is a point on the paddle's flat face, in piece-local mm.
/// - `normal` is the outward-from-workpiece unit normal (i.e. the
///   direction the paddle is pushing *from*). For a paddle pressed
///   down onto a piece on the workbench, `normal` points up.
/// - `radius` is the disk's radius. The paddle acts on the workpiece
///   only within this disk.
/// - `workbench_y` mirrors the constraint on the other tools.
#[derive(Copy, Clone, Debug)]
pub struct Paddle {
    pub center: Vec3,
    pub normal: Vec3,
    pub radius: f32,
    pub workbench_y: Option<f32>,
}

/// Apply one paddle stamp. Convenience wrapper.
pub fn apply_paddle(grid: &mut Grid, paddle: &Paddle) -> Option<DirtyRegion> {
    apply_paddle_with_callback(grid, paddle, |_, _, _, _| {})
}

/// Callback variant — pre-mutation values for the undo journal.
pub fn apply_paddle_with_callback<F>(
    grid: &mut Grid,
    paddle: &Paddle,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let vs = grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = grid.res();

    // Bounding sphere: enough to cover the disk radius. The half-space
    // extends infinitely on the paddle side, but voxels far from the
    // plane along the normal that ARE within the disk radius will be
    // hit by the axial extent of the grid anyway. Simpler: bound by
    // the paddle's radius + a margin, iterate that box, skip voxels
    // outside the SDF's slab-with-cap footprint.
    let bound = paddle.radius + vs;
    let min_p = (paddle.center - Vec3::splat(bound) - grid.origin()) * inv_vs;
    let max_p = (paddle.center + Vec3::splat(bound) - grid.origin()) * inv_vs;

    let mut aabb_min = UVec3::new(
        min_p.x.floor().max(0.0) as u32,
        min_p.y.floor().max(0.0) as u32,
        min_p.z.floor().max(0.0) as u32,
    )
    .min(res - UVec3::ONE);
    let aabb_max = UVec3::new(
        (max_p.x.ceil() as i32).max(0) as u32,
        (max_p.y.ceil() as i32).max(0) as u32,
        (max_p.z.ceil() as i32).max(0) as u32,
    )
    .min(res);

    // We also need to extend the box in the +normal direction to
    // cover the axial reach of the paddle (the half-space above the
    // plane). Rather than compute a tight bound, expand by the disk
    // radius along each axis — cheap and correct.
    // (Already covered by the ±bound splat above, since bound = radius.)
    // No-op; kept as a comment for anyone tempted to shrink the AABB.
    let _ = &mut aabb_min;

    if aabb_min.x >= aabb_max.x || aabb_min.y >= aabb_max.y || aabb_min.z >= aabb_max.z {
        return None;
    }

    let c = paddle.center;
    let n = paddle.normal;
    let r = paddle.radius;
    let mut touched = false;
    let mut dirty_min = UVec3::MAX;
    let mut dirty_max = UVec3::ZERO;

    for iz in aabb_min.z..aabb_max.z {
        for iy in aabb_min.y..aabb_max.y {
            for ix in aabb_min.x..aabb_max.x {
                let p = grid.position(ix, iy, iz);
                let d = p - c;
                let axial = d.dot(n);
                // Radial distance from the paddle's axis.
                let radial = (d - n * axial).length();
                if radial >= r {
                    // Outside the disk: paddle can't reach.
                    continue;
                }

                // Paddle-SDF: intersection of half-space and cylinder.
                //   half_space_sdf = -axial       (negative above plane)
                //   cylinder_sdf   = radial - r   (negative inside cylinder)
                //   paddle_sdf     = max(half_space_sdf, cylinder_sdf)
                // CSG subtract: new = max(old, -paddle_sdf)
                //             = max(old, min(axial, r - radial))
                //
                // For voxels *above* the plane and *inside* the disk
                // (axial > 0, radial < r), min(axial, r - radial) > 0,
                // so `new` may become positive (voxel becomes outside).
                // For voxels below the plane, axial < 0 so the min is
                // negative and old is preserved unless it was even
                // more negative.
                let paddle_neg_sdf = axial.min(r - radial);
                let old = grid.get(ix, iy, iz);
                let mut new = old.max(paddle_neg_sdf);

                if let Some(wb_y) = paddle.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }

                if new != old {
                    on_pre_mutation(ix, iy, iz, old);
                    grid.set(ix, iy, iz, new);
                    let v = UVec3::new(ix, iy, iz);
                    if !touched {
                        dirty_min = v;
                        dirty_max = v + UVec3::ONE;
                        touched = true;
                    } else {
                        dirty_min = dirty_min.min(v);
                        dirty_max = dirty_max.max(v + UVec3::ONE);
                    }
                }
            }
        }
    }

    if touched {
        Some(DirtyRegion {
            min: dirty_min,
            max: dirty_max,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paddle_flattens_a_bump() {
        // Big spherical workpiece; press a paddle down on top of it,
        // flat plane at y = 40 with normal +Y. Everything above the
        // plane (up to disk radius) should be sheared off.
        let g_res = UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);

        let top_of_ball = g.sample(Vec3::new(32.0, 52.0, 32.0));
        assert!(top_of_ball.abs() < 1.0, "top of ball should be on surface");
        // A voxel above the ball but on axis: outside.
        assert!(g.sample(Vec3::new(32.0, 48.0, 32.0)) < 0.0);

        let paddle = Paddle {
            center: Vec3::new(32.0, 45.0, 32.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            radius: 15.0,
            workbench_y: None,
        };
        let _ = apply_paddle(&mut g, &paddle);

        // Above the plane, on axis, inside the disk: carved (positive).
        assert!(g.sample(Vec3::new(32.0, 48.0, 32.0)) > 0.0);
        // Below the plane, on axis, inside the disk: still material.
        assert!(g.sample(Vec3::new(32.0, 40.0, 32.0)) < 0.0);
        // 'Outside disk = untouched' is covered by
        // paddle_leaves_material_outside_the_disk below.
    }

    #[test]
    fn paddle_leaves_material_outside_the_disk() {
        // Verify the disk constraint: material at (x=45, ...) is well
        // outside the disk radius, so a paddle at (32, 45, 32) with
        // radius 5 can't touch it.
        let g_res = UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);

        let before = g.sample(Vec3::new(45.0, 45.0, 32.0));
        let paddle = Paddle {
            center: Vec3::new(32.0, 45.0, 32.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            radius: 5.0,
            workbench_y: None,
        };
        let _ = apply_paddle(&mut g, &paddle);
        let after = g.sample(Vec3::new(45.0, 45.0, 32.0));
        assert!(
            (before - after).abs() < 1e-3,
            "outside the disk should be untouched: before={before}, after={after}",
        );
    }
}
