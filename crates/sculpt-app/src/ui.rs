//! Egui-based menus, tool palette, layers panel, and status strip.
//!
//! Adds panels around the 3D viewport:
//!
//! - **Top**: menu bar with `File`, `Edit` (tool parameters /
//!   preferences), `Sculpt`, and `View`. Shortcut hints inline so
//!   keyboard-inclined users learn the shortcuts by using the menu.
//! - **Left**: tool palette — one button per [`ToolKind`], current
//!   tool highlighted. Includes the 1–8 shortcut in the label so the
//!   two paths are self-teaching.
//! - **Right**: Layers panel (Track B3) — name, active indicator,
//!   visibility, delete, Merge Down.
//! - **Bottom**: status strip showing current tool size (`mm`),
//!   symmetry state, magic-clay state, and active layer.
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
use crate::export::{timestamped_filename, ExportNotice};
use crate::gravity::SettleDialogState;
use crate::input_gate::UiCapturesInput;
use crate::move_tool::MoveState;
use crate::primitives::{PrimitiveDialogState, PrimitiveShape};
use crate::project::{AutosaveState, FileDialogState, RecentFiles};
use sculpt_core::MesherKind;

use crate::pen::PenState;
use crate::sculpt::{
    tool_label, CutterFamily, SculptSymmetry, SculptTool, ToolKind, SIZE_MAX, SIZE_MIN,
};
use crate::selection::Selection;
use crate::settings::{AppSettings, SettingsDialogState};
use crate::matcap::MatcapPreset;
use crate::view::{CameraBookmarks, ViewPreset, WorkbenchGridState};
use crate::workpiece::LayersState;

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
    pen: Res<PenState>,
    selection: Res<Selection>,
    mut workpiece: ResMut<LayersState>,
    grid_state: Res<WorkbenchGridState>,
    mut move_state: ResMut<MoveState>,
    recent: Res<RecentFiles>,
    bookmarks: Res<CameraBookmarks>,
    app_settings: Res<AppSettings>,
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
                ui.menu_button("Open Recent", |ui| {
                    let entries: Vec<PathBuf> = recent.paths().to_vec();
                    let mut shown = 0usize;
                    for path in &entries {
                        if !path.exists() {
                            continue;
                        }
                        shown += 1;
                        let label = path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        if ui
                            .add(egui::Button::new(label).min_size(egui::vec2(180.0, 0.0)))
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            actions.send(AppAction::OpenProject(path.clone()));
                            ui.close_menu();
                        }
                    }
                    if shown == 0 {
                        ui.add_enabled(false, egui::Button::new("(empty)"));
                    }
                    if !entries.is_empty() {
                        ui.separator();
                        if menu_item(ui, "Clear Recent", "") {
                            actions.send(AppAction::ClearRecentFiles);
                            ui.close_menu();
                        }
                    }
                });
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
            ui.menu_button("Edit", |ui| {
                if menu_item(ui, "Tool parameters\u{2026}", "") {
                    actions.send(AppAction::ShowToolSettingsDialog);
                    ui.close_menu();
                }
                if menu_item(ui, "Preferences\u{2026}", "") {
                    actions.send(AppAction::ShowPreferencesDialog);
                    ui.close_menu();
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
                ui.add_enabled_ui(selection.picked_voxel.is_some(), |ui| {
                    if menu_item(ui, "Snap selection to bench", "") {
                        actions.send(AppAction::SnapSelectionToWorkbench);
                        ui.close_menu();
                    }
                });
                if menu_item(ui, "Settle (gravity)\u{2026}", "Ctrl+Shift+G") {
                    actions.send(AppAction::ShowSettleDialog);
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
                ui.separator();
                ui.menu_button("Matcap", |ui| {
                    for preset in MatcapPreset::all() {
                        let label = if app_settings.matcap == preset {
                            format!("{} ✓", preset.label())
                        } else {
                            preset.label().to_string()
                        };
                        if menu_item(ui, &label, "") {
                            actions.send(AppAction::SetMatcap(preset));
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                ui.menu_button("Bookmarks", |ui| {
                    if menu_item(ui, "Save Current", "") {
                        actions.send(AppAction::SaveCameraBookmark);
                        ui.close_menu();
                    }
                    let slots = bookmarks.slots().to_vec();
                    if slots.is_empty() {
                        ui.add_enabled(false, egui::Button::new("(empty)"));
                    } else {
                        ui.separator();
                        for (i, bookmark) in slots.iter().enumerate() {
                            let tip = format!(
                                "target ({:.0}, {:.0}, {:.0}) · dist {:.0} mm",
                                bookmark.target.x,
                                bookmark.target.y,
                                bookmark.target.z,
                                bookmark.distance
                            );
                            if ui
                                .add(
                                    egui::Button::new(&bookmark.name)
                                        .min_size(egui::vec2(180.0, 0.0)),
                                )
                                .on_hover_text(tip)
                                .clicked()
                            {
                                actions.send(AppAction::RestoreCameraBookmark(i));
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        if menu_item(ui, "Clear Bookmarks", "") {
                            actions.send(AppAction::ClearCameraBookmarks);
                            ui.close_menu();
                        }
                    }
                });
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

    draw_layers_panel(ctx, &mut workpiece, &mut actions);

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
            if app_settings.pressure_to_depth {
                ui.separator();
                if pen.from_stylus {
                    ui.label(format!("Pen: {:.0}%", pen.pressure * 100.0));
                } else {
                    ui.small("Pen: mouse (full depth)");
                }
            }
            ui.separator();
            let active_idx = workpiece.active_index();
            let layer_n = active_idx + 1;
            let layer_m = workpiece.layer_count();
            let layer_name = workpiece.active_layer().name.clone();
            ui.label(format!("Layer {layer_n}/{layer_m} · {layer_name}"));
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
    // X/Y/Z spinners in mm plus an Apply button. Anchored left of
    // the Layers panel so the two don't overlap.
    if matches!(tool.kind, ToolKind::Move) {
        draw_move_widget(ctx, &selection, &mut move_state, &mut actions);
    }
}

/// Right-side Layers panel (Track B3). Newest layer at the top
/// (Photoshop-like). Click a row to activate; eye toggles visibility;
/// × deletes (disabled on the last layer); Merge Down unions the
/// active layer into the one below it.
fn draw_layers_panel(
    ctx: &egui::Context,
    workpiece: &mut LayersState,
    actions: &mut EventWriter<AppAction>,
) {
    egui::SidePanel::right("mud_layers")
        .default_width(220.0)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading("Layers");
            ui.small("Tools edit the active layer only.");
            ui.separator();

            let count = workpiece.layer_count();
            let active = workpiece.active_index();
            let can_delete = count > 1;
            let can_merge = active > 0;

            // Snapshot names/visibility for the row loop so we can
            // still call `layer_mut` for in-place rename without
            // fighting the borrow checker over `layers()`.
            let rows: Vec<(usize, bool)> = (0..count)
                .rev()
                .map(|idx| (idx, workpiece.layers()[idx].visible))
                .collect();

            for (idx, visible) in rows {
                ui.horizontal(|ui| {
                    let vis_label = if visible { "vis" } else { "hid" };
                    if ui
                        .add(egui::Button::new(vis_label).min_size(egui::vec2(28.0, 18.0)))
                        .on_hover_text(if visible {
                            "Hide layer"
                        } else {
                            "Show layer"
                        })
                        .clicked()
                    {
                        actions.send(AppAction::SetLayerVisible(idx, !visible));
                    }

                    let is_active = idx == active;
                    // Edit a local copy so we can validate through
                    // `rename_layer` on focus loss (rejects blanks).
                    let mut name = workpiece.layers()[idx].name.clone();
                    let edit = egui::TextEdit::singleline(&mut name)
                        .desired_width(110.0)
                        .frame(is_active);
                    let response = ui.add(edit);
                    if response.changed() {
                        // Live preview while typing; final trim /
                        // reject happens on focus loss below.
                        if let Some(layer) = workpiece.layer_mut(idx) {
                            layer.name = name.clone();
                        }
                    }
                    if response.gained_focus() && !is_active {
                        actions.send(AppAction::SetActiveLayer(idx));
                    }
                    if response.lost_focus() && !workpiece.rename_layer(idx, name) {
                        // Blank / whitespace — restore a stable label.
                        let _ = workpiece.rename_layer(idx, format!("Layer {}", idx + 1));
                    }

                    let marker = if is_active { "*" } else { " " };
                    if ui
                        .selectable_label(is_active, marker)
                        .on_hover_text("Set active")
                        .clicked()
                    {
                        actions.send(AppAction::SetActiveLayer(idx));
                    }

                    ui.add_enabled_ui(can_delete, |ui| {
                        if ui
                            .small_button("x")
                            .on_hover_text("Delete layer")
                            .clicked()
                        {
                            actions.send(AppAction::DeleteLayer(idx));
                        }
                    });
                });
            }

            ui.separator();
            ui.add_enabled_ui(can_merge, |ui| {
                if ui
                    .button("Merge Down")
                    .on_hover_text("Union active layer into the one below, then drop it")
                    .clicked()
                {
                    actions.send(AppAction::MergeDown);
                }
            });
            if !can_merge {
                ui.small("Merge Down needs a layer below.");
            }
        });
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
        // Sit just left of the Layers panel (~220px) so the two
        // don't stack on top of each other.
        .anchor(egui::Align2::RIGHT_TOP, [-232.0, 44.0])
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
    workpiece: &LayersState,
    actions: &mut EventWriter<AppAction>,
) {
    let vs = workpiece.grid().voxel_size();
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
        if ui.button("Snap to bench").clicked() {
            actions.send(AppAction::SnapSelectionToWorkbench);
        }
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
#[allow(clippy::too_many_arguments)]
fn draw_dialogs(
    mut contexts: EguiContexts,
    mut state: ResMut<FileDialogState>,
    mut prim_state: ResMut<PrimitiveDialogState>,
    mut settle_state: ResMut<SettleDialogState>,
    mut settings_dialogs: ResMut<SettingsDialogState>,
    mut tool: ResMut<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    mut app_settings: ResMut<AppSettings>,
    mut workpiece: ResMut<LayersState>,
    grid_state: Res<WorkbenchGridState>,
    autosave: Res<AutosaveState>,
    mut export_notice: ResMut<ExportNotice>,
    mut actions: EventWriter<AppAction>,
) {
    let ctx = contexts.ctx_mut();

    // Post-export watertightness / result notice.
    if export_notice.message.is_some() {
        let mut dismiss = false;
        if let Some(msg) = export_notice.message.as_ref() {
            egui::Window::new("Export STL")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(msg);
                    if ui.button("OK").clicked() {
                        dismiss = true;
                    }
                });
        }
        if dismiss {
            export_notice.message = None;
        }
    }

    // Crash-recovery prompt — shown once when an autosave file was
    // left behind from a previous session.
    if autosave.recovery_pending {
        egui::Window::new("Recover unsaved work?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "An autosave from a previous session was found.\n\
                     Restore it onto the worktable, or discard it?",
                );
                ui.small(format!("{}", crate::project::autosave_path().display()));
                ui.horizontal(|ui| {
                    if ui.button("Restore").clicked() {
                        actions.send(AppAction::RestoreAutosave);
                    }
                    if ui.button("Discard").clicked() {
                        actions.send(AppAction::DiscardAutosave);
                    }
                });
            });
    }

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

    // Gravity settle dialog — softness slider, then one-shot burst.
    if settle_state.open {
        let mut commit: Option<Option<f32>> = None;
        egui::Window::new("Settle (gravity)")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Softness under gravity (stiff → soft clay):");
                ui.add(
                    egui::Slider::new(&mut settle_state.plasticity, 0.0..=1.0)
                        .text("softness"),
                );
                ui.small(
                    "Drop → sag first (eased in) → tip → thick splat.\n\
                     Even soft clay bows before it tips or pancakes.\n\
                     Active layer only · one undo stroke.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Settle").clicked() {
                        commit = Some(Some(settle_state.plasticity));
                    }
                    if ui.button("Cancel").clicked() {
                        commit = Some(None);
                    }
                });
            });
        match commit {
            Some(Some(p)) => {
                actions.send(AppAction::SettlePlastic(p));
            }
            Some(None) => {
                settle_state.open = false;
            }
            None => {}
        }
    }

    // Tool parameters — live-edit the active tool knobs.
    if settings_dialogs.tool_open {
        let mut close = false;
        egui::Window::new("Tool parameters")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Active tool: {}", tool_label(tool.kind)));
                ui.add_space(6.0);
                ui.add(
                    egui::Slider::new(&mut tool.size, SIZE_MIN..=SIZE_MAX)
                        .suffix(" mm")
                        .text("Size"),
                );
                ui.add(
                    egui::Slider::new(&mut tool.advance_per_step, 0.05..=3.0)
                        .text("Advance / step"),
                );
                ui.add(
                    egui::Slider::new(&mut tool.smooth_strength, 0.05..=1.0)
                        .text("Smooth strength"),
                );
                ui.checkbox(&mut tool.displace, "Magic clay (soft CSG / bulge)");
                let mut sym = symmetry.enabled;
                if ui.checkbox(&mut sym, "Mirror symmetry (X = 0)").changed() {
                    actions.send(AppAction::ToggleSymmetry);
                }
                if matches!(
                    tool.kind,
                    ToolKind::Cutter(CutterFamily::Circle) | ToolKind::Cutter(CutterFamily::Square)
                ) {
                    ui.add_space(6.0);
                    ui.heading("Cutter shape");
                    match tool.kind {
                        ToolKind::Cutter(CutterFamily::Square) => {
                            let max_corner = tool.size * 0.95;
                            ui.add(
                                egui::Slider::new(
                                    &mut tool.cutter_params.corner_radius,
                                    0.0..=max_corner,
                                )
                                .suffix(" mm")
                                .text("Corner radius"),
                            );
                            ui.small("0 = sharp square · higher rounds the corners.");
                        }
                        ToolKind::Cutter(CutterFamily::Circle) => {
                            ui.add(
                                egui::Slider::new(&mut tool.cutter_params.wave_amp, 0.0..=0.45)
                                    .text("Wave amplitude"),
                            );
                            ui.add(
                                egui::Slider::new(&mut tool.cutter_params.wave_freq, 2.0..=16.0)
                                    .text("Wave count"),
                            );
                            ui.small(
                                "Amplitude is a fraction of size (0 = smooth circle).\n\
                                 Wave count is how many lobes around the outline.",
                            );
                        }
                        _ => {}
                    }
                }
                ui.small(
                    "Size also responds to [ ] / - = and Shift+scroll.\n\
                     Advance affects Clay bite, Press engagement, and Paddle depth.\n\
                     Smooth strength is Smooth-tool only.\n\
                     Press (P) / Pull (L) displace volume; Add/Remove stays CSG.",
                );
                ui.add_space(4.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        if close {
            settings_dialogs.tool_open = false;
        }
    }

    // Preferences — turntable, grid, settle default.
    if settings_dialogs.prefs_open {
        let mut close = false;
        let mut grid_on = grid_state.visible;
        egui::Window::new("Preferences")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Workbench");
                if ui.checkbox(&mut grid_on, "Show workbench grid").changed() {
                    actions.send(AppAction::SetWorkbenchGrid(grid_on));
                }
                ui.small("10 mm major lines · also View → Workbench grid.");
                ui.add_space(8.0);
                ui.heading("Turntable");
                ui.add(
                    egui::Slider::new(&mut app_settings.turntable_period_secs, 2.0..=30.0)
                        .suffix(" s")
                        .text("Seconds per revolution"),
                );
                ui.small("Hold Q / E to rotate. Longer = slower.");
                ui.add_space(8.0);
                ui.heading("Settle (gravity)");
                ui.add(
                    egui::Slider::new(&mut app_settings.default_plasticity, 0.0..=1.0)
                        .text("Default softness"),
                );
                ui.small("Pre-fills Sculpt → Settle (gravity)…");
                ui.add_space(8.0);
                ui.heading("Shading");
                ui.horizontal(|ui| {
                    ui.label("Matcap:");
                    egui::ComboBox::from_id_salt("mud_matcap_preset")
                        .selected_text(app_settings.matcap.label())
                        .show_ui(ui, |ui| {
                            for preset in MatcapPreset::all() {
                                ui.selectable_value(
                                    &mut app_settings.matcap,
                                    preset,
                                    preset.label(),
                                );
                            }
                        });
                });
                ui.add(
                    egui::Slider::new(&mut app_settings.cavity_strength, 0.0..=1.0)
                        .text("Cavity"),
                );
                ui.small("Crevice darkening from the SDF (smoothed φ − φ).");
                ui.add_space(8.0);
                ui.heading("Mesher");
                let prev_mesher = app_settings.mesher;
                egui::ComboBox::from_id_salt("mud_mesher_kind")
                    .selected_text(app_settings.mesher.label())
                    .show_ui(ui, |ui| {
                        for kind in MesherKind::all() {
                            ui.selectable_value(&mut app_settings.mesher, kind, kind.label());
                        }
                    });
                if app_settings.mesher != prev_mesher {
                    workpiece.redirty_all_meshes();
                }
                ui.small(
                    "Surface Nets is smoother and cheaper.\n\
                     Dual Contouring keeps sharper cube corners and cuts.\n\
                     Also used for STL export.",
                );
                ui.add_space(8.0);
                ui.heading("Pen / stylus");
                ui.checkbox(
                    &mut app_settings.pressure_to_depth,
                    "Pressure controls depth",
                );
                ui.small(
                    "Stylus / touch force scales Clay bite, Paddle advance,\n\
                     and Smooth strength. Mouse always uses full depth.\n\
                     (Tilt → orientation is not wired yet.)",
                );
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        if close {
            settings_dialogs.prefs_open = false;
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
                         blank exports with a fresh timestamp.\n\
                         A watertightness check runs after extract.",
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
#[allow(clippy::too_many_arguments)]
fn publish_ui_capture(
    mut contexts: EguiContexts,
    dialogs: Res<FileDialogState>,
    prim: Res<PrimitiveDialogState>,
    settle: Res<SettleDialogState>,
    settings: Res<SettingsDialogState>,
    autosave: Res<AutosaveState>,
    export_notice: Res<ExportNotice>,
    mut gate: ResMut<UiCapturesInput>,
) {
    let ctx = contexts.ctx_mut();
    let modal = dialogs.any_open()
        || prim.open.is_some()
        || settle.open
        || settings.any_open()
        || autosave.any_modal()
        || export_notice.any_open();
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
fn tool_palette_order() -> [(&'static str, ToolKind); 13] {
    [
        ("1", ToolKind::Clay),
        ("P", ToolKind::Press),
        ("L", ToolKind::Pull),
        ("K", ToolKind::Knife),
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
        ToolKind::Press => "Press",
        ToolKind::Pull => "Pull",
        ToolKind::Knife => "Knife",
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
        ToolKind::Press => {
            "Hold LMB to press — clay displaces into a rim.\n\
             Volume is conserved (not carved away).\n\
             Shift+scroll or [ / ] resize · Advance sets bite."
        }
        ToolKind::Pull => {
            "Hold LMB to pull — surface grows, rim thins.\n\
             Volume is conserved (drawn from surroundings).\n\
             Empty bench: use Add/Remove to deposit coils."
        }
        ToolKind::Knife => {
            "Hold LMB and drag to cut a shallow kerf.\n\
             Removes clay (no rim recruitment).\n\
             For a through-slice use the Wire cutter."
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
            "Hold LMB to press a flat — clay squeezes to a rim.\n\
             Volume is conserved (displace, not carve).\n\
             Shift+scroll or [ / ] resize · Advance sets depth."
        }
        ToolKind::Select => {
            "LMB picks the piece under the cursor.\n\
             The selected piece is boxed in yellow.\n\
             Del  removes the selected piece.\n\
             A    active-only sculpt (other pieces stay).\n\
             Ctrl+G  drop every floating piece."
        }
        ToolKind::Move => {
            "Pick a piece (LMB) or use the current selection.\n\
             Grab a red / green / blue arrow and drag along it\n\
             for a live preview; release to commit.\n\
             Or type X / Y / Z nudges (mm) and Apply.\n\
             All moves snap to whole voxels."
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
