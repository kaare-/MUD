//! Rotate tool — rigidly rotate the selected component by 90° steps.
//!
//! Lattice-preserving (same lossless bar as Move): angles are always
//! multiples of 90° about the piece's AABB centre. Entry points:
//!
//! - HUD widget: quarter-step spinners + ±90 / Apply (honest 90° UI).
//! - 3D axis **rings** (scaled to the selection): drag crosses a 45°
//!   threshold → ±1 quarter; tick marks show the stops.
//! - Keyboard: `X`/`Y`/`Z` = +90°, `Shift+X/Y/Z` = −90° while Rotate
//!   is active.
//!
//! If a rotate would put solid below the workbench (`y < 0`), the
//! piece is lifted first so the turn stays lossless, then rotated —
//! one undo stroke for the whole op.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::{IVec3, UVec3, Vec3 as GVec3};
use sculpt_core::{
    component_pivot, label_components, rotate_component, rotate_voxel, touched_region_for_rotate,
    touched_region_for_translate, translate_component, ComponentField, ComponentId, Grid,
    RegionSnapshot,
};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::sculpt::{SculptTool, ToolKind};
use crate::selection::Selection;
use crate::turntable::TurntableSet;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{LayersState, WorkpieceRoot};

/// Pending rotation in **quarter-turns** about piece-local X / Y / Z
/// (1 = +90°). The widget and gizmo only ever set integer quarters.
#[derive(Resource, Default)]
pub struct RotateState {
    pub pending_quarters: IVec3,
    /// True when the current pending rotate would need a bench lift
    /// (or would have clipped without one). Shown in the widget.
    pub warn_below_bench: bool,
}

impl RotateState {
    pub fn pending_deg(&self) -> Vec3 {
        Vec3::new(
            self.pending_quarters.x as f32 * 90.0,
            self.pending_quarters.y as f32 * 90.0,
            self.pending_quarters.z as f32 * 90.0,
        )
    }
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

    pub fn active_drag_bounds_and_quarters(
        &self,
        state: &RotateState,
    ) -> Option<(UVec3, UVec3, IVec3, IVec3)> {
        let active = self.active.as_ref()?;
        let (mn, mx) = active.labels.bounds_of(active.component_id)?;
        let quarters = state.pending_quarters;
        let pivot = component_pivot(&active.labels, active.component_id)?;
        Some((mn, mx, quarters, pivot))
    }
}

struct ActiveDrag {
    axis: Axis,
    angle_start: f32,
    initial_quarters: IVec3,
    grid_snapshot: RegionSnapshot,
    labels: ComponentField,
    component_id: ComponentId,
    dirty_chunks: HashSet<(u32, u32, u32)>,
    /// Ring radius (mm) captured at drag start for picking.
    ring_radius_mm: f32,
}

#[derive(SystemSet, Debug, Clone, Hash, PartialEq, Eq)]
pub struct RotateGizmoInputSet;

/// Canonical torus mesh major radius in mm; entities scale around this.
const RING_MESH_RADIUS: f32 = 40.0;
const RING_MESH_TUBE: f32 = 1.6;
const RING_RADIUS_MIN: f32 = 18.0;
const RING_RADIUS_MAX: f32 = 80.0;
const PICK_TOLERANCE_FRAC: f32 = 0.22;

/// Degrees of drag past which we add another quarter-turn.
const QUARTER_SNAP_DEG: f32 = 45.0;

fn axis_color(axis: Axis) -> Color {
    match axis {
        Axis::X => Color::srgb(1.0, 0.25, 0.30),
        Axis::Y => Color::srgb(0.30, 1.0, 0.30),
        Axis::Z => Color::srgb(0.30, 0.50, 1.0),
    }
}

fn axis_emissive(axis: Axis) -> LinearRgba {
    match axis {
        Axis::X => LinearRgba::new(1.4, 0.12, 0.16, 1.0),
        Axis::Y => LinearRgba::new(0.16, 1.4, 0.16, 1.0),
        Axis::Z => LinearRgba::new(0.16, 0.28, 1.4, 1.0),
    }
}

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

fn quarters_is_identity(q: IVec3) -> bool {
    q.x.rem_euclid(4) == 0 && q.y.rem_euclid(4) == 0 && q.z.rem_euclid(4) == 0
}

/// Ring radius from a selection AABB (piece-local mm), clamped.
pub fn ring_radius_for_aabb(mn: UVec3, mx: UVec3, voxel_size: f32) -> f32 {
    let dx = (mx.x.saturating_sub(mn.x)) as f32 * voxel_size;
    let dy = (mx.y.saturating_sub(mn.y)) as f32 * voxel_size;
    let dz = (mx.z.saturating_sub(mn.z)) as f32 * voxel_size;
    let diag = (dx * dx + dy * dy + dz * dz).sqrt();
    // ~0.55 of the diagonal reads as "around the piece".
    (diag * 0.55).clamp(RING_RADIUS_MIN, RING_RADIUS_MAX)
}

/// Voxels to lift (+Y) before a rotate so the result stays on/above
/// the workbench. `0` when the rotated solid AABB already clears `y=0`.
pub fn bench_lift_voxels(labels: &ComponentField, id: ComponentId, quarters: IVec3) -> i32 {
    if quarters_is_identity(quarters) {
        return 0;
    }
    let Some((mn, mx)) = labels.bounds_of(id) else {
        return 0;
    };
    let Some(pivot) = component_pivot(labels, id) else {
        return 0;
    };
    if mx.x <= mn.x || mx.y <= mn.y || mx.z <= mn.z {
        return 0;
    }
    let cx = [mn.x as i32, mx.x as i32 - 1];
    let cy = [mn.y as i32, mx.y as i32 - 1];
    let cz = [mn.z as i32, mx.z as i32 - 1];
    let mut min_y = i32::MAX;
    for &x in &cx {
        for &y in &cy {
            for &z in &cz {
                let r = rotate_voxel(IVec3::new(x, y, z), pivot, quarters);
                min_y = min_y.min(r.y);
            }
        }
    }
    if min_y == i32::MAX || min_y >= 0 {
        0
    } else {
        -min_y
    }
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
            handle_rotate_keyboard,
            clear_frame_flags,
            update_bench_warning,
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
    let torus = meshes.add(Torus::new(
        RING_MESH_RADIUS - RING_MESH_TUBE,
        RING_MESH_RADIUS + RING_MESH_TUBE,
    ));
    // Short radial ticks at 90° stops (torus lies in XZ; Y through hole).
    let tick_mesh = meshes.add(Cuboid::new(2.2, 2.2, 7.0));
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let material = materials.add(StandardMaterial {
            base_color: axis_color(axis),
            emissive: axis_emissive(axis),
            perceptual_roughness: 0.4,
            metallic: 0.0,
            ..default()
        });
        let ring = commands
            .spawn((
                RotateRing { axis },
                Mesh3d(torus.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_rotation(ring_rotation(axis)),
                Visibility::Hidden,
            ))
            .id();
        commands.entity(root).add_child(ring);
        let tick_r = RING_MESH_RADIUS + 3.5;
        for (ox, oz) in [
            (tick_r, 0.0),
            (-tick_r, 0.0),
            (0.0, tick_r),
            (0.0, -tick_r),
        ] {
            let tick = commands
                .spawn((
                    Mesh3d(tick_mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::from_translation(Vec3::new(ox, 0.0, oz)),
                ))
                .id();
            commands.entity(ring).add_child(tick);
        }
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

    let (centre_local, radius_mm) = if let Some(active) = drag.active.as_ref() {
        let c = centre_from_drag(active, &workpiece, state.pending_quarters);
        (c, active.ring_radius_mm)
    } else {
        match selection_centre_and_radius(&mut selection, &workpiece) {
            Some(pair) => (Some(pair.0), pair.1),
            None => (None, RING_RADIUS_MIN),
        }
    };

    let Some(centre_local) = centre_local else {
        for (_, _, mut vis) in &mut q_rings {
            *vis = Visibility::Hidden;
        }
        return;
    };

    let quarters = state.pending_quarters;
    let scale = radius_mm / RING_MESH_RADIUS;
    for (ring, mut tf, mut vis) in &mut q_rings {
        tf.translation = Vec3::new(centre_local.x, centre_local.y, centre_local.z);
        tf.scale = Vec3::splat(scale);
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
    quarters: IVec3,
) -> Option<GVec3> {
    let (mn, mx) = active.labels.bounds_of(active.component_id)?;
    let pivot = component_pivot(&active.labels, active.component_id)?;
    let lift = bench_lift_voxels(&active.labels, active.component_id, quarters);
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    let centre_vox = IVec3::new(
        ((mn.x + mx.x) / 2) as i32,
        ((mn.y + mx.y) / 2) as i32,
        ((mn.z + mx.z) / 2) as i32,
    );
    // Preview pose: lift then rotate (same as commit).
    let lifted = centre_vox + IVec3::new(0, lift, 0);
    let pivot_lifted = pivot + IVec3::new(0, lift, 0);
    let rotated = rotate_voxel(lifted, pivot_lifted, quarters);
    Some(GVec3::new(
        origin.x + rotated.x as f32 * vs + vs * 0.5,
        origin.y + rotated.y as f32 * vs + vs * 0.5,
        origin.z + rotated.z as f32 * vs + vs * 0.5,
    ))
}

fn selection_centre_and_radius(
    selection: &mut Selection,
    workpiece: &LayersState,
) -> Option<(GVec3, f32)> {
    let labels = ensure_labels_owned(selection, workpiece)?;
    let id = selection.selected_id(&labels);
    let bounds = id.and_then(|id| labels.bounds_of(id));
    selection.set_labels(labels);
    let (mn, mx) = bounds?;
    let vs = workpiece.grid().voxel_size();
    let origin = workpiece.grid().origin();
    let centre = GVec3::new(
        origin.x + (mn.x + mx.x) as f32 * 0.5 * vs,
        origin.y + (mn.y + mx.y) as f32 * 0.5 * vs,
        origin.z + (mn.z + mx.z) as f32 * 0.5 * vs,
    );
    let radius = ring_radius_for_aabb(mn, mx, vs);
    Some((centre, radius))
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

fn update_bench_warning(
    tool: Res<SculptTool>,
    mut selection: ResMut<Selection>,
    workpiece: Res<LayersState>,
    mut state: ResMut<RotateState>,
    drag: Res<RotateGizmoDrag>,
) {
    if !matches!(tool.kind, ToolKind::Rotate) {
        state.warn_below_bench = false;
        return;
    }
    if selection.picked_voxel.is_none() || quarters_is_identity(state.pending_quarters) {
        state.warn_below_bench = false;
        return;
    }
    let lift = if let Some(active) = drag.active.as_ref() {
        bench_lift_voxels(&active.labels, active.component_id, state.pending_quarters)
    } else {
        let labels = ensure_labels_owned(&mut selection, &workpiece);
        let Some(labels) = labels else {
            state.warn_below_bench = false;
            return;
        };
        let id = selection.selected_id(&labels);
        let lift = id
            .map(|id| bench_lift_voxels(&labels, id, state.pending_quarters))
            .unwrap_or(0);
        selection.set_labels(labels);
        lift
    };
    state.warn_below_bench = lift > 0;
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
                state.pending_quarters,
                &mut workpiece,
                &mut selection,
                &mut history,
            );
            state.pending_quarters = IVec3::ZERO;
            state.warn_below_bench = false;
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

    let Some((centre, ring_radius_mm)) = selection_centre_and_radius(&mut selection, &workpiece)
    else {
        return;
    };
    let pick_tol = (ring_radius_mm * PICK_TOLERANCE_FRAC).clamp(6.0, 14.0);

    let mut best: Option<(Axis, f32, f32)> = None;
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let Some((dist, _t, angle)) =
            ray_circle_hit(ray_o, ray_d, centre, axis.dir_local(), ring_radius_mm)
        else {
            continue;
        };
        if dist > pick_tol {
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
        initial_quarters: state.pending_quarters,
        grid_snapshot: snapshot,
        labels,
        component_id,
        dirty_chunks: HashSet::new(),
        ring_radius_mm,
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
        active.ring_radius_mm,
    ) else {
        return;
    };
    let mut delta = angle_now - active.angle_start;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta <= -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    // Discrete quarters: every 45° of drag adds another step.
    let step = (delta.to_degrees() / QUARTER_SNAP_DEG).round() as i32;
    let mut q = active.initial_quarters;
    q[active.axis.index()] += step;
    state.pending_quarters = q;
}

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
    let lift = bench_lift_voxels(&active.labels, active.component_id, quarters);
    let res = grid.res();
    let mut need_min: Option<UVec3> = None;
    let mut need_max: Option<UVec3> = None;
    let expand = |a: &mut Option<UVec3>, b: &mut Option<UVec3>, mn: UVec3, mx: UVec3| {
        *a = Some(match *a {
            Some(cur) => cur.min(mn),
            None => mn,
        });
        *b = Some(match *b {
            Some(cur) => cur.max(mx),
            None => mx,
        });
    };
    if lift > 0 {
        if let Some((mn, mx)) = touched_region_for_translate(
            &active.labels,
            active.component_id,
            IVec3::new(0, lift, 0),
            res,
        ) {
            expand(&mut need_min, &mut need_max, mn, mx);
        }
    }
    if let Some((mn, mx)) =
        touched_region_for_rotate(&active.labels, active.component_id, quarters, res)
    {
        let mut mx = mx;
        if lift > 0 {
            mx.y = (mx.y + lift as u32).min(res.y);
        }
        expand(&mut need_min, &mut need_max, mn, mx);
    }
    let (Some(need_min), Some(need_max)) = (need_min, need_max) else {
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

/// Lift (if needed) then rotate. Journals into `on_pre`. Returns
/// whether anything changed, and updates `picked` with the voxel
/// after both ops.
fn lift_then_rotate<F>(
    grid: &mut Grid,
    labels: &ComponentField,
    id: ComponentId,
    quarters: IVec3,
    picked: &mut Option<(u32, u32, u32)>,
    mut on_pre: F,
) -> bool
where
    F: FnMut(u32, u32, u32, f32),
{
    if quarters_is_identity(quarters) {
        return false;
    }
    let lift = bench_lift_voxels(labels, id, quarters);
    let mut changed = false;
    let mut working_id = id;
    let owned_labels: ComponentField;

    let labels_ref: &ComponentField = if lift > 0 {
        if translate_component(
            grid,
            labels,
            id,
            IVec3::new(0, lift, 0),
            &mut on_pre,
        )
        .is_some()
        {
            changed = true;
        }
        if let Some((x, y, z)) = *picked {
            *picked = Some((x, y + lift as u32, z));
        }
        owned_labels = label_components(grid);
        working_id = match *picked {
            Some((x, y, z)) => owned_labels.id_at(x, y, z),
            None => working_id,
        };
        if working_id == sculpt_core::EMPTY {
            return changed;
        }
        &owned_labels
    } else {
        labels
    };

    if let Some(pivot) = component_pivot(labels_ref, working_id) {
        if rotate_component(grid, labels_ref, working_id, quarters, &mut on_pre).is_some() {
            changed = true;
        }
        if let Some((x, y, z)) = *picked {
            let mapped = rotate_voxel(IVec3::new(x as i32, y as i32, z as i32), pivot, quarters);
            let res = grid.res();
            *picked = if mapped.x >= 0
                && mapped.y >= 0
                && mapped.z >= 0
                && mapped.x < res.x as i32
                && mapped.y < res.y as i32
                && mapped.z < res.z as i32
            {
                Some((mapped.x as u32, mapped.y as u32, mapped.z as u32))
            } else {
                None
            };
        }
    }
    changed
}

fn apply_preview(
    drag: &mut RotateGizmoDrag,
    workpiece: &mut LayersState,
    state: &RotateState,
) {
    let Some(active) = drag.active.as_mut() else {
        return;
    };
    let quarters = state.pending_quarters;
    ensure_region_covers(active, workpiece.grid_mut(), quarters);
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    let mut picked = None; // preview doesn't need pick tracking
    let _ = lift_then_rotate(
        workpiece.grid_mut(),
        &active.labels,
        active.component_id,
        quarters,
        &mut picked,
        |_, _, _, _| {},
    );
    // Dirty: mark the covered region chunks (conservative).
    let (mn, mx) = active.grid_snapshot.bounds();
    let grid_res = workpiece.grid().res();
    // Touch chunks spanning the snapshot bounds.
    let cs = sculpt_core::CHUNK_SIZE;
    let c0 = (mn.x / cs, mn.y / cs, mn.z / cs);
    let c1 = (
        mx.x.saturating_sub(1) / cs,
        mx.y.saturating_sub(1) / cs,
        mx.z.saturating_sub(1) / cs,
    );
    for cz in c0.2..=c1.2 {
        for cy in c0.1..=c1.1 {
            for cx in c0.0..=c1.0 {
                active.dirty_chunks.insert((cx, cy, cz));
            }
        }
    }
    let _ = grid_res;
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
}

fn commit_drag(
    mut active: ActiveDrag,
    quarters: IVec3,
    workpiece: &mut ResMut<LayersState>,
    selection: &mut ResMut<Selection>,
    history: &mut ResMut<UndoHistory>,
) {
    ensure_region_covers(&mut active, workpiece.grid_mut(), quarters);
    workpiece.grid_mut().restore_region(&active.grid_snapshot);
    if quarters_is_identity(quarters) {
        for &(x, y, z) in &active.dirty_chunks {
            workpiece.mark_dirty((x, y, z));
        }
        selection.invalidate_labels();
        info!("rotate: drag ended with identity — nothing to commit");
        return;
    }

    let mut recorder = StrokeRecorder::default();
    let mut picked = selection.picked_voxel;
    let changed = lift_then_rotate(
        workpiece.grid_mut(),
        &active.labels,
        active.component_id,
        quarters,
        &mut picked,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );
    let grid_res = workpiece.grid().res();
    // Journal dirty from recorder's touched voxels via a full finish —
    // also mark preview chunks.
    if changed {
        // Approximate dirty: snapshot bounds (lift+rotate stay inside).
        let (mn, mx) = active.grid_snapshot.bounds();
        use sculpt_core::DirtyRegion;
        recorder.record_dirty_region(
            DirtyRegion {
                min: mn,
                max: mx,
            },
            grid_res,
        );
    }
    for &(x, y, z) in &active.dirty_chunks {
        workpiece.mark_dirty((x, y, z));
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
        let (mn, mx) = active.grid_snapshot.bounds();
        let cs = sculpt_core::CHUNK_SIZE;
        for cz in (mn.z / cs)..=(mx.z.saturating_sub(1) / cs) {
            for cy in (mn.y / cs)..=(mx.y.saturating_sub(1) / cs) {
                for cx in (mn.x / cs)..=(mx.x.saturating_sub(1) / cs) {
                    workpiece.mark_dirty((cx, cy, cz));
                }
            }
        }
    }

    selection.picked_voxel = picked;
    selection.invalidate_labels();
    info!(
        "rotated selection by quarters {:?}{}",
        quarters,
        if bench_lift_voxels(&active.labels, active.component_id, quarters) > 0 {
            " (auto-lifted above bench)"
        } else {
            ""
        }
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
    state.pending_quarters = active.initial_quarters;
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
                deg_to_quarters(*deg),
                &mut workpiece,
                &mut selection,
                &mut history,
                &mut stroke,
            ) {
                state.pending_quarters = IVec3::ZERO;
                state.warn_below_bench = false;
            }
        }
    }
}

fn handle_rotate_keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    tool: Res<SculptTool>,
    drag: Res<RotateGizmoDrag>,
    mut workpiece: ResMut<LayersState>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
    mut state: ResMut<RotateState>,
) {
    if !matches!(tool.kind, ToolKind::Rotate) || drag.is_active() || ui_gate.keyboard {
        return;
    }
    if selection.picked_voxel.is_none() {
        return;
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let sign = if shift { -1 } else { 1 };
    let mut q = IVec3::ZERO;
    if keys.just_pressed(KeyCode::KeyX) {
        q.x = sign;
    } else if keys.just_pressed(KeyCode::KeyY) {
        q.y = sign;
    } else if keys.just_pressed(KeyCode::KeyZ) {
        q.z = sign;
    } else {
        return;
    }
    if apply_rotate(
        q,
        &mut workpiece,
        &mut selection,
        &mut history,
        &mut stroke,
    ) {
        state.pending_quarters = IVec3::ZERO;
        state.warn_below_bench = false;
    }
}

fn clear_frame_flags(mut drag: ResMut<RotateGizmoDrag>) {
    drag.started_this_frame = false;
}

fn apply_rotate(
    quarters: IVec3,
    workpiece: &mut LayersState,
    selection: &mut Selection,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) -> bool {
    if selection.picked_voxel.is_none() {
        info!("rotate: nothing selected");
        return false;
    }
    if quarters_is_identity(quarters) {
        info!("rotate: identity quarters — nothing to do");
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

    let mut recorder = StrokeRecorder::default();
    let mut picked = selection.picked_voxel;
    let lift = bench_lift_voxels(&labels, id, quarters);
    let changed = lift_then_rotate(
        workpiece.grid_mut(),
        &labels,
        id,
        quarters,
        &mut picked,
        |x, y, z, pre| recorder.record_pre_value(x, y, z, pre),
    );
    if !changed {
        return false;
    }
    let grid_res = workpiece.grid().res();
    // Dirty region: generous union from lift + rotate reach.
    if let Some((mn, mx)) = touched_region_for_rotate(&labels, id, quarters, grid_res) {
        let mut mx = mx;
        if lift > 0 {
            mx.y = (mx.y + lift as u32).min(grid_res.y);
        }
        use sculpt_core::DirtyRegion;
        recorder.record_dirty_region(DirtyRegion { min: mn, max: mx }, grid_res);
        let cs = sculpt_core::CHUNK_SIZE;
        for cz in (mn.z / cs)..=(mx.z.saturating_sub(1) / cs) {
            for cy in (mn.y / cs)..=(mx.y.saturating_sub(1) / cs) {
                for cx in (mn.x / cs)..=(mx.x.saturating_sub(1) / cs) {
                    workpiece.mark_dirty((cx, cy, cz));
                }
            }
        }
    }
    if let Some(entry) = recorder.finish(workpiece.grid(), workpiece.active_id()) {
        history.push_stroke(entry);
    }
    selection.picked_voxel = picked;
    selection.invalidate_labels();
    info!(
        "rotated selection by quarters {:?}{}",
        quarters,
        if lift > 0 {
            " (auto-lifted above bench)"
        } else {
            ""
        }
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
            IVec3::new(0, 1, 0),
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
            IVec3::new(0, 1, 0),
            &mut workpiece,
            &mut selection,
            &mut history,
            &mut stroke,
        ));
    }

    #[test]
    fn deg_to_quarters_snaps() {
        assert_eq!(
            deg_to_quarters(Vec3::new(80.0, -100.0, 10.0)),
            IVec3::new(1, -1, 0)
        );
    }

    #[test]
    fn ring_radius_clamps_tiny_and_huge() {
        let tiny = ring_radius_for_aabb(UVec3::new(10, 10, 10), UVec3::new(12, 12, 12), 1.0);
        assert!((tiny - RING_RADIUS_MIN).abs() < 1e-3);
        let huge = ring_radius_for_aabb(UVec3::ZERO, UVec3::new(400, 400, 400), 1.0);
        assert!((huge - RING_RADIUS_MAX).abs() < 1e-3);
    }

    #[test]
    fn bench_lift_when_rotate_would_go_below() {
        let mut g = Grid::empty(UVec3::new(48, 48, 48), 1.0, glam::Vec3::ZERO);
        // Flat slab on the bench — 90° about X tips it into −Y.
        let _ = apply_primitive(
            &mut g,
            &Primitive {
                kind: PrimitiveKind::Box {
                    half_extents: glam::Vec3::new(8.0, 2.0, 8.0),
                },
                center: glam::Vec3::new(24.0, 2.0, 24.0),
                workbench_y: Some(0.0),
            },
        );
        let labels = label_components(&g);
        let id = labels.ids_by_size_desc()[0];
        let lift = bench_lift_voxels(&labels, id, IVec3::new(1, 0, 0));
        assert!(lift > 0, "expected a bench lift, got {lift}");
    }

    #[test]
    fn apply_rotate_auto_lifts_above_bench_one_undo() {
        let mut workpiece = LayersState::new_for_test(Grid::empty(
            UVec3::new(48, 48, 48),
            1.0,
            glam::Vec3::ZERO,
        ));
        let _ = apply_primitive(
            workpiece.grid_mut(),
            &Primitive {
                kind: PrimitiveKind::Box {
                    half_extents: glam::Vec3::new(8.0, 2.0, 8.0),
                },
                center: glam::Vec3::new(24.0, 2.0, 24.0),
                workbench_y: Some(0.0),
            },
        );
        let mut selection = Selection::default();
        selection.picked_voxel = Some((24, 2, 24));
        let mut history = UndoHistory::default();
        let mut stroke = SculptStroke::default();

        assert!(apply_rotate(
            IVec3::new(1, 0, 0),
            &mut workpiece,
            &mut selection,
            &mut history,
            &mut stroke,
        ));
        let labels = label_components(workpiece.grid());
        let id = labels.ids_by_size_desc()[0];
        let (mn, _) = labels.bounds_of(id).unwrap();
        assert_eq!(mn.y, 0, "auto-lift should leave the piece on the bench");
        assert_eq!(history.undo_len_for_test(), 1, "lift+rotate = one undo");
    }

    #[test]
    fn forty_five_deg_snaps_to_one_quarter() {
        assert_eq!(deg_to_quarters(Vec3::new(45.0, 0.0, 0.0)), IVec3::new(1, 0, 0));
        assert_eq!(deg_to_quarters(Vec3::new(44.0, 0.0, 0.0)), IVec3::new(0, 0, 0));
    }
}
