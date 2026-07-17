//! App-wide preferences and dialog open-state.
//!
//! Tool knobs live on [`crate::sculpt::SculptTool`] / symmetry resources;
//! this module owns the slower-changing software settings (turntable
//! speed, default settle plasticity) plus the Edit-menu dialog flags.

use bevy::prelude::*;

use crate::actions::AppAction;
use crate::matcap::MatcapPreset;

/// General software preferences (Edit → Preferences…).
#[derive(Resource, Clone)]
pub struct AppSettings {
    /// Seconds for one full turntable revolution while Q or E is held.
    pub turntable_period_secs: f32,
    /// Softness pre-filled when opening Settle (gravity)….
    pub default_plasticity: f32,
    /// Clay matcap preset (View → Matcap / Preferences).
    pub matcap: MatcapPreset,
    /// How strongly crevice darkening (SDF cavity) is applied, `0..=1`.
    pub cavity_strength: f32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            // Matches the previous hard-coded ~6.5 s / rev.
            turntable_period_secs: 6.5,
            default_plasticity: 0.7,
            matcap: MatcapPreset::Clay,
            cavity_strength: 0.65,
        }
    }
}

/// Which settings windows are open.
#[derive(Resource, Default)]
pub struct SettingsDialogState {
    pub tool_open: bool,
    pub prefs_open: bool,
}

impl SettingsDialogState {
    pub fn any_open(&self) -> bool {
        self.tool_open || self.prefs_open
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<AppSettings>();
    app.init_resource::<SettingsDialogState>();
    app.add_systems(Update, handle_settings_actions);
}

fn handle_settings_actions(
    mut events: EventReader<AppAction>,
    mut dialogs: ResMut<SettingsDialogState>,
    mut settings: ResMut<AppSettings>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    for a in events.read() {
        match a {
            AppAction::ShowToolSettingsDialog => {
                dialogs.tool_open = true;
                dialogs.prefs_open = false;
            }
            AppAction::ShowPreferencesDialog => {
                dialogs.prefs_open = true;
                dialogs.tool_open = false;
            }
            AppAction::SetMatcap(preset) => {
                settings.matcap = *preset;
            }
            _ => {}
        }
    }

    // Escape closes settings windows. `publish_ui_capture` marks the
    // keyboard as UI-owned while either dialog is open so `esc_quit`
    // does not also fire Quit on the same keypress.
    if keys.just_pressed(KeyCode::Escape) && dialogs.any_open() {
        dialogs.tool_open = false;
        dialogs.prefs_open = false;
    }
}
