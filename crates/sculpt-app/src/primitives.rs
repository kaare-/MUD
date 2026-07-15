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
use crate::sculpt::{tool_label, SculptTool, ToolKind};
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::LayersState;

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
    mut workpiece: ResMut<LayersState>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut selection: ResMut<crate::selection::Selection>,
    mut tool: ResMut<SculptTool>,
) {
    for a in events.read() {
        if let AppAction::InsertPrimitive(shape, size_mm) = a {
            let seed = insert_primitive_centered_on_bench(
                *shape,
                *size_mm,
                &mut workpiece,
                &mut history,
                &mut stroke,
            );
            selection.invalidate_labels();
            // Auto-select the newly-inserted piece and switch to
            // the Move tool so the XYZ widget is right there. Done
            // via direct mutation of `SculptTool` rather than a
            // second AppAction event because Bevy forbids reading
            // and writing the same event type in one system.
            if let Some(voxel) = seed {
                selection.picked_voxel = Some(voxel);
                if tool.kind != ToolKind::Move {
                    tool.kind = ToolKind::Move;
                    stroke.discard_live();
                    info!("tool: {}", tool_label(ToolKind::Move));
                }
            }
        }
    }
}

/// Perform the SDF union of `shape` at the workbench centre, sized
/// by `size_mm`. Recorded as a single undo stroke.
///
/// Returns a voxel coordinate that lies inside the freshly-unioned
/// primitive (its geometric centre, clamped to the grid), so callers
/// can seed the [`Selection`] with the new piece. Returns `None`
/// when the union was a full no-op.
fn insert_primitive_centered_on_bench(
    shape: PrimitiveShape,
    size_mm: f32,
    workpiece: &mut LayersState,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) -> Option<(u32, u32, u32)> {
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
        workpiece.grid_mut(),
        &prim,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );

    let grid_res = workpiece.grid().res();
    if let Some(region) = region {
        recorder.record_dirty_region(region, grid_res);
        for c in region.touched_chunks(grid_res) {
            let ChunkCoord { x, y, z } = c;
            workpiece.mark_dirty((x, y, z));
        }
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
        info!("inserted {} ({:.1} mm)", shape.label().to_lowercase(), size);
        // Voxel index of the primitive's centre. Clamped so we
        // never hand out an out-of-range index — the primitive is
        // always at least partly in-bounds because we clipped it
        // to the bench.
        let grid_res = workpiece.grid().res();
        let vs = workpiece.grid().voxel_size();
        let origin = workpiece.grid().origin();
        let cx = ((prim.center.x - origin.x) / vs).round() as i32;
        let cy = ((prim.center.y - origin.y) / vs).round() as i32;
        let cz = ((prim.center.z - origin.z) / vs).round() as i32;
        let voxel = (
            cx.clamp(0, grid_res.x as i32 - 1) as u32,
            cy.clamp(0, grid_res.y as i32 - 1) as u32,
            cz.clamp(0, grid_res.z as i32 - 1) as u32,
        );
        Some(voxel)
    } else {
        info!("insert {} was a no-op — nothing changed", shape.label());
        None
    }
}
