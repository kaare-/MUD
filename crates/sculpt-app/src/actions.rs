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
    /// Flip the finger's magic-clay volume-displacement mode.
    ToggleMagicClay,
    /// Write the current SDF grid to a timestamped `.mudclay` file.
    SaveProject,
    /// Load the newest `.mudclay` file in the working directory.
    LoadNewestProject,
    /// Extract a mesh from the current SDF and write a binary STL.
    ExportStl,
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
