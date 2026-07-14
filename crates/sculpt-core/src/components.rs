//! Connected-component labelling on the SDF grid.
//!
//! Every voxel with `φ < 0` (inside the workpiece) belongs to a
//! component; every `φ ≥ 0` voxel is empty. Two voxels join the same
//! component when they share a 6-connected face and both are inside.
//! Air pockets inside a hollow shape are not modelled — this is
//! "external solid pieces" only, matching the mental model of *bits
//! of clay lying on the workbench* rather than *cavities*.
//!
//! The output is a flat `Vec<ComponentId>` in the same x-major layout
//! as `Grid::data`, plus a per-id voxel count so the caller can rank
//! components by size (the "tiny bit" the user wants to pick).
//!
//! Cost is O(N) in grid voxels; a full-band 128³ workpiece labels in
//! roughly the same time as one mesh extract. We recompute lazily
//! (only when the selection resource asks for it), never on every
//! frame.

use std::collections::VecDeque;

use glam::UVec3;

use crate::grid::Grid;

/// Component identifier. `0` is reserved for "empty" (φ ≥ 0). Valid
/// solid components start at `1`.
pub type ComponentId = u32;

/// Sentinel for the "no component" cells.
pub const EMPTY: ComponentId = 0;

/// A per-voxel component label plus size statistics.
///
/// Sparse queries (`id_at`, `voxel_count`) are O(1); the label array
/// is publicly readable for callers that need to iterate.
pub struct ComponentField {
    ids: Vec<ComponentId>,
    res: UVec3,
    /// `voxel_count[i]` is the number of voxels with `ComponentId == i`.
    /// `voxel_count[0]` counts empty voxels.
    voxel_count: Vec<u32>,
}

impl ComponentField {
    /// Grid resolution this field was labelled for. Callers should
    /// re-label when the grid changes size (never, in Stage 1) or
    /// after any stroke (in the app layer).
    pub fn res(&self) -> UVec3 {
        self.res
    }

    /// Raw component id at a voxel (`0` = empty).
    #[inline]
    pub fn id_at(&self, x: u32, y: u32, z: u32) -> ComponentId {
        self.ids[(x + y * self.res.x + z * self.res.x * self.res.y) as usize]
    }

    /// Read-only view of the packed id array (x-major, matching
    /// `Grid::samples`).
    pub fn ids(&self) -> &[ComponentId] {
        &self.ids
    }

    /// Voxel count for a component. Returns `0` for out-of-range ids.
    pub fn voxel_count(&self, id: ComponentId) -> u32 {
        self.voxel_count.get(id as usize).copied().unwrap_or(0)
    }

    /// Approximate mm³ of a component. Simple voxel * voxel_size³ —
    /// no marching-cubes-accurate volume, but good enough for a HUD
    /// readout.
    pub fn volume_mm3(&self, id: ComponentId, voxel_size: f32) -> f32 {
        self.voxel_count(id) as f32 * voxel_size.powi(3)
    }

    /// Every non-empty component id in size order (largest first).
    /// Handy for "select the biggest" and "list the tiny fragments"
    /// UIs. Cost: O(k log k) where k = number of components.
    pub fn ids_by_size_desc(&self) -> Vec<ComponentId> {
        let mut ids: Vec<ComponentId> = (1..self.voxel_count.len() as u32).collect();
        ids.sort_by_key(|&i| std::cmp::Reverse(self.voxel_count(i)));
        ids
    }

    /// Number of distinct solid components (excludes empty).
    pub fn component_count(&self) -> usize {
        self.voxel_count.len().saturating_sub(1)
    }
}

/// Assign a component id to every solid (`φ < 0`) voxel via a
/// breadth-first flood-fill over 6-connected neighbours.
///
/// Runs in O(N) grid voxels. Uses a single reusable `VecDeque`
/// queue and a bit-per-voxel visited mask, so allocation is bounded
/// even on the 128³ Stage-1 grid.
pub fn label_components(grid: &Grid) -> ComponentField {
    let res = grid.res();
    let n = (res.x * res.y * res.z) as usize;
    let stride_y = res.x as usize;
    let stride_z = (res.x * res.y) as usize;

    let mut ids = vec![EMPTY; n];
    // voxel_count[0] = empty voxel count.
    let mut voxel_count: Vec<u32> = vec![0];
    let mut next_id: ComponentId = 1;
    let mut queue: VecDeque<(u32, u32, u32)> = VecDeque::new();

    for iz in 0..res.z {
        for iy in 0..res.y {
            for ix in 0..res.x {
                let idx = ix as usize + iy as usize * stride_y + iz as usize * stride_z;
                if grid.get(ix, iy, iz) >= 0.0 {
                    voxel_count[0] += 1;
                    continue;
                }
                if ids[idx] != EMPTY {
                    continue;
                }

                let comp_id = next_id;
                next_id += 1;
                voxel_count.push(0);

                queue.clear();
                queue.push_back((ix, iy, iz));
                ids[idx] = comp_id;

                while let Some((x, y, z)) = queue.pop_front() {
                    voxel_count[comp_id as usize] += 1;

                    // 6-connected neighbours. Inline the loop for
                    // faster iteration on the 128³ grid.
                    let neighbours: [(i32, i32, i32); 6] = [
                        (1, 0, 0),
                        (-1, 0, 0),
                        (0, 1, 0),
                        (0, -1, 0),
                        (0, 0, 1),
                        (0, 0, -1),
                    ];
                    for (dx, dy, dz) in neighbours {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        let nz = z as i32 + dz;
                        if nx < 0
                            || ny < 0
                            || nz < 0
                            || nx >= res.x as i32
                            || ny >= res.y as i32
                            || nz >= res.z as i32
                        {
                            continue;
                        }
                        let (nx, ny, nz) = (nx as u32, ny as u32, nz as u32);
                        let nidx = nx as usize
                            + ny as usize * stride_y
                            + nz as usize * stride_z;
                        if ids[nidx] != EMPTY {
                            continue;
                        }
                        if grid.get(nx, ny, nz) >= 0.0 {
                            continue;
                        }
                        ids[nidx] = comp_id;
                        queue.push_back((nx, ny, nz));
                    }
                }
            }
        }
    }

    ComponentField {
        ids,
        res,
        voxel_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{apply_sphere_brush, BrushMode, SphereBrush};
    use glam::Vec3;

    fn sphere_grid(radius: f32, centre: Vec3) -> Grid {
        Grid::from_sphere(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO, centre, radius)
    }

    #[test]
    fn empty_grid_has_no_components() {
        let g = Grid::empty(UVec3::new(16, 16, 16), 1.0, Vec3::ZERO);
        let f = label_components(&g);
        assert_eq!(f.component_count(), 0);
        assert_eq!(f.id_at(8, 8, 8), EMPTY);
    }

    #[test]
    fn single_sphere_is_one_component() {
        let g = sphere_grid(6.0, Vec3::new(16.0, 16.0, 16.0));
        let f = label_components(&g);
        assert_eq!(f.component_count(), 1);
        let centre_id = f.id_at(16, 16, 16);
        assert_ne!(centre_id, EMPTY);
        // Way outside → empty.
        assert_eq!(f.id_at(0, 0, 0), EMPTY);
    }

    #[test]
    fn two_disjoint_spheres_are_two_components() {
        // Start with one sphere, then carve away the middle so we
        // have a big lump and a small lump on the far side.
        let mut g = sphere_grid(10.0, Vec3::new(16.0, 16.0, 16.0));
        // Carve a large chunk out of the middle, splitting the ball.
        let carver = SphereBrush {
            center: Vec3::new(16.0, 16.0, 22.0),
            radius: 4.0,
            mode: BrushMode::Press,
            direction: Vec3::new(0.0, 0.0, -1.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &carver);
        // Actually the single big carve won't split; instead build
        // two explicit small spheres in an empty grid.
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        let add_a = SphereBrush {
            center: Vec3::new(8.0, 16.0, 16.0),
            radius: 3.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        let add_b = SphereBrush {
            center: Vec3::new(24.0, 16.0, 16.0),
            radius: 3.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &add_a);
        let _ = apply_sphere_brush(&mut g, &add_b);

        let f = label_components(&g);
        assert_eq!(f.component_count(), 2);
        // The two centres must have different ids.
        let id_a = f.id_at(8, 16, 16);
        let id_b = f.id_at(24, 16, 16);
        assert_ne!(id_a, EMPTY);
        assert_ne!(id_b, EMPTY);
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn tiny_fragment_ranks_last_by_size() {
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        let big = SphereBrush {
            center: Vec3::new(8.0, 16.0, 16.0),
            radius: 6.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        let tiny = SphereBrush {
            center: Vec3::new(24.0, 16.0, 16.0),
            radius: 2.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(-1.0, 0.0, 0.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &big);
        let _ = apply_sphere_brush(&mut g, &tiny);

        let f = label_components(&g);
        let ranked = f.ids_by_size_desc();
        assert_eq!(ranked.len(), 2);
        // First element = biggest.
        assert!(f.voxel_count(ranked[0]) > f.voxel_count(ranked[1]));
    }

    #[test]
    fn volume_mm3_scales_with_voxel_size() {
        let mut g = Grid::empty(UVec3::new(16, 16, 16), 2.0, Vec3::ZERO);
        let add = SphereBrush {
            center: Vec3::new(16.0, 16.0, 16.0),
            radius: 4.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(0.0, 0.0, -1.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &add);
        let f = label_components(&g);
        assert_eq!(f.component_count(), 1);
        let id = f.ids_by_size_desc()[0];
        let count = f.voxel_count(id) as f32;
        // Each voxel is 2 mm × 2 mm × 2 mm = 8 mm³.
        assert!((f.volume_mm3(id, 2.0) - count * 8.0).abs() < 1e-3);
    }
}
