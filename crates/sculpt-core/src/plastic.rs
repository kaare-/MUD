//! Gravity settle — drop floaters, elastic bow, then soft squash.
//!
//! See `PLASTIC_GRAVITY.md`. Not FEM: phases on the active layer,
//! one undo stroke for the whole burst.
//!
//! 1. **Drop** — rigid rest of every floating component onto `iy = 0`
//!    (same backbone as `Ctrl+G`). Airborne lumps always crash down.
//! 2. **Bow** — tall thin stalks shear sideways with a quadratic
//!    height profile (elastic lean) while keeping their elevation.
//! 3. **Squash** — sandpile any column taller than a softness-
//!    dependent cap so soft clay can still collapse / pancake.

use std::collections::{HashMap, HashSet};

use glam::UVec3;

use crate::components::{label_components, ComponentField, ComponentId, EMPTY};
use crate::gravity::rest_components_on_bench;
use crate::grid::{DirtyRegion, Grid};

/// Padding around component AABBs so bow tip + sandpile stay in-region.
const PAD: u32 = 20;

/// Narrow-band width (voxels) rewritten around the new solid mask.
const BAND: u32 = 3;

/// Default squash redistributions (sandpile passes).
pub const DEFAULT_ITERATIONS: u32 = 64;

/// Parameters for one gravity-settle burst.
#[derive(Copy, Clone, Debug)]
pub struct PlasticSettleParams {
    /// `0` = drop floaters only, `1` = soft clay bows hard and pancakes.
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

type Voxel = (u32, u32, u32);

/// Run a gravity-settle burst on `grid`.
///
/// `labels` must match the pre-op grid (used for the initial drop).
/// Later phases re-label after the drop so component bounds stay valid.
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
    let mut seen: HashSet<u64> = HashSet::new();

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

    // Softness 0: gravity drop only (no bow, no squash).
    if plasticity <= 1e-4 {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

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

    // Solid set for bow + squash (keeps cantilevers; not column-packed).
    let mut solids = collect_solids(grid, rmin, rmax);
    if solids.is_empty() {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

    // --- Phase 2: elastic bow (tall thin stalks lean) -----------------
    apply_elastic_bow(&mut solids, &labels_after, plasticity, rmin, rmax, res);

    // Close tiny gaps opened by the discrete shear (fall onto support
    // below in the same column — does not crush cantilevers to the floor).
    column_gap_fall(&mut solids);

    // --- Phase 3: soft squash / pancake -------------------------------
    // Soft clay holds almost nothing; stiff clay holds tall stacks.
    let max_stable = (2.0 + (1.0 - plasticity).powi(2) * 48.0)
        .round()
        .max(2.0) as u32;
    let iterations_run = if params.iterations == 0 {
        0
    } else {
        sandpile_squash(&mut solids, max_stable, params.iterations, rmin, rmax)
    };

    // --- Write solid mask + narrow band -------------------------------
    let (write_min, write_max) = solid_write_bounds(&solids, rmin, rmax, res);
    let sx = (write_max.x - write_min.x) as usize;
    let sy = (write_max.y - write_min.y) as usize;
    let sz = (write_max.z - write_min.z) as usize;
    if sx == 0 || sy == 0 || sz == 0 {
        return PlasticSettleSummary {
            iterations_run,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

    let mut solid_mask = vec![false; sx * sy * sz];
    for &(x, y, z) in &solids {
        if x < write_min.x
            || y < write_min.y
            || z < write_min.z
            || x >= write_max.x
            || y >= write_max.y
            || z >= write_max.z
        {
            continue;
        }
        let li = mask_i(x, y, z, write_min, sx, sy);
        solid_mask[li] = true;
    }

    let vs = grid.voxel_size();
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
    // Clear / rewrite the union of old bounds and new write box so
    // vacated bow source cells don't leave ghost solids behind.
    let clear_min = UVec3::new(rmin.x.min(write_min.x), 0, rmin.z.min(write_min.z));
    let clear_max = UVec3::new(
        rmax.x.max(write_max.x),
        rmax.y.max(write_max.y),
        rmax.z.max(write_max.z),
    );
    for iz in clear_min.z..clear_max.z {
        for iy in clear_min.y..clear_max.y {
            for ix in clear_min.x..clear_max.x {
                let inside_write = ix >= write_min.x
                    && iy >= write_min.y
                    && iz >= write_min.z
                    && ix < write_max.x
                    && iy < write_max.y
                    && iz < write_max.z;
                let new = if inside_write {
                    let li = mask_i(ix, iy, iz, write_min, sx, sy);
                    if solid_mask[li] {
                        -vs
                    } else {
                        let d = min_solid_dist(
                            ix, iy, iz, &solid_mask, write_min, write_max, sx, sy,
                        );
                        (d.min(band_f + 1.0)) * vs
                    }
                } else {
                    // Outside the new mask: scrub leftover solid / band.
                    vs * (band_f + 1.0)
                };
                set_tracked(grid, ix, iy, iz, new);
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

fn collect_solids(grid: &Grid, rmin: UVec3, rmax: UVec3) -> HashSet<Voxel> {
    let mut solids = HashSet::new();
    for iz in rmin.z..rmax.z {
        for iy in rmin.y..rmax.y {
            for ix in rmin.x..rmax.x {
                if grid.get(ix, iy, iz) < 0.0 {
                    solids.insert((ix, iy, iz));
                }
            }
        }
    }
    solids
}

/// Quadratic lateral shear for slender components. Tip deflects more
/// than the base (`shift ∝ y²`), which reads as an elastic bow.
fn apply_elastic_bow(
    solids: &mut HashSet<Voxel>,
    labels: &ComponentField,
    plasticity: f32,
    rmin: UVec3,
    rmax: UVec3,
    res: UVec3,
) {
    // Group solid voxels by component id (from post-drop labels).
    let mut by_id: HashMap<ComponentId, Vec<Voxel>> = HashMap::new();
    for &(x, y, z) in solids.iter() {
        let id = labels.id_at(x, y, z);
        if id == EMPTY {
            // Bow may run after gap-fall in future; tolerate orphans.
            continue;
        }
        by_id.entry(id).or_default().push((x, y, z));
    }

    let mut next = HashSet::with_capacity(solids.len());
    // Keep unlabelled solids (shouldn't happen post-drop) unmoved.
    for &v in solids.iter() {
        if labels.id_at(v.0, v.1, v.2) == EMPTY {
            next.insert(v);
        }
    }

    for (_id, voxels) in by_id {
        if voxels.is_empty() {
            continue;
        }
        let mut min_x = u32::MAX;
        let mut max_x = 0u32;
        let mut min_y = u32::MAX;
        let mut max_y = 0u32;
        let mut min_z = u32::MAX;
        let mut max_z = 0u32;
        let mut bot_cx = 0.0f32;
        let mut bot_cz = 0.0f32;
        let mut bot_n = 0.0f32;
        let mut top_cx = 0.0f32;
        let mut top_cz = 0.0f32;
        let mut top_n = 0.0f32;
        for &(x, y, z) in &voxels {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            min_z = min_z.min(z);
            max_z = max_z.max(z);
        }
        let height = max_y.saturating_sub(min_y).saturating_add(1);
        let footprint = (max_x - min_x + 1).min(max_z - min_z + 1).max(1);
        let aspect = height as f32 / footprint as f32;
        // Fat / squat forms don't bow — only stalky aspect ratios.
        if aspect < 1.35 || height < 8 {
            for v in voxels {
                next.insert(v);
            }
            continue;
        }

        let y_bot = min_y + (height / 4).max(1);
        let y_top = max_y.saturating_sub((height / 4).max(1));
        for &(x, y, z) in &voxels {
            if y <= y_bot {
                bot_cx += x as f32;
                bot_cz += z as f32;
                bot_n += 1.0;
            }
            if y >= y_top {
                top_cx += x as f32;
                top_cz += z as f32;
                top_n += 1.0;
            }
        }
        let (dir_x, dir_z) = lean_direction(bot_cx, bot_cz, bot_n, top_cx, top_cz, top_n);

        // Tip deflection in voxels. Soft + skinny → bigger bow.
        // Cap so consecutive rows stay ~connected (Δshift ≲ 1).
        let amp = (plasticity * (aspect - 1.0) * 2.8)
            .clamp(0.0, (height as f32) * 0.42)
            .max(0.0);
        if amp < 0.75 {
            for v in voxels {
                next.insert(v);
            }
            continue;
        }

        let h = height.max(1) as f32;
        for &(x, y, z) in &voxels {
            let t = (y.saturating_sub(min_y) as f32) / h;
            let shift = (amp * t * t).round() as i32;
            let nx = (x as i32 + dir_x * shift).clamp(rmin.x as i32, rmax.x as i32 - 1);
            let nz = (z as i32 + dir_z * shift).clamp(rmin.z as i32, rmax.z as i32 - 1);
            let nx = (nx as u32).min(res.x.saturating_sub(1));
            let nz = (nz as u32).min(res.z.saturating_sub(1));
            next.insert((nx, y, nz));
        }
    }

    *solids = next;
}

fn lean_direction(
    bot_cx: f32,
    bot_cz: f32,
    bot_n: f32,
    top_cx: f32,
    top_cz: f32,
    top_n: f32,
) -> (i32, i32) {
    if bot_n > 0.0 && top_n > 0.0 {
        let dx = top_cx / top_n - bot_cx / bot_n;
        let dz = top_cz / top_n - bot_cz / bot_n;
        if dx * dx + dz * dz > 0.25 {
            // Amplify any existing lean.
            if dx.abs() >= dz.abs() {
                return (if dx >= 0.0 { 1 } else { -1 }, 0);
            }
            return (0, if dz >= 0.0 { 1 } else { -1 });
        }
    }
    // Symmetric stalk: deterministic +X bow.
    (1, 0)
}

/// Drop solids down within their column until supported (closes shear
/// gaps). Does **not** pull cantilevers down to the workbench.
fn column_gap_fall(solids: &mut HashSet<Voxel>) {
    // Group by (x,z), sort y ascending, rewrite packed from the lowest
    // existing solid in that column (preserve floating column bases —
    // after drop everything should already touch y=0 for grounded
    // pieces; cantilevers after bow have solids at various y).
    let mut cols: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for &(x, y, z) in solids.iter() {
        cols.entry((x, z)).or_default().push(y);
    }
    let mut next = HashSet::with_capacity(solids.len());
    for ((x, z), mut ys) in cols {
        ys.sort_unstable();
        ys.dedup();
        // Pack contiguous from the column's own floor (min y), filling
        // internal gaps only — a bowed tip column that starts at y=10
        // stays starting at y=10 rather than crashing to the bench.
        if ys.is_empty() {
            continue;
        }
        let floor = ys[0];
        for (i, _) in ys.iter().enumerate() {
            next.insert((x, floor + i as u32, z));
        }
    }
    *solids = next;
}

fn sandpile_squash(
    solids: &mut HashSet<Voxel>,
    max_stable: u32,
    iterations: u32,
    rmin: UVec3,
    rmax: UVec3,
) -> u32 {
    let sx = (rmax.x - rmin.x) as usize;
    let sz = (rmax.z - rmin.z) as usize;
    if sx == 0 || sz == 0 {
        return 0;
    }

    // Rebuild column contents as sorted Y lists.
    let mut cols: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for &(x, y, z) in solids.iter() {
        if x < rmin.x || z < rmin.z || x >= rmax.x || z >= rmax.z {
            continue;
        }
        cols.entry((x, z)).or_default().push(y);
    }
    for ys in cols.values_mut() {
        ys.sort_unstable();
        ys.dedup();
    }

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

    let mut iterations_run = 0u32;
    for _ in 0..iterations {
        iterations_run += 1;
        let mut moved = false;
        // Yield on solid *count* or peak height above the bench —
        // after a bow, tip cantilevers are often 1-voxel columns at
        // high y (count OK, peak not).
        let metrics: HashMap<(u32, u32), (u32, u32)> = cols
            .iter()
            .map(|(&k, ys)| {
                let count = ys.len() as u32;
                let peak_h = ys.last().copied().unwrap_or(0).saturating_add(1);
                (k, (count, peak_h))
            })
            .collect();
        let keys: Vec<(u32, u32)> = metrics.keys().copied().collect();
        for (x, z) in keys {
            let (count, peak_h) = *metrics.get(&(x, z)).unwrap_or(&(0, 0));
            if count <= max_stable && peak_h <= max_stable {
                continue;
            }
            let mut best: Option<(u32, u32, u32)> = None; // count, nx, nz
            for (dx, dz) in dirs {
                let tx = x as i32 + dx;
                let tz = z as i32 + dz;
                if tx < rmin.x as i32
                    || tz < rmin.z as i32
                    || tx >= rmax.x as i32
                    || tz >= rmax.z as i32
                {
                    continue;
                }
                let nx = tx as u32;
                let nz = tz as u32;
                let c = metrics.get(&(nx, nz)).map(|(c, _)| *c).unwrap_or(0);
                let better = best.map(|(bc, _, _)| c < bc).unwrap_or(true);
                if better {
                    best = Some((c, nx, nz));
                }
            }
            let Some((_, nx, nz)) = best else {
                continue;
            };
            let Some(ys) = cols.get_mut(&(x, z)) else {
                continue;
            };
            if ys.pop().is_none() {
                continue;
            }
            if ys.is_empty() {
                cols.remove(&(x, z));
            } else if let Some(&floor) = ys.first() {
                let n = ys.len();
                *ys = (0..n).map(|i| floor + i as u32).collect();
            }
            // Plant on neighbour top, or on the bench if empty.
            let plant_y = cols
                .get(&(nx, nz))
                .and_then(|nys| nys.last().copied())
                .map(|t| t + 1)
                .unwrap_or(0);
            {
                let nys = cols.entry((nx, nz)).or_default();
                nys.push(plant_y);
                nys.sort_unstable();
                nys.dedup();
                if let Some(&floor) = nys.first() {
                    let n = nys.len();
                    *nys = (0..n).map(|i| floor + i as u32).collect();
                }
            }
            let _ = sx;
            let _ = sz;
            moved = true;
        }
        if !moved {
            break;
        }
        if cols.values().all(|ys| {
            let count = ys.len() as u32;
            let peak_h = ys.last().copied().unwrap_or(0).saturating_add(1);
            count <= max_stable && peak_h <= max_stable
        }) {
            break;
        }
    }

    let mut next = HashSet::new();
    for ((x, z), ys) in cols {
        for y in ys {
            next.insert((x, y, z));
        }
    }
    *solids = next;
    iterations_run
}

fn solid_write_bounds(
    solids: &HashSet<Voxel>,
    rmin: UVec3,
    rmax: UVec3,
    res: UVec3,
) -> (UVec3, UVec3) {
    let mut mn = UVec3::new(res.x, res.y, res.z);
    let mut mx = UVec3::ZERO;
    for &(x, y, z) in solids {
        mn = mn.min(UVec3::new(x, y, z));
        mx = mx.max(UVec3::new(x + 1, y + 1, z + 1));
    }
    if mn.x >= mx.x {
        return (rmin, rmax);
    }
    let write_min = UVec3::new(
        mn.x.saturating_sub(BAND).min(rmin.x),
        0,
        mn.z.saturating_sub(BAND).min(rmin.z),
    );
    let write_max = UVec3::new(
        (mx.x + BAND).max(rmax.x).min(res.x),
        (mx.y + BAND + 1).max(rmax.y).min(res.y),
        (mx.z + BAND).max(rmax.z).min(res.z),
    );
    (write_min, write_max)
}

#[inline]
fn mask_i(ix: u32, iy: u32, iz: u32, write_min: UVec3, sx: usize, sy: usize) -> usize {
    let x = (ix - write_min.x) as usize;
    let y = (iy - write_min.y) as usize;
    let z = (iz - write_min.z) as usize;
    x + y * sx + z * sx * sy
}

#[allow(clippy::too_many_arguments)]
fn min_solid_dist(
    ix: u32,
    iy: u32,
    iz: u32,
    solid: &[bool],
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
                let li = mask_i(tx as u32, ty as u32, tz as u32, write_min, sx, sy);
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

    /// Mean X of solids in the top quartile of height.
    fn top_com_x(g: &Grid) -> f32 {
        let peak = solid_peak_iy(g);
        let low = solid_lowest_iy(g);
        let cut = low + ((peak - low) * 3 / 4);
        let mut sx = 0.0f32;
        let mut n = 0.0f32;
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if iy >= cut && g.get(ix, iy, iz) < 0.0 {
                            sx += ix as f32;
                            n += 1.0;
                        }
                    }
                }
            }
        }
        if n <= 0.0 {
            0.0
        } else {
            sx / n
        }
    }

    fn base_com_x(g: &Grid) -> f32 {
        let peak = solid_peak_iy(g);
        let low = solid_lowest_iy(g);
        let cut = low + ((peak - low) / 4).max(1);
        let mut sx = 0.0f32;
        let mut n = 0.0f32;
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if iy <= cut && g.get(ix, iy, iz) < 0.0 {
                            sx += ix as f32;
                            n += 1.0;
                        }
                    }
                }
            }
        }
        if n <= 0.0 {
            0.0
        } else {
            sx / n
        }
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
    fn medium_softness_bows_a_tall_stalk() {
        let mut g = tall_tower();
        let base_before = base_com_x(&g);
        let peak_before = solid_peak_iy(&g);
        let labels = label_components(&g);
        // Soft enough to bow, stiff enough that sandpile won't fully
        // pancake the stalk away (max_stable ≈ 22 at p=0.35).
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 0.35,
                iterations: 64,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0);
        let lean = top_com_x(&g) - base_com_x(&g);
        assert!(
            lean.abs() >= 2.0,
            "stalk tip should bow sideways: lean={lean} (base was {base_before})"
        );
        let peak_after = solid_peak_iy(&g);
        assert!(
            peak_after + 8 >= peak_before,
            "bow should keep most of the height: before={peak_before} after={peak_after}"
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
