//! App-level glue for the `rest_components_on_bench` core op.
//!
//! `View → Rest on Bench` (or `Ctrl+G`) drops every floating piece
//! onto the workbench. The core op reports every touched voxel via a
//! callback so we can journal the whole operation as one undo stroke.
//!
//! Selection state (component id caches, active picks) is
//! invalidated because ids reshuffle after any translation.

use bevy::prelude::*;
use sculpt_core::{label_components, rest_components_on_bench};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::selection::Selection;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::LayersState;

pub fn plugin(app: &mut App) {
    app.add_systems(Update, (emit_hotkey, handle_action));
}

fn emit_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    mut actions: EventWriter<AppAction>,
) {
    if ui_gate.keyboard {
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let super_key =
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);
    if (ctrl || super_key) && keys.just_pressed(KeyCode::KeyG) {
        actions.send(AppAction::RestPiecesOnBench);
    }
}

fn handle_action(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<LayersState>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut selection: ResMut<Selection>,
) {
    for a in events.read() {
        if matches!(a, AppAction::RestPiecesOnBench) {
            rest_on_bench_now(
                &mut workpiece,
                &mut history,
                &mut stroke,
                &mut selection,
            );
        }
    }
}

fn rest_on_bench_now(
    workpiece: &mut LayersState,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
    selection: &mut Selection,
) {
    // Abandon any live sculpting stroke — rest is a separate undo unit.
    stroke.discard_live();

    let labels = label_components(workpiece.grid());
    if labels.component_count() == 0 {
        info!("rest: no material on the workbench");
        return;
    }

    let mut recorder = StrokeRecorder::default();
    let summary = rest_components_on_bench(
        workpiece.grid_mut(),
        &labels,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );

    let grid_res = workpiece.grid().res();
    if let Some(region) = summary.dirty {
        recorder.record_dirty_region(region, grid_res);
        for c in region.touched_chunks(grid_res) {
            workpiece.mark_dirty((c.x, c.y, c.z));
        }
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
    }

    // Component ids reshuffle after translation; drop selection cache.
    selection.picked_voxel = None;
    selection.invalidate_labels();

    info!(
        "rest: dropped {} of {} pieces onto the workbench",
        summary.moved, summary.components
    );
}
