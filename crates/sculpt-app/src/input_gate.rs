//! Egui ↔ world-input arbitration.
//!
//! Every frame, the UI plugin peeks at egui's context and publishes
//! whether the UI wants the pointer or the keyboard *this* frame.
//! World-input systems (sculpt, camera, turntable, tool-adjust, undo,
//! save/load) check the flags before consuming input, so a click on a
//! menu doesn't accidentally start a sculpting stroke and a keypress
//! inside a hypothetical text field doesn't advance the turntable.
//!
//! Semantics come straight from egui:
//! - `wants_pointer_input()` — egui reports "yes" while the cursor is
//!   over a panel, a menu is open, or a widget is being dragged.
//! - `wants_keyboard_input()` — egui reports "yes" while a text field
//!   has focus (no text fields yet in MUD, but we keep the plumbing
//!   for when Save-As lands).

use bevy::prelude::*;

/// Frame-local snapshot of which input types egui is currently
/// absorbing. Written by the UI plugin's early system, read by every
/// world-input consumer.
///
/// Defaults to "UI wants nothing" so the game runs normally before
/// egui gets a chance to initialise.
#[derive(Resource, Default, Copy, Clone, Debug)]
pub struct UiCapturesInput {
    pub pointer: bool,
    pub keyboard: bool,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<UiCapturesInput>();
}
