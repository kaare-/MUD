//! Pen / stylus pressure and tilt tracking.
//!
//! Bevy 0.15 exposes force (and optional altitude) through
//! [`TouchInput`] (iOS Pencil, Windows stylus / touch). Mouse and
//! tablets that appear as plain mouse buttons report no force —
//! those strokes use pressure `1.0` and upright tilt.
//!
//! - **Pressure → depth** on Clay / Smooth (`depth_scale`) and a
//!   softer **engagement** curve on Press / Pull / Paddle / Knife.
//! - **Altitude → lean** on Knife / Paddle: flat stylus leans the
//!   tool axis toward the view tangent; upright = surface normal.

use bevy::input::touch::{ForceTouch, TouchInput, TouchPhase};
use bevy::prelude::*;
use glam::Vec3 as GVec3;

/// Latest stylus / touch sample.
#[derive(Resource, Debug, Clone)]
pub struct PenState {
    /// Normalized pressure in `0..=1`. Mouse / unknown devices stay at
    /// `1.0` so existing mouse workflows are unchanged.
    pub pressure: f32,
    /// Stylus altitude in radians when known: `0` = flat on the
    /// surface, `π/2` = perpendicular. `None` → treat as upright.
    pub altitude_rad: Option<f32>,
    /// True when the current sample came from a force-capable contact.
    pub from_stylus: bool,
}

impl Default for PenState {
    fn default() -> Self {
        Self {
            pressure: 1.0,
            altitude_rad: None,
            from_stylus: false,
        }
    }
}

impl PenState {
    /// Depth / strength multiplier for Clay bite and Smooth blend.
    /// When `use_pressure` is off, always `1.0`.
    pub fn depth_scale(&self, use_pressure: bool) -> f32 {
        if !use_pressure {
            return 1.0;
        }
        // Floor so a feather-light hover still leaves a faint mark
        // rather than a mysterious no-op.
        self.pressure.clamp(0.05, 1.0)
    }

    /// Engagement curve for Press / Pull / Paddle / Knife.
    ///
    /// Softer toe-in than raw pressure so light presses still read,
    /// while hard presses still reach full depth.
    pub fn engagement_scale(&self, use_pressure: bool) -> f32 {
        if !use_pressure {
            return 1.0;
        }
        let p = self.pressure.clamp(0.0, 1.0);
        (0.12 + 0.88 * p.powf(0.75)).clamp(0.12, 1.0)
    }

    /// How hard the stylus is leaning, `0` = upright, `1` = flat.
    pub fn lean_amount(&self) -> f32 {
        match self.altitude_rad {
            Some(a) => {
                let upright = std::f32::consts::FRAC_PI_2;
                (1.0 - (a.clamp(0.0, upright) / upright)).clamp(0.0, 1.0)
            }
            None => 0.0,
        }
    }

    /// Tool axis for directional tools: blends `into_surface` toward a
    /// view-aligned lean on the tangent plane as the stylus flattens.
    ///
    /// Without azimuth from the OS we lean toward the camera's
    /// approach projected onto the surface — readable with mouse too
    /// when altitude is simulated, and natural for Pencil altitude.
    pub fn leaned_into(&self, into_surface: GVec3, view_dir: GVec3) -> GVec3 {
        let lean = self.lean_amount();
        if lean < 1e-4 {
            return into_surface.normalize_or_zero();
        }
        let into = into_surface.normalize_or_zero();
        if into.length_squared() < 1e-8 {
            return GVec3::NEG_Y;
        }
        // Project view onto the tangent plane (⊥ into). Prefer the
        // direction the user is looking "across" the surface.
        let mut tangent = view_dir - into * view_dir.dot(into);
        if tangent.length_squared() < 1e-8 {
            // Degenerate (looking straight into the surface): pick a
            // stable tangent from a world-up fallback.
            let up = GVec3::Y;
            tangent = up - into * up.dot(into);
            if tangent.length_squared() < 1e-8 {
                tangent = GVec3::X - into * into.x;
            }
        }
        let tangent = tangent.normalize_or_zero();
        // Max lean ~35° off the surface normal — enough to read, not
        // enough to skim parallel and miss the clay.
        let max_blend = 0.55;
        (into + tangent * (lean * max_blend)).normalize_or_zero()
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<PenState>();
    app.add_systems(PreUpdate, update_pen_state);
}

fn update_pen_state(mut pen: ResMut<PenState>, mut touches: EventReader<TouchInput>) {
    for ev in touches.read() {
        match ev.phase {
            TouchPhase::Started | TouchPhase::Moved => {
                if let Some(force) = ev.force {
                    let (pressure, altitude) = force_to_pen(force);
                    pen.pressure = pressure;
                    pen.altitude_rad = altitude;
                    pen.from_stylus = true;
                } else if !pen.from_stylus {
                    // Touch without force (many Android / Linux paths):
                    // treat as full press so the stroke still works.
                    pen.pressure = 1.0;
                    pen.altitude_rad = None;
                }
            }
            TouchPhase::Ended | TouchPhase::Canceled => {
                pen.pressure = 1.0;
                pen.altitude_rad = None;
                pen.from_stylus = false;
            }
        }
    }
}

fn force_to_pen(force: ForceTouch) -> (f32, Option<f32>) {
    match force {
        ForceTouch::Calibrated {
            force,
            max_possible_force,
            altitude_angle,
        } => {
            let pressure = if max_possible_force > 1e-6 {
                ((force / max_possible_force) as f32).clamp(0.0, 1.0)
            } else {
                (force as f32).clamp(0.0, 1.0)
            };
            let altitude = altitude_angle.map(|a| a as f32);
            (pressure, altitude)
        }
        ForceTouch::Normalized(n) => ((n as f32).clamp(0.0, 1.0), None),
    }
}

fn force_to_pressure(force: ForceTouch) -> f32 {
    force_to_pen(force).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_default_is_full_depth() {
        let pen = PenState::default();
        assert!((pen.depth_scale(true) - 1.0).abs() < 1e-5);
        assert!((pen.engagement_scale(true) - 1.0).abs() < 1e-5);
        assert!(pen.lean_amount() < 1e-5);
    }

    #[test]
    fn light_pressure_scales_depth() {
        let pen = PenState {
            pressure: 0.25,
            altitude_rad: None,
            from_stylus: true,
        };
        assert!((pen.depth_scale(true) - 0.25).abs() < 1e-5);
        assert!((pen.depth_scale(false) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn engagement_is_softer_than_raw_at_light_press() {
        let pen = PenState {
            pressure: 0.25,
            altitude_rad: None,
            from_stylus: true,
        };
        let e = pen.engagement_scale(true);
        assert!(e > 0.25, "engagement toe-in should exceed raw light press");
        assert!(e < 0.55, "but still well below full: e={e}");
    }

    #[test]
    fn flat_altitude_leans_off_normal() {
        let pen = PenState {
            pressure: 1.0,
            altitude_rad: Some(0.0), // flat on the surface
            from_stylus: true,
        };
        assert!((pen.lean_amount() - 1.0).abs() < 1e-4);
        let into = GVec3::NEG_X;
        let view = GVec3::new(0.0, 0.0, -1.0);
        let leaned = pen.leaned_into(into, view);
        assert!(
            leaned.dot(into) < 0.98,
            "flat stylus should lean the axis off the surface normal"
        );
        assert!(
            leaned.z.abs() > 0.1,
            "lean should pick up the view-tangent component"
        );
    }

    #[test]
    fn upright_altitude_keeps_into() {
        let pen = PenState {
            pressure: 1.0,
            altitude_rad: Some(std::f32::consts::FRAC_PI_2),
            from_stylus: true,
        };
        let into = GVec3::NEG_Y;
        let leaned = pen.leaned_into(into, GVec3::NEG_Z);
        assert!((leaned - into).length() < 1e-4);
    }

    #[test]
    fn normalized_force_maps_to_unit_interval() {
        assert!((force_to_pressure(ForceTouch::Normalized(0.4)) - 0.4).abs() < 1e-5);
        assert!((force_to_pressure(ForceTouch::Normalized(1.5)) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn calibrated_force_uses_max_and_keeps_altitude() {
        let f = ForceTouch::Calibrated {
            force: 2.0,
            max_possible_force: 4.0,
            altitude_angle: Some(0.5),
        };
        let (p, a) = force_to_pen(f);
        assert!((p - 0.5).abs() < 1e-5);
        assert!((a.unwrap() - 0.5).abs() < 1e-5);
    }
}
