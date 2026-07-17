//! View features: workbench grid overlay + standard camera views +
//! camera bookmarks.
//!
//! Adds a top-level `View` menu in the UI plus dispatch of:
//!
//! - [`AppAction::ToggleWorkbenchGrid`] — translucent grid on the bench
//! - [`AppAction::SetView`] — standard Top / Front / … presets
//! - Camera bookmarks — save / restore full orbit poses (target,
//!   distance, yaw, pitch), persisted under `~/.mud/bookmarks.txt`

use std::fs;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;

use crate::actions::AppAction;
use crate::camera::{compute_orbit_transform, OrbitCamera};

/// Cap for saved camera bookmarks (and on-disk list).
pub const MAX_CAMERA_BOOKMARKS: usize = 8;

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

/// One saved orbit pose.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraBookmark {
    pub name: String,
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl CameraBookmark {
    pub fn from_camera(name: String, cam: &OrbitCamera) -> Self {
        Self {
            name,
            target: cam.target,
            distance: cam.distance,
            yaw: cam.yaw,
            pitch: cam.pitch,
        }
    }

    pub fn apply_to(&self, cam: &mut OrbitCamera) {
        cam.target = self.target;
        cam.distance = self.distance;
        cam.yaw = self.yaw;
        cam.pitch = self.pitch;
    }
}

/// Saved camera bookmarks for `View → Bookmarks`.
///
/// Persisted under `~/.mud/bookmarks.txt` (one tab-separated line
/// per bookmark). Newest save is prepended; list is capped at
/// [`MAX_CAMERA_BOOKMARKS`].
#[derive(Resource, Clone, Debug)]
pub struct CameraBookmarks {
    slots: Vec<CameraBookmark>,
}

impl Default for CameraBookmarks {
    fn default() -> Self {
        Self {
            slots: load_bookmarks(&bookmarks_store_path()),
        }
    }
}

impl CameraBookmarks {
    pub fn slots(&self) -> &[CameraBookmark] {
        &self.slots
    }

    /// Snapshot `cam` as a new bookmark at the front of the list.
    pub fn save_current(&mut self, cam: &OrbitCamera) -> &CameraBookmark {
        let name = next_bookmark_name(&self.slots);
        let bookmark = CameraBookmark::from_camera(name, cam);
        self.slots.insert(0, bookmark);
        self.slots.truncate(MAX_CAMERA_BOOKMARKS);
        save_bookmarks(&bookmarks_store_path(), &self.slots);
        &self.slots[0]
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        save_bookmarks(&bookmarks_store_path(), &self.slots);
    }
}

fn next_bookmark_name(slots: &[CameraBookmark]) -> String {
    let mut n = slots.len() + 1;
    loop {
        let candidate = format!("Bookmark {n}");
        if slots.iter().all(|b| b.name != candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn bookmarks_store_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match home {
        Some(h) => h.join(".mud").join("bookmarks.txt"),
        None => PathBuf::from(".mud-bookmarks.txt"),
    }
}

fn load_bookmarks(store: &Path) -> Vec<CameraBookmark> {
    let Ok(text) = fs::read_to_string(store) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(parse_bookmark_line)
        .take(MAX_CAMERA_BOOKMARKS)
        .collect()
}

fn parse_bookmark_line(line: &str) -> Option<CameraBookmark> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.split('\t');
    let name = parts.next()?.trim();
    if name.is_empty() {
        return None;
    }
    let tx: f32 = parts.next()?.parse().ok()?;
    let ty: f32 = parts.next()?.parse().ok()?;
    let tz: f32 = parts.next()?.parse().ok()?;
    let distance: f32 = parts.next()?.parse().ok()?;
    let yaw: f32 = parts.next()?.parse().ok()?;
    let pitch: f32 = parts.next()?.parse().ok()?;
    if !distance.is_finite() || distance <= 0.0 {
        return None;
    }
    Some(CameraBookmark {
        name: name.to_string(),
        target: Vec3::new(tx, ty, tz),
        distance,
        yaw,
        pitch,
    })
}

fn save_bookmarks(store: &Path, slots: &[CameraBookmark]) {
    if let Some(parent) = store.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body: String = slots
        .iter()
        .map(|b| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                b.name.replace(['\t', '\n'], " "),
                b.target.x,
                b.target.y,
                b.target.z,
                b.distance,
                b.yaw,
                b.pitch
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(e) = fs::write(store, body) {
        warn!(
            "couldn't write camera bookmarks {}: {e}",
            store.display()
        );
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
    app.init_resource::<CameraBookmarks>();
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

/// Consume grid / view-preset / bookmark actions.
fn handle_view_actions(
    mut events: EventReader<AppAction>,
    mut grid_state: ResMut<WorkbenchGridState>,
    mut bookmarks: ResMut<CameraBookmarks>,
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
            AppAction::SaveCameraBookmark => {
                if let Ok((cam, _)) = q_cam.get_single() {
                    let saved = bookmarks.save_current(cam);
                    info!("bookmark saved: {}", saved.name);
                }
            }
            AppAction::RestoreCameraBookmark(index) => {
                let Some(bookmark) = bookmarks.slots().get(*index).cloned() else {
                    warn!("bookmark #{index} missing");
                    continue;
                };
                if let Ok((mut cam, mut tf)) = q_cam.get_single_mut() {
                    bookmark.apply_to(&mut cam);
                    *tf = compute_orbit_transform(&cam);
                    info!("view: restored {}", bookmark.name);
                }
            }
            AppAction::ClearCameraBookmarks => {
                bookmarks.clear();
                info!("camera bookmarks cleared");
            }
            _ => {}
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_round_trips_through_text_line() {
        let b = CameraBookmark {
            name: "Bookmark 1".into(),
            target: Vec3::new(1.0, 45.0, -2.5),
            distance: 300.0,
            yaw: 0.6,
            pitch: 0.35,
        };
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            b.name, b.target.x, b.target.y, b.target.z, b.distance, b.yaw, b.pitch
        );
        let parsed = parse_bookmark_line(&line).expect("parse");
        assert_eq!(parsed, b);
    }

    #[test]
    fn bookmark_list_round_trips_on_disk() {
        let dir = std::env::temp_dir().join("mud-bookmark-test-roundtrip");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = dir.join("bookmarks.txt");
        let slots = vec![
            CameraBookmark {
                name: "Bookmark 1".into(),
                target: Vec3::new(0.0, 45.0, 0.0),
                distance: 280.0,
                yaw: 1.0,
                pitch: 0.2,
            },
            CameraBookmark {
                name: "Bookmark 2".into(),
                target: Vec3::new(10.0, 20.0, 30.0),
                distance: 400.0,
                yaw: -0.5,
                pitch: 0.1,
            },
        ];
        save_bookmarks(&store, &slots);
        assert_eq!(load_bookmarks(&store), slots);
    }

    #[test]
    fn save_current_prepends_and_caps() {
        let mut bookmarks = CameraBookmarks { slots: Vec::new() };
        let cam = OrbitCamera::new(Vec3::ZERO, 200.0, 0.0, 0.0);
        for _ in 0..MAX_CAMERA_BOOKMARKS + 2 {
            bookmarks.slots.insert(
                0,
                CameraBookmark::from_camera(next_bookmark_name(&bookmarks.slots), &cam),
            );
            bookmarks.slots.truncate(MAX_CAMERA_BOOKMARKS);
        }
        assert_eq!(bookmarks.slots.len(), MAX_CAMERA_BOOKMARKS);
        assert_eq!(bookmarks.slots[0].name, format!("Bookmark {}", MAX_CAMERA_BOOKMARKS + 2));
    }

    #[test]
    fn apply_to_copies_full_pose() {
        let bookmark = CameraBookmark {
            name: "x".into(),
            target: Vec3::new(3.0, 4.0, 5.0),
            distance: 123.0,
            yaw: 0.7,
            pitch: -0.2,
        };
        let mut cam = OrbitCamera::new(Vec3::ZERO, 300.0, 0.0, 0.0);
        bookmark.apply_to(&mut cam);
        assert_eq!(cam.target, bookmark.target);
        assert_eq!(cam.distance, bookmark.distance);
        assert_eq!(cam.yaw, bookmark.yaw);
        assert_eq!(cam.pitch, bookmark.pitch);
    }
}
