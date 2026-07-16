//! App-level glue for rigid rest and plastic settle.
//!
//! - `Ctrl+G` / `Sculpt → Rest pieces on bench` — rigid −Y drop.
//! - `Ctrl+Shift+G` / `Sculpt → Settle (plastic)…` — column-squash
//!   burst (`PLASTIC_GRAVITY.md`).

use bevy::prelude::*;
use sculpt_core::{
    label_components, rest_components_on_bench, settle_components_plastic, PlasticSettleParams,
};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::selection::Selection;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::LayersState;

/// Plasticity slider state for the Settle dialog.
#[derive(Resource)]
pub struct SettleDialogState {
    pub open: bool,
    /// Draft plasticity in the dialog (committed on Settle).
    pub plasticity: f32,
}

impl Default for SettleDialogState {
    fn default() -> Self {
        Self {
            open: false,
            plasticity: 0.7,
        }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<SettleDialogState>();
    app.add_systems(Update, (emit_hotkeys, handle_action));
}

fn emit_hotkeys(
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
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !(ctrl || super_key) || !keys.just_pressed(KeyCode::KeyG) {
        return;
    }
    if shift {
        actions.send(AppAction::ShowSettleDialog);
    } else {
        actions.send(AppAction::RestPiecesOnBench);
    }
}

fn handle_action(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<LayersState>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut selection: ResMut<Selection>,
    mut dialog: ResMut<SettleDialogState>,
) {
    for a in events.read() {
        match a {
            AppAction::RestPiecesOnBench => {
                rest_on_bench_now(
                    &mut workpiece,
                    &mut history,
                    &mut stroke,
                    &mut selection,
                );
            }
            AppAction::ShowSettleDialog => {
                dialog.open = true;
            }
            AppAction::SettlePlastic(p) => {
                dialog.plasticity = p.clamp(0.0, 1.0);
                dialog.open = false;
                settle_plastic_now(
                    &mut workpiece,
                    &mut history,
                    &mut stroke,
                    &mut selection,
                    dialog.plasticity,
                );
            }
            _ => {}
        }
    }
}

fn rest_on_bench_now(
    workpiece: &mut LayersState,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
    selection: &mut Selection,
) {
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

    selection.picked_voxel = None;
    selection.invalidate_labels();

    info!(
        "rest: dropped {} of {} pieces onto the workbench",
        summary.moved, summary.components
    );
}

fn settle_plastic_now(
    workpiece: &mut LayersState,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
    selection: &mut Selection,
    plasticity: f32,
) {
    stroke.discard_live();

    let labels = label_components(workpiece.grid());
    if labels.component_count() == 0 {
        info!("settle: no material on the active layer");
        return;
    }

    let params = PlasticSettleParams {
        plasticity,
        ..PlasticSettleParams::default()
    };
    let mut recorder = StrokeRecorder::default();
    let summary = settle_components_plastic(
        workpiece.grid_mut(),
        &labels,
        &params,
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

    selection.picked_voxel = None;
    selection.invalidate_labels();

    info!(
        "settle: plasticity={:.2}, touched {} voxels over {} iter(s)",
        plasticity, summary.voxels_touched, summary.iterations_run
    );
}
