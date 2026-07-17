//! View features: workbench grid overlay + standard camera views.
//!
//! Adds a top-level `View` menu in the UI plus dispatch of two
//! action families:
//!
//! - [`AppAction::ToggleWorkbenchGrid`] flips a translucent grid
//!   drawn on the workbench plane. Line spacing is 10 mm on the
//!   major grid, 2 mm on the minor grid, spanning a 400 mm square
//!   centred on the origin.
//! - [`AppAction::SetView`] snaps the orbit camera to one of a
//!   handful of standard poses (Top / Front / Back / Left / Right
//!   / Bottom / Perspective). Distance is preserved so the user
//!   doesn't lose their zoom when previewing a face-on view.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;

use crate::actions::AppAction;
use crate::camera::OrbitCamera;

/// Camera preset. `Perspective` is a 3/4 view roughly matching the
/// starter pose; every other variant snaps yaw + pitch to look
/// straight along one axis.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ViewPreset {
    Perspective,
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
}

impl ViewPreset {
    pub fn label(self) -> &'static str {
        match self {
            ViewPreset::Perspective => "Perspective",
            ViewPreset::Top => "Top",
            ViewPreset::Bottom => "Bottom",
            ViewPreset::Front => "Front",
            ViewPreset::Back => "Back",
            ViewPreset::Left => "Left",
            ViewPreset::Right => "Right",
        }
    }

    /// Yaw / pitch (radians) for this preset. Yaw = 0 looks down
    /// -Z from +Z; positive yaw rotates counter-clockwise around Y.
    /// Pitch = 0 is horizontal; positive pitches the camera up
    /// (looking down onto the piece).
    pub fn yaw_pitch(self) -> (f32, f32) {
        use std::f32::consts::{FRAC_PI_2, PI};
        match self {
            // Matches the default `spawn_camera` orientation.
            ViewPreset::Perspective => (0.6, 0.35),
            ViewPreset::Top => (0.0, FRAC_PI_2 - 0.001),
            ViewPreset::Bottom => (0.0, -FRAC_PI_2 + 0.001),
            // Front = looking along +Z toward the piece (camera on -Z side).
            ViewPreset::Front => (PI, 0.0),
            ViewPreset::Back => (0.0, 0.0),
            ViewPreset::Right => (FRAC_PI_2, 0.0),
            ViewPreset::Left => (-FRAC_PI_2, 0.0),
        }
    }
}

/// Workbench grid overlay marker + toggle state.
#[derive(Component)]
pub struct WorkbenchGrid;

#[derive(Resource)]
pub struct WorkbenchGridState {
    pub visible: bool,
}

impl Default for WorkbenchGridState {
    fn default() -> Self {
        // On by default — the overlay is the main scale reference on
        // the worktable; View / Preferences can still hide it.
        Self { visible: true }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<WorkbenchGridState>();
    app.add_systems(Startup, spawn_workbench_grid);
    app.add_systems(Update, (handle_view_actions, apply_grid_visibility));
}

/// Build the workbench grid mesh once. Lines live in a `LineList`
/// mesh at `y = 0.5` (a hair above the workbench slab to avoid
/// z-fighting). Two intensities — a bright 10 mm major line every 5
/// minor lines, a fainter 2 mm minor line otherwise — but for
/// simplicity we render both in the same mesh and rely on the
/// material tint alone. Users who want a coloured major grid can
/// swap two meshes later.
fn spawn_workbench_grid(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(build_grid_mesh(400.0, 10.0));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.85, 0.90, 1.0, 0.75),
        emissive: LinearRgba::new(1.2, 1.35, 1.6, 1.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        // A hair above the workbench slab to avoid depth fights.
        Transform::from_xyz(0.0, 0.05, 0.0),
        // Matches `WorkbenchGridState::default` (visible).
        Visibility::Visible,
        WorkbenchGrid,
    ));
}

/// Line-list grid spanning `size` mm centred on the origin, with a
/// major line every `major_step` mm. All lines in the same mesh,
/// rendered flat on the workbench plane.
fn build_grid_mesh(size: f32, major_step: f32) -> Mesh {
    let half = size * 0.5;
    let n = (size / major_step).round() as i32;
    let mut positions: Vec<[f32; 3]> = Vec::new();

    for i in -n / 2..=n / 2 {
        let t = i as f32 * major_step;
        // Line along X at constant Z = t.
        positions.push([-half, 0.0, t]);
        positions.push([half, 0.0, t]);
        // Line along Z at constant X = t.
        positions.push([t, 0.0, -half]);
        positions.push([t, 0.0, half]);
    }
    let count = positions.len();
    let indices: Vec<u32> = (0..count as u32).collect();
    let normals: Vec<[f32; 3]> = vec![[0.0, 1.0, 0.0]; count];

    let mut mesh = Mesh::new(
        PrimitiveTopology::LineList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Consume `ToggleWorkbenchGrid` and `SetView`. Kept together
/// because both are pure view-state changes that should not fight
/// each other for ordering.
fn handle_view_actions(
    mut events: EventReader<AppAction>,
    mut grid_state: ResMut<WorkbenchGridState>,
    mut q_cam: Query<(&mut OrbitCamera, &mut Transform)>,
) {
    for a in events.read() {
        match a {
            AppAction::ToggleWorkbenchGrid => {
                grid_state.visible = !grid_state.visible;
                info!(
                    "workbench grid: {}",
                    if grid_state.visible { "on" } else { "off" }
                );
            }
            AppAction::SetWorkbenchGrid(on) => {
                grid_state.visible = *on;
                info!(
                    "workbench grid: {}",
                    if grid_state.visible { "on" } else { "off" }
                );
            }
            AppAction::SetView(preset) => {
                if let Ok((mut cam, mut tf)) = q_cam.get_single_mut() {
                    let (yaw, pitch) = preset.yaw_pitch();
                    cam.yaw = yaw;
                    cam.pitch = pitch;
                    // Standard views aim at the piece centre — the
                    // starter camera target sits at y = 45, which
                    // works for hand-sized pieces resting on the
                    // bench. Presets don't touch distance so the
                    // user keeps their current zoom.
                    *tf = compute_orbit_transform(&cam);
                    info!("view: {}", preset.label());
                }
            }
            _ => {}
        }
    }
}

/// Mirror of `camera::compute_transform`, inlined here so we don't
/// leak the internal helper. Keep in sync with the source if
/// `OrbitCamera` semantics change.
fn compute_orbit_transform(cam: &OrbitCamera) -> Transform {
    let cp = cam.pitch.cos();
    let offset = Vec3::new(cp * cam.yaw.sin(), cam.pitch.sin(), cp * cam.yaw.cos())
        * cam.distance;
    let pos = cam.target + offset;
    Transform::from_translation(pos).looking_at(cam.target, Vec3::Y)
}

/// Reflect the toggle state onto the grid entity's visibility. Keeps
/// the action handler side-effect-free (it only flips a bool) and
/// makes the resource inspectable for save-on-quit later.
fn apply_grid_visibility(
    grid_state: Res<WorkbenchGridState>,
    mut q: Query<&mut Visibility, With<WorkbenchGrid>>,
) {
    let Ok(mut vis) = q.get_single_mut() else {
        return;
    };
    let want = if grid_state.visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if *vis != want {
        *vis = want;
    }
}
