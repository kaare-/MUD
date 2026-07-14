//! Insert-primitive dialog and action handler.
//!
//! Menu action: `File → Insert Primitive…`. The user picks a shape
//! (sphere / box / cylinder / torus) and a size (mm); on Insert we
//! union that primitive into the current SDF grid, resting on the
//! workbench, and journal the change as a single undo stroke so
//! `Ctrl+Z` can take the insert back out.
//!
//! Placement is deterministic: the primitive sits on the workbench
//! (`y = 0`), centred on `x = z = 0`. Users can subsequently sculpt
//! it in place. Freeform positioning is a later concern.

use bevy::prelude::*;
use glam::Vec3 as GVec3;
use sculpt_core::{apply_primitive_with_callback, ChunkCoord, Primitive, PrimitiveKind};

use crate::actions::AppAction;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::SculptWorkpiece;

pub fn plugin(app: &mut App) {
    app.init_resource::<PrimitiveDialogState>();
    app.add_systems(
        Update,
        (open_primitive_dialog, handle_insert_action),
    );
}

/// Shapes exposed in the Insert Primitive dialog.
///
/// Not the same as `PrimitiveKind` — this UI-level enum carries a
/// single `size` scalar plus a shape identifier so the dialog widget
/// stays simple. The handler converts to `PrimitiveKind` with the
/// right per-shape geometry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PrimitiveShape {
    Sphere,
    Cube,
    Cylinder,
    Torus,
}

impl PrimitiveShape {
    pub fn label(self) -> &'static str {
        match self {
            PrimitiveShape::Sphere => "Sphere",
            PrimitiveShape::Cube => "Cube",
            PrimitiveShape::Cylinder => "Cylinder",
            PrimitiveShape::Torus => "Torus",
        }
    }
}

/// Modal state for the Insert Primitive dialog. `None` when closed.
#[derive(Resource, Default)]
pub struct PrimitiveDialogState {
    pub open: Option<InsertPrimitiveDialog>,
}

pub struct InsertPrimitiveDialog {
    pub shape: PrimitiveShape,
    /// Radius / half-side / etc, in mm.
    pub size_mm: f32,
}

impl Default for InsertPrimitiveDialog {
    fn default() -> Self {
        Self {
            shape: PrimitiveShape::Sphere,
            size_mm: 20.0,
        }
    }
}

fn open_primitive_dialog(
    mut events: EventReader<AppAction>,
    mut state: ResMut<PrimitiveDialogState>,
) {
    for a in events.read() {
        if matches!(a, AppAction::ShowInsertPrimitiveDialog) {
            state.open = Some(InsertPrimitiveDialog::default());
        }
    }
}

fn handle_insert_action(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
) {
    for a in events.read() {
        if let AppAction::InsertPrimitive(shape, size_mm) = a {
            insert_primitive_centered_on_bench(
                *shape,
                *size_mm,
                &mut workpiece,
                &mut history,
                &mut stroke,
            );
        }
    }
}

/// Perform the SDF union of `shape` at the workbench centre, sized
/// by `size_mm`. Recorded as a single undo stroke.
fn insert_primitive_centered_on_bench(
    shape: PrimitiveShape,
    size_mm: f32,
    workpiece: &mut SculptWorkpiece,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) {
    // Abandon any live drag; the insert supersedes it.
    stroke.discard_live();
    let size = size_mm.max(1.0);
    let (kind, y_center) = match shape {
        PrimitiveShape::Sphere => (PrimitiveKind::Sphere { radius: size }, size),
        PrimitiveShape::Cube => (
            PrimitiveKind::Box {
                half_extents: GVec3::splat(size),
            },
            size,
        ),
        PrimitiveShape::Cylinder => (
            PrimitiveKind::Cylinder {
                radius: size,
                half_height: size,
            },
            size,
        ),
        PrimitiveShape::Torus => (
            PrimitiveKind::Torus {
                major: size,
                minor: (size * 0.35).max(1.0),
            },
            (size * 0.35).max(1.0),
        ),
    };
    let prim = Primitive {
        kind,
        center: GVec3::new(0.0, y_center, 0.0),
        workbench_y: Some(0.0),
    };

    let mut recorder = StrokeRecorder::default();
    let region = apply_primitive_with_callback(
        &mut workpiece.grid,
        &prim,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );

    let grid_res = workpiece.grid.res();
    if let Some(region) = region {
        recorder.record_dirty_region(region, grid_res);
        for c in region.touched_chunks(grid_res) {
            let ChunkCoord { x, y, z } = c;
            workpiece.dirty.insert((x, y, z));
        }
    }
    if let Some(entry) = recorder.finish(&workpiece.grid) {
        history.push_stroke(entry);
        info!("inserted {} ({:.1} mm)", shape.label().to_lowercase(), size);
    } else {
        info!("insert {} was a no-op — nothing changed", shape.label());
    }
}
