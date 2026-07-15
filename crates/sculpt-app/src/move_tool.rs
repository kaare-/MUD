//! Move tool — rigidly translate the currently-selected component.
//!
//! The Move tool doesn't sculpt: it applies a signed X/Y/Z
//! millimetre offset to the selected component. Two entry points:
//!
//! - The HUD widget in `ui.rs` shows a compact `⟨X | Y | Z⟩`
//!   trio of number inputs plus Apply / Reset.
//! - A **3D axis gizmo** anchored on the selected piece: three
//!   arrows (red X, green Y, blue Z) that the user can grab with
//!   the mouse and drag along. The piece previews in real time
//!   while the drag is in progress and the widget's numbers
//!   update live to match. Releasing the mouse commits as a
//!   single undo entry. Users who prefer typing can ignore the
//!   gizmo and enter values into the widget directly.
//!
//! Snap-to-voxel: all moves are integer-voxel offsets. Non-integer
//! translations would resample the SDF and blur the surface; every
//! DCC move here is lossless.
//!
//! Live preview strategy: at drag start, a region-scoped snapshot
//! covering just the selected component's own (widened) bounds is
//! taken, and the component labels are frozen. `ensure_region_covers`
//! grows that snapshot on demand as the drag reaches farther (Track
//! A3, `PLAN.md`) — never a whole-domain copy. Each drag frame resets
//! the grid from the snapshot and re-applies `translate_component`
//! with the *total* delta, so undo doesn't need to record
//! intermediate states. On release, the final delta is applied one
//! last time via a journalling recorder → one clean undo entry.
//!
//! The heavy lifting — shifting voxel values along with their
//! narrow band — lives in `sculpt_core::translate_component`.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::{IVec3, Vec3 as GVec3};
use sculpt_core::{
    label_components, touched_region_for_translate, translate_component, ChunkCoord,
    ComponentField, ComponentId, Grid, RegionSnapshot,
};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::sculpt::{SculptTool, ToolKind};
use crate::selection::Selection;
use crate::turntable::TurntableSet;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{LayersState, WorkpieceRoot};

/// User-editable pending delta (mm along each axis) that the Move
/// HUD widget in `ui.rs` binds to. Cleared to zero after each
/// successful move so consecutive nudges accumulate on the piece
/// not on the widget.
///
/// While a gizmo drag is in progress this resource is updated live
/// so the widget shows the current running total.
#[derive(Resource, Default)]
pub struct MoveState {
    pub pending_mm: Vec3,
}

/// Which of the three primary axes a drag is running along.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    fn dir_local(self) -> GVec3 {
        match self {
            Axis::X => GVec3::X,
            Axis::Y => GVec3::Y,
            Axis::Z => GVec3::Z,
        }
    }
    /// Component index (0 = X, 1 = Y, 2 = Z). Used to write into
    /// [`MoveState::pending_mm`] without a `match` at every call.
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

/// Marker component for the three axis arrows.
#[derive(Component)]
struct MoveArrow {
    axis: Axis,
}

/// Live gizmo-drag state. `None` when the user isn't dragging an
/// arrow — including when the Move tool is off, or the mouse is
/// down but engaged with the pick-a-piece codepath instead.
#[derive(Resource, Default)]
pub struct MoveGizmoDrag {
    active: Option<ActiveDrag>,
    /// Set to true on the frame a drag begins so that
    /// `selection::selection_input` can skip its own pick and let
    /// the gizmo own the click. Cleared at the end of the frame.
    pub started_this_frame: bool,
}

impl MoveGizmoDrag {
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// If a drag is in flight, return the pre-drag AABB of the
    /// piece plus this frame's voxel-snapped delta. Consumers
    /// (selection highlight, arrows) use this to render the
    /// piece's *previewed* pose while the labels cached on
    /// [`Selection`] are still pinned to its pre-drag pose.
    pub fn active_drag_bounds_and_delta(
        &self,
        state: &MoveState,
    ) -> Option<(glam::UVec3, glam::UVec3, glam::IVec3)> {
        let active = self.active.as_ref()?;
        let (mn, mx) = active.labels.bounds_of(active.component_id)?;
        // Round `pending_mm` to voxels: that's the exact integer
        // offset `apply_preview` writes into the grid each frame,
        // so the highlight lines up with the visible piece.
        let vs = active.voxel_size;
        let delta = IVec3::new(
            (state.pending_mm.x / vs).round() as i32,
            (state.pending_mm.y / vs).round() as i32,
            (state.pending_mm.z / vs).round() as i32,
        );
        Some((mn, mx, delta))
    }
}

struct ActiveDrag {
    axis: Axis,
    /// Parameter along the axis (in piece-local mm) that the cursor
    /// projected to at LMB-down. Used as the zero-point for the
    /// running delta.
    t_start: f32,
    /// [`MoveState::pending_mm`] at LMB-down. The gizmo adds to the
    /// existing widget value rather than clobbering it, so a typed
    /// nudge can be followed by a gizmo tweak without losing the
    /// typed part.
    initial_widget_mm: Vec3,
    /// Region-scoped snapshot covering everywhere this drag has
    /// reached so far, used to rewind before each frame's
    /// re-application (so repeated `translate_component` calls with
    /// a varying total delta don't compound). Grows on demand via
    /// [`ensure_region_covers`] instead of paying a full-domain
    /// `to_dense()` / `restore_samples()` every frame.
    grid_snapshot: RegionSnapshot,
    /// Labels captured at drag start. Since we always start each
    /// frame's translate from `grid_snapshot`, the label field
    /// stays valid for the whole drag.
    labels: ComponentField,
    component_id: ComponentId,
    /// Voxel size of the grid at drag start (constant during a
    /// drag). Cached so consumers outside the plugin can convert
    /// `pending_mm` to a voxel delta without a live grid handle.
    voxel_size: f32,
    /// Union of every chunk we've dirtied since drag start, so we
    /// keep re-meshing the "old-position" chunks even after the
    /// piece has moved on and their apparent SDF has reset.
    dirty_chunks: HashSet<(u32, u32, u32)>,
    /// Total delta applied on the previous frame (or zero on
    /// frame 0). Used only for logging / tests.
    #[allow(dead_code)]
    last_applied_vox: IVec3,
}

/// System set for gizmo pointer input, exported so `selection` can
/// order its own pointer-picking `after(MoveGizmoInputSet)`. Keeps
/// gizmo/selection ordering explicit even though they live in
/// different plugins.
#[derive(SystemSet, Debug, Clone, Hash, PartialEq, Eq)]
pub struct MoveGizmoInputSet;

pub fn plugin(app: &mut App) {
    app.init_resource::<MoveState>();
    app.init_resource::<MoveGizmoDrag>();
    // PostStartup: workpiece root must exist first.
    app.add_systems(PostStartup, spawn_axis_gizmo);
    app.add_systems(
        Update,
        (
            update_gizmo_visibility_and_transform,
            gizmo_pointer_input.in_set(MoveGizmoInputSet),
            handle_move_action,
            clear_frame_flags,
        )
            // Same "after turntable" placement as selection_input so
            // this frame's Q/E rotation is baked in when we test
            // cursor rays.
            .chain()
            .after(TurntableSet),
    );
}

/// Length of each axis arrow in piece-local mm. Chosen so the
/// arrow tips fall well outside typical starter primitives (radius
/// 20 mm) but not so far that they poke off the viewport at close
/// zoom.
const ARROW_LENGTH: f32 = 60.0;
/// Radius of the arrow shaft, in piece-local mm. Used both for the
/// visible shaft cylinder and the pointer-pick tolerance.
const ARROW_RADIUS: f32 = 1.6;
/// Cone tip half-length, in mm.
const TIP_LENGTH: f32 = 10.0;
/// Cone tip base radius, in mm.
const TIP_RADIUS: f32 = 4.5;
/// Pointer-pick tolerance around each arrow, in piece-local mm.
/// Any cursor-ray whose closest approach to an arrow line is
/// under this distance wins the click. Slightly wider than the
/// shaft so the arrow feels sticky rather than pixel-perfect.
const PICK_TOLERANCE: f32 = 6.0;

/// Colour per axis. Bevy names them "Vec3::X", "Vec3::Y", "Vec3::Z"
/// so it's convenient to co-ordinate the RGB channels with them.
fn axis_color(axis: Axis) -> Color {
    match axis {
        Axis::X => Color::srgb(1.0, 0.25, 0.30),
        Axis::Y => Color::srgb(0.30, 1.0, 0.30),
        Axis::Z => Color::srgb(0.30, 0.50, 1.0),
    }
}

fn axis_emissive(axis: Axis) -> LinearRgba {
    match axis {
        Axis::X => LinearRgba::new(1.8, 0.15, 0.20, 1.0),
        Axis::Y => LinearRgba::new(0.20, 1.8, 0.20, 1.0),
        Axis::Z => LinearRgba::new(0.20, 0.35, 1.8, 1.0),
    }
}

/// Rotation that takes the default Y-up shaft/cone geometry onto
/// the target axis.
fn axis_rotation(axis: Axis) -> Quat {
    match axis {
        // Cylinder default is Y-up; rotate to X or Z as needed.
        Axis::X => Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
        Axis::Y => Quat::IDENTITY,
        Axis::Z => Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
    }
}

fn spawn_axis_gizmo(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    q_root: Query<Entity, With<WorkpieceRoot>>,
) {
    let Ok(root) = q_root.get_single() else {
        return;
    };
    // Shared shaft/tip meshes: one cylinder for the shaft, one
    // cone for the tip. Materials differ per axis.
    let shaft_mesh = meshes.add(Cylinder::new(ARROW_RADIUS, ARROW_LENGTH - TIP_LENGTH));
    let tip_mesh = meshes.add(Cone::new(TIP_RADIUS, TIP_LENGTH));

    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let material = materials.add(StandardMaterial {
            base_color: axis_color(axis),
            emissive: axis_emissive(axis),
            perceptual_roughness: 0.35,
            metallic: 0.0,
            unlit: false,
            ..default()
        });
        // Group entity: transform + visibility for the whole arrow.
        let arrow = commands
            .spawn((
                MoveArrow { axis },
                Transform::default(),
                Visibility::Hidden,
                InheritedVisibility::default(),
            ))
            .id();
        commands.entity(root).add_child(arrow);

        // Shaft: centred halfway up the length minus the tip.
        let shaft_len = ARROW_LENGTH - TIP_LENGTH;
        let shaft = commands
            .spawn((
                Mesh3d(shaft_mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(Vec3::Y * (shaft_len * 0.5)),
            ))
            .id();
        commands.entity(arrow).add_child(shaft);

        // Tip: sits on top of the shaft. Cone's centroid is at
        // its base + half its height along Y (Bevy convention).
        let tip = commands
            .spawn((
                Mesh3d(tip_mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(
                    Vec3::Y * (shaft_len + TIP_LENGTH * 0.5),
                ),
            ))
            .id();
        commands.entity(arrow).add_child(tip);

        // Rotate the whole arrow onto its target axis.
        commands
            .entity(arrow)
            .insert(Transform::from_rotation(axis_rotation(axis)));
    }
}

/// Show / hide the arrows based on tool + selection, and reposition
/// them at the selected component's AABB centroid. Piece-local
/// coordinates only — the arrows are children of `WorkpieceRoot`,
/// so the turntable rotation applies automatically.
///
/// When a gizmo drag is in flight, the arrows follow the *live*
/// preview position: the pre-drag centroid plus this frame's
/// voxel-snapped delta. Using stale cached labels here caused the
/// "arrows sometimes don't follow the sphere" bug — the piece had
/// moved but the arrows stayed pinned to its pre-drag pose.
#[allow(clippy::too_many_arguments)]
fn update_gizmo_visibility_and_transform(
    tool: Res<SculptTool>,
    mut selection: ResMut<Selection>,
    workpiece: Res<LayersState>,
    drag: Res<MoveGizmoDrag>,
    state: Res<MoveState>,
    mut q_arrows: Query<(&MoveArrow, &mut Transform, &mut Visibility)>,
) {
    let should_show = matches!(tool.kind, ToolKind::Move) && selection.picked_voxel.is_some();
    if !should_show {
        for (_, _, mut vis) in &mut q_arrows {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        }
        return;
    }

    let centre_local = if let Some(active) = drag.active.as_ref() {
        // During a drag: use the snapshot's bounds + the actual
        // (voxel-snapped) delta being previewed this frame, so
        // the arrows sit exactly on the moving piece.
        centre_from_drag(active, &workpiece, &state.pending_mm)
    } else {
        piece_local_centroid_of_selection(&mut selection, &workpiece)
    };

    let Some(centre_local) = centre_local else {
        // Selection points at nothing (piece was carved away since
        // the last label refresh); hide the gizmo until the user
        // repicks.
        for (_, _, mut vis) in &mut q_arrows {
            *vis = Visibility::Hidden;
        }
        return;
    };
    for (arrow, mut tf, mut vis) in &mut q_arrows {
        tf.translation = Vec3::new(centre_local.x, centre_local.y, centre_local.z);
        tf.rotation = axis_rotation(arrow.axis);
        if *vis != Visibility::Visible {
            *vis = Visibility::Visible;
        }
    }
}

/// Compute the piece-local centroid of the piece as it appears on
/// this frame, given the active drag: snapshot bounds shifted by
/// the current voxel-snapped pending delta.
fn centre_from_drag(
    active: &ActiveDrag,
    workpiece: &LayersState,
    pending_mm: &Vec3,
) -> Option<GVec3> {
    let (mn, mx) = active.labels.bounds_of(active.component_id)?;
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    // Snap the mm delta to voxels so we track the *actual*
    // translation, not the widget's raw millimetre value.
    let dx = (pending_mm.x / vs).round() * vs;
    let dy = (pending_mm.y / vs).round() * vs;
    let dz = (pending_mm.z / vs).round() * vs;
    Some(GVec3::new(
        origin.x + (mn.x + mx.x) as f32 * 0.5 * vs + dx,
        origin.y + (mn.y + mx.y) as f32 * 0.5 * vs + dy,
        origin.z + (mn.z + mx.z) as f32 * 0.5 * vs + dz,
    ))
}

fn piece_local_centroid_of_selection(
    selection: &mut Selection,
    workpiece: &LayersState,
) -> Option<GVec3> {
    // Same "temporarily own the labels" pattern used elsewhere.
    let labels = ensure_labels_owned(selection, workpiece)?;
    let id = selection.selected_id(&labels);
    let bounds = id.and_then(|id| labels.bounds_of(id));
    // Put the labels back before returning.
    selection.set_labels(labels);
    let (mn, mx) = bounds?;
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    let cx = origin.x + (mn.x + mx.x) as f32 * 0.5 * vs;
    let cy = origin.y + (mn.y + mx.y) as f32 * 0.5 * vs;
    let cz = origin.z + (mn.z + mx.z) as f32 * 0.5 * vs;
    Some(GVec3::new(cx, cy, cz))
}

/// Small helper: return the current label field, computing it if
/// stale. The caller must put the field back via `set_labels`
/// before another selection-consumer sees the resource.
fn ensure_labels_owned(
    selection: &mut Selection,
    workpiece: &LayersState,
) -> Option<ComponentField> {
    if selection.labels().is_none() {
        let fresh = label_components(workpiece.grid());
        selection.set_labels(fresh);
    }
    selection.take_labels()
}

/// Handle mouse interaction with the axis arrows.
///
/// Runs after `TurntableSet` so this frame's Q/E rotation is baked
/// into `WorkpieceRoot`'s transform when we build the cursor ray.
/// Runs before `selection::selection_input` (both order-chained via
/// the plugin) so an axis-consuming click doesn't also fire a
/// `Select`-style pick.
#[allow(clippy::too_many_arguments)]
fn gizmo_pointer_input(
    buttons: Res<ButtonInput<MouseButton>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&Transform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    ui_gate: Res<UiCapturesInput>,
    mut selection: ResMut<Selection>,
    mut workpiece: ResMut<LayersState>,
    mut history: ResMut<UndoHistory>,
    mut state: ResMut<MoveState>,
    mut drag: ResMut<MoveGizmoDrag>,
    q_arrows: Query<&MoveArrow>,
) {
    if !matches!(tool.kind, ToolKind::Move) {
        // If we were dragging when the user swapped tools, cancel
        // the drag and revert the grid so we don't leave a
        // half-committed preview stuck on screen.
        if let Some(active) = drag.active.take() {
            cancel_drag(active, &mut workpiece, &mut state);
        }
        return;
    }

    // LMB release: commit whatever's showing right now.
    if buttons.just_released(MouseButton::Left) {
        if let Some(active) = drag.active.take() {
            commit_drag(
                active,
                &state.pending_mm,
                &mut workpiece,
                &mut selection,
                &mut history,
            );
            // Zero the widget: the piece is now at its new
            // position, so "pending nudge" is 0. Leaving the last
            // drag's mm value in the widget would cause the next
            // Apply / arrow-tap to re-apply it — the source of
            // the "drag 10 mm then click and it moves another 10
            // mm" bug.
            state.pending_mm = Vec3::ZERO;
        }
        return;
    }

    // Continue an in-flight drag.
    if drag.active.is_some() {
        update_drag(
            &mut drag,
            &q_window,
            &q_camera,
            &q_piece,
            &workpiece,
            &mut state,
        );
        // Apply the running total to the grid.
        apply_preview(&mut drag, &mut workpiece, &state);
        return;
    }

    if ui_gate.pointer {
        return;
    }
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    if selection.picked_voxel.is_none() {
        return;
    }

    // Try to start a new drag. We need the cursor ray in piece-local
    // coords and the piece-local centroid to test each axis.
    let Ok(window) = q_window.get_single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_tf)) = q_camera.get_single() else {
        return;
    };
    let Ok(piece_tf) = q_piece.get_single() else {
        return;
    };
    let Ok(ray_world) = camera.viewport_to_world(cam_tf, cursor) else {
        return;
    };
    let piece_inv = piece_tf.compute_affine().inverse();
    let ray_origin_local = piece_inv.transform_point3(ray_world.origin);
    let ray_dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();

    let Some(centre) = piece_local_centroid_of_selection(&mut selection, &workpiece) else {
        return;
    };

    // Test each visible arrow, keeping the smallest-distance hit.
    let mut best: Option<(Axis, f32, f32)> = None; // (axis, distance, t)
    for arrow in q_arrows.iter() {
        let axis_dir = arrow.axis.dir_local();
        let Some((distance, t_ray, t_axis)) = ray_line_closest(
            GVec3::new(
                ray_origin_local.x,
                ray_origin_local.y,
                ray_origin_local.z,
            ),
            GVec3::new(ray_dir_local.x, ray_dir_local.y, ray_dir_local.z),
            centre,
            axis_dir,
        ) else {
            continue;
        };
        // Only pick if the closest approach is on the arrow's
        // finite length (0..ARROW_LENGTH) and in front of the
        // camera (t_ray > 0). t_axis is measured from the centroid;
        // negative values are on the far-side "tail" which we do
        // not render, so ignore them.
        if t_ray < 0.0 || !(0.0..=ARROW_LENGTH).contains(&t_axis) {
            continue;
        }
        if distance > PICK_TOLERANCE {
            continue;
        }
        match best {
            None => best = Some((arrow.axis, distance, t_axis)),
            Some((_, d, _)) if distance < d => {
                best = Some((arrow.axis, distance, t_axis))
            }
            _ => {}
        }
    }

    let Some((axis, _, t_start)) = best else {
        return;
    };

    // Kick off a drag.
    let labels = label_components(workpiece.grid());
    let Some(component_id) = selection.selected_id(&labels) else {
        // Bail: nothing to move.
        return;
    };
    // Region-scoped snapshot of just the component's own (widened)
    // bounds at zero delta; `ensure_region_covers` grows this as the
    // drag reaches farther, so we never pay a full-domain copy here.
    let Some((region_min, region_max)) = touched_region_for_translate(
        &labels,
        component_id,
        IVec3::ZERO,
        workpiece.grid().res(),
    ) else {
        // No bounds for the selected component — shouldn't happen
        // given `selected_id` just succeeded, but bail cleanly.
        return;
    };
    let snapshot = workpiece.grid().snapshot_region(region_min, region_max);
    let active = ActiveDrag {
        axis,
        t_start,
        initial_widget_mm: state.pending_mm,
        grid_snapshot: snapshot,
        labels,
        component_id,
        voxel_size: workpiece.grid().voxel_size(),
        dirty_chunks: HashSet::new(),
        last_applied_vox: IVec3::ZERO,
    };
    drag.active = Some(active);
    drag.started_this_frame = true;
    info!("move: drag started on {:?} axis", axis);
}

fn update_drag(
    drag: &mut MoveGizmoDrag,
    q_window: &Query<&Window, With<PrimaryWindow>>,
    q_camera: &Query<(&Camera, &GlobalTransform)>,
    q_piece: &Query<&Transform, With<WorkpieceRoot>>,
    workpiece: &LayersState,
    state: &mut MoveState,
) {
    let Some(active) = drag.active.as_ref() else {
        return;
    };
    let Ok(window) = q_window.get_single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_tf)) = q_camera.get_single() else {
        return;
    };
    let Ok(piece_tf) = q_piece.get_single() else {
        return;
    };
    let Ok(ray_world) = camera.viewport_to_world(cam_tf, cursor) else {
        return;
    };
    let piece_inv = piece_tf.compute_affine().inverse();
    let ray_origin_local = piece_inv.transform_point3(ray_world.origin);
    let ray_dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();

    // Recompute centre from the pre-drag labels/bounds, not the
    // live selection labels (which we may have invalidated).
    let (mn, mx) = active
        .labels
        .bounds_of(active.component_id)
        .expect("component has bounds; captured at drag start");
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    let centre = GVec3::new(
        origin.x + (mn.x + mx.x) as f32 * 0.5 * vs,
        origin.y + (mn.y + mx.y) as f32 * 0.5 * vs,
        origin.z + (mn.z + mx.z) as f32 * 0.5 * vs,
    );

    let Some((_dist, _t_ray, t_now)) = ray_line_closest(
        GVec3::new(ray_origin_local.x, ray_origin_local.y, ray_origin_local.z),
        GVec3::new(ray_dir_local.x, ray_dir_local.y, ray_dir_local.z),
        centre,
        active.axis.dir_local(),
    ) else {
        return;
    };
    let delta_axis = t_now - active.t_start;

    // Merge the drag delta into the widget's pending value on
    // this axis; other axes keep whatever the user typed.
    let mut new_mm = active.initial_widget_mm;
    new_mm[active.axis.index()] += delta_axis;
    state.pending_mm = new_mm;
}

/// Ensure `active.grid_snapshot` covers everywhere a translate by
/// `delta` could read from or write to.
///
/// The common case (the drag hasn't grown past its current reach) is
/// a cheap bounds check and nothing else. Growing is still O(region),
/// never O(domain): the *currently* snapshotted box is first restored
/// to pristine (undoing whatever this drag has previewed there so
/// far), which — since nothing outside that box has ever been
/// written to during this drag — leaves the *entire* grid pristine.
/// A fresh, bigger snapshot covering the union of the old box and the
/// newly-required one is then safe to take straight from the live
/// grid.
fn ensure_region_covers(active: &mut ActiveDrag, grid: &mut Grid, delta: IVec3) {
    let Some((need_min, need_max)) =
        touched_region_for_translate(&active.labels, active.component_id, delta, grid.res())
    else {
        return;
    };
    let (cur_min, cur_max) = active.grid_snapshot.bounds();
    let already_covered = need_min.x >= cur_min.x
        && need_min.y >= cur_min.y
        && need_min.z >= cur_min.z
        && need_max.x <= cur_max.x
        && need_max.y <= cur_max.y
        && need_max.z <= cur_max.z;
    if already_covered {
        return;
    }
    grid.restore_region(&active.grid_snapshot);
    let new_min = cur_min.min(need_min);
    let new_max = cur_max.max(need_max);
    active.grid_snapshot = grid.snapshot_region(new_min, new_max);
}

fn apply_preview(
    drag: &mut MoveGizmoDrag,
    workpiece: &mut LayersState,
    state: &MoveState,
) {
    let Some(active) = drag.active.as_mut() else {
        return;
    };
    let vs = workpiece.grid().voxel_size();
    let target_delta_vox = IVec3::new(
        (state.pending_mm.x / vs).round() as i32,
        (state.pending_mm.y / vs).round() as i32,
        (state.pending_mm.z / vs).round() as i32,
    );
    // Reset the grid from the snapshot every frame; then translate
    // by the total offset. That way we don't accumulate rounding
    // error and undo doesn't need to record intermediate states.
    ensure_region_covers(active, workpiece.grid_mut(), target_delta_vox);
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    let dirty = translate_component(
        workpiece.grid_mut(),
        &active.labels,
        active.component_id,
        target_delta_vox,
        |_, _, _, _| {},
    );
    let grid_res = workpiece.grid().res();
    if let Some(region) = dirty {
        for c in region.touched_chunks(grid_res) {
            active.dirty_chunks.insert((c.x, c.y, c.z));
        }
    }
    // Also dirty everything we've ever touched during this drag,
    // so chunks we vacated get re-meshed to their reset state.
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
    active.last_applied_vox = target_delta_vox;
}

fn commit_drag(
    mut active: ActiveDrag,
    final_pending_mm: &Vec3,
    workpiece: &mut ResMut<LayersState>,
    selection: &mut ResMut<Selection>,
    history: &mut ResMut<UndoHistory>,
) {
    let vs = workpiece.grid().voxel_size();
    let final_delta_vox = IVec3::new(
        (final_pending_mm.x / vs).round() as i32,
        (final_pending_mm.y / vs).round() as i32,
        (final_pending_mm.z / vs).round() as i32,
    );
    // Reset one last time so the recorder sees pre-values from the
    // true pre-drag state, not the intermediate preview. The region
    // should already cover this delta from the last preview frame,
    // but re-check defensively — cheap when it's already covered.
    ensure_region_covers(&mut active, workpiece.grid_mut(), final_delta_vox);
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    if final_delta_vox == IVec3::ZERO {
        info!("move: drag ended with zero-voxel offset — nothing to commit");
        // Ensure the reset chunks get re-meshed.
        for &(x, y, z) in &active.dirty_chunks {
            workpiece.mark_dirty((x, y, z));
        }
        selection.invalidate_labels();
        return;
    }

    let mut recorder = StrokeRecorder::default();
    let dirty = translate_component(
        workpiece.grid_mut(),
        &active.labels,
        active.component_id,
        final_delta_vox,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );
    let grid_res = workpiece.grid().res();
    if let Some(region) = dirty {
        recorder.record_dirty_region(region, grid_res);
        for c in region.touched_chunks(grid_res) {
            let ChunkCoord { x, y, z } = c;
            workpiece.mark_dirty((x, y, z));
        }
    }
    // Also re-mesh every chunk we vacated during the preview so
    // the reset from the snapshot actually shows up.
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
    }

    // Shift the picked voxel with the piece so the selection HUD
    // and highlight follow the new position.
    if let Some((x, y, z)) = selection.picked_voxel {
        let nx = x as i32 + final_delta_vox.x;
        let ny = y as i32 + final_delta_vox.y;
        let nz = z as i32 + final_delta_vox.z;
        if nx >= 0
            && ny >= 0
            && nz >= 0
            && nx < grid_res.x as i32
            && ny < grid_res.y as i32
            && nz < grid_res.z as i32
        {
            selection.picked_voxel = Some((nx as u32, ny as u32, nz as u32));
        } else {
            selection.picked_voxel = None;
        }
    }
    selection.invalidate_labels();

    info!(
        "moved selection by ({:.1}, {:.1}, {:.1}) mm ({} voxels)",
        final_pending_mm.x, final_pending_mm.y, final_pending_mm.z, final_delta_vox
    );
}

fn cancel_drag(
    active: ActiveDrag,
    workpiece: &mut ResMut<LayersState>,
    state: &mut ResMut<MoveState>,
) {
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
    state.pending_mm = active.initial_widget_mm;
    info!("move: drag cancelled — grid reset");
}

/// Solve for the closest approach between a ray (`ray_origin +
/// t_r * ray_dir`) and an infinite line (`line_origin + t_l *
/// line_dir`). Returns `(distance, t_r, t_l)` or `None` when the
/// two are near-parallel.
fn ray_line_closest(
    ray_origin: GVec3,
    ray_dir: GVec3,
    line_origin: GVec3,
    line_dir: GVec3,
) -> Option<(f32, f32, f32)> {
    let u = ray_dir;
    let v = line_dir;
    let w = ray_origin - line_origin;
    let a = u.dot(u);
    let b = u.dot(v);
    let c = v.dot(v);
    let d = u.dot(w);
    let e = v.dot(w);
    let denom = a * c - b * b;
    if denom.abs() < 1e-6 {
        return None;
    }
    let t_r = (b * e - c * d) / denom;
    let t_l = (a * e - b * d) / denom;
    let point_r = ray_origin + u * t_r;
    let point_l = line_origin + v * t_l;
    let distance = (point_r - point_l).length();
    Some((distance, t_r, t_l))
}

/// Consume the widget's Apply command. Only runs when *not* mid-
/// gizmo-drag: the drag has its own commit path. That way a click
/// on Apply while an axis is being dragged doesn't double-apply.
fn handle_move_action(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<LayersState>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut state: ResMut<MoveState>,
    drag: Res<MoveGizmoDrag>,
) {
    for a in events.read() {
        if let AppAction::MoveSelection(delta_mm) = a {
            if drag.is_active() {
                continue;
            }
            let applied = apply_move(
                *delta_mm,
                &mut workpiece,
                &mut selection,
                &mut history,
                &mut stroke,
            );
            if applied {
                state.pending_mm = Vec3::ZERO;
            }
        }
    }
}

fn clear_frame_flags(mut drag: ResMut<MoveGizmoDrag>) {
    drag.started_this_frame = false;
}

fn apply_move(
    delta_mm: Vec3,
    workpiece: &mut LayersState,
    selection: &mut Selection,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) -> bool {
    if selection.picked_voxel.is_none() {
        info!("move: nothing selected");
        return false;
    }
    if delta_mm.length_squared() < 1e-6 {
        return false;
    }
    stroke.discard_live();

    let vs = workpiece.grid().voxel_size();
    let delta_vox = IVec3::new(
        (delta_mm.x / vs).round() as i32,
        (delta_mm.y / vs).round() as i32,
        (delta_mm.z / vs).round() as i32,
    );
    if delta_vox == IVec3::ZERO {
        info!(
            "move: delta {:?} mm rounds to zero voxels — bump it up",
            delta_mm
        );
        return false;
    }

    let labels = label_components(workpiece.grid());
    let id = match selection.selected_id(&labels) {
        Some(id) => id,
        None => {
            info!("move: selected piece has been carved away");
            return false;
        }
    };
    let old_voxel = selection.picked_voxel;

    let mut recorder = StrokeRecorder::default();
    let dirty = translate_component(
        workpiece.grid_mut(),
        &labels,
        id,
        delta_vox,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );
    let grid_res = workpiece.grid().res();
    if let Some(region) = dirty {
        recorder.record_dirty_region(region, grid_res);
        for c in region.touched_chunks(grid_res) {
            let ChunkCoord { x, y, z } = c;
            workpiece.mark_dirty((x, y, z));
        }
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
    }
    selection.invalidate_labels();
    selection.picked_voxel = old_voxel.and_then(|(x, y, z)| {
        let nx = x as i32 + delta_vox.x;
        let ny = y as i32 + delta_vox.y;
        let nz = z as i32 + delta_vox.z;
        if nx < 0
            || ny < 0
            || nz < 0
            || nx >= grid_res.x as i32
            || ny >= grid_res.y as i32
            || nz >= grid_res.z as i32
        {
            None
        } else {
            Some((nx as u32, ny as u32, nz as u32))
        }
    });
    info!(
        "moved selection by ({:.1}, {:.1}, {:.1}) mm ({} voxels)",
        delta_mm.x, delta_mm.y, delta_mm.z, delta_vox
    );
    true
}
