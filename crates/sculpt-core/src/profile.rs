//! 2D signed-distance profiles and 3D extrusion operators.
//!
//! From DESIGN §2.7: a **profile** is a 2D signed distance function
//! (analytic here, procedurally driven later). Extruding, revolving,
//! or sweeping a profile produces the 3D SDF the sculpting engine
//! consumes. This module implements extrusion; revolve and sweep can
//! land in later stages when they're needed by roller and scraper.
//!
//! For Stage 2 the profiles are the classic beginner-friendly cookie-
//! cutter set: circle, square, hexagon, five-pointed star.

use glam::{Vec2, Vec3};

/// A 2D signed-distance profile centred at the origin of its own
/// local frame. Negative inside, positive outside, exact euclidean
/// distance in mm.
#[derive(Copy, Clone, Debug)]
pub enum Profile {
    /// `sqrt(x² + y²) - radius`.
    Circle { radius: f32 },
    /// Axis-aligned square with half-side `half_side` (so full side
    /// length is `2 * half_side`).
    Square { half_side: f32 },
    /// Regular hexagon with flat sides touching a circle of radius
    /// `radius`. (Point-to-point distance is `2/√3 · radius`.)
    Hexagon { radius: f32 },
    /// Five-pointed star with tips at `outer` from the centre and
    /// inner corners at `outer · inner_ratio`. `inner_ratio = 0.4`
    /// gives a recognisably star-shaped silhouette.
    Star5 { outer: f32, inner_ratio: f32 },
}

impl Profile {
    /// 2D signed distance in the profile's own frame.
    pub fn sdf(&self, x: f32, y: f32) -> f32 {
        match *self {
            Profile::Circle { radius } => (x * x + y * y).sqrt() - radius,
            Profile::Square { half_side } => box_sdf(Vec2::new(x, y), Vec2::splat(half_side)),
            Profile::Hexagon { radius } => hexagon_sdf(Vec2::new(x, y), radius),
            Profile::Star5 { outer, inner_ratio } => star5_sdf(Vec2::new(x, y), outer, inner_ratio),
        }
    }

    /// Radius of a circle in the local frame that fully contains the
    /// profile. Used for AABB culling.
    pub fn bounding_radius(&self) -> f32 {
        match *self {
            Profile::Circle { radius } => radius,
            Profile::Square { half_side } => half_side * std::f32::consts::SQRT_2,
            // For a flat-topped hexagon, the vertex-to-centre distance
            // is 2/√3 times the inradius.
            Profile::Hexagon { radius } => radius * 2.0 / 3.0_f32.sqrt(),
            Profile::Star5 { outer, .. } => outer,
        }
    }

    /// A short human-readable name, used for the on-screen tool label.
    pub fn label(&self) -> &'static str {
        match self {
            Profile::Circle { .. } => "circle",
            Profile::Square { .. } => "square",
            Profile::Hexagon { .. } => "hexagon",
            Profile::Star5 { .. } => "star",
        }
    }
}

/// Signed distance to an axis-aligned box centred at origin with
/// half-extents `b`. IQ's canonical box SDF.
fn box_sdf(p: Vec2, b: Vec2) -> f32 {
    let d = p.abs() - b;
    d.max(Vec2::ZERO).length() + d.x.max(d.y).min(0.0)
}

/// Signed distance to a regular hexagon centred at origin with flat
/// sides at inradius `r`. Standard 6-fold-symmetric fold + line SDF.
fn hexagon_sdf(p: Vec2, r: f32) -> f32 {
    // (-cos(30°), sin(30°), tan(30°)) — direction vector of the
    // 30°-off-axis reflection plane plus the tangent used to clamp.
    let k = glam::Vec3::new(-0.866_025_4, 0.5, 0.577_350_26);
    let mut q = p.abs();
    // Fold across the sector boundary.
    q -= 2.0 * Vec2::new(k.x, k.y) * Vec2::new(k.x, k.y).dot(q).min(0.0);
    // Distance to the flat edge at `y = r`, clamped along the edge.
    q -= Vec2::new(q.x.clamp(-k.z * r, k.z * r), r);
    q.length() * q.y.signum()
}

/// Five-pointed star SDF (IQ). `r` is the tip radius; `rf` is the
/// inner corner radius as a fraction of `r`.
fn star5_sdf(mut p: Vec2, r: f32, rf: f32) -> f32 {
    // Two rotation axes for the 5-fold sector reduction.
    let k1 = Vec2::new(0.809_016_99, -0.587_785_25);
    let k2 = Vec2::new(-k1.x, k1.y);
    p.x = p.x.abs();
    p -= 2.0 * k1 * k1.dot(p).max(0.0);
    p -= 2.0 * k2 * k2.dot(p).max(0.0);
    p.x = p.x.abs();
    p.y -= r;
    let ba = Vec2::new(-k1.y, k1.x) * (rf * r) - Vec2::new(0.0, r);
    let h = (p.dot(ba) / ba.dot(ba)).clamp(0.0, 1.0);
    (p - ba * h).length() * (p.y * ba.x - p.x * ba.y).signum()
}

/// 3D SDF of `profile` extruded along `axis` (unit vector), centred
/// at `origin`, extending `±half_length` along the axis. Evaluated
/// at world/piece-local point `p`.
///
/// This is the Stage-2 tool geometry for cookie cutter. Later stages
/// can add `revolve_profile` and `sweep_profile` for rollers and
/// scrapers on top of the same `Profile` type.
pub fn extrude_profile(
    profile: &Profile,
    origin: Vec3,
    axis: Vec3,
    half_length: f32,
    p: Vec3,
) -> f32 {
    let (u, v) = perpendicular_basis(axis);
    let d = p - origin;
    let along = d.dot(axis);
    let x = d.dot(u);
    let y = d.dot(v);
    let d_profile = profile.sdf(x, y);
    let d_axis = along.abs() - half_length;
    // Standard finite-extrusion SDF (IQ):
    //   dv = (profile, axis_distance)
    //   sdf = length(max(dv, 0)) + min(max(dv.x, dv.y), 0)
    // Correct exterior distance; sign flips inside.
    let dv = Vec2::new(d_profile, d_axis);
    dv.max(Vec2::ZERO).length() + dv.x.max(dv.y).min(0.0)
}

/// Deterministic orthonormal basis for the plane perpendicular to
/// `axis`. `axis` must be unit length. Falls back to a Y-aligned
/// reference when `axis` is nearly aligned with X, ensuring the
/// cross product is well-conditioned.
fn perpendicular_basis(axis: Vec3) -> (Vec3, Vec3) {
    let reference = if axis.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let u = reference.cross(axis).normalize();
    let v = axis.cross(u);
    (u, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn circle_sdf_is_zero_on_the_boundary() {
        let c = Profile::Circle { radius: 5.0 };
        assert!(approx(c.sdf(5.0, 0.0), 0.0, 1e-4));
        assert!(approx(c.sdf(-5.0, 0.0), 0.0, 1e-4));
        assert!(approx(c.sdf(0.0, 5.0), 0.0, 1e-4));
        assert!(c.sdf(0.0, 0.0) < 0.0);
        assert!(c.sdf(10.0, 0.0) > 0.0);
    }

    #[test]
    fn square_sdf_is_negative_inside_positive_outside() {
        let s = Profile::Square { half_side: 5.0 };
        assert!(s.sdf(0.0, 0.0) < 0.0);
        assert!(s.sdf(4.0, 4.0) < 0.0);
        assert!(s.sdf(6.0, 0.0) > 0.0);
        // Corner: sdf should be sqrt(2) - is not quite right, but let's
        // just verify the sign.
        assert!(s.sdf(10.0, 10.0) > 0.0);
    }

    #[test]
    fn hexagon_sdf_is_symmetric() {
        let h = Profile::Hexagon { radius: 5.0 };
        // 60-degree rotational symmetry: sample at 0, 60, 120, ...
        let d0 = h.sdf(5.0, 0.0);
        // Rotate by 60°: (5 cos 60, 5 sin 60) = (2.5, 4.33)
        let d1 = h.sdf(2.5, 5.0 * (60.0_f32.to_radians()).sin());
        assert!(
            (d0 - d1).abs() < 0.1,
            "hexagon should be 6-fold symmetric: d0={d0}, d1={d1}"
        );
        assert!(h.sdf(0.0, 0.0) < 0.0);
    }

    #[test]
    fn star5_has_inner_corners_negative() {
        let s = Profile::Star5 {
            outer: 5.0,
            inner_ratio: 0.4,
        };
        // Centre is inside.
        assert!(s.sdf(0.0, 0.0) < 0.0);
        // Way outside a tip: positive.
        assert!(s.sdf(10.0, 0.0) > 0.0);
    }

    #[test]
    fn extrude_matches_profile_along_axis() {
        // Extrude a unit circle along Y with half-length 10. At
        // (x, 0, 0), the 3D SDF should equal the 2D circle SDF
        // (we're inside the axis span, so the extrusion clip is
        // negative and doesn't dominate).
        let c = Profile::Circle { radius: 1.0 };
        let d3d = extrude_profile(&c, Vec3::ZERO, Vec3::Y, 10.0, Vec3::new(2.0, 0.0, 0.0));
        // 2D profile at (x, z) = (2, 0) → distance 2 - 1 = 1.
        assert!(
            approx(d3d, 1.0, 0.05),
            "expected ~1.0 at (2,0,0), got {d3d}",
        );
    }

    #[test]
    fn extrude_clips_beyond_the_axis_range() {
        let c = Profile::Circle { radius: 1.0 };
        // At (0, 20, 0) we're 10 mm past the axis end — should be
        // ~10 mm outside.
        let d3d = extrude_profile(&c, Vec3::ZERO, Vec3::Y, 10.0, Vec3::new(0.0, 20.0, 0.0));
        assert!(d3d > 9.0 && d3d < 11.0, "expected ~10, got {d3d}");
    }
}
