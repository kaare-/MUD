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

    /// Build a grid from an existing `data` buffer. The buffer must
    /// have length `res.x * res.y * res.z`; otherwise this returns
    /// `None`. Layout must match [`Grid::idx`] (x-major).
    ///
    /// Used by the project-file loader — round-tripping the raw f32
    /// samples is the cheapest way to preserve an exact sculpt state
    /// across sessions.
    pub fn from_samples(res: UVec3, voxel_size: f32, origin: Vec3, data: Vec<f32>) -> Option<Self> {
        let expected = (res.x as usize)
            .checked_mul(res.y as usize)?
            .checked_mul(res.z as usize)?;
        if data.len() != expected {
            return None;
        }
        Some(Self {
            data,
            res,
            voxel_size,
            origin,
        })
    }

    /// Raw sample buffer. Read-only. Provided so the project-file
    /// writer can emit the SDF bytes without going voxel-by-voxel.
    pub fn samples(&self) -> &[f32] {
        &self.data
    }

    /// Overwrite every sample in the grid with values from `data`.
    /// Length must match `res.x * res.y * res.z`; the method returns
    /// `false` and leaves the grid untouched if it doesn't. Used by
    /// the Move-tool live preview to reset to a snapshot each frame
    /// before applying a fresh translation.
    pub fn restore_samples(&mut self, data: &[f32]) -> bool {
        if data.len() != self.data.len() {
            return false;
        }
        self.data.copy_from_slice(data);
        true
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

    /// Sample without a bounds check.
    ///
    /// # Safety
    /// The caller must guarantee that `(ix, iy, iz)` is a valid voxel
    /// index inside this grid (i.e. `ix < res.x` and equivalently for
    /// `iy`, `iz`). Passing out-of-range indices is undefined behaviour.
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
            self.res.x.div_ceil(CHUNK_SIZE),
            self.res.y.div_ceil(CHUNK_SIZE),
            self.res.z.div_ceil(CHUNK_SIZE),
        )
    }

    /// Trilinearly interpolated sample at an arbitrary piece-local point.
    /// Points outside the grid AABB return a large positive value
    /// ("empty air"), so ray-marching from outside the grid does not
    /// spuriously report hits.
    pub fn sample(&self, p: Vec3) -> f32 {
        let inv_vs = 1.0 / self.voxel_size;
        let pi = (p - self.origin) * inv_vs;
        let x = pi.x;
        let y = pi.y;
        let z = pi.z;
        let ix0 = x.floor() as i32;
        let iy0 = y.floor() as i32;
        let iz0 = z.floor() as i32;
        let rx = self.res.x as i32;
        let ry = self.res.y as i32;
        let rz = self.res.z as i32;
        if ix0 < 0 || iy0 < 0 || iz0 < 0 || ix0 >= rx - 1 || iy0 >= ry - 1 || iz0 >= rz - 1 {
            return self.voxel_size * 32.0;
        }
        let fx = x - ix0 as f32;
        let fy = y - iy0 as f32;
        let fz = z - iz0 as f32;
        let ix0 = ix0 as u32;
        let iy0 = iy0 as u32;
        let iz0 = iz0 as u32;
        let c000 = self.get(ix0, iy0, iz0);
        let c100 = self.get(ix0 + 1, iy0, iz0);
        let c010 = self.get(ix0, iy0 + 1, iz0);
        let c110 = self.get(ix0 + 1, iy0 + 1, iz0);
        let c001 = self.get(ix0, iy0, iz0 + 1);
        let c101 = self.get(ix0 + 1, iy0, iz0 + 1);
        let c011 = self.get(ix0, iy0 + 1, iz0 + 1);
        let c111 = self.get(ix0 + 1, iy0 + 1, iz0 + 1);
        let c00 = c000 * (1.0 - fx) + c100 * fx;
        let c10 = c010 * (1.0 - fx) + c110 * fx;
        let c01 = c001 * (1.0 - fx) + c101 * fx;
        let c11 = c011 * (1.0 - fx) + c111 * fx;
        let c0 = c00 * (1.0 - fy) + c10 * fy;
        let c1 = c01 * (1.0 - fy) + c11 * fy;
        c0 * (1.0 - fz) + c1 * fz
    }

    /// Central-differences gradient at an arbitrary piece-local point,
    /// computed from four `sample()` calls per axis. The result is not
    /// normalised; callers who want a surface normal should normalise
    /// it themselves.
    pub fn gradient_at(&self, p: Vec3) -> Vec3 {
        let h = self.voxel_size;
        let dx = self.sample(p + Vec3::new(h, 0.0, 0.0)) - self.sample(p - Vec3::new(h, 0.0, 0.0));
        let dy = self.sample(p + Vec3::new(0.0, h, 0.0)) - self.sample(p - Vec3::new(0.0, h, 0.0));
        let dz = self.sample(p + Vec3::new(0.0, 0.0, h)) - self.sample(p - Vec3::new(0.0, 0.0, h));
        Vec3::new(dx, dy, dz) * (0.5 / h)
    }

    /// Conservative ray march against the SDF. `origin` / `dir` are
    /// piece-local; `dir` should be unit length. Returns the nearest
    /// hit along the ray (camera-facing surface) or `None`.
    ///
    /// After brush CSG the field is no longer a true Euclidean
    /// distance — `min` / `max` / soft-min routinely *overestimate*
    /// outside carved or joined regions. Classic sphere tracing
    /// (`t += φ(p)`) then skips the near face and latches onto a deeper
    /// isosurface (often the original starter sphere). We therefore
    /// never step more than one voxel, and we detect a zero-crossing
    /// between consecutive samples so thin fronts can't be jumped.
    pub fn ray_march(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Vec3> {
        let min_step = self.voxel_size * 0.25;
        let max_step = self.voxel_size;
        let hit_eps = self.voxel_size * 0.15;

        let mut t = 0.0;
        let mut prev_d = self.sample(origin);
        if prev_d < hit_eps {
            // Started inside (or on) the surface — treat origin as hit.
            return Some(origin);
        }

        // Enough iterations to walk a full Stage-2 domain (~192 mm) at
        // one voxel per step, with headroom for grazes.
        for _ in 0..1024 {
            let step = prev_d.max(min_step).min(max_step);
            let t_next = t + step;
            if t_next > max_dist {
                return None;
            }
            let d = self.sample(origin + dir * t_next);

            // Latched inside, or crossed from outside → inside between
            // the previous and current samples.
            if d < hit_eps || (prev_d > 0.0 && d <= 0.0) {
                let mut t_out = t;
                let mut t_in = t_next;
                // Ensure the outside bracket is actually outside.
                if self.sample(origin + dir * t_out) <= 0.0 {
                    t_out = (t - max_step).max(0.0);
                }
                for _ in 0..10 {
                    let tm = 0.5 * (t_out + t_in);
                    if self.sample(origin + dir * tm) > 0.0 {
                        t_out = tm;
                    } else {
                        t_in = tm;
                    }
                }
                return Some(origin + dir * t_in);
            }

            t = t_next;
            prev_d = d;
        }
        None
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
        res.x.div_ceil(CHUNK_SIZE),
        res.y.div_ceil(CHUNK_SIZE),
        res.z.div_ceil(CHUNK_SIZE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn sphere_grid_is_zero_at_surface() {
        let res = UVec3::new(32, 32, 32);
        let g = Grid::from_sphere(res, 1.0, Vec3::ZERO, Vec3::new(16.0, 16.0, 16.0), 8.0);
        // A voxel on the +x axis exactly 8 mm from the centre should
        // report distance ~= 0.
        let p_surface = Vec3::new(24.0, 16.0, 16.0);
        assert!(approx_eq(g.sample(p_surface), 0.0, 0.05));
    }

    #[test]
    fn sphere_grid_is_negative_inside_positive_outside() {
        let res = UVec3::new(32, 32, 32);
        let g = Grid::from_sphere(res, 1.0, Vec3::ZERO, Vec3::new(16.0, 16.0, 16.0), 8.0);
        assert!(g.sample(Vec3::new(16.0, 16.0, 16.0)) < 0.0);
        assert!(g.sample(Vec3::new(30.0, 16.0, 16.0)) > 0.0);
    }

    #[test]
    fn ray_march_hits_sphere() {
        let res = UVec3::new(64, 64, 64);
        let g = Grid::from_sphere(res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 10.0);
        let hit = g
            .ray_march(Vec3::new(-10.0, 32.0, 32.0), Vec3::X, 200.0)
            .expect("ray should hit the sphere");
        // Expected first hit: x ~= 22 (32 - 10). Tolerance is generous
        // because trilinear sampling smooths the surface a little.
        assert!(
            (hit.x - 22.0).abs() < 1.5,
            "unexpected hit x = {}",
            hit.x
        );
        assert!(hit.y > 30.0 && hit.y < 34.0);
        assert!(hit.z > 30.0 && hit.z < 34.0);
    }

    #[test]
    fn ray_march_hits_near_face_when_field_overestimates() {
        // Regression for the ghost sticking to the starter sphere: after
        // CSG the SDF often *overestimates* distance. Naïve sphere
        // tracing (`t += φ`) then skips a near slab and latches onto a
        // deeper body. Plant a solid near wall + a deep sphere, and
        // inflate the air gap between them so φ lies about the distance.
        let res = UVec3::new(64, 64, 64);
        let vs = 1.0;
        let mut g = Grid::empty(res, vs, Vec3::ZERO);
        let deep_centre = Vec3::new(48.0, 32.0, 32.0);
        let deep_r = 8.0;
        let near_x = 16.0;
        let near_half = 2.0;

        for iz in 0..res.z {
            for iy in 0..res.y {
                for ix in 0..res.x {
                    let p = g.position(ix, iy, iz);
                    let deep = (p - deep_centre).length() - deep_r;
                    // Thin slab around x = near_x.
                    let near = (p.x - near_x).abs() - near_half;
                    let mut d = deep.min(near);
                    // Lie: between the slab and the sphere, claim we are
                    // much farther from any surface than we really are.
                    if p.x > near_x + near_half + 1.0 && p.x < deep_centre.x - deep_r - 1.0 {
                        d = d.max(20.0);
                    }
                    g.set(ix, iy, iz, d);
                }
            }
        }

        let hit = g
            .ray_march(Vec3::new(-5.0, 32.0, 32.0), Vec3::X, 200.0)
            .expect("ray should hitch the near slab");
        assert!(
            (hit.x - (near_x - near_half)).abs() < 1.5,
            "should hit the camera-facing near slab (~x={}), got {hit:?}",
            near_x - near_half,
        );
        // Must NOT have tunnelled through to the deep sphere (~x=40).
        assert!(
            hit.x < 25.0,
            "ray tunnelled past the near face onto the deep body: {hit:?}"
        );
    }

    #[test]
    fn ray_march_misses_when_no_intersection() {
        let res = UVec3::new(64, 64, 64);
        let g = Grid::from_sphere(res, 1.0, Vec3::ZERO, Vec3::new(32.0, 32.0, 32.0), 5.0);
        // Aim way off to one side.
        let hit = g.ray_march(Vec3::new(-10.0, 60.0, 32.0), Vec3::X, 200.0);
        assert!(hit.is_none());
    }
}
