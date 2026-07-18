//! Volume-conserving Press / Pull displace (DESIGN.md §2.4).
//!
//! Separate from the Clay Add/Remove brush (`brush.rs`), which stays
//! soft-CSG ± heuristic bulge.
//!
//! **Press:** depress → measure `∆V⁻` → pile into a rim → redistance.
//! **Pull:** grow outward → measure `∆V⁺` → thin the rim → redistance.

use glam::{UVec3, Vec3};

use crate::brush::{soft_max, soft_min};
use crate::grid::{DirtyRegion, Grid};

/// Spherical finger Press — displaces volume rather than deleting it.
#[derive(Copy, Clone, Debug)]
pub struct PressDisplace {
    pub center: Vec3,
    pub radius: f32,
    /// Unit vector pointing *into* the workpiece at contact (`-n`).
    pub direction: Vec3,
    pub workbench_y: Option<f32>,
}

/// Spherical finger Pull — grows the surface and draws volume from
/// the surrounding rim (inverse of [`PressDisplace`]).
#[derive(Copy, Clone, Debug)]
pub struct PullDisplace {
    pub center: Vec3,
    pub radius: f32,
    /// Unit vector pointing *into* the workpiece at contact (`-n`).
    pub direction: Vec3,
    pub workbench_y: Option<f32>,
}

/// Smooth occupancy in `0..=1` from an SDF sample. Solid → 1, air → 0,
/// linear blend across one voxel of the band.
#[inline]
fn occupancy(phi: f32, vs: f32) -> f32 {
    let vs = vs.max(1e-6);
    if phi <= -vs {
        1.0
    } else if phi >= vs {
        0.0
    } else {
        0.5 - 0.5 * (phi / vs)
    }
}

/// Estimate solid volume (mm³) inside `[min, max)` from occupancy.
pub fn solid_volume_in_region(grid: &Grid, min: UVec3, max: UVec3) -> f32 {
    let vs = grid.voxel_size();
    let cell = vs * vs * vs;
    let mut v = 0.0f32;
    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                v += occupancy(grid.get(ix, iy, iz), vs) * cell;
            }
        }
    }
    v
}

fn clamp_aabb(grid: &Grid, center: Vec3, radius: f32) -> Option<(UVec3, UVec3)> {
    let vs = grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = grid.res();
    let min_p = (center - Vec3::splat(radius) - grid.origin()) * inv_vs;
    let max_p = (center + Vec3::splat(radius) - grid.origin()) * inv_vs;
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
        None
    } else {
        Some((min, max))
    }
}

fn expand_region(grid: &Grid, min: UVec3, max: UVec3, pad: u32) -> (UVec3, UVec3) {
    let res = grid.res();
    let min = UVec3::new(
        min.x.saturating_sub(pad),
        min.y.saturating_sub(pad),
        min.z.saturating_sub(pad),
    );
    let max = UVec3::new(
        (max.x + pad).min(res.x),
        (max.y + pad).min(res.y),
        (max.z + pad).min(res.z),
    );
    (min, max)
}

/// Apply one Press stamp. Returns the dirty AABB covering stamp +
/// recruitment + redistance halo.
pub fn apply_press_displace(grid: &mut Grid, brush: &PressDisplace) -> Option<DirtyRegion> {
    apply_press_displace_with_callback(grid, brush, |_, _, _, _| {})
}

/// Same as [`apply_press_displace`] with an undo pre-mutation callback.
/// Fired once per voxel before its first write in this stamp.
pub fn apply_press_displace_with_callback<F>(
    grid: &mut Grid,
    brush: &PressDisplace,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let r = brush.radius.max(1e-3);
    // Rim outside the tool where displaced clay piles up.
    let rim = r * 0.75;
    let soft_k = r * 0.40;
    let surface_band = rim * 1.15 + grid.voxel_size();

    let Some((stamp_min, stamp_max)) = clamp_aabb(grid, brush.center, r + grid.voxel_size()) else {
        return None;
    };
    let Some((edit_min, edit_max)) =
        clamp_aabb(grid, brush.center, r + rim + grid.voxel_size() * 2.0)
    else {
        return None;
    };

    let vs = grid.voxel_size();
    let vol_before = solid_volume_in_region(grid, stamp_min, stamp_max);

    // --- 1) Primary edit: soft spherical depression -----------------
    let c = brush.center;
    let dir = {
        let len = brush.direction.length();
        if len > 1e-6 {
            brush.direction / len
        } else {
            Vec3::NEG_Y
        }
    };

    for iz in stamp_min.z..stamp_max.z {
        for iy in stamp_min.y..stamp_max.y {
            for ix in stamp_min.x..stamp_max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                let old = grid.get(ix, iy, iz);
                let mut new = soft_max(old, -d_brush, soft_k);
                if let Some(wb_y) = brush.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }
                if (new - old).abs() > 1e-7 {
                    on_pre_mutation(ix, iy, iz, old);
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    let vol_after_stamp = solid_volume_in_region(grid, stamp_min, stamp_max);
    let delta_v = (vol_before - vol_after_stamp).max(0.0);
    // Tiny bites aren't worth recruiting — avoids noise amplify.
    let min_v = vs * vs * vs * 0.25;
    if delta_v < min_v {
        let (rmin, rmax) = expand_region(grid, stamp_min, stamp_max, 1);
        redistance_local(grid, rmin, rmax, 2);
        return Some(DirtyRegion {
            min: rmin,
            max: rmax,
        });
    }

    // --- 2) Build recruitment weights in the rim --------------------
    // Weight peaks just outside the tool, on the surface band, biased
    // sideways / behind the press (opposite `dir`).
    let mut weights: Vec<(u32, u32, u32, f32)> = Vec::new();
    let mut weight_sum = 0.0f32;

    for iz in edit_min.z..edit_max.z {
        for iy in edit_min.y..edit_max.y {
            for ix in edit_min.x..edit_max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                if d_brush <= 0.0 || d_brush >= rim {
                    continue;
                }
                let phi = grid.get(ix, iy, iz);
                let surface_weight = (1.0 - (phi.abs() / surface_band).min(1.0)).max(0.0);
                if surface_weight <= 1e-4 {
                    continue;
                }
                let ring_t = (d_brush / rim).clamp(0.0, 1.0);
                // Peak near the tool rim, fall off outward.
                let ring_weight = (1.0 - ring_t) * (1.0 - ring_t);
                let radial = (p - c).normalize_or_zero();
                let cos_theta = radial.dot(dir);
                // Prefer sides / behind: low when aligned with press.
                let dir_weight = ((1.0 - cos_theta) * 0.5).clamp(0.05, 1.0);
                let w = ring_weight * surface_weight * dir_weight;
                if w > 1e-5 {
                    weights.push((ix, iy, iz, w));
                    weight_sum += w;
                }
            }
        }
    }

    if weight_sum > 1e-6 {
        // Linearised occupancy: Δocc ≈ (0.5/vs)·δφ_decrease
        // → δφ = (2/vs²)·ΔV_i  with  ΔV_i = ∆V · w_i / W
        let scale = (2.0 / (vs * vs).max(1e-8)) * (delta_v / weight_sum);
        for (ix, iy, iz, w) in weights {
            let old = grid.get(ix, iy, iz);
            let mut new = old - scale * w;
            if let Some(wb_y) = brush.workbench_y {
                let p = grid.position(ix, iy, iz);
                let below = wb_y - p.y;
                if below > new {
                    new = below;
                }
            }
            if (new - old).abs() > 1e-7 {
                on_pre_mutation(ix, iy, iz, old);
                grid.set(ix, iy, iz, new);
            }
        }
    }

    // --- 3) Local redistance ----------------------------------------
    let (rmin, rmax) = expand_region(grid, edit_min, edit_max, 2);
    redistance_local(grid, rmin, rmax, 4);

    Some(DirtyRegion {
        min: rmin,
        max: rmax,
    })
}

/// Apply one Pull stamp. Returns the dirty AABB covering stamp +
/// draw-from-rim + redistance halo.
pub fn apply_pull_displace(grid: &mut Grid, brush: &PullDisplace) -> Option<DirtyRegion> {
    apply_pull_displace_with_callback(grid, brush, |_, _, _, _| {})
}

/// Same as [`apply_pull_displace`] with an undo pre-mutation callback.
pub fn apply_pull_displace_with_callback<F>(
    grid: &mut Grid,
    brush: &PullDisplace,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let r = brush.radius.max(1e-3);
    let rim = r * 0.85;
    let soft_k = r * 0.40;
    let surface_band = rim * 1.15 + grid.voxel_size();

    let Some((stamp_min, stamp_max)) = clamp_aabb(grid, brush.center, r + grid.voxel_size()) else {
        return None;
    };
    let Some((edit_min, edit_max)) =
        clamp_aabb(grid, brush.center, r + rim + grid.voxel_size() * 2.0)
    else {
        return None;
    };

    let vs = grid.voxel_size();
    // Measure volume in the wider edit region so rim thinning is
    // visible in the conservation check; stamp AABB alone undercounts
    // the draw-from-neighbourhood step.
    let vol_before = solid_volume_in_region(grid, edit_min, edit_max);

    let c = brush.center;
    let dir = {
        let len = brush.direction.length();
        if len > 1e-6 {
            brush.direction / len
        } else {
            Vec3::NEG_Y
        }
    };

    // --- 1) Primary edit: soft spherical grow (union) ---------------
    for iz in stamp_min.z..stamp_max.z {
        for iy in stamp_min.y..stamp_max.y {
            for ix in stamp_min.x..stamp_max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                let old = grid.get(ix, iy, iz);
                let mut new = soft_min(old, d_brush, soft_k);
                if let Some(wb_y) = brush.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }
                if (new - old).abs() > 1e-7 {
                    on_pre_mutation(ix, iy, iz, old);
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    let vol_after_stamp = solid_volume_in_region(grid, edit_min, edit_max);
    let delta_v = (vol_after_stamp - vol_before).max(0.0);
    let min_v = vs * vs * vs * 0.25;
    if delta_v < min_v {
        let (rmin, rmax) = expand_region(grid, stamp_min, stamp_max, 1);
        redistance_local(grid, rmin, rmax, 2);
        return Some(DirtyRegion {
            min: rmin,
            max: rmax,
        });
    }

    // --- 2) Draw volume from the surrounding rim --------------------
    // Prefer solid surface just outside the tool; bias sideways so we
    // don't excavate straight through the contact along `dir`.
    let mut weights: Vec<(u32, u32, u32, f32)> = Vec::new();
    let mut weight_sum = 0.0f32;

    for iz in edit_min.z..edit_max.z {
        for iy in edit_min.y..edit_max.y {
            for ix in edit_min.x..edit_max.x {
                let p = grid.position(ix, iy, iz);
                let d_brush = (p - c).length() - r;
                if d_brush <= 0.0 || d_brush >= rim {
                    continue;
                }
                let phi = grid.get(ix, iy, iz);
                // Must have material to draw from.
                if phi > surface_band * 0.5 {
                    continue;
                }
                let surface_weight = (1.0 - (phi.abs() / surface_band).min(1.0)).max(0.0);
                if surface_weight <= 1e-4 {
                    continue;
                }
                let ring_t = (d_brush / rim).clamp(0.0, 1.0);
                let ring_weight = (1.0 - ring_t) * (1.0 - ring_t);
                let radial = (p - c).normalize_or_zero();
                let cos_theta = radial.dot(dir);
                let dir_weight = ((1.0 - cos_theta) * 0.5).clamp(0.05, 1.0);
                let w = ring_weight * surface_weight * dir_weight;
                if w > 1e-5 {
                    weights.push((ix, iy, iz, w));
                    weight_sum += w;
                }
            }
        }
    }

    if weight_sum > 1e-6 {
        // Increase φ (remove material): opposite sign from Press recruit.
        let scale = (2.0 / (vs * vs).max(1e-8)) * (delta_v / weight_sum);
        for (ix, iy, iz, w) in weights {
            let old = grid.get(ix, iy, iz);
            let mut new = old + scale * w;
            if let Some(wb_y) = brush.workbench_y {
                let p = grid.position(ix, iy, iz);
                let below = wb_y - p.y;
                if below > new {
                    new = below;
                }
            }
            if (new - old).abs() > 1e-7 {
                on_pre_mutation(ix, iy, iz, old);
                grid.set(ix, iy, iz, new);
            }
        }
    }

    let (rmin, rmax) = expand_region(grid, edit_min, edit_max, 2);
    redistance_local(grid, rmin, rmax, 4);

    Some(DirtyRegion {
        min: rmin,
        max: rmax,
    })
}

/// A few Jacobi-style redistance iterations inside `[min, max)`.
/// Keeps sign of φ; relaxes `|∇φ| → 1` so the next stamp / mesher see
/// a usable band. Not a full fast-sweep — good enough for local edits.
fn redistance_local(grid: &mut Grid, min: UVec3, max: UVec3, iterations: u32) {
    if iterations == 0 || min.x >= max.x {
        return;
    }
    let vs = grid.voxel_size();
    let res = grid.res();
    // Snapshot signs from the pre-relax field.
    let nx = (max.x - min.x) as usize;
    let ny = (max.y - min.y) as usize;
    let nz = (max.z - min.z) as usize;
    let mut phi = vec![0.0f32; nx * ny * nz];
    let mut sign = vec![0.0f32; nx * ny * nz];
    let idx = |x: u32, y: u32, z: u32| {
        ((x - min.x) as usize)
            + ((y - min.y) as usize) * nx
            + ((z - min.z) as usize) * nx * ny
    };
    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                let v = grid.get(ix, iy, iz);
                let i = idx(ix, iy, iz);
                phi[i] = v;
                // Smoothed sign.
                sign[i] = v / (v * v + vs * vs).sqrt();
            }
        }
    }

    let dt = 0.4 * vs;
    for _ in 0..iterations {
        let mut next = phi.clone();
        for iz in min.z..max.z {
            for iy in min.y..max.y {
                for ix in min.x..max.x {
                    // Skip one-voxel border of the region — needs neighbours.
                    if ix == min.x
                        || iy == min.y
                        || iz == min.z
                        || ix + 1 == max.x
                        || iy + 1 == max.y
                        || iz + 1 == max.z
                    {
                        continue;
                    }
                    if ix + 1 >= res.x || iy + 1 >= res.y || iz + 1 >= res.z {
                        continue;
                    }
                    let i = idx(ix, iy, iz);
                    let dx = (phi[idx(ix + 1, iy, iz)] - phi[idx(ix - 1, iy, iz)]) * (0.5 / vs);
                    let dy = (phi[idx(ix, iy + 1, iz)] - phi[idx(ix, iy - 1, iz)]) * (0.5 / vs);
                    let dz = (phi[idx(ix, iy, iz + 1)] - phi[idx(ix, iy, iz - 1)]) * (0.5 / vs);
                    let grad = (dx * dx + dy * dy + dz * dz).sqrt();
                    next[i] = phi[i] - dt * sign[i] * (grad - 1.0);
                }
            }
        }
        phi = next;
    }

    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                if ix == min.x
                    || iy == min.y
                    || iz == min.z
                    || ix + 1 == max.x
                    || iy + 1 == max.y
                    || iz + 1 == max.z
                {
                    continue;
                }
                grid.set(ix, iy, iz, phi[idx(ix, iy, iz)]);
            }
        }
    }
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
            18.0,
        )
    }

    #[test]
    fn press_depresses_contact_point() {
        let mut g = sphere_grid();
        // Surface on +X at x≈50. Press from outside along −X.
        let hit = Vec3::new(50.0, 32.0, 32.0);
        let into = Vec3::NEG_X;
        let radius = 6.0;
        let advance = 1.5;
        let center = hit - into * (radius - advance);
        let before = g.sample(hit);
        let _ = apply_press_displace(
            &mut g,
            &PressDisplace {
                center,
                radius,
                direction: into,
                workbench_y: None,
            },
        );
        let after = g.sample(hit);
        assert!(
            after > before,
            "press should push the surface inward (φ↑): before={before}, after={after}"
        );
    }

    #[test]
    fn press_roughly_conserves_volume() {
        let mut g = sphere_grid();
        let res = g.res();
        let full_min = UVec3::ZERO;
        let full_max = res;
        let v0 = solid_volume_in_region(&g, full_min, full_max);

        let hit = Vec3::new(50.0, 32.0, 32.0);
        let into = Vec3::NEG_X;
        let radius = 6.0;
        let center = hit - into * (radius - 1.5);
        let _ = apply_press_displace(
            &mut g,
            &PressDisplace {
                center,
                radius,
                direction: into,
                workbench_y: None,
            },
        );
        let v1 = solid_volume_in_region(&g, full_min, full_max);
        let rel = ((v1 - v0) / v0).abs();
        assert!(
            rel < 0.04,
            "volume should stay within ~4%: v0={v0}, v1={v1}, rel={rel}"
        );
    }

    #[test]
    fn press_builds_a_side_rim() {
        let mut g = sphere_grid();
        let hit = Vec3::new(50.0, 32.0, 32.0);
        let into = Vec3::NEG_X;
        let radius = 6.0;
        let center = hit - into * (radius - 1.5);
        // Probe sideways on the surface band, outside the tool.
        let probe = Vec3::new(50.0, 38.5, 32.0);
        let before = g.sample(probe);
        let _ = apply_press_displace(
            &mut g,
            &PressDisplace {
                center,
                radius,
                direction: into,
                workbench_y: None,
            },
        );
        let after = g.sample(probe);
        assert!(
            after < before - 0.05,
            "rim should gain material (φ↓): before={before}, after={after}"
        );
    }

    #[test]
    fn plain_csg_remove_does_not_conserve_like_press() {
        // Sanity: hard Press (Add/Remove path) loses volume — contrast
        // with apply_press_displace. Uses brush module directly.
        use crate::brush::{apply_sphere_brush, BrushMode, SphereBrush};
        let mut g = sphere_grid();
        let res = g.res();
        let v0 = solid_volume_in_region(&g, UVec3::ZERO, res);
        let _ = apply_sphere_brush(
            &mut g,
            &SphereBrush {
                center: Vec3::new(50.0, 32.0, 32.0),
                radius: 6.0,
                mode: BrushMode::Press,
                direction: Vec3::NEG_X,
                displace: false,
                workbench_y: None,
            },
        );
        let v1 = solid_volume_in_region(&g, UVec3::ZERO, res);
        assert!(
            v1 < v0 - 200.0,
            "hard CSG remove should clearly lose volume: v0={v0}, v1={v1}"
        );
    }

    #[test]
    fn pull_grows_contact_point() {
        let mut g = sphere_grid();
        let hit = Vec3::new(50.0, 32.0, 32.0);
        let into = Vec3::NEG_X;
        let radius = 6.0;
        let center = hit - into * (radius - 1.5);
        let before = g.sample(hit);
        let _ = apply_pull_displace(
            &mut g,
            &PullDisplace {
                center,
                radius,
                direction: into,
                workbench_y: None,
            },
        );
        let after = g.sample(hit);
        assert!(
            after < before,
            "pull should grow the surface outward (φ↓): before={before}, after={after}"
        );
    }

    #[test]
    fn pull_roughly_conserves_volume() {
        let mut g = sphere_grid();
        let v0 = solid_volume_in_region(&g, UVec3::ZERO, g.res());
        let hit = Vec3::new(50.0, 32.0, 32.0);
        let into = Vec3::NEG_X;
        let radius = 6.0;
        let center = hit - into * (radius - 1.5);
        let _ = apply_pull_displace(
            &mut g,
            &PullDisplace {
                center,
                radius,
                direction: into,
                workbench_y: None,
            },
        );
        let v1 = solid_volume_in_region(&g, UVec3::ZERO, g.res());
        let rel = ((v1 - v0) / v0).abs();
        assert!(
            rel < 0.05,
            "volume should stay within ~5%: v0={v0}, v1={v1}, rel={rel}"
        );
    }

    #[test]
    fn pull_thins_the_side_rim() {
        let mut g = sphere_grid();
        let hit = Vec3::new(50.0, 32.0, 32.0);
        let into = Vec3::NEG_X;
        let radius = 6.0;
        let center = hit - into * (radius - 1.5);
        // Probe sideways on the surface — should lose material to the pull.
        let probe = Vec3::new(49.0, 38.0, 32.0);
        let before = g.sample(probe);
        let _ = apply_pull_displace(
            &mut g,
            &PullDisplace {
                center,
                radius,
                direction: into,
                workbench_y: None,
            },
        );
        let after = g.sample(probe);
        assert!(
            after > before + 0.05,
            "rim should thin (φ↑): before={before}, after={after}"
        );
    }
}
