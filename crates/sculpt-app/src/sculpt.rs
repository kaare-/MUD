//! Sculpt input dispatch.
//!
//! Two tools live here in Stage 2: the spherical finger from Stage 0/1
//! and the cookie cutter introduced by Stage 2. Left-drag engages the
//! finger continuously; left-click one-shots the cookie cutter.
//!
//! Symmetry is a small overlay that applies each stamp again at its
//! mirror image across the piece-local X = 0 plane. Toggle with `S`.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::Vec3 as GVec3;

use sculpt_core::{
    apply_cookie_cutter_with_callback, apply_paddle_with_callback,
    apply_smooth_brush_with_callback, apply_sphere_brush_with_callback,
    apply_wire_cutter_with_callback, BrushMode, CookieCutter, Paddle, Profile, SmoothBrush,
    SphereBrush, WireCutter,
};

use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{SculptWorkpiece, WorkpieceRoot};

/// Which tool the user is currently holding.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ToolKind {
    Finger,
    Cutter(CutterFamily),
    /// Wire cutter — a planar slab cut defined by two cursor
    /// positions (LMB down = anchor A, LMB up = anchor B). Slices
    /// all the way through anything it intersects.
    WireCutter,
    /// Smooth brush — local Laplacian blur of the SDF, ironing out
    /// high-frequency detail while leaving shape intact.
    Smooth,
    /// Paddle — a disk-shaped half-space press for making flats.
    Paddle,
}

/// The set of cookie-cutter shapes the Stage-2 palette exposes. Each
/// maps to a `Profile` via `profile(size)`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CutterFamily {
    Circle,
    Square,
    Hexagon,
    Star5,
}

impl CutterFamily {
    pub fn profile(self, size: f32) -> Profile {
        match self {
            CutterFamily::Circle => Profile::Circle { radius: size },
            CutterFamily::Square => Profile::Square { half_side: size },
            CutterFamily::Hexagon => Profile::Hexagon { radius: size },
            CutterFamily::Star5 => Profile::Star5 {
                outer: size,
                inner_ratio: 0.4,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CutterFamily::Circle => "circle",
            CutterFamily::Square => "square",
            CutterFamily::Hexagon => "hexagon",
            CutterFamily::Star5 => "star",
        }
    }
}

/// The active tool plus its size and mode flags.
///
/// `size` is a shared knob (mm). Every stampable tool interprets it
/// as its footprint radius / characteristic size. `[`, `]`, `-`, `=`,
/// and `Shift + scroll` all adjust it regardless of which tool is
/// active.
#[derive(Resource)]
pub struct SculptTool {
    pub kind: ToolKind,
    pub size: f32,
    /// Finger + paddle: how far the tool centre advances along the
    /// surface normal each frame while held. Closest thing Stage 1
    /// has to pressure sensitivity.
    pub advance_per_step: f32,
    /// Finger-only: whether to apply the magic-clay bulge on Press.
    pub displace: bool,
    /// Smooth-only: per-frame Laplacian blend factor, 0..1. Small
    /// values give a gentle polish; 1.0 fully replaces each voxel
    /// with its neighbour average in one frame.
    pub smooth_strength: f32,
}

impl Default for SculptTool {
    fn default() -> Self {
        Self {
            kind: ToolKind::Finger,
            size: 12.0,
            advance_per_step: 0.6,
            displace: true,
            smooth_strength: 0.35,
        }
    }
}

/// Live mirror plane. Piece-local X = 0. When enabled, every stamp is
/// applied twice — once at the primary contact point, once at its
/// reflection across the plane.
#[derive(Resource, Copy, Clone)]
pub struct SculptSymmetry {
    pub enabled: bool,
}

impl Default for SculptSymmetry {
    fn default() -> Self {
        // On by default, per DESIGN §9 decision 5 (symmetry defaults
        // on for the starter primitive).
        Self { enabled: true }
    }
}

/// In-flight wire-cutter state: the piece-local hit point at
/// LMB-down and the piece-local camera direction at that moment.
/// The cut plane is computed on LMB-up.
#[derive(Resource, Default)]
pub struct WireCutState {
    pub anchor_a: Option<GVec3>,
    pub view_dir_local: Option<GVec3>,
}

/// Slab thickness (mm) the wire cutter carves. Corresponds to the
/// "kerf" of a physical wire — thin, so the cut looks like a slice
/// rather than a groove.
const WIRE_CUTTER_THICKNESS: f32 = 1.5;

pub fn plugin(app: &mut App) {
    app.init_resource::<SculptTool>();
    app.init_resource::<SculptSymmetry>();
    app.init_resource::<WireCutState>();
    app.add_systems(Update, (sculpt_input, wire_cutter_input, adjust_tool));
}

#[allow(clippy::too_many_arguments)]
fn sculpt_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut stroke: ResMut<SculptStroke>,
    mut history: ResMut<UndoHistory>,
) {
    // Wire cutter has its own lifecycle (drag anchors A→B on release);
    // hand it off to `wire_cutter_input`.
    if matches!(tool.kind, ToolKind::WireCutter) {
        return;
    }

    // Stroke lifecycle. Same recorder shape for both tools; the
    // cutter just contributes fewer stamps (typically one).
    if buttons.just_pressed(MouseButton::Left) {
        stroke.recorder = Some(StrokeRecorder::default());
    }
    if buttons.just_released(MouseButton::Left) {
        if let Some(rec) = stroke.recorder.take() {
            if let Some(entry) = rec.finish(&workpiece.grid) {
                history.push_stroke(entry);
            }
        }
    }

    // Continuous tools (finger, smooth, paddle) engage every frame
    // LMB is held. The cookie cutter is a one-shot: fire on the frame
    // LMB is first pressed, then stop so a slow drag doesn't chain
    // cutter stamps by accident. Wire cutter has its own path.
    let should_engage = match tool.kind {
        ToolKind::Finger | ToolKind::Smooth | ToolKind::Paddle => {
            buttons.pressed(MouseButton::Left)
        }
        ToolKind::Cutter(_) => buttons.just_pressed(MouseButton::Left),
        ToolKind::WireCutter => return,
    };
    if !should_engage {
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

    let ray_world = match camera.viewport_to_world(cam_tf, cursor) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Transform the ray into piece-local space so tool paths are
    // recorded independent of the turntable rotation.
    let piece_inv = piece_tf.affine().inverse();
    let origin_local = piece_inv.transform_point3(ray_world.origin);
    let dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();

    let hit_g = GVec3::new(origin_local.x, origin_local.y, origin_local.z);
    let dir_g = GVec3::new(dir_local.x, dir_local.y, dir_local.z);
    let hit = match workpiece.grid.ray_march(hit_g, dir_g, 4000.0) {
        Some(p) => p,
        None => return,
    };

    // "Into surface" direction — SDF gradient points outward, so
    // negate. Fall back to the ray direction on a degenerate gradient.
    let grad = workpiece.grid.gradient_at(hit);
    let into_surface = if grad.length_squared() > 1e-4 {
        -grad.normalize()
    } else {
        dir_g
    };

    // Apply once at the primary contact, then again mirrored if
    // symmetry is on. The mirror flips both the position and the
    // press direction across piece-local X = 0.
    apply_at(
        &mut workpiece,
        stroke.recorder.as_mut(),
        tool.kind,
        &tool,
        keys.as_ref(),
        hit,
        into_surface,
    );
    if symmetry.enabled {
        let mirrored_hit = GVec3::new(-hit.x, hit.y, hit.z);
        let mirrored_dir = GVec3::new(-into_surface.x, into_surface.y, into_surface.z);
        apply_at(
            &mut workpiece,
            stroke.recorder.as_mut(),
            tool.kind,
            &tool,
            keys.as_ref(),
            mirrored_hit,
            mirrored_dir,
        );
    }
}

fn apply_at(
    workpiece: &mut SculptWorkpiece,
    mut recorder: Option<&mut StrokeRecorder>,
    kind: ToolKind,
    tool: &SculptTool,
    keys: &ButtonInput<KeyCode>,
    hit: GVec3,
    into_surface: GVec3,
) {
    let grid_res = workpiece.grid.res();

    let region = match kind {
        ToolKind::WireCutter => return, // handled by wire_cutter_input
        ToolKind::Finger => {
            let mode = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                BrushMode::Pull
            } else {
                BrushMode::Press
            };
            // Offset each frame so a held button carves progressively:
            // Press pushes deeper into the surface, Pull backs out.
            let advance = tool.advance_per_step;
            let center = match mode {
                BrushMode::Press => hit + into_surface * advance,
                BrushMode::Pull => hit - into_surface * advance,
            };
            let brush = SphereBrush {
                center,
                radius: tool.size,
                mode,
                direction: into_surface,
                displace: tool.displace,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_sphere_brush_with_callback(&mut workpiece.grid, &brush, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_sphere_brush_with_callback(&mut workpiece.grid, &brush, |_, _, _, _| {})
            }
        }
        ToolKind::Cutter(family) => {
            // Cutter cuts along the surface normal, symmetric around
            // the click point. A half-length of half the grid extent
            // is enough to punch through any Stage-2 workpiece.
            let half_length = workpiece.grid.extent().max_element() * 0.5;
            let cutter = CookieCutter {
                profile: family.profile(tool.size),
                origin: hit,
                axis: into_surface,
                half_length,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_cookie_cutter_with_callback(&mut workpiece.grid, &cutter, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_cookie_cutter_with_callback(&mut workpiece.grid, &cutter, |_, _, _, _| {})
            }
        }
        ToolKind::Smooth => {
            let brush = SmoothBrush {
                center: hit,
                radius: tool.size,
                strength: tool.smooth_strength,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_smooth_brush_with_callback(&mut workpiece.grid, &brush, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_smooth_brush_with_callback(&mut workpiece.grid, &brush, |_, _, _, _| {})
            }
        }
        ToolKind::Paddle => {
            // Paddle plane advances into the surface each frame, so
            // holding the button gradually flattens the piece to a
            // deeper plane. `normal` points *outward* from the
            // workpiece (opposite the surface's into-direction).
            let advance = tool.advance_per_step;
            let paddle = Paddle {
                center: hit + into_surface * advance,
                normal: -into_surface,
                radius: tool.size,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_paddle_with_callback(&mut workpiece.grid, &paddle, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_paddle_with_callback(&mut workpiece.grid, &paddle, |_, _, _, _| {})
            }
        }
    };

    if let Some(region) = region {
        if let Some(rec) = recorder.as_mut() {
            rec.record_dirty_region(region, grid_res);
        }
        for c in region.touched_chunks(grid_res) {
            workpiece.dirty.insert((c.x, c.y, c.z));
        }
    }
}

/// Wire cutter — drag-to-slice input handler.
///
/// LMB down records `anchor_a` (the surface hit at the click), LMB up
/// records `anchor_b` and computes the cut plane from those two
/// points plus the piece-local camera view direction:
///
/// - `anchor  = midpoint(A, B)`, chosen so a small stroke still lands
///   the plane where the user aimed rather than at one end.
/// - `normal  = normalize(view_dir × (B − A))`, i.e. perpendicular to
///   both the wire (B − A) and the pulling direction (view). If the
///   drag is degenerate (A ≈ B or nearly along the view direction),
///   we bail — no cut.
///
/// Symmetry is honoured: the mirrored cut has both `anchor` and
/// `normal` reflected across the piece-local X = 0 plane.
#[allow(clippy::too_many_arguments)]
fn wire_cutter_input(
    buttons: Res<ButtonInput<MouseButton>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&GlobalTransform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    mut workpiece: ResMut<SculptWorkpiece>,
    mut stroke: ResMut<SculptStroke>,
    mut history: ResMut<UndoHistory>,
    mut state: ResMut<WireCutState>,
) {
    if !matches!(tool.kind, ToolKind::WireCutter) {
        // Clean up any stale state if the user switched tools mid-drag.
        state.anchor_a = None;
        state.view_dir_local = None;
        return;
    }

    // Resolve the cursor ray in piece-local space. Returns (hit, dir_local)
    // if the cursor is over the workpiece surface, else None.
    let sample = || -> Option<(GVec3, GVec3)> {
        let window = q_window.get_single().ok()?;
        let cursor = window.cursor_position()?;
        let (camera, cam_tf) = q_camera.get_single().ok()?;
        let piece_tf = q_piece.get_single().ok()?;
        let ray_world = camera.viewport_to_world(cam_tf, cursor).ok()?;
        let piece_inv = piece_tf.affine().inverse();
        let origin_local = piece_inv.transform_point3(ray_world.origin);
        let dir_local = piece_inv
            .transform_vector3(*ray_world.direction)
            .normalize();
        let hit = workpiece.grid.ray_march(
            GVec3::new(origin_local.x, origin_local.y, origin_local.z),
            GVec3::new(dir_local.x, dir_local.y, dir_local.z),
            4000.0,
        )?;
        Some((
            hit,
            GVec3::new(dir_local.x, dir_local.y, dir_local.z),
        ))
    };

    // LMB down: record anchor A and start a stroke recorder.
    if buttons.just_pressed(MouseButton::Left) {
        if let Some((hit, dir)) = sample() {
            state.anchor_a = Some(hit);
            state.view_dir_local = Some(dir);
            stroke.recorder = Some(StrokeRecorder::default());
        }
    }

    // LMB up: complete the cut.
    if buttons.just_released(MouseButton::Left) {
        // Take the recorded anchor and view direction, whether or not
        // we get a valid B — clearing state must happen either way.
        let anchor_a = state.anchor_a.take();
        let view_dir = state.view_dir_local.take();
        let recorder_opt = stroke.recorder.take();

        if let (Some(a), Some(view), Some(rec)) = (anchor_a, view_dir, recorder_opt) {
            // Try to get anchor B from the current cursor position.
            // If the release wasn't over the workpiece, use whatever
            // the ray hit — if that fails too, drop the stroke.
            let b_opt = sample().map(|(hit, _)| hit);
            let mut rec = rec;

            if let Some(b) = b_opt {
                let mut rec_opt: Option<&mut StrokeRecorder> = Some(&mut rec);
                apply_wire_cut(
                    &mut workpiece,
                    &mut rec_opt,
                    a,
                    b,
                    view,
                    symmetry.enabled,
                );
            }

            if let Some(entry) = rec.finish(&workpiece.grid) {
                history.push_stroke(entry);
            }
        }
    }
}

/// Apply a wire-cutter slab between anchors `a` and `b`, plus its
/// mirror if `symmetric` is set. All inputs are in piece-local mm.
fn apply_wire_cut(
    workpiece: &mut SculptWorkpiece,
    recorder: &mut Option<&mut StrokeRecorder>,
    a: GVec3,
    b: GVec3,
    view_dir_local: GVec3,
    symmetric: bool,
) {
    let wire = b - a;
    // Degenerate strokes: A ≈ B (a tap, no drag) or wire ≈ view dir
    // (the cross product would be near-zero). Silently drop rather
    // than making a plane with garbage normal.
    let wire_len_sq = wire.length_squared();
    if wire_len_sq < 4.0 {
        return;
    }
    let normal = view_dir_local.cross(wire);
    let n_len_sq = normal.length_squared();
    if n_len_sq < 1e-4 {
        return;
    }
    let normal = normal / n_len_sq.sqrt();
    let anchor = (a + b) * 0.5;

    // Primary cut.
    stamp_wire(workpiece, recorder, anchor, normal);

    // Mirror cut. Reflect anchor and normal across piece-local X = 0.
    if symmetric {
        let mirrored_anchor = GVec3::new(-anchor.x, anchor.y, anchor.z);
        let mirrored_normal = GVec3::new(-normal.x, normal.y, normal.z);
        // Skip a duplicate cut when the primary anchor is on the mirror
        // plane and the normal has no X component — the mirror would
        // land on top of the primary.
        let is_duplicate = anchor.x.abs() < 1e-3 && normal.x.abs() < 1e-3;
        if !is_duplicate {
            stamp_wire(workpiece, recorder, mirrored_anchor, mirrored_normal);
        }
    }
}

fn stamp_wire(
    workpiece: &mut SculptWorkpiece,
    recorder: &mut Option<&mut StrokeRecorder>,
    anchor: GVec3,
    normal: GVec3,
) {
    let grid_res = workpiece.grid.res();
    let cutter = WireCutter {
        anchor,
        normal,
        thickness: WIRE_CUTTER_THICKNESS,
        workbench_y: Some(0.0),
    };
    let region = if let Some(rec) = recorder.as_deref_mut() {
        let r = apply_wire_cutter_with_callback(&mut workpiece.grid, &cutter, |x, y, z, pre| {
            rec.record_pre_value(x, y, z, pre)
        });
        if let Some(region) = r {
            rec.record_dirty_region(region, grid_res);
        }
        r
    } else {
        apply_wire_cutter_with_callback(&mut workpiece.grid, &cutter, |_, _, _, _| {})
    };
    if let Some(region) = region {
        for c in region.touched_chunks(grid_res) {
            workpiece.dirty.insert((c.x, c.y, c.z));
        }
    }
}

/// Minimum and maximum tool size in mm. Below the min the sculpt
/// footprint is smaller than a voxel; above the max it dwarfs the
/// starter primitive.
const SIZE_MIN: f32 = 2.0;
const SIZE_MAX: f32 = 60.0;
/// Per-keystroke size step. `SIZE_STEP_UP` = `1 / SIZE_STEP_DOWN` so
/// pressing "smaller then bigger" returns you to the same size.
const SIZE_STEP_DOWN: f32 = 0.85;
const SIZE_STEP_UP: f32 = 1.0 / 0.85;
/// Shift-scroll size sensitivity (per wheel tick).
const SIZE_SCROLL_SENSITIVITY: f32 = 0.06;

/// Tool selection, size, magic-clay toggle, symmetry toggle.
///
/// Size can be adjusted three equivalent ways so no keyboard layout
/// leaves you stranded:
///
/// - `[` / `]` — the classic brush-size convention.
/// - `-` / `=` — same, on physical keys that live outside the
///   alphabet range on most non-US layouts.
/// - `Shift + scroll` — universal; works on Mac trackpads too.
///
/// Every change logs the new size so you get instant feedback.
fn adjust_tool(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: EventReader<MouseWheel>,
    mut tool: ResMut<SculptTool>,
    mut symmetry: ResMut<SculptSymmetry>,
) {
    if keys.just_pressed(KeyCode::Digit1) {
        tool.kind = ToolKind::Finger;
        bevy::log::info!("tool: finger");
    }
    if keys.just_pressed(KeyCode::Digit2) {
        tool.kind = ToolKind::Cutter(CutterFamily::Circle);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Circle.label());
    }
    if keys.just_pressed(KeyCode::Digit3) {
        tool.kind = ToolKind::Cutter(CutterFamily::Square);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Square.label());
    }
    if keys.just_pressed(KeyCode::Digit4) {
        tool.kind = ToolKind::Cutter(CutterFamily::Hexagon);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Hexagon.label());
    }
    if keys.just_pressed(KeyCode::Digit5) {
        tool.kind = ToolKind::Cutter(CutterFamily::Star5);
        bevy::log::info!("tool: cutter/{}", CutterFamily::Star5.label());
    }
    if keys.just_pressed(KeyCode::Digit6) {
        tool.kind = ToolKind::WireCutter;
        bevy::log::info!("tool: wire cutter");
    }
    if keys.just_pressed(KeyCode::Digit7) {
        tool.kind = ToolKind::Smooth;
        bevy::log::info!("tool: smooth");
    }
    if keys.just_pressed(KeyCode::Digit8) {
        tool.kind = ToolKind::Paddle;
        bevy::log::info!("tool: paddle");
    }

    // Size: three equivalent ways to adjust it. Track the old value
    // so we only log when it actually changes.
    let old_size = tool.size;

    // Keyboard: [ / ] and - / =.
    if keys.just_pressed(KeyCode::BracketLeft) || keys.just_pressed(KeyCode::Minus) {
        tool.size = (tool.size * SIZE_STEP_DOWN).max(SIZE_MIN);
    }
    if keys.just_pressed(KeyCode::BracketRight) || keys.just_pressed(KeyCode::Equal) {
        tool.size = (tool.size * SIZE_STEP_UP).min(SIZE_MAX);
    }

    // Shift + scroll wheel. When Shift is held the camera plugin
    // yields, so this doesn't double up with zoom.
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut scroll_delta = 0.0f32;
    for ev in wheel.read() {
        if shift {
            scroll_delta += ev.y;
        }
    }
    if scroll_delta != 0.0 {
        tool.size = (tool.size * (1.0 + scroll_delta * SIZE_SCROLL_SENSITIVITY))
            .clamp(SIZE_MIN, SIZE_MAX);
    }

    if (tool.size - old_size).abs() > 0.05 {
        bevy::log::info!("tool size: {:.1} mm", tool.size);
    }

    if keys.just_pressed(KeyCode::KeyM) {
        tool.displace = !tool.displace;
        bevy::log::info!(
            "magic-clay displacement: {}",
            if tool.displace { "on" } else { "off" }
        );
    }

    if keys.just_pressed(KeyCode::KeyS) {
        symmetry.enabled = !symmetry.enabled;
        bevy::log::info!(
            "symmetry: {}",
            if symmetry.enabled { "on" } else { "off" }
        );
    }
}
