//! Orbit camera in the third-person-game convention: right-drag to orbit,
//! middle-drag to pan, scroll to zoom. Deliberately not Blender-style.

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;

use crate::input_gate::UiCapturesInput;

#[derive(Component)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl OrbitCamera {
    pub fn new(target: Vec3, distance: f32, yaw: f32, pitch: f32) -> Self {
        Self {
            target,
            distance,
            yaw,
            pitch,
        }
    }
}

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, spawn_camera);
    app.add_systems(Update, orbit_camera_control);
}

fn spawn_camera(mut commands: Commands) {
    // Aim at roughly the top of the starter ball. Distance in mm.
    let cam = OrbitCamera::new(Vec3::new(0.0, 45.0, 0.0), 300.0, 0.6, 0.35);
    let tf = compute_orbit_transform(&cam);
    commands.spawn((Camera3d::default(), tf, cam));
}

/// World transform for an orbit pose. Shared with View presets /
/// bookmarks so restore matches live orbit control.
pub fn compute_orbit_transform(cam: &OrbitCamera) -> Transform {
    let cp = cam.pitch.cos();
    let offset = Vec3::new(cp * cam.yaw.sin(), cam.pitch.sin(), cp * cam.yaw.cos()) * cam.distance;
    let pos = cam.target + offset;
    Transform::from_translation(pos).looking_at(cam.target, Vec3::Y)
}

fn orbit_camera_control(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    mut motion: EventReader<MouseMotion>,
    mut wheel: EventReader<MouseWheel>,
    mut q: Query<(&mut OrbitCamera, &mut Transform)>,
) {
    // Drain events even when the UI has the pointer, so we don't
    // apply a huge accumulated delta the frame the cursor re-enters
    // the viewport. But skip actually moving the camera.
    let mut mouse_delta = Vec2::ZERO;
    for ev in motion.read() {
        mouse_delta += ev.delta;
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut scroll = 0.0f32;
    for ev in wheel.read() {
        if !shift {
            scroll += ev.y;
        }
    }

    if ui_gate.pointer {
        return;
    }

    let Ok((mut cam, mut tf)) = q.get_single_mut() else {
        return;
    };

    if buttons.pressed(MouseButton::Right) {
        cam.yaw -= mouse_delta.x * 0.005;
        cam.pitch = (cam.pitch + mouse_delta.y * 0.005).clamp(-1.4, 1.4);
    }
    if buttons.pressed(MouseButton::Middle) && mouse_delta.length_squared() > 0.0 {
        // Pan in the camera's local frame. Speed scales with distance so
        // the piece feels the same size on screen regardless of zoom.
        let right = tf.rotation * Vec3::X;
        let up = tf.rotation * Vec3::Y;
        let dist = cam.distance;
        cam.target += (-right * mouse_delta.x + up * mouse_delta.y) * dist * 0.002;
    }
    if scroll != 0.0 {
        cam.distance = (cam.distance * (1.0 - scroll * 0.1)).clamp(60.0, 2000.0);
    }

    *tf = compute_orbit_transform(&cam);
}
