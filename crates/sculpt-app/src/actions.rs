//! Cross-cutting UI + keyboard actions.
//!
//! Every user-visible command that a menu might want to expose lives
//! here as an [`AppAction`] variant. Keyboard handlers *and* the egui
//! UI both emit these events; a set of small handler systems in the
//! owning modules (sculpt/project/export/main) consume them and do
//! the actual work.
//!
//! Wiring things this way keeps a single source of truth for what
//! each command does. When we add a "Save As…" file-picker later, the
//! only new code is the picker glue that fires
//! [`AppAction::SaveProject`] — no menu code has to be re-plumbed.

use std::path::PathBuf;

use bevy::prelude::*;

use crate::primitives::PrimitiveShape;
use crate::sculpt::ToolKind;
use crate::view::ViewPreset;

/// A user-triggered command. Emitted by keyboard handlers, UI
/// buttons, or (eventually) OS integrations. Handlers live in the
/// module that owns the affected state.
#[derive(Event, Clone, Debug)]
pub enum AppAction {
    /// Pick up a specific tool from the palette.
    SelectTool(ToolKind),
    /// Flip the mirror-plane symmetry (piece-local X = 0).
    ToggleSymmetry,
    /// Flip magic-clay soft CSG / bulge for the Add/Remove tool.
    ToggleMagicClay,
    /// Reset the workpiece to an empty worktable (no material).
    /// Clears the undo history and any in-flight stroke.
    NewWorkpiece,
    /// Pop up the Insert Primitive dialog.
    ShowInsertPrimitiveDialog,
    /// Union a primitive of the given shape and size (mm) into the
    /// current grid, resting on the workbench, recorded as one undo
    /// stroke.
    InsertPrimitive(PrimitiveShape, f32),
    /// Write the current SDF grid to a timestamped `.mudclay` file
    /// in the working directory.
    SaveProject,
    /// Write the current SDF grid to a specific path (from Save-As).
    SaveProjectAs(PathBuf),
    /// Load the newest `.mudclay` file in the working directory.
    LoadNewestProject,
    /// Load a specific `.mudclay` file (from the Open dialog).
    OpenProject(PathBuf),
    /// Clear the Open Recent MRU list.
    ClearRecentFiles,
    /// Restore the crash-recovery autosave into the worktable.
    RestoreAutosave,
    /// Discard the crash-recovery autosave file.
    DiscardAutosave,
    /// Pop up the Save-As dialog. UI-only affordance; keyboard has
    /// Ctrl+Shift+S.
    ShowSaveAsDialog,
    /// Pop up the Open dialog listing every `.mudclay` file in CWD.
    ShowOpenDialog,
    /// Extract a mesh and write a binary STL to a specific path
    /// (from the Export-STL-As dialog).
    ExportStlAs(PathBuf),
    /// Pop up the Export-STL-As dialog. Fired by both `Ctrl+E` and
    /// the `File > Export STL…` menu item — the two paths are
    /// deliberately identical.
    ShowExportStlDialog,
    /// Remove the currently-selected connected component from the
    /// workpiece. No-op when nothing is selected.
    DeleteSelection,
    /// Flip active-only sculpt gating: when on, sculpting tools only
    /// mutate voxels belonging to the selected component.
    ToggleActiveOnly,
    /// Drop every floating connected component onto the workbench
    /// (rigid gravity — no plastic deformation).
    RestPiecesOnBench,
    /// Snap the currently-selected piece's base to the workbench.
    /// No-op when nothing is selected or the piece is already down.
    SnapSelectionToWorkbench,
    /// Pop up the gravity-settle dialog (softness slider).
    ShowSettleDialog,
    /// Run a gravity-settle burst at the given softness (0..1).
    SettlePlastic(f32),
    /// Flip the workbench grid overlay.
    ToggleWorkbenchGrid,
    /// Explicitly show / hide the workbench grid (Preferences).
    SetWorkbenchGrid(bool),
    /// Pop up Edit → Tool parameters… (size, advance, smooth, etc.).
    ShowToolSettingsDialog,
    /// Pop up Edit → Preferences… (turntable, grid, settle defaults).
    ShowPreferencesDialog,
    /// Snap the orbit camera to a standard preset (Top / Front /
    /// Left / Right / Back / Bottom / Perspective). Distance and
    /// target are preserved.
    SetView(ViewPreset),
    /// Save the current orbit camera as a named bookmark.
    SaveCameraBookmark,
    /// Restore bookmark at index in the bookmarks list.
    RestoreCameraBookmark(usize),
    /// Clear every saved camera bookmark.
    ClearCameraBookmarks,
    /// Rigidly translate the selected component by `(dx, dy, dz)`
    /// millimetres. No-op when nothing is selected.
    MoveSelection(Vec3),
    /// Make layer `index` active (tools read/write that layer).
    SetActiveLayer(usize),
    /// Show / hide layer `index` (remesh, pick, and export skip hidden).
    SetLayerVisible(usize, bool),
    /// Delete layer `index` (refuses the last remaining layer).
    DeleteLayer(usize),
    /// Merge the active layer into the one below it (Photoshop
    /// Merge Down). No-op when the active layer is already at the
    /// bottom of the stack.
    MergeDown,
    /// Cleanly shut down the app (equivalent to `AppExit::Success`).
    Quit,
}

pub fn plugin(app: &mut App) {
    app.add_event::<AppAction>();
    app.add_systems(Update, handle_quit);
}

fn handle_quit(mut events: EventReader<AppAction>, mut ev: EventWriter<AppExit>) {
    for a in events.read() {
        if matches!(a, AppAction::Quit) {
            ev.send(AppExit::Success);
        }
    }
}
