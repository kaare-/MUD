//! Egui-based menus, tool palette, and status strip.
//!
//! Adds three panels around the 3D viewport:
//!
//! - **Top**: menu bar with a `File` dropdown (Save / Load / Export
//!   STL / Quit). Shortcut hints inline so keyboard-inclined users
//!   learn the shortcuts by using the menu.
//! - **Left**: tool palette — one button per [`ToolKind`], current
//!   tool highlighted. Includes the 1–8 shortcut in the label so the
//!   two paths are self-teaching.
//! - **Bottom**: status strip showing current tool size (`mm`),
//!   symmetry state, and magic-clay state. The two toggles are
//!   themselves buttons.
//!
//! Every UI action fires the same [`AppAction`] events the keyboard
//! path emits, so the "what does this do?" answer is in one place
//! per feature (the module that owns the affected state).
//!
//! The panels also publish an [`UiCapturesInput`] snapshot every
//! frame so world-input systems know when to yield.

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPlugin};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::sculpt::{tool_label, CutterFamily, SculptSymmetry, SculptTool, ToolKind};

pub fn plugin(app: &mut App) {
    app.add_plugins(EguiPlugin);
    app.add_systems(
        Update,
        (draw_ui, publish_ui_capture.after(draw_ui)).chain(),
    );
}

fn draw_ui(
    mut contexts: EguiContexts,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    mut actions: EventWriter<AppAction>,
    mut app_exit: EventWriter<AppExit>,
) {
    let ctx = contexts.ctx_mut();

    // Top menu bar. `File > Save / Load / Export STL / Quit`, each
    // with its keyboard shortcut on the right. Room for `Edit`,
    // `View`, etc. later.
    egui::TopBottomPanel::top("mud_menu_bar").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if menu_item(ui, "Save", "Ctrl+S") {
                    actions.send(AppAction::SaveProject);
                    ui.close_menu();
                }
                if menu_item(ui, "Load newest", "Ctrl+O") {
                    actions.send(AppAction::LoadNewestProject);
                    ui.close_menu();
                }
                ui.separator();
                if menu_item(ui, "Export STL", "Ctrl+E") {
                    actions.send(AppAction::ExportStl);
                    ui.close_menu();
                }
                ui.separator();
                if menu_item(ui, "Quit", "Esc") {
                    // Quit is a special case: the AppAction handler in
                    // `actions::plugin` fires AppExit, but a menu click
                    // typically expects the app to close *this frame*.
                    // We send AppExit directly here and also emit the
                    // event so log-and-metric consumers see it.
                    app_exit.send(AppExit::Success);
                    actions.send(AppAction::Quit);
                }
            });
            ui.separator();
            ui.label(
                egui::RichText::new(format!("Tool: {}", tool_label(tool.kind)))
                    .color(egui::Color32::from_gray(180)),
            );
        });
    });

    // Left tool palette. One row per tool with its number-key
    // shortcut. Selected tool is highlighted.
    egui::SidePanel::left("mud_tool_palette")
        .resizable(false)
        .exact_width(160.0)
        .show(ctx, |ui| {
            ui.heading("Tools");
            ui.add_space(4.0);
            for (n, kind) in tool_palette_order() {
                let selected = tool.kind == kind;
                let label = format!("{n}  {}", short_label(kind));
                if ui.selectable_label(selected, label).clicked() {
                    actions.send(AppAction::SelectTool(kind));
                }
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.small(
                "Shift+scroll or [ / ]\nto resize the active tool.\n\
                 Right-drag orbits the camera.\n\
                 Q / E turntables left / right.",
            );
        });

    // Bottom status strip. Size (read-only readout — the number is
    // driven by the wheel/keys), symmetry (toggle button), magic-clay
    // (toggle button). Keeping toggles here means the state is
    // *always visible* rather than living in a menu.
    egui::TopBottomPanel::bottom("mud_status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!("Size: {:.1} mm", tool.size));
            ui.separator();
            let sym_text = if symmetry.enabled {
                "Symmetry: on"
            } else {
                "Symmetry: off"
            };
            if ui.selectable_label(symmetry.enabled, sym_text).clicked() {
                actions.send(AppAction::ToggleSymmetry);
            }
            let mc_text = if tool.displace {
                "Magic clay: on"
            } else {
                "Magic clay: off"
            };
            // Magic clay only affects the finger; grey the button out
            // for tools it doesn't apply to so users don't wonder why
            // toggling it changed nothing.
            let mc_relevant = matches!(tool.kind, ToolKind::Finger);
            ui.add_enabled_ui(mc_relevant, |ui| {
                if ui.selectable_label(tool.displace, mc_text).clicked() {
                    actions.send(AppAction::ToggleMagicClay);
                }
            });
            ui.separator();
            ui.small("Ctrl+Z / Ctrl+Y — undo / redo");
        });
    });
}

/// After egui has processed inputs for the frame, snapshot whether
/// the UI is absorbing pointer or keyboard events. World-input
/// systems consult this resource before consuming input themselves.
fn publish_ui_capture(mut contexts: EguiContexts, mut gate: ResMut<UiCapturesInput>) {
    let ctx = contexts.ctx_mut();
    gate.pointer = ctx.wants_pointer_input() || ctx.is_pointer_over_area();
    gate.keyboard = ctx.wants_keyboard_input();
}

/// A single menu item with a right-aligned shortcut hint. Egui's
/// built-in `Button` supports this via `Button::new(...).shortcut_text(...)`
/// but wrapping it here keeps every File-menu row visually consistent.
fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> bool {
    ui.add(
        egui::Button::new(label)
            .shortcut_text(shortcut)
            .min_size(egui::vec2(180.0, 0.0)),
    )
    .clicked()
}

/// Palette order + number-key shortcut. Kept in sync with the
/// keyboard mapping in `sculpt::adjust_tool`.
fn tool_palette_order() -> [(u8, ToolKind); 8] {
    [
        (1, ToolKind::Finger),
        (2, ToolKind::Cutter(CutterFamily::Circle)),
        (3, ToolKind::Cutter(CutterFamily::Square)),
        (4, ToolKind::Cutter(CutterFamily::Hexagon)),
        (5, ToolKind::Cutter(CutterFamily::Star5)),
        (6, ToolKind::WireCutter),
        (7, ToolKind::Smooth),
        (8, ToolKind::Paddle),
    ]
}

/// Short display label for a tool in the palette. Different from
/// `sculpt::tool_label`, which is optimised for log lines
/// (`"cutter/circle"`).
fn short_label(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Finger => "Finger",
        ToolKind::Cutter(CutterFamily::Circle) => "Circle cutter",
        ToolKind::Cutter(CutterFamily::Square) => "Square cutter",
        ToolKind::Cutter(CutterFamily::Hexagon) => "Hex cutter",
        ToolKind::Cutter(CutterFamily::Star5) => "Star cutter",
        ToolKind::WireCutter => "Wire cutter",
        ToolKind::Smooth => "Smooth",
        ToolKind::Paddle => "Paddle",
    }
}
