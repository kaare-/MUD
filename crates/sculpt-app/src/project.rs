//! Save (`Ctrl+S`) and load (`Ctrl+O`) native `.mudclay` projects.
//!
//! **Save** is the mirror of STL export: extract the current grid
//! state, write a `.mudclay` blob to the current working directory
//! with a timestamped name (`mud-sculpt-YYYYMMDD-HHMMSS.mudclay`),
//! log the path and byte count.
//!
//! **Load** is deliberately as-dumb-as-possible for the MVP: no
//! file picker, no "recent files" menu — instead, `Ctrl+O` scans the
//! CWD for `mud-sculpt-*.mudclay`, picks the alphabetically-last
//! one (which is also the newest, because the timestamps are
//! zero-padded YYYYMMDD-HHMMSS), and loads it. That's enough for the
//! reload-and-continue workflow that motivates the feature; a proper
//! Save-As / Open dialog can wait until we have `rfd` or a native
//! file picker plugin plumbed through.
//!
//! On load the current grid is swapped, every chunk is marked dirty
//! for re-meshing, and the undo history is cleared (the pre/post
//! voxel values it held reference the old grid and would be
//! actively harmful if replayed against the new one).

use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use bevy::prelude::*;
use sculpt_core::{project_size, read_project, write_project};

use crate::actions::AppAction;
use crate::export::timestamped_filename;
use crate::input_gate::UiCapturesInput;
use crate::undo::UndoHistory;
use crate::workpiece::SculptWorkpiece;

pub fn plugin(app: &mut App) {
    app.add_systems(Update, (emit_project_hotkeys, handle_project_actions));
}

/// Keyboard emitter — Ctrl+S and Ctrl+O fire the same events UI menu
/// items do. Nothing else in this system, so the work function
/// stays testable without a running Bevy world.
fn emit_project_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    mut actions: EventWriter<AppAction>,
) {
    if ui_gate.keyboard {
        return;
    }
    if just_pressed_with_ctrl(&keys, KeyCode::KeyS) {
        actions.send(AppAction::SaveProject);
    }
    if just_pressed_with_ctrl(&keys, KeyCode::KeyO) {
        actions.send(AppAction::LoadNewestProject);
    }
}

fn handle_project_actions(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut history: ResMut<UndoHistory>,
) {
    for a in events.read() {
        match a {
            AppAction::SaveProject => save_current(&workpiece),
            AppAction::LoadNewestProject => load_newest(&mut workpiece, &mut history),
            _ => {}
        }
    }
}

fn save_current(workpiece: &SculptWorkpiece) {
    let path = PathBuf::from(timestamped_filename("mud-sculpt-", ".mudclay"));
    info!("saving project to {}", path.display());
    let file = match File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            error!("failed to create {}: {e}", path.display());
            return;
        }
    };
    let mut writer = BufWriter::new(file);
    if let Err(e) = write_project(&workpiece.grid, &mut writer) {
        error!("failed to write project: {e}");
        return;
    }
    let bytes = project_size(&workpiece.grid);
    info!("wrote {} bytes to {}", bytes, path.display());
}

fn load_newest(workpiece: &mut SculptWorkpiece, history: &mut UndoHistory) {
    let path = match newest_mudclay_in_cwd() {
        Some(p) => p,
        None => {
            warn!("no mud-sculpt-*.mudclay files found in the working directory");
            return;
        }
    };
    info!("loading project from {}", path.display());
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            error!("failed to open {}: {e}", path.display());
            return;
        }
    };
    let mut reader = BufReader::new(file);
    let new_grid = match read_project(&mut reader) {
        Ok(g) => g,
        Err(e) => {
            error!("failed to read {}: {e}", path.display());
            return;
        }
    };
    match workpiece.swap_grid(new_grid) {
        Ok(()) => {
            history.clear();
            info!(
                "loaded {} — undo history cleared",
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            );
        }
        Err(e) => {
            error!("can't apply loaded grid: {e}");
        }
    }
}

fn just_pressed_with_ctrl(keys: &ButtonInput<KeyCode>, key: KeyCode) -> bool {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let super_key =
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);
    (ctrl || super_key) && keys.just_pressed(key)
}

/// Newest `mud-sculpt-*.mudclay` in the current working directory,
/// or `None` if the directory has none. Uses alphabetical order,
/// which is chronological order given the timestamped filename
/// convention.
fn newest_mudclay_in_cwd() -> Option<PathBuf> {
    let dir = std::env::current_dir().ok()?;
    let mut best: Option<PathBuf> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !fname.starts_with("mud-sculpt-") || !fname.ends_with(".mudclay") {
            continue;
        }
        match &best {
            None => best = Some(path),
            Some(b) => {
                if path.file_name() > b.file_name() {
                    best = Some(path);
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Guard for the CWD environment change: sets a temp dir on
    /// construction, restores the previous CWD on drop. Kept private
    /// so no other test accidentally leaks the state.
    struct CwdGuard {
        prev: PathBuf,
    }

    impl CwdGuard {
        fn to(new_cwd: &PathBuf) -> Self {
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(new_cwd).unwrap();
            CwdGuard { prev }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.prev).ok();
        }
    }

    #[test]
    fn newest_finds_the_latest_timestamped_file() {
        let tmp = std::env::temp_dir().join("mud-newest-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for name in [
            "mud-sculpt-20250101-000000.mudclay",
            "mud-sculpt-20260714-081500.mudclay",
            "mud-sculpt-20260714-081459.mudclay",
            "not-a-mudclay.txt",
            "random.mudclay",
        ] {
            let mut f = File::create(tmp.join(name)).unwrap();
            f.write_all(b"stub").unwrap();
        }

        let _guard = CwdGuard::to(&tmp);
        let newest = newest_mudclay_in_cwd().expect("should find at least one");
        assert_eq!(
            newest.file_name().unwrap().to_str().unwrap(),
            "mud-sculpt-20260714-081500.mudclay",
            "alphabetical order = chronological order given our filename convention",
        );
    }

    #[test]
    fn newest_returns_none_when_directory_has_no_projects() {
        let tmp = std::env::temp_dir().join("mud-newest-empty-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        File::create(tmp.join("some-other.stl")).unwrap();
        let _guard = CwdGuard::to(&tmp);
        assert!(newest_mudclay_in_cwd().is_none());
    }
}
