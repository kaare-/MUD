//! Sculpt input dispatch.
//!
//! Two tools live here in Stage 2: the spherical finger from Stage 0/1
//! and the cookie cutter introduced by Stage 2. Left-drag engages the
//! finger continuously; left-click one-shots the cookie cutter.
//!
//! Symmetry is a small overlay that applies each stamp again at its
//! mirror image across the piece-local X = 0 plane. Toggle with `S`.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::Vec3 as GVec3;

use sculpt_core::{
    apply_cookie_cutter_with_callback, apply_sphere_brush_with_callback, BrushMode, CookieCutter,
    Profile, SphereBrush,
};

use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{SculptWorkpiece, WorkpieceRoot};

/// Which tool the user is currently holding.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ToolKind {
    Finger,
    Cutter(CutterFamily),
}

/// The set of cookie-cutter shapes the Stage-2 palette exposes. Each
/// maps to a `Profile` via `profile(size)`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CutterFamily {
    Circle,
    Square,
    Hexagon,
    Star5,
}

impl CutterFamily {
    pub fn profile(self, size: f32) -> Profile {
        match self {
            CutterFamily::Circle => Profile::Circle { radius: size },
            CutterFamily::Square => Profile::Square { half_side: size },
            CutterFamily::Hexagon => Profile::Hexagon { radius: size },
            CutterFamily::Star5 => Profile::Star5 {
                outer: size,
                inner_ratio: 0.4,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CutterFamily::Circle => "circle",
            CutterFamily::Square => "square",
            CutterFamily::Hexagon => "hexagon",
            CutterFamily::Star5 => "star",
        }
    }
}

/// The active tool plus its size and mode flags.
///
/// `size` is a shared knob (mm). For the finger it's the brush
/// radius; for a cutter it's the profile's characteristic size.
/// `[` and `]` adjust it regardless of which tool is active.
#[derive(Resource)]
pub struct SculptTool {
    pub kind: ToolKind,
    pub size: f32,
    /// Finger-only: how far the brush centre advances along the surface
    /// normal each frame while held. Closest thing Stage 1 has to
    /// pressure sensitivity.
    pub advance_per_step: f32,
    /// Finger-only: whether to apply the magic-clay bulge on Press.
    pub displace: bool,
}

impl Default for SculptTool {
    fn default() -> Self {
        Self {
            kind: ToolKind::Finger,
            size: 12.0,
            advance_per_step: 0.6,
            displace: true,
        }
    }
}

/// Live mirror plane. Piece-local X = 0. When enabled, every stamp is
/// applied twice — once at the primary contact point, once at its
/// reflection across the plane.
#[derive(Resource, Copy, Clone)]
pub struct SculptSymmetry {
    pub enabled: bool,
}

impl Default for SculptSymmetry {
    fn default() -> Self {
        // On by default, per DESIGN §9 decision 5 (symmetry defaults
        // on for the starter primitive).
        Self { enabled: true }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<SculptTool>();
    app.init_resource::<SculptSymmetry>();
    app.add_systems(Update, (sculpt_input, adjust_tool));
}

#[allow(clippy::too_many_arguments)]
fn sculpt_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut stroke: ResMut<SculptStroke>,
    mut history: ResMut<UndoHistory>,
) {
    // Stroke lifecycle. Same recorder shape for both tools; the
    // cutter just contributes fewer stamps (typically one).
    if buttons.just_pressed(MouseButton::Left) {
        stroke.recorder = Some(StrokeRecorder::default());
    }
    if buttons.just_released(MouseButton::Left) {
        if let Some(rec) = stroke.recorder.take() {
            if let Some(entry) = rec.finish(&workpiece.grid) {
                history.push_stroke(entry);
            }
        }
    }

    // The finger engages every frame LMB is held. The cutter is a
    // one-shot: fire on the frame LMB is first pressed, then stop
    // so a slow drag doesn't chain cutter stamps by accident.
    let should_engage = match tool.kind {
        ToolKind::Finger => buttons.pressed(MouseButton::Left),
        ToolKind::Cutter(_) => buttons.just_pressed(MouseButton::Left),
    };
    if !should_engage {
        return;
    }

    let Ok(window) = q_window.get_single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_tf)) = q_camera.get_single() else {
        return;
    };
    let Ok(piece_tf) = q_piece.get_single() else {
        return;
    };

    let ray_world = match camera.viewport_to_world(cam_tf, cursor) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Transform the ray into piece-local space so tool paths are
    // recorded independent of the turntable rotation.
    let piece_inv = piece_tf.affine().inverse();
    let origin_local = piece_inv.transform_point3(ray_world.origin);
    let dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();

    let hit_g = GVec3::new(origin_local.x, origin_local.y, origin_local.z);
    let dir_g = GVec3::new(dir_local.x, dir_local.y, dir_local.z);
    let hit = match workpiece.grid.ray_march(hit_g, dir_g, 4000.0) {
        Some(p) => p,
        None => return,
    };

    // "Into surface" direction — SDF gradient points outward, so
    // negate. Fall back to the ray direction on a degenerate gradient.
    let grad = workpiece.grid.gradient_at(hit);
    let into_surface = if grad.length_squared() > 1e-4 {
        -grad.normalize()
    } else {
        dir_g
    };

    // Apply once at the primary contact, then again mirrored if
    // symmetry is on. The mirror flips both the position and the
    // press direction across piece-local X = 0.
    apply_at(
        &mut workpiece,
        stroke.recorder.as_mut(),
        tool.kind,
        &tool,
        keys.as_ref(),
        hit,
        into_surface,
    );
    if symmetry.enabled {
        let mirrored_hit = GVec3::new(-hit.x, hit.y, hit.z);
        let mirrored_dir = GVec3::new(-into_surface.x, into_surface.y, into_surface.z);
        apply_at(
            &mut workpiece,
            stroke.recorder.as_mut(),
            tool.kind,
            &tool,
            keys.as_ref(),
            mirrored_hit,
            mirrored_dir,
        );
    }
}

fn apply_at(
    workpiece: &mut SculptWorkpiece,
    mut recorder: Option<&mut StrokeRecorder>,
    kind: ToolKind,
    tool: &SculptTool,
    keys: &ButtonInput<KeyCode>,
    hit: GVec3,
    into_surface: GVec3,
) {
    let grid_res = workpiece.grid.res();

    let region = match kind {
        ToolKind::Finger => {
            let mode = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                BrushMode::Pull
            } else {
                BrushMode::Press
            };
            // Offset each frame so a held button carves progressively:
            // Press pushes deeper into the surface, Pull backs out.
            let advance = tool.advance_per_step;
            let center = match mode {
                BrushMode::Press => hit + into_surface * advance,
                BrushMode::Pull => hit - into_surface * advance,
            };
            let brush = SphereBrush {
                center,
                radius: tool.size,
                mode,
                direction: into_surface,
                displace: tool.displace,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_sphere_brush_with_callback(&mut workpiece.grid, &brush, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_sphere_brush_with_callback(&mut workpiece.grid, &brush, |_, _, _, _| {})
            }
        }
        ToolKind::Cutter(family) => {
            // Cutter cuts along the surface normal, symmetric around
            // the click point. A half-length of half the grid extent
            // is enough to punch through any Stage-2 workpiece.
            let half_length = workpiece.grid.extent().max_element() * 0.5;
            let cutter = CookieCutter {
                profile: family.profile(tool.size),
                origin: hit,
                axis: into_surface,
                half_length,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_cookie_cutter_with_callback(&mut workpiece.grid, &cutter, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_cookie_cutter_with_callback(&mut workpiece.grid, &cutter, |_, _, _, _| {})
            }
        }
    };

    if let Some(region) = region {
        if let Some(rec) = recorder.as_mut() {
            rec.record_dirty_region(region, grid_res);
        }
        for c in region.touched_chunks(grid_res) {
            workpiece.dirty.insert((c.x, c.y, c.z));
        }
    }
}

/// Minimum and maximum tool size in mm. Below the min the sculpt
/// footprint is smaller than a voxel; above the max it dwarfs the
/// starter primitive.
const SIZE_MIN: f32 = 2.0;
const SIZE_MAX: f32 = 60.0;
/// Per-keystroke size step. `SIZE_STEP_UP` = `1 / SIZE_STEP_DOWN` so
/// pressing "smaller then bigger" returns you to the same size.
const SIZE_STEP_DOWN: f32 = 0.85;
const SIZE_STEP_UP: f32 = 1.0 / 0.85;
/// Shift-scroll size sensitivity (per wheel tick).
const SIZE_SCROLL_SENSITIVITY: f32 = 0.06;

/// Tool selection, size, magic-clay toggle, symmetry toggle.
///
/// Size can be adjusted three equivalent ways so no keyboard layout
/// leaves you stranded:
///
/// - `[` / `]` — the classic brush-size convention.
/// - `-` / `=` — same, on physical keys that live outside the
///   alphabet range on most non-US layouts.
/// - `Shift + scroll` — universal; works on Mac trackpads too.
///
/// Every change logs the new size so you get instant feedback.
fn adjust_tool(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: EventReader<MouseWheel>,
    mut tool: ResMut<SculptTool>,
    mut symmetry: ResMut<SculptSymmetry>,
) {
    if keys.just_pressed(KeyCode::Digit1) {
        tool.kind = ToolKind::Finger;
        bevy::log::info!("tool: finger");
    }
    if keys.just_pressed(KeyCode::Digit2) {
        tool.kind = ToolKind::Cutter(CutterFamily::Circle);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Circle.label());
    }
    if keys.just_pressed(KeyCode::Digit3) {
        tool.kind = ToolKind::Cutter(CutterFamily::Square);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Square.label());
    }
    if keys.just_pressed(KeyCode::Digit4) {
        tool.kind = ToolKind::Cutter(CutterFamily::Hexagon);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Hexagon.label());
    }
    if keys.just_pressed(KeyCode::Digit5) {
        tool.kind = ToolKind::Cutter(CutterFamily::Star5);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Star5.label());
    }

    // Size: three equivalent ways to adjust it. Track the old value
    // so we only log when it actually changes.
    let old_size = tool.size;

    // Keyboard: [ / ] and - / =.
    if keys.just_pressed(KeyCode::BracketLeft) || keys.just_pressed(KeyCode::Minus) {
        tool.size = (tool.size * SIZE_STEP_DOWN).max(SIZE_MIN);
    }
    if keys.just_pressed(KeyCode::BracketRight) || keys.just_pressed(KeyCode::Equal) {
        tool.size = (tool.size * SIZE_STEP_UP).min(SIZE_MAX);
    }

    // Shift + scroll wheel. When Shift is held the camera plugin
    // yields, so this doesn't double up with zoom.
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut scroll_delta = 0.0f32;
    for ev in wheel.read() {
        if shift {
            scroll_delta += ev.y;
        }
    }
    if scroll_delta != 0.0 {
        tool.size = (tool.size * (1.0 + scroll_delta * SIZE_SCROLL_SENSITIVITY))
            .clamp(SIZE_MIN, SIZE_MAX);
    }

    if (tool.size - old_size).abs() > 0.05 {
        bevy::log::info!("tool size: {:.1} mm", tool.size);
    }

    if keys.just_pressed(KeyCode::KeyM) {
        tool.displace = !tool.displace;
        bevy::log::info!(
            "magic-clay displacement: {}",
            if tool.displace { "on" } else { "off" }
        );
    }

    if keys.just_pressed(KeyCode::KeyS) {
        symmetry.enabled = !symmetry.enabled;
        bevy::log::info!(
            "symmetry: {}",
            if symmetry.enabled { "on" } else { "off" }
        );
    }
}
