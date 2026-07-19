//! Rotate tool — rigidly rotate the selected component by 90° steps.
//!
//! Lattice-preserving (same lossless bar as Move): angles snap to
//! multiples of 90° about the piece's AABB centre. Two entry points:
//!
//! - HUD widget: X / Y / Z degrees + Apply / Reset (±90 buttons).
//! - 3D axis **rings** at the selection centroid: drag around a ring
//!   to preview; release commits one undo stroke.
//!
//! Live preview mirrors Move: growable region snapshot + frozen
//! labels; each frame restores then re-applies `rotate_component`.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::{IVec3, Vec3 as GVec3};
use sculpt_core::{
    label_components, rotate_component, rotate_voxel, touched_region_for_rotate, ChunkCoord,
    ComponentField, ComponentId, Grid, RegionSnapshot,
};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::sculpt::{SculptTool, ToolKind};
use crate::selection::Selection;
use crate::turntable::TurntableSet;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{LayersState, WorkpieceRoot};

/// Pending rotation in degrees about piece-local X / Y / Z.
/// Snapped to multiples of 90° when previewing / committing.
#[derive(Resource, Default)]
pub struct RotateState {
    pub pending_deg: Vec3,
}

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
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

#[derive(Component)]
struct RotateRing {
    axis: Axis,
}

#[derive(Resource, Default)]
pub struct RotateGizmoDrag {
    active: Option<ActiveDrag>,
    pub started_this_frame: bool,
}

impl RotateGizmoDrag {
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Pre-drag AABB plus this frame's snapped quarters — used so the
    /// selection highlight can follow the previewed pose.
    pub fn active_drag_bounds_and_quarters(
        &self,
        state: &RotateState,
    ) -> Option<(glam::UVec3, glam::UVec3, IVec3, IVec3)> {
        let active = self.active.as_ref()?;
        let (mn, mx) = active.labels.bounds_of(active.component_id)?;
        let quarters = deg_to_quarters(state.pending_deg);
        let pivot = sculpt_core::component_pivot(&active.labels, active.component_id)?;
        Some((mn, mx, quarters, pivot))
    }
}

struct ActiveDrag {
    axis: Axis,
    angle_start: f32,
    initial_widget_deg: Vec3,
    grid_snapshot: RegionSnapshot,
    labels: ComponentField,
    component_id: ComponentId,
    dirty_chunks: HashSet<(u32, u32, u32)>,
}

#[derive(SystemSet, Debug, Clone, Hash, PartialEq, Eq)]
pub struct RotateGizmoInputSet;

/// Ring radius in piece-local mm.
const RING_RADIUS: f32 = 42.0;
const RING_TUBE: f32 = 1.4;
const PICK_TOLERANCE: f32 = 8.0;

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

/// Bevy's `Torus` lies in the XZ plane (Y = up through the hole).
/// Rotate so the hole axis aligns with `axis`.
fn ring_rotation(axis: Axis) -> Quat {
    match axis {
        Axis::X => Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
        Axis::Y => Quat::IDENTITY,
        Axis::Z => Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
    }
}

pub fn deg_to_quarters(deg: Vec3) -> IVec3 {
    IVec3::new(
        (deg.x / 90.0).round() as i32,
        (deg.y / 90.0).round() as i32,
        (deg.z / 90.0).round() as i32,
    )
}

pub fn plugin(app: &mut App) {
    app.init_resource::<RotateState>();
    app.init_resource::<RotateGizmoDrag>();
    app.add_systems(PostStartup, spawn_ring_gizmo);
    app.add_systems(
        Update,
        (
            update_gizmo_visibility_and_transform,
            gizmo_pointer_input.in_set(RotateGizmoInputSet),
            handle_rotate_action,
            clear_frame_flags,
        )
            .chain()
            .after(TurntableSet),
    );
}

fn spawn_ring_gizmo(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    q_root: Query<Entity, With<WorkpieceRoot>>,
) {
    let Ok(root) = q_root.get_single() else {
        return;
    };
    let torus = meshes.add(Torus::new(RING_RADIUS - RING_TUBE, RING_RADIUS + RING_TUBE));
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let material = materials.add(StandardMaterial {
            base_color: axis_color(axis),
            emissive: axis_emissive(axis),
            perceptual_roughness: 0.35,
            metallic: 0.0,
            ..default()
        });
        let ring = commands
            .spawn((
                RotateRing { axis },
                Mesh3d(torus.clone()),
                MeshMaterial3d(material),
                Transform::from_rotation(ring_rotation(axis)),
                Visibility::Hidden,
            ))
            .id();
        commands.entity(root).add_child(ring);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_gizmo_visibility_and_transform(
    tool: Res<SculptTool>,
    mut selection: ResMut<Selection>,
    workpiece: Res<LayersState>,
    drag: Res<RotateGizmoDrag>,
    state: Res<RotateState>,
    mut q_rings: Query<(&RotateRing, &mut Transform, &mut Visibility)>,
) {
    let should_show = matches!(tool.kind, ToolKind::Rotate) && selection.picked_voxel.is_some();
    if !should_show {
        for (_, _, mut vis) in &mut q_rings {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        }
        return;
    }

    let centre_local = if let Some(active) = drag.active.as_ref() {
        centre_from_drag(active, &workpiece, &state.pending_deg)
    } else {
        piece_local_centroid_of_selection(&mut selection, &workpiece)
    };

    let Some(centre_local) = centre_local else {
        for (_, _, mut vis) in &mut q_rings {
            *vis = Visibility::Hidden;
        }
        return;
    };

    let quarters = deg_to_quarters(state.pending_deg);
    // Visual spin of each ring while previewing that axis.
    for (ring, mut tf, mut vis) in &mut q_rings {
        tf.translation = Vec3::new(centre_local.x, centre_local.y, centre_local.z);
        let spin = match ring.axis {
            Axis::X => Quat::from_rotation_x(quarters.x as f32 * std::f32::consts::FRAC_PI_2),
            Axis::Y => Quat::from_rotation_y(quarters.y as f32 * std::f32::consts::FRAC_PI_2),
            Axis::Z => Quat::from_rotation_z(quarters.z as f32 * std::f32::consts::FRAC_PI_2),
        };
        tf.rotation = spin * ring_rotation(ring.axis);
        if *vis != Visibility::Visible {
            *vis = Visibility::Visible;
        }
    }
}

fn centre_from_drag(
    active: &ActiveDrag,
    workpiece: &LayersState,
    pending_deg: &Vec3,
) -> Option<GVec3> {
    let (mn, mx) = active.labels.bounds_of(active.component_id)?;
    let pivot = sculpt_core::component_pivot(&active.labels, active.component_id)?;
    let quarters = deg_to_quarters(*pending_deg);
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    // Rotate the AABB centre voxel about the pivot for ring placement.
    let centre_vox = IVec3::new(
        ((mn.x + mx.x) / 2) as i32,
        ((mn.y + mx.y) / 2) as i32,
        ((mn.z + mx.z) / 2) as i32,
    );
    let rotated = rotate_voxel(centre_vox, pivot, quarters);
    Some(GVec3::new(
        origin.x + rotated.x as f32 * vs + vs * 0.5,
        origin.y + rotated.y as f32 * vs + vs * 0.5,
        origin.z + rotated.z as f32 * vs + vs * 0.5,
    ))
}

fn piece_local_centroid_of_selection(
    selection: &mut Selection,
    workpiece: &LayersState,
) -> Option<GVec3> {
    let labels = ensure_labels_owned(selection, workpiece)?;
    let id = selection.selected_id(&labels);
    let bounds = id.and_then(|id| labels.bounds_of(id));
    selection.set_labels(labels);
    let (mn, mx) = bounds?;
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    Some(GVec3::new(
        origin.x + (mn.x + mx.x) as f32 * 0.5 * vs,
        origin.y + (mn.y + mx.y) as f32 * 0.5 * vs,
        origin.z + (mn.z + mx.z) as f32 * 0.5 * vs,
    ))
}

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
    mut state: ResMut<RotateState>,
    mut drag: ResMut<RotateGizmoDrag>,
) {
    if !matches!(tool.kind, ToolKind::Rotate) {
        if let Some(active) = drag.active.take() {
            cancel_drag(active, &mut workpiece, &mut state);
        }
        return;
    }

    if buttons.just_released(MouseButton::Left) {
        if let Some(active) = drag.active.take() {
            commit_drag(
                active,
                &state.pending_deg,
                &mut workpiece,
                &mut selection,
                &mut history,
            );
            state.pending_deg = Vec3::ZERO;
        }
        return;
    }

    if drag.active.is_some() {
        update_drag(
            &mut drag,
            &q_window,
            &q_camera,
            &q_piece,
            &workpiece,
            &mut state,
        );
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
    let ray_o = GVec3::new(ray_origin_local.x, ray_origin_local.y, ray_origin_local.z);
    let ray_d = GVec3::new(ray_dir_local.x, ray_dir_local.y, ray_dir_local.z);

    let Some(centre) = piece_local_centroid_of_selection(&mut selection, &workpiece) else {
        return;
    };

    let mut best: Option<(Axis, f32, f32)> = None; // axis, dist, angle
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let Some((dist, _t, angle)) =
            ray_circle_hit(ray_o, ray_d, centre, axis.dir_local(), RING_RADIUS)
        else {
            continue;
        };
        if dist > PICK_TOLERANCE {
            continue;
        }
        match best {
            None => best = Some((axis, dist, angle)),
            Some((_, d, _)) if dist < d => best = Some((axis, dist, angle)),
            _ => {}
        }
    }
    let Some((axis, _, angle_start)) = best else {
        return;
    };

    let labels = label_components(workpiece.grid());
    let Some(component_id) = selection.selected_id(&labels) else {
        return;
    };
    let Some((region_min, region_max)) = touched_region_for_rotate(
        &labels,
        component_id,
        IVec3::ZERO,
        workpiece.grid().res(),
    ) else {
        return;
    };
    let snapshot = workpiece.grid().snapshot_region(region_min, region_max);
    drag.active = Some(ActiveDrag {
        axis,
        angle_start,
        initial_widget_deg: state.pending_deg,
        grid_snapshot: snapshot,
        labels,
        component_id,
        dirty_chunks: HashSet::new(),
    });
    drag.started_this_frame = true;
    info!("rotate: drag started on {:?} axis", axis);
}

fn update_drag(
    drag: &mut RotateGizmoDrag,
    q_window: &Query<&Window, With<PrimaryWindow>>,
    q_camera: &Query<(&Camera, &GlobalTransform)>,
    q_piece: &Query<&Transform, With<WorkpieceRoot>>,
    workpiece: &LayersState,
    state: &mut RotateState,
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
    let (mn, mx) = active
        .labels
        .bounds_of(active.component_id)
        .expect("bounds");
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    let centre = GVec3::new(
        origin.x + (mn.x + mx.x) as f32 * 0.5 * vs,
        origin.y + (mn.y + mx.y) as f32 * 0.5 * vs,
        origin.z + (mn.z + mx.z) as f32 * 0.5 * vs,
    );
    let Some((_dist, _t, angle_now)) = ray_circle_hit(
        GVec3::new(ray_origin_local.x, ray_origin_local.y, ray_origin_local.z),
        GVec3::new(ray_dir_local.x, ray_dir_local.y, ray_dir_local.z),
        centre,
        active.axis.dir_local(),
        RING_RADIUS,
    ) else {
        return;
    };
    let mut delta = angle_now - active.angle_start;
    // Wrap to (−π, π] so crossing the branch cut doesn't jump ±360°.
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta <= -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    let delta_deg = delta.to_degrees();
    let mut new_deg = active.initial_widget_deg;
    new_deg[active.axis.index()] += delta_deg;
    state.pending_deg = new_deg;
}

/// Closest approach from a ray to a circle in a plane.
/// Returns `(distance_to_circle, t_ray, angle)` where angle is in the
/// circle's plane measured from a stable tangent basis.
fn ray_circle_hit(
    ray_o: GVec3,
    ray_d: GVec3,
    centre: GVec3,
    axis: GVec3,
    radius: f32,
) -> Option<(f32, f32, f32)> {
    let axis = axis.normalize_or_zero();
    if axis.length_squared() < 1e-8 {
        return None;
    }
    let denom = ray_d.dot(axis);
    if denom.abs() < 1e-5 {
        return None;
    }
    let t = (centre - ray_o).dot(axis) / denom;
    if t < 0.0 {
        return None;
    }
    let hit = ray_o + ray_d * t;
    let radial = hit - centre;
    let in_plane = radial - axis * radial.dot(axis);
    let len = in_plane.length();
    if len < 1e-6 {
        return None;
    }
    let dist = (len - radius).abs();
    // Orthonormal basis in the plane for a continuous angle.
    let mut u = GVec3::Y.cross(axis);
    if u.length_squared() < 1e-6 {
        u = GVec3::X.cross(axis);
    }
    let u = u.normalize_or_zero();
    let v = axis.cross(u);
    let dir = in_plane / len;
    let angle = v.dot(dir).atan2(u.dot(dir));
    Some((dist, t, angle))
}

fn ensure_region_covers(active: &mut ActiveDrag, grid: &mut Grid, quarters: IVec3) {
    let Some((need_min, need_max)) =
        touched_region_for_rotate(&active.labels, active.component_id, quarters, grid.res())
    else {
        return;
    };
    let (cur_min, cur_max) = active.grid_snapshot.bounds();
    let already = need_min.x >= cur_min.x
        && need_min.y >= cur_min.y
        && need_min.z >= cur_min.z
        && need_max.x <= cur_max.x
        && need_max.y <= cur_max.y
        && need_max.z <= cur_max.z;
    if already {
        return;
    }
    grid.restore_region(&active.grid_snapshot);
    let new_min = cur_min.min(need_min);
    let new_max = cur_max.max(need_max);
    active.grid_snapshot = grid.snapshot_region(new_min, new_max);
}

fn apply_preview(
    drag: &mut RotateGizmoDrag,
    workpiece: &mut LayersState,
    state: &RotateState,
) {
    let Some(active) = drag.active.as_mut() else {
        return;
    };
    let quarters = deg_to_quarters(state.pending_deg);
    ensure_region_covers(active, workpiece.grid_mut(), quarters);
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    let dirty = rotate_component(
        workpiece.grid_mut(),
        &active.labels,
        active.component_id,
        quarters,
        |_, _, _, _| {},
    );
    let grid_res = workpiece.grid().res();
    if let Some(region) = dirty {
        for c in region.touched_chunks(grid_res) {
            active.dirty_chunks.insert((c.x, c.y, c.z));
        }
    }
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
}

fn commit_drag(
    mut active: ActiveDrag,
    final_pending_deg: &Vec3,
    workpiece: &mut ResMut<LayersState>,
    selection: &mut ResMut<Selection>,
    history: &mut ResMut<UndoHistory>,
) {
    let quarters = deg_to_quarters(*final_pending_deg);
    ensure_region_covers(&mut active, workpiece.grid_mut(), quarters);
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    if quarters == IVec3::ZERO
        || (quarters.x.rem_euclid(4) == 0
            && quarters.y.rem_euclid(4) == 0
            && quarters.z.rem_euclid(4) == 0)
    {
        for &(x, y, z) in &active.dirty_chunks {
            workpiece.mark_dirty((x, y, z));
        }
        selection.invalidate_labels();
        info!("rotate: drag ended with identity — nothing to commit");
        return;
    }

    let pivot = sculpt_core::component_pivot(&active.labels, active.component_id);
    let mut recorder = StrokeRecorder::default();
    let dirty = rotate_component(
        workpiece.grid_mut(),
        &active.labels,
        active.component_id,
        quarters,
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
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
    }

    if let (Some((x, y, z)), Some(pivot)) = (selection.picked_voxel, pivot) {
        let mapped = rotate_voxel(IVec3::new(x as i32, y as i32, z as i32), pivot, quarters);
        if mapped.x >= 0
            && mapped.y >= 0
            && mapped.z >= 0
            && mapped.x < grid_res.x as i32
            && mapped.y < grid_res.y as i32
            && mapped.z < grid_res.z as i32
        {
            selection.picked_voxel = Some((mapped.x as u32, mapped.y as u32, mapped.z as u32));
        } else {
            selection.picked_voxel = None;
        }
    }
    selection.invalidate_labels();
    info!(
        "rotated selection by ({:.0}, {:.0}, {:.0})° (quarters {:?})",
        final_pending_deg.x, final_pending_deg.y, final_pending_deg.z, quarters
    );
}

fn cancel_drag(
    active: ActiveDrag,
    workpiece: &mut ResMut<LayersState>,
    state: &mut ResMut<RotateState>,
) {
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
    state.pending_deg = active.initial_widget_deg;
    info!("rotate: drag cancelled — grid reset");
}

fn handle_rotate_action(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<LayersState>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut state: ResMut<RotateState>,
    drag: Res<RotateGizmoDrag>,
) {
    for a in events.read() {
        if let AppAction::RotateSelection(deg) = a {
            if drag.is_active() {
                continue;
            }
            if apply_rotate(
                *deg,
                &mut workpiece,
                &mut selection,
                &mut history,
                &mut stroke,
            ) {
                state.pending_deg = Vec3::ZERO;
            }
        }
    }
}

fn clear_frame_flags(mut drag: ResMut<RotateGizmoDrag>) {
    drag.started_this_frame = false;
}

fn apply_rotate(
    deg: Vec3,
    workpiece: &mut LayersState,
    selection: &mut Selection,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) -> bool {
    if selection.picked_voxel.is_none() {
        info!("rotate: nothing selected");
        return false;
    }
    let quarters = deg_to_quarters(deg);
    if quarters.x.rem_euclid(4) == 0
        && quarters.y.rem_euclid(4) == 0
        && quarters.z.rem_euclid(4) == 0
    {
        info!("rotate: degrees {:?} snap to identity", deg);
        return false;
    }
    stroke.discard_live();

    let labels = label_components(workpiece.grid());
    let id = match selection.selected_id(&labels) {
        Some(id) => id,
        None => {
            info!("rotate: selected piece has been carved away");
            return false;
        }
    };
    let pivot = match sculpt_core::component_pivot(&labels, id) {
        Some(p) => p,
        None => return false,
    };
    let old_voxel = selection.picked_voxel;

    let mut recorder = StrokeRecorder::default();
    let dirty = rotate_component(
        workpiece.grid_mut(),
        &labels,
        id,
        quarters,
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
        let mapped = rotate_voxel(IVec3::new(x as i32, y as i32, z as i32), pivot, quarters);
        if mapped.x < 0
            || mapped.y < 0
            || mapped.z < 0
            || mapped.x >= grid_res.x as i32
            || mapped.y >= grid_res.y as i32
            || mapped.z >= grid_res.z as i32
        {
            None
        } else {
            Some((mapped.x as u32, mapped.y as u32, mapped.z as u32))
        }
    });
    info!(
        "rotated selection by ({:.0}, {:.0}, {:.0})° (quarters {:?})",
        deg.x, deg.y, deg.z, quarters
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec3;
    use sculpt_core::{apply_primitive, Primitive, PrimitiveKind};

    #[test]
    fn apply_rotate_turns_box_about_y() {
        let mut workpiece = LayersState::new_for_test(Grid::empty(
            UVec3::new(64, 64, 64),
            1.0,
            glam::Vec3::ZERO,
        ));
        let _ = apply_primitive(
            workpiece.grid_mut(),
            &Primitive {
                kind: PrimitiveKind::Box {
                    half_extents: glam::Vec3::new(12.0, 4.0, 4.0),
                },
                center: glam::Vec3::new(32.0, 32.0, 32.0),
                workbench_y: None,
            },
        );
        let mut selection = Selection::default();
        selection.picked_voxel = Some((40, 32, 32));
        let mut history = UndoHistory::default();
        let mut stroke = SculptStroke::default();

        let applied = apply_rotate(
            Vec3::new(0.0, 90.0, 0.0),
            &mut workpiece,
            &mut selection,
            &mut history,
            &mut stroke,
        );
        assert!(applied);
        assert!(workpiece.grid().sample(glam::Vec3::new(40.0, 32.0, 32.0)) > 0.0);
        assert!(
            workpiece.grid().sample(glam::Vec3::new(32.0, 32.0, 24.0)) < 0.0
                || workpiece.grid().sample(glam::Vec3::new(32.0, 32.0, 20.0)) < 0.0
        );
        assert_eq!(history.undo_len_for_test(), 1);
    }

    #[test]
    fn apply_rotate_noop_without_selection() {
        let mut workpiece = LayersState::new_for_test(Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            glam::Vec3::ZERO,
            glam::Vec3::new(16.0, 16.0, 16.0),
            6.0,
        ));
        let mut selection = Selection::default();
        let mut history = UndoHistory::default();
        let mut stroke = SculptStroke::default();
        assert!(!apply_rotate(
            Vec3::new(0.0, 90.0, 0.0),
            &mut workpiece,
            &mut selection,
            &mut history,
            &mut stroke,
        ));
    }

    #[test]
    fn deg_to_quarters_snaps() {
        assert_eq!(deg_to_quarters(Vec3::new(80.0, -100.0, 10.0)), IVec3::new(1, -1, 0));
    }
}
