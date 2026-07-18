//! MUD geometry engine.
//!
//! Dense SDF grid in piece-local space, spherical add/remove brush,
//! cutters / smooth / paddle, marching-cubes mesh extraction.
//!
//! Bevy-independent: the app layer owns rendering and input.
//!
//! Convention: signed distance is negative inside the workpiece,
//! positive outside, zero on the surface. Distances are in the
//! same physical units as `voxel_size` (millimetres, by convention).

pub mod brush;
pub mod components;
pub mod cutter;
pub mod displace;
pub mod dual_contour;
pub mod export;
pub mod gravity;
pub mod grid;
pub mod mesh;
pub mod paddle;
pub mod plastic;
pub mod primitives;
pub mod profile;
pub mod project;
pub mod smooth;

pub use brush::{
    apply_sphere_brush, apply_sphere_brush_with_callback, BrushMode, SphereBrush,
};
pub use displace::{
    apply_press_displace, apply_press_displace_with_callback, apply_pull_displace,
    apply_pull_displace_with_callback, solid_volume_in_region, PressDisplace, PullDisplace,
};
pub use components::{label_components, ComponentField, ComponentId, EMPTY};
pub use gravity::{
    rest_component_on_bench, rest_components_on_bench, touched_region_for_translate,
    translate_component, RestSummary,
};
pub use plastic::{
    settle_components_plastic, PlasticSettleParams, PlasticSettleSummary, DEFAULT_ITERATIONS,
};
pub use cutter::{
    apply_cookie_cutter, apply_cookie_cutter_with_callback, apply_wire_cutter,
    apply_wire_cutter_with_callback, CookieCutter, WireCutter,
};
pub use export::{
    check_watertight, extract_full_mesh, extract_full_mesh_with, stl_binary_size,
    write_stl_binary, Orientation,
    WatertightReport,
};
pub use grid::{ChunkCoord, Grid, DirtyRegion, RegionSnapshot, CHUNK_SIZE};
pub use dual_contour::{extract_chunk_dc, extract_full_mesh_dc};
pub use mesh::{cavity_brightness, extract_chunk, extract_chunk_with, ExtractedMesh, MesherKind};
pub use paddle::{apply_paddle, apply_paddle_with_callback, Paddle};
pub use primitives::{apply_primitive, apply_primitive_with_callback, Primitive, PrimitiveKind};
pub use profile::{extrude_profile, Profile};
pub use project::{
    project_scene_size, project_size, read_project, read_project_scene, write_project,
    write_project_scene, ProjectLayer, ProjectScene, ReadError, MAGIC, VERSION,
};
pub use smooth::{apply_smooth_brush, apply_smooth_brush_with_callback, SmoothBrush};
