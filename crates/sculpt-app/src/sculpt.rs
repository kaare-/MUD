//! Sculpt input handler.
//!
//! Left-drag on the workpiece: press (carve). Shift + left-drag: pull
//! (add). The tool position is picked by sphere-tracing the SDF from
//! the camera through the cursor.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::Vec3 as GVec3;

use sculpt_core::{apply_sphere_brush_with_callback, BrushMode, SphereBrush};

use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{SculptWorkpiece, WorkpieceRoot};

/// The one Stage-0/1 tool — a spherical finger.
///
/// - `radius` is the brush footprint in mm.
/// - `advance_per_step` is how far the brush is offset along the
///   surface normal each frame it is engaged. A larger value carves
///   faster; a small value gives a gentle, precise touch. This is
///   the closest Stage 1 gets to a pressure analog.
/// - `displace` enables the magic-clay volume-displacement bulge for
///   Press mode. Toggled with `M`.
#[derive(Resource)]
pub struct SculptTool {
    pub radius: f32,
    pub advance_per_step: f32,
    pub displace: bool,
}

impl Default for SculptTool {
    fn default() -> Self {
        Self {
            radius: 12.0,
            advance_per_step: 0.6,
            displace: true,
        }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<SculptTool>();
    app.add_systems(Update, sculpt_input);
    app.add_systems(Update, adjust_tool_size);
}

#[allow(clippy::too_many_arguments)]
fn sculpt_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut stroke: ResMut<SculptStroke>,
    mut history: ResMut<UndoHistory>,
) {
    // Stroke lifecycle: pressing LMB starts a recorder; releasing it
    // ends the stroke and pushes to the undo stack. This runs even
    // when the ray misses the surface (empty click) — the recorder
    // will simply have no entries and get discarded.
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

    let hit_g = GVec3::new(origin_local.x, origin_local.y, origin_local.z);
    let dir_g = GVec3::new(dir_local.x, dir_local.y, dir_local.z);
    let hit = match workpiece.grid.ray_march(hit_g, dir_g, 4000.0) {
        Some(p) => p,
        None => return,
    };

    let mode = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
        BrushMode::Pull
    } else {
        BrushMode::Press
    };

    // Direction the tool is pushing *into* the surface. Use the local
    // surface normal (SDF gradient) rather than the raw ray direction,
    // so oblique cursor angles still produce natural side-squeeze
    // behaviour rather than a bulge biased toward the viewer.
    //
    // Gradient points *outward* from the workpiece (positive-outside
    // convention), so negate it to get "into the surface". Fall back
    // to the ray direction if the gradient is degenerate.
    let grad = workpiece.grid.gradient_at(hit);
    let into_surface = if grad.length_squared() > 1e-4 {
        -grad.normalize()
    } else {
        dir_g
    };

    // Offset the brush centre each frame so a held button carves
    // progressively:
    // - Press: push slightly further into the surface each frame.
    // - Pull: push slightly back toward the viewer.
    let advance = tool.advance_per_step;
    let center = match mode {
        BrushMode::Press => hit + into_surface * advance,
        BrushMode::Pull => hit - into_surface * advance,
    };

    let brush = SphereBrush {
        center,
        radius: tool.radius,
        mode,
        direction: into_surface,
        // Magic clay is on for Press by default. Pull ignores the flag.
        displace: tool.displace,
        // The workbench is at Y=0 in piece-local coordinates while the
        // piece sits on the workbench with identity turntable rotation.
        // Once the turntable is nontrivial, the piece-local Y axis
        // rotates with the piece, so the workbench needs its own
        // world→piece-local transform. For Stage 1 we clip against the
        // world-space plane by transforming it into piece-local — but
        // in the current setup that's the same as `y = 0` in
        // piece-local, and only *pure* Y-axis turntable rotations are
        // supported, which preserve this. Rework in Stage 2 alongside
        // the "configurable turntable axis" work.
        workbench_y: Some(0.0),
    };

    // Read the grid dimensions before the mutable borrow.
    let grid_res = workpiece.grid.res();

    // Feed the recorder before each voxel is mutated, so pre-stroke
    // values are captured exactly once per voxel. When no stroke is
    // active (which shouldn't happen inside this branch, but guard
    // anyway), we skip the callback overhead entirely.
    let region = if let Some(rec) = stroke.recorder.as_mut() {
        let r = apply_sphere_brush_with_callback(
            &mut workpiece.grid,
            &brush,
            |x, y, z, pre| rec.record_pre_value(x, y, z, pre),
        );
        if let Some(region) = r {
            rec.record_dirty_region(region, grid_res);
        }
        r
    } else {
        apply_sphere_brush_with_callback(&mut workpiece.grid, &brush, |_, _, _, _| {})
    };

    if let Some(region) = region {
        let touched = region.touched_chunks(grid_res);
        for c in touched {
            workpiece.dirty.insert((c.x, c.y, c.z));
        }
    }
}

/// `[` shrinks the brush, `]` grows it. `M` toggles magic-clay
/// displacement so you can feel the difference against pure CSG.
fn adjust_tool_size(keys: Res<ButtonInput<KeyCode>>, mut tool: ResMut<SculptTool>) {
    if keys.just_pressed(KeyCode::BracketLeft) {
        tool.radius = (tool.radius * 0.85).max(2.0);
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        tool.radius = (tool.radius * 1.176).min(60.0);
    }
    if keys.just_pressed(KeyCode::KeyM) {
        tool.displace = !tool.displace;
        bevy::log::info!(
            "magic-clay displacement: {}",
            if tool.displace { "on" } else { "off" }
        );
    }
}
