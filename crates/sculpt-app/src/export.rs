//! Export the workpiece to disk on `Ctrl+E`.
//!
//! Extracts a single seamless mesh from the current SDF grid via
//! `sculpt_core::extract_full_mesh`, writes a binary STL (Z-up, so
//! it drops onto a slicer's print bed with the workbench at Z=0),
//! and logs the path + file size + triangle count so the user knows
//! where the file went.
//!
//! Blocking extraction is fine at Stage-2 grid sizes (128³ ≈
//! 100 ms). If we ever need it non-blocking, spawn a task and hand
//! the writer job to Bevy's `AsyncComputeTaskPool`.
//!
//! Files land in the current working directory with a
//! `mud-sculpt-<yyyymmdd>-<HHMMSS>.stl` name. Predictable, sortable,
//! and never overwrites a previous export.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use sculpt_core::{extract_full_mesh, stl_binary_size, write_stl_binary, Orientation};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::workpiece::SculptWorkpiece;

pub fn plugin(app: &mut App) {
    app.add_systems(Update, (emit_export_hotkey, handle_export_action));
}

fn emit_export_hotkey(
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
    if (ctrl || super_key) && keys.just_pressed(KeyCode::KeyE) {
        actions.send(AppAction::ExportStl);
    }
}

fn handle_export_action(
    mut events: EventReader<AppAction>,
    workpiece: Res<SculptWorkpiece>,
) {
    for a in events.read() {
        if matches!(a, AppAction::ExportStl) {
            export_stl_now(&workpiece);
        }
    }
}

fn export_stl_now(workpiece: &SculptWorkpiece) {
    let path = PathBuf::from(timestamped_filename("mud-sculpt-", ".stl"));
    info!("exporting STL to {}", path.display());

    let mesh = extract_full_mesh(&workpiece.grid);
    if mesh.is_empty() {
        warn!("no material to export — grid is empty");
        return;
    }
    let expected_bytes = stl_binary_size(&mesh);
    let tri_count = mesh.indices.len() / 3;

    let file = match File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            error!("failed to create {}: {e}", path.display());
            return;
        }
    };
    let mut writer = BufWriter::new(file);
    if let Err(e) = write_stl_binary(&mesh, Orientation::Zup, &mut writer) {
        error!("failed to write STL: {e}");
        return;
    }
    info!(
        "wrote {} tris, {} bytes to {}",
        tri_count,
        expected_bytes,
        path.display()
    );
}

/// Timestamped filename of the form `<prefix>YYYYMMDD-HHMMSS<ext>`.
/// Used by both STL export and `.mudclay` save. Public so
/// `project::save_on_hotkey` reuses the same convention.
pub(crate) fn timestamped_filename(prefix: &str, ext: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day, h, m, s) = seconds_to_ymdhms(now);
    format!("{prefix}{year:04}{month:02}{day:02}-{h:02}{m:02}{s:02}{ext}")
}

/// Convert a UNIX timestamp (seconds since 1970-01-01 UTC) into a
/// broken-down date/time. Handles leap years by the standard Gregorian
/// rules. Enough precision for a filename; not a general-purpose
/// datetime library.
fn seconds_to_ymdhms(mut secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = (secs % 60) as u32;
    secs /= 60;
    let m = (secs % 60) as u32;
    secs /= 60;
    let h = (secs % 24) as u32;
    secs /= 24;
    // `secs` now = days since 1970-01-01.
    let mut year: u32 = 1970;
    let mut days = secs as u32;
    loop {
        let ydays = if is_leap(year) { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        year += 1;
    }
    let month_lens: [u32; 12] = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month: u32 = 1;
    for len in &month_lens {
        if days < *len {
            break;
        }
        days -= *len;
        month += 1;
    }
    let day = days + 1;
    (year, month, day, h, m, s)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_is_1970_01_01_00_00_00() {
        assert_eq!(seconds_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn known_timestamp_2026_07_13() {
        // 2026-07-13T00:00:00Z = 1_783_900_800
        // (verified with `date -u -d "@1783900800"`)
        assert_eq!(
            seconds_to_ymdhms(1_783_900_800),
            (2026, 7, 13, 0, 0, 0),
        );
    }

    #[test]
    fn timestamp_after_leap_day_is_correct() {
        // 2024-03-01T12:34:56Z = 1_709_296_496
        assert_eq!(
            seconds_to_ymdhms(1_709_296_496),
            (2024, 3, 1, 12, 34, 56),
        );
    }
}
