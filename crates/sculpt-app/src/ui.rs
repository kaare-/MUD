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

use std::path::PathBuf;

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPlugin};

use crate::actions::AppAction;
use crate::export::timestamped_filename;
use crate::input_gate::UiCapturesInput;
use crate::move_tool::MoveState;
use crate::primitives::{PrimitiveDialogState, PrimitiveShape};
use crate::project::FileDialogState;
use crate::sculpt::{tool_label, CutterFamily, SculptSymmetry, SculptTool, ToolKind};
use crate::selection::Selection;
use crate::view::{ViewPreset, WorkbenchGridState};
use crate::workpiece::SculptWorkpiece;

pub fn plugin(app: &mut App) {
    app.add_plugins(EguiPlugin);
    app.add_systems(
        Update,
        (
            draw_ui,
            draw_dialogs.after(draw_ui),
            publish_ui_capture.after(draw_dialogs),
        )
            .chain(),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_ui(
    mut contexts: EguiContexts,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    selection: Res<Selection>,
    workpiece: Res<SculptWorkpiece>,
    grid_state: Res<WorkbenchGridState>,
    mut move_state: ResMut<MoveState>,
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
                if menu_item(ui, "New", "Ctrl+N") {
                    actions.send(AppAction::NewWorkpiece);
                    ui.close_menu();
                }
                if menu_item(ui, "Insert Primitive\u{2026}", "Shift+N") {
                    actions.send(AppAction::ShowInsertPrimitiveDialog);
                    ui.close_menu();
                }
                ui.separator();
                if menu_item(ui, "Save", "Ctrl+S") {
                    actions.send(AppAction::SaveProject);
                    ui.close_menu();
                }
                if menu_item(ui, "Save As\u{2026}", "Ctrl+Shift+S") {
                    actions.send(AppAction::ShowSaveAsDialog);
                    ui.close_menu();
                }
                ui.separator();
                if menu_item(ui, "Open\u{2026}", "Ctrl+O") {
                    actions.send(AppAction::ShowOpenDialog);
                    ui.close_menu();
                }
                if menu_item(ui, "Reopen most recent", "Ctrl+Shift+O") {
                    actions.send(AppAction::LoadNewestProject);
                    ui.close_menu();
                }
                ui.separator();
                if menu_item(ui, "Export STL\u{2026}", "Ctrl+E") {
                    actions.send(AppAction::ShowExportStlDialog);
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
            ui.menu_button("Sculpt", |ui| {
                if menu_item(ui, "Insert Primitive\u{2026}", "Shift+N") {
                    actions.send(AppAction::ShowInsertPrimitiveDialog);
                    ui.close_menu();
                }
                ui.separator();
                let has_selection = selection.picked_voxel.is_some();
                if ui
                    .add_enabled(
                        has_selection,
                        egui::Button::new("Delete selection")
                            .shortcut_text("Del")
                            .min_size(egui::vec2(220.0, 0.0)),
                    )
                    .clicked()
                {
                    actions.send(AppAction::DeleteSelection);
                    ui.close_menu();
                }
                let active_label = if selection.active_only {
                    "Active-only sculpt ✓"
                } else {
                    "Active-only sculpt"
                };
                if menu_item(ui, active_label, "A") {
                    actions.send(AppAction::ToggleActiveOnly);
                    ui.close_menu();
                }
                ui.separator();
                if menu_item(ui, "Rest pieces on bench", "Ctrl+G") {
                    actions.send(AppAction::RestPiecesOnBench);
                    ui.close_menu();
                }
            });
            ui.menu_button("View", |ui| {
                let grid_label = if grid_state.visible {
                    "Workbench grid ✓"
                } else {
                    "Workbench grid"
                };
                if menu_item(ui, grid_label, "") {
                    actions.send(AppAction::ToggleWorkbenchGrid);
                    ui.close_menu();
                }
                ui.separator();
                for preset in [
                    ViewPreset::Perspective,
                    ViewPreset::Top,
                    ViewPreset::Bottom,
                    ViewPreset::Front,
                    ViewPreset::Back,
                    ViewPreset::Left,
                    ViewPreset::Right,
                ] {
                    if menu_item(ui, preset.label(), "") {
                        actions.send(AppAction::SetView(preset));
                        ui.close_menu();
                    }
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
            for (digit, kind) in tool_palette_order() {
                let selected = tool.kind == kind;
                let label = format!("{digit}  {}", short_label(kind));
                if ui.selectable_label(selected, label).clicked() {
                    actions.send(AppAction::SelectTool(kind));
                }
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.small(tool_palette_hint(tool.kind));
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
            // Magic clay only affects Add/Remove; grey the button out
            // for tools it doesn't apply to so users don't wonder why
            // toggling it changed nothing.
            let mc_relevant = matches!(tool.kind, ToolKind::Clay);
            ui.add_enabled_ui(mc_relevant, |ui| {
                if ui.selectable_label(tool.displace, mc_text).clicked() {
                    actions.send(AppAction::ToggleMagicClay);
                }
            });
            ui.separator();
            // Selection HUD: current pick, delete, active-only.
            draw_selection_hud(ui, &selection, &workpiece, &mut actions);
            ui.separator();
            ui.small("Ctrl+Z / Ctrl+Y — undo / redo");
        });
    });

    // Move widget. Only shown when the Move tool is active — the
    // widget is a floating egui window anchored to the top-right
    // corner of the viewport (out of the sculpt path). Compact
    // X/Y/Z spinners in mm plus an Apply button.
    if matches!(tool.kind, ToolKind::Move) {
        draw_move_widget(ctx, &selection, &mut move_state, &mut actions);
    }
}

/// Compact `⟨ X | Y | Z ⟩` translate widget. Reads / writes the
/// pending delta on [`MoveState`]; Apply fires
/// [`AppAction::MoveSelection`], which the `move_tool` module
/// handles. Nothing about the piece's absolute position is shown
/// — a sculptor thinks in nudges, not coordinates.
fn draw_move_widget(
    ctx: &egui::Context,
    selection: &Selection,
    state: &mut MoveState,
    actions: &mut EventWriter<AppAction>,
) {
    let has_selection = selection.picked_voxel.is_some();
    egui::Window::new("Move")
        .anchor(egui::Align2::RIGHT_TOP, [-12.0, 44.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            if has_selection {
                ui.label("Nudge selection (mm):");
            } else {
                ui.small("Pick a piece first (LMB).");
                ui.small("Or Insert Primitive to auto-select.");
            }
            ui.horizontal(|ui| {
                ui.label("X");
                ui.add(
                    egui::DragValue::new(&mut state.pending_mm.x)
                        .speed(0.5)
                        .suffix(" mm"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Y");
                ui.add(
                    egui::DragValue::new(&mut state.pending_mm.y)
                        .speed(0.5)
                        .suffix(" mm"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Z");
                ui.add(
                    egui::DragValue::new(&mut state.pending_mm.z)
                        .speed(0.5)
                        .suffix(" mm"),
                );
            });
            ui.horizontal(|ui| {
                let apply = ui
                    .add_enabled(has_selection, egui::Button::new("Apply"))
                    .clicked();
                if ui.button("Reset").clicked() {
                    state.pending_mm = Vec3::ZERO;
                }
                if apply {
                    actions.send(AppAction::MoveSelection(state.pending_mm));
                }
            });
        });
}

/// Selection segment of the status strip. Shows the picked component
/// (or "no selection"), a Delete button, and the Active-only toggle.
fn draw_selection_hud(
    ui: &mut egui::Ui,
    selection: &Selection,
    workpiece: &SculptWorkpiece,
    actions: &mut EventWriter<AppAction>,
) {
    let vs = workpiece.grid.voxel_size();
    let (text, has_selection) = match selection.labels().and_then(|labels| {
        selection
            .selected_id(labels)
            .map(|id| (id, labels.voxel_count(id), labels.volume_mm3(id, vs)))
    }) {
        Some((id, count, mm3)) => (
            format!("Sel #{id}: {count} vx · {mm3:.0} mm³"),
            true,
        ),
        None => ("Selection: none".to_string(), false),
    };
    ui.label(text);
    ui.add_enabled_ui(has_selection, |ui| {
        if ui.button("Delete [Del]").clicked() {
            actions.send(AppAction::DeleteSelection);
        }
    });
    let active_text = if selection.active_only {
        "Active only: on [A]"
    } else {
        "Active only: off [A]"
    };
    if ui
        .selectable_label(selection.active_only, active_text)
        .clicked()
    {
        actions.send(AppAction::ToggleActiveOnly);
    }
}

/// Save-As and Open modal dialogs, when their state is populated.
///
/// Both dialogs are drawn as centred `egui::Window`s. While either
/// is up, the `publish_ui_capture` snapshot marks pointer + keyboard
/// as UI-owned so background clicks and Escape don't leak through.
///
/// - **Save As** shows a text field pre-filled with a timestamped
///   filename, plus Save / Cancel. Enter in the field saves. Any
///   filename without an extension gets `.mudclay` appended.
/// - **Open** shows the list of `.mudclay` files in the CWD (newest
///   first by mtime), plus Open / Cancel. Single click selects,
///   double click opens.
fn draw_dialogs(
    mut contexts: EguiContexts,
    mut state: ResMut<FileDialogState>,
    mut prim_state: ResMut<PrimitiveDialogState>,
    mut actions: EventWriter<AppAction>,
) {
    let ctx = contexts.ctx_mut();

    // Save-As dialog. Egui doesn't have a first-class modal concept,
    // so we anchor to the centre, disable resize/collapse, and rely
    // on `publish_ui_capture` to suppress world input.
    if state.save_as.is_some() {
        let mut done = None; // Some(Ok(path)) = save, Some(Err) = cancel
        if let Some(dialog) = state.save_as.as_mut() {
            egui::Window::new("Save As")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Filename (blank = timestamped):");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut dialog.name)
                            .desired_width(280.0)
                            .hint_text("wolf-head-v3"),
                    );
                    // Enter = commit. First frame the dialog opens
                    // we want focus in the field so the user can
                    // just type. On subsequent frames the resp keeps
                    // its own focus; requesting again is a no-op.
                    let submit_via_enter =
                        resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if !resp.has_focus() {
                        resp.request_focus();
                    }
                    ui.small(
                        "Saves into the working directory. Extension\n\
                         .mudclay is added automatically. Leaving the\n\
                         field blank saves with a fresh timestamp.",
                    );
                    let save_clicked = ui.horizontal(|ui| {
                        let save = ui.button("Save").clicked();
                        let cancel = ui.button("Cancel").clicked();
                        (save, cancel)
                    });
                    let (save, cancel) = save_clicked.inner;
                    if save || submit_via_enter {
                        done = Some(Ok(finalise_save_path(&dialog.name)));
                    } else if cancel {
                        done = Some(Err(()));
                    }
                });
        }
        match done {
            Some(Ok(path)) => {
                actions.send(AppAction::SaveProjectAs(path));
                state.save_as = None;
            }
            Some(Err(())) => {
                state.save_as = None;
            }
            None => {}
        }
    }

    // Insert Primitive dialog. Combo box for shape + a slider for
    // size (mm). Insert emits `InsertPrimitive`; Cancel closes.
    if prim_state.open.is_some() {
        let mut result: Option<Option<(PrimitiveShape, f32)>> = None;
        if let Some(dialog) = prim_state.open.as_mut() {
            egui::Window::new("Insert Primitive")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Shape:");
                        egui::ComboBox::from_id_salt("mud_primitive_shape")
                            .selected_text(dialog.shape.label())
                            .show_ui(ui, |ui| {
                                for s in [
                                    PrimitiveShape::Sphere,
                                    PrimitiveShape::Cube,
                                    PrimitiveShape::Cylinder,
                                    PrimitiveShape::Torus,
                                ] {
                                    ui.selectable_value(&mut dialog.shape, s, s.label());
                                }
                            });
                    });
                    ui.add(
                        egui::Slider::new(&mut dialog.size_mm, 2.0..=50.0)
                            .suffix(" mm")
                            .text("Size"),
                    );
                    ui.small(
                        "Placed centred on the workbench.\n\
                         Torus size is the ring radius; tube is 0.35× size.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Insert").clicked() {
                            result = Some(Some((dialog.shape, dialog.size_mm)));
                        }
                        if ui.button("Cancel").clicked() {
                            result = Some(None);
                        }
                    });
                });
        }
        if let Some(res) = result {
            if let Some((shape, size)) = res {
                actions.send(AppAction::InsertPrimitive(shape, size));
            }
            prim_state.open = None;
        }
    }

    // Export-STL-As dialog. Same shape as Save-As but the finaliser
    // enforces the `.stl` extension.
    if state.export_stl.is_some() {
        let mut done: Option<Result<PathBuf, ()>> = None;
        if let Some(dialog) = state.export_stl.as_mut() {
            egui::Window::new("Export STL As")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Filename (blank = timestamped):");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut dialog.name)
                            .desired_width(280.0)
                            .hint_text("wolf-head-v3"),
                    );
                    let submit_via_enter =
                        resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if !resp.has_focus() {
                        resp.request_focus();
                    }
                    ui.small(
                        "Exports into the working directory. Extension\n\
                         .stl is added automatically. Leaving the field\n\
                         blank exports with a fresh timestamp.",
                    );
                    let clicked = ui.horizontal(|ui| {
                        let save = ui.button("Export").clicked();
                        let cancel = ui.button("Cancel").clicked();
                        (save, cancel)
                    });
                    let (save, cancel) = clicked.inner;
                    if save || submit_via_enter {
                        done = Some(Ok(finalise_export_path(&dialog.name)));
                    } else if cancel {
                        done = Some(Err(()));
                    }
                });
        }
        match done {
            Some(Ok(path)) => {
                actions.send(AppAction::ExportStlAs(path));
                state.export_stl = None;
            }
            Some(Err(())) => {
                state.export_stl = None;
            }
            None => {}
        }
    }

    // Open dialog.
    if state.open.is_some() {
        let mut done: Option<Option<PathBuf>> = None;
        if let Some(dialog) = state.open.as_mut() {
            egui::Window::new("Open project")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(360.0)
                .show(ctx, |ui| {
                    if dialog.files.is_empty() {
                        ui.label("No .mudclay files in this directory.");
                    } else {
                        ui.label(format!(
                            "{} projects (newest first):",
                            dialog.files.len()
                        ));
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for (path, _mtime) in &dialog.files {
                                    let name = path
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("?");
                                    let selected =
                                        dialog.selected.as_deref() == Some(path.as_path());
                                    let resp = ui.selectable_label(selected, name);
                                    if resp.clicked() {
                                        dialog.selected = Some(path.clone());
                                    }
                                    if resp.double_clicked() {
                                        done = Some(Some(path.clone()));
                                    }
                                }
                            });
                    }
                    ui.horizontal(|ui| {
                        let can_open = dialog.selected.is_some();
                        if ui
                            .add_enabled(can_open, egui::Button::new("Open"))
                            .clicked()
                        {
                            done = Some(dialog.selected.clone());
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(None);
                        }
                    });
                });
        }
        if let Some(result) = done {
            if let Some(path) = result {
                actions.send(AppAction::OpenProject(path));
            }
            state.open = None;
        }
    }
}

/// Fold "user typed some name" into a canonical PathBuf.
///
/// - Empty (or whitespace-only) input falls back to a fresh
///   timestamped filename, so pressing Enter with nothing typed
///   still saves.
/// - Otherwise: trim whitespace, and add `.mudclay` if the path
///   doesn't already end with `.mudclay`. We check the whole suffix
///   rather than `path.extension().is_none()` because names like
///   `wolf.v3` or `mudclayawolf-head` have an "extension" that
///   isn't `.mudclay`; we want those to save as `wolf.v3.mudclay`
///   or `mudclayawolf-head.mudclay`, not to silently keep whatever
///   suffix the user typed and become non-loadable.
fn finalise_save_path(input: &str) -> PathBuf {
    finalise_typed_path(input, "mudclay")
}

/// Export-STL variant of `finalise_save_path` — same rules with the
/// `.stl` extension.
fn finalise_export_path(input: &str) -> PathBuf {
    finalise_typed_path(input, "stl")
}

fn finalise_typed_path(input: &str, ext: &str) -> PathBuf {
    let trimmed = input.trim();
    let dot_ext = format!(".{ext}");
    if trimmed.is_empty() {
        return PathBuf::from(timestamped_filename("mud-sculpt-", &dot_ext));
    }
    if trimmed.ends_with(&dot_ext) {
        PathBuf::from(trimmed)
    } else {
        PathBuf::from(format!("{trimmed}{dot_ext}"))
    }
}

/// After egui has processed inputs for the frame, snapshot whether
/// the UI is absorbing pointer or keyboard events. World-input
/// systems consult this resource before consuming input themselves.
///
/// Two subtle bits:
/// - `is_pointer_over_area()` covers hover-only cases like tooltip
///   arming — egui hasn't consumed the pointer, but the sculpt path
///   still shouldn't fire.
/// - An **open menu** doesn't set `wants_keyboard_input()` (egui only
///   flags that for focused text edits), but Escape while a menu is
///   open should absolutely close the menu rather than quit the app.
///   We check `memory.any_popup_open()` and treat that as "keyboard
///   is in UI-land" too, so Escape (and future menu-navigation keys)
///   don't leak through to `esc_quit`.
/// - When a **file-dialog window** is on screen, the whole app should
///   feel modal: clicks anywhere shouldn't sculpt, and shortcut keys
///   shouldn't move the turntable while the user's typing a filename.
///   We check the `FileDialogState` resource directly for this.
fn publish_ui_capture(
    mut contexts: EguiContexts,
    dialogs: Res<FileDialogState>,
    prim: Res<PrimitiveDialogState>,
    mut gate: ResMut<UiCapturesInput>,
) {
    let ctx = contexts.ctx_mut();
    let modal = dialogs.any_open() || prim.open.is_some();
    gate.pointer = modal || ctx.wants_pointer_input() || ctx.is_pointer_over_area();
    gate.keyboard =
        modal || ctx.wants_keyboard_input() || ctx.memory(|m| m.any_popup_open());
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
/// keyboard mapping in `sculpt::adjust_tool`. `0` is the Move tool
/// so the digit row reads "1..9" for stamps and "0" for the rigid
/// transform — same convention as most DCC tool palettes.
fn tool_palette_order() -> [(&'static str, ToolKind); 10] {
    [
        ("1", ToolKind::Clay),
        ("2", ToolKind::Cutter(CutterFamily::Circle)),
        ("3", ToolKind::Cutter(CutterFamily::Square)),
        ("4", ToolKind::Cutter(CutterFamily::Hexagon)),
        ("5", ToolKind::Cutter(CutterFamily::Star5)),
        ("6", ToolKind::WireCutter),
        ("7", ToolKind::Smooth),
        ("8", ToolKind::Paddle),
        ("9", ToolKind::Select),
        ("0", ToolKind::Move),
    ]
}

/// Short display label for a tool in the palette. Different from
/// `sculpt::tool_label`, which is optimised for log lines
/// (`"cutter/circle"`).
fn short_label(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Clay => "Add/Remove",
        ToolKind::Cutter(CutterFamily::Circle) => "Circle cutter",
        ToolKind::Cutter(CutterFamily::Square) => "Square cutter",
        ToolKind::Cutter(CutterFamily::Hexagon) => "Hex cutter",
        ToolKind::Cutter(CutterFamily::Star5) => "Star cutter",
        ToolKind::WireCutter => "Wire cutter",
        ToolKind::Smooth => "Smooth",
        ToolKind::Paddle => "Paddle",
        ToolKind::Select => "Select",
        ToolKind::Move => "Move",
    }
}

/// Palette hint text tailored to the current tool. Kept short and
/// physical — no jargon — so users can learn a tool by using it.
fn tool_palette_hint(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Clay => {
            "LMB remove · Shift+LMB add\n\
             Shift+LMB on an empty bench builds up.\n\
             Shift+scroll or [ / ] resize.\n\
             Right-drag orbits · Q / E turntable.\n\
             Sideways add + Q/E draws a ring."
        }
        ToolKind::Cutter(_) => {
            "Click punches a hole.\n\
             Shift+scroll or [ / ] resize.\n\
             Right-drag orbits · Q / E turntable."
        }
        ToolKind::WireCutter => {
            "Drag through the workpiece to slice.\n\
             Release completes the cut."
        }
        ToolKind::Smooth => {
            "Hold LMB to polish detail.\n\
             Shift+scroll or [ / ] resize."
        }
        ToolKind::Paddle => {
            "Hold LMB to press a flat.\n\
             Shift+scroll or [ / ] resize."
        }
        ToolKind::Select => {
            "LMB picks the piece under the cursor.\n\
             The selected piece is boxed in yellow.\n\
             Del  removes the selected piece.\n\
             A    active-only sculpt (other pieces stay).\n\
             Ctrl+G  drop every floating piece."
        }
        ToolKind::Move => {
            "Pick a piece (LMB) or use the current selection,\n\
             then type the X / Y / Z nudge (mm) and Apply.\n\
             Values snap to whole voxels."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_falls_back_to_timestamp() {
        let p = finalise_save_path("");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("mud-sculpt-"));
        assert!(name.ends_with(".mudclay"));
        // Whitespace-only counts as empty.
        let ws = finalise_save_path("   \t");
        assert!(ws
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("mud-sculpt-"));
    }

    #[test]
    fn extension_is_appended_when_missing() {
        assert_eq!(finalise_save_path("wolf-head"), PathBuf::from("wolf-head.mudclay"));
        assert_eq!(finalise_save_path("  spaces  "), PathBuf::from("spaces.mudclay"));
    }

    #[test]
    fn extension_is_kept_when_already_correct() {
        assert_eq!(finalise_save_path("wolf.mudclay"), PathBuf::from("wolf.mudclay"));
    }

    #[test]
    fn export_path_appends_stl_extension() {
        assert_eq!(
            finalise_export_path("wolf-head"),
            PathBuf::from("wolf-head.stl"),
        );
        assert_eq!(
            finalise_export_path("wolf.stl"),
            PathBuf::from("wolf.stl"),
        );
        let empty = finalise_export_path("");
        assert!(empty
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("mud-sculpt-"));
        assert!(empty.extension().unwrap() == "stl");
    }

    #[test]
    fn non_mudclay_extension_is_treated_as_stem_and_appended() {
        // The pre-fix behaviour let names like "wolf.v3" through
        // unmodified, producing files unloadable as projects. Now
        // the whole-suffix check catches them.
        assert_eq!(
            finalise_save_path("wolf.v3"),
            PathBuf::from("wolf.v3.mudclay"),
        );
        assert_eq!(
            finalise_save_path("mud-sculpt-20260714-104649.mudclayawolf-head"),
            PathBuf::from("mud-sculpt-20260714-104649.mudclayawolf-head.mudclay"),
        );
    }
}
