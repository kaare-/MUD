//! MUD — Stage 0 prototype.
//!
//! One SDF grid, one spherical brush, orbit camera, Q/E turntable.
//! No undo, no workbench collision, no volume redistribution.
//! The point of this stage is to answer:
//!
//!   "Does a 3D-native player, with no tutorial, start pushing material
//!    around within 30 seconds and end up with something they're
//!    pleased with in five minutes?"
//!
//! Controls:
//!   Right-drag ....... orbit camera
//!   Middle-drag ...... pan camera
//!   Scroll ........... zoom
//!   Q / E ............ turntable left / right (hold)
//!   Left-drag ........ engage the active tool
//!   Shift+Left ....... Add/Remove only: add material
//!   1 ................ tool: Add/Remove (default)
//!   2 ................ tool: cookie cutter (circle)
//!   3 ................ tool: cookie cutter (square)
//!   4 ................ tool: cookie cutter (hexagon)
//!   5 ................ tool: cookie cutter (star)
//!   6 ................ tool: wire cutter (LMB-drag slices through)
//!   7 ................ tool: smooth (hold LMB to polish high-frequency detail)
//!   8 ................ tool: paddle (hold LMB to press a flat)
//!   [ / ] or - / = ... shrink / grow the active tool (keyboard)
//!   Shift + scroll .... shrink / grow the active tool (mouse / trackpad)
//!   M ................ toggle magic-clay (Add/Remove)
//!   S ................ toggle mirror symmetry (piece-local X = 0)
//!   Ctrl+Z ........... undo last stroke
//!   Ctrl+Y ........... redo (also Ctrl+Shift+Z)
//!   Ctrl+E ........... export the current piece as STL (Z-up, print-ready)
//!   Ctrl+S ........... save the current piece (auto-timestamped filename)
//!   Ctrl+Shift+S ..... save the current piece with a chosen filename (dialog)
//!   Ctrl+O ........... open a project via a picker dialog
//!   Ctrl+Shift+O ..... quick-reopen the most recently modified project
//!   Esc .............. quit

use bevy::prelude::*;
use bevy::window::WindowResolution;

mod actions;
mod camera;
mod export;
mod input_gate;
mod preview;
mod project;
mod sculpt;
mod turntable;
mod ui;
mod undo;
mod workpiece;

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "MUD — sculpt".into(),
                        resolution: WindowResolution::new(1280.0, 800.0),
                        ..default()
                    }),
                    ..default()
                })
                // Bevy's default LogPlugin is fine; keep terminal noise low.
                .set(bevy::log::LogPlugin {
                    level: bevy::log::Level::INFO,
                    // wgpu_hal::gles emits noisy "ERROR" messages at
                    // startup that are actually informational (dimension
                    // heuristics on the GL ES backend). Silence just
                    // that target; genuine wgpu warnings still surface.
                    filter: "wgpu=warn,naga=warn,wgpu_hal::gles=off".into(),
                    ..default()
                }),
        )
        .insert_resource(ClearColor(Color::srgb(0.14, 0.15, 0.17)))
        .insert_resource(AmbientLight {
            color: Color::srgb(0.9, 0.92, 1.0),
            brightness: 220.0,
        })
        .add_plugins((
            actions::plugin,
            input_gate::plugin,
            camera::plugin,
            turntable::plugin,
            workpiece::plugin,
            undo::plugin,
            sculpt::plugin,
            preview::plugin,
            export::plugin,
            project::plugin,
            ui::plugin,
        ))
        .add_systems(Startup, setup_scene)
        .add_systems(Update, esc_quit)
        .run();
}

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Workbench: a wide matte slab at Y=0. Deliberately non-fancy —
    // Stage 0 does not collide with it yet.
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(2000.0, 4.0, 2000.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.62, 0.55, 0.48),
            perceptual_roughness: 0.92,
            metallic: 0.0,
            ..default()
        })),
        Transform::from_xyz(0.0, -2.0, 0.0),
    ));

    // Key light: warm, upper-front-right. Shadows on — sculpt reads
    // much better with contact shadow on the workbench.
    commands.spawn((
        DirectionalLight {
            illuminance: 14_000.0,
            shadows_enabled: true,
            color: Color::srgb(1.0, 0.98, 0.94),
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -0.6, -1.1, 0.0)),
    ));

    // Fill light: cool, upper-back-left. No shadows (cheaper).
    commands.spawn((
        DirectionalLight {
            illuminance: 5_500.0,
            shadows_enabled: false,
            color: Color::srgb(0.75, 0.85, 1.0),
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 2.4, -0.6, 0.0)),
    ));
}

fn esc_quit(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<input_gate::UiCapturesInput>,
    mut actions: EventWriter<actions::AppAction>,
) {
    if ui_gate.keyboard {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        actions.send(actions::AppAction::Quit);
    }
}
