//! Component selection: pick a connected lump, then delete it or
//! scope sculpting to it (active-only mode).
//!
//! Selection state lives in [`Selection`]. The user picks a component
//! via the `Select` tool (LMB) and can then:
//!
//! - Press `Delete` to remove that component's voxels (recorded as a
//!   single undo stroke).
//! - Toggle **active-only** with `A` — while on, other sculpting
//!   tools only mutate voxels that belong to the selected
//!   component. Useful for detailing a small piece without
//!   disturbing anything else on the workbench.
//!
//! We store the *voxel* the user clicked on rather than the raw
//! component id: any grid mutation reshuffles ids, but the picked
//! voxel keeps pointing at the same lump of clay unless the user
//! carves through it. Component ids are re-derived from fresh labels
//! whenever they're needed.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::window::PrimaryWindow;
use glam::{UVec3, Vec3 as GVec3};
use sculpt_core::{label_components, ChunkCoord, ComponentField, ComponentId, EMPTY};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::sculpt::{SculptTool, ToolKind};
use crate::turntable::TurntableSet;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{SculptWorkpiece, WorkpieceRoot};

pub fn plugin(app: &mut App) {
    app.init_resource::<Selection>();
    // PostStartup so `WorkpieceRoot` has been spawned by
    // `workpiece::spawn_workpiece` (also `Startup`) and we can
    // parent the highlight under it.
    app.add_systems(PostStartup, spawn_selection_highlight);
    app.add_systems(
        Update,
        (
            selection_input,
            handle_selection_actions,
            delete_hotkey,
            update_selection_highlight,
        )
            .after(TurntableSet),
    );
}

/// Full selection state, held in a global resource.
#[derive(Resource, Default)]
pub struct Selection {
    /// The voxel the user last clicked with the Select tool. `None`
    /// means nothing is selected. Kept in piece-local voxel indices;
    /// stays valid across grid mutations until the underlying lump
    /// is carved away.
    pub picked_voxel: Option<(u32, u32, u32)>,
    /// When on, sculpting stamps only mutate voxels belonging to the
    /// selected component (see [`voxel_allowed`]).
    pub active_only: bool,
    /// Cached component labels. `None` = stale; regenerate before
    /// next use. See [`ensure_labels_fresh`].
    labels: Option<ComponentField>,
}

impl Selection {
    /// Drop cached labels. Call after any mutation to the workpiece
    /// grid so the next selection query re-labels.
    pub fn invalidate_labels(&mut self) {
        self.labels = None;
    }

    /// Component id under `picked_voxel`, resolved against the given
    /// labels. Returns `None` when nothing is picked, when the
    /// picked voxel is now empty (the lump was carved away), or
    /// when `picked_voxel` is out of the labelled grid range.
    /// Passing labels explicitly (rather than reading `self.labels`)
    /// lets `delete` temporarily own the field to avoid a double
    /// borrow.
    pub fn selected_id(&self, labels: &ComponentField) -> Option<ComponentId> {
        let (x, y, z) = self.picked_voxel?;
        let res = labels.res();
        if x >= res.x || y >= res.y || z >= res.z {
            return None;
        }
        let id = labels.id_at(x, y, z);
        if id == EMPTY {
            None
        } else {
            Some(id)
        }
    }

    /// Read-only view of the cached labels, if we have them. Callers
    /// that need a guaranteed-fresh field should use
    /// [`ensure_labels_fresh`] (in-module) or
    /// [`Self::set_labels`] after re-labelling themselves.
    pub fn labels(&self) -> Option<&ComponentField> {
        self.labels.as_ref()
    }

    /// Install a freshly-computed label field. Callers outside this
    /// module use it to prime the cache when they've already
    /// re-labelled (e.g. the sculpt path's active-only gate).
    pub fn set_labels(&mut self, labels: ComponentField) {
        self.labels = Some(labels);
    }

}

/// Ensure [`Selection::labels`] is populated for the current grid,
/// running the labeller lazily if the cache is stale. Returns the
/// labels reference for the caller. Costs O(N) grid voxels on a
/// stale-cache call; free on a hit.
fn ensure_labels_fresh<'a>(
    selection: &'a mut Selection,
    workpiece: &SculptWorkpiece,
) -> &'a ComponentField {
    if selection.labels.is_none() {
        selection.labels = Some(label_components(&workpiece.grid));
    }
    selection.labels.as_ref().expect("just populated")
}

#[allow(clippy::too_many_arguments)]
fn selection_input(
    buttons: Res<ButtonInput<MouseButton>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&Transform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    ui_gate: Res<UiCapturesInput>,
    workpiece: Res<SculptWorkpiece>,
    mut selection: ResMut<Selection>,
) {
    // Both Select and Move accept LMB-picks so the user can jump
    // straight into "pick and move" without swapping tools.
    if !matches!(tool.kind, ToolKind::Select | ToolKind::Move) {
        return;
    }
    if ui_gate.pointer {
        return;
    }
    if !buttons.just_pressed(MouseButton::Left) {
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
    let origin_local = piece_inv.transform_point3(ray_world.origin);
    let dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();
    let Some(hit) = workpiece.grid.ray_march(
        GVec3::new(origin_local.x, origin_local.y, origin_local.z),
        GVec3::new(dir_local.x, dir_local.y, dir_local.z),
        4000.0,
    ) else {
        return;
    };

    // Step a tiny bit *into* the surface along the view ray so we
    // land on a solid voxel rather than the outside face — the SDF
    // is only ≤ 0 inside.
    let step = workpiece.grid.voxel_size() * 0.5;
    let inside = hit
        + GVec3::new(dir_local.x, dir_local.y, dir_local.z) * step;
    let Some(voxel) = grid_voxel_of(&workpiece, inside) else {
        return;
    };

    let labels = ensure_labels_fresh(&mut selection, &workpiece);
    let id = labels.id_at(voxel.0, voxel.1, voxel.2);
    if id == EMPTY {
        info!("select: hit voxel is empty — nothing picked");
        return;
    }
    let voxel_count = labels.voxel_count(id);
    let volume_mm3 = labels.volume_mm3(id, workpiece.grid.voxel_size());
    selection.picked_voxel = Some(voxel);
    info!(
        "selected component #{id} — {voxel_count} voxels, {volume_mm3:.0} mm³",
    );
}

/// Map a piece-local point to the grid voxel it lies in, or `None`
/// when the point falls outside the grid AABB.
fn grid_voxel_of(workpiece: &SculptWorkpiece, p: GVec3) -> Option<(u32, u32, u32)> {
    let vs = workpiece.grid.voxel_size();
    let inv_vs = 1.0 / vs;
    let res = workpiece.grid.res();
    let origin = workpiece.grid.origin();
    let local = (p - origin) * inv_vs;
    if local.x < 0.0
        || local.y < 0.0
        || local.z < 0.0
        || local.x >= res.x as f32
        || local.y >= res.y as f32
        || local.z >= res.z as f32
    {
        return None;
    }
    Some((local.x as u32, local.y as u32, local.z as u32))
}

/// Handle `AppAction::DeleteSelection` and `AppAction::ToggleActiveOnly`.
///
/// Kept separate from `selection_input` so the same actions can be
/// fired by keyboard, menus, or programmatic tests without going
/// through the Select-tool pointer path.
fn handle_selection_actions(
    mut events: EventReader<AppAction>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<UndoHistory>,
    mut stroke: ResMut<SculptStroke>,
) {
    for a in events.read() {
        match a {
            AppAction::DeleteSelection => {
                delete_selected_component(
                    &mut workpiece,
                    &mut selection,
                    &mut history,
                    &mut stroke,
                );
            }
            AppAction::ToggleActiveOnly => {
                selection.active_only = !selection.active_only;
                info!(
                    "active-only: {}",
                    if selection.active_only { "on" } else { "off" }
                );
            }
            _ => {}
        }
    }
}

fn delete_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    ui_gate: Res<UiCapturesInput>,
    mut actions: EventWriter<AppAction>,
) {
    if ui_gate.keyboard {
        return;
    }
    if keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace) {
        actions.send(AppAction::DeleteSelection);
    }
    if keys.just_pressed(KeyCode::KeyA)
        && !keys.pressed(KeyCode::ControlLeft)
        && !keys.pressed(KeyCode::ControlRight)
        && !keys.pressed(KeyCode::SuperLeft)
        && !keys.pressed(KeyCode::SuperRight)
    {
        actions.send(AppAction::ToggleActiveOnly);
    }
}

/// Actually remove the selected component: iterate its voxels, set
/// them to "far positive" (empty), journal every change as a single
/// undo stroke, dirty the touched chunks, and clear the selection.
fn delete_selected_component(
    workpiece: &mut SculptWorkpiece,
    selection: &mut Selection,
    history: &mut UndoHistory,
    stroke: &mut SculptStroke,
) {
    // Abandon any live sculpting stroke — the delete is a separate
    // undo unit and mixing them would confuse both.
    stroke.discard_live();

    // Take the labels out of the resource so we can consult them
    // without keeping a live borrow while mutating the grid.
    let labels = {
        let _ = ensure_labels_fresh(selection, workpiece);
        selection.labels.take().expect("labels populated just above")
    };
    let Some(target) = selection.selected_id(&labels) else {
        info!("delete: nothing selected");
        // Put labels back so the next query is still cheap.
        selection.labels = Some(labels);
        return;
    };

    let res = labels.res();
    let empty_value = workpiece.grid.voxel_size() * 32.0;
    let mut recorder = StrokeRecorder::default();
    let mut removed = 0u32;
    let mut min = UVec3::new(u32::MAX, u32::MAX, u32::MAX);
    let mut max = UVec3::ZERO;
    let ids = labels.ids();

    for iz in 0..res.z {
        for iy in 0..res.y {
            for ix in 0..res.x {
                let idx = (ix + iy * res.x + iz * res.x * res.y) as usize;
                if ids[idx] != target {
                    continue;
                }
                let pre = workpiece.grid.get(ix, iy, iz);
                recorder.record_pre_value(ix, iy, iz, pre);
                workpiece.grid.set(ix, iy, iz, empty_value);
                removed += 1;
                min.x = min.x.min(ix);
                min.y = min.y.min(iy);
                min.z = min.z.min(iz);
                max.x = max.x.max(ix + 1);
                max.y = max.y.max(iy + 1);
                max.z = max.z.max(iz + 1);
            }
        }
    }

    if removed == 0 {
        info!("delete: selected component had no voxels — nothing to do");
        selection.picked_voxel = None;
        return;
    }

    let region = sculpt_core::DirtyRegion { min, max };
    recorder.record_dirty_region(region, res);
    for c in region.touched_chunks(res) {
        let ChunkCoord { x, y, z } = c;
        workpiece.dirty.insert((x, y, z));
    }
    if let Some(entry) = recorder.finish(&workpiece.grid) {
        history.push_stroke(entry);
    }
    selection.picked_voxel = None;
    selection.invalidate_labels();
    info!("deleted selected component ({removed} voxels)");
}

/// Marker component for the wireframe box overlay that highlights the
/// selected component. One entity, parented under [`WorkpieceRoot`]
/// so it turns with the turntable.
#[derive(Component)]
pub struct SelectionHighlight;

/// Spawn the highlight entity. Mesh is a unit-cube's 12 edges as a
/// `LineList` so scaling the transform stretches it into any AABB.
/// Material is unlit-emissive so it reads on any background.
fn spawn_selection_highlight(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    q_root: Query<Entity, With<WorkpieceRoot>>,
) {
    let mesh = meshes.add(unit_cube_edges());
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.85, 0.35, 0.95),
        emissive: LinearRgba::new(2.6, 2.1, 0.6, 1.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let entity = commands
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden,
            SelectionHighlight,
        ))
        .id();
    if let Ok(root) = q_root.get_single() {
        commands.entity(root).add_child(entity);
    }
}

/// Reposition + rescale the highlight to hug the selected
/// component's voxel-space AABB (converted to piece-local mm).
/// Hides the entity when nothing is selected.
///
/// A tiny padding is added so the wire sits *just outside* the
/// meshed surface rather than z-fighting the marching-cubes shell.
#[allow(clippy::too_many_arguments)]
fn update_selection_highlight(
    mut selection: ResMut<Selection>,
    workpiece: Res<SculptWorkpiece>,
    mut q_highlight: Query<
        (&mut Transform, &mut Visibility),
        With<SelectionHighlight>,
    >,
) {
    let Ok((mut tf, mut vis)) = q_highlight.get_single_mut() else {
        return;
    };

    // Nothing picked, or the picked voxel got carved away.
    if selection.picked_voxel.is_none() {
        *vis = Visibility::Hidden;
        return;
    }

    // Only run the labeller when it's stale; keep the reference
    // shape short so we don't fight the borrow checker with the
    // grid probe below. Same "temporarily own the labels" trick
    // as `delete_selected_component`.
    let bounds = {
        ensure_labels_fresh(&mut selection, &workpiece);
        let labels = selection
            .labels
            .take()
            .expect("labels populated just above");
        let bounds = selection
            .selected_id(&labels)
            .and_then(|id| labels.bounds_of(id));
        selection.labels = Some(labels);
        bounds
    };

    let Some((min, max)) = bounds else {
        *vis = Visibility::Hidden;
        return;
    };

    let vs = workpiece.grid.voxel_size();
    let origin = workpiece.grid.origin();
    let pad = vs * 0.35;
    let local_min = glam::Vec3::new(
        origin.x + min.x as f32 * vs - pad,
        origin.y + min.y as f32 * vs - pad,
        origin.z + min.z as f32 * vs - pad,
    );
    let local_max = glam::Vec3::new(
        origin.x + max.x as f32 * vs + pad,
        origin.y + max.y as f32 * vs + pad,
        origin.z + max.z as f32 * vs + pad,
    );
    let centre = (local_min + local_max) * 0.5;
    let size = local_max - local_min;
    tf.translation = Vec3::new(centre.x, centre.y, centre.z);
    tf.scale = Vec3::new(size.x, size.y, size.z);
    tf.rotation = Quat::IDENTITY;
    *vis = Visibility::Visible;
}

/// Build a [`LineList`] mesh with the 12 edges of a unit cube from
/// `-0.5..0.5`. Transform scaling then stretches it into whatever
/// AABB we need without ever regenerating the mesh.
fn unit_cube_edges() -> Mesh {
    let corners: [[f32; 3]; 8] = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    // 12 edges, each two consecutive indices form one line segment.
    let edges: [u32; 24] = [
        0, 1, 1, 2, 2, 3, 3, 0, // bottom Z
        4, 5, 5, 6, 6, 7, 7, 4, // top Z
        0, 4, 1, 5, 2, 6, 3, 7, // vertical pillars
    ];
    let mut mesh = Mesh::new(
        PrimitiveTopology::LineList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
    // Bevy's PBR pipeline requires a normal attribute even for lines.
    let normals: Vec<[f32; 3]> = vec![[0.0, 1.0, 0.0]; corners.len()];
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(edges.to_vec()));
    mesh
}
