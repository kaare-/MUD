//! Ghost tool preview: a translucent shape at the cursor showing where
//! the active tool will land.
//!
//! For Add/Remove this is a sphere the same size as the brush; for a
//! cookie cutter it's a short prism of the profile shape, oriented
//! along the surface normal at the hit point. The orientation is the
//! implicit "which way the cut goes" indicator you asked for.
//!
//! Press / Pull / Knife use distinct tint families so displace and
//! remove tools don't look identical to Add/Remove at a glance.
//!
//! The preview hides when the cursor isn't over the workpiece (no
//! ray-march hit). While the user is actively sculpting (LMB held)
//! the ghost stays on screen but switches to a dimmer, less-saturated
//! material so it doesn't fight the sculpting feedback for attention
//! — the user still sees where the brush footprint is.
//!
//! Placement is in **world space** from the workpiece's
//! [`GlobalTransform`], computed in `PostUpdate` after transform
//! propagation. Parenting the ghost under the turntable used piece-
//! local `Transform`s but raced with deferred `set_parent` and
//! stale `GlobalTransform`s in `Update`, which made the ghost stick
//! to the wrong face after Q/E turns.
//!
//! One entity is spawned at startup and reused. The mesh is rebuilt
//! only when the active tool or its size changes, not every frame.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::transform::TransformSystem;
use bevy::window::PrimaryWindow;
use glam::{Vec2 as GVec2, Vec3 as GVec3};

use sculpt_core::Profile;

use crate::pen::PenState;
use crate::sculpt::{
    clay_brush_center, knife_profile, press_brush_center, CutterParams, SculptTool, ToolKind,
};
use crate::settings::AppSettings;
use crate::workpiece::{LayersState, WorkpieceRoot};

/// Marker component for the single preview entity.
#[derive(Component)]
pub struct ToolPreview;

/// Cache of the last-built mesh signature. When this differs from the
/// current tool state, we regenerate the mesh; otherwise we leave it
/// alone. Prevents allocating a fresh mesh every frame.
#[derive(Resource, Default)]
struct PreviewMeshState {
    /// `(kind, size×10, corner_r×10, wave_amp×100, wave_freq)`.
    last: Option<(ToolKind, i32, i32, i32, i32)>,
}

/// Ghost material pairs by tool family. Idle = hover bright; active =
/// dim while LMB is held.
#[derive(Clone)]
struct GhostPair {
    idle: Handle<StandardMaterial>,
    active: Handle<StandardMaterial>,
}

#[derive(Resource)]
struct PreviewMaterials {
    /// Default blue — Clay, Smooth, cutters, paddle, select, move.
    default: GhostPair,
    /// Amber — Press (displace in).
    press: GhostPair,
    /// Teal — Pull (displace out).
    pull: GhostPair,
    /// Steel — Knife (remove).
    knife: GhostPair,
}

impl PreviewMaterials {
    fn pair_for(&self, kind: ToolKind) -> &GhostPair {
        match kind {
            // Paddle is also volume-conserving displace — same amber
            // family as Press so "squash" tools read together.
            ToolKind::Press | ToolKind::Paddle => &self.press,
            ToolKind::Pull => &self.pull,
            ToolKind::Knife => &self.knife,
            _ => &self.default,
        }
    }
}

/// Vertical thickness of the cutter preview prism in mm, distributed
/// half above and half below the hit point. Kept short so the preview
/// visualises orientation without hiding the workpiece behind a long
/// tube.
const CUTTER_PREVIEW_LENGTH: f32 = 24.0;
/// Knife ghost is shorter — a shallow bite, not a through-cut.
const KNIFE_PREVIEW_LENGTH: f32 = 14.0;

/// How many segments to use when discretising a circle profile.
/// Higher = smoother circle at the cost of a few more triangles.
const CIRCLE_SEGMENTS: u32 = 32;

pub fn plugin(app: &mut App) {
    app.init_resource::<PreviewMeshState>();
    app.add_systems(Startup, spawn_preview);
    // After TransformPropagate so camera + workpiece GlobalTransforms
    // match this frame's turntable / orbit — otherwise the ghost ray
    // is one frame behind the visible clay and drifts off the facing
    // surface after Q/E.
    app.add_systems(
        PostUpdate,
        update_preview.after(TransformSystem::TransformPropagate),
    );
}

fn make_ghost_pair(
    materials: &mut Assets<StandardMaterial>,
    idle_rgba: [f32; 4],
    idle_emissive: [f32; 3],
    active_rgba: [f32; 4],
) -> GhostPair {
    let idle = materials.add(StandardMaterial {
        base_color: Color::srgba(idle_rgba[0], idle_rgba[1], idle_rgba[2], idle_rgba[3]),
        emissive: LinearRgba::new(idle_emissive[0], idle_emissive[1], idle_emissive[2], 1.0),
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        unlit: false,
        ..default()
    });
    let active = materials.add(StandardMaterial {
        base_color: Color::srgba(active_rgba[0], active_rgba[1], active_rgba[2], active_rgba[3]),
        emissive: LinearRgba::new(0.0, 0.0, 0.0, 1.0),
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        unlit: false,
        ..default()
    });
    GhostPair { idle, active }
}

fn spawn_preview(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let handle = meshes.add(Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    ));
    let default = make_ghost_pair(
        &mut materials,
        [0.35, 0.65, 1.0, 0.32],
        [0.10, 0.22, 0.38],
        [0.75, 0.85, 1.0, 0.12],
    );
    let press = make_ghost_pair(
        &mut materials,
        [0.95, 0.55, 0.18, 0.34],
        [0.35, 0.16, 0.04],
        [0.95, 0.75, 0.45, 0.12],
    );
    let pull = make_ghost_pair(
        &mut materials,
        [0.25, 0.85, 0.55, 0.34],
        [0.06, 0.28, 0.16],
        [0.55, 0.90, 0.70, 0.12],
    );
    let knife = make_ghost_pair(
        &mut materials,
        [0.70, 0.76, 0.82, 0.40],
        [0.18, 0.20, 0.24],
        [0.85, 0.88, 0.92, 0.14],
    );
    commands.spawn((
        Mesh3d(handle),
        MeshMaterial3d(default.idle.clone()),
        Transform::default(),
        Visibility::Hidden,
        ToolPreview,
    ));
    commands.insert_resource(PreviewMaterials {
        default,
        press,
        pull,
        knife,
    });
}

#[allow(clippy::too_many_arguments)]
fn update_preview(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    q_preview: Query<(Entity, &Mesh3d), With<ToolPreview>>,
    workpiece: Res<LayersState>,
    tool: Res<SculptTool>,
    pen: Res<PenState>,
    settings: Res<AppSettings>,
    preview_mats: Res<PreviewMaterials>,
    mut state: ResMut<PreviewMeshState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut q_transforms: Query<&mut Transform>,
    mut q_visibility: Query<&mut Visibility>,
    mut q_materials: Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    let Ok((preview_entity, mesh3d)) = q_preview.get_single() else {
        return;
    };

    // (1) Rebuild the mesh if the tool identity, size, or cutter
    // shape params changed.
    let p = tool.cutter_params;
    let signature = (
        tool.kind,
        (tool.size * 10.0).round() as i32,
        (p.corner_radius * 10.0).round() as i32,
        (p.wave_amp * 100.0).round() as i32,
        p.wave_freq.round() as i32,
    );
    if state.last != Some(signature) {
        let new_mesh = build_preview_mesh(tool.kind, tool.size, tool.cutter_params);
        meshes.insert(mesh3d.0.id(), new_mesh);
        state.last = Some(signature);
    }

    // (2) Swap between idle-bright and active-dim materials for the
    // current tool family so Press / Pull / Knife read differently
    // from Add/Remove at a glance.
    let sculpting = buttons.pressed(MouseButton::Left);
    if let Ok(mut mat) = q_materials.get_mut(preview_entity) {
        let pair = preview_mats.pair_for(tool.kind);
        let want = if sculpting {
            pair.active.clone()
        } else {
            pair.idle.clone()
        };
        if mat.0.id() != want.id() {
            *mat = MeshMaterial3d(want);
        }
    }

    let Ok(window) = q_window.get_single() else {
        hide(preview_entity, &mut q_visibility);
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        hide(preview_entity, &mut q_visibility);
        return;
    };
    let Ok((camera, cam_tf)) = q_camera.get_single() else {
        hide(preview_entity, &mut q_visibility);
        return;
    };
    let Ok(piece_tf) = q_piece.get_single() else {
        hide(preview_entity, &mut q_visibility);
        return;
    };

    let ray_world = match camera.viewport_to_world(cam_tf, cursor) {
        Ok(r) => r,
        Err(_) => {
            hide(preview_entity, &mut q_visibility);
            return;
        }
    };

    let piece_inv = piece_tf.affine().inverse();
    let origin_local = piece_inv.transform_point3(ray_world.origin);
    let dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();
    let hit = match workpiece.grid().ray_march(
        GVec3::new(origin_local.x, origin_local.y, origin_local.z),
        GVec3::new(dir_local.x, dir_local.y, dir_local.z),
        4000.0,
    ) {
        Some(p) => p,
        None => {
            hide(preview_entity, &mut q_visibility);
            return;
        }
    };
    let grad = workpiece.grid().gradient_at(hit);
    let normal = if grad.length_squared() > 1e-4 {
        grad.normalize()
    } else {
        -GVec3::new(dir_local.x, dir_local.y, dir_local.z)
    };
    let into = -normal;

    if let Ok(mut tf) = q_transforms.get_mut(preview_entity) {
        let hit_g = hit;
        let into_g = into;
        let view_g = GVec3::new(dir_local.x, dir_local.y, dir_local.z);
        let normal_local = Vec3::new(normal.x, normal.y, normal.z);

        let local_pos = match tool.kind {
            ToolKind::Clay => {
                let adding =
                    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
                let advance =
                    tool.advance_per_step * pen.depth_scale(settings.pressure_to_depth);
                let c = clay_brush_center(
                    hit_g,
                    into_g,
                    view_g,
                    tool.size,
                    advance,
                    adding,
                );
                Vec3::new(c.x, c.y, c.z)
            }
            ToolKind::Press | ToolKind::Pull => {
                let advance =
                    tool.advance_per_step * pen.depth_scale(settings.pressure_to_depth);
                let c = press_brush_center(hit_g, into_g, tool.size, advance);
                Vec3::new(c.x, c.y, c.z)
            }
            ToolKind::Smooth => Vec3::new(hit.x, hit.y, hit.z) + normal_local * 0.15,
            ToolKind::Cutter(_) | ToolKind::Paddle | ToolKind::WireCutter | ToolKind::Knife => {
                Vec3::new(hit.x, hit.y, hit.z) + normal_local * 0.15
            }
            ToolKind::Select | ToolKind::Move => {
                Vec3::new(hit.x, hit.y, hit.z) + normal_local * 0.15
            }
        };
        tf.translation = piece_tf.transform_point(local_pos);

        tf.rotation = match tool.kind {
            ToolKind::Clay
            | ToolKind::Press
            | ToolKind::Pull
            | ToolKind::Smooth
            | ToolKind::WireCutter
            | ToolKind::Select
            | ToolKind::Move => Quat::IDENTITY,
            ToolKind::Cutter(_) | ToolKind::Paddle | ToolKind::Knife => {
                let normal_world = piece_tf.rotation() * normal_local;
                let n = if normal_world.length_squared() > 1e-8 {
                    normal_world.normalize()
                } else {
                    Vec3::Y
                };
                Quat::from_rotation_arc(Vec3::Y, n)
            }
        };
    }

    if let Ok(mut vis) = q_visibility.get_mut(preview_entity) {
        *vis = Visibility::Visible;
    }
}

fn hide(entity: Entity, q_visibility: &mut Query<&mut Visibility>) {
    if let Ok(mut vis) = q_visibility.get_mut(entity) {
        *vis = Visibility::Hidden;
    }
}

/// Build the preview mesh for the given tool state.
fn build_preview_mesh(kind: ToolKind, size: f32, params: CutterParams) -> Mesh {
    match kind {
        ToolKind::Clay | ToolKind::Press | ToolKind::Pull | ToolKind::Smooth => {
            Sphere::new(size).mesh().uv(24, 16)
        }
        ToolKind::Select | ToolKind::Move => Sphere::new(2.5).mesh().uv(16, 12),
        ToolKind::Cutter(family) => {
            let profile = family.profile(size, params);
            build_prism_mesh(&profile, CUTTER_PREVIEW_LENGTH * 0.5)
        }
        ToolKind::Knife => {
            let profile = knife_profile(size);
            build_prism_mesh(&profile, KNIFE_PREVIEW_LENGTH * 0.5)
        }
        ToolKind::WireCutter => Sphere::new(2.5).mesh().uv(16, 12),
        ToolKind::Paddle => Cylinder::new(size, 1.5).mesh().resolution(32).build(),
    }
}

/// Build a prism mesh from a profile's 2D outline, extruded ±half_length
/// along the local Y axis. Fan-triangulated caps from the centroid
/// (safe for all Stage-2 profiles including the star, which is
/// star-shaped in the polygon sense — every point on the boundary is
/// visible from the centre).
fn build_prism_mesh(profile: &Profile, half_length: f32) -> Mesh {
    let outline = profile.outline(CIRCLE_SEGMENTS);
    let n = outline.len();

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2 + 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * 2 + 2);
    let mut indices: Vec<u32> = Vec::new();

    for GVec2 { x, y } in &outline {
        positions.push([*x, -half_length, *y]);
        normals.push(direction_normal(*x, *y));
    }
    for GVec2 { x, y } in &outline {
        positions.push([*x, half_length, *y]);
        normals.push(direction_normal(*x, *y));
    }

    let bottom_centre = positions.len() as u32;
    positions.push([0.0, -half_length, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    let top_centre = positions.len() as u32;
    positions.push([0.0, half_length, 0.0]);
    normals.push([0.0, 1.0, 0.0]);

    let n32 = n as u32;
    for i in 0..n32 {
        let i_next = (i + 1) % n32;
        let b0 = i;
        let b1 = i_next;
        let t0 = i + n32;
        let t1 = i_next + n32;
        indices.extend_from_slice(&[b0, b1, t1, b0, t1, t0]);
    }
    for i in 0..n32 {
        let i_next = (i + 1) % n32;
        indices.extend_from_slice(&[bottom_centre, i_next, i]);
    }
    for i in 0..n32 {
        let i_next = (i + 1) % n32;
        indices.extend_from_slice(&[top_centre, i + n32, i_next + n32]);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn direction_normal(x: f32, y: f32) -> [f32; 3] {
    let len = (x * x + y * y).sqrt().max(1e-6);
    [x / len, 0.0, y / len]
}
