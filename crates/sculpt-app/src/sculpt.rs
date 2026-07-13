//! Sculpt input handler.
//!
//! Left-drag on the workpiece: press (carve). Shift + left-drag: pull
//! (add). The tool position is picked by sphere-tracing the SDF from
//! the camera through the cursor.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::Vec3 as GVec3;

use sculpt_core::{apply_sphere_brush, BrushMode, SphereBrush};

use crate::workpiece::{SculptWorkpiece, WorkpieceRoot};

/// The one Stage-0 tool.
///
/// - `radius` is the brush footprint in mm.
/// - `advance_per_step` is how far the brush is offset along the ray
///   direction each frame it is engaged. A larger value carves faster;
///   a small value gives a gentle, precise touch. This is the closest
///   Stage 0 gets to a "pressure" analog.
#[derive(Resource)]
pub struct SculptTool {
    pub radius: f32,
    pub advance_per_step: f32,
}

impl Default for SculptTool {
    fn default() -> Self {
        Self {
            radius: 12.0,
            advance_per_step: 0.6,
        }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<SculptTool>();
    app.add_systems(Update, sculpt_input);
    app.add_systems(Update, adjust_tool_size);
}

fn sculpt_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    mut workpiece: ResMut<SculptWorkpiece>,
) {
    if !buttons.pressed(MouseButton::Left) {
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

    // World ray from the camera through the cursor.
    let ray_world = match camera.viewport_to_world(cam_tf, cursor) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Transform ray into piece-local space (turntable rotation undone).
    let piece_inv = piece_tf.affine().inverse();
    let origin_local = piece_inv.transform_point3(ray_world.origin);
    let dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();

    let hit = match workpiece.grid.ray_march(
        GVec3::new(origin_local.x, origin_local.y, origin_local.z),
        GVec3::new(dir_local.x, dir_local.y, dir_local.z),
        4000.0,
    ) {
        Some(p) => p,
        None => return,
    };

    let mode = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
        BrushMode::Pull
    } else {
        BrushMode::Press
    };

    // The brush is a hard CSG operation. To get progressive-feeling
    // strokes when the mouse is held stationary, offset the centre
    // each frame:
    // - Press: push the brush slightly *further along* the ray, i.e.
    //   deeper into the workpiece. Each frame carves a fresh slice.
    // - Pull: push the brush slightly *back toward the viewer*, so
    //   material builds outward.
    let advance = tool.advance_per_step;
    let center = match mode {
        BrushMode::Press => hit + GVec3::new(dir_local.x, dir_local.y, dir_local.z) * advance,
        BrushMode::Pull => hit - GVec3::new(dir_local.x, dir_local.y, dir_local.z) * advance,
    };

    let brush = SphereBrush {
        center,
        radius: tool.radius,
        mode,
    };

    if let Some(region) = apply_sphere_brush(&mut workpiece.grid, &brush) {
        let touched = region.touched_chunks(workpiece.grid.res());
        for c in touched {
            workpiece.dirty.insert((c.x, c.y, c.z));
        }
    }
}

/// `[` shrinks the brush, `]` grows it. Small steps because you tune
/// this once for the piece you're working on, not constantly.
fn adjust_tool_size(keys: Res<ButtonInput<KeyCode>>, mut tool: ResMut<SculptTool>) {
    if keys.just_pressed(KeyCode::BracketLeft) {
        tool.radius = (tool.radius * 0.85).max(2.0);
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        tool.radius = (tool.radius * 1.176).min(60.0);
    }
}
