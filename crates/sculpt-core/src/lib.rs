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
pub mod cutter;
pub mod export;
pub mod grid;
pub mod mesh;
pub mod paddle;
pub mod profile;
pub mod project;
pub mod smooth;

pub use brush::{
    apply_sphere_brush, apply_sphere_brush_with_callback, BrushMode, SphereBrush,
};
pub use cutter::{
    apply_cookie_cutter, apply_cookie_cutter_with_callback, apply_wire_cutter,
    apply_wire_cutter_with_callback, CookieCutter, WireCutter,
};
pub use export::{extract_full_mesh, stl_binary_size, write_stl_binary, Orientation};
pub use grid::{ChunkCoord, Grid, DirtyRegion, CHUNK_SIZE};
pub use mesh::{extract_chunk, ExtractedMesh};
pub use paddle::{apply_paddle, apply_paddle_with_callback, Paddle};
pub use profile::{extrude_profile, Profile};
pub use project::{project_size, read_project, write_project, ReadError, MAGIC, VERSION};
pub use smooth::{apply_smooth_brush, apply_smooth_brush_with_callback, SmoothBrush};
