//! Plastic gravity spike — iterative column squash under self-weight.
//!
//! See `PLASTIC_GRAVITY.md`. Not FEM: each iteration finds `(ix, iz)`
//! columns taller than a plasticity-dependent stable height, keeps the
//! bottom `max_stable` solid voxels, and relocates the excess into the
//! shortest neighbouring columns at the base (flare). Active layer
//! only; one undo stroke for the whole burst.

use glam::UVec3;

use crate::components::ComponentField;
use crate::grid::{DirtyRegion, Grid};

/// Padding around component AABBs so base flare stays in-region.
const PAD: u32 = 8;

/// Default burst length.
pub const DEFAULT_ITERATIONS: u32 = 8;

/// Parameters for one plastic-settle burst.
#[derive(Copy, Clone, Debug)]
pub struct PlasticSettleParams {
    /// `0` = elastic / no-op, `1` = soft yielding clay.
    pub plasticity: f32,
    /// Number of squash iterations in this burst.
    pub iterations: u32,
}

impl Default for PlasticSettleParams {
    fn default() -> Self {
        Self {
            plasticity: 0.7,
            iterations: DEFAULT_ITERATIONS,
        }
    }
}

/// Summary of a settle burst (for logs / tests).
#[derive(Copy, Clone, Debug, Default)]
pub struct PlasticSettleSummary {
    pub iterations_run: u32,
    pub voxels_touched: u32,
    pub dirty: Option<DirtyRegion>,
}

/// Run a plastic-settle burst on `grid`.
pub fn settle_components_plastic<F>(
    grid: &mut Grid,
    labels: &ComponentField,
    params: &PlasticSettleParams,
    mut on_pre_mutation: F,
) -> PlasticSettleSummary
where
    F: FnMut(u32, u32, u32, f32),
{
    let plasticity = params.plasticity.clamp(0.0, 1.0);
    if plasticity <= 1e-4 || params.iterations == 0 || labels.component_count() == 0 {
        return PlasticSettleSummary::default();
    }

    let res = grid.res();
    let vs = grid.voxel_size();
    let Some((rmin, rmax)) = union_component_bounds(labels, res) else {
        return PlasticSettleSummary::default();
    };

    // Stable height scales with the piece's footprint: a fat blob can
    // hold taller columns than a skinny tower. Plasticity shortens the
    // allowed stack (soft clay → pancake).
    let (bb_min, bb_max) = {
        let mut mn = UVec3::new(res.x, res.y, res.z);
        let mut mx = UVec3::ZERO;
        for id in 1..=labels.component_count() as u32 {
            if let Some((a, b)) = labels.bounds_of(id) {
                mn = mn.min(a);
                mx = mx.max(b);
            }
        }
        (mn, mx)
    };
    let footprint = (bb_max.x - bb_min.x).min(bb_max.z - bb_min.z).max(1) as f32;
    // Two caps, take the tighter one:
    // - footprint-relative: soft clay holds ~0.55× its smallest plan
    //   extent; stiff clay holds ~2.2× (skinny towers always yield).
    // - absolute: even medium / squat forms move when soft — without
    //   this, a typical sphere (height ≈ footprint) never exceeds the
    //   footprint rule alone and Settle appears broken.
    let support_fp = footprint * (2.2 - 1.65 * plasticity);
    let support_abs = 5.0 + (1.0 - plasticity) * 34.0;
    let max_stable = support_fp.min(support_abs).round().max(3.0) as usize;
    // Cap how much excess we move per column per iteration so a burst
    // looks gradual rather than collapsing in one frame.
    let move_cap = 1 + (plasticity * 4.0).round() as usize;

    let mut dirty_min = UVec3::new(u32::MAX, u32::MAX, u32::MAX);
    let mut dirty_max = UVec3::ZERO;
    let mut any_dirty = false;
    let mut voxels_touched = 0u32;
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut iterations_run = 0u32;

    let mut set_tracked = |grid: &mut Grid, x: u32, y: u32, z: u32, new: f32| {
        let pre = grid.get(x, y, z);
        if (pre - new).abs() < 1e-7 {
            return;
        }
        let key = (x as u64) | ((y as u64) << 20) | ((z as u64) << 40);
        if seen.insert(key) {
            on_pre_mutation(x, y, z, pre);
            voxels_touched += 1;
        }
        grid.set(x, y, z, new);
        dirty_min = dirty_min.min(UVec3::new(x, y, z));
        dirty_max = dirty_max.max(UVec3::new(x + 1, y + 1, z + 1));
        any_dirty = true;
    };

    for _iter in 0..params.iterations {
        iterations_run += 1;
        let snap = grid.snapshot_region(rmin, rmax);
        let (smin, smax) = snap.bounds();

        // Column solid counts + highest solid iy for plant targeting.
        // Plant y must use each neighbour's own top — using the
        // yielding column's base + neighbour count assumes matching
        // floors and plants *inside* sphere / organic columns.
        let sx = (smax.x - smin.x) as usize;
        let sz = (smax.z - smin.z) as usize;
        let mut col_count = vec![0u32; sx * sz];
        let mut col_hi = vec![None::<u32>; sx * sz];
        for iz in smin.z..smax.z {
            for ix in smin.x..smax.x {
                let mut n = 0u32;
                let mut hi = None;
                for iy in smin.y..smax.y {
                    if snap.get(ix, iy, iz) < 0.0 {
                        n += 1;
                        hi = Some(iy);
                    }
                }
                let i = col_i(ix, iz, smin, sx);
                col_count[i] = n;
                col_hi[i] = hi;
            }
        }

        let mut moved = 0u32;

        for iz in smin.z..smax.z {
            for ix in smin.x..smax.x {
                let mut solids: Vec<u32> = Vec::new();
                for iy in smin.y..smax.y {
                    if snap.get(ix, iy, iz) < 0.0 {
                        solids.push(iy);
                    }
                }
                if solids.len() <= max_stable {
                    continue;
                }
                let excess = (solids.len() - max_stable).min(move_cap);
                // Peel only when a plant seat exists — keeps solid
                // voxel count stable (1:1 relocate).
                for k in 0..excess {
                    let top_iy = solids[solids.len() - 1 - k];
                    // Plant strictly below the peeled voxel so soft
                    // settle slumps instead of stacking a taller peak.
                    let Some(site) = find_plant_site(
                        ix,
                        iz,
                        solids[0],
                        top_iy,
                        &col_count,
                        &col_hi,
                        smin,
                        smax,
                        sx,
                        res,
                    ) else {
                        break;
                    };
                    // Refuse seats that are already solid in the live grid.
                    if grid.get(site.0, site.1, site.2) < 0.0 {
                        break;
                    }
                    set_tracked(grid, ix, top_iy, iz, vs);
                    set_tracked(grid, site.0, site.1, site.2, -vs);
                    paint_band_support(grid, site.0, site.1, site.2, vs, &mut set_tracked);
                    let ci = col_i(site.0, site.2, smin, sx);
                    col_count[ci] = col_count[ci].saturating_add(1);
                    col_hi[ci] = Some(match col_hi[ci] {
                        Some(h) => h.max(site.1),
                        None => site.1,
                    });
                    moved += 1;
                }
            }
        }

        if moved == 0 {
            break;
        }

        mollify_positive_band(grid, rmin, rmax, 0.2, &mut set_tracked);
    }

    let dirty = if any_dirty {
        Some(DirtyRegion {
            min: dirty_min,
            max: dirty_max,
        })
    } else {
        None
    };

    PlasticSettleSummary {
        iterations_run,
        voxels_touched,
        dirty,
    }
}

#[inline]
fn col_i(ix: u32, iz: u32, smin: UVec3, sx: usize) -> usize {
    (ix - smin.x) as usize + (iz - smin.z) as usize * sx
}

/// Empty cell in the shortest neighbouring column.
///
/// - Occupied neighbour → plant just above that column's own top.
/// - Empty neighbour → plant at the yielding column's foot (`base_iy`)
///   so material flares outward at the base instead of mid-air.
/// - Always require `plant_y < peel_iy` so settle cannot raise the peak.
#[allow(clippy::too_many_arguments)]
fn find_plant_site(
    ix: u32,
    iz: u32,
    base_iy: u32,
    peel_iy: u32,
    col_count: &[u32],
    col_hi: &[Option<u32>],
    smin: UVec3,
    smax: UVec3,
    sx: usize,
    res: UVec3,
) -> Option<(u32, u32, u32)> {
    let dirs = [
        (1i32, 0i32),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (-1, 1),
        (1, -1),
        (-1, -1),
    ];
    // Score: prefer empty / short columns, then lower plant seats.
    let mut best: Option<(u32, u32, u32, u32, u32)> = None; // count, py, x, y, z
    for (dx, dz) in dirs {
        let tx = ix as i32 + dx;
        let tz = iz as i32 + dz;
        if tx < smin.x as i32 || tz < smin.z as i32 || tx >= smax.x as i32 || tz >= smax.z as i32
        {
            continue;
        }
        let ux = tx as u32;
        let uz = tz as u32;
        let i = col_i(ux, uz, smin, sx);
        let count = col_count[i];
        let py = match col_hi[i] {
            Some(h) => h.saturating_add(1),
            // Empty neighbour: plant near the region floor so soft
            // clay puddles outward at the bench, not mid-height.
            None => {
                let _ = base_iy;
                smin.y
            }
        };
        if py >= smax.y || py >= res.y || py >= peel_iy {
            continue;
        }
        let better = match best {
            None => true,
            Some((c, y, _, _, _)) => count < c || (count == c && py < y),
        };
        if better {
            best = Some((count, py, ux, py, uz));
        }
    }
    best.map(|(_, _, x, y, z)| (x, y, z))
}

fn paint_band_support<F>(grid: &mut Grid, x: u32, y: u32, z: u32, vs: f32, set_tracked: &mut F)
where
    F: FnMut(&mut Grid, u32, u32, u32, f32),
{
    let res = grid.res();
    for dz in -1i32..=1 {
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                let tx = x as i32 + dx;
                let ty = y as i32 + dy;
                let tz = z as i32 + dz;
                if tx < 0 || ty < 0 || tz < 0 {
                    continue;
                }
                let ux = tx as u32;
                let uy = ty as u32;
                let uz = tz as u32;
                if ux >= res.x || uy >= res.y || uz >= res.z {
                    continue;
                }
                let cur = grid.get(ux, uy, uz);
                if cur < 0.0 {
                    continue;
                }
                let dist = ((dx * dx + dy * dy + dz * dz) as f32).sqrt() * vs;
                if cur > dist {
                    set_tracked(grid, ux, uy, uz, dist);
                }
            }
        }
    }
}

fn union_component_bounds(labels: &ComponentField, res: UVec3) -> Option<(UVec3, UVec3)> {
    let n = labels.component_count();
    if n == 0 {
        return None;
    }
    let mut mn = UVec3::new(res.x, res.y, res.z);
    let mut mx = UVec3::ZERO;
    let mut any = false;
    for id in 1..=n as u32 {
        if let Some((a, b)) = labels.bounds_of(id) {
            mn = mn.min(a);
            mx = mx.max(b);
            any = true;
        }
    }
    if !any {
        return None;
    }
    let rmin = UVec3::new(
        mn.x.saturating_sub(PAD),
        mn.y.saturating_sub(2),
        mn.z.saturating_sub(PAD),
    );
    let rmax = UVec3::new(
        (mx.x + PAD).min(res.x),
        (mx.y + 2).min(res.y),
        (mx.z + PAD).min(res.z),
    );
    if rmin.x >= rmax.x || rmin.y >= rmax.y || rmin.z >= rmax.z {
        return None;
    }
    Some((rmin, rmax))
}

fn mollify_positive_band<F>(
    grid: &mut Grid,
    min: UVec3,
    max: UVec3,
    strength: f32,
    set_tracked: &mut F,
) where
    F: FnMut(&mut Grid, u32, u32, u32, f32),
{
    if strength <= 0.0 {
        return;
    }
    let snap = grid.snapshot_region(min, max);
    let (smin, smax) = snap.bounds();
    let vs = grid.voxel_size();
    let origin = grid.origin();
    for iz in smin.z..smax.z {
        for iy in smin.y..smax.y {
            for ix in smin.x..smax.x {
                if ix == smin.x
                    || iy == smin.y
                    || iz == smin.z
                    || ix + 1 == smax.x
                    || iy + 1 == smax.y
                    || iz + 1 == smax.z
                {
                    continue;
                }
                let c = snap.get(ix, iy, iz);
                if c <= 0.0 || c > vs * 3.0 {
                    continue;
                }
                let avg = (snap.get(ix - 1, iy, iz)
                    + snap.get(ix + 1, iy, iz)
                    + snap.get(ix, iy - 1, iz)
                    + snap.get(ix, iy + 1, iz)
                    + snap.get(ix, iy, iz - 1)
                    + snap.get(ix, iy, iz + 1))
                    * (1.0 / 6.0);
                let mut new = (c + (avg - c) * strength).max(1e-4);
                let world_y = origin.y + (iy as f32 + 0.5) * vs;
                let below = 0.0 - world_y;
                if below > new {
                    new = below;
                }
                set_tracked(grid, ix, iy, iz, new);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::label_components;
    use crate::primitives::{apply_primitive, Primitive, PrimitiveKind};
    use glam::Vec3;

    fn solid_count(g: &Grid) -> u32 {
        let mut n = 0u32;
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if g.get(ix, iy, iz) < 0.0 {
                            n += 1;
                        }
                    }
                }
            }
        }
        n
    }

    fn solid_peak_iy(g: &Grid) -> u32 {
        let mut peak = 0u32;
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if g.get(ix, iy, iz) < 0.0 {
                            peak = peak.max(iy);
                        }
                    }
                }
            }
        }
        peak
    }

    fn solid_base_width_xz(g: &Grid) -> u32 {
        let mut min_x = u32::MAX;
        let mut max_x = 0u32;
        let mut min_z = u32::MAX;
        let mut max_z = 0u32;
        let mut any = false;
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y.min(4) {
                    for ix in base.x..max.x {
                        if g.get(ix, iy, iz) < 0.0 {
                            min_x = min_x.min(ix);
                            max_x = max_x.max(ix);
                            min_z = min_z.min(iz);
                            max_z = max_z.max(iz);
                            any = true;
                        }
                    }
                }
            }
        }
        if !any {
            return 0;
        }
        (max_x - min_x + 1).max(max_z - min_z + 1)
    }

    fn tall_tower() -> Grid {
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        for iy in 0..28 {
            for iz in 30..34 {
                for ix in 30..34 {
                    g.set(ix, iy, iz, -1.0);
                }
            }
        }
        for iy in 0..30 {
            for iz in 28..36 {
                for ix in 28..36 {
                    if g.get(ix, iy, iz) >= 0.0 {
                        let dx = (ix as i32 - 31).unsigned_abs();
                        let dz = (iz as i32 - 31).unsigned_abs();
                        g.set(ix, iy, iz, (dx.max(dz) + 1) as f32);
                    }
                }
            }
        }
        g
    }

    #[test]
    fn plasticity_zero_is_noop() {
        let mut g = tall_tower();
        let before = solid_count(&g);
        let peak = solid_peak_iy(&g);
        let labels = label_components(&g);
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 0.0,
                iterations: 8,
            },
            |_, _, _, _| {},
        );
        assert_eq!(summary.voxels_touched, 0);
        assert_eq!(solid_count(&g), before);
        assert_eq!(solid_peak_iy(&g), peak);
    }

    #[test]
    fn soft_settle_lowers_and_widens_a_tall_tower() {
        let mut g = tall_tower();
        let peak_before = solid_peak_iy(&g);
        let width_before = solid_base_width_xz(&g);
        let vol_before = solid_count(&g);
        let labels = label_components(&g);
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 1.0,
                iterations: 16,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0, "soft settle must touch voxels");
        let peak_after = solid_peak_iy(&g);
        assert!(
            peak_after + 4 < peak_before,
            "tower should lose substantial height: before={peak_before} after={peak_after}"
        );
        let width_after = solid_base_width_xz(&g);
        assert!(
            width_after > width_before,
            "base should flare: before={width_before} after={width_after}"
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.90..=1.10).contains(&ratio),
            "volume should stay roughly conserved: before={vol_before} after={vol_after}"
        );
    }

    #[test]
    fn soft_sphere_settles_and_conserves_volume() {
        // Starter-like squat forms used to no-op: footprint rule alone
        // kept max_stable ≥ height. Soft plasticity must visibly move.
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        let _ = apply_primitive(
            &mut g,
            &Primitive {
                kind: PrimitiveKind::Sphere { radius: 10.0 },
                center: Vec3::new(0.0, 10.0, 0.0),
                workbench_y: Some(0.0),
            },
        );
        let width_before = solid_base_width_xz(&g);
        let vol_before = solid_count(&g);
        let labels = label_components(&g);
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 1.0,
                iterations: 16,
            },
            |_, _, _, _| {},
        );
        let width_after = solid_base_width_xz(&g);
        assert!(
            summary.voxels_touched > 0,
            "soft sphere must relocate voxels (got touched=0)"
        );
        // Sphere tips are short columns (below max_stable) so peak iy
        // can stay put; the body must still puddle outward.
        assert!(
            width_after > width_before,
            "soft sphere should flare at the base: before={width_before} after={width_after}"
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "sphere volume drift: before={vol_before} after={vol_after}"
        );
    }

    #[test]
    fn stiff_fat_blob_barely_moves() {
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        let _ = apply_primitive(
            &mut g,
            &Primitive {
                kind: PrimitiveKind::Sphere { radius: 8.0 },
                center: Vec3::new(0.0, 8.0, 0.0),
                workbench_y: Some(0.0),
            },
        );
        let peak_before = solid_peak_iy(&g);
        let vol_before = solid_count(&g);
        let labels = label_components(&g);
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 0.25,
                iterations: 8,
            },
            |_, _, _, _| {},
        );
        let peak_after = solid_peak_iy(&g);
        let drop = peak_before as i32 - peak_after as i32;
        assert!(
            drop <= 3,
            "stiff fat blob should barely settle: drop={drop} touched={}",
            summary.voxels_touched
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.90..=1.10).contains(&ratio),
            "fat blob volume drift: before={vol_before} after={vol_after}"
        );
    }
}
