//! Ghost tool preview: a translucent shape at the cursor showing where
//! the active tool will land.
//!
//! For the finger this is a sphere the same size as the brush; for a
//! cookie cutter it's a short prism of the profile shape, oriented
//! along the surface normal at the hit point. The orientation is the
//! implicit "which way the cut goes" indicator you asked for.
//!
//! The preview hides when:
//! - the cursor isn't over the workpiece (no ray-march hit), or
//! - the user is actively sculpting (LMB held) — the preview would
//!   otherwise fight the sculpting feedback for attention.
//!
//! One entity is spawned at startup and reused. The mesh is rebuilt
//! only when the active tool or its size changes, not every frame.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::window::PrimaryWindow;
use glam::{Vec2 as GVec2, Vec3 as GVec3};

use sculpt_core::Profile;

use crate::sculpt::{SculptTool, ToolKind};
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
    app.add_systems(Update, update_preview);
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
    let material = materials.add(StandardMaterial {
        // Warm terracotta base with alpha, plus a cool emissive so
        // the shape reads on any workpiece colour.
        base_color: Color::srgba(0.35, 0.65, 1.0, 0.32),
        emissive: LinearRgba::new(0.10, 0.22, 0.38, 1.0),
        alpha_mode: AlphaMode::Blend,
        // Two-sided so the far side of the prism doesn't punch a hole.
        double_sided: true,
        cull_mode: None,
        unlit: false,
        ..default()
    });
    commands.spawn((
        Mesh3d(handle),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
        ToolPreview,
    ));
}

#[allow(clippy::too_many_arguments)]
fn update_preview(
    buttons: Res<ButtonInput<MouseButton>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<(Entity, &GlobalTransform), With<WorkpieceRoot>>,
    q_preview: Query<(Entity, &Mesh3d), With<ToolPreview>>,
    workpiece: Res<SculptWorkpiece>,
    tool: Res<SculptTool>,
    mut state: ResMut<PreviewMeshState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
    mut q_transforms: Query<&mut Transform>,
    mut q_visibility: Query<&mut Visibility>,
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

    // (2) Hide while the user is actively sculpting; the preview
    // otherwise doubles up with the ghost of the just-carved crater.
    if buttons.pressed(MouseButton::Left) {
        if let Ok(mut vis) = q_visibility.get_mut(preview_entity) {
            *vis = Visibility::Hidden;
        }
        return;
    }

    // (3) Cursor ray → piece-local → SDF hit.
    let Ok(window) = q_window.get_single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_tf)) = q_camera.get_single() else {
        return;
    };
    let Ok((piece_entity, piece_tf)) = q_piece.get_single() else {
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
    // Surface normal from the SDF gradient. For the preview we want
    // the *outward* normal (positive SDF direction), so no negation.
    let grad = workpiece.grid.gradient_at(hit);
    let normal = if grad.length_squared() > 1e-4 {
        grad.normalize()
    } else {
        -GVec3::new(dir_local.x, dir_local.y, dir_local.z)
    };

    // (4) Position and orient the preview.
    //
    // The preview lives *under* the workpiece root so it inherits the
    // turntable rotation automatically — reparent once at startup
    // when we have piece_entity.
    if let Ok(mut tf) = q_transforms.get_mut(preview_entity) {
        // Slight bias off the surface so the preview doesn't z-fight
        // with the workpiece mesh. 0.15 mm ≈ 1/10 voxel.
        let bias = 0.15;
        let hit_bevy = Vec3::new(hit.x, hit.y, hit.z);
        let normal_bevy = Vec3::new(normal.x, normal.y, normal.z);
        tf.translation = hit_bevy + normal_bevy * bias;

        // Radially symmetric tools (finger + wire cutter marker) need
        // no rotation. Cutter prisms align their local Y axis with
        // the surface normal so the extrusion direction is visible.
        tf.rotation = match tool.kind {
            ToolKind::Finger | ToolKind::WireCutter => Quat::IDENTITY,
            ToolKind::Cutter(_) => Quat::from_rotation_arc(Vec3::Y, normal_bevy),
        };
    }
    if let Ok(mut vis) = q_visibility.get_mut(preview_entity) {
        *vis = Visibility::Visible;
    }

    // (5) Reparent the preview under the workpiece root the first
    // time we see the workpiece. This lets the turntable transform
    // reach the preview for free.
    if state.last.is_some() {
        commands.entity(preview_entity).set_parent(piece_entity);
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
        ToolKind::Finger => Sphere::new(size).mesh().uv(24, 16),
        ToolKind::Cutter(family) => {
            let profile = family.profile(size);
            build_prism_mesh(&profile, CUTTER_PREVIEW_LENGTH * 0.5)
        }
        // Wire cutter's cut direction is determined by the drag, so a
        // static hover mesh can't encode it. Show a small marker so
        // the user still gets 'cursor is on the material' feedback;
        // the actual cut plane appears on release.
        ToolKind::WireCutter => Sphere::new(2.5).mesh().uv(16, 12),
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
