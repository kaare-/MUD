//! Gravity settle — drop floaters, then squash soft clay toward the bench.
//!
//! See `PLASTIC_GRAVITY.md`. Not FEM: two phases on the active layer,
//! one undo stroke for the whole burst.
//!
//! 1. **Drop** — rigid rest of every floating component onto `iy = 0`
//!    (same backbone as `Ctrl+G`). Airborne lumps always crash down.
//! 2. **Squash** — when plasticity > 0, pack each `(ix, iz)` column
//!    onto the bench and sandpile any height above a plasticity-
//!    dependent cap into neighbouring columns. Tall stalks collapse;
//!    soft blobs pancake. This is bulk gravity, not a surface filter.

use glam::UVec3;

use crate::components::{label_components, ComponentField};
use crate::gravity::rest_components_on_bench;
use crate::grid::{DirtyRegion, Grid};

/// Padding around component AABBs so sandpile flare stays in-region.
const PAD: u32 = 16;

/// Narrow-band width (voxels) rewritten around the new solid mask.
const BAND: u32 = 3;

/// Default squash redistributions (sandpile passes).
pub const DEFAULT_ITERATIONS: u32 = 64;

/// Parameters for one gravity-settle burst.
#[derive(Copy, Clone, Debug)]
pub struct PlasticSettleParams {
    /// `0` = drop floaters only (no squash), `1` = soft clay pancakes.
    pub plasticity: f32,
    /// Max sandpile redistribution steps in the squash phase.
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

/// Run a gravity-settle burst on `grid`.
///
/// `labels` must match the pre-op grid (used for the initial drop).
/// Soft squash re-labels after the drop so component bounds stay valid.
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
    if labels.component_count() == 0 {
        return PlasticSettleSummary::default();
    }

    let mut dirty_min = UVec3::new(u32::MAX, u32::MAX, u32::MAX);
    let mut dirty_max = UVec3::ZERO;
    let mut any_dirty = false;
    let mut voxels_touched = 0u32;
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();

    let mut track = |x: u32, y: u32, z: u32, pre: f32| {
        let key = (x as u64) | ((y as u64) << 20) | ((z as u64) << 40);
        if seen.insert(key) {
            on_pre_mutation(x, y, z, pre);
            voxels_touched += 1;
        }
    };

    // --- Phase 1: airborne lumps crash to the workbench ---------------
    let rest = rest_components_on_bench(grid, labels, |x, y, z, pre| {
        track(x, y, z, pre);
    });
    if let Some(region) = rest.dirty {
        dirty_min = dirty_min.min(region.min);
        dirty_max = dirty_max.max(region.max);
        any_dirty = true;
    }

    // Elastic / zero softness: gravity drop only.
    if plasticity <= 1e-4 || params.iterations == 0 {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

    // --- Phase 2: soft clay collapses under its own weight ------------
    let labels_after = label_components(grid);
    if labels_after.component_count() == 0 {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

    let res = grid.res();
    let Some((rmin, rmax)) = union_component_bounds(&labels_after, res) else {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    };

    // Soft clay holds almost nothing; stiff clay holds tall stacks.
    // Quadratic ease so mid/high plasticity visibly collapses.
    let max_stable = (2.0 + (1.0 - plasticity).powi(2) * 48.0)
        .round()
        .max(2.0) as u32;

    let sx = (rmax.x - rmin.x) as usize;
    let sz = (rmax.z - rmin.z) as usize;
    if sx == 0 || sz == 0 {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

    // Packed solid counts per column (gaps already fallen out).
    let mut counts = vec![0u32; sx * sz];
    for iz in rmin.z..rmax.z {
        for ix in rmin.x..rmax.x {
            let mut n = 0u32;
            for iy in rmin.y..rmax.y {
                if grid.get(ix, iy, iz) < 0.0 {
                    n += 1;
                }
            }
            counts[col_i(ix, iz, rmin, sx)] = n;
        }
    }

    // Sandpile: peel overload onto the shortest neighbour until every
    // column is ≤ max_stable or we hit the iteration budget.
    let mut iterations_run = 0u32;
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
    for _ in 0..params.iterations {
        iterations_run += 1;
        let mut moved = false;
        // Snapshot counts so order within a pass is stable.
        let prev = counts.clone();
        for iz in rmin.z..rmax.z {
            for ix in rmin.x..rmax.x {
                let i = col_i(ix, iz, rmin, sx);
                if prev[i] <= max_stable {
                    continue;
                }
                // Prefer the shortest neighbour; ties → lower plant.
                let mut best: Option<(u32, usize)> = None;
                for (dx, dz) in dirs {
                    let tx = ix as i32 + dx;
                    let tz = iz as i32 + dz;
                    if tx < rmin.x as i32
                        || tz < rmin.z as i32
                        || tx >= rmax.x as i32
                        || tz >= rmax.z as i32
                    {
                        continue;
                    }
                    let j = col_i(tx as u32, tz as u32, rmin, sx);
                    let c = counts[j];
                    let better = best.map(|(bc, _)| c < bc).unwrap_or(true);
                    if better {
                        best = Some((c, j));
                    }
                }
                if let Some((_, j)) = best {
                    counts[i] -= 1;
                    counts[j] += 1;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
        // Early out once every column is stable.
        if counts.iter().all(|&c| c <= max_stable) {
            break;
        }
    }

    // Materialise packed columns + rebuild a narrow band. Everything
    // sits on the workbench (`iy` from 0 upward).
    let vs = grid.voxel_size();
    let write_ymin = 0u32;
    let write_ymax = rmax.y.max(
        counts
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(BAND + 1)
            .min(res.y),
    );
    let write_min = UVec3::new(rmin.x, write_ymin, rmin.z);
    let write_max = UVec3::new(rmax.x, write_ymax, rmax.z);

    // Dense solid mask for band distances.
    let sy = (write_max.y - write_min.y) as usize;
    let mut solid = vec![false; sx * sy * sz];
    for iz in rmin.z..rmax.z {
        for ix in rmin.x..rmax.x {
            let n = counts[col_i(ix, iz, rmin, sx)].min(write_max.y);
            for iy in 0..n {
                let li = mask_i(ix, iy, iz, rmin, write_min, sx, sy);
                solid[li] = true;
            }
        }
    }

    let mut set_tracked = |grid: &mut Grid, x: u32, y: u32, z: u32, new: f32| {
        let pre = grid.get(x, y, z);
        if (pre - new).abs() < 1e-7 {
            return;
        }
        track(x, y, z, pre);
        grid.set(x, y, z, new);
        dirty_min = dirty_min.min(UVec3::new(x, y, z));
        dirty_max = dirty_max.max(UVec3::new(x + 1, y + 1, z + 1));
        any_dirty = true;
    };

    let band_f = BAND as f32;
    for iz in write_min.z..write_max.z {
        for iy in write_min.y..write_max.y {
            for ix in write_min.x..write_max.x {
                let li = mask_i(ix, iy, iz, rmin, write_min, sx, sy);
                let new = if solid[li] {
                    -vs
                } else {
                    let d = min_solid_dist(ix, iy, iz, &solid, rmin, write_min, write_max, sx, sy);
                    (d.min(band_f + 1.0)) * vs
                };
                set_tracked(grid, ix, iy, iz, new);
            }
        }
    }

    // If the pre-squash piece stuck above the new packed height, clear
    // leftover solids in the old AABB (already covered when write_ymax
    // includes old rmax.y). Ensure old peak cells above write_ymax are
    // scrubbed when the original bounds were taller.
    if rmax.y > write_max.y {
        for iz in rmin.z..rmax.z {
            for iy in write_max.y..rmax.y {
                for ix in rmin.x..rmax.x {
                    let pre = grid.get(ix, iy, iz);
                    if pre < vs * (band_f + 1.0) {
                        set_tracked(grid, ix, iy, iz, vs * (band_f + 1.0));
                    }
                }
            }
        }
    }

    PlasticSettleSummary {
        iterations_run,
        voxels_touched,
        dirty: any_dirty.then_some(DirtyRegion {
            min: dirty_min,
            max: dirty_max,
        }),
    }
}

#[inline]
fn col_i(ix: u32, iz: u32, rmin: UVec3, sx: usize) -> usize {
    (ix - rmin.x) as usize + (iz - rmin.z) as usize * sx
}

#[inline]
fn mask_i(
    ix: u32,
    iy: u32,
    iz: u32,
    rmin: UVec3,
    write_min: UVec3,
    sx: usize,
    sy: usize,
) -> usize {
    let x = (ix - rmin.x) as usize;
    let y = (iy - write_min.y) as usize;
    let z = (iz - rmin.z) as usize;
    x + y * sx + z * sx * sy
}

#[allow(clippy::too_many_arguments)]
fn min_solid_dist(
    ix: u32,
    iy: u32,
    iz: u32,
    solid: &[bool],
    rmin: UVec3,
    write_min: UVec3,
    write_max: UVec3,
    sx: usize,
    sy: usize,
) -> f32 {
    let mut best = (BAND + 1) as f32;
    let b = BAND as i32;
    for dz in -b..=b {
        for dy in -b..=b {
            for dx in -b..=b {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                let tx = ix as i32 + dx;
                let ty = iy as i32 + dy;
                let tz = iz as i32 + dz;
                if tx < write_min.x as i32
                    || ty < write_min.y as i32
                    || tz < write_min.z as i32
                    || tx >= write_max.x as i32
                    || ty >= write_max.y as i32
                    || tz >= write_max.z as i32
                {
                    continue;
                }
                let li = mask_i(tx as u32, ty as u32, tz as u32, rmin, write_min, sx, sy);
                if solid[li] {
                    let d = ((dx * dx + dy * dy + dz * dz) as f32).sqrt();
                    if d < best {
                        best = d;
                    }
                }
            }
        }
    }
    best
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
    // Always include the workbench floor so packed columns land at iy=0.
    let rmin = UVec3::new(mn.x.saturating_sub(PAD), 0, mn.z.saturating_sub(PAD));
    let rmax = UVec3::new(
        (mx.x + PAD).min(res.x),
        (mx.y + BAND + 2).min(res.y),
        (mx.z + PAD).min(res.z),
    );
    if rmin.x >= rmax.x || rmin.y >= rmax.y || rmin.z >= rmax.z {
        return None;
    }
    Some((rmin, rmax))
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

    fn solid_lowest_iy(g: &Grid) -> u32 {
        let mut low = u32::MAX;
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if g.get(ix, iy, iz) < 0.0 {
                            low = low.min(iy);
                        }
                    }
                }
            }
        }
        low
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
    fn plasticity_zero_leaves_grounded_tower_alone() {
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
    fn floating_lump_crashes_to_bench_even_when_stiff() {
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        let _ = apply_primitive(
            &mut g,
            &Primitive {
                kind: PrimitiveKind::Sphere { radius: 5.0 },
                center: Vec3::new(0.0, 28.0, 0.0),
                workbench_y: None,
            },
        );
        assert!(solid_lowest_iy(&g) > 5, "precondition: lump is airborne");
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
        assert!(summary.voxels_touched > 0);
        assert_eq!(
            solid_lowest_iy(&g),
            0,
            "floating lump must land on the workbench"
        );
    }

    #[test]
    fn soft_settle_collapses_tower_down_onto_bench() {
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
                iterations: 256,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0, "soft settle must touch voxels");
        assert_eq!(solid_lowest_iy(&g), 0, "collapsed clay sits on the bench");
        let peak_after = solid_peak_iy(&g);
        assert!(
            peak_after + 10 < peak_before,
            "tower should collapse hard: before={peak_before} after={peak_after}"
        );
        let width_after = solid_base_width_xz(&g);
        assert!(
            width_after > width_before,
            "base should pile outward: before={width_before} after={width_after}"
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "volume should stay roughly conserved: before={vol_before} after={vol_after}"
        );
    }

    #[test]
    fn soft_sphere_pancakes_toward_bench() {
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        let _ = apply_primitive(
            &mut g,
            &Primitive {
                kind: PrimitiveKind::Sphere { radius: 10.0 },
                center: Vec3::new(0.0, 10.0, 0.0),
                workbench_y: Some(0.0),
            },
        );
        let peak_before = solid_peak_iy(&g);
        let width_before = solid_base_width_xz(&g);
        let vol_before = solid_count(&g);
        let labels = label_components(&g);
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 1.0,
                iterations: 256,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0);
        assert_eq!(solid_lowest_iy(&g), 0);
        let peak_after = solid_peak_iy(&g);
        assert!(
            peak_after + 6 < peak_before,
            "soft sphere should lose height: before={peak_before} after={peak_after}"
        );
        let width_after = solid_base_width_xz(&g);
        assert!(
            width_after > width_before,
            "soft sphere should pancake outward: before={width_before} after={width_after}"
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "sphere volume drift: before={vol_before} after={vol_after}"
        );
    }

    #[test]
    fn stiff_fat_blob_barely_squashes() {
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
                plasticity: 0.2,
                iterations: 64,
            },
            |_, _, _, _| {},
        );
        let peak_after = solid_peak_iy(&g);
        let drop = peak_before as i32 - peak_after as i32;
        assert!(
            drop <= 4,
            "stiff fat blob should barely squash: drop={drop} touched={}",
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
