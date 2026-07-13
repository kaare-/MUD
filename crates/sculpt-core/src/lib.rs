//! MUD geometry engine.
//!
//! Stage 0 scope:
//! - one dense SDF grid (`Grid`) in a single piece-local frame,
//! - one brush primitive (`SphereBrush`) with press / pull modes,
//! - marching-cubes mesh extraction on chunks of that grid.
//!
//! Everything here is deliberately Bevy-independent. The app layer
//! consumes the mesh output and owns the render side.
//!
//! Convention: signed distance is negative inside the workpiece,
//! positive outside, zero on the surface. Distances are in the
//! same physical units as `voxel_size` (millimetres, by convention).

pub mod brush;
pub mod grid;
pub mod mesh;

pub use brush::{apply_sphere_brush, BrushMode, SphereBrush};
pub use grid::{ChunkCoord, Grid, DirtyRegion, CHUNK_SIZE};
pub use mesh::{extract_chunk, ExtractedMesh};
