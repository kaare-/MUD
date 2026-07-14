//! Cookie cutter tool.
//!
//! A cookie cutter is a `Profile` extruded along an axis, applied as
//! a CSG subtraction to the workpiece. Unlike the clay brush, the
//! cutter is a *removal* tool (DESIGN §5, decision 1): pressing it
//! into clay carves a shape-of-the-profile channel that goes all the
//! way through whatever it hits. No magic-clay bulge — clay squeezed
//! out by a real metal cutter does end up on the cutter, not on the
//! remaining piece.

use glam::{UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};
use crate::profile::{extrude_profile, Profile};

/// A planar-slab cutter — the digital analogue of pulling a taut wire
/// through clay.
///
/// The cut is a thin slab of thickness `thickness` centred on a plane
/// passing through `anchor` with unit normal `normal`. Voxels inside
/// the slab are subtracted from the workpiece; everything else is
/// left alone. The plane is infinite: the slab always slices all the
/// way through in the two directions perpendicular to `normal`.
///
/// This is the *removal* half of Stage 2's cutting story. Formal
/// object identity (each disconnected piece as its own editable
/// object) is deferred; the current dense-grid workpiece already
/// treats disconnected SDF regions correctly for the clay brush and
/// cookie cutter, so cut → sculpt-each-half works implicitly.
#[derive(Copy, Clone, Debug)]
pub struct WireCutter {
    pub anchor: Vec3,
    pub normal: Vec3,
    pub thickness: f32,
    pub workbench_y: Option<f32>,
}

/// A cookie-cutter stamp.
///
/// - `profile` is the cross-section shape (`Profile::Circle`, etc.).
/// - `origin` is the centre of the cutter in piece-local mm.
/// - `axis` is the extrusion direction (unit length). Typically the
///   inverse surface normal at the click point, so the cutter presses
///   into the workpiece the way the user is aiming.
/// - `half_length` is how far the cutter extends along `axis` in
///   each direction from `origin`. For a "cut all the way through"
///   feel, set this to roughly the workpiece's largest dimension.
/// - `workbench_y` mirrors the field on `SphereBrush`; usually
///   `Some(0.0)` so the piece is never carved *below* the workbench.
#[derive(Copy, Clone, Debug)]
pub struct CookieCutter {
    pub profile: Profile,
    pub origin: Vec3,
    pub axis: Vec3,
    pub half_length: f32,
    pub workbench_y: Option<f32>,
}

/// Apply one cookie-cutter stamp. Wrapper around the callback variant
/// with an empty callback, matching the shape of `apply_sphere_brush`.
pub fn apply_cookie_cutter(grid: &mut Grid, cutter: &CookieCutter) -> Option<DirtyRegion> {
    apply_cookie_cutter_with_callback(grid, cutter, |_, _, _, _| {})
}

/// Callback variant. `on_pre_mutation` fires once per voxel that is
/// about to change, receiving the pre-mutation SDF value — suitable
/// for feeding the undo journal.
pub fn apply_cookie_cutter_with_callback<F>(
    grid: &mut Grid,
    cutter: &CookieCutter,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    // Conservative bounding sphere: circumscribes both the profile
    // cross-section and the axial extent. Cheap; the extra voxels we
    // scan outside the true cutter AABB are still guarded by the
    // exact SDF check inside the loop.
    let bound = cutter
        .profile
        .bounding_radius()
        .max(cutter.half_length)
        + grid.voxel_size();

    let vs = grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = grid.res();

    let min_p = (cutter.origin - Vec3::splat(bound) - grid.origin()) * inv_vs;
    let max_p = (cutter.origin + Vec3::splat(bound) - grid.origin()) * inv_vs;

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

    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                let p = grid.position(ix, iy, iz);
                let d_cut = extrude_profile(
                    &cutter.profile,
                    cutter.origin,
                    cutter.axis,
                    cutter.half_length,
                    p,
                );
                let old = grid.get(ix, iy, iz);
                // CSG subtract: A minus B in SDF land is max(A, -B).
                let mut new = old.max(-d_cut);
                // Workbench half-space clip, same as SphereBrush.
                if let Some(wb_y) = cutter.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }
                if new != old {
                    on_pre_mutation(ix, iy, iz, old);
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    Some(DirtyRegion { min, max })
}

/// Apply one wire-cutter stroke. Convenience wrapper with an empty
/// callback, matching the shape of `apply_sphere_brush`.
pub fn apply_wire_cutter(grid: &mut Grid, cutter: &WireCutter) -> Option<DirtyRegion> {
    apply_wire_cutter_with_callback(grid, cutter, |_, _, _, _| {})
}

/// Callback variant. `on_pre_mutation` fires once per voxel that is
/// about to change with the pre-mutation SDF value — for feeding the
/// undo journal, same as the other cutter variants.
pub fn apply_wire_cutter_with_callback<F>(
    grid: &mut Grid,
    cutter: &WireCutter,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let vs = grid.voxel_size();
    let res = grid.res();
    let half_thick = cutter.thickness * 0.5;
    // One voxel of margin so surface-nets sees a continuous field
    // across the slab boundary and doesn't emit zig-zag artefacts
    // along the cut edge.
    let margin = vs;

    // The slab is infinite in two directions, so we iterate every
    // voxel but early-exit for voxels far from the plane. At 128^3
    // that's ~2M ops per stroke — comfortably under 30 ms.
    // Dirty AABB is grown dynamically as we go.
    let mut dirty_min = UVec3::MAX;
    let mut dirty_max = UVec3::ZERO;
    let mut touched = false;

    let n = cutter.normal;
    let a = cutter.anchor;

    for iz in 0..res.z {
        for iy in 0..res.y {
            for ix in 0..res.x {
                let p = grid.position(ix, iy, iz);
                // Signed distance to the cut plane. Slab SDF is
                // |signed_dist| - half_thick: negative inside the
                // slab, positive outside.
                let signed = (p - a).dot(n);
                let d_slab = signed.abs() - half_thick;
                if d_slab > margin {
                    continue;
                }

                let old = grid.get(ix, iy, iz);
                // CSG subtract the slab.
                let mut new = old.max(-d_slab);
                if let Some(wb_y) = cutter.workbench_y {
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
    fn circle_cutter_removes_a_cylindrical_channel() {
        let g_res = UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);

        // Cut a 4 mm radius channel straight down through the ball.
        let cutter = CookieCutter {
            profile: Profile::Circle { radius: 4.0 },
            origin: Vec3::new(32.0, 32.0, 32.0),
            axis: Vec3::Y,
            half_length: 50.0,
            workbench_y: None,
        };
        let region = apply_cookie_cutter(&mut g, &cutter);
        assert!(region.is_some());

        // Centre of the channel: should be positive (carved out).
        assert!(g.sample(Vec3::new(32.0, 32.0, 32.0)) > 0.0);
        // Just outside the channel radius, still inside the ball:
        // should still be negative (material there).
        assert!(g.sample(Vec3::new(38.0, 32.0, 32.0)) < 0.0);
        // Directly above the ball, in empty air: unchanged (positive).
        assert!(g.sample(Vec3::new(32.0, 55.0, 32.0)) > 0.0);
    }

    #[test]
    fn square_cutter_removes_a_square_channel() {
        let g_res = UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);

        let cutter = CookieCutter {
            profile: Profile::Square { half_side: 3.0 },
            origin: Vec3::new(32.0, 32.0, 32.0),
            axis: Vec3::Y,
            half_length: 50.0,
            workbench_y: None,
        };
        let _ = apply_cookie_cutter(&mut g, &cutter);

        // Centre: carved.
        assert!(g.sample(Vec3::new(32.0, 32.0, 32.0)) > 0.0);
        // Along the diagonal at radius > 3√2 ≈ 4.24, still inside
        // the ball: material there.
        assert!(g.sample(Vec3::new(37.0, 32.0, 32.0)) < 0.0);
    }

    #[test]
    fn wire_cutter_splits_a_sphere_into_two_disconnected_regions() {
        let g_res = UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 15.0);

        // Slice through the middle of the ball, perpendicular to X.
        let cutter = WireCutter {
            anchor: Vec3::new(32.0, 32.0, 32.0),
            normal: Vec3::new(1.0, 0.0, 0.0),
            thickness: 2.0,
            workbench_y: None,
        };
        let region = apply_wire_cutter(&mut g, &cutter);
        assert!(region.is_some());

        // At the plane: carved (positive SDF).
        assert!(g.sample(Vec3::new(32.0, 32.0, 32.0)) > 0.0);

        // Well inside the two halves, still solid (negative SDF).
        // Left half at x ≈ 25.
        assert!(g.sample(Vec3::new(25.0, 32.0, 32.0)) < 0.0);
        // Right half at x ≈ 39.
        assert!(g.sample(Vec3::new(39.0, 32.0, 32.0)) < 0.0);
    }

    #[test]
    fn wire_cutter_workbench_clip_is_respected() {
        let g_res = UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(16.0, 20.0, 16.0), 6.0);
        let cutter = WireCutter {
            anchor: Vec3::new(16.0, 6.0, 16.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            thickness: 6.0,
            workbench_y: Some(8.0),
        };
        let _ = apply_wire_cutter(&mut g, &cutter);
        let below = g.sample(Vec3::new(16.0, 3.0, 16.0));
        assert!(below > 0.0, "voxel below workbench should be outside, got {below}");
    }

    #[test]
    fn cutter_workbench_clip_is_respected() {
        let g_res = UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(16.0, 16.0, 16.0), 6.0);
        // Cutter that would try to write below workbench Y=8; the clip
        // should keep those voxels safely outside the workpiece.
        let cutter = CookieCutter {
            profile: Profile::Circle { radius: 4.0 },
            origin: Vec3::new(16.0, 8.0, 16.0),
            axis: Vec3::Y,
            half_length: 20.0,
            workbench_y: Some(8.0),
        };
        let _ = apply_cookie_cutter(&mut g, &cutter);
        // Below the workbench: must be outside (positive SDF).
        let below = g.sample(Vec3::new(16.0, 4.0, 16.0));
        assert!(
            below > 0.0,
            "voxel below workbench should not become material, got {below}"
        );
    }
}
