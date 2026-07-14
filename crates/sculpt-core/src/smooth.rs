//! Smoothing brush.
//!
//! Sculptors reach for a smoother almost as often as they reach for a
//! clay brush; MUD needs one before the interaction feels complete. This
//! is the "polish it out" primitive: run a small local blur on the
//! SDF, weighted by a radial falloff around the brush centre, so
//! high-frequency features (crenelated bulge rims, jaggies from
//! surface-nets meshing, chatter marks) melt away without touching
//! low-frequency shape.
//!
//! Implementation is Laplacian smoothing on the SDF grid: for each
//! voxel in the brush footprint, blend its old value toward the
//! average of its 6-connected neighbours by `t = falloff * strength`.
//! Two passes:
//!   1. Copy the AABB into a scratch buffer (so all neighbour reads
//!      see the pre-stamp state).
//!   2. Write blurred values back into the grid.
//!
//! Smoothing the SDF this way propagates the isosurface toward
//! locally-averaged geometry — bumps flatten, ridges soften.

use glam::{IVec3, UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};

/// A single-step smoothing stamp centred at `center` with footprint
/// radius `radius` (mm) and per-step strength `strength` in [0, 1].
///
/// Held-button smoothing accumulates: each stamp does `strength`-worth
/// of blur, so multiple frames compound the effect. `strength = 0.35`
/// at 60 Hz gives a firm-but-controllable feel.
#[derive(Copy, Clone, Debug)]
pub struct SmoothBrush {
    pub center: Vec3,
    pub radius: f32,
    pub strength: f32,
    pub workbench_y: Option<f32>,
}

/// Apply one smoothing stamp. Convenience wrapper with an empty
/// callback.
pub fn apply_smooth_brush(grid: &mut Grid, brush: &SmoothBrush) -> Option<DirtyRegion> {
    apply_smooth_brush_with_callback(grid, brush, |_, _, _, _| {})
}

/// Callback variant. Fires once per voxel about to change with the
/// pre-mutation SDF value — for feeding the undo journal.
pub fn apply_smooth_brush_with_callback<F>(
    grid: &mut Grid,
    brush: &SmoothBrush,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let vs = grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = grid.res();

    // AABB: brush footprint + 1-voxel halo for the 6-neighbour stencil.
    let bound = brush.radius + vs;
    let min_p = (brush.center - Vec3::splat(bound) - grid.origin()) * inv_vs;
    let max_p = (brush.center + Vec3::splat(bound) - grid.origin()) * inv_vs;

    let aabb_min = UVec3::new(
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

    if aabb_min.x >= aabb_max.x || aabb_min.y >= aabb_max.y || aabb_min.z >= aabb_max.z {
        return None;
    }

    // Copy the AABB (plus a halo, if it fits inside the grid) into a
    // scratch buffer. All neighbour reads during the smoothing pass
    // consult this buffer, not the grid, so we avoid the "voxel A's
    // new value contaminates voxel B's computation" pitfall.
    let scratch_min = UVec3::new(
        aabb_min.x.saturating_sub(1),
        aabb_min.y.saturating_sub(1),
        aabb_min.z.saturating_sub(1),
    );
    let scratch_max = UVec3::new(
        (aabb_max.x + 1).min(res.x),
        (aabb_max.y + 1).min(res.y),
        (aabb_max.z + 1).min(res.z),
    );
    let scratch_size = scratch_max - scratch_min;
    let scratch_len = (scratch_size.x * scratch_size.y * scratch_size.z) as usize;
    let mut scratch = Vec::with_capacity(scratch_len);
    for iz in scratch_min.z..scratch_max.z {
        for iy in scratch_min.y..scratch_max.y {
            for ix in scratch_min.x..scratch_max.x {
                scratch.push(grid.get(ix, iy, iz));
            }
        }
    }
    let scratch_get = |v: IVec3| -> f32 {
        let sx = v.x - scratch_min.x as i32;
        let sy = v.y - scratch_min.y as i32;
        let sz = v.z - scratch_min.z as i32;
        let i = (sx
            + sy * scratch_size.x as i32
            + sz * (scratch_size.x * scratch_size.y) as i32) as usize;
        scratch[i]
    };
    let in_scratch = |v: IVec3| -> bool {
        v.x >= scratch_min.x as i32
            && v.y >= scratch_min.y as i32
            && v.z >= scratch_min.z as i32
            && v.x < scratch_max.x as i32
            && v.y < scratch_max.y as i32
            && v.z < scratch_max.z as i32
    };

    let r = brush.radius;
    let inv_r = 1.0 / r.max(1e-6);
    let strength = brush.strength.clamp(0.0, 1.0);
    let c = brush.center;

    for iz in aabb_min.z..aabb_max.z {
        for iy in aabb_min.y..aabb_max.y {
            for ix in aabb_min.x..aabb_max.x {
                let p = grid.position(ix, iy, iz);
                let dist = (p - c).length();
                if dist >= r {
                    continue;
                }
                // Quadratic falloff: 1 at centre, 0 at brush radius.
                let radial = 1.0 - dist * inv_r;
                let falloff = radial * radial;

                let self_val = scratch_get(IVec3::new(ix as i32, iy as i32, iz as i32));

                // 6-connected neighbour average. Neighbours outside
                // the scratch region (i.e., outside the grid entirely)
                // are skipped rather than treated as some out-of-band
                // value, so smoothing on the border doesn't drift.
                let mut sum = 0.0f32;
                let mut count = 0u32;
                for offset in [
                    IVec3::new(1, 0, 0),
                    IVec3::new(-1, 0, 0),
                    IVec3::new(0, 1, 0),
                    IVec3::new(0, -1, 0),
                    IVec3::new(0, 0, 1),
                    IVec3::new(0, 0, -1),
                ] {
                    let nv = IVec3::new(ix as i32, iy as i32, iz as i32) + offset;
                    if in_scratch(nv) {
                        sum += scratch_get(nv);
                        count += 1;
                    }
                }
                if count == 0 {
                    continue;
                }
                let neighbour_avg = sum / count as f32;

                let t = falloff * strength;
                let mut new = self_val * (1.0 - t) + neighbour_avg * t;

                if let Some(wb_y) = brush.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }

                if (new - self_val).abs() > 1e-6 {
                    on_pre_mutation(ix, iy, iz, self_val);
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    Some(DirtyRegion {
        min: aabb_min,
        max: aabb_max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_reduces_a_bump() {
        // Baseline: a flat-ish workpiece with one voxel bumped INWARD
        // (SDF very negative there, less negative around it).
        // Smoothing should raise that voxel toward its neighbours.
        let mut g = Grid::empty(UVec3::new(16, 16, 16), 1.0, Vec3::ZERO);
        for iz in 0..16 {
            for iy in 0..16 {
                for ix in 0..16 {
                    g.set(ix, iy, iz, -1.0);
                }
            }
        }
        g.set(8, 8, 8, -10.0);

        let brush = SmoothBrush {
            center: Vec3::new(8.0, 8.0, 8.0),
            radius: 4.0,
            strength: 1.0,
            workbench_y: None,
        };
        let before = g.get(8, 8, 8);
        let _ = apply_smooth_brush(&mut g, &brush);
        let after = g.get(8, 8, 8);
        assert!(
            after > before,
            "smoothed voxel should move toward the (less negative) neighbour average: before={before}, after={after}",
        );
    }

    #[test]
    fn smooth_leaves_uniform_field_unchanged() {
        // Constant field: smoothing does nothing.
        let mut g = Grid::empty(UVec3::new(16, 16, 16), 1.0, Vec3::ZERO);
        for iz in 0..16 {
            for iy in 0..16 {
                for ix in 0..16 {
                    g.set(ix, iy, iz, -1.0);
                }
            }
        }
        let brush = SmoothBrush {
            center: Vec3::new(8.0, 8.0, 8.0),
            radius: 4.0,
            strength: 1.0,
            workbench_y: None,
        };
        let _ = apply_smooth_brush(&mut g, &brush);
        // Every voxel should still be -1.0 (or extremely close).
        for iz in 0..16 {
            for iy in 0..16 {
                for ix in 0..16 {
                    let v = g.get(ix, iy, iz);
                    assert!(
                        (v + 1.0).abs() < 1e-3,
                        "constant field disturbed at ({ix},{iy},{iz}): {v}",
                    );
                }
            }
        }
    }

    #[test]
    fn smooth_falloff_is_radial() {
        // Two identical bumps at different distances from the brush
        // centre: the closer one should smooth more.
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        for iz in 0..32 {
            for iy in 0..32 {
                for ix in 0..32 {
                    g.set(ix, iy, iz, -1.0);
                }
            }
        }
        g.set(16, 16, 16, -10.0); // near centre
        g.set(20, 16, 16, -10.0); // near edge of a 5-mm brush

        let brush = SmoothBrush {
            center: Vec3::new(16.0, 16.0, 16.0),
            radius: 5.0,
            strength: 1.0,
            workbench_y: None,
        };
        let _ = apply_smooth_brush(&mut g, &brush);
        let near = g.get(16, 16, 16);
        let far = g.get(20, 16, 16);
        // Both should move toward the neighbour average (~ -1.0);
        // the near one should get closer to -1 than the far one.
        assert!(
            (near + 1.0).abs() < (far + 1.0).abs(),
            "closer voxel should smooth more: near={near}, far={far}",
        );
    }
}
