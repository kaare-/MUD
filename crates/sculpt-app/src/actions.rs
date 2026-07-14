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

use crate::sculpt::ToolKind;

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
    /// Write the current SDF grid to a timestamped `.mudclay` file
    /// in the working directory.
    SaveProject,
    /// Write the current SDF grid to a specific path (from Save-As).
    SaveProjectAs(PathBuf),
    /// Load the newest `.mudclay` file in the working directory.
    LoadNewestProject,
    /// Load a specific `.mudclay` file (from the Open dialog).
    OpenProject(PathBuf),
    /// Pop up the Save-As dialog. UI-only affordance; keyboard has
    /// Ctrl+Shift+S.
    ShowSaveAsDialog,
    /// Pop up the Open dialog listing every `.mudclay` file in CWD.
    ShowOpenDialog,
    /// Extract a mesh from the current SDF and write a binary STL to
    /// a timestamped path in CWD.
    ExportStl,
    /// Extract a mesh and write a binary STL to a specific path
    /// (from the Export-STL-As dialog).
    ExportStlAs(PathBuf),
    /// Pop up the Export-STL-As dialog. UI-only affordance; keyboard
    /// has Ctrl+Shift+E.
    ShowExportStlDialog,
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
