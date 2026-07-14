//! Digital turntable. Hold Q for CCW, E for CW. No inertia, no ramp —
//! release the key and rotation stops immediately. This is deliberately
//! *better than a real pottery wheel*, matching the brief in §Turntable.

use bevy::prelude::*;

use crate::input_gate::UiCapturesInput;
use crate::workpiece::WorkpieceRoot;

/// System set so sculpt can sample the piece transform *after* this
/// frame's turntable write (otherwise stamps lag Q/E by one frame and
/// small-brush rings drop out).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TurntableSet;

#[derive(Resource, Default)]
pub struct TurntableState {
    /// Cumulative rotation angle (radians) around world Y.
    pub angle: f32,
    /// Current angular velocity in rad/s. Zero when neither key is held.
    pub angular_vel: f32,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<TurntableState>();
    app.configure_sets(Update, TurntableSet);
    app.add_systems(
        Update,
        (read_turntable_input, apply_turntable_rotation)
            .chain()
            .in_set(TurntableSet),
    );
}

fn read_turntable_input(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    mut state: ResMut<TurntableState>,
) {
    // One revolution in ~6.5 s — slower than the old ~4 s turn so
    // add-while-rotating rings stay controllable, especially with
    // small brushes.
    const TARGET_SPEED: f32 = std::f32::consts::TAU / 6.5;
    if ui_gate.keyboard {
        state.angular_vel = 0.0;
        return;
    }
    let mut v = 0.0f32;
    if keys.pressed(KeyCode::KeyQ) {
        v += TARGET_SPEED;
    }
    if keys.pressed(KeyCode::KeyE) {
        v -= TARGET_SPEED;
    }
    state.angular_vel = v;
}

fn apply_turntable_rotation(
    time: Res<Time>,
    mut state: ResMut<TurntableState>,
    mut q_piece: Query<&mut Transform, With<WorkpieceRoot>>,
) {
    if state.angular_vel == 0.0 {
        return;
    }
    state.angle += state.angular_vel * time.delta_secs();
    if let Ok(mut tf) = q_piece.get_single_mut() {
        tf.rotation = Quat::from_rotation_y(state.angle);
    }
}
