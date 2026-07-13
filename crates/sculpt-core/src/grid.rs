use glam::{UVec3, Vec3};

/// Marching-cubes chunk size in voxels. Chunks share a one-voxel halo at
/// their positive boundaries, so a chunk of size N covers `N` cells and
/// samples `N+1` voxels along each axis.
pub const CHUNK_SIZE: u32 = 32;

/// Dense scalar field. Layout is x-major: index = `x + y*res.x + z*res.x*res.y`.
///
/// The field stores signed distance in mm. Sign convention: negative
/// inside the workpiece, positive outside.
#[derive(Clone)]
pub struct Grid {
    data: Vec<f32>,
    res: UVec3,
    voxel_size: f32,
    origin: Vec3,
}

impl Grid {
    /// Create a grid entirely outside the surface (all far-positive).
    pub fn empty(res: UVec3, voxel_size: f32, origin: Vec3) -> Self {
        let count = (res.x * res.y * res.z) as usize;
        Self {
            data: vec![f32::MAX / 4.0; count],
            res,
            voxel_size,
            origin,
        }
    }

    /// Create a grid initialised to the SDF of a solid sphere. Everything
    /// outside the sphere has a *bounded* positive distance (still exact
    /// for gradient purposes near the surface).
    pub fn from_sphere(
        res: UVec3,
        voxel_size: f32,
        origin: Vec3,
        center: Vec3,
        radius: f32,
    ) -> Self {
        let count = (res.x * res.y * res.z) as usize;
        let mut data = Vec::with_capacity(count);
        for iz in 0..res.z {
            for iy in 0..res.y {
                for ix in 0..res.x {
                    let p = origin
                        + Vec3::new(ix as f32, iy as f32, iz as f32) * voxel_size;
                    data.push((p - center).length() - radius);
                }
            }
        }
        Self {
            data,
            res,
            voxel_size,
            origin,
        }
    }

    #[inline]
    pub fn res(&self) -> UVec3 {
        self.res
    }

    #[inline]
    pub fn voxel_size(&self) -> f32 {
        self.voxel_size
    }

    #[inline]
    pub fn origin(&self) -> Vec3 {
        self.origin
    }

    /// Piece-local physical extent of the grid.
    pub fn extent(&self) -> Vec3 {
        Vec3::new(
            self.res.x as f32,
            self.res.y as f32,
            self.res.z as f32,
        ) * self.voxel_size
    }

    /// Piece-local position of voxel `(ix, iy, iz)`.
    #[inline]
    pub fn position(&self, ix: u32, iy: u32, iz: u32) -> Vec3 {
        self.origin
            + Vec3::new(ix as f32, iy as f32, iz as f32) * self.voxel_size
    }

    #[inline]
    pub fn idx(&self, ix: u32, iy: u32, iz: u32) -> usize {
        (ix + iy * self.res.x + iz * self.res.x * self.res.y) as usize
    }

    #[inline]
    pub fn get(&self, ix: u32, iy: u32, iz: u32) -> f32 {
        self.data[self.idx(ix, iy, iz)]
    }

    #[inline]
    pub fn set(&mut self, ix: u32, iy: u32, iz: u32, v: f32) {
        let i = self.idx(ix, iy, iz);
        self.data[i] = v;
    }

    /// Sample without a bounds check; the caller must ensure indices are
    /// in range. Used in the tight brush loop.
    #[inline]
    pub unsafe fn get_unchecked(&self, ix: u32, iy: u32, iz: u32) -> f32 {
        *self
            .data
            .get_unchecked((ix + iy * self.res.x + iz * self.res.x * self.res.y) as usize)
    }

    /// Central-differences gradient. Clamps at the borders so that
    /// out-of-bounds gradient queries do not panic; the result at the
    /// border is one-sided.
    pub fn gradient(&self, ix: u32, iy: u32, iz: u32) -> Vec3 {
        let (xp, xm) = (
            self.get(ix.saturating_add(1).min(self.res.x - 1), iy, iz),
            self.get(ix.saturating_sub(1), iy, iz),
        );
        let (yp, ym) = (
            self.get(ix, iy.saturating_add(1).min(self.res.y - 1), iz),
            self.get(ix, iy.saturating_sub(1), iz),
        );
        let (zp, zm) = (
            self.get(ix, iy, iz.saturating_add(1).min(self.res.z - 1)),
            self.get(ix, iy, iz.saturating_sub(1)),
        );
        Vec3::new(xp - xm, yp - ym, zp - zm) * (0.5 / self.voxel_size)
    }

    pub fn num_chunks(&self) -> UVec3 {
        UVec3::new(
            (self.res.x + CHUNK_SIZE - 1) / CHUNK_SIZE,
            (self.res.y + CHUNK_SIZE - 1) / CHUNK_SIZE,
            (self.res.z + CHUNK_SIZE - 1) / CHUNK_SIZE,
        )
    }
}

/// Chunk coordinates (in units of `CHUNK_SIZE` voxels).
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct ChunkCoord {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

impl ChunkCoord {
    pub fn new(x: u32, y: u32, z: u32) -> Self {
        Self { x, y, z }
    }

    /// Inclusive min voxel index of this chunk.
    pub fn voxel_min(&self) -> UVec3 {
        UVec3::new(self.x, self.y, self.z) * CHUNK_SIZE
    }

    /// Exclusive max voxel index of this chunk (before halo).
    pub fn voxel_max(&self, grid_res: UVec3) -> UVec3 {
        UVec3::new(
            ((self.x + 1) * CHUNK_SIZE).min(grid_res.x),
            ((self.y + 1) * CHUNK_SIZE).min(grid_res.y),
            ((self.z + 1) * CHUNK_SIZE).min(grid_res.z),
        )
    }
}

/// AABB of dirty voxels (indices are inclusive/exclusive: [min, max)).
#[derive(Copy, Clone, Debug)]
pub struct DirtyRegion {
    pub min: UVec3,
    pub max: UVec3,
}

impl DirtyRegion {
    /// Iterate the chunk coordinates that this region intersects,
    /// accounting for the one-voxel halo each chunk needs.
    pub fn touched_chunks(&self, grid_res: UVec3) -> Vec<ChunkCoord> {
        let num = num_chunks(grid_res);
        // A voxel at index v is sampled by chunk c if c*CHUNK_SIZE <= v <= (c+1)*CHUNK_SIZE
        // (inclusive on both ends because of the halo). So the chunk range
        // that touches [min, max) is:
        let cmin = UVec3::new(
            self.min.x.saturating_sub(1) / CHUNK_SIZE,
            self.min.y.saturating_sub(1) / CHUNK_SIZE,
            self.min.z.saturating_sub(1) / CHUNK_SIZE,
        );
        let cmax = UVec3::new(
            (self.max.x / CHUNK_SIZE).min(num.x - 1),
            (self.max.y / CHUNK_SIZE).min(num.y - 1),
            (self.max.z / CHUNK_SIZE).min(num.z - 1),
        );
        let mut out = Vec::new();
        for z in cmin.z..=cmax.z {
            for y in cmin.y..=cmax.y {
                for x in cmin.x..=cmax.x {
                    out.push(ChunkCoord::new(x, y, z));
                }
            }
        }
        out
    }
}

fn num_chunks(res: UVec3) -> UVec3 {
    UVec3::new(
        (res.x + CHUNK_SIZE - 1) / CHUNK_SIZE,
        (res.y + CHUNK_SIZE - 1) / CHUNK_SIZE,
        (res.z + CHUNK_SIZE - 1) / CHUNK_SIZE,
    )
}
