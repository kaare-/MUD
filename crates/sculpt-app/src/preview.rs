//! Ghost tool preview: a translucent shape at the cursor showing where
//! the active tool will land.
//!
//! For Add/Remove this is a sphere the same size as the brush; for a
//! cookie cutter it's a short prism of the profile shape, oriented
//! along the surface normal at the hit point. The orientation is the
//! implicit "which way the cut goes" indicator you asked for.
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

use crate::sculpt::{clay_brush_center, SculptTool, ToolKind};
use crate::workpiece::{SculptWorkpiece, WorkpieceRoot};

/// Marker component for the single preview entity.
#[derive(Component)]
pub struct ToolPreview;

/// Cache of the last-built mesh signature. When this differs from the
/// current tool state, we regenerate the mesh; otherwise we leave it
/// alone. Prevents allocating a fresh mesh every frame.
#[derive(Resource, Default)]
struct PreviewMeshState {
    last: Option<(ToolKind, i32)>,
}

/// Pair of preview materials: the bright hover ghost + the dimmer
/// "you're sculpting through me" variant. Swapping the handle on the
/// entity is cheaper than mutating the material's alpha every frame.
#[derive(Resource)]
struct PreviewMaterials {
    idle: Handle<StandardMaterial>,
    active: Handle<StandardMaterial>,
}

/// Vertical thickness of the cutter preview prism in mm, distributed
/// half above and half below the hit point. Kept short so the preview
/// visualises orientation without hiding the workpiece behind a long
/// tube.
const CUTTER_PREVIEW_LENGTH: f32 = 24.0;

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

fn spawn_preview(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Empty mesh — the update system fills it in on the first frame
    // once the tool state is available.
    let handle = meshes.add(Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    ));
    // Bright hover ghost (used when LMB is up): cool blue tint with
    // emissive so the shape reads on any workpiece colour.
    let idle = materials.add(StandardMaterial {
        base_color: Color::srgba(0.35, 0.65, 1.0, 0.32),
        emissive: LinearRgba::new(0.10, 0.22, 0.38, 1.0),
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        unlit: false,
        ..default()
    });
    // In-use dim ghost: much fainter alpha and no emissive, so it
    // reads as "cursor footprint, not primary feedback" while the
    // user is dragging.
    let active = materials.add(StandardMaterial {
        base_color: Color::srgba(0.75, 0.85, 1.0, 0.12),
        emissive: LinearRgba::new(0.0, 0.0, 0.0, 1.0),
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        unlit: false,
        ..default()
    });
    commands.spawn((
        Mesh3d(handle),
        MeshMaterial3d(idle.clone()),
        Transform::default(),
        Visibility::Hidden,
        ToolPreview,
    ));
    commands.insert_resource(PreviewMaterials { idle, active });
}

#[allow(clippy::too_many_arguments)]
fn update_preview(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    q_preview: Query<(Entity, &Mesh3d), With<ToolPreview>>,
    workpiece: Res<SculptWorkpiece>,
    tool: Res<SculptTool>,
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

    // (1) Rebuild the mesh if the tool identity or size changed.
    let signature = (tool.kind, (tool.size * 10.0).round() as i32);
    if state.last != Some(signature) {
        let new_mesh = build_preview_mesh(tool.kind, tool.size);
        meshes.insert(mesh3d.0.id(), new_mesh);
        state.last = Some(signature);
    }

    // (2) Swap between idle-bright and active-dim materials so the
    // ghost dims while sculpting (readable footprint, not primary
    // feedback) instead of vanishing entirely.
    let sculpting = buttons.pressed(MouseButton::Left);
    if let Ok(mut mat) = q_materials.get_mut(preview_entity) {
        let want = if sculpting {
            preview_mats.active.clone()
        } else {
            preview_mats.idle.clone()
        };
        if mat.0.id() != want.id() {
            *mat = MeshMaterial3d(want);
        }
    }

    // (3) Cursor ray → piece-local → SDF hit (nearest surface along
    // the camera ray = front / "top" face from the view).
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
    let hit = match workpiece.grid.ray_march(
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
    // Surface normal from the SDF gradient. Preview placement for Clay
    // matches the stamp centre; Smooth / cutters use the outward normal.
    let grad = workpiece.grid.gradient_at(hit);
    let normal = if grad.length_squared() > 1e-4 {
        grad.normalize()
    } else {
        -GVec3::new(dir_local.x, dir_local.y, dir_local.z)
    };
    let into = -normal;

    // (4) World-space placement — no parent under WorkpieceRoot.
    // piece_tf already includes the turntable rotation (propagated).
    if let Ok(mut tf) = q_transforms.get_mut(preview_entity) {
        let hit_g = hit;
        let into_g = into;
        let view_g = GVec3::new(dir_local.x, dir_local.y, dir_local.z);
        let normal_local = Vec3::new(normal.x, normal.y, normal.z);

        let local_pos = match tool.kind {
            // Same centre math as the clay stamp so the ghost shows
            // the bite, not a view-ray ball floating off the surface.
            ToolKind::Clay => {
                let adding =
                    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
                let c = clay_brush_center(
                    hit_g,
                    into_g,
                    view_g,
                    tool.size,
                    tool.advance_per_step,
                    adding,
                );
                Vec3::new(c.x, c.y, c.z)
            }
            // Smooth stamps at the contact; tiny lift avoids z-fight.
            ToolKind::Smooth => {
                Vec3::new(hit.x, hit.y, hit.z) + normal_local * 0.15
            }
            ToolKind::Cutter(_) | ToolKind::Paddle | ToolKind::WireCutter => {
                Vec3::new(hit.x, hit.y, hit.z) + normal_local * 0.15
            }
            // Select and Move show the same small pip as the wire
            // cutter ("cursor is on material"); the real feedback
            // for each is the HUD (selection details, Move widget).
            ToolKind::Select | ToolKind::Move => {
                Vec3::new(hit.x, hit.y, hit.z) + normal_local * 0.15
            }
        };
        tf.translation = piece_tf.transform_point(local_pos);

        tf.rotation = match tool.kind {
            ToolKind::Clay
            | ToolKind::Smooth
            | ToolKind::WireCutter
            | ToolKind::Select
            | ToolKind::Move => Quat::IDENTITY,
            ToolKind::Cutter(_) | ToolKind::Paddle => {
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
fn build_preview_mesh(kind: ToolKind, size: f32) -> Mesh {
    match kind {
        // Both clay and smooth are radially-symmetric sphere
        // brushes at their `size` radius — the preview mesh is
        // identical. The material tint distinguishes them if we
        // want to later (currently the same emissive blue).
        ToolKind::Clay | ToolKind::Smooth => Sphere::new(size).mesh().uv(24, 16),
        // Select and Move: same "cursor is on material" pip as the
        // wire cutter marker, in the same emissive tint so users
        // don't confuse it with a live brush.
        ToolKind::Select | ToolKind::Move => Sphere::new(2.5).mesh().uv(16, 12),
        ToolKind::Cutter(family) => {
            let profile = family.profile(size);
            build_prism_mesh(&profile, CUTTER_PREVIEW_LENGTH * 0.5)
        }
        // Wire cutter's cut direction is determined by the drag, so a
        // static hover mesh can't encode it. Show a small marker so
        // the user still gets 'cursor is on the material' feedback;
        // the actual cut plane appears on release.
        ToolKind::WireCutter => Sphere::new(2.5).mesh().uv(16, 12),
        // Paddle: a thin disk oriented so its flat face sits on the
        // surface. Cylinder along Y with a tiny height so it reads
        // as a plate rather than a rod. Alignment to the surface
        // normal is handled by the transform-rotation code below.
        ToolKind::Paddle => Cylinder::new(size, 1.5).mesh().resolution(32).build(),
    }
}

/// Build a prism mesh from a profile's 2D outline, extruded ±half_length
/// along the local Y axis. Fan-triangulated caps from the centroid
/// (safe for all four Stage-2 profiles including the star, which is
/// star-shaped in the polygon sense — every point on the boundary is
/// visible from the centre).
fn build_prism_mesh(profile: &Profile, half_length: f32) -> Mesh {
    let outline = profile.outline(CIRCLE_SEGMENTS);
    let n = outline.len();

    // Positions: bottom ring, top ring, plus two centroid vertices for
    // the fan caps. Bottom is at y = -half_length, top at +half_length.
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 2 + 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * 2 + 2);
    let mut indices: Vec<u32> = Vec::new();

    // Bottom ring, then top ring.
    for GVec2 { x, y } in &outline {
        positions.push([*x, -half_length, *y]);
        normals.push(direction_normal(*x, *y));
    }
    for GVec2 { x, y } in &outline {
        positions.push([*x, half_length, *y]);
        normals.push(direction_normal(*x, *y));
    }

    // Centroid vertices for the caps.
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, -half_length, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    let top_centre = positions.len() as u32;
    positions.push([0.0, half_length, 0.0]);
    normals.push([0.0, 1.0, 0.0]);

    let n32 = n as u32;

    // Side quads (each edge = two triangles).
    for i in 0..n32 {
        let i_next = (i + 1) % n32;
        let b0 = i;
        let b1 = i_next;
        let t0 = i + n32;
        let t1 = i_next + n32;
        // b0 - b1 - t1 and b0 - t1 - t0 (CCW viewed from outside).
        indices.extend_from_slice(&[b0, b1, t1, b0, t1, t0]);
    }

    // Bottom cap (fan from centre, wound so the normal points -Y).
    for i in 0..n32 {
        let i_next = (i + 1) % n32;
        indices.extend_from_slice(&[bottom_centre, i_next, i]);
    }
    // Top cap (fan from centre, wound so the normal points +Y).
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

/// A cheap outward-facing normal for a side-wall vertex. Uses the
/// vertex's radial direction (from the local axis) since our profiles
/// are all convex or star-convex — good enough for lighting a
/// translucent preview.
fn direction_normal(x: f32, y: f32) -> [f32; 3] {
    let len = (x * x + y * y).sqrt().max(1e-6);
    [x / len, 0.0, y / len]
}
