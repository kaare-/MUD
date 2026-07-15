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

use std::collections::{HashMap, VecDeque};

use glam::UVec3;

use crate::grid::{ChunkCoord, Grid, CHUNK_SIZE};

/// Component identifier. `0` is reserved for "empty" (φ ≥ 0). Valid
/// solid components start at `1`.
pub type ComponentId = u32;

/// Sentinel for the "no component" cells.
pub const EMPTY: ComponentId = 0;

const TILE_DIM: usize = CHUNK_SIZE as usize;
const TILE_LEN: usize = TILE_DIM * TILE_DIM * TILE_DIM;
type IdTile = Box<[ComponentId; TILE_LEN]>;

#[inline]
fn tile_key(ix: u32, iy: u32, iz: u32) -> ChunkCoord {
    ChunkCoord::new(ix / CHUNK_SIZE, iy / CHUNK_SIZE, iz / CHUNK_SIZE)
}

#[inline]
fn local_index(ix: u32, iy: u32, iz: u32) -> usize {
    let lx = (ix % CHUNK_SIZE) as usize;
    let ly = (iy % CHUNK_SIZE) as usize;
    let lz = (iz % CHUNK_SIZE) as usize;
    lx + ly * TILE_DIM + lz * TILE_DIM * TILE_DIM
}

/// A per-voxel component label plus size statistics.
///
/// Storage mirrors `Grid`'s sparse tiles (`PLAN.md` Track A3): a
/// component can only ever occupy voxels inside an *allocated* `Grid`
/// tile (an unallocated one is guaranteed `φ >= 0` = empty), so the
/// label field only allocates a tile where labelling actually wrote
/// a non-[`EMPTY`] id into it. `id_at` is the only way to read a
/// label — there's no dense-array escape hatch — so every consumer
/// pays a cost proportional to the region it actually visits, not to
/// `res`.
pub struct ComponentField {
    ids: HashMap<ChunkCoord, IdTile>,
    res: UVec3,
    /// `voxel_count[i]` is the number of voxels with `ComponentId == i`.
    /// `voxel_count[0]` counts empty voxels.
    voxel_count: Vec<u32>,
    /// Inclusive-exclusive AABB per component in voxel coordinates.
    /// `bounds[i] = (min, max)` where `min.x..max.x` covers every
    /// voxel with `ComponentId == i` (and analogously for y, z).
    /// `bounds[0]` is unused (empty voxels have no meaningful box).
    bounds: Vec<(UVec3, UVec3)>,
}

impl ComponentField {
    /// Grid resolution this field was labelled for. Callers should
    /// re-label when the grid changes size (never, in Stage 1) or
    /// after any stroke (in the app layer).
    pub fn res(&self) -> UVec3 {
        self.res
    }

    /// Raw component id at a voxel (`0` = empty). The only accessor
    /// for label data — callers that need to visit many voxels
    /// should bound themselves with `bounds_of` first (as
    /// `translate_components` and the app's delete-selection path
    /// both do) rather than raster-scanning `res`.
    #[inline]
    pub fn id_at(&self, x: u32, y: u32, z: u32) -> ComponentId {
        match self.ids.get(&tile_key(x, y, z)) {
            Some(tile) => tile[local_index(x, y, z)],
            None => EMPTY,
        }
    }

    #[inline]
    fn set_id(&mut self, x: u32, y: u32, z: u32, id: ComponentId) {
        let tile = self
            .ids
            .entry(tile_key(x, y, z))
            .or_insert_with(|| Box::new([EMPTY; TILE_LEN]));
        tile[local_index(x, y, z)] = id;
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

    /// Voxel-coordinate AABB of a component: `(min, max)` where
    /// `min.x..max.x` is the half-open x range spanned by any voxel
    /// with `ComponentId == id`. Returns `None` for empty ids or
    /// out-of-range component ids. Cheap: filled during labelling.
    pub fn bounds_of(&self, id: ComponentId) -> Option<(UVec3, UVec3)> {
        if id == EMPTY {
            return None;
        }
        let (min, max) = *self.bounds.get(id as usize)?;
        if max.x <= min.x || max.y <= min.y || max.z <= min.z {
            return None;
        }
        Some((min, max))
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
/// Both the label storage and the seed-scan are sparse (`PLAN.md`
/// Track A3), mirroring `Grid`'s own tiles: a voxel in an
/// unallocated `Grid` tile is guaranteed `φ >= 0` (empty), so it can
/// never seed or extend a component, doesn't need visiting during
/// the scan, and never needs a label tile allocated for it. The
/// flood-fill itself is unchanged in spirit — neighbour lookups
/// still call `grid.get` / `field.id_at`, which already return the
/// correct "empty" answer for unallocated regions, so a flood
/// correctly stops at a tile boundary with no special-casing.
///
/// Uses a single reusable `VecDeque` queue and a bit-per-voxel
/// visited mask, so allocation is bounded even on a fully-occupied
/// grid.
pub fn label_components(grid: &Grid) -> ComponentField {
    let res = grid.res();
    let n = (res.x as usize) * (res.y as usize) * (res.z as usize);

    let mut field = ComponentField {
        ids: HashMap::new(),
        res,
        // voxel_count[0] = empty voxel count.
        voxel_count: vec![0],
        // bounds[0] is a placeholder; the fill fields it in for id >= 1.
        bounds: vec![(UVec3::ZERO, UVec3::ZERO)],
    };
    let mut next_id: ComponentId = 1;
    let mut queue: VecDeque<(u32, u32, u32)> = VecDeque::new();
    let mut visited_voxels: usize = 0;

    for coord in grid.allocated_chunk_coords() {
        let base = coord.voxel_min();
        let tile_max = coord.voxel_max(res);
        for iz in base.z..tile_max.z {
            for iy in base.y..tile_max.y {
                for ix in base.x..tile_max.x {
                    visited_voxels += 1;
                    if grid.get(ix, iy, iz) >= 0.0 {
                        field.voxel_count[0] += 1;
                        continue;
                    }
                    if field.id_at(ix, iy, iz) != EMPTY {
                        continue;
                    }

                    let comp_id = next_id;
                    next_id += 1;
                    field.voxel_count.push(0);
                    // Seed the AABB with the first voxel. min = start,
                    // max = start + 1 (half-open). Grows as the flood
                    // finds more voxels.
                    let mut c_min = UVec3::new(ix, iy, iz);
                    let mut c_max = UVec3::new(ix + 1, iy + 1, iz + 1);

                    queue.clear();
                    queue.push_back((ix, iy, iz));
                    field.set_id(ix, iy, iz, comp_id);

                    while let Some((x, y, z)) = queue.pop_front() {
                        field.voxel_count[comp_id as usize] += 1;
                        c_min.x = c_min.x.min(x);
                        c_min.y = c_min.y.min(y);
                        c_min.z = c_min.z.min(z);
                        c_max.x = c_max.x.max(x + 1);
                        c_max.y = c_max.y.max(y + 1);
                        c_max.z = c_max.z.max(z + 1);

                        // 6-connected neighbours. Inline the loop for
                        // faster iteration.
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
                            if field.id_at(nx, ny, nz) != EMPTY {
                                continue;
                            }
                            if grid.get(nx, ny, nz) >= 0.0 {
                                continue;
                            }
                            field.set_id(nx, ny, nz, comp_id);
                            queue.push_back((nx, ny, nz));
                        }
                    }

                    field.bounds.push((c_min, c_max));
                }
            }
        }
    }

    // Every voxel we never visited belongs to an unallocated tile,
    // which is guaranteed empty (`φ >= 0`) by the sparse `Grid`
    // contract — count them in bulk instead of visiting each one.
    field.voxel_count[0] += (n - visited_voxels) as u32;

    field
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
    fn bounds_cover_component_voxels() {
        // Add one sphere and check its AABB spans exactly the voxels
        // that belong to it in the labelled field. min is inclusive,
        // max is exclusive.
        let mut g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        let s = SphereBrush {
            center: Vec3::new(16.0, 16.0, 16.0),
            radius: 4.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(0.0, 0.0, -1.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &s);
        let f = label_components(&g);
        assert_eq!(f.component_count(), 1);
        let id = f.ids_by_size_desc()[0];
        let (mn, mx) = f.bounds_of(id).expect("component has bounds");
        // Every voxel labelled with `id` must lie inside [mn, mx),
        // and no voxel outside that AABB may carry `id`.
        for iz in 0..32u32 {
            for iy in 0..32u32 {
                for ix in 0..32u32 {
                    if f.id_at(ix, iy, iz) == id {
                        assert!(ix >= mn.x && ix < mx.x, "x out of bounds");
                        assert!(iy >= mn.y && iy < mx.y, "y out of bounds");
                        assert!(iz >= mn.z && iz < mx.z, "z out of bounds");
                    }
                }
            }
        }
        assert!(mx.x > mn.x && mx.y > mn.y && mx.z > mn.z);
    }

    #[test]
    fn bounds_of_empty_or_unknown_id_is_none() {
        let g = sphere_grid(5.0, Vec3::new(16.0, 16.0, 16.0));
        let f = label_components(&g);
        assert!(f.bounds_of(EMPTY).is_none());
        assert!(f.bounds_of(9999).is_none());
    }

    /// Regression for the Track A3 sparsification: the bulk
    /// "everything unvisited is empty" accounting must agree with a
    /// full per-voxel count, and must correctly include every voxel
    /// belonging to tiles that never got allocated at all (not just
    /// empty voxels *within* allocated tiles).
    #[test]
    fn empty_voxel_count_accounts_for_every_voxel_including_unallocated_tiles() {
        let res = UVec3::new(96, 96, 96);
        let mut g = Grid::empty(res, 1.0, Vec3::ZERO);
        let add = SphereBrush {
            center: Vec3::new(16.0, 16.0, 16.0),
            radius: 6.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(0.0, 0.0, -1.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &add);
        // Most of this 96³ grid is untouched — well under the full
        // 3³ = 27 tiles a 96³/32 grid could allocate.
        assert!(g.allocated_tile_count() < 27);

        let f = label_components(&g);
        let total = (res.x as u64) * (res.y as u64) * (res.z as u64);
        let solid: u64 = f
            .ids_by_size_desc()
            .iter()
            .map(|&id| f.voxel_count(id) as u64)
            .sum();
        assert_eq!(
            f.voxel_count(EMPTY) as u64 + solid,
            total,
            "empty + solid voxel counts must cover every voxel in the domain"
        );
    }

    /// Regression for the Track A3 label-storage sparsification: a
    /// small component on a large grid must only allocate a handful
    /// of label tiles, not one dense buffer covering the whole
    /// domain.
    #[test]
    fn label_field_stays_sparse_for_a_small_component_on_a_large_grid() {
        let mut g = Grid::empty(UVec3::new(192, 192, 192), 1.0, Vec3::ZERO);
        let add = SphereBrush {
            center: Vec3::new(16.0, 16.0, 16.0),
            radius: 5.0,
            mode: BrushMode::Pull,
            direction: Vec3::new(0.0, 0.0, -1.0),
            displace: false,
            workbench_y: None,
        };
        let _ = apply_sphere_brush(&mut g, &add);
        let f = label_components(&g);
        assert_eq!(f.component_count(), 1);
        // A 5 mm sphere spans well under one 32-voxel tile; a couple
        // of label tiles (interior + any neighbouring band spillover)
        // is the right ballpark, nowhere near the 6³ = 216 a dense
        // `res³` scan would have touched.
        assert!(f.ids.len() <= 4, "label field allocated {} tiles, expected only a few", f.ids.len());
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
