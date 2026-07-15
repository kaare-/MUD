//! Rigid "rest on bench" and free translation for labelled SDF grids.
//!
//! Every connected component is treated as an infinitely rigid lump of
//! clay: no plastic deformation, no simulation, just a **rigid**
//! translation along one or more axes. Two things get moved together
//! for each component:
//!
//! - the interior voxels (`φ < 0`), and
//! - the narrow-band voxels around them (`0 ≤ φ < band`).
//!
//! Moving *only* the interior — the previous implementation — sheared
//! the SDF at the piece's boundary: the outer band was stuck at the
//! piece's old position while the interior teleported to the new one.
//! Marching cubes then read a corrugated `φ = 0` isosurface where the
//! narrow band and the "cleared" cells met, giving the whole piece a
//! crumpled, rougher look after a rest.
//!
//! The new algorithm re-emits, for every cell in a component's
//! *widened* AABB, the pre-op SDF value at the shifted source
//! position. Overlapping destinations union via `min` so multiple
//! pieces settling onto the same spot weld naturally.
//!
//! Undo-friendly: every mutation is reported via `on_pre_mutation`
//! with the pre-op value so the app-side stroke recorder can journal
//! one undo entry that reverses the whole operation.

use glam::{IVec3, UVec3};

use crate::components::{ComponentField, ComponentId, EMPTY};
use crate::grid::{DirtyRegion, Grid};

/// Voxels of narrow band we assume surround every solid interior.
/// The mesher needs a smooth band on both sides of `φ = 0`; three
/// voxels is enough for marching cubes and comfortably covers Bevy's
/// gradient probe. Widen this if we ever migrate to a fatter band.
const BAND: u32 = 3;

/// Result of a rest-on-bench pass.
#[derive(Copy, Clone, Debug, Default)]
pub struct RestSummary {
    /// Total number of distinct components inspected (including
    /// those already at rest).
    pub components: u32,
    /// Number of components that were actually translated. A piece
    /// already sitting on the bench (`min iy == 0`) isn't counted.
    pub moved: u32,
    /// Union of every voxel that changed value during the op. `None`
    /// when nothing changed.
    pub dirty: Option<DirtyRegion>,
}

/// Drop every non-empty component onto the workbench.
///
/// - `labels` must have been produced from `grid` with a fresh call
///   to [`crate::components::label_components`]; the operation
///   trusts the label→voxel mapping.
/// - `on_pre_mutation` receives the pre-mutation SDF value for every
///   voxel we touch, so the app-side stroke recorder can journal a
///   single undo entry that reverses the whole rest.
pub fn rest_components_on_bench<F>(
    grid: &mut Grid,
    labels: &ComponentField,
    on_pre_mutation: F,
) -> RestSummary
where
    F: FnMut(u32, u32, u32, f32),
{
    let component_count = labels.component_count();
    if component_count == 0 {
        return RestSummary::default();
    }

    let res = grid.res();
    // Per-component lowest interior iy → drop offset.
    let mut lowest_iy = vec![u32::MAX; component_count + 1];
    for iz in 0..res.z {
        for iy in 0..res.y {
            for ix in 0..res.x {
                let id = labels.id_at(ix, iy, iz);
                if id != EMPTY {
                    let slot = &mut lowest_iy[id as usize];
                    *slot = (*slot).min(iy);
                }
            }
        }
    }

    let mut deltas: Vec<(ComponentId, IVec3)> = Vec::new();
    let mut moved = 0u32;
    for c in 1..=component_count as ComponentId {
        let low = lowest_iy[c as usize];
        if low == 0 || low == u32::MAX {
            continue;
        }
        deltas.push((c, IVec3::new(0, -(low as i32), 0)));
        moved += 1;
    }

    let dirty = translate_components(grid, labels, &deltas, on_pre_mutation);

    RestSummary {
        components: component_count as u32,
        moved,
        dirty,
    }
}

/// Rigidly translate one component by `delta` voxels. Positive `y`
/// lifts, negative drops; same for x and z. The workbench (`iy = 0`)
/// is a hard floor — any part of the piece that would land at
/// `iy < 0` is clipped off (its cells simply don't get re-emitted).
///
/// The core-side helper doesn't clamp the delta to keep the piece
/// on the workbench; call sites (Move tool, gravity) can precompute
/// a delta that already respects bench and grid bounds if they need
/// stricter semantics.
///
/// Returns the union of every voxel whose value changed, or `None`
/// when the delta is `(0, 0, 0)` / the component doesn't exist.
pub fn translate_component<F>(
    grid: &mut Grid,
    labels: &ComponentField,
    id: ComponentId,
    delta: IVec3,
    on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    if delta == IVec3::ZERO {
        return None;
    }
    let deltas = [(id, delta)];
    translate_components(grid, labels, &deltas, on_pre_mutation)
}

/// Bulk rigid translation. Applies each `(id, delta)` in a single
/// pass: every affected cell samples the pre-op grid at the shifted
/// position, union-min across contributions, workbench-clipped.
///
/// This is the common backbone for rest-on-bench (per-component
/// `y`-only drop) and Move (single component, arbitrary delta).
pub fn translate_components<F>(
    grid: &mut Grid,
    labels: &ComponentField,
    deltas: &[(ComponentId, IVec3)],
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let res = grid.res();
    let component_count = labels.component_count();
    if component_count == 0 {
        return None;
    }

    // Per-component delta (0 for anything not in `deltas`, so
    // non-moving components fall through the algorithm unchanged
    // via a `dy = 0` contribution at the same cell).
    let mut per_delta: Vec<IVec3> = vec![IVec3::ZERO; component_count + 1];
    let mut any_move = false;
    for &(id, delta) in deltas {
        if (id as usize) < per_delta.len() {
            per_delta[id as usize] = delta;
            if delta != IVec3::ZERO {
                any_move = true;
            }
        }
    }
    if !any_move {
        return None;
    }

    // Widened OLD AABB per component. `None` = empty component /
    // out-of-range id, skipped in the loop below.
    let mut widened: Vec<Option<(UVec3, UVec3)>> = Vec::with_capacity(component_count + 1);
    widened.push(None);
    for c in 1..=component_count as ComponentId {
        if let Some((mn, mx)) = labels.bounds_of(c) {
            let wmin = UVec3::new(
                mn.x.saturating_sub(BAND),
                mn.y.saturating_sub(BAND),
                mn.z.saturating_sub(BAND),
            );
            let wmax = UVec3::new(
                (mx.x + BAND).min(res.x),
                (mx.y + BAND).min(res.y),
                (mx.z + BAND).min(res.z),
            );
            widened.push(Some((wmin, wmax)));
        } else {
            widened.push(None);
        }
    }

    // Bounding box of every cell we might touch (union of every
    // widened AABB and every widened AABB shifted by its delta).
    let mut union_min = IVec3::new(i32::MAX, i32::MAX, i32::MAX);
    let mut union_max = IVec3::new(i32::MIN, i32::MIN, i32::MIN);
    for c in 1..=component_count {
        let Some((mn, mx)) = widened[c] else { continue };
        // Source box (unchanged).
        expand_i(&mut union_min, &mut union_max, mn.as_ivec3(), mx.as_ivec3());
        // Destination box for the component's delta.
        let d = per_delta[c];
        let dst_min = mn.as_ivec3() + d;
        let dst_max = mx.as_ivec3() + d;
        expand_i(&mut union_min, &mut union_max, dst_min, dst_max);
    }
    // Clip to the grid.
    union_min.x = union_min.x.max(0);
    union_min.y = union_min.y.max(0);
    union_min.z = union_min.z.max(0);
    union_max.x = union_max.x.min(res.x as i32);
    union_max.y = union_max.y.min(res.y as i32);
    union_max.z = union_max.z.min(res.z as i32);
    if union_min.x >= union_max.x
        || union_min.y >= union_max.y
        || union_min.z >= union_max.z
    {
        return None;
    }

    let empty_sdf = grid.voxel_size() * 32.0;
    // Region-scoped snapshot (`PLAN.md` Track A3), not a full-domain
    // `to_dense()`: every read below stays inside `[union_min,
    // union_max)`, which we've already computed above as the union
    // of every component's old + shifted widened AABB — exactly the
    // bound this algorithm can possibly touch or read from.
    let old_region = grid.snapshot_region(union_min.as_uvec3(), union_max.as_uvec3());
    let old_ids = labels.ids();
    let stride_y = res.x as usize;
    let stride_z = (res.x * res.y) as usize;

    let mut min = UVec3::new(u32::MAX, u32::MAX, u32::MAX);
    let mut max = UVec3::ZERO;

    for dst_iz in union_min.z..union_max.z {
        for dst_iy in union_min.y..union_max.y {
            for dst_ix in union_min.x..union_max.x {
                let (dx, dy, dz) = (dst_ix as u32, dst_iy as u32, dst_iz as u32);
                let dst_idx =
                    dx as usize + dy as usize * stride_y + dz as usize * stride_z;
                let old_val_here = old_region.get(dx, dy, dz);

                let mut new_val = f32::INFINITY;

                // Non-moving contribution: cell keeps its OLD value
                // iff it is / was inside a non-moving component's
                // widened AABB. Cells inside a *moving* component's
                // widened AABB drop their old value — the piece
                // isn't there any more, everything must come from
                // shifted contributions.
                let mut base_is_from_non_moving = false;
                for c in 1..=component_count {
                    if per_delta[c] != IVec3::ZERO {
                        continue;
                    }
                    let Some((wmin, wmax)) = widened[c] else { continue };
                    if dx >= wmin.x
                        && dx < wmax.x
                        && dy >= wmin.y
                        && dy < wmax.y
                        && dz >= wmin.z
                        && dz < wmax.z
                    {
                        // Only cells labelled c or EMPTY belong to
                        // c's shifted band. Anything else here is
                        // owned by yet another component and gets
                        // handled when *that* component's loop iter
                        // fires below.
                        let label_here = old_ids[dst_idx];
                        if label_here == c as ComponentId || label_here == EMPTY {
                            new_val = new_val.min(old_val_here);
                            base_is_from_non_moving = true;
                            break;
                        }
                    }
                }

                // Moving components: sample old grid at src = dst -
                // delta and union-min into new_val.
                for c in 1..=component_count {
                    let d = per_delta[c];
                    if d == IVec3::ZERO {
                        continue;
                    }
                    let src = IVec3::new(dst_ix, dst_iy, dst_iz) - d;
                    if src.x < 0
                        || src.y < 0
                        || src.z < 0
                        || src.x >= res.x as i32
                        || src.y >= res.y as i32
                        || src.z >= res.z as i32
                    {
                        continue;
                    }
                    let (sx, sy, sz) = (src.x as u32, src.y as u32, src.z as u32);
                    let Some((wmin, wmax)) = widened[c] else { continue };
                    if sx < wmin.x
                        || sx >= wmax.x
                        || sy < wmin.y
                        || sy >= wmax.y
                        || sz < wmin.z
                        || sz >= wmax.z
                    {
                        continue;
                    }
                    let src_idx =
                        sx as usize + sy as usize * stride_y + sz as usize * stride_z;
                    let label_src = old_ids[src_idx];
                    if label_src != c as ComponentId && label_src != EMPTY {
                        continue;
                    }
                    new_val = new_val.min(old_region.get(sx, sy, sz));
                }

                let final_val = if new_val.is_finite() {
                    new_val
                } else if base_is_from_non_moving {
                    // Shouldn't reach this branch (new_val would be
                    // finite), but preserve base defensively.
                    old_val_here
                } else {
                    empty_sdf
                };

                if (final_val - old_val_here).abs() > 1e-6 {
                    on_pre_mutation(dx, dy, dz, old_val_here);
                    grid.set(dx, dy, dz, final_val);
                    expand(&mut min, &mut max, dx, dy, dz);
                }
            }
        }
    }

    if min.x == u32::MAX {
        None
    } else {
        Some(DirtyRegion { min, max })
    }
}

fn expand(min: &mut UVec3, max: &mut UVec3, ix: u32, iy: u32, iz: u32) {
    min.x = min.x.min(ix);
    min.y = min.y.min(iy);
    min.z = min.z.min(iz);
    max.x = max.x.max(ix + 1);
    max.y = max.y.max(iy + 1);
    max.z = max.z.max(iz + 1);
}

fn expand_i(min: &mut IVec3, max: &mut IVec3, lo: IVec3, hi: IVec3) {
    min.x = min.x.min(lo.x);
    min.y = min.y.min(lo.y);
    min.z = min.z.min(lo.z);
    max.x = max.x.max(hi.x);
    max.y = max.y.max(hi.y);
    max.z = max.z.max(hi.z);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{apply_sphere_brush, BrushMode, SphereBrush};
    use crate::components::label_components;
    use glam::Vec3;

    fn add_sphere(g: &mut Grid, centre: Vec3, radius: f32) {
        let brush = SphereBrush {
            center: centre,
            radius,
            mode: BrushMode::Pull,
            direction: Vec3::new(0.0, -1.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(g, &brush);
    }

    fn lowest_iy_of(g: &Grid) -> u32 {
        let labels = label_components(g);
        let id = labels
            .ids_by_size_desc()
            .into_iter()
            .next()
            .expect("at least one component");
        let (mn, _) = labels.bounds_of(id).expect("component has bounds");
        mn.y
    }

    #[test]
    fn floating_piece_drops_so_its_bottom_touches_bench() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(16.0, 20.0, 16.0), 4.0);
        let labels = label_components(&g);
        let summary = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        assert_eq!(summary.moved, 1);
        assert_eq!(lowest_iy_of(&g), 0);
    }

    #[test]
    fn resting_piece_is_left_alone() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(16.0, 4.0, 16.0), 5.0);
        let labels = label_components(&g);
        let before = g.to_dense();
        let summary = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        assert_eq!(summary.moved, 0);
        assert_eq!(g.to_dense(), before);
    }

    #[test]
    fn two_disjoint_pieces_both_land() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(10.0, 18.0, 16.0), 3.0);
        add_sphere(&mut g, Vec3::new(22.0, 24.0, 16.0), 3.0);
        let labels = label_components(&g);
        assert_eq!(labels.component_count(), 2);
        let summary = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        assert_eq!(summary.moved, 2);

        // Both should now touch the bench.
        let labels_after = label_components(&g);
        for id in labels_after.ids_by_size_desc() {
            let (mn, _) = labels_after.bounds_of(id).expect("component");
            assert_eq!(mn.y, 0, "component {id} should touch the bench");
        }
    }

    #[test]
    fn pre_mutation_callback_captures_touched_voxels() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(16.0, 20.0, 16.0), 3.0);
        let labels = label_components(&g);
        let mut hits = 0;
        let _ = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {
            hits += 1;
        });
        assert!(hits > 0);
    }

    #[test]
    fn empty_grid_is_a_noop() {
        let mut g = Grid::empty(UVec3::new(16, 16, 16), 1.0, Vec3::ZERO);
        let labels = label_components(&g);
        let summary = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        assert_eq!(summary.components, 0);
        assert_eq!(summary.moved, 0);
        assert!(summary.dirty.is_none());
    }

    /// The whole point of the rewrite: after a rest, the surface at
    /// the dropped position must match the surface the piece had in
    /// mid-air. We snapshot the narrow band (`0 < φ < 3`) before the
    /// drop and verify every voxel is preserved to within a small
    /// tolerance at its shifted position.
    #[test]
    fn dropped_piece_preserves_narrow_band_around_the_new_surface() {
        let mut g = Grid::empty(UVec3::new(48, 48, 48), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(24.0, 24.0, 24.0), 8.0);

        let mut band_before: Vec<(u32, u32, u32, f32)> = Vec::new();
        for iz in 0..48 {
            for iy in 0..48 {
                for ix in 0..48 {
                    let v = g.get(ix, iy, iz);
                    if v > 0.0 && v < 3.0 {
                        band_before.push((ix, iy, iz, v));
                    }
                }
            }
        }
        assert!(!band_before.is_empty(), "starter sphere should have a band");

        // Actual drop = lowest interior iy pre-rest (the piece's
        // bottom lands at iy = 0).
        let labels_before = label_components(&g);
        let id_before = labels_before.ids_by_size_desc()[0];
        let drop = labels_before.bounds_of(id_before).unwrap().0.y;

        let _ = rest_components_on_bench(&mut g, &labels_before, |_, _, _, _| {});

        let mut preserved = 0usize;
        let mut sheared = 0usize;
        for (ix, iy, iz, v_before) in band_before {
            if iy < drop {
                continue;
            }
            let new_iy = iy - drop;
            let v_after = g.get(ix, new_iy, iz);
            // Tolerance covers voxel-quantisation noise at the band
            // fringe. A rigid shift preserves values to within
            // 0.51 mm for a 1 mm/voxel grid.
            if (v_after - v_before).abs() < 0.51 {
                preserved += 1;
            } else {
                sheared += 1;
            }
        }
        let ratio = preserved as f32 / (preserved + sheared).max(1) as f32;
        assert!(
            ratio > 0.9,
            "band preservation ratio {ratio:.2} — surface got sheared during rest",
        );
    }

    #[test]
    fn translate_component_shifts_a_piece_along_x() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(10.0, 16.0, 16.0), 4.0);
        let labels = label_components(&g);
        let id = labels.ids_by_size_desc()[0];
        let dirty = translate_component(
            &mut g,
            &labels,
            id,
            IVec3::new(6, 0, 0),
            |_, _, _, _| {},
        );
        assert!(dirty.is_some(), "shift should produce a dirty region");
        let labels_after = label_components(&g);
        let id_after = labels_after.ids_by_size_desc()[0];
        let (mn, mx) = labels_after.bounds_of(id_after).expect("bounds");
        // Original centre x = 10, delta = +6 → new centre x ≈ 16.
        let mid = (mn.x + mx.x) as f32 * 0.5;
        assert!(
            (mid - 16.5).abs() <= 1.0,
            "shifted centre x = {mid}, expected ≈ 16.5 voxels",
        );
    }

    /// Regression for the Track A3 region-scoped snapshot: moving one
    /// component must not disturb a distant, uninvolved one whose
    /// voxels fall well outside the moving component's widened AABB
    /// (i.e. outside the snapshot region `translate_component` takes).
    #[test]
    fn translating_one_component_leaves_a_distant_component_untouched() {
        let mut g = Grid::empty(UVec3::new(160, 160, 160), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(10.0, 30.0, 10.0), 4.0);
        add_sphere(&mut g, Vec3::new(140.0, 30.0, 140.0), 4.0);
        let labels = label_components(&g);
        assert_eq!(labels.component_count(), 2);

        let near_id = labels.id_at(10, 30, 10);
        let far_id_before = labels.id_at(140, 30, 140);
        let far_bounds_before = labels.bounds_of(far_id_before).expect("far piece has bounds");
        let far_centre_before = g.get(140, 30, 140);

        let _ = translate_component(
            &mut g,
            &labels,
            near_id,
            IVec3::new(20, 0, 0),
            |_, _, _, _| {},
        );

        // The far component's centre voxel is completely unaffected.
        assert_eq!(g.get(140, 30, 140), far_centre_before);
        let labels_after = label_components(&g);
        assert_eq!(labels_after.component_count(), 2);
        let far_id_after = labels_after.id_at(140, 30, 140);
        assert_ne!(far_id_after, EMPTY);
        let far_bounds_after = labels_after.bounds_of(far_id_after).expect("far piece still there");
        assert_eq!(far_bounds_after, far_bounds_before, "far piece must not move or resize");
    }

    #[test]
    fn zero_delta_is_a_noop() {
        let mut g = Grid::empty(UVec3::new(16, 16, 16), 1.0, Vec3::ZERO);
        add_sphere(&mut g, Vec3::new(8.0, 8.0, 8.0), 3.0);
        let labels = label_components(&g);
        let id = labels.ids_by_size_desc()[0];
        let before = g.to_dense();
        let dirty = translate_component(
            &mut g,
            &labels,
            id,
            IVec3::ZERO,
            |_, _, _, _| {},
        );
        assert!(dirty.is_none());
        assert_eq!(g.to_dense(), before);
    }
}
