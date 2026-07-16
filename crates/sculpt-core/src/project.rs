//! Native project format: `.mudclay`.
//!
//! Round-trips the workpiece's SDF (and, from v3, its layer stack)
//! exactly, so a user can save a session, quit, and reopen later with
//! the same voxel values, resolution, and layer boundaries. STL is
//! one-way; this file is the reverse trip.
//!
//! # Format versions
//!
//! Little-endian throughout. Magic is always `b"MUDCLAY\0"`.
//!
//! ## v1 — dense single grid (legacy)
//!
//! ```text
//! magic, version=1, flags=0
//! res.xyz, voxel_size, origin.xyz
//! sample_count (= res.x*res.y*res.z)
//! raw f32 SDF samples, x-major
//! ```
//!
//! ## v2 — sparse tiles, single grid (Track A5)
//!
//! ```text
//! magic, version=2, flags=0
//! res.xyz, voxel_size, origin.xyz
//! tile_size (=32), tile_count
//! repeated: tile_ix, tile_iy, tile_iz, 32³ f32 samples
//! ```
//!
//! ## v3 — sparse tiles × layers (Track B4) — **current writer**
//!
//! ```text
//! magic, version=3, flags=0
//! res.xyz, voxel_size, origin.xyz   // shared domain
//! layer_count, active_index
//! repeated per layer:
//!   layer_id (u32)
//!   name_len (u32) + UTF-8 name
//!   visible (u32, 0 or 1)
//!   tile_count (u32)
//!   repeated: tile_ix, tile_iy, tile_iz, 32³ f32 samples
//! ```
//!
//! Readers accept v1 / v2 / v3. v1 and v2 become a one-layer scene.
//! Writers always emit v3.

use std::io::{self, Read, Write};

use glam::{UVec3, Vec3};

use crate::grid::{ChunkCoord, Grid, CHUNK_SIZE};

/// Magic bytes at the start of every `.mudclay` file.
pub const MAGIC: &[u8; 8] = b"MUDCLAY\0";

/// Current on-disk version written by [`write_project_scene`].
pub const VERSION: u32 = 3;

const VERSION_V1: u32 = 1;
const VERSION_V2: u32 = 2;
const VERSION_V3: u32 = 3;

/// Shared header prefix length: magic + version + flags + res +
/// voxel_size + origin. v1 appends `sample_count` after this; v2/v3
/// diverge.
pub const HEADER_PREFIX_LEN: usize = 44;

/// v1 header length including `sample_count`.
pub const HEADER_LEN: usize = 48;

/// Cap on a layer name so a corrupt `name_len` can't force a huge alloc.
const MAX_NAME_BYTES: usize = 256;

/// Cap on layer / tile counts for the same reason.
const MAX_LAYERS: usize = 256;
const MAX_TILES: usize = 1 << 20;

/// Samples per sparse tile (`CHUNK_SIZE³`).
const TILE_SAMPLES: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);

/// One layer as stored in a v3 project (or synthesised from v1/v2).
#[derive(Clone)]
pub struct ProjectLayer {
    pub id: u32,
    pub name: String,
    pub visible: bool,
    pub grid: Grid,
}

/// Full scene: one or more layers sharing a domain, plus which is active.
#[derive(Clone)]
pub struct ProjectScene {
    pub layers: Vec<ProjectLayer>,
    pub active: usize,
}

impl ProjectScene {
    /// Wrap a single grid as a one-layer scene (v1/v2 ingest path and
    /// the legacy [`write_project`] helper).
    pub fn from_grid(grid: Grid) -> Self {
        Self {
            layers: vec![ProjectLayer {
                id: 0,
                name: "Layer 1".into(),
                visible: true,
                grid,
            }],
            active: 0,
        }
    }

    /// Shared domain — every layer must match. Panics if empty.
    pub fn domain_res(&self) -> UVec3 {
        self.layers[0].grid.res()
    }

    pub fn domain_voxel_size(&self) -> f32 {
        self.layers[0].grid.voxel_size()
    }

    pub fn domain_origin(&self) -> Vec3 {
        self.layers[0].grid.origin()
    }
}

/// Reasons `read_project` / `read_project_scene` can fail beyond raw I/O.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    /// Declared counts / sizes don't agree (sample_count, tile_size,
    /// layer_count vs active, empty scene, …).
    HeaderInconsistent,
    /// The reserved/flags word in the header is non-zero.
    ReservedFieldSet,
    /// The file was shorter than the declared payload.
    Truncated,
    /// A layer name was not valid UTF-8.
    BadName,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "I/O error: {e}"),
            ReadError::BadMagic => write!(f, "not a MUDCLAY file (bad magic bytes)"),
            ReadError::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported .mudclay version {v} (this build reads v1–v{VERSION})"
                )
            }
            ReadError::HeaderInconsistent => write!(f, "header fields are inconsistent"),
            ReadError::ReservedFieldSet => write!(f, "reserved header field is non-zero"),
            ReadError::Truncated => write!(f, "file is shorter than the declared payload"),
            ReadError::BadName => write!(f, "layer name is not valid UTF-8"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// Write a multi-layer scene as `.mudclay` v3 (sparse tiles per layer).
pub fn write_project_scene<W: Write>(scene: &ProjectScene, writer: &mut W) -> io::Result<()> {
    if scene.layers.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot write an empty project scene",
        ));
    }
    if scene.active >= scene.layers.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "active layer index out of range",
        ));
    }
    let res = scene.domain_res();
    let voxel_size = scene.domain_voxel_size();
    let origin = scene.domain_origin();
    for layer in &scene.layers {
        if layer.grid.res() != res
            || (layer.grid.voxel_size() - voxel_size).abs() > 1e-6
            || layer.grid.origin() != origin
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "all layers must share the same domain",
            ));
        }
    }

    write_common_header(writer, VERSION_V3, res, voxel_size, origin)?;
    writer.write_all(&(scene.layers.len() as u32).to_le_bytes())?;
    writer.write_all(&(scene.active as u32).to_le_bytes())?;

    for layer in &scene.layers {
        writer.write_all(&layer.id.to_le_bytes())?;
        let name_bytes = layer.name.as_bytes();
        let name_len = (name_bytes.len() as u32).min(MAX_NAME_BYTES as u32);
        writer.write_all(&name_len.to_le_bytes())?;
        writer.write_all(&name_bytes[..name_len as usize])?;
        let visible: u32 = if layer.visible { 1 } else { 0 };
        writer.write_all(&visible.to_le_bytes())?;
        write_sparse_tiles(&layer.grid, writer)?;
    }

    writer.flush()?;
    Ok(())
}

/// Write a single grid as a one-layer v3 scene. Kept for call sites
/// and tests that don't care about layers.
pub fn write_project<W: Write>(grid: &Grid, writer: &mut W) -> io::Result<()> {
    write_project_scene(&ProjectScene::from_grid(grid.clone()), writer)
}

/// Read a `.mudclay` file (v1 / v2 / v3) into a [`ProjectScene`].
pub fn read_project_scene<R: Read>(reader: &mut R) -> Result<ProjectScene, ReadError> {
    let mut prefix = [0u8; HEADER_PREFIX_LEN];
    reader.read_exact(&mut prefix)?;

    if &prefix[0..8] != MAGIC {
        return Err(ReadError::BadMagic);
    }
    let version = u32::from_le_bytes(prefix[8..12].try_into().unwrap());
    let reserved = u32::from_le_bytes(prefix[12..16].try_into().unwrap());
    if reserved != 0 {
        return Err(ReadError::ReservedFieldSet);
    }
    // Reject unknown versions before inspecting the rest of the
    // prefix — a truncated/zeroed fixture for version 99 shouldn't
    // report HeaderInconsistent instead.
    if !matches!(version, VERSION_V1 | VERSION_V2 | VERSION_V3) {
        return Err(ReadError::UnsupportedVersion(version));
    }

    let res = UVec3::new(
        u32::from_le_bytes(prefix[16..20].try_into().unwrap()),
        u32::from_le_bytes(prefix[20..24].try_into().unwrap()),
        u32::from_le_bytes(prefix[24..28].try_into().unwrap()),
    );
    let voxel_size = f32::from_le_bytes(prefix[28..32].try_into().unwrap());
    let origin = Vec3::new(
        f32::from_le_bytes(prefix[32..36].try_into().unwrap()),
        f32::from_le_bytes(prefix[36..40].try_into().unwrap()),
        f32::from_le_bytes(prefix[40..44].try_into().unwrap()),
    );
    if res.x == 0 || res.y == 0 || res.z == 0 {
        return Err(ReadError::HeaderInconsistent);
    }

    match version {
        VERSION_V1 => {
            let grid = read_v1_body(reader, res, voxel_size, origin)?;
            Ok(ProjectScene::from_grid(grid))
        }
        VERSION_V2 => {
            let grid = read_v2_grid(reader, res, voxel_size, origin)?;
            Ok(ProjectScene::from_grid(grid))
        }
        VERSION_V3 => read_v3_body(reader, res, voxel_size, origin),
        _ => unreachable!("version gated above"),
    }
}

/// Read a project and return only its (active) grid. v3 files yield
/// the active layer's grid; v1/v2 yield the sole grid. Prefer
/// [`read_project_scene`] when layer boundaries matter.
pub fn read_project<R: Read>(reader: &mut R) -> Result<Grid, ReadError> {
    let scene = read_project_scene(reader)?;
    let idx = scene.active.min(scene.layers.len().saturating_sub(1));
    Ok(scene.layers[idx].grid.clone())
}

/// Approximate on-disk size for a single grid written as v3. Useful
/// for log messages; exact size depends on UTF-8 name length.
pub fn project_size(grid: &Grid) -> usize {
    project_scene_size(&ProjectScene::from_grid(grid.clone()))
}

/// Exact on-disk size for a v3 scene (matches what
/// [`write_project_scene`] emits).
pub fn project_scene_size(scene: &ProjectScene) -> usize {
    let mut n = HEADER_PREFIX_LEN + 8; // layer_count + active
    for layer in &scene.layers {
        let name_len = layer.name.len().min(MAX_NAME_BYTES);
        n += 4 + 4 + name_len + 4 + 4; // id, name_len, name, visible, tile_count
        n += layer.grid.allocated_tile_count() * (12 + TILE_SAMPLES * 4);
    }
    n
}

fn write_common_header<W: Write>(
    writer: &mut W,
    version: u32,
    res: UVec3,
    voxel_size: f32,
    origin: Vec3,
) -> io::Result<()> {
    writer.write_all(MAGIC)?;
    writer.write_all(&version.to_le_bytes())?;
    writer.write_all(&0u32.to_le_bytes())?;
    writer.write_all(&res.x.to_le_bytes())?;
    writer.write_all(&res.y.to_le_bytes())?;
    writer.write_all(&res.z.to_le_bytes())?;
    writer.write_all(&voxel_size.to_le_bytes())?;
    writer.write_all(&origin.x.to_le_bytes())?;
    writer.write_all(&origin.y.to_le_bytes())?;
    writer.write_all(&origin.z.to_le_bytes())?;
    Ok(())
}

fn write_sparse_tiles<W: Write>(grid: &Grid, writer: &mut W) -> io::Result<()> {
    let coords = grid.allocated_chunk_coords();
    writer.write_all(&(coords.len() as u32).to_le_bytes())?;
    let res = grid.res();
    // Matches `grid::FAR_POSITIVE` — voxels past `res` are skipped on
    // read, so the exact sentinel only matters for in-domain empties.
    const FAR: f32 = f32::MAX / 4.0;
    for coord in coords {
        writer.write_all(&coord.x.to_le_bytes())?;
        writer.write_all(&coord.y.to_le_bytes())?;
        writer.write_all(&coord.z.to_le_bytes())?;
        // Full 32³ payload, x-major within the tile (lx + ly*32 + lz*32²).
        let base = coord.voxel_min();
        let mut samples = vec![FAR; TILE_SAMPLES];
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let ix = base.x + lx;
                    let iy = base.y + ly;
                    let iz = base.z + lz;
                    if ix >= res.x || iy >= res.y || iz >= res.z {
                        continue;
                    }
                    let idx = (lx as usize)
                        + (ly as usize) * (CHUNK_SIZE as usize)
                        + (lz as usize) * (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
                    samples[idx] = grid.get(ix, iy, iz);
                }
            }
        }
        writer.write_all(bytemuck_cast(&samples))?;
    }
    Ok(())
}

fn read_v1_body<R: Read>(
    reader: &mut R,
    res: UVec3,
    voxel_size: f32,
    origin: Vec3,
) -> Result<Grid, ReadError> {
    let mut count_buf = [0u8; 4];
    reader.read_exact(&mut count_buf)?;
    let sample_count = u32::from_le_bytes(count_buf) as usize;
    let expected = (res.x as usize)
        .checked_mul(res.y as usize)
        .and_then(|n| n.checked_mul(res.z as usize))
        .ok_or(ReadError::HeaderInconsistent)?;
    if sample_count != expected {
        return Err(ReadError::HeaderInconsistent);
    }
    let mut sample_bytes = vec![0u8; sample_count * 4];
    read_exact_trunc(reader, &mut sample_bytes)?;
    let samples: Vec<f32> = sample_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Grid::from_samples(res, voxel_size, origin, samples).ok_or(ReadError::HeaderInconsistent)
}

fn read_v3_body<R: Read>(
    reader: &mut R,
    res: UVec3,
    voxel_size: f32,
    origin: Vec3,
) -> Result<ProjectScene, ReadError> {
    let layer_count = read_u32(reader)? as usize;
    let active = read_u32(reader)? as usize;
    if layer_count == 0 || layer_count > MAX_LAYERS || active >= layer_count {
        return Err(ReadError::HeaderInconsistent);
    }

    let mut layers = Vec::with_capacity(layer_count);
    for _ in 0..layer_count {
        let id = read_u32(reader)?;
        let name_len = read_u32(reader)? as usize;
        if name_len > MAX_NAME_BYTES {
            return Err(ReadError::HeaderInconsistent);
        }
        let mut name_bytes = vec![0u8; name_len];
        read_exact_trunc(reader, &mut name_bytes)?;
        let name = String::from_utf8(name_bytes).map_err(|_| ReadError::BadName)?;
        let visible = match read_u32(reader)? {
            0 => false,
            1 => true,
            _ => return Err(ReadError::HeaderInconsistent),
        };
        let grid = read_sparse_tiles_into(reader, res, voxel_size, origin)?;
        layers.push(ProjectLayer {
            id,
            name,
            visible,
            grid,
        });
    }

    Ok(ProjectScene { layers, active })
}

fn read_v2_grid<R: Read>(
    reader: &mut R,
    res: UVec3,
    voxel_size: f32,
    origin: Vec3,
) -> Result<Grid, ReadError> {
    let tile_size = read_u32(reader)?;
    if tile_size != CHUNK_SIZE {
        return Err(ReadError::HeaderInconsistent);
    }
    read_sparse_tiles_into(reader, res, voxel_size, origin)
}

fn read_sparse_tiles_into<R: Read>(
    reader: &mut R,
    res: UVec3,
    voxel_size: f32,
    origin: Vec3,
) -> Result<Grid, ReadError> {
    let tile_count = read_u32(reader)? as usize;
    if tile_count > MAX_TILES {
        return Err(ReadError::HeaderInconsistent);
    }
    let mut grid = Grid::empty(res, voxel_size, origin);
    let mut sample_bytes = vec![0u8; TILE_SAMPLES * 4];
    for _ in 0..tile_count {
        let tx = read_u32(reader)?;
        let ty = read_u32(reader)?;
        let tz = read_u32(reader)?;
        read_exact_trunc(reader, &mut sample_bytes)?;
        let coord = ChunkCoord::new(tx, ty, tz);
        let base = coord.voxel_min();
        for lz in 0..CHUNK_SIZE {
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let ix = base.x + lx;
                    let iy = base.y + ly;
                    let iz = base.z + lz;
                    if ix >= res.x || iy >= res.y || iz >= res.z {
                        continue;
                    }
                    let idx = (lx as usize)
                        + (ly as usize) * (CHUNK_SIZE as usize)
                        + (lz as usize) * (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
                    let v = f32::from_le_bytes([
                        sample_bytes[idx * 4],
                        sample_bytes[idx * 4 + 1],
                        sample_bytes[idx * 4 + 2],
                        sample_bytes[idx * 4 + 3],
                    ]);
                    grid.set(ix, iy, iz, v);
                }
            }
        }
    }
    Ok(grid)
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, ReadError> {
    let mut buf = [0u8; 4];
    read_exact_trunc(reader, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_exact_trunc<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), ReadError> {
    reader.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => ReadError::Truncated,
        _ => ReadError::Io(e),
    })
}

/// Reinterpret an `&[f32]` as `&[u8]`. Safe: `f32` has no padding, no
/// invalid bit patterns for read purposes, and 4-byte alignment is
/// stricter than 1-byte, so downcasting is fine.
fn bytemuck_cast(samples: &[f32]) -> &[u8] {
    // SAFETY: `f32` is `#[repr(C)]`-equivalent with a fixed 4-byte
    // layout and no invalid bit patterns. Casting to `u8` is always
    // sound; the resulting slice's length in bytes is 4× the sample
    // count.
    unsafe { std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 4) }
}

/// Write a legacy v1 dense file (tests / back-compat fixtures).
#[cfg(test)]
fn write_project_v1_dense<W: Write>(grid: &Grid, writer: &mut W) -> io::Result<()> {
    let res = grid.res();
    let origin = grid.origin();
    let samples = grid.to_dense();
    write_common_header(writer, VERSION_V1, res, grid.voxel_size(), origin)?;
    writer.write_all(&(samples.len() as u32).to_le_bytes())?;
    writer.write_all(bytemuck_cast(&samples))?;
    writer.flush()?;
    Ok(())
}

/// Write a v2 sparse single-grid file (tests / back-compat fixtures).
#[cfg(test)]
fn write_project_v2_sparse<W: Write>(grid: &Grid, writer: &mut W) -> io::Result<()> {
    write_common_header(writer, VERSION_V2, grid.res(), grid.voxel_size(), grid.origin())?;
    writer.write_all(&CHUNK_SIZE.to_le_bytes())?;
    write_sparse_tiles(grid, writer)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_round_trip_preserves_layers() {
        let mut a = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        let mut b = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::new(-32.0, 0.0, -32.0));
        for iz in 4..12 {
            for iy in 4..12 {
                for ix in 4..12 {
                    a.set(ix, iy, iz, -1.0);
                }
            }
        }
        for iz in 40..48 {
            for iy in 40..48 {
                for ix in 40..48 {
                    b.set(ix, iy, iz, -2.0);
                }
            }
        }
        let scene = ProjectScene {
            layers: vec![
                ProjectLayer {
                    id: 0,
                    name: "Base".into(),
                    visible: true,
                    grid: a,
                },
                ProjectLayer {
                    id: 3,
                    name: "Top lump".into(),
                    visible: false,
                    grid: b,
                },
            ],
            active: 1,
        };

        let mut buf = Vec::new();
        write_project_scene(&scene, &mut buf).unwrap();
        assert_eq!(buf.len(), project_scene_size(&scene));
        assert_eq!(&buf[8..12], &VERSION_V3.to_le_bytes());

        let loaded = read_project_scene(&mut &buf[..]).unwrap();
        assert_eq!(loaded.layers.len(), 2);
        assert_eq!(loaded.active, 1);
        assert_eq!(loaded.layers[0].name, "Base");
        assert_eq!(loaded.layers[1].name, "Top lump");
        assert!(loaded.layers[0].visible);
        assert!(!loaded.layers[1].visible);
        assert_eq!(loaded.layers[1].id, 3);
        assert_eq!(loaded.layers[0].grid.get(8, 8, 8), -1.0);
        assert_eq!(loaded.layers[1].grid.get(44, 44, 44), -2.0);
        // Hidden layer's material must still be on disk (visibility is
        // a flag, not a filter at save time).
        assert!(loaded.layers[1].grid.allocated_tile_count() > 0);
    }

    #[test]
    fn write_project_single_grid_is_v3() {
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mut buf = Vec::new();
        write_project(&g, &mut buf).unwrap();
        assert_eq!(&buf[8..12], &VERSION_V3.to_le_bytes());
        let loaded = read_project(&mut &buf[..]).unwrap();
        assert_eq!(loaded.get(16, 16, 16), g.get(16, 16, 16));
    }

    #[test]
    fn v1_dense_loads_as_single_layer_scene() {
        let g = Grid::from_sphere(
            UVec3::new(16, 16, 16),
            1.0,
            Vec3::ZERO,
            Vec3::new(8.0, 8.0, 8.0),
            4.0,
        );
        let mut buf = Vec::new();
        write_project_v1_dense(&g, &mut buf).unwrap();
        let scene = read_project_scene(&mut &buf[..]).unwrap();
        assert_eq!(scene.layers.len(), 1);
        assert_eq!(scene.active, 0);
        assert_eq!(scene.layers[0].grid.get(8, 8, 8), g.get(8, 8, 8));
    }

    #[test]
    fn v2_sparse_loads_as_single_layer_scene() {
        let mut g = Grid::empty(UVec3::new(64, 64, 64), 1.0, Vec3::ZERO);
        g.set(3, 3, 3, -1.5);
        g.set(40, 40, 40, -2.5);
        let mut buf = Vec::new();
        write_project_v2_sparse(&g, &mut buf).unwrap();
        assert_eq!(&buf[8..12], &VERSION_V2.to_le_bytes());
        // v2 files are much smaller than a dense 64³ f32 blob.
        assert!(buf.len() < 64 * 64 * 64 * 4);
        let scene = read_project_scene(&mut &buf[..]).unwrap();
        assert_eq!(scene.layers.len(), 1);
        assert_eq!(scene.layers[0].grid.get(3, 3, 3), -1.5);
        assert_eq!(scene.layers[0].grid.get(40, 40, 40), -2.5);
    }

    #[test]
    fn round_trip_preserves_every_voxel_in_touched_tiles() {
        let mut g = Grid::empty(UVec3::new(16, 12, 8), 1.5, Vec3::new(-10.0, 0.0, 3.0));
        for iz in 0..8 {
            for iy in 0..12 {
                for ix in 0..16 {
                    g.set(ix, iy, iz, (ix as f32) * 0.3 - (iy as f32) * 0.2 + (iz as f32) * 0.1);
                }
            }
        }

        let mut buf = Vec::new();
        write_project(&g, &mut buf).unwrap();
        assert_eq!(buf.len(), project_size(&g));

        let loaded = read_project(&mut &buf[..]).expect("round-trip should succeed");
        assert_eq!(loaded.res(), g.res());
        assert_eq!(loaded.voxel_size(), g.voxel_size());
        assert_eq!(loaded.origin(), g.origin());
        for iz in 0..8 {
            for iy in 0..12 {
                for ix in 0..16 {
                    assert_eq!(
                        loaded.get(ix, iy, iz),
                        g.get(ix, iy, iz),
                        "mismatch at ({ix},{iy},{iz})",
                    );
                }
            }
        }
    }

    fn expect_err<T>(res: Result<T, ReadError>, want: &str, check: impl Fn(&ReadError) -> bool) {
        match res {
            Ok(_) => panic!("expected error {want}, got Ok"),
            Err(ref e) if check(e) => {}
            Err(e) => panic!("expected error {want}, got {e}"),
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        let buf = [b'N'; HEADER_LEN + 4];
        expect_err(read_project_scene(&mut &buf[..]), "BadMagic", |e| {
            matches!(e, ReadError::BadMagic)
        });
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&99u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; HEADER_PREFIX_LEN - 12]);
        expect_err(
            read_project_scene(&mut &buf[..]),
            "UnsupportedVersion(99)",
            |e| matches!(e, ReadError::UnsupportedVersion(99)),
        );
    }

    #[test]
    fn reserved_field_set_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION_V3.to_le_bytes());
        buf.extend_from_slice(&0xdeadbeefu32.to_le_bytes());
        buf.extend_from_slice(&[0u8; HEADER_PREFIX_LEN - 16]);
        expect_err(read_project_scene(&mut &buf[..]), "ReservedFieldSet", |e| {
            matches!(e, ReadError::ReservedFieldSet)
        });
    }

    #[test]
    fn truncated_v3_body_is_rejected() {
        let g = Grid::empty(UVec3::new(32, 32, 32), 1.0, Vec3::ZERO);
        let mut buf = Vec::new();
        write_project(&g, &mut buf).unwrap();
        buf.truncate(buf.len().saturating_sub(8));
        expect_err(read_project_scene(&mut &buf[..]), "Truncated", |e| {
            matches!(e, ReadError::Truncated)
        });
    }
}
