use glam::{UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};

/// Which side of the "displace vs. remove" line this brush sits on.
///
/// This is a per-tool property in the finished product (see DESIGN §5).
/// In Stage 1 the finger brush supports both because the app is still
/// exposing one tool with two modes.
#[derive(Copy, Clone, Debug)]
pub enum BrushMode {
    /// Carve material away. The brush footprint is subtracted from
    /// the workpiece. When paired with `SphereBrush::displace = true`,
    /// the displaced material bulges out around the brush — the core
    /// of Stage 1's "magic clay" behaviour.
    Press,
    /// Add material. The brush footprint is unioned into the workpiece.
    /// Stage 1 does not do redistribution for Pull; that would be the
    /// "smear" operation and lives in a later stage.
    Pull,
}

/// A single-step spherical brush stamp.
///
/// - `center`, `radius` — in piece-local mm.
/// - `mode` — Press or Pull.
/// - `direction` — unit vector pointing *into* the workpiece surface
///   at the contact point (i.e. `-surface_normal`). Only meaningful
///   when `displace = true`; the bulge is biased perpendicular and
///   opposite to this direction, so material squeezes out sideways
///   and behind the press, not into the press direction.
/// - `displace` — enable the volume-displacement bulge (magic clay).
///   When false the stamp is a plain hard CSG operation, matching
///   Stage 0 behaviour.
/// - `workbench_y` — if set, no voxel below this Y coordinate is
///   allowed to become "inside" the workpiece. Enforces the workbench
///   plane as a hard floor without a separate physics pass.
#[derive(Copy, Clone, Debug)]
pub struct SphereBrush {
    pub center: Vec3,
    pub radius: f32,
    pub mode: BrushMode,
    pub direction: Vec3,
    pub displace: bool,
    pub workbench_y: Option<f32>,
}

/// Apply one stamp of a spherical brush to the grid. Returns the dirty
/// voxel AABB so the caller can queue chunk re-meshing.
///
/// This is a thin wrapper around [`apply_sphere_brush_with_callback`]
/// that discards the pre-mutation values. Callers who want to journal
/// undo state should call the callback variant directly.
pub fn apply_sphere_brush(grid: &mut Grid, brush: &SphereBrush) -> Option<DirtyRegion> {
    apply_sphere_brush_with_callback(grid, brush, |_, _, _, _| {})
}

/// Same as [`apply_sphere_brush`] but with a callback fired for every
/// voxel that is *about to change*. The callback receives the voxel
/// index and the pre-mutation SDF value, in time to save it into an
/// undo journal.
pub fn apply_sphere_brush_with_callback<F>(
    grid: &mut Grid,
    brush: &SphereBrush,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    // Bulge geometry constants. Tuned so repeated presses / pulls
    // read as clay rather than eraser / add-material, for brush radii
    // in the 8-30 mm range:
    // - `BULGE_THICKNESS_FACTOR`: ring width relative to brush radius.
    //   Wider ring = more of the surrounding surface participates.
    // - `BULGE_INTENSITY_FACTOR`: max SDF change per stamp at peak
    //   weight, relative to brush radius. Higher = more visible pile-
    //   up per frame.
    //
    // Iteration history:
    // - Stage 1 initial: 0.45 / 0.18 read as too subtle in a real
    //   user session ("feels more like eraser and add-material than
    //   clay"). Bumped up here.
    const BULGE_THICKNESS_FACTOR: f32 = 0.60;
    const BULGE_INTENSITY_FACTOR: f32 = 0.32;

    let bulge_thickness = if brush.displace {
        brush.radius * BULGE_THICKNESS_FACTOR
    } else {
        0.0
    };
    let bulge_intensity = brush.radius * BULGE_INTENSITY_FACTOR;

    // "Surface band" is the range of pre-stamp SDF values around zero
    // where we consider a voxel to be "on the surface" for bulge
    // weighting. Slightly larger than the ring so the bulge picks up
    // the surface reliably even when the brush partially misses.
    let surface_band = bulge_thickness * 1.2 + grid.voxel_size();

    let effective_radius = brush.radius + bulge_thickness + grid.voxel_size();

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
    let dir = brush.direction;

    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                let old = grid.get(ix, iy, iz);

                // Base CSG operation.
                let mut new = match brush.mode {
                    // Subtract: A minus B in SDF land is max(A, -B).
                    BrushMode::Press => old.max(-d_brush),
                    // Union: A ∪ B in SDF land is min(A, B).
                    BrushMode::Pull => old.min(d_brush),
                };

                // Volume-displacement bulge. Fires with `displace = true`
                // for both Press and Pull, in the annular ring just
                // outside the brush footprint, only where the pre-stamp
                // field was near the surface. Direction weight biases
                // perpendicular / opposite to the tool direction so
                // material squeezes out (Press) or is recruited from
                // the surrounding surface (Pull), instead of appearing
                // straight ahead or behind.
                //
                // Sign convention:
                // - Press: material squeezed OUT of the brush footprint
                //   piles up in the ring. SDF becomes more negative
                //   (surface pushed outward) → `new -= bulge`.
                // - Pull:  material recruited INTO the pulled bulge is
                //   drawn from the surrounding surface. SDF becomes
                //   more positive (surrounding surface retreats
                //   inward) → `new += bulge`. This is what turns Pull
                //   from "spawn material from air" into "smear".
                if brush.displace && d_brush > 0.0 && d_brush < bulge_thickness {
                    let surface_weight = (1.0 - (old.abs() / surface_band).min(1.0)).max(0.0);
                    if surface_weight > 0.0 {
                        let ring_t = (d_brush / bulge_thickness).clamp(0.0, 1.0);
                        // Press piles material against the tool face, so
                        // weight peaks at the brush rim (`ring_t → 0`).
                        // Pull must *not* peak there: recruiting at the
                        // contact seam digs a trench around the freshly
                        // unioned blob (visible as a moat / glitchy
                        // silhouette). Instead peak mid-shoulder and
                        // fall to zero at the rim and outer edge.
                        let ring_weight = match brush.mode {
                            BrushMode::Press => (1.0 - ring_t) * (1.0 - ring_t),
                            BrushMode::Pull => 4.0 * ring_t * (1.0 - ring_t) * (1.0 - ring_t),
                        };

                        let bulge_dir = (p - c) * (1.0 / (d_brush + r).max(1e-6));
                        let cos_theta = bulge_dir.dot(dir);
                        let dir_weight = ((1.0 - cos_theta) * 0.5).clamp(0.0, 1.0);

                        let bulge = bulge_intensity * ring_weight * surface_weight * dir_weight;
                        match brush.mode {
                            BrushMode::Press => new -= bulge,
                            BrushMode::Pull => new += bulge,
                        }
                    }
                }

                // Workbench constraint: no material below the workbench.
                // Equivalent to intersecting the workpiece with the
                // upper half-space { y >= workbench_y }, whose SDF is
                // `workbench_y - y` (positive below the plane).
                if let Some(wb_y) = brush.workbench_y {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    fn plain_press_brush(center: Vec3, radius: f32) -> SphereBrush {
        SphereBrush {
            center,
            radius,
            mode: BrushMode::Press,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        }
    }

    fn plain_pull_brush(center: Vec3, radius: f32) -> SphereBrush {
        SphereBrush {
            center,
            radius,
            mode: BrushMode::Pull,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        }
    }

    #[test]
    fn press_removes_material_inside_brush() {
        let g_res = glam::UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(16.0, 16.0, 16.0), 8.0);
        assert!(g.sample(Vec3::new(16.0, 16.0, 16.0)) < 0.0);
        let dirty = apply_sphere_brush(&mut g, &plain_press_brush(Vec3::new(16.0, 16.0, 16.0), 20.0));
        assert!(dirty.is_some());
        assert!(g.sample(Vec3::new(16.0, 16.0, 16.0)) > 15.0);
    }

    #[test]
    fn pull_adds_material_outside_brush() {
        let g_res = glam::UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(8.0, 16.0, 16.0), 4.0);
        assert!(g.sample(Vec3::new(20.0, 16.0, 16.0)) > 0.0);
        let _ = apply_sphere_brush(&mut g, &plain_pull_brush(Vec3::new(20.0, 16.0, 16.0), 4.0));
        assert!(g.sample(Vec3::new(20.0, 16.0, 16.0)) < 0.0);
    }

    #[test]
    fn press_outside_workpiece_is_a_noop_on_the_surface() {
        let g_res = glam::UVec3::new(32, 32, 32);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(8.0, 16.0, 16.0), 4.0);
        let before = g.sample(Vec3::new(8.0, 16.0, 16.0));
        let _ = apply_sphere_brush(&mut g, &plain_press_brush(Vec3::new(28.0, 28.0, 28.0), 2.0));
        let after = g.sample(Vec3::new(8.0, 16.0, 16.0));
        assert!((before - after).abs() < 1e-4);
    }

    #[test]
    fn press_with_displacement_bulges_surface_sideways() {
        // Big workpiece so the brush is comfortably embedded.
        let g_res = glam::UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);

        // Brush at the workpiece surface (radius 20 → x=52), press
        // direction along -x (into the surface).
        //
        // Probe: perpendicular to press (+y), just outside the brush
        // footprint (radius 5), sitting near the workpiece surface at
        // that latitude. Concretely: at (52, 38, 32), the distance to
        // the brush centre (52, 32, 32) is 6 mm — inside the bulge
        // ring — and the distance to the workpiece centre
        // (32, 32, 32) is ~20.88 mm, so the SDF here starts near
        // zero. This voxel is precisely where "material squeezing
        // out sideways" should register.
        let probe = Vec3::new(52.0, 38.0, 32.0);
        let before = g.sample(probe);
        assert!(
            before > 0.0 && before < 2.0,
            "probe should sit just outside the workpiece surface: before={before}",
        );

        let brush = SphereBrush {
            center: Vec3::new(52.0, 32.0, 32.0),
            radius: 5.0,
            mode: BrushMode::Press,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: true,
            workbench_y: None,
        };
        // 40 stamps ≈ 2/3 of a second of held press at 60 fps.
        for _ in 0..40 {
            let _ = apply_sphere_brush(&mut g, &brush);
        }
        let after = g.sample(probe);
        assert!(
            after < before - 0.5,
            "displacement should bulge the surface toward the probe: before={before}, after={after}",
        );
    }

    #[test]
    fn pull_with_displacement_recruits_from_surrounding_surface() {
        // Big workpiece so the brush is comfortably at the surface.
        let g_res = glam::UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);

        // Mid-shoulder probe in the bulge ring (d_brush ≈ 1 mm with
        // thickness 3 mm → ring_t ≈ 1/3), near the workpiece surface,
        // perpendicular to the tool direction. Pull recruitment should
        // retreat the surrounding surface here (SDF grows).
        let probe = Vec3::new(52.0, 38.0, 32.0);
        let before = g.sample(probe);

        let brush = SphereBrush {
            center: Vec3::new(52.0, 32.0, 32.0),
            radius: 5.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: true,
            workbench_y: None,
        };
        for _ in 0..40 {
            let _ = apply_sphere_brush(&mut g, &brush);
        }
        let after = g.sample(probe);
        assert!(
            after > before + 0.3,
            "pull recruitment should retreat the surrounding surface (SDF should grow): before={before}, after={after}",
        );
    }

    #[test]
    fn pull_with_displacement_does_not_trench_at_brush_rim() {
        // Regression for the "moat" around Shift+LMB pulls: recruitment
        // used to peak at ring_t→0 and carve a trench at the contact
        // seam. The rim probe sits just outside the brush footprint on
        // the workpiece surface; after many Pull stamps its SDF must
        // not jump positive far past the no-displace baseline.
        let g_res = glam::UVec3::new(64, 64, 64);
        let centre = Vec3::new(32.0, 32.0, 32.0);
        let brush_c = Vec3::new(52.0, 32.0, 32.0);
        let radius = 5.0;
        // Just outside the brush sphere, on the +Y side of the tip —
        // the contact seam where the old ring weight peaked.
        let rim = Vec3::new(52.0, 37.25, 32.0);

        let mut g_plain =
            Grid::from_sphere(g_res, 1.0, Vec3::ZERO, centre, 20.0);
        let mut g_disp =
            Grid::from_sphere(g_res, 1.0, Vec3::ZERO, centre, 20.0);

        let plain = SphereBrush {
            center: brush_c,
            radius,
            mode: BrushMode::Pull,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        let with_disp = SphereBrush {
            displace: true,
            ..plain
        };
        for _ in 0..40 {
            let _ = apply_sphere_brush(&mut g_plain, &plain);
            let _ = apply_sphere_brush(&mut g_disp, &with_disp);
        }

        let plain_rim = g_plain.sample(rim);
        let disp_rim = g_disp.sample(rim);
        // Displacement may retreat the mid-ring a little, but the rim
        // must not be dug into empty space relative to plain union.
        assert!(
            disp_rim < plain_rim + 0.75,
            "pull displace must not dig a trench at the brush rim: plain={plain_rim}, displace={disp_rim}",
        );
        // And the rim should still read as solid / near-surface, not a
        // carved groove (large positive SDF).
        assert!(
            disp_rim < 1.5,
            "pull rim should stay near/inside the surface, got SDF={disp_rim}",
        );
    }

    #[test]
    fn press_without_displacement_matches_stage0_behaviour() {
        // With `displace = false`, Stage 1 press should behave
        // identically to Stage 0 — no bulge outside the brush ring.
        let g_res = glam::UVec3::new(64, 64, 64);
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 20.0);
        let probe = Vec3::new(52.0, 38.0, 32.0);
        let before = g.sample(probe);

        let brush = SphereBrush {
            center: Vec3::new(52.0, 32.0, 32.0),
            radius: 5.0,
            mode: BrushMode::Press,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        for _ in 0..40 {
            let _ = apply_sphere_brush(&mut g, &brush);
        }
        let after = g.sample(probe);
        // The probe is *outside* the brush footprint (d_brush > 0),
        // so hard CSG shouldn't touch it at all.
        assert!(
            (after - before).abs() < 1e-4,
            "no-displace mode should leave outside-ring voxels alone: before={before}, after={after}",
        );
    }

    #[test]
    fn workbench_clip_prevents_material_below_plane() {
        let g_res = glam::UVec3::new(32, 32, 32);
        // Grid with y from 0 to 32. Workbench at y=8. Initial workpiece
        // is a sphere entirely above y=8.
        let mut g = Grid::from_sphere(g_res, 1.0, Vec3::ZERO, Vec3::new(16.0, 20.0, 16.0), 6.0);

        // Pull a lot of material downward, straddling the workbench.
        let brush = SphereBrush {
            center: Vec3::new(16.0, 6.0, 16.0),
            radius: 10.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(0.0, -1.0, 0.0),
            displace: false,
            workbench_y: Some(8.0),
        };
        let _ = apply_sphere_brush(&mut g, &brush);

        // Below the workbench, the SDF should be positive (outside
        // workpiece) regardless of what Pull tried to do.
        let below = g.sample(Vec3::new(16.0, 3.0, 16.0));
        assert!(
            below > 0.0,
            "voxel below workbench should be outside the workpiece, got SDF={below}"
        );

        // Above the workbench, Pull should have worked normally.
        let above = g.sample(Vec3::new(16.0, 10.0, 16.0));
        assert!(
            above < 0.0,
            "voxel above workbench should be inside the workpiece, got SDF={above}"
        );
    }
}
