//! Pen / stylus pressure tracking.
//!
//! Stage-3 target from PLAN: **pressure → depth**. Bevy 0.15 exposes
//! force only through [`TouchInput`] (iOS Pencil, Windows stylus / touch).
//! Mouse and tablets that appear as plain mouse buttons report no force —
//! those strokes use pressure `1.0` (full depth).
//!
//! Tilt → orientation for directional tools is deferred until a tool
//! needs it (scraper / knife).

use bevy::input::touch::{ForceTouch, TouchInput, TouchPhase};
use bevy::prelude::*;

/// Latest stylus / touch pressure sample.
#[derive(Resource, Debug, Clone)]
pub struct PenState {
    /// Normalized pressure in `0..=1`. Mouse / unknown devices stay at
    /// `1.0` so existing mouse workflows are unchanged.
    pub pressure: f32,
    /// True when the current sample came from a force-capable contact.
    pub from_stylus: bool,
}

impl Default for PenState {
    fn default() -> Self {
        Self {
            pressure: 1.0,
            from_stylus: false,
        }
    }
}

impl PenState {
    /// Depth / strength multiplier for clay bite, paddle advance, and
    /// smooth blend. When `use_pressure` is off, always `1.0`.
    pub fn depth_scale(&self, use_pressure: bool) -> f32 {
        if !use_pressure {
            return 1.0;
        }
        // Floor so a feather-light hover still leaves a faint mark
        // rather than a mysterious no-op.
        self.pressure.clamp(0.05, 1.0)
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
                    pen.pressure = force_to_pressure(force);
                    pen.from_stylus = true;
                } else if !pen.from_stylus {
                    // Touch without force (many Android / Linux paths):
                    // treat as full press so the stroke still works.
                    pen.pressure = 1.0;
                }
            }
            TouchPhase::Ended | TouchPhase::Canceled => {
                pen.pressure = 1.0;
                pen.from_stylus = false;
            }
        }
    }
}

fn force_to_pressure(force: ForceTouch) -> f32 {
    match force {
        ForceTouch::Calibrated {
            force,
            max_possible_force,
            ..
        } => {
            if max_possible_force > 1e-6 {
                ((force / max_possible_force) as f32).clamp(0.0, 1.0)
            } else {
                // Apple: ~1.0 ≈ average touch; clamp hard presses.
                (force as f32).clamp(0.0, 1.0)
            }
        }
        ForceTouch::Normalized(n) => (n as f32).clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_default_is_full_depth() {
        let pen = PenState::default();
        assert!((pen.depth_scale(true) - 1.0).abs() < 1e-5);
        assert!((pen.depth_scale(false) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn light_pressure_scales_depth() {
        let pen = PenState {
            pressure: 0.25,
            from_stylus: true,
        };
        assert!((pen.depth_scale(true) - 0.25).abs() < 1e-5);
        assert!((pen.depth_scale(false) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn normalized_force_maps_to_unit_interval() {
        assert!((force_to_pressure(ForceTouch::Normalized(0.4)) - 0.4).abs() < 1e-5);
        assert!((force_to_pressure(ForceTouch::Normalized(1.5)) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn calibrated_force_uses_max() {
        let f = ForceTouch::Calibrated {
            force: 2.0,
            max_possible_force: 4.0,
            altitude_angle: None,
        };
        assert!((force_to_pressure(f) - 0.5).abs() < 1e-5);
    }
}
