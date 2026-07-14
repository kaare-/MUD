//! Native project format: `.mudclay`.
//!
//! Round-trips the workpiece's SDF grid exactly, so a user can save a
//! session, quit, and reopen it later with the same voxel values,
//! same resolution, same everything. STL is one-way (you can never
//! recover an SDF from a triangle soup); this file is the reverse
//! trip.
//!
//! # Format (v1)
//!
//! Little-endian throughout.
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -------------------------------------------------
//!    0      8   Magic bytes: b"MUDCLAY\0"
//!    8      4   File format version (u32) — currently 1
//!   12      4   Reserved / flags (u32) — must be 0 in v1
//!   16      4   Grid res.x (u32)
//!   20      4   Grid res.y (u32)
//!   24      4   Grid res.z (u32)
//!   28      4   Voxel size in mm (f32)
//!   32      4   Origin x in piece-local mm (f32)
//!   36      4   Origin y (f32)
//!   40      4   Origin z (f32)
//!   44      4   Sample count (u32) — must equal res.x * res.y * res.z
//!   48    4*N   Raw f32 SDF samples, x-major (matches Grid::idx).
//! ```
//!
//! ## Why raw f32 and not compression?
//!
//! At Stage 2 grid sizes (128³) the raw payload is 8 MiB, which is
//! fine on any 2000s-or-later machine and lets us skip a zstd/flate
//! dependency. When we migrate to sparse SDF in a later stage the
//! format will change anyway; a v2 could add compression then.
//!
//! ## Versioning
//!
//! The `version` field is the sole compatibility check. A reader
//! that doesn't recognise the version returns `Err(ReadError::UnsupportedVersion)`
//! rather than trying to guess. Every future field addition bumps
//! the version.

use std::io::{self, Read, Write};

use glam::{UVec3, Vec3};

use crate::grid::Grid;

/// Magic bytes at the start of every `.mudclay` file.
pub const MAGIC: &[u8; 8] = b"MUDCLAY\0";

/// Current format version. Bump when the on-disk layout changes.
pub const VERSION: u32 = 1;

/// Header length in bytes. Kept as a const so the reader can bail
/// early on truncated files without having to parse anything.
pub const HEADER_LEN: usize = 48;

/// Reasons `read_project` can fail beyond raw I/O errors.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    /// The file's declared sample count doesn't match `res.x*res.y*res.z`,
    /// so we refuse rather than allocate a mis-shaped buffer.
    HeaderInconsistent,
    /// The reserved/flags word in the header is non-zero (v1 must be 0).
    ReservedFieldSet,
    /// The file was shorter than the declared sample count.
    Truncated,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "I/O error: {e}"),
            ReadError::BadMagic => write!(f, "not a MUDCLAY file (bad magic bytes)"),
            ReadError::UnsupportedVersion(v) => {
                write!(f, "unsupported .mudclay version {v} (this build reads v{VERSION})")
            }
            ReadError::HeaderInconsistent => write!(f, "header sample count disagrees with res.xyz"),
            ReadError::ReservedFieldSet => write!(f, "reserved header field is non-zero"),
            ReadError::Truncated => write!(f, "file is shorter than the declared sample count"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// Write the workpiece grid to `writer` in the current on-disk format.
pub fn write_project<W: Write>(grid: &Grid, writer: &mut W) -> io::Result<()> {
    let res = grid.res();
    let origin = grid.origin();
    let samples = grid.samples();
    let sample_count = samples.len() as u32;

    writer.write_all(MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&0u32.to_le_bytes())?;
    writer.write_all(&res.x.to_le_bytes())?;
    writer.write_all(&res.y.to_le_bytes())?;
    writer.write_all(&res.z.to_le_bytes())?;
    writer.write_all(&grid.voxel_size().to_le_bytes())?;
    writer.write_all(&origin.x.to_le_bytes())?;
    writer.write_all(&origin.y.to_le_bytes())?;
    writer.write_all(&origin.z.to_le_bytes())?;
    writer.write_all(&sample_count.to_le_bytes())?;

    // Bulk-write the samples. A single `write_all` beats per-voxel
    // formatting by ~50x on debug builds and matters even on release
    // for 128³ grids (8 MiB blob).
    let byte_buf: &[u8] = bytemuck_cast(samples);
    writer.write_all(byte_buf)?;
    writer.flush()?;
    Ok(())
}

/// Total on-disk size for a given grid. Useful for pre-sizing buffers
/// or reporting file size in log messages.
pub fn project_size(grid: &Grid) -> usize {
    HEADER_LEN + 4 * grid.samples().len()
}

/// Read a project file from `reader` and return the reconstructed grid.
pub fn read_project<R: Read>(reader: &mut R) -> Result<Grid, ReadError> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header)?;

    if &header[0..8] != MAGIC {
        return Err(ReadError::BadMagic);
    }
    let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if version != VERSION {
        return Err(ReadError::UnsupportedVersion(version));
    }
    let reserved = u32::from_le_bytes(header[12..16].try_into().unwrap());
    if reserved != 0 {
        return Err(ReadError::ReservedFieldSet);
    }

    let res_x = u32::from_le_bytes(header[16..20].try_into().unwrap());
    let res_y = u32::from_le_bytes(header[20..24].try_into().unwrap());
    let res_z = u32::from_le_bytes(header[24..28].try_into().unwrap());
    let voxel_size = f32::from_le_bytes(header[28..32].try_into().unwrap());
    let origin = Vec3::new(
        f32::from_le_bytes(header[32..36].try_into().unwrap()),
        f32::from_le_bytes(header[36..40].try_into().unwrap()),
        f32::from_le_bytes(header[40..44].try_into().unwrap()),
    );
    let sample_count = u32::from_le_bytes(header[44..48].try_into().unwrap()) as usize;

    let expected = (res_x as usize)
        .checked_mul(res_y as usize)
        .and_then(|n| n.checked_mul(res_z as usize))
        .ok_or(ReadError::HeaderInconsistent)?;
    if sample_count != expected {
        return Err(ReadError::HeaderInconsistent);
    }

    let mut sample_bytes = vec![0u8; sample_count * 4];
    reader
        .read_exact(&mut sample_bytes)
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => ReadError::Truncated,
            _ => ReadError::Io(e),
        })?;

    // Decode into an f32 Vec. We accept any bit pattern including
    // NaN/inf — the sculpt-core operators tolerate those (they show
    // as "empty air" via max()) and refusing them would break files
    // written by earlier bugs.
    let samples: Vec<f32> = sample_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    Grid::from_samples(UVec3::new(res_x, res_y, res_z), voxel_size, origin, samples)
        .ok_or(ReadError::HeaderInconsistent)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_every_voxel() {
        let mut g = Grid::empty(UVec3::new(16, 12, 8), 1.5, Vec3::new(-10.0, 0.0, 3.0));
        // Deterministic per-voxel values so any bit flip is visible.
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

        let mut r = &buf[..];
        let loaded = read_project(&mut r).expect("round-trip should succeed");

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

    #[test]
    fn round_trip_of_a_sphere() {
        // Realistic-shape check — makes sure we don't accidentally
        // scramble sign conventions.
        let g = Grid::from_sphere(
            UVec3::new(32, 32, 32),
            1.0,
            Vec3::ZERO,
            Vec3::new(16.0, 16.0, 16.0),
            8.0,
        );
        let mut buf = Vec::new();
        write_project(&g, &mut buf).unwrap();
        let loaded = read_project(&mut &buf[..]).unwrap();
        for iz in [0, 8, 16, 24, 31] {
            for iy in [0, 8, 16, 24, 31] {
                for ix in [0, 8, 16, 24, 31] {
                    assert_eq!(loaded.get(ix, iy, iz), g.get(ix, iy, iz));
                }
            }
        }
    }

    /// Helper: match `Err(expected)` on a read result without needing
    /// `Grid: Debug` (which we don't want because Grid holds 8 MiB
    /// of floats we don't want to format on a test failure).
    fn expect_err<T>(res: Result<T, ReadError>, want: &str, check: impl Fn(&ReadError) -> bool) {
        match res {
            Ok(_) => panic!("expected error {want}, got Ok"),
            Err(ref e) if check(e) => {}
            Err(e) => panic!("expected error {want}, got {e}"),
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut buf = [b'N'; HEADER_LEN + 4];
        expect_err(read_project(&mut &buf[..]), "BadMagic", |e| {
            matches!(e, ReadError::BadMagic)
        });
        buf[..4].copy_from_slice(b"abcd");
        expect_err(read_project(&mut &buf[..]), "BadMagic", |e| {
            matches!(e, ReadError::BadMagic)
        });
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&99u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; HEADER_LEN - 12]);
        expect_err(read_project(&mut &buf[..]), "UnsupportedVersion(99)", |e| {
            matches!(e, ReadError::UnsupportedVersion(99))
        });
    }

    #[test]
    fn inconsistent_header_is_rejected() {
        // Declare 8×8×8 but sample_count says 999.
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&1.0f32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 12]);
        buf.extend_from_slice(&999u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 999 * 4]);
        expect_err(read_project(&mut &buf[..]), "HeaderInconsistent", |e| {
            matches!(e, ReadError::HeaderInconsistent)
        });
    }

    #[test]
    fn truncated_body_is_rejected() {
        let g = Grid::empty(UVec3::new(4, 4, 4), 1.0, Vec3::ZERO);
        let mut buf = Vec::new();
        write_project(&g, &mut buf).unwrap();
        buf.truncate(buf.len() - 20);
        expect_err(read_project(&mut &buf[..]), "Truncated", |e| {
            matches!(e, ReadError::Truncated)
        });
    }

    #[test]
    fn reserved_field_set_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&0xdeadbeefu32.to_le_bytes());
        buf.extend_from_slice(&[0u8; HEADER_LEN - 16]);
        expect_err(read_project(&mut &buf[..]), "ReservedFieldSet", |e| {
            matches!(e, ReadError::ReservedFieldSet)
        });
    }
}
