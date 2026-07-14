//! Insert a primitive SDF (sphere / box / cylinder / torus) into the
//! workpiece via SDF union.
//!
//! Sits alongside the sphere brush: same iterate-over-AABB pattern,
//! same pre-mutation callback so undo can journal every voxel that
//! actually changes. The one difference is CSG polarity — a primitive
//! is a pure union (`new = min(old, prim_sdf)`), never a subtract.
//!
//! Shapes are analytic SDFs with their local origin at the shape's
//! centre. The `Primitive` wrapper translates them to `center` in
//! piece-local mm and (optionally) clips to a workbench floor so an
//! insertion below the bench can't leak material through the floor.

use glam::{UVec3, Vec3};

use crate::grid::{DirtyRegion, Grid};

/// Analytic primitive shape, centred at the origin. Every field is in
/// piece-local mm.
#[derive(Copy, Clone, Debug)]
pub enum PrimitiveKind {
    Sphere {
        radius: f32,
    },
    Box {
        half_extents: Vec3,
    },
    /// Cylinder aligned to the Y axis. `half_height` is measured along
    /// Y; caps live at ±half_height.
    Cylinder {
        radius: f32,
        half_height: f32,
    },
    /// Torus lying in the XZ plane. `major` is the distance from the
    /// piece centre to the tube centre; `minor` is the tube radius.
    Torus {
        major: f32,
        minor: f32,
    },
}

impl PrimitiveKind {
    /// Signed distance from a point to this primitive's surface,
    /// measured in the shape's local frame (centre at origin).
    pub fn sdf(&self, p: Vec3) -> f32 {
        match *self {
            PrimitiveKind::Sphere { radius } => p.length() - radius,
            PrimitiveKind::Box { half_extents } => {
                let q = p.abs() - half_extents;
                let outside = Vec3::new(q.x.max(0.0), q.y.max(0.0), q.z.max(0.0)).length();
                let inside = q.x.max(q.y.max(q.z)).min(0.0);
                outside + inside
            }
            PrimitiveKind::Cylinder {
                radius,
                half_height,
            } => {
                let d_xz = (p.x * p.x + p.z * p.z).sqrt() - radius;
                let d_y = p.y.abs() - half_height;
                let outside =
                    Vec3::new(d_xz.max(0.0), d_y.max(0.0), 0.0).length();
                let inside = d_xz.max(d_y).min(0.0);
                outside + inside
            }
            PrimitiveKind::Torus { major, minor } => {
                let q_x = (p.x * p.x + p.z * p.z).sqrt() - major;
                (q_x * q_x + p.y * p.y).sqrt() - minor
            }
        }
    }

    /// Piece-local half-extents that contain the primitive (before
    /// translation to `Primitive::center`). Used to size the AABB the
    /// grid iterator visits.
    pub fn half_extents(&self) -> Vec3 {
        match *self {
            PrimitiveKind::Sphere { radius } => Vec3::splat(radius),
            PrimitiveKind::Box { half_extents } => half_extents,
            PrimitiveKind::Cylinder {
                radius,
                half_height,
            } => Vec3::new(radius, half_height, radius),
            PrimitiveKind::Torus { major, minor } => {
                Vec3::new(major + minor, minor, major + minor)
            }
        }
    }
}

/// A primitive placed in piece-local space, ready to union into the
/// grid.
#[derive(Copy, Clone, Debug)]
pub struct Primitive {
    pub kind: PrimitiveKind,
    pub center: Vec3,
    /// Same semantics as `SphereBrush::workbench_y`: no voxel below
    /// this Y coordinate is allowed to become inside the workpiece.
    pub workbench_y: Option<f32>,
}

/// Union `primitive` into `grid`. Convenience wrapper around
/// [`apply_primitive_with_callback`] that discards pre-mutation
/// values.
pub fn apply_primitive(grid: &mut Grid, primitive: &Primitive) -> Option<DirtyRegion> {
    apply_primitive_with_callback(grid, primitive, |_, _, _, _| {})
}

/// Union a primitive into the grid; fire a callback for every voxel
/// that is about to change so the undo journal can record it. Voxels
/// where the primitive's SDF is larger than the workpiece's are left
/// untouched — the caller pays for iteration inside a padded AABB but
/// not for spurious writes.
pub fn apply_primitive_with_callback<F>(
    grid: &mut Grid,
    primitive: &Primitive,
    mut on_pre_mutation: F,
) -> Option<DirtyRegion>
where
    F: FnMut(u32, u32, u32, f32),
{
    let vs = grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = grid.res();
    let half = primitive.kind.half_extents() + Vec3::splat(vs);
    let min_p = (primitive.center - half - grid.origin()) * inv_vs;
    let max_p = (primitive.center + half - grid.origin()) * inv_vs;

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
        return None;
    }

    for iz in min.z..max.z {
        for iy in min.y..max.y {
            for ix in min.x..max.x {
                let p = grid.position(ix, iy, iz);
                let local = p - primitive.center;
                let d_prim = primitive.kind.sdf(local);
                let old = grid.get(ix, iy, iz);
                let mut new = old.min(d_prim);
                if let Some(wb_y) = primitive.workbench_y {
                    let below = wb_y - p.y;
                    if below > new {
                        new = below;
                    }
                }
                if new != old {
                    on_pre_mutation(ix, iy, iz, old);
                    grid.set(ix, iy, iz, new);
                }
            }
        }
    }

    Some(DirtyRegion { min, max })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_grid() -> Grid {
        Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::ZERO)
    }

    #[test]
    fn sphere_inserted_into_empty_grid_leaves_interior_negative() {
        let mut g = empty_grid();
        let prim = Primitive {
            kind: PrimitiveKind::Sphere { radius: 10.0 },
            center: Vec3::new(32.0, 32.0, 32.0),
            workbench_y: None,
        };
        let region = apply_primitive(&mut g, &prim);
        assert!(region.is_some());
        assert!(g.sample(Vec3::new(32.0, 32.0, 32.0)) < 0.0);
        // Far outside the primitive: field must remain positive.
        assert!(g.sample(Vec3::new(60.0, 32.0, 32.0)) > 0.0);
    }

    #[test]
    fn primitive_union_preserves_pre_existing_material() {
        // Existing near sphere plus an inserted far sphere: both
        // interiors must read as inside after the union.
        let mut g = Grid::from_sphere(
            UVec3::new(64, 64, 64),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 32.0, 32.0),
            6.0,
        );
        assert!(g.sample(Vec3::new(16.0, 32.0, 32.0)) < 0.0);
        let prim = Primitive {
            kind: PrimitiveKind::Sphere { radius: 6.0 },
            center: Vec3::new(48.0, 32.0, 32.0),
            workbench_y: None,
        };
        let _ = apply_primitive(&mut g, &prim);
        assert!(g.sample(Vec3::new(16.0, 32.0, 32.0)) < 0.0);
        assert!(g.sample(Vec3::new(48.0, 32.0, 32.0)) < 0.0);
    }

    #[test]
    fn box_edges_and_corners_are_solid() {
        let mut g = empty_grid();
        let prim = Primitive {
            kind: PrimitiveKind::Box {
                half_extents: Vec3::new(8.0, 8.0, 8.0),
            },
            center: Vec3::new(32.0, 32.0, 32.0),
            workbench_y: None,
        };
        let _ = apply_primitive(&mut g, &prim);
        // Interior negative.
        assert!(g.sample(Vec3::new(32.0, 32.0, 32.0)) < -7.0);
        // Just inside a face.
        assert!(g.sample(Vec3::new(39.5, 32.0, 32.0)) < 0.0);
        // Just outside a corner.
        assert!(g.sample(Vec3::new(41.0, 41.0, 41.0)) > 0.0);
    }

    #[test]
    fn cylinder_is_axial_along_y() {
        let mut g = empty_grid();
        let prim = Primitive {
            kind: PrimitiveKind::Cylinder {
                radius: 6.0,
                half_height: 12.0,
            },
            center: Vec3::new(32.0, 32.0, 32.0),
            workbench_y: None,
        };
        let _ = apply_primitive(&mut g, &prim);
        // Along the axis, inside the height range: solid.
        assert!(g.sample(Vec3::new(32.0, 40.0, 32.0)) < 0.0);
        // Same radial distance, outside height: empty.
        assert!(g.sample(Vec3::new(32.0, 50.0, 32.0)) > 0.0);
        // Off-axis inside radius, inside height: solid.
        assert!(g.sample(Vec3::new(36.0, 32.0, 32.0)) < 0.0);
        // Beyond the radius: empty.
        assert!(g.sample(Vec3::new(42.0, 32.0, 32.0)) > 0.0);
    }

    #[test]
    fn torus_hole_stays_empty() {
        let mut g = empty_grid();
        let prim = Primitive {
            kind: PrimitiveKind::Torus {
                major: 10.0,
                minor: 3.0,
            },
            center: Vec3::new(32.0, 32.0, 32.0),
            workbench_y: None,
        };
        let _ = apply_primitive(&mut g, &prim);
        // The torus tube passes through +X = 32+10 = 42 area.
        assert!(g.sample(Vec3::new(42.0, 32.0, 32.0)) < 0.0);
        // Centre of the hole: empty air.
        assert!(g.sample(Vec3::new(32.0, 32.0, 32.0)) > 0.0);
    }

    #[test]
    fn workbench_clip_prevents_material_below_the_plane() {
        let mut g = empty_grid();
        // Sphere half above, half below the workbench at y=32.
        let prim = Primitive {
            kind: PrimitiveKind::Sphere { radius: 6.0 },
            center: Vec3::new(32.0, 32.0, 32.0),
            workbench_y: Some(32.0),
        };
        let _ = apply_primitive(&mut g, &prim);
        assert!(g.sample(Vec3::new(32.0, 28.0, 32.0)) > 0.0);
        assert!(g.sample(Vec3::new(32.0, 36.0, 32.0)) < 0.0);
    }
}
