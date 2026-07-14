//! Move tool — rigidly translate the currently-selected component.
//!
//! The Move tool doesn't sculpt: it applies a signed X/Y/Z
//! millimetre offset to the selected component. The UI layer draws
//! a small numeric widget when the tool is active; this module
//! owns the shared state (`MoveState`) that the widget mutates and
//! the [`AppAction::MoveSelection`] handler that actually shifts
//! voxels.
//!
//! We deliberately treat the move as a *delta* rather than an
//! absolute position: the piece's absolute position doesn't have
//! meaning to a sculptor ("move it 5 mm to the right" reads;
//! "position x = 43.5 mm" doesn't). The HUD widget lets the user
//! either type a delta and Apply, or nudge with the buttons.
//!
//! The heavy lifting — shifting voxel values along with their
//! narrow band, journalling every touched cell for undo — lives in
//! `sculpt_core::translate_component`. This module is glue.

use bevy::prelude::*;
use glam::IVec3;
use sculpt_core::{label_components, translate_component, ChunkCoord};

use crate::actions::AppAction;
use crate::selection::Selection;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::SculptWorkpiece;

/// User-editable pending delta (mm along each axis) that the Move
/// HUD widget in `ui.rs` binds to. `apply` clears it back to zero
/// after each successful move so consecutive nudges accumulate on
/// the piece, not on the widget.
#[derive(Resource, Default)]
pub struct MoveState {
    pub pending_mm: Vec3,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<MoveState>();
    app.add_systems(Update, handle_move_action);
}

fn handle_move_action(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut state: ResMut<MoveState>,
) {
    for a in events.read() {
        if let AppAction::MoveSelection(delta_mm) = a {
            let applied = apply_move(
                *delta_mm,
                &mut workpiece,
                &mut selection,
                &mut history,
                &mut stroke,
            );
            if applied {
                state.pending_mm = Vec3::ZERO;
            }
        }
    }
}

fn apply_move(
    delta_mm: Vec3,
    workpiece: &mut SculptWorkpiece,
    selection: &mut Selection,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) -> bool {
    if selection.picked_voxel.is_none() {
        info!("move: nothing selected");
        return false;
    }
    if delta_mm.length_squared() < 1e-6 {
        return false;
    }
    // Any live stroke is abandoned — the move is its own undo unit.
    stroke.discard_live();

    // Snap the mm delta to a whole-voxel offset. Non-integer voxel
    // moves would resample the SDF and blur the surface; every DCC
    // move here should be lossless.
    let vs = workpiece.grid.voxel_size();
    let delta_vox = IVec3::new(
        (delta_mm.x / vs).round() as i32,
        (delta_mm.y / vs).round() as i32,
        (delta_mm.z / vs).round() as i32,
    );
    if delta_vox == IVec3::ZERO {
        info!(
            "move: delta {:?} mm rounds to zero voxels — bump it up",
            delta_mm
        );
        return false;
    }

    let labels = label_components(&workpiece.grid);
    let id = match selection.selected_id(&labels) {
        Some(id) => id,
        None => {
            info!("move: selected piece has been carved away");
            return false;
        }
    };

    // Track the new picked voxel by shifting the old one. If it
    // ends up outside the grid, drop the selection cache so a
    // future click on the moved piece picks a valid voxel.
    let old_voxel = selection.picked_voxel;

    let mut recorder = StrokeRecorder::default();
    let dirty = translate_component(
        &mut workpiece.grid,
        &labels,
        id,
        delta_vox,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );

    let grid_res = workpiece.grid.res();
    if let Some(region) = dirty {
        recorder.record_dirty_region(region, grid_res);
        for c in region.touched_chunks(grid_res) {
            let ChunkCoord { x, y, z } = c;
            workpiece.dirty.insert((x, y, z));
        }
    }
    if let Some(entry) = recorder.finish(&workpiece.grid) {
        history.push_stroke(entry);
    }

    selection.invalidate_labels();
    selection.picked_voxel = old_voxel.and_then(|(x, y, z)| {
        let nx = x as i32 + delta_vox.x;
        let ny = y as i32 + delta_vox.y;
        let nz = z as i32 + delta_vox.z;
        if nx < 0
            || ny < 0
            || nz < 0
            || nx >= grid_res.x as i32
            || ny >= grid_res.y as i32
            || nz >= grid_res.z as i32
        {
            None
        } else {
            Some((nx as u32, ny as u32, nz as u32))
        }
    });

    info!(
        "moved selection by ({:.1}, {:.1}, {:.1}) mm ({} voxels)",
        delta_mm.x, delta_mm.y, delta_mm.z, delta_vox
    );
    true
}
