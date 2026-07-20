//! Paddle tool — a flat contact-stamp for making flats.
//!
//! Physically the paddle is a rigid flat plate you press against clay
//! to squash a region into a plane. Digitally we model it as a disk-
//! shaped half-space press: pick a plane (`center` + outward `normal`)
//! and a disk `radius`; material within the disk on the paddle side of
//! the plane is depressed, and the lost volume is redistributed into a
//! rim around the disk (DESIGN §2.4 / §5 — paddle *displaces*).
//!
//! Progressive-feel behaviour: the caller offsets the `center` a bit
//! deeper into the surface each frame the button is held, so the
//! flat grows in one direction until the plane is buried.

use glam::{UVec3, Vec3};

use crate::displace::{expand_region, redistance_local, solid_volume_in_region};
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

/// Apply one paddle stamp (displace). Convenience wrapper.
pub fn apply_paddle(grid: &mut Grid, paddle: &Paddle) -> Option<DirtyRegion> {
    apply_paddle_with_callback(grid, paddle, |_, _, _, _| {})
}

/// Callback variant — pre-mutation values for the undo journal.
///
/// 1. Flatten (hard half-space ∩ disk).
/// 2. Measure `∆V⁻`.
/// 3. Recruit into an annular rim just outside the disk.
/// 4. Local redistance.
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
    let r = paddle.radius.max(1e-3);
    let rim = r * 0.55;
    let surface_band = rim * 1.2 + vs;

    let n = {
        let len = paddle.normal.length();
        if len > 1e-6 {
            paddle.normal / len
        } else {
            Vec3::Y
        }
    };
    let c = paddle.center;

    // Edit AABB covers disk + rim ring.
    let bound = r + rim + vs * 2.0;
    let min_p = (c - Vec3::splat(bound) - grid.origin()) * inv_vs;
    let max_p = (c + Vec3::splat(bound) - grid.origin()) * inv_vs;
    let edit_min = UVec3::new(
        min_p.x.floor().max(0.0) as u32,
        min_p.y.floor().max(0.0) as u32,
        min_p.z.floor().max(0.0) as u32,
    )
    .min(res - UVec3::ONE);
    let edit_max = UVec3::new(
        (max_p.x.ceil() as i32).max(0) as u32,
        (max_p.y.ceil() as i32).max(0) as u32,
        (max_p.z.ceil() as i32).max(0) as u32,
    )
    .min(res);

    if edit_min.x >= edit_max.x || edit_min.y >= edit_max.y || edit_min.z >= edit_max.z {
        return None;
    }

    let vol_before = solid_volume_in_region(grid, edit_min, edit_max);

    // --- 1) Primary edit: flatten within the disk -------------------
    let mut touched = false;
    let mut dirty_min = UVec3::MAX;
    let mut dirty_max = UVec3::ZERO;

    for iz in edit_min.z..edit_max.z {
        for iy in edit_min.y..edit_max.y {
            for ix in edit_min.x..edit_max.x {
                let p = grid.position(ix, iy, iz);
                let d = p - c;
                let axial = d.dot(n);
                let radial = (d - n * axial).length();
                if radial >= r {
                    continue;
                }
                // Same CSG as the old carve paddle — depresses above
                // the plane inside the disk.
                let paddle_neg_sdf = axial.min(r - radial);
                let old = grid.get(ix, iy, iz);
                let mut new = old.max(paddle_neg_sdf);
                if let Some(wb_y) = paddle.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }
                if (new - old).abs() > 1e-7 {
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

    if !touched {
        return None;
    }

    let vol_after = solid_volume_in_region(grid, edit_min, edit_max);
    let delta_v = (vol_before - vol_after).max(0.0);
    let min_v = vs * vs * vs * 0.25;

    // --- 2) Recruit lost volume into the annular rim ----------------
    if delta_v >= min_v {
        let mut weights: Vec<(u32, u32, u32, f32)> = Vec::new();
        let mut weight_sum = 0.0f32;

        for iz in edit_min.z..edit_max.z {
            for iy in edit_min.y..edit_max.y {
                for ix in edit_min.x..edit_max.x {
                    let p = grid.position(ix, iy, iz);
                    let d = p - c;
                    let axial = d.dot(n);
                    let radial = (d - n * axial).length();
                    // Annulus just outside the disk.
                    if radial < r || radial >= r + rim {
                        continue;
                    }
                    // Prefer near the plane (surface band) and slightly
                    // below / at the flat so clay squeezes out beside
                    // the paddle rather than into the air above it.
                    if axial > surface_band {
                        continue;
                    }
                    let phi = grid.get(ix, iy, iz);
                    let surface_weight = (1.0 - (phi.abs() / surface_band).min(1.0)).max(0.0);
                    if surface_weight <= 1e-4 {
                        continue;
                    }
                    let ring_t = ((radial - r) / rim).clamp(0.0, 1.0);
                    let ring_weight = (1.0 - ring_t) * (1.0 - ring_t);
                    // Favour voxels near/below the plane.
                    let plane_weight = (1.0 - (axial.abs() / surface_band).min(1.0)).max(0.15);
                    let w = ring_weight * surface_weight * plane_weight;
                    if w > 1e-5 {
                        weights.push((ix, iy, iz, w));
                        weight_sum += w;
                    }
                }
            }
        }

        if weight_sum > 1e-6 {
            let scale = (2.0 / (vs * vs).max(1e-8)) * (delta_v / weight_sum);
            for (ix, iy, iz, w) in weights {
                let old = grid.get(ix, iy, iz);
                let mut new = old - scale * w;
                if let Some(wb_y) = paddle.workbench_y {
                    let p = grid.position(ix, iy, iz);
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }
                if (new - old).abs() > 1e-7 {
                    on_pre_mutation(ix, iy, iz, old);
                    grid.set(ix, iy, iz, new);
                    let v = UVec3::new(ix, iy, iz);
                    dirty_min = dirty_min.min(v);
                    dirty_max = dirty_max.max(v + UVec3::ONE);
                }
            }
        }
    }

    // --- 3) Local redistance ----------------------------------------
    let (rmin, rmax) = expand_region(grid, edit_min, edit_max, 2);
    redistance_local(grid, rmin, rmax, 4, &mut on_pre_mutation);

    Some(DirtyRegion {
        min: rmin.min(dirty_min),
        max: rmax.max(dirty_max),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere_grid() -> Grid {
        Grid::from_sphere(
            UVec3::new(64, 64, 64),
            1.0,
            Vec3::ZERO,
            Vec3::new(32.0, 32.0, 32.0),
            20.0,
        )
    }

    #[test]
    fn paddle_flattens_a_bump() {
        let mut g = sphere_grid();
        let top_of_ball = g.sample(Vec3::new(32.0, 52.0, 32.0));
        assert!(top_of_ball.abs() < 1.0, "top of ball should be on surface");
        assert!(g.sample(Vec3::new(32.0, 48.0, 32.0)) < 0.0);

        let paddle = Paddle {
            center: Vec3::new(32.0, 45.0, 32.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            radius: 15.0,
            workbench_y: None,
        };
        let _ = apply_paddle(&mut g, &paddle);

        assert!(g.sample(Vec3::new(32.0, 48.0, 32.0)) > 0.0);
        assert!(g.sample(Vec3::new(32.0, 40.0, 32.0)) < 0.0);
    }

    #[test]
    fn paddle_leaves_material_outside_the_disk() {
        let mut g = sphere_grid();
        let before = g.sample(Vec3::new(45.0, 45.0, 32.0));
        let paddle = Paddle {
            center: Vec3::new(32.0, 45.0, 32.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            radius: 5.0,
            workbench_y: None,
        };
        let _ = apply_paddle(&mut g, &paddle);
        let after = g.sample(Vec3::new(45.0, 45.0, 32.0));
        // Far outside disk+rim — untouched.
        assert!(
            (before - after).abs() < 1e-3,
            "outside the disk should be untouched: before={before}, after={after}",
        );
    }

    #[test]
    fn paddle_roughly_conserves_volume() {
        let mut g = sphere_grid();
        let v0 = solid_volume_in_region(&g, UVec3::ZERO, g.res());
        let paddle = Paddle {
            center: Vec3::new(32.0, 46.0, 32.0),
            normal: Vec3::Y,
            radius: 10.0,
            workbench_y: None,
        };
        let _ = apply_paddle(&mut g, &paddle);
        let v1 = solid_volume_in_region(&g, UVec3::ZERO, g.res());
        let rel = ((v1 - v0) / v0).abs();
        assert!(
            rel < 0.05,
            "paddle displace should keep volume within ~5%: v0={v0}, v1={v1}, rel={rel}"
        );
    }

    #[test]
    fn paddle_builds_a_rim_outside_the_disk() {
        let mut g = sphere_grid();
        // Probe just outside a moderate disk, near the plane height.
        let paddle = Paddle {
            center: Vec3::new(32.0, 46.0, 32.0),
            normal: Vec3::Y,
            radius: 8.0,
            workbench_y: None,
        };
        let probe = Vec3::new(32.0 + 9.5, 45.5, 32.0);
        let before = g.sample(probe);
        let _ = apply_paddle(&mut g, &paddle);
        let after = g.sample(probe);
        assert!(
            after < before - 0.05,
            "rim outside disk should gain material (φ↓): before={before}, after={after}"
        );
    }
}
