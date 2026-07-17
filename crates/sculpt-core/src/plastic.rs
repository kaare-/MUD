//! Gravity settle — drop, then throttle-up sag, then tip/splat.
//!
//! See `PLASTIC_GRAVITY.md`. Not FEM. One undo stroke on the active
//! layer.
//!
//! 1. **Drop** — rigid rest onto `iy = 0` (same as `Ctrl+G`).
//! 2. **Sag first** — progressive arch/stalk sag, strength easing in
//!    over several passes so even high softness bows before it
//!    collapses (no full-onset tip/splat).
//! 3. **Tip** — only if still needle-tall after sag (high softness).
//! 4. **Splat** — thick mound, only at high softness, blended in.
//!
//! The solid→SDF rewrite runs only when geometry actually changed.

use std::collections::{HashMap, HashSet};

use glam::UVec3;

use crate::components::{label_components, ComponentField, ComponentId, EMPTY};
use crate::gravity::rest_components_on_bench;
use crate::grid::{DirtyRegion, Grid};

/// Padding so tipped / bowed / splatted clay stays in-region.
const PAD: u32 = 24;

/// Narrow-band width rewritten around the solid mask.
const BAND: u32 = 3;

/// Default height-field equalisation passes in the splat phase.
pub const DEFAULT_ITERATIONS: u32 = 48;

/// Parameters for one gravity-settle burst.
#[derive(Copy, Clone, Debug)]
pub struct PlasticSettleParams {
    /// `0` = drop only · low/mid = progressive sag · high = tip + splat.
    pub plasticity: f32,
    /// Splat mound-equalisation iterations.
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

#[derive(Copy, Clone, Debug)]
struct LabeledVoxel {
    x: u32,
    y: u32,
    z: u32,
    id: ComponentId,
}

/// Run a gravity-settle burst on `grid`.
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

    // --- Phase 1: drop ------------------------------------------------
    let rest = rest_components_on_bench(grid, labels, |x, y, z, pre| {
        track(x, y, z, pre);
    });
    if let Some(region) = rest.dirty {
        dirty_min = dirty_min.min(region.min);
        dirty_max = dirty_max.max(region.max);
        any_dirty = true;
    }

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

    let mut voxels = collect_labeled(grid, &labels_after, rmin, rmax);
    if voxels.is_empty() {
        return PlasticSettleSummary {
            iterations_run: 0,
            voxels_touched,
            dirty: any_dirty.then_some(DirtyRegion {
                min: dirty_min,
                max: dirty_max,
            }),
        };
    }

    // --- Phase 2: sag first, throttled up across passes ---------------
    // Ease-in: pass strengths are *increments* that sum to `plasticity`,
    // so we never re-apply a full drop each pass (that crushed volume).
    let sag_passes = (1 + (plasticity * 5.0).round() as u32).clamp(1, 6);
    let mut changed = false;
    let mut iterations_run = 0u32;
    let mut prev_ease = 0.0f32;
    for pass in 0..sag_passes {
        let ease = ((pass + 1) as f32 / sag_passes as f32).powf(1.35);
        let delta = (ease - prev_ease).max(0.0);
        prev_ease = ease;
        let local = (plasticity * delta).clamp(0.0, 1.0);
        if apply_arch_sag(&mut voxels, local, res) {
            changed = true;
            iterations_run += 1;
        }
    }

    // --- Phase 3: tip only if still a needle after sag ----------------
    // Onset eases in above 0.55 — partial lean before a full 90° tip.
    if plasticity >= 0.55 {
        let tip_onset = ((plasticity - 0.55) / 0.45).clamp(0.0, 1.0).powf(1.35);
        if tip_unstable(&mut voxels, plasticity, tip_onset, rmin, rmax, res) {
            drop_solids_to_bench(&mut voxels);
            changed = true;
        }
    }

    // --- Phase 4: splat only at high softness, blended in -------------
    if params.iterations > 0 && plasticity >= 0.5 {
        let steps = volumetric_splat(&mut voxels, plasticity, params.iterations, rmin, rmax);
        if steps > 0 {
            smooth_solid_mask(&mut voxels, rmin, rmax);
            changed = true;
            iterations_run += steps;
        }
    }

    // Only rewrite the SDF when tip/sag/splat actually moved solids.
    // Rewriting an unchanged mask was sandblasting the surface into
    // the "erosion" look even at softness 0.01.
    if changed {
        write_solids_to_grid(
            grid,
            &voxels,
            rmin,
            rmax,
            res,
            &mut track,
            &mut dirty_min,
            &mut dirty_max,
            &mut any_dirty,
        );
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

fn collect_labeled(
    grid: &Grid,
    labels: &ComponentField,
    rmin: UVec3,
    rmax: UVec3,
) -> Vec<LabeledVoxel> {
    let mut out = Vec::new();
    for iz in rmin.z..rmax.z {
        for iy in rmin.y..rmax.y {
            for ix in rmin.x..rmax.x {
                if grid.get(ix, iy, iz) < 0.0 {
                    let id = labels.id_at(ix, iy, iz);
                    if id != EMPTY {
                        out.push(LabeledVoxel {
                            x: ix,
                            y: iy,
                            z: iz,
                            id,
                        });
                    }
                }
            }
        }
    }
    out
}

fn group_by_id(voxels: &[LabeledVoxel]) -> HashMap<ComponentId, Vec<usize>> {
    let mut map: HashMap<ComponentId, Vec<usize>> = HashMap::new();
    for (i, v) in voxels.iter().enumerate() {
        map.entry(v.id).or_default().push(i);
    }
    map
}

/// Tip components whose height dominates the footprint (unstable
/// towers / stalks). Softness gates how eager we are to tip.
/// Returns whether any component actually tipped.
fn tip_unstable(
    voxels: &mut Vec<LabeledVoxel>,
    plasticity: f32,
    tip_onset: f32,
    _rmin: UVec3,
    _rmax: UVec3,
    res: UVec3,
) -> bool {
    // Tip is a last resort after sag — only very soft clay.
    if plasticity < 0.55 || tip_onset <= 1e-4 {
        return false;
    }
    let groups = group_by_id(voxels);
    let mut next = voxels.clone();
    let mut any = false;
    // tip_onset 0 → identity, 1 → full 90° lay-down.
    let angle = tip_onset * std::f32::consts::FRAC_PI_2;
    let (cos_a, sin_a) = (angle.cos(), angle.sin());
    for (id, idxs) in groups {
        if idxs.is_empty() {
            continue;
        }
        let mut min_x = u32::MAX;
        let mut max_x = 0u32;
        let mut min_y = u32::MAX;
        let mut max_y = 0u32;
        let mut min_z = u32::MAX;
        let mut max_z = 0u32;
        let mut cx = 0.0f32;
        let mut cy = 0.0f32;
        let mut cz = 0.0f32;
        for &i in &idxs {
            let v = voxels[i];
            min_x = min_x.min(v.x);
            max_x = max_x.max(v.x);
            min_y = min_y.min(v.y);
            max_y = max_y.max(v.y);
            min_z = min_z.min(v.z);
            max_z = max_z.max(v.z);
            cx += v.x as f32;
            cy += v.y as f32;
            cz += v.z as f32;
        }
        let n = idxs.len() as f32;
        cx /= n;
        cy /= n;
        cz /= n;
        let ex = (max_x - min_x + 1) as f32;
        let ey = (max_y - min_y + 1) as f32;
        let ez = (max_z - min_z + 1) as f32;
        let foot = ex.max(ez);
        // After sag, only tip what's still extreme.
        let tip_aspect = 2.6 - plasticity * 0.5; // p=1 → 2.1, p=0.55 → 2.3
        if ey < tip_aspect * foot || ey < 14.0 {
            continue;
        }

        // Rotate about Z through the centroid; onset throttles the angle.
        // Scatter into free cells when two solids land on the same voxel
        // so tip doesn't erase volume.
        let mut claimed: HashSet<(u32, u32, u32)> = HashSet::new();
        for &i in &idxs {
            let v = voxels[i];
            let rx = v.x as f32 - cx;
            let ry = v.y as f32 - cy;
            let rz = v.z as f32 - cz;
            let nx = (cx + rx * cos_a - ry * sin_a).round() as i32;
            let ny = (cy + rx * sin_a + ry * cos_a).round() as i32;
            let nz = (cz + rz).round() as i32;
            let (x, y, z) = free_seat(nx, ny, nz, res, &mut claimed);
            next[i] = LabeledVoxel { x, y, z, id };
        }
        any = true;
    }
    if any {
        *voxels = dedup_voxels(next);
    }
    any
}

/// Prefer the ideal landing cell; if taken, walk a neighbourhood for an
/// empty seat so discrete moves don't erase volume.
fn free_seat(
    nx: i32,
    ny: i32,
    nz: i32,
    res: UVec3,
    claimed: &mut HashSet<(u32, u32, u32)>,
) -> (u32, u32, u32) {
    let clamp_pt = |x: i32, y: i32, z: i32| -> (u32, u32, u32) {
        (
            x.clamp(0, res.x as i32 - 1) as u32,
            y.clamp(0, res.y as i32 - 1) as u32,
            z.clamp(0, res.z as i32 - 1) as u32,
        )
    };
    let ideal = clamp_pt(nx, ny, nz);
    if claimed.insert(ideal) {
        return ideal;
    }
    for ring in 1i32..=8 {
        for dy in -ring..=ring {
            for dz in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs().max(dy.abs()).max(dz.abs()) != ring {
                        continue;
                    }
                    let cand = clamp_pt(nx + dx, ny + dy, nz + dz);
                    if claimed.insert(cand) {
                        return cand;
                    }
                }
            }
        }
    }
    ideal
}

fn drop_solids_to_bench(voxels: &mut [LabeledVoxel]) {
    let groups = group_by_id(voxels);
    for (_id, idxs) in groups {
        let mut min_y = u32::MAX;
        for &i in &idxs {
            min_y = min_y.min(voxels[i].y);
        }
        if min_y == 0 || min_y == u32::MAX {
            continue;
        }
        for &i in &idxs {
            voxels[i].y -= min_y;
        }
    }
}

/// Sag voxels toward the bench — cantilevers/branches and tall stalks.
///
/// `plasticity` here is already throttled by the caller (ease-in across
/// passes). Bench-touching solids are anchors; tips drop by
/// `tip_sag · t²`. Tall upright stalks also ease down + lean slightly
/// so gravity always bows before tip/splat.
fn apply_arch_sag(voxels: &mut Vec<LabeledVoxel>, plasticity: f32, res: UVec3) -> bool {
    if voxels.is_empty() || plasticity <= 1e-5 {
        return false;
    }

    // Per-component so separate lumps don't borrow each other's support.
    let groups = group_by_id(voxels);
    let mut next = voxels.clone();
    let mut any_sag = false;
    let dirs6 = [
        (1i32, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];

    for (_id, idxs) in groups {
        if idxs.len() < 8 {
            continue;
        }

        let mut key_to_local: HashMap<(u32, u32, u32), usize> = HashMap::new();
        let mut min_x = u32::MAX;
        let mut max_x = 0u32;
        let mut min_y = u32::MAX;
        let mut max_y = 0u32;
        let mut min_z = u32::MAX;
        let mut max_z = 0u32;
        for &i in &idxs {
            let v = voxels[i];
            key_to_local.insert((v.x, v.y, v.z), i);
            min_x = min_x.min(v.x);
            max_x = max_x.max(v.x);
            min_y = min_y.min(v.y);
            max_y = max_y.max(v.y);
            min_z = min_z.min(v.z);
            max_z = max_z.max(v.z);
        }
        let height = max_y.saturating_sub(min_y).saturating_add(1);
        let foot = (max_x - min_x + 1).min(max_z - min_z + 1).max(1);
        let aspect = height as f32 / foot as f32;

        // Bench anchors (xz) + BFS distance through the solid.
        let mut anchors: Vec<(u32, u32)> = Vec::new();
        let mut dist: HashMap<(u32, u32, u32), u32> = HashMap::new();
        let mut queue: std::collections::VecDeque<(u32, u32, u32)> =
            std::collections::VecDeque::new();
        for &i in &idxs {
            let v = voxels[i];
            if v.y == 0 {
                anchors.push((v.x, v.z));
                dist.insert((v.x, v.y, v.z), 0);
                queue.push_back((v.x, v.y, v.z));
            }
        }
        if queue.is_empty() || anchors.is_empty() {
            continue;
        }
        while let Some((x, y, z)) = queue.pop_front() {
            let d0 = dist[&(x, y, z)];
            for (dx, dy, dz) in dirs6 {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                let nz = z as i32 + dz;
                if nx < 0 || ny < 0 || nz < 0 {
                    continue;
                }
                let key = (nx as u32, ny as u32, nz as u32);
                if !key_to_local.contains_key(&key) || dist.contains_key(&key) {
                    continue;
                }
                dist.insert(key, d0 + 1);
                queue.push_back(key);
            }
        }

        let horiz_of = |x: u32, z: u32| -> u32 {
            anchors
                .iter()
                .map(|&(ax, az)| {
                    let dx = (x as i32 - ax as i32).unsigned_abs();
                    let dz = (z as i32 - az as i32).unsigned_abs();
                    dx.max(dz)
                })
                .min()
                .unwrap_or(0)
        };

        let mut max_cantilever = 0u32;
        for &i in &idxs {
            let v = voxels[i];
            let Some(&d) = dist.get(&(v.x, v.y, v.z)) else {
                continue;
            };
            let h = horiz_of(v.x, v.z);
            if h >= 8 && d > v.y.saturating_add(3) {
                max_cantilever = max_cantilever.max(d);
            }
        }

        // Prefer cantilever sag when the form has a real overhang —
        // don't also treat trunk+branch as a tall stalk (lean shears arches).
        let is_cantilever = max_cantilever >= 14;
        let is_tall_stalk = !is_cantilever && aspect >= 1.45 && height >= 10;

        if !is_cantilever && !is_tall_stalk {
            continue;
        }

        // Drop scales with the *incremental* plasticity from the caller.
        // Near-linear through origin so multi-pass increments accumulate
        // cleanly instead of re-applying a large base each time.
        let reach = if is_cantilever {
            (max_cantilever as f32 / 18.0).clamp(0.85, 2.8)
        } else {
            (aspect / 1.5).clamp(0.85, 2.5)
        };
        // Cantilevers: real tip drop. Stalks: light bow only — heavy
        // vertical compress piles a column onto itself and nukes volume.
        let tip_sag = if is_cantilever {
            (plasticity * 28.0 * reach)
                .min(height as f32 * 0.55)
                .max(0.0)
        } else {
            (plasticity * 6.0 * reach)
                .min(height as f32 * 0.12)
                .max(0.0)
        };
        if tip_sag < 0.35 && !is_tall_stalk {
            continue;
        }

        // Stalks bow sideways; cantilevers stay in-plane.
        let lean = if is_tall_stalk {
            (plasticity * aspect * 2.2).clamp(0.0, height as f32 * 0.35)
        } else {
            0.0
        };
        if tip_sag < 0.35 && lean < 0.35 {
            continue;
        }

        let h = height.max(1) as f32;
        let denom = if is_cantilever {
            max_cantilever as f32
        } else {
            h
        };

        // Claim current seats first so unmoved voxels keep their cells.
        let mut claimed: HashSet<(u32, u32, u32)> = HashSet::new();
        for &i in &idxs {
            let v = next[i];
            claimed.insert((v.x, v.y, v.z));
        }

        for &i in &idxs {
            let v = voxels[i];
            let Some(&d) = dist.get(&(v.x, v.y, v.z)) else {
                continue;
            };

            let t = if is_cantilever {
                let hz = horiz_of(v.x, v.z);
                if hz < 8 || d <= v.y.saturating_add(3) {
                    continue;
                }
                d as f32 / denom
            } else {
                (v.y.saturating_sub(min_y) as f32) / h
            };

            let dy = (tip_sag * t * t).round() as i32;
            let dx = (lean * t * t).round() as i32;
            if dy <= 0 && dx == 0 {
                continue;
            }
            let want_x = v.x as i32 + dx;
            let want_y = (v.y as i32 - dy).max(0);
            let want_z = v.z as i32;
            // Free the old seat before claiming a new one.
            claimed.remove(&(v.x, v.y, v.z));
            let (nx, ny, nz) = free_seat(want_x, want_y, want_z, res, &mut claimed);
            if nx != v.x || ny != v.y || nz != v.z {
                any_sag = true;
            }
            next[i] = LabeledVoxel {
                x: nx,
                y: ny,
                z: nz,
                id: v.id,
            };
        }
    }

    if !any_sag {
        return false;
    }
    *voxels = dedup_voxels(next);
    reseal_arch_gaps(voxels);
    true
}

/// If sag opened a 1-cell gap between two solids of the same id, fill
/// the midpoint so the branch stays one piece (arch, not crumbs).
fn reseal_arch_gaps(voxels: &mut Vec<LabeledVoxel>) {
    let set: HashSet<(u32, u32, u32)> = voxels.iter().map(|v| (v.x, v.y, v.z)).collect();
    let mut id_at: HashMap<(u32, u32, u32), ComponentId> = HashMap::new();
    for v in voxels.iter() {
        id_at.insert((v.x, v.y, v.z), v.id);
    }
    let mut add: Vec<LabeledVoxel> = Vec::new();
    let dirs = [
        (2i32, 0, 0),
        (-2, 0, 0),
        (0, 2, 0),
        (0, -2, 0),
        (0, 0, 2),
        (0, 0, -2),
    ];
    for v in voxels.iter() {
        for (dx, dy, dz) in dirs {
            let tx = v.x as i32 + dx;
            let ty = v.y as i32 + dy;
            let tz = v.z as i32 + dz;
            if tx < 0 || ty < 0 || tz < 0 {
                continue;
            }
            let far = (tx as u32, ty as u32, tz as u32);
            if !set.contains(&far) {
                continue;
            }
            if id_at.get(&far).copied() != Some(v.id) {
                continue;
            }
            let mx = (v.x as i32 + tx) / 2;
            let my = (v.y as i32 + ty) / 2;
            let mz = (v.z as i32 + tz) / 2;
            if mx < 0 || my < 0 || mz < 0 {
                continue;
            }
            let mid = (mx as u32, my as u32, mz as u32);
            if set.contains(&mid) {
                continue;
            }
            add.push(LabeledVoxel {
                x: mid.0,
                y: mid.1,
                z: mid.2,
                id: v.id,
            });
        }
    }
    if add.is_empty() {
        return;
    }
    voxels.extend(add);
    *voxels = dedup_voxels(std::mem::take(voxels));
}

/// Compress soft clay into a **thick** mound: bench-pack, cap peak
/// height from volume, spread overflow into a neighbourhood, then
/// equalise the height field so the pile is a dome — not needles.
fn volumetric_splat(
    voxels: &mut Vec<LabeledVoxel>,
    plasticity: f32,
    iterations: u32,
    rmin: UVec3,
    rmax: UVec3,
) -> u32 {
    // Splat is the last, softest-clay stage (caller also gates ≥ 0.5).
    if plasticity < 0.5 {
        return 0;
    }

    // Force every column onto the bench — splat is gravity pile-up.
    for v in voxels.iter_mut() {
        // Will re-pack below; just collect.
        let _ = v;
    }
    let mut cols: HashMap<(u32, u32), Vec<ComponentId>> = HashMap::new();
    // Represent each solid as one unit in its (x,z) column; remember
    // a donor component id for bookkeeping (majority wins on write).
    for v in voxels.iter() {
        cols.entry((v.x, v.z)).or_default().push(v.id);
    }
    // Bench-pack: height = count, floor = 0.
    let mut height: HashMap<(u32, u32), u32> = cols
        .iter()
        .map(|(&k, ids)| (k, ids.len() as u32))
        .collect();
    let mut id_at: HashMap<(u32, u32), ComponentId> = cols
        .iter()
        .map(|(&k, ids)| (k, majority_id(ids)))
        .collect();

    let volume: u32 = height.values().sum();
    if volume == 0 {
        return 0;
    }
    let natural = (volume as f32).cbrt();
    let peak_now = height.values().copied().max().unwrap_or(1).max(1) as f32;
    // Blend from current height toward a soft mound — not an instant
    // pancake the moment splat unlocks.
    let compress = ((plasticity - 0.5) / 0.5).clamp(0.0, 1.0).powf(1.35);
    let mound = (natural * 0.85).clamp(4.0, 28.0);
    let target_h = (peak_now * (1.0 - compress) + mound * compress)
        .round()
        .max(4.0) as u32;

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

    let mut steps = 0u32;
    // Pass A: knock peaks down into neighbours until ≤ target_h.
    for _ in 0..iterations {
        steps += 1;
        let mut moved = false;
        let snapshot = height.clone();
        let keys: Vec<(u32, u32)> = snapshot.keys().copied().collect();
        for (x, z) in keys {
            let h = *snapshot.get(&(x, z)).unwrap_or(&0);
            if h <= target_h {
                continue;
            }
            let mut best: Option<(u32, u32, u32)> = None;
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
                let c = *height.get(&(nx, nz)).unwrap_or(&0);
                if c >= target_h {
                    continue;
                }
                let better = best.map(|(bc, _, _)| c < bc).unwrap_or(true);
                if better {
                    best = Some((c, nx, nz));
                }
            }
            // If every neighbour is full, search a wider ring.
            let dest = best.or_else(|| {
                find_spread_seat(x, z, target_h, &height, rmin, rmax, 4)
            });
            let Some((_, nx, nz)) = dest else {
                continue;
            };
            let donor = id_at.get(&(x, z)).copied().unwrap_or(1);
            *height.get_mut(&(x, z)).unwrap() -= 1;
            if height[&(x, z)] == 0 {
                height.remove(&(x, z));
                id_at.remove(&(x, z));
            }
            *height.entry((nx, nz)).or_insert(0) += 1;
            id_at.entry((nx, nz)).or_insert(donor);
            moved = true;
        }
        if !moved {
            break;
        }
        if height.values().all(|&h| h <= target_h) {
            break;
        }
    }

    // Pass B: laplacian equalise → dome / soft mound, kills spikes.
    let equalise = ((8.0 + plasticity * 24.0) as u32).min(iterations);
    for _ in 0..equalise {
        steps += 1;
        let snapshot = height.clone();
        let keys: Vec<(u32, u32)> = snapshot.keys().copied().collect();
        let mut moved = false;
        for (x, z) in keys {
            let h = *snapshot.get(&(x, z)).unwrap_or(&0);
            if h == 0 {
                continue;
            }
            let mut sum = 0u32;
            let mut n = 0u32;
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
                sum += *snapshot.get(&(tx as u32, tz as u32)).unwrap_or(&0);
                n += 1;
            }
            if n == 0 {
                continue;
            }
            let avg = sum / n;
            if h > avg + 1 {
                // Donate one to the shortest neighbour.
                let mut best: Option<(u32, u32, u32)> = None;
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
                    let c = *height.get(&(nx, nz)).unwrap_or(&0);
                    let better = best.map(|(bc, _, _)| c < bc).unwrap_or(true);
                    if better {
                        best = Some((c, nx, nz));
                    }
                }
                if let Some((_, nx, nz)) = best {
                    let donor = id_at
                        .get(&(x, z))
                        .copied()
                        .or_else(|| id_at.get(&(nx, nz)).copied())
                        .unwrap_or(1);
                    *height.get_mut(&(x, z)).unwrap() -= 1;
                    if height[&(x, z)] == 0 {
                        height.remove(&(x, z));
                        id_at.remove(&(x, z));
                    }
                    *height.entry((nx, nz)).or_insert(0) += 1;
                    id_at.entry((nx, nz)).or_insert(donor);
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }

    // Rebuild voxel list: solid columns from y=0 .. h-1.
    let mut next = Vec::new();
    for ((x, z), h) in height {
        let id = id_at.get(&(x, z)).copied().unwrap_or(1);
        for y in 0..h {
            next.push(LabeledVoxel { x, y, z, id });
        }
    }
    *voxels = next;
    steps
}

fn find_spread_seat(
    x: u32,
    z: u32,
    target_h: u32,
    height: &HashMap<(u32, u32), u32>,
    rmin: UVec3,
    rmax: UVec3,
    max_ring: i32,
) -> Option<(u32, u32, u32)> {
    let mut best: Option<(u32, i32, u32, u32)> = None; // h, dist, x, z
    for ring in 2..=max_ring {
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs() != ring && dz.abs() != ring {
                    continue;
                }
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
                let c = *height.get(&(nx, nz)).unwrap_or(&0);
                if c >= target_h {
                    continue;
                }
                let dist = dx.abs() + dz.abs();
                let better = match best {
                    None => true,
                    Some((bh, bd, _, _)) => c < bh || (c == bh && dist < bd),
                };
                if better {
                    best = Some((c, dist, nx, nz));
                }
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.map(|(c, _, nx, nz)| (c, nx, nz))
}

fn majority_id(ids: &[ComponentId]) -> ComponentId {
    let mut counts: HashMap<ComponentId, u32> = HashMap::new();
    for &id in ids {
        *counts.entry(id).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(id, _)| id)
        .unwrap_or(1)
}

/// Drop lone needle voxels (0–1 six-neighbours). Does not dilate, so
/// volume stays conserved — avoids the old “grow a spiky shell” look.
fn smooth_solid_mask(voxels: &mut Vec<LabeledVoxel>, _rmin: UVec3, _rmax: UVec3) {
    if voxels.is_empty() {
        return;
    }
    let set: HashSet<(u32, u32, u32)> = voxels.iter().map(|v| (v.x, v.y, v.z)).collect();
    let dirs6 = [
        (1i32, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];
    let mut next = Vec::with_capacity(voxels.len());
    for v in voxels.iter() {
        let mut n = 0u32;
        for (dx, dy, dz) in dirs6 {
            let nx = v.x as i32 + dx;
            let ny = v.y as i32 + dy;
            let nz = v.z as i32 + dz;
            if nx < 0 || ny < 0 || nz < 0 {
                continue;
            }
            if set.contains(&(nx as u32, ny as u32, nz as u32)) {
                n += 1;
            }
        }
        // Keep bench skin even if sparse; drop mid-air needles.
        if n >= 2 || v.y == 0 {
            next.push(*v);
        }
    }
    if next.len() * 2 >= voxels.len() {
        *voxels = next;
    }
}

fn dedup_voxels(voxels: Vec<LabeledVoxel>) -> Vec<LabeledVoxel> {
    let mut seen: HashSet<(u32, u32, u32)> = HashSet::new();
    let mut out = Vec::with_capacity(voxels.len());
    for v in voxels {
        if seen.insert((v.x, v.y, v.z)) {
            out.push(v);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn write_solids_to_grid<F>(
    grid: &mut Grid,
    voxels: &[LabeledVoxel],
    rmin: UVec3,
    rmax: UVec3,
    res: UVec3,
    track: &mut F,
    dirty_min: &mut UVec3,
    dirty_max: &mut UVec3,
    any_dirty: &mut bool,
) where
    F: FnMut(u32, u32, u32, f32),
{
    let mut mn = UVec3::new(res.x, res.y, res.z);
    let mut mx = UVec3::ZERO;
    let mut solid_set: HashSet<(u32, u32, u32)> = HashSet::new();
    for v in voxels {
        solid_set.insert((v.x, v.y, v.z));
        mn = mn.min(UVec3::new(v.x, v.y, v.z));
        mx = mx.max(UVec3::new(v.x + 1, v.y + 1, v.z + 1));
    }
    if solid_set.is_empty() {
        return;
    }

    let write_min = UVec3::new(
        mn.x.saturating_sub(BAND + 1).min(rmin.x),
        0,
        mn.z.saturating_sub(BAND + 1).min(rmin.z),
    );
    let write_max = UVec3::new(
        (mx.x + BAND + 1).max(rmax.x).min(res.x),
        (mx.y + BAND + 2).max(rmax.y).min(res.y),
        (mx.z + BAND + 1).max(rmax.z).min(res.z),
    );
    let clear_min = UVec3::new(rmin.x.min(write_min.x), 0, rmin.z.min(write_min.z));
    let clear_max = UVec3::new(
        rmax.x.max(write_max.x),
        rmax.y.max(write_max.y),
        rmax.z.max(write_max.z),
    );

    let sx = (write_max.x - write_min.x) as usize;
    let sy = (write_max.y - write_min.y) as usize;
    let sz = (write_max.z - write_min.z) as usize;
    if sx == 0 || sy == 0 || sz == 0 {
        return;
    }

    // Binary solid mask + one light exterior blur for the band only.
    // Interior stays exactly the solid set so volume doesn't inflate.
    let mut solid_mask = vec![false; sx * sy * sz];
    for &(x, y, z) in &solid_set {
        if x < write_min.x
            || y < write_min.y
            || z < write_min.z
            || x >= write_max.x
            || y >= write_max.y
            || z >= write_max.z
        {
            continue;
        }
        solid_mask[mask_i(x, y, z, write_min, sx, sy)] = true;
    }

    let vs = grid.voxel_size();
    let band_f = BAND as f32;
    for iz in clear_min.z..clear_max.z {
        for iy in clear_min.y..clear_max.y {
            for ix in clear_min.x..clear_max.x {
                let inside = ix >= write_min.x
                    && iy >= write_min.y
                    && iz >= write_min.z
                    && ix < write_max.x
                    && iy < write_max.y
                    && iz < write_max.z;
                let new = if inside {
                    let li = mask_i(ix, iy, iz, write_min, sx, sy);
                    if solid_mask[li] {
                        // Distance to exterior for a clean interior band.
                        let d = min_empty_mask_dist(
                            ix, iy, iz, &solid_mask, write_min, write_max, sx, sy,
                        );
                        (-d * vs).min(-1e-4)
                    } else {
                        let d = min_solid_mask_dist(
                            ix, iy, iz, &solid_mask, write_min, write_max, sx, sy,
                        );
                        (d * vs).max(1e-4).min(vs * (band_f + 1.0))
                    }
                } else {
                    vs * (band_f + 1.0)
                };
                let pre = grid.get(ix, iy, iz);
                if (pre - new).abs() < 1e-7 {
                    continue;
                }
                track(ix, iy, iz, pre);
                grid.set(ix, iy, iz, new);
                *dirty_min = dirty_min.min(UVec3::new(ix, iy, iz));
                *dirty_max = dirty_max.max(UVec3::new(ix + 1, iy + 1, iz + 1));
                *any_dirty = true;
            }
        }
    }
}

#[inline]
fn mask_i(ix: u32, iy: u32, iz: u32, write_min: UVec3, sx: usize, sy: usize) -> usize {
    let x = (ix - write_min.x) as usize;
    let y = (iy - write_min.y) as usize;
    let z = (iz - write_min.z) as usize;
    x + y * sx + z * sx * sy
}

#[allow(clippy::too_many_arguments)]
fn min_solid_mask_dist(
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
                if solid[mask_i(tx as u32, ty as u32, tz as u32, write_min, sx, sy)] {
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

#[allow(clippy::too_many_arguments)]
fn min_empty_mask_dist(
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
                    // Outside the write box counts as empty.
                    let d = ((dx * dx + dy * dy + dz * dz) as f32).sqrt();
                    if d < best {
                        best = d;
                    }
                    continue;
                }
                if !solid[mask_i(tx as u32, ty as u32, tz as u32, write_min, sx, sy)] {
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
                for iy in base.y..max.y.min(5) {
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

    /// Max solid count in any single (x,z) column — splat thickness.
    fn max_column_height(g: &Grid) -> u32 {
        let mut cols: HashMap<(u32, u32), u32> = HashMap::new();
        for c in g.allocated_chunk_coords() {
            let base = c.voxel_min();
            let max = c.voxel_max(g.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if g.get(ix, iy, iz) < 0.0 {
                            *cols.entry((ix, iz)).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
        cols.values().copied().max().unwrap_or(0)
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
        assert_eq!(solid_lowest_iy(&g), 0);
    }

    #[test]
    fn soft_tower_tips_or_splats_onto_bench() {
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
                iterations: 96,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0);
        assert_eq!(solid_lowest_iy(&g), 0);
        let peak_after = solid_peak_iy(&g);
        assert!(
            peak_after + 8 < peak_before,
            "soft tower should lose height via tip/splat: before={peak_before} after={peak_after}"
        );
        let width_after = solid_base_width_xz(&g);
        assert!(
            width_after >= width_before,
            "footprint should not shrink: before={width_before} after={width_after}"
        );
        // Volumetric splat must stay thick — not a 1–2 voxel sheet.
        let col_h = max_column_height(&g);
        assert!(
            col_h >= 4,
            "splat must be a thick mound, got max column height {col_h}"
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.75..=1.25).contains(&ratio),
            "volume drift: before={vol_before} after={vol_after}"
        );
    }

    #[test]
    fn light_settle_sags_a_long_branch_into_an_arch() {
        // Trunk on the bench + a long thin horizontal branch. At very
        // low softness the tip should ease down (arch), not snap into
        // falling crumbs or sandblast the surface.
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        // Trunk.
        for iy in 0..10 {
            for iz in 28..34 {
                for ix in 16..22 {
                    g.set(ix, iy, iz, -1.0);
                }
            }
        }
        // Branch out along +X at mid height.
        for ix in 22..52 {
            for iy in 7..10 {
                for iz in 29..33 {
                    g.set(ix, iy, iz, -1.0);
                }
            }
        }
        let tip_y_before = {
            let mut peak = 0u32;
            for iy in 0..64u32 {
                for iz in 29..33 {
                    if g.get(50, iy, iz) < 0.0 {
                        peak = peak.max(iy);
                    }
                }
            }
            peak
        };
        assert!(tip_y_before >= 7, "precondition: branch tip is raised");
        let vol_before = solid_count(&g);
        let labels = label_components(&g);
        assert_eq!(labels.component_count(), 1, "trunk+branch are one piece");
        let summary = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 0.05,
                iterations: 16,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0, "light sag should rewrite geometry");
        let tip_y_after = {
            let mut peak = 0u32;
            let mut any = false;
            for ix in 48..54u32 {
                for iy in 0..64u32 {
                    for iz in 28..34 {
                        if g.get(ix, iy, iz) < 0.0 {
                            peak = peak.max(iy);
                            any = true;
                        }
                    }
                }
            }
            assert!(any, "branch tip must still exist (not dissolve)");
            peak
        };
        assert!(
            tip_y_after < tip_y_before,
            "branch tip should sag toward the bench: before={tip_y_before} after={tip_y_after}"
        );
        // Root still on the bench.
        assert_eq!(solid_lowest_iy(&g), 0);
        // Still one connected piece — not crumbs in the air.
        let labels_after = label_components(&g);
        assert_eq!(
            labels_after.component_count(),
            1,
            "arch sag must not shatter the branch into separate pieces"
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.85..=1.2).contains(&ratio),
            "volume drift: before={vol_before} after={vol_after}"
        );
    }

    #[test]
    fn tiny_softness_without_cantilevers_does_not_sandblast() {
        // A grounded blob with no long branches: softness 0.01 should
        // match a pure drop (no solid-mask rewrite / erosion).
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        let _ = apply_primitive(
            &mut g,
            &Primitive {
                kind: PrimitiveKind::Sphere { radius: 8.0 },
                center: Vec3::new(0.0, 8.0, 0.0),
                workbench_y: Some(0.0),
            },
        );
        let labels = label_components(&g);
        let _ = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        let before = g.to_dense();
        let labels = label_components(&g);
        let _ = settle_components_plastic(
            &mut g,
            &labels,
            &PlasticSettleParams {
                plasticity: 0.01,
                iterations: 8,
            },
            |_, _, _, _| {},
        );
        assert_eq!(
            g.to_dense(),
            before,
            "no cantilever → tiny softness must not sandblast the surface"
        );
    }

    #[test]
    fn soft_sphere_becomes_a_thick_splat() {
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
                iterations: 96,
            },
            |_, _, _, _| {},
        );
        assert!(summary.voxels_touched > 0);
        assert_eq!(solid_lowest_iy(&g), 0);
        let peak_after = solid_peak_iy(&g);
        assert!(
            peak_after < peak_before,
            "sphere should lose height: before={peak_before} after={peak_after}"
        );
        let width_after = solid_base_width_xz(&g);
        assert!(
            width_after >= width_before,
            "splat should spread: before={width_before} after={width_after}"
        );
        assert!(
            max_column_height(&g) >= 4,
            "splat mound must be thick, got {}",
            max_column_height(&g)
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.75..=1.25).contains(&ratio),
            "volume drift: before={vol_before} after={vol_after}"
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
                plasticity: 0.12,
                iterations: 32,
            },
            |_, _, _, _| {},
        );
        let peak_after = solid_peak_iy(&g);
        let drop = peak_before as i32 - peak_after as i32;
        assert!(
            drop <= 5,
            "stiff blob should barely move: drop={drop} touched={}",
            summary.voxels_touched
        );
        let vol_after = solid_count(&g);
        let ratio = vol_after as f32 / vol_before as f32;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "volume drift: before={vol_before} after={vol_after}"
        );
    }
}
