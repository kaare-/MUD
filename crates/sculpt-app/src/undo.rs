//! Journal-based undo / redo.
//!
//! Design (see `DESIGN.md` §3.2): one stroke = one undo unit. During a
//! stroke, the recorder captures the *first* pre-mutation SDF value it
//! sees for every voxel the brush touches. On stroke end, it also
//! reads back the *final* post-stroke value for those voxels, so both
//! directions of history are covered.
//!
//! Storage is sparse: only voxels that actually changed are stored,
//! and each voxel is stored once regardless of how many stamps hit
//! it during the stroke. In Stage 1 the grid is small (128³), so
//! a fully-touched stroke is bounded at ~2 M voxels; typical strokes
//! touch a few thousand.
//!
//! Chunk invalidation is packaged with each entry so we can re-mesh
//! only the affected chunks on undo/redo instead of rebuilding the
//! whole grid.

use std::collections::HashSet;

use bevy::prelude::*;
use glam::UVec3;

use sculpt_core::{ChunkCoord, DirtyRegion, Grid, CHUNK_SIZE};

use crate::input_gate::UiCapturesInput;
use crate::workpiece::SculptWorkpiece;

/// Cap on how many strokes we remember. Prevents runaway memory growth
/// during long sessions. `Ctrl+Z` past this point simply stops.
const MAX_HISTORY: usize = 64;

/// One completed stroke, ready to be undone or redone.
pub struct UndoEntry {
    /// Voxel indices touched during the stroke (deduplicated).
    voxels: Vec<(u32, u32, u32)>,
    /// Pre-stroke SDF values, parallel to `voxels`.
    pre: Vec<f32>,
    /// Post-stroke SDF values, parallel to `voxels`.
    post: Vec<f32>,
    /// Chunk keys that need re-meshing on undo/redo.
    dirty_chunks: Vec<(u32, u32, u32)>,
}

impl UndoEntry {
    fn apply_pre(&self, grid: &mut Grid) {
        for ((x, y, z), pre) in self.voxels.iter().copied().zip(self.pre.iter().copied()) {
            grid.set(x, y, z, pre);
        }
    }

    fn apply_post(&self, grid: &mut Grid) {
        for ((x, y, z), post) in self.voxels.iter().copied().zip(self.post.iter().copied()) {
            grid.set(x, y, z, post);
        }
    }
}

/// Stroke-time recorder. One instance lives in [`SculptStroke`] while
/// a stroke is in progress.
#[derive(Default)]
pub struct StrokeRecorder {
    /// Packed voxel keys we've already captured a pre-value for.
    /// Prevents overwriting the true pre-stroke value with a
    /// mid-stroke value on later stamps.
    seen: HashSet<u64>,
    /// Parallel arrays of voxel indices and their pre-stroke values.
    voxels: Vec<(u32, u32, u32)>,
    pre: Vec<f32>,
    /// Chunks that have been marked dirty during this stroke.
    dirty_chunks: HashSet<(u32, u32, u32)>,
}

impl StrokeRecorder {
    pub fn record_pre_value(&mut self, x: u32, y: u32, z: u32, pre_value: f32) {
        let key = pack_key(x, y, z);
        if self.seen.insert(key) {
            self.voxels.push((x, y, z));
            self.pre.push(pre_value);
        }
    }

    pub fn record_dirty_region(&mut self, region: DirtyRegion, grid_res: UVec3) {
        for c in region.touched_chunks(grid_res) {
            self.dirty_chunks.insert((c.x, c.y, c.z));
        }
    }

    /// Wrap up the stroke, reading back the post-stroke values from
    /// the (now-modified) grid. Returns `None` if the stroke didn't
    /// actually touch anything (e.g. a click in empty air).
    pub fn finish(self, grid: &Grid) -> Option<UndoEntry> {
        if self.voxels.is_empty() {
            return None;
        }
        let post = self
            .voxels
            .iter()
            .map(|&(x, y, z)| grid.get(x, y, z))
            .collect();
        Some(UndoEntry {
            voxels: self.voxels,
            pre: self.pre,
            post,
            dirty_chunks: self.dirty_chunks.into_iter().collect(),
        })
    }
}

#[inline]
fn pack_key(x: u32, y: u32, z: u32) -> u64 {
    // 20 bits per axis is 1 M voxels per axis — three orders of magnitude
    // over anything MUD will realistically hit even after the Stage 2
    // migration to sparse tiles.
    (x as u64) | ((y as u64) << 20) | ((z as u64) << 40)
}

/// Global undo / redo stacks.
#[derive(Resource, Default)]
pub struct UndoHistory {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl UndoHistory {
    pub fn push_stroke(&mut self, entry: UndoEntry) {
        // A new stroke invalidates the redo stack — you can't cherry-
        // pick a branch of history you diverged from.
        self.redo.clear();
        self.undo.push(entry);
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
    }

    /// Wipe all history. Used when we replace the grid wholesale
    /// (e.g. loading a project file) — the recorded pre/post voxel
    /// values are now nonsense against the new grid, so we drop them
    /// rather than let the user Ctrl+Z into a corrupt intermediate.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

/// Wraps the stroke recorder so `sculpt_input` can push voxel values
/// into it without owning it directly. `None` means no stroke is in
/// progress.
#[derive(Resource, Default)]
pub struct SculptStroke {
    pub recorder: Option<StrokeRecorder>,
    /// Piece-local hit of the last clay (add/remove) stamp in the
    /// current stroke. Used to space stamps so hold-still can't race
    /// along the view axis.
    pub last_clay_hit: Option<glam::Vec3>,
    /// When painting face-on (not growing a side column), stamps are
    /// projected onto this tangent plane `(point, normal)` so a
    /// screen-vertical drag stays vertical instead of tip-chasing
    /// toward the camera at ~45°.
    pub paint_plane: Option<(glam::Vec3, glam::Vec3)>,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<UndoHistory>();
    app.init_resource::<SculptStroke>();
    app.add_systems(Update, handle_undo_redo_input);
}

fn handle_undo_redo_input(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    mut history: ResMut<UndoHistory>,
    mut workpiece: ResMut<SculptWorkpiece>,
) {
    if ui_gate.keyboard {
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    if !ctrl {
        return;
    }

    // Ctrl+Z = undo, Ctrl+Y or Ctrl+Shift+Z = redo. Matches most desktop
    // conventions the target user brings from other apps.
    let want_undo = keys.just_pressed(KeyCode::KeyZ) && !shift;
    let want_redo =
        keys.just_pressed(KeyCode::KeyY) || (keys.just_pressed(KeyCode::KeyZ) && shift);

    if want_undo {
        if let Some(entry) = history.undo.pop() {
            entry.apply_pre(&mut workpiece.grid);
            for c in &entry.dirty_chunks {
                workpiece.dirty.insert(*c);
            }
            history.redo.push(entry);
        }
    } else if want_redo {
        if let Some(entry) = history.redo.pop() {
            entry.apply_post(&mut workpiece.grid);
            for c in &entry.dirty_chunks {
                workpiece.dirty.insert(*c);
            }
            history.undo.push(entry);
            if history.undo.len() > MAX_HISTORY {
                history.undo.remove(0);
            }
        }
    }
}

/// Chunk coord that a voxel `(x, y, z)` belongs to. Useful for callers
/// that want to invalidate a specific voxel's chunk without going
/// through the DirtyRegion helper.
#[allow(dead_code)]
pub fn chunk_of_voxel(x: u32, y: u32, z: u32) -> ChunkCoord {
    ChunkCoord::new(x / CHUNK_SIZE, y / CHUNK_SIZE, z / CHUNK_SIZE)
}
