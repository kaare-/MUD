//! Save (`Ctrl+S`) and load (`Ctrl+O`) native `.mudclay` projects.
//!
//! **Save** writes the full layer stack as `.mudclay` v3 (sparse
//! tiles per layer — Track B4). STL export still flattens visible
//! layers; the project file keeps boundaries, names, and visibility.
//!
//! **Load** accepts v1 (dense) / v2 (sparse single grid) / v3
//! (multi-layer). Older files become a one-layer scene. On success
//! the live stack is replaced, chunk entities are despawned, and
//! the undo history is cleared.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bevy::prelude::*;
use sculpt_core::{
    project_scene_size, read_project_scene, write_project_scene, ProjectLayer, ProjectScene,
};

use crate::actions::AppAction;
use crate::export::timestamped_filename;
use crate::input_gate::UiCapturesInput;
use crate::undo::{SculptStroke, UndoHistory};
use crate::workpiece::LayersState;

pub fn plugin(app: &mut App) {
    app.init_resource::<FileDialogState>();
    app.add_systems(
        Update,
        (
            emit_project_hotkeys,
            handle_project_actions,
            handle_dialog_actions,
        ),
    );
}

/// Which (if any) in-app dialog is currently visible.
///
/// The dialog is drawn by the `ui` module and reads/writes this
/// resource. Handler systems here own the "spawn the dialog" and
/// "act on the dialog's result" paths.
#[derive(Resource, Default)]
pub struct FileDialogState {
    pub save_as: Option<SaveAsDialog>,
    pub open: Option<OpenDialog>,
    pub export_stl: Option<ExportStlDialog>,
}

impl FileDialogState {
    /// True while any modal file dialog is on screen. World-input
    /// systems check this via the `UiCapturesInput` gate.
    pub fn any_open(&self) -> bool {
        self.save_as.is_some() || self.open.is_some() || self.export_stl.is_some()
    }
}

/// State for the Save-As dialog. `name` is the editable text field —
/// initially seeded with a fresh timestamped name so a user who just
/// wants a new file can press Enter without typing.
pub struct SaveAsDialog {
    pub name: String,
}

/// State for the Export-STL-As dialog. Same shape as `SaveAsDialog`
/// but the finaliser appends `.stl` (see `ui::finalise_export_path`).
pub struct ExportStlDialog {
    pub name: String,
}

/// State for the Open dialog. `files` is a snapshot of `list_mudclay_in_cwd`
/// taken at the moment the dialog opened; `selected` is whatever the
/// user's clicked on so far.
pub struct OpenDialog {
    pub files: Vec<(PathBuf, SystemTime)>,
    pub selected: Option<PathBuf>,
}

fn handle_dialog_actions(
    mut events: EventReader<AppAction>,
    mut state: ResMut<FileDialogState>,
) {
    for a in events.read() {
        match a {
            AppAction::ShowSaveAsDialog => {
                // Start empty. Hint text in the dialog nudges the
                // user toward what to type; leaving the field blank
                // and hitting Save (or Enter) falls back to a fresh
                // timestamp, so 'just save something' still works
                // without typing. Pre-filling would force users to
                // clear a long string every time they want a name
                // of their own.
                state.save_as = Some(SaveAsDialog {
                    name: String::new(),
                });
                // Only one dialog at a time — a Save-As while an
                // Open is drifting on screen would be confusing.
                state.open = None;
            }
            AppAction::ShowOpenDialog => {
                state.open = Some(OpenDialog {
                    files: list_mudclay_in_cwd(),
                    selected: None,
                });
                state.save_as = None;
                state.export_stl = None;
            }
            AppAction::ShowExportStlDialog => {
                state.export_stl = Some(ExportStlDialog {
                    name: String::new(),
                });
                state.save_as = None;
                state.open = None;
            }
            _ => {}
        }
    }
}

/// Keyboard emitter — fires the same events UI menu items do.
///
/// | Shortcut              | Action                           |
/// |-----------------------|----------------------------------|
/// | Ctrl+S                | SaveProject (auto-timestamp)     |
/// | Ctrl+Shift+S          | ShowSaveAsDialog                 |
/// | Ctrl+O                | ShowOpenDialog (list picker)     |
/// | Ctrl+Shift+O          | LoadNewestProject (quick reopen) |
fn emit_project_hotkeys(
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
    let modifier = ctrl || super_key;
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !modifier {
        return;
    }
    if keys.just_pressed(KeyCode::KeyN) {
        if shift {
            actions.send(AppAction::ShowInsertPrimitiveDialog);
        } else {
            actions.send(AppAction::NewWorkpiece);
        }
    }
    if keys.just_pressed(KeyCode::KeyS) {
        if shift {
            actions.send(AppAction::ShowSaveAsDialog);
        } else {
            actions.send(AppAction::SaveProject);
        }
    }
    if keys.just_pressed(KeyCode::KeyO) {
        if shift {
            actions.send(AppAction::LoadNewestProject);
        } else {
            actions.send(AppAction::ShowOpenDialog);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_project_actions(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<LayersState>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut selection: ResMut<crate::selection::Selection>,
) {
    for a in events.read() {
        match a {
            AppAction::NewWorkpiece => {
                clear_worktable(&mut workpiece, &mut history, &mut stroke);
                selection.picked_voxel = None;
                selection.invalidate_labels();
            }
            AppAction::SaveProject => {
                let path = PathBuf::from(timestamped_filename("mud-sculpt-", ".mudclay"));
                save_to_path(&workpiece, &path);
            }
            AppAction::SaveProjectAs(path) => save_to_path(&workpiece, path),
            AppAction::LoadNewestProject => match newest_mudclay_in_cwd() {
                Some(p) => {
                    load_from_path(&p, &mut workpiece, &mut history, &mut stroke);
                    selection.picked_voxel = None;
                    selection.invalidate_labels();
                }
                None => warn!("no mud-sculpt-*.mudclay files in the working directory"),
            },
            AppAction::OpenProject(path) => {
                load_from_path(path, &mut workpiece, &mut history, &mut stroke);
                selection.picked_voxel = None;
                selection.invalidate_labels();
            }
            _ => {}
        }
    }
}

/// Reset the workpiece to an empty grid at the current dimensions.
/// Clears undo history and any in-flight stroke — the journal held
/// pre/post values against the old grid and would misapply otherwise.
pub fn clear_worktable(
    workpiece: &mut LayersState,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) {
    // Drop every extra layer and empty the survivor — File → New is
    // "blank worktable", not "clear the active layer only".
    workpiece.reset_to_empty_single_layer();
    history.clear();
    stroke.discard_live();
    info!("worktable cleared — undo history cleared");
}

/// Write the workpiece to `path` as `.mudclay` v3 (all layers).
/// Wraps every failure in a user-facing log message rather than
/// propagating — we're being called from a fire-and-forget event
/// handler and there's nowhere useful for the Result to go.
pub fn save_to_path(workpiece: &LayersState, path: &Path) {
    info!("saving project to {}", path.display());
    let file = match File::create(path) {
        Ok(f) => f,
        Err(e) => {
            error!("failed to create {}: {e}", path.display());
            return;
        }
    };
    let mut writer = BufWriter::new(file);
    let scene = scene_from_workpiece(workpiece);
    if let Err(e) = write_project_scene(&scene, &mut writer) {
        error!("failed to write project: {e}");
        return;
    }
    let bytes = project_scene_size(&scene);
    info!(
        "wrote {} bytes ({} layer(s)) to {}",
        bytes,
        scene.layers.len(),
        path.display()
    );
}

/// Read a `.mudclay` file at `path` and replace the live layer stack.
/// Clears the undo history and any in-flight stroke on success —
/// journaled voxel values from before the swap reference a different
/// grid.
pub fn load_from_path(
    path: &Path,
    workpiece: &mut LayersState,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) {
    info!("loading project from {}", path.display());
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            error!("failed to open {}: {e}", path.display());
            return;
        }
    };
    let mut reader = BufReader::new(file);
    let scene = match read_project_scene(&mut reader) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to read {}: {e}", path.display());
            return;
        }
    };
    let layers: Vec<_> = scene
        .layers
        .into_iter()
        .map(|ProjectLayer { id, name, visible, grid }| (id, name, visible, grid))
        .collect();
    let layer_count = layers.len();
    let active = scene.active;
    match workpiece.replace_from_scene(layers, active) {
        Ok(()) => {
            history.clear();
            stroke.discard_live();
            info!(
                "loaded {} ({} layer(s)) — undo history cleared",
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                layer_count,
            );
        }
        Err(e) => {
            error!("can't apply loaded project: {e}");
        }
    }
}

fn scene_from_workpiece(workpiece: &LayersState) -> ProjectScene {
    ProjectScene {
        layers: workpiece
            .layers()
            .iter()
            .map(|layer| ProjectLayer {
                id: layer.id,
                name: layer.name.clone(),
                visible: layer.visible,
                grid: layer.grid.clone(),
            })
            .collect(),
        active: workpiece.active_index(),
    }
}

/// Newest `.mudclay` in the current working directory by mtime,
/// or `None` if the directory has none.
fn newest_mudclay_in_cwd() -> Option<PathBuf> {
    newest_mudclay_in(&std::env::current_dir().ok()?)
}

/// Testable core of `newest_mudclay_in_cwd`. Parameterising the
/// directory lets tests use dedicated tmp dirs without racing on the
/// process-global `current_dir` while `cargo test` runs test threads
/// in parallel.
fn newest_mudclay_in(dir: &Path) -> Option<PathBuf> {
    list_mudclay_in(dir).into_iter().next().map(|(p, _)| p)
}

/// Every `.mudclay` file in the working directory, paired with its
/// modification time. Sorted newest-first for the Open dialog.
pub fn list_mudclay_in_cwd() -> Vec<(PathBuf, SystemTime)> {
    match std::env::current_dir() {
        Ok(dir) => list_mudclay_in(&dir),
        Err(_) => Vec::new(),
    }
}

/// Testable core of `list_mudclay_in_cwd`. Filters by `.mudclay`
/// extension and sorts by descending mtime, so a hand-renamed
/// `wolf-head.mudclay` participates alongside the auto-timestamped
/// files.
fn list_mudclay_in(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let iter = match fs::read_dir(dir) {
        Ok(i) => i,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("mudclay") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        out.push((path, mtime));
    }
    out.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Fresh scratch directory keyed by test name. Each test owns its
    /// own path so parallel `cargo test` can't clobber siblings — we
    /// avoid the process-global `current_dir` entirely by passing
    /// paths in.
    fn scratch(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("mud-project-test-{name}"));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    #[test]
    fn list_filters_by_extension_and_picks_up_hand_named_files() {
        let dir = scratch("list-filter");
        for name in [
            "mud-sculpt-20260714-081500.mudclay",
            "wolf-head-v3.mudclay",
            "not-a-mudclay.txt",
            "random.stl",
        ] {
            let mut f = File::create(dir.join(name)).unwrap();
            f.write_all(b"stub").unwrap();
        }

        let list = list_mudclay_in(&dir);
        assert_eq!(list.len(), 2, "only .mudclay files count, txt/stl ignored");
        let names: Vec<String> = list
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"mud-sculpt-20260714-081500.mudclay".into()));
        assert!(names.contains(&"wolf-head-v3.mudclay".into()));
    }

    #[test]
    fn list_returns_empty_when_directory_has_no_projects() {
        let dir = scratch("list-empty");
        File::create(dir.join("some-other.stl")).unwrap();
        assert!(list_mudclay_in(&dir).is_empty());
        assert!(newest_mudclay_in(&dir).is_none());
    }

    #[test]
    fn newest_returns_the_most_recently_modified() {
        // set_modified pins mtimes deterministically, avoiding
        // sleep-in-test flakiness on filesystems with 1s mtime
        // resolution.
        use std::time::{Duration, SystemTime};
        let dir = scratch("newest-mtime");
        let now = SystemTime::now();
        for (name, secs_ago) in [
            ("old.mudclay", 3600),
            ("recent.mudclay", 60),
            ("middle.mudclay", 600),
        ] {
            let f = File::create(dir.join(name)).unwrap();
            f.set_modified(now - Duration::from_secs(secs_ago)).unwrap();
        }
        let newest = newest_mudclay_in(&dir).expect("should find one");
        assert_eq!(
            newest.file_name().unwrap().to_str().unwrap(),
            "recent.mudclay",
        );
    }
}
