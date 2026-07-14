//! Rigid "rest on bench" operation for a labelled SDF grid.
//!
//! Every connected component is treated as an infinitely rigid lump of
//! clay: no plastic deformation, no simulation, just a translation
//! along -Y so its lowest solid voxel sits on the workbench plane
//! (`iy == 0`). Components already on the bench are left alone.
//!
//! Multiple components can overlap in their landing zones — that's a
//! natural welding case for magic clay, so we union rather than
//! collide. Any resulting cluster becomes one component on the next
//! relabel.
//!
//! Undo-friendly: the operation reports every voxel it mutated with
//! its pre-mutation value via the same `on_pre_mutation` callback
//! shape used by brushes, so the app can wrap the whole thing in a
//! single `StrokeRecorder` entry.

use glam::UVec3;

use crate::components::{ComponentField, EMPTY};
use crate::grid::{DirtyRegion, Grid};

/// Result of a rest-on-bench pass.
#[derive(Copy, Clone, Debug, Default)]
pub struct RestSummary {
    /// Total number of distinct components inspected (including
    /// those already at rest).
    pub components: u32,
    /// Number of components that were actually translated. A piece
    /// already sitting on the bench (`min iy == 0`) isn't counted.
    pub moved: u32,
    /// Union of every voxel that changed (either the source that got
    /// cleared or the destination that received a value). `None`
    /// when nothing was mutated.
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
///
/// The "empty" SDF sentinel used to erase moved source cells is
/// `grid.voxel_size() * 32.0`, matching the far-positive value
/// `Grid::empty` and `Grid::sample` return for outside-the-band cells.
pub fn rest_components_on_bench<F>(
    grid: &mut Grid,
    labels: &ComponentField,
    mut on_pre_mutation: F,
) -> RestSummary
where
    F: FnMut(u32, u32, u32, f32),
{
    let res = grid.res();
    let component_count = labels.component_count();
    if component_count == 0 {
        return RestSummary::default();
    }

    // Lowest y-voxel per component id (indexed by id). Empty
    // components stay at u32::MAX and are ignored.
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

    let empty_sdf = grid.voxel_size() * 32.0;
    let old_ids = labels.ids();
    let old_samples: Vec<f32> = grid.samples().to_vec();
    let stride_y = res.x as usize;
    let stride_z = (res.x * res.y) as usize;

    let mut moved: u32 = 0;
    let mut min = UVec3::new(u32::MAX, u32::MAX, u32::MAX);
    let mut max = UVec3::ZERO;

    // First pass: clear every solid voxel that belongs to a moving
    // component. Voxels of non-moving pieces are left untouched.
    for iz in 0..res.z {
        for iy in 0..res.y {
            for ix in 0..res.x {
                let idx =
                    ix as usize + iy as usize * stride_y + iz as usize * stride_z;
                let id = old_ids[idx];
                if id == EMPTY {
                    continue;
                }
                let bottom = lowest_iy[id as usize];
                if bottom == 0 || bottom == u32::MAX {
                    continue;
                }
                let pre = old_samples[idx];
                on_pre_mutation(ix, iy, iz, pre);
                grid.set(ix, iy, iz, empty_sdf);
                expand(&mut min, &mut max, ix, iy, iz);
            }
        }
    }

    // Track which ids we've counted as moved so `moved` is a set
    // count, not a voxel count.
    let mut counted = vec![false; component_count + 1];

    // Second pass: re-emit the values at translated positions.
    for iz in 0..res.z {
        for iy in 0..res.y {
            for ix in 0..res.x {
                let idx =
                    ix as usize + iy as usize * stride_y + iz as usize * stride_z;
                let id = old_ids[idx];
                if id == EMPTY {
                    continue;
                }
                let bottom = lowest_iy[id as usize];
                if bottom == 0 || bottom == u32::MAX {
                    continue;
                }
                if !counted[id as usize] {
                    counted[id as usize] = true;
                    moved += 1;
                }
                let new_iy = iy - bottom;
                let old_value = old_samples[idx];
                let dst_idx =
                    ix as usize + new_iy as usize * stride_y + iz as usize * stride_z;
                let cur = if new_iy == iy {
                    // Same cell — we already cleared it above.
                    empty_sdf
                } else {
                    // Read live value (may have been cleared in pass
                    // 1 if this destination was also a source).
                    grid.get(ix, new_iy, iz)
                };
                // Journal the destination cell's pre value (before
                // this operation). If two pieces settle onto the
                // same cell, only the first hit records — the
                // second sees an already-touched cell, but our
                // recorder de-duplicates upstream so that's fine.
                let dst_pre_before_op = old_samples[dst_idx];
                on_pre_mutation(ix, new_iy, iz, dst_pre_before_op);
                let unioned = old_value.min(cur);
                grid.set(ix, new_iy, iz, unioned);
                expand(&mut min, &mut max, ix, new_iy, iz);
            }
        }
    }

    let dirty = if min.x == u32::MAX {
        None
    } else {
        Some(DirtyRegion { min, max })
    };

    RestSummary {
        components: component_count as u32,
        moved,
        dirty,
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

    #[test]
    fn floating_piece_drops_so_its_bottom_touches_bench() {
        // 32^3 grid, voxel size 1 mm, origin at 0 → bench at iy=0.
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        // Sphere well above bench, radius 4, centre y=20.
        add_sphere(&mut g, Vec3::new(16.0, 20.0, 16.0), 4.0);
        let labels = label_components(&g);
        let summary = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        assert_eq!(summary.moved, 1);

        // After rest, the sphere's lowest voxel should be at iy=0.
        let labels_after = label_components(&g);
        let id = labels_after.ids_by_size_desc()[0];
        let mut lowest = u32::MAX;
        for iz in 0..32 {
            for iy in 0..32 {
                for ix in 0..32 {
                    if labels_after.id_at(ix, iy, iz) == id {
                        lowest = lowest.min(iy);
                    }
                }
            }
        }
        assert_eq!(lowest, 0);
    }

    #[test]
    fn resting_piece_is_left_alone() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        // Sphere with a solid voxel at iy=0. Centre y=4, radius 5 →
        // SDF at (16, 0, 16) = -1, so iy=0 is inside → resting.
        add_sphere(&mut g, Vec3::new(16.0, 4.0, 16.0), 5.0);
        let labels = label_components(&g);
        let before = g.samples().to_vec();
        let summary = rest_components_on_bench(&mut g, &labels, |_, _, _, _| {});
        assert_eq!(summary.moved, 0);
        assert_eq!(g.samples(), before.as_slice());
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
            let mut lowest = u32::MAX;
            for iz in 0..32 {
                for iy in 0..32 {
                    for ix in 0..32 {
                        if labels_after.id_at(ix, iy, iz) == id {
                            lowest = lowest.min(iy);
                        }
                    }
                }
            }
            assert_eq!(lowest, 0, "component {id} should touch the bench");
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
        // At minimum, every source voxel + every destination voxel
        // gets a callback. Concrete count depends on the sphere, but
        // it must be > 0.
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
}
