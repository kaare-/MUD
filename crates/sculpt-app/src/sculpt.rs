//! Sculpt input dispatch.
//!
//! Two tools live here in Stage 2: the spherical add/remove clay brush
//! from Stage 0/1 and the cookie cutter introduced by Stage 2. Left-drag
//! engages clay continuously; left-click one-shots the cookie cutter.
//!
//! Symmetry is a small overlay that applies each stamp again at its
//! mirror image across the piece-local X = 0 plane. Toggle with `S`.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use glam::Vec3 as GVec3;

use sculpt_core::{
    apply_cookie_cutter_with_callback, apply_paddle_with_callback,
    apply_press_displace_with_callback, apply_smooth_brush_with_callback,
    apply_sphere_brush_with_callback, apply_wire_cutter_with_callback, label_components,
    BrushMode, ComponentField, CookieCutter, Paddle, PressDisplace, Profile, SmoothBrush,
    SphereBrush, WireCutter,
};

use crate::actions::AppAction;
use crate::input_gate::UiCapturesInput;
use crate::pen::PenState;
use crate::selection::Selection;
use crate::settings::AppSettings;
use crate::turntable::TurntableState;
use crate::undo::{SculptStroke, StrokeRecorder, UndoHistory};
use crate::workpiece::{LayersState, WorkpieceRoot};

/// Which tool the user is currently holding.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ToolKind {
    /// Spherical add (Shift+LMB) / remove (LMB) brush.
    Clay,
    /// Volume-conserving finger press (DESIGN §2.4). Separate from
    /// Clay Add/Remove — displaces with rim recruitment.
    Press,
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
    /// Select: LMB picks the connected component under the cursor,
    /// so the user can Delete it or scope other tools to it via
    /// active-only mode.
    Select,
    /// Move: rigidly translate the currently-selected component
    /// along X / Y / Z via a small numeric HUD widget. Picking
    /// with LMB (like Select) is also accepted so users can jump
    /// straight into "pick + move".
    Move,
}

/// The set of cookie-cutter shapes the Stage-2 palette exposes. Each
/// maps to a `Profile` via `profile(size, params)`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CutterFamily {
    Circle,
    Square,
    Hexagon,
    Star5,
}

/// Optional shape tweaks for circle / square cutters (Edit → Tool
/// parameters). Hex and star ignore these for now.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CutterParams {
    /// Square corner fillet in mm. `0` = sharp square.
    pub corner_radius: f32,
    /// Circle radial wave amplitude as a fraction of size (`0` = smooth).
    pub wave_amp: f32,
    /// Number of waves around a circle outline.
    pub wave_freq: f32,
}

impl Default for CutterParams {
    fn default() -> Self {
        Self {
            corner_radius: 0.0,
            wave_amp: 0.0,
            wave_freq: 6.0,
        }
    }
}

impl CutterFamily {
    pub fn profile(self, size: f32, params: CutterParams) -> Profile {
        match self {
            CutterFamily::Circle => {
                let amp = params.wave_amp.clamp(0.0, 0.45);
                if amp > 1e-4 {
                    Profile::WavyCircle {
                        radius: size,
                        amp: amp * size,
                        freq: params.wave_freq.clamp(2.0, 24.0),
                    }
                } else {
                    Profile::Circle { radius: size }
                }
            }
            CutterFamily::Square => {
                let cr = params.corner_radius.clamp(0.0, size * 0.95);
                if cr > 1e-4 {
                    Profile::RoundedSquare {
                        half_side: size,
                        corner_r: cr,
                    }
                } else {
                    Profile::Square { half_side: size }
                }
            }
            CutterFamily::Hexagon => Profile::Hexagon { radius: size },
            CutterFamily::Star5 => Profile::Star5 {
                outer: size,
                inner_ratio: 0.4,
            },
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
    /// Clay: shallow bite / side-column gain used when placing the
    /// brush centre. Paddle: how far the plane advances into the
    /// surface each frame while held.
    pub advance_per_step: f32,
    /// Clay-only: whether to apply soft CSG + the magic-clay bulge
    /// on remove.
    pub displace: bool,
    /// Smooth-only: per-frame Laplacian blend factor, 0..1. Small
    /// values give a gentle polish; 1.0 fully replaces each voxel
    /// with its neighbour average in one frame.
    pub smooth_strength: f32,
    /// Cookie-cutter shape tweaks (corner radius / wave). Used when
    /// `kind` is a circle or square cutter.
    pub cutter_params: CutterParams,
}

impl Default for SculptTool {
    fn default() -> Self {
        Self {
            kind: ToolKind::Clay,
            size: 12.0,
            advance_per_step: 0.6,
            displace: true,
            smooth_strength: 0.35,
            cutter_params: CutterParams::default(),
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

/// Slab thickness (mm) the wire cutter carves. Two voxels wide at
/// the default `1.5 mm` voxel size, so the mesher has a real
/// gradient across the slab and the two cut faces come out clean
/// planes instead of a saw-tooth strip where each voxel column
/// straddles the plane edge at a slightly different y.
const WIRE_CUTTER_THICKNESS: f32 = 3.0;

pub fn plugin(app: &mut App) {
    app.init_resource::<SculptTool>();
    app.init_resource::<SculptSymmetry>();
    app.init_resource::<WireCutState>();
    app.add_systems(
        Update,
        (
            sculpt_input,
            wire_cutter_input,
            adjust_tool,
            handle_tool_actions,
        )
            // Piece `Transform` is written in TurntableSet; we sample it
            // directly so Q/E + add stays locked to this frame's angle.
            .after(crate::turntable::TurntableSet),
    );
}

/// Consume [`AppAction`] events that affect tool state. Runs in the
/// same schedule as the keyboard emitters — events are 2-frame
/// buffered so ordering doesn't matter for correctness, only for the
/// visible latency (~ 16 ms, imperceptible).
fn handle_tool_actions(
    mut events: EventReader<AppAction>,
    mut tool: ResMut<SculptTool>,
    mut symmetry: ResMut<SculptSymmetry>,
    mut stroke: ResMut<SculptStroke>,
) {
    for a in events.read() {
        match a {
            AppAction::SelectTool(kind) => {
                if tool.kind != *kind {
                    tool.kind = *kind;
                    stroke.discard_live();
                    bevy::log::info!("tool: {}", tool_label(*kind));
                }
            }
            AppAction::ToggleSymmetry => {
                symmetry.enabled = !symmetry.enabled;
                bevy::log::info!(
                    "symmetry: {}",
                    if symmetry.enabled { "on" } else { "off" }
                );
            }
            AppAction::ToggleMagicClay => {
                tool.displace = !tool.displace;
                bevy::log::info!(
                    "magic-clay displacement: {}",
                    if tool.displace { "on" } else { "off" }
                );
            }
            _ => {}
        }
    }
}

/// Human-readable label for a tool kind. Used for log lines and the
/// tool-palette UI.
pub fn tool_label(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Clay => "add/remove",
        ToolKind::Press => "press",
        ToolKind::Cutter(CutterFamily::Circle) => "cutter/circle",
        ToolKind::Cutter(CutterFamily::Square) => "cutter/square",
        ToolKind::Cutter(CutterFamily::Hexagon) => "cutter/hexagon",
        ToolKind::Cutter(CutterFamily::Star5) => "cutter/star",
        ToolKind::WireCutter => "wire cutter",
        ToolKind::Smooth => "smooth",
        ToolKind::Paddle => "paddle",
        ToolKind::Select => "select",
        ToolKind::Move => "move",
    }
}

/// Brush centre for the Press tool: seat the sphere so it engages the
/// surface by `advance` mm (pen pressure scales advance upstream).
pub fn press_brush_center(hit: GVec3, into_surface: GVec3, size: f32, advance: f32) -> GVec3 {
    let embed = (size - advance).max(size * 0.2);
    hit - into_surface * embed
}

#[allow(clippy::too_many_arguments)]
fn sculpt_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    q_piece: Query<&Transform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    pen: Res<PenState>,
    settings: Res<AppSettings>,
    ui_gate: Res<UiCapturesInput>,
    turntable: Res<TurntableState>,
    mut workpiece: ResMut<LayersState>,
    mut stroke: ResMut<SculptStroke>,
    mut history: ResMut<UndoHistory>,
    mut selection: ResMut<Selection>,
) {
    // Wire cutter, Select, and Move have their own systems (drag
    // anchors on release / pick-only click / numeric widget
    // respectively) — bail here.
    if matches!(
        tool.kind,
        ToolKind::WireCutter | ToolKind::Select | ToolKind::Move
    ) {
        return;
    }

    // Always finish on LMB release — even when the pointer is over a
    // menu/panel — otherwise a stroke started in the viewport and
    // released on the UI never hits the undo journal.
    if buttons.just_released(MouseButton::Left) {
        if let Some(rec) = stroke.recorder.take() {
            if let Some(entry) = rec.finish(workpiece.grid(), workpiece.active_id()) {
                history.push_stroke(entry);
                // Any completed stroke reshuffles component labels;
                // drop the cache so the next selection query
                // re-labels.
                selection.invalidate_labels();
            }
        }
        stroke.last_clay_hit = None;
        stroke.paint_plane = None;
        stroke.bench_paint = false;
    }

    // Menus and panels absorb new presses / continued stamping.
    if ui_gate.pointer {
        return;
    }

    if buttons.just_pressed(MouseButton::Left) {
        stroke.recorder = Some(StrokeRecorder::default());
        stroke.last_clay_hit = None;
        stroke.paint_plane = None;
        stroke.bench_paint = false;
    }

    // Continuous tools (clay, smooth, paddle) engage every frame
    // LMB is held. The cookie cutter is a one-shot: fire on the frame
    // LMB is first pressed, then stop so a slow drag doesn't chain
    // cutter stamps by accident. Wire cutter has its own path.
    let should_engage = match tool.kind {
        ToolKind::Clay | ToolKind::Press | ToolKind::Smooth | ToolKind::Paddle => {
            buttons.pressed(MouseButton::Left)
        }
        ToolKind::Cutter(_) => buttons.just_pressed(MouseButton::Left),
        // Wire cutter, Select, and Move live in their own systems
        // and must not fire the general-purpose sculpt pipeline.
        ToolKind::WireCutter | ToolKind::Select | ToolKind::Move => return,
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
    // recorded independent of the turntable rotation. Use the
    // piece `Transform` (not GlobalTransform) so we see this frame's
    // Q/E write from TurntableSet.
    let piece_inv = piece_tf.compute_affine().inverse();
    let origin_local = piece_inv.transform_point3(ray_world.origin);
    let dir_local = piece_inv
        .transform_vector3(*ray_world.direction)
        .normalize();

    let hit_g = GVec3::new(origin_local.x, origin_local.y, origin_local.z);
    let dir_g = GVec3::new(dir_local.x, dir_local.y, dir_local.z);
    let is_clay_early = matches!(tool.kind, ToolKind::Clay);
    let is_add_early = is_clay_early
        && (keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight));

    let depth_scale = pen.depth_scale(settings.pressure_to_depth);

    // Bench-coil lock: once this stroke has deposited on an empty
    // workbench, keep stamping at bench height for the rest of the
    // stroke. Dense Q/E spacing lands the next ray on the previous
    // bead; without this latch, control would switch to the surface
    // Clay path and tip-chase toward the camera (~advance per stamp).
    // Closing the coil onto the first bead then thickens by soft
    // overlap of same-height spheres — one layer, not a spiral climb.
    if is_add_early && stroke.bench_paint {
        empty_bench_add(
            &mut workpiece,
            &mut stroke,
            &tool,
            turntable.angular_vel,
            symmetry.enabled,
            depth_scale,
            hit_g,
            dir_g,
        );
        return;
    }

    let hit = match workpiece.grid().ray_march(hit_g, dir_g, 4000.0) {
        Some(p) => p,
        None => {
            // Empty worktable Add: no surface, but a click on the bench
            // still deposits a blob so the user can build up from
            // nothing. Every other tool needs a hit and bails.
            if is_add_early {
                empty_bench_add(
                    &mut workpiece,
                    &mut stroke,
                    &tool,
                    turntable.angular_vel,
                    symmetry.enabled,
                    depth_scale,
                    hit_g,
                    dir_g,
                );
            }
            return;
        }
    };

    // "Into surface" direction — SDF gradient points outward, so
    // negate. Fall back to the ray direction on a degenerate gradient.
    let grad = workpiece.grid().gradient_at(hit);
    let into_surface = if grad.length_squared() > 1e-4 {
        -grad.normalize()
    } else {
        dir_g
    };

    let is_clay = is_clay_early;
    let is_add = is_add_early;

    // Active-only gate: if the user picked a component and toggled
    // active-only on, skip stamps whose hit doesn't fall on that
    // component. Refreshes labels lazily. Not a full per-voxel mask —
    // a brush partially overlapping the boundary can still bleed —
    // but "the cursor must be on the selected lump" is a clear rule
    // the user can predict.
    if selection.active_only
        && selection.picked_voxel.is_some()
        && !stamp_is_on_selected_component(&mut selection, &workpiece, hit)
    {
        return;
    }

    let column = clay_column_weight(into_surface, dir_g);

    // Pin face-on *add* strokes to the first contact's tangent plane so
    // a front-view vertical drag can't tip-chase toward the camera.
    // Side-column / ring modes, and remove, leave the plane unlocked.
    let mut hit = hit;
    let mut into_surface = into_surface;
    if is_add && column < CLAY_COLUMN_UNLOCK {
        if let Some((plane_p, plane_n)) = stroke.paint_plane {
            let n_len_sq = plane_n.length_squared();
            if n_len_sq > 1e-8 {
                let n = plane_n * (1.0 / n_len_sq.sqrt());
                hit -= n * (hit - plane_p).dot(n);
                into_surface = n;
            }
        } else {
            stroke.paint_plane = Some((hit, into_surface));
        }
    } else {
        stroke.paint_plane = None;
    }

    // Stamp spacing. Face-on needs a generous gap so hold-still doesn't
    // race along the view. Turning or unlocked side-column uses denser
    // spacing so small brushes leave a continuous ring bead.
    // Press uses the same latch so a held click deepens in steps rather
    // than melting the surface every frame.
    let is_press = matches!(tool.kind, ToolKind::Press);
    if is_clay || is_press {
        let turning = turntable.angular_vel.abs() > 1e-4;
        let min_spacing = if is_clay && is_add && (turning || column >= CLAY_COLUMN_UNLOCK) {
            (tool.size * 0.18).clamp(0.35, 2.5)
        } else {
            (tool.size * 0.4).max(0.8)
        };
        if let Some(last) = stroke.last_clay_hit {
            if (hit - last).length_squared() < min_spacing * min_spacing {
                return;
            }
        }
    }

    // Apply once at the primary contact, then again mirrored if
    // symmetry is on. The mirror flips both the position and the
    // press direction across piece-local X = 0.
    apply_at(
        &mut workpiece,
        stroke.recorder.as_mut(),
        tool.kind,
        &tool,
        keys.as_ref(),
        depth_scale,
        hit,
        into_surface,
        dir_g,
    );
    if symmetry.enabled {
        let mirrored_hit = GVec3::new(-hit.x, hit.y, hit.z);
        let mirrored_dir = GVec3::new(-into_surface.x, into_surface.y, into_surface.z);
        let mirrored_view = GVec3::new(-dir_g.x, dir_g.y, dir_g.z);
        apply_at(
            &mut workpiece,
            stroke.recorder.as_mut(),
            tool.kind,
            &tool,
            keys.as_ref(),
            depth_scale,
            mirrored_hit,
            mirrored_dir,
            mirrored_view,
        );
    }
    if is_clay || is_press {
        stroke.last_clay_hit = Some(hit);
    }
}

/// Above this column weight, Add may grow a horizontal side column /
/// ring; below it, stamps stay face-on surface paint (plane-locked).
pub const CLAY_COLUMN_UNLOCK: f32 = 0.35;

/// True when the ray hit falls on the currently-selected component
/// (or nothing is selected). Refreshes `Selection::labels` on demand.
///
/// The "hit voxel" is a half-step *inside* the surface along the
/// gradient — the ray-march latch lands on the outside face where
/// `φ ≈ 0` but voxel labels only exist for solid (`φ < 0`) cells.
fn stamp_is_on_selected_component(
    selection: &mut Selection,
    workpiece: &LayersState,
    hit: GVec3,
) -> bool {
    let Some(picked) = selection.picked_voxel else {
        return true;
    };
    let labels = ensure_selection_labels(selection, workpiece);
    let (px, py, pz) = picked;
    let res = labels.res();
    if px >= res.x || py >= res.y || pz >= res.z {
        return false;
    }
    let target = labels.id_at(px, py, pz);
    if target == sculpt_core::EMPTY {
        return false;
    }
    let vs = workpiece.grid().voxel_size();
    let grad = workpiece.grid().gradient_at(hit);
    let inside_probe = if grad.length_squared() > 1e-4 {
        hit - grad.normalize() * vs * 0.5
    } else {
        hit
    };
    let origin = workpiece.grid().origin();
    let local = (inside_probe - origin) * (1.0 / vs);
    if local.x < 0.0
        || local.y < 0.0
        || local.z < 0.0
        || local.x >= res.x as f32
        || local.y >= res.y as f32
        || local.z >= res.z as f32
    {
        return false;
    }
    let ix = local.x as u32;
    let iy = local.y as u32;
    let iz = local.z as u32;
    labels.id_at(ix, iy, iz) == target
}

/// Lazily populate `Selection::labels` from the current grid and
/// hand back a reference. Same trick as `selection::ensure_labels_fresh`
/// but callable from the sculpt path (which already holds a
/// mutable borrow on `Selection`).
fn ensure_selection_labels<'a>(
    selection: &'a mut Selection,
    workpiece: &LayersState,
) -> &'a ComponentField {
    if selection.labels().is_none() {
        let labels = label_components(workpiece.grid());
        selection.set_labels(labels);
    }
    selection.labels().expect("just populated")
}

/// How strongly the contact wants a sideways horizontal column
/// (`0` = face-on / crown, `1` = equatorial side facing the camera).
pub fn clay_column_weight(into_surface: GVec3, view_dir: GVec3) -> f32 {
    let outward = -into_surface;
    let side = 1.0 - outward.dot(-view_dir).clamp(0.0, 1.0);
    let flatness = GVec3::new(outward.x, 0.0, outward.z)
        .length()
        .clamp(0.0, 1.0);
    side * flatness * flatness
}

/// Piece-local brush centre for the Clay tool — shared by stamping and
/// the ghost preview so hover matches the bite.
pub fn clay_brush_center(
    hit: GVec3,
    into_surface: GVec3,
    view_dir: GVec3,
    size: f32,
    advance: f32,
    adding: bool,
) -> GVec3 {
    let outward = -into_surface;
    let column = clay_column_weight(into_surface, view_dir);
    let shallow_embed = (size - advance).max(size * 0.5);
    if !adding {
        // Shallow remove: seat almost the whole sphere in air.
        return hit - into_surface * shallow_embed;
    }
    let flat_out = GVec3::new(outward.x, 0.0, outward.z);
    let flatness = flat_out.length().clamp(0.0, 1.0);
    let embed = shallow_embed * (1.0 - column) + size * 0.15 * column;
    let push = (advance * 4.0 + size * 0.05) * column * column;
    let push_dir = if flatness > 1e-3 {
        flat_out / flatness
    } else {
        GVec3::ZERO
    };
    hit + into_surface * embed + push_dir * push
}

#[allow(clippy::too_many_arguments)]
fn apply_at(
    workpiece: &mut LayersState,
    mut recorder: Option<&mut StrokeRecorder>,
    kind: ToolKind,
    tool: &SculptTool,
    keys: &ButtonInput<KeyCode>,
    depth_scale: f32,
    hit: GVec3,
    into_surface: GVec3,
    view_dir: GVec3,
) {
    let grid_res = workpiece.grid().res();

    let region = match kind {
        // These live in their own systems (wire_cutter_input,
        // selection_input, move_input) and must never run through here.
        ToolKind::WireCutter | ToolKind::Select | ToolKind::Move => return,
        ToolKind::Clay => {
            let adding =
                keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            let mode = if adding {
                BrushMode::Pull
            } else {
                BrushMode::Press
            };
            // BrushMode::Pull = add, Press = remove (historical names in
            // sculpt-core). Placement matches the ghost via
            // `clay_brush_center`. Stylus pressure scales advance (depth).
            let center = clay_brush_center(
                hit,
                into_surface,
                view_dir,
                tool.size,
                tool.advance_per_step * depth_scale,
                adding,
            );
            let brush = SphereBrush {
                center,
                radius: tool.size,
                mode,
                direction: into_surface,
                displace: tool.displace,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_sphere_brush_with_callback(workpiece.grid_mut(), &brush, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_sphere_brush_with_callback(workpiece.grid_mut(), &brush, |_, _, _, _| {})
            }
        }
        ToolKind::Press => {
            let advance = tool.advance_per_step * depth_scale;
            let center = press_brush_center(hit, into_surface, tool.size, advance);
            let brush = PressDisplace {
                center,
                radius: tool.size,
                direction: into_surface,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_press_displace_with_callback(workpiece.grid_mut(), &brush, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_press_displace_with_callback(workpiece.grid_mut(), &brush, |_, _, _, _| {})
            }
        }
        ToolKind::Cutter(family) => {
            // Cutter cuts along the surface normal, symmetric around
            // the click point. A half-length of half the grid extent
            // is enough to punch through any Stage-2 workpiece.
            // Cutters are binary punches — pressure does not apply.
            let half_length = workpiece.grid().extent().max_element() * 0.5;
            let cutter = CookieCutter {
                profile: family.profile(tool.size, tool.cutter_params),
                origin: hit,
                axis: into_surface,
                half_length,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_cookie_cutter_with_callback(workpiece.grid_mut(), &cutter, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_cookie_cutter_with_callback(workpiece.grid_mut(), &cutter, |_, _, _, _| {})
            }
        }
        ToolKind::Smooth => {
            let brush = SmoothBrush {
                center: hit,
                radius: tool.size,
                strength: tool.smooth_strength * depth_scale,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_smooth_brush_with_callback(workpiece.grid_mut(), &brush, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_smooth_brush_with_callback(workpiece.grid_mut(), &brush, |_, _, _, _| {})
            }
        }
        ToolKind::Paddle => {
            // Paddle plane advances into the surface each frame, so
            // holding the button gradually flattens the piece to a
            // deeper plane. `normal` points *outward* from the
            // workpiece (opposite the surface's into-direction).
            let advance = tool.advance_per_step * depth_scale;
            let paddle = Paddle {
                center: hit + into_surface * advance,
                normal: -into_surface,
                radius: tool.size,
                workbench_y: Some(0.0),
            };
            if let Some(rec) = recorder.as_mut() {
                apply_paddle_with_callback(workpiece.grid_mut(), &paddle, |x, y, z, pre| {
                    rec.record_pre_value(x, y, z, pre)
                })
            } else {
                apply_paddle_with_callback(workpiece.grid_mut(), &paddle, |_, _, _, _| {})
            }
        }
    };

    if let Some(region) = region {
        if let Some(rec) = recorder.as_mut() {
            rec.record_dirty_region(region, grid_res);
        }
        for c in region.touched_chunks(grid_res) {
            workpiece.mark_dirty((c.x, c.y, c.z));
        }
    }
}

/// Intersect a piece-local ray with the workbench plane (y = 0). Only
/// returns a point when the ray is aimed downward at a bench point in
/// front of the camera. Returns `None` if the ray points up, is
/// parallel to the plane, or starts below the bench.
fn ray_bench_intersection(origin: GVec3, dir: GVec3) -> Option<GVec3> {
    if origin.y <= 0.0 || dir.y >= -1e-4 {
        return None;
    }
    let t = -origin.y / dir.y;
    if t <= 0.0 {
        return None;
    }
    let p = origin + dir * t;
    Some(GVec3::new(p.x, 0.0, p.z))
}

/// Stamp a Clay-Add sphere resting on the empty workbench. Called
/// when `sculpt_input`'s ray march missed the workpiece but the user
/// is holding Shift+LMB — instead of doing nothing, deposit a blob
/// on the bench so the user can build up from nothing. All other
/// tools require an existing surface.
#[allow(clippy::too_many_arguments)]
fn empty_bench_add(
    workpiece: &mut LayersState,
    stroke: &mut SculptStroke,
    tool: &SculptTool,
    angular_vel: f32,
    symmetric: bool,
    depth_scale: f32,
    ray_origin_local: GVec3,
    ray_dir_local: GVec3,
) {
    let Some(bench) = ray_bench_intersection(ray_origin_local, ray_dir_local) else {
        return;
    };

    // Stamp spacing so hold-still doesn't puddle in one spot. Same
    // "denser while turning" idea as the surface path, since Q/E
    // with an empty-bench hold should paint a ring in the air.
    let turning = angular_vel.abs() > 1e-4;
    let min_spacing = if turning {
        (tool.size * 0.18).clamp(0.35, 2.5)
    } else {
        (tool.size * 0.4).max(0.8)
    };
    if let Some(last) = stroke.last_clay_hit {
        if (bench - last).length_squared() < min_spacing * min_spacing {
            return;
        }
    }

    // Face-on paint is not meaningful with no surface; skip the plane
    // lock and drop a sphere sitting on the bench (`center.y = size`).
    // Latch so later frames in this stroke stay on the bench even if
    // the ray hits this stroke's clay (top-view turntable coils).
    stroke.paint_plane = None;
    stroke.bench_paint = true;
    stamp_bench_blob(workpiece, stroke.recorder.as_mut(), tool, depth_scale, bench);
    if symmetric {
        let mirrored = GVec3::new(-bench.x, 0.0, bench.z);
        stamp_bench_blob(
            workpiece,
            stroke.recorder.as_mut(),
            tool,
            depth_scale,
            mirrored,
        );
    }
    stroke.last_clay_hit = Some(bench);
}

/// Stamp a single Add sphere resting on the bench at the given
/// (x, 0, z). Splits out the actual sculpt-core call so the mirrored
/// stamp reuses it. `depth_scale` shrinks the mound under light pen
/// pressure (amount ≈ depth when there is no surface to bite into).
fn stamp_bench_blob(
    workpiece: &mut LayersState,
    mut recorder: Option<&mut StrokeRecorder>,
    tool: &SculptTool,
    depth_scale: f32,
    bench: GVec3,
) {
    let grid_res = workpiece.grid().res();
    let radius = tool.size * depth_scale;
    let center = GVec3::new(bench.x, radius, bench.z);
    let brush = SphereBrush {
        center,
        radius,
        mode: BrushMode::Pull,
        direction: GVec3::new(0.0, -1.0, 0.0),
        displace: tool.displace,
        workbench_y: Some(0.0),
    };
    let region = if let Some(rec) = recorder.as_mut() {
        apply_sphere_brush_with_callback(workpiece.grid_mut(), &brush, |x, y, z, pre| {
            rec.record_pre_value(x, y, z, pre)
        })
    } else {
        apply_sphere_brush_with_callback(workpiece.grid_mut(), &brush, |_, _, _, _| {})
    };
    if let Some(region) = region {
        if let Some(rec) = recorder.as_mut() {
            rec.record_dirty_region(region, grid_res);
        }
        for c in region.touched_chunks(grid_res) {
            workpiece.mark_dirty((c.x, c.y, c.z));
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
    q_piece: Query<&Transform, With<WorkpieceRoot>>,
    tool: Res<SculptTool>,
    symmetry: Res<SculptSymmetry>,
    ui_gate: Res<UiCapturesInput>,
    mut workpiece: ResMut<LayersState>,
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

    // Piece-local cursor sample (hit, view dir).
    let sample = || -> Option<(GVec3, GVec3)> {
        let window = q_window.get_single().ok()?;
        let cursor = window.cursor_position()?;
        let (camera, cam_tf) = q_camera.get_single().ok()?;
        let piece_tf = q_piece.get_single().ok()?;
        let ray_world = camera.viewport_to_world(cam_tf, cursor).ok()?;
        let piece_inv = piece_tf.compute_affine().inverse();
        let origin_local = piece_inv.transform_point3(ray_world.origin);
        let dir_local = piece_inv
            .transform_vector3(*ray_world.direction)
            .normalize();
        let hit = workpiece.grid().ray_march(
            GVec3::new(origin_local.x, origin_local.y, origin_local.z),
            GVec3::new(dir_local.x, dir_local.y, dir_local.z),
            4000.0,
        )?;
        Some((
            hit,
            GVec3::new(dir_local.x, dir_local.y, dir_local.z),
        ))
    };

    // LMB up always resolves in-flight state (even over UI) so a drag
    // started in-world and released on a panel still finishes or clears.
    if buttons.just_released(MouseButton::Left) {
        let anchor_a = state.anchor_a.take();
        let view_dir = state.view_dir_local.take();
        let recorder_opt = stroke.recorder.take();

        if let (Some(a), Some(view), Some(rec)) = (anchor_a, view_dir, recorder_opt) {
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
            if let Some(entry) = rec.finish(workpiece.grid(), workpiece.active_id()) {
                history.push_stroke(entry);
            }
        }
        return;
    }

    if ui_gate.pointer {
        return;
    }

    if buttons.just_pressed(MouseButton::Left) {
        if let Some((hit, dir)) = sample() {
            state.anchor_a = Some(hit);
            state.view_dir_local = Some(dir);
            stroke.recorder = Some(StrokeRecorder::default());
        }
    }
}

/// Apply a wire-cutter slab between anchors `a` and `b`, plus its
/// mirror if `symmetric` is set. All inputs are in piece-local mm.
fn apply_wire_cut(
    workpiece: &mut LayersState,
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
    workpiece: &mut LayersState,
    recorder: &mut Option<&mut StrokeRecorder>,
    anchor: GVec3,
    normal: GVec3,
) {
    let grid_res = workpiece.grid().res();
    let cutter = WireCutter {
        anchor,
        normal,
        thickness: WIRE_CUTTER_THICKNESS,
        workbench_y: Some(0.0),
    };
    let region = if let Some(rec) = recorder.as_deref_mut() {
        let r = apply_wire_cutter_with_callback(workpiece.grid_mut(), &cutter, |x, y, z, pre| {
            rec.record_pre_value(x, y, z, pre)
        });
        if let Some(region) = r {
            rec.record_dirty_region(region, grid_res);
        }
        r
    } else {
        apply_wire_cutter_with_callback(workpiece.grid_mut(), &cutter, |_, _, _, _| {})
    };
    if let Some(region) = region {
        for c in region.touched_chunks(grid_res) {
            workpiece.mark_dirty((c.x, c.y, c.z));
        }
    }
}

/// Minimum and maximum tool size in mm. Below the min the sculpt
/// footprint is smaller than a voxel; above the max it dwarfs the
/// starter primitive.
pub const SIZE_MIN: f32 = 2.0;
pub const SIZE_MAX: f32 = 60.0;
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
    mut actions: EventWriter<AppAction>,
    ui_gate: Res<UiCapturesInput>,
) {
    // When egui has an active text input or a focused button, keyboard
    // events belong to the UI, not the sculpt path. Number keys and
    // toggle keys would otherwise fight the UI's own keyboard use.
    if ui_gate.keyboard {
        return;
    }

    // Tool selection — fire an event; the actual mutation happens in
    // handle_tool_actions so the UI and keyboard share exactly one code
    // path.
    let mut send_tool = |kind: ToolKind| {
        actions.send(AppAction::SelectTool(kind));
    };
    if keys.just_pressed(KeyCode::Digit1) {
        send_tool(ToolKind::Clay);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        send_tool(ToolKind::Press);
    }
    if keys.just_pressed(KeyCode::Digit2) {
        send_tool(ToolKind::Cutter(CutterFamily::Circle));
    }
    if keys.just_pressed(KeyCode::Digit3) {
        send_tool(ToolKind::Cutter(CutterFamily::Square));
    }
    if keys.just_pressed(KeyCode::Digit4) {
        send_tool(ToolKind::Cutter(CutterFamily::Hexagon));
    }
    if keys.just_pressed(KeyCode::Digit5) {
        send_tool(ToolKind::Cutter(CutterFamily::Star5));
    }
    if keys.just_pressed(KeyCode::Digit6) {
        send_tool(ToolKind::WireCutter);
    }
    if keys.just_pressed(KeyCode::Digit7) {
        send_tool(ToolKind::Smooth);
    }
    if keys.just_pressed(KeyCode::Digit8) {
        send_tool(ToolKind::Paddle);
    }
    if keys.just_pressed(KeyCode::Digit9) {
        send_tool(ToolKind::Select);
    }
    if keys.just_pressed(KeyCode::Digit0) {
        send_tool(ToolKind::Move);
    }

    // Size: continuous adjustment, stays inline (no menu path needs
    // it — it's driven by scroll and bracket keys during a stroke).
    let old_size = tool.size;
    if keys.just_pressed(KeyCode::BracketLeft) || keys.just_pressed(KeyCode::Minus) {
        tool.size = (tool.size * SIZE_STEP_DOWN).max(SIZE_MIN);
    }
    if keys.just_pressed(KeyCode::BracketRight) || keys.just_pressed(KeyCode::Equal) {
        tool.size = (tool.size * SIZE_STEP_UP).min(SIZE_MAX);
    }
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

    // Mode toggles.
    if keys.just_pressed(KeyCode::KeyM) {
        actions.send(AppAction::ToggleMagicClay);
    }
    // Note: `Ctrl+S` is also KeyS; we only toggle symmetry when Ctrl
    // is *not* held, so the save-file shortcut wins that race.
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let super_key =
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);
    if keys.just_pressed(KeyCode::KeyS) && !ctrl && !super_key {
        actions.send(AppAction::ToggleSymmetry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sculpt_core::{apply_sphere_brush_with_callback, Grid};

    fn empty_bench_workpiece() -> LayersState {
        // Small domain is enough for a few 8 mm stamps on y = 0.
        let grid = Grid::empty(
            glam::UVec3::new(64, 64, 64),
            1.0,
            GVec3::new(-32.0, 0.0, -32.0),
        );
        LayersState::new_for_test(grid)
    }

    fn tool_sized(size: f32) -> SculptTool {
        SculptTool {
            size,
            ..SculptTool::default()
        }
    }

    /// Highest world-Y of any solid (φ < 0) voxel in allocated tiles.
    fn solid_peak_y(grid: &Grid) -> f32 {
        let origin = grid.origin();
        let vs = grid.voxel_size();
        let mut peak = f32::NEG_INFINITY;
        for coord in grid.allocated_chunk_coords() {
            let base = coord.voxel_min();
            let max = coord.voxel_max(grid.res());
            for iz in base.z..max.z {
                for iy in base.y..max.y {
                    for ix in base.x..max.x {
                        if grid.get(ix, iy, iz) < 0.0 {
                            let y = origin.y + (iy as f32 + 0.5) * vs;
                            peak = peak.max(y);
                        }
                    }
                }
            }
        }
        peak
    }

    #[test]
    fn empty_bench_add_latches_bench_paint() {
        let mut workpiece = empty_bench_workpiece();
        let mut stroke = SculptStroke::default();
        let tool = tool_sized(8.0);
        // Ray from above the bench toward (10, 0, 0).
        empty_bench_add(
            &mut workpiece,
            &mut stroke,
            &tool,
            0.0,
            false,
            1.0,
            GVec3::new(10.0, 40.0, 0.0),
            GVec3::new(0.0, -1.0, 0.0),
        );
        assert!(stroke.bench_paint, "first empty-bench stamp must latch");
        assert!(stroke.last_clay_hit.is_some());
        let peak = solid_peak_y(workpiece.grid());
        assert!(
            (peak - 2.0 * tool.size).abs() < 1.5,
            "blob sitting on the bench should peak near 2*size, got {peak}"
        );
    }

    #[test]
    fn bench_coil_stamps_stay_level_while_overlapping() {
        // Simulate top-view turntable coil: many overlapping bench
        // stamps along an arc. Peak Y must stay at one bead thickness
        // even where stamps overlap — not creep toward the camera.
        let mut workpiece = empty_bench_workpiece();
        let mut stroke = SculptStroke::default();
        let tool = tool_sized(8.0);
        let radius = 14.0;
        for i in 0..24 {
            let a = (i as f32) * std::f32::consts::TAU / 24.0;
            let x = radius * a.cos();
            let z = radius * a.sin();
            empty_bench_add(
                &mut workpiece,
                &mut stroke,
                &tool,
                1.0, // turning → denser spacing path
                false,
                1.0,
                GVec3::new(x, 40.0, z),
                GVec3::new(0.0, -1.0, 0.0),
            );
            assert!(stroke.bench_paint);
        }
        let peak = solid_peak_y(workpiece.grid());
        let expected = 2.0 * tool.size;
        assert!(
            (peak - expected).abs() < 2.0,
            "coil on the bench must stay one bead thick (≈{expected}), got peak={peak}"
        );
    }

    #[test]
    fn surface_path_after_bench_blob_creeps_upward() {
        // Documents the bug the bench_paint latch prevents: once the
        // ray hits the previous bead, face-on Clay placement protrudes
        // ~advance per stamp and the peak climbs toward the camera.
        let mut workpiece = empty_bench_workpiece();
        let tool = tool_sized(8.0);
        let bench = GVec3::new(0.0, 0.0, 0.0);
        stamp_bench_blob(&mut workpiece, None, &tool, 1.0, bench);
        let after_bench = solid_peak_y(workpiece.grid());

        let into = GVec3::new(0.0, -1.0, 0.0);
        let view = GVec3::new(0.0, -1.0, 0.0);
        for _ in 0..12 {
            let hit = GVec3::new(0.0, solid_peak_y(workpiece.grid()), 0.0);
            let center = clay_brush_center(
                hit,
                into,
                view,
                tool.size,
                tool.advance_per_step,
                true,
            );
            let brush = SphereBrush {
                center,
                radius: tool.size,
                mode: BrushMode::Pull,
                direction: into,
                displace: tool.displace,
                workbench_y: Some(0.0),
            };
            let _ = apply_sphere_brush_with_callback(workpiece.grid_mut(), &brush, |_, _, _, _| {});
        }
        let after_surface = solid_peak_y(workpiece.grid());
        assert!(
            after_surface > after_bench + 2.0,
            "surface tip-chase should climb (bench peak {after_bench}, after {after_surface}); \
             if this fails, the creep regress may need a different latch"
        );
    }
}
