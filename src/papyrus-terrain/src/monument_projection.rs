//! Read-only, lazy single-stamp projection of native height and surface cells.
//! Ocean connectivity can be rebuilt without copying terrain. Local-water geometry
//! remains an explicit separate input; it is never inferred from a stamp texture.
use super::*;

pub struct Terrain<'a> {
    layers: &'a Layers,
    alpha: &'a [u8],
    heights: HeightTerrain<'a>,
}
pub struct Projected<'a, 'b, 'c> {
    terrain: &'a Terrain<'a>,
    stamp: &'b Stamp<'c>,
    height: ProjectedHeight<'a, 'b, 'c>,
    transform: Transform,
    bounds: [usize; 4],
    quad: Option<crate::source_stamp_raster::Quad>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub splat: [u8; 8],
    pub biome: [u8; 5],
    pub alpha: u8,
    pub topology: i32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub height: f32,
    pub splat: [f32; 8],
    pub biome: [f32; 5],
    /// Discrete cells match the overview renderer; holes are not interpolated.
    pub alpha: u8,
    pub topology: i32,
}
impl<'a> Terrain<'a> {
    /// Validate once on a worker. No terrain arrays are copied or owned.
    pub fn new(
        layers: &'a Layers,
        alpha: &'a [u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let (hr, sr) = dimensions(layers.size)?;
        let cells = sr * sr;
        if layers.height_resolution != hr
            || layers.surface_resolution != sr
            || layers.height.len() != hr * hr
            || layers.splat.len() != cells * 8
            || layers.biome.len() != cells * 5
            || layers.topology.len() != cells
            || alpha.len() != cells
        {
            return Err("Invalid stamp projection terrain dimensions".into());
        }
        for chunk in layers.height.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid stamp projection terrain heights".into());
            }
        }
        Ok((!cancelled()).then_some(Self {
            layers,
            alpha,
            heights: HeightTerrain::from_validated(&layers.height, hr, layers.size),
        }))
    }
    pub fn projected<'b, 'c>(
        &'a self,
        stamp: &'b Stamp<'c>,
    ) -> Result<Projected<'a, 'b, 'c>, String> {
        let height = self.heights.projected(stamp)?;
        let transform = Transform::new(stamp);
        let quad = stamp
            .source_quad_raster
            .then(|| {
                transform.quad(
                    self.layers.size,
                    self.layers.surface_resolution,
                    [stamp.radius; 2],
                )
            })
            .transpose()?;
        let bounds = quad.as_ref().map_or_else(
            || {
                transform.bounds(
                    self.layers.size as f64,
                    self.layers.surface_resolution,
                    [stamp.radius; 2],
                )
            },
            crate::source_stamp_raster::Quad::bounds,
        );
        Ok(Projected {
            terrain: self,
            stamp,
            height,
            transform,
            bounds,
            quad,
        })
    }
}
impl Projected<'_, '_, '_> {
    pub fn size(&self) -> u32 {
        self.terrain.layers.size
    }
    pub fn height_resolution(&self) -> usize {
        self.terrain.layers.height_resolution
    }
    pub fn surface_resolution(&self) -> usize {
        self.terrain.layers.surface_resolution
    }
    /// Conservative world-aligned patch, including rectangular height blends,
    /// radial paint and the neighboring height samples used for terrain lighting.
    /// Returns [minimum X, minimum Z, maximum X, maximum Z], clipped to the map.
    pub fn world_bounds(&self) -> Option<[f32; 4]> {
        let s = self.stamp;
        let half = if s.height.is_some() && s.blend.is_some() {
            [
                (s.size[0] * 0.5).max(s.radius),
                (s.size[2] * 0.5).max(s.radius),
            ]
        } else {
            [s.radius; 2]
        };
        let t = &self.transform;
        let padding = 3. * f64::from(self.size()) / (self.height_resolution() - 1) as f64;
        let extent = [
            t.cos.abs() * half[0] + t.sin.abs() * half[1] + padding,
            t.sin.abs() * half[0] + t.cos.abs() * half[1] + padding,
        ];
        let limit = f64::from(self.size()) * 0.5;
        let b = [
            (t.center[0] - extent[0]).max(-limit),
            (t.center[1] - extent[1]).max(-limit),
            (t.center[0] + extent[0]).min(limit),
            (t.center[1] + extent[1]).min(limit),
        ];
        (b[0] < b[2] && b[1] < b[3]).then(|| b.map(|v| v as f32))
    }
    pub fn ocean(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<crate::ocean::Mask>, String> {
        let n = self.surface_resolution();
        let hr = self.height_resolution();
        crate::ocean::classify_sampled(
            n,
            |x, z| {
                Ok(self
                    .height
                    .cell(x * (hr - 1) / (n - 1), z * (hr - 1) / (n - 1))
                    <= 16383)
            },
            cancelled,
        )
    }
    /// Match the overview's whole-image palette eligibility after projected paint.
    pub fn supports_biomes(&self, count: usize, cancelled: impl Fn() -> bool) -> Option<bool> {
        if !(1..=5).contains(&count) {
            return Some(false);
        }
        let layers = self.terrain.layers;
        let n = self.surface_resolution();
        let cells = n * n;
        for channel in count..5 {
            for z in 0..n {
                if cancelled() {
                    return None;
                }
                for x in 0..n {
                    let value = if self.stamp.biome.is_some()
                        && x >= self.bounds[0]
                        && x < self.bounds[1]
                        && z >= self.bounds[2]
                        && z < self.bounds[3]
                    {
                        self.cell(x, z)?.biome[channel]
                    } else {
                        layers.biome[channel * cells + z * n + x]
                    };
                    if value != 0 {
                        return Some(false);
                    }
                }
            }
        }
        (!cancelled()).then_some(true)
    }
    pub fn height(&self, u: f32, v: f32) -> Option<f32> {
        if !u.is_finite() || !v.is_finite() || !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v)
        {
            return None;
        }
        // Match the committed overview: interpolate encoded vertices before
        // decoding. Decoding first can flip an exact freshwater/ground tie.
        // Height-only placement projections retain their versioned sampler.
        let n = self.height_resolution();
        let x = u * (n - 1) as f32;
        let z = v * (n - 1) as f32;
        let (a, b) = (x.floor() as usize, z.floor() as usize);
        let (c, d) = ((a + 1).min(n - 1), (b + 1).min(n - 1));
        let tx = x - a as f32;
        let tz = z - b as f32;
        let vertices = [(a, b), (c, b), (a, d), (c, d)];
        let weights = [
            (1. - tx) * (1. - tz),
            tx * (1. - tz),
            (1. - tx) * tz,
            tx * tz,
        ];
        Some(
            vertices
                .into_iter()
                .zip(weights)
                .map(|((x, z), w)| self.height.cell(x, z) as f32 * w)
                .sum::<f32>()
                / 32766.
                * 1000.
                - 500.,
        )
    }
    /// Exact committed height vertex after this stamp's transform/blend/rounding.
    pub fn encoded_height(&self, x: usize, z: usize) -> Option<i16> {
        let n = self.height_resolution();
        (x < n && z < n).then(|| self.height.cell(x, z))
    }
    pub fn source_height(
        &self,
        position: [f32; 3],
        origin_y: f32,
        vertical_span: f32,
    ) -> Result<f32, String> {
        self.height.sample_source(position, origin_y, vertical_span)
    }
    /// Identity of the immutable baseline used by this projected stamp.
    pub fn uses_layers(&self, layers: &Layers) -> bool {
        std::ptr::eq(self.terrain.layers, layers)
    }
    /// Immutable baseline for worker-side derived terrain fields.
    pub fn source_layers(&self) -> &Layers {
        self.terrain.layers
    }
    pub fn source_height_with_support(
        &self,
        position: [f32; 3],
        origin_y: f32,
        vertical_span: f32,
    ) -> Result<(f32, [usize; 4]), String> {
        self.height
            .sample_source_with_support(position, origin_y, vertical_span)
    }
    fn slope(&self, x: usize, z: usize) -> f64 {
        let layers = self.terrain.layers;
        let sr = layers.surface_resolution;
        let hr = layers.height_resolution;
        let hx = x * (hr - 1) / (sr - 1);
        let hz = z * (hr - 1) / (sr - 1);
        let left = hx.saturating_sub(1);
        let right = (hx + 1).min(hr - 1);
        let down = hz.saturating_sub(1);
        let up = (hz + 1).min(hr - 1);
        let at = |x, z| f64::from(self.height.cell(x, z)) / 32766. * 1000.;
        let dx = (at(right, hz) - at(left, hz))
            / ((right - left) as f64 * layers.size as f64 / hr as f64);
        let dz =
            (at(hx, up) - at(hx, down)) / ((up - down) as f64 * layers.size as f64 / hr as f64);
        dx.hypot(dz).atan().to_degrees()
    }
    fn contains_surface_cell(&self, x: usize, z: usize) -> bool {
        let [x0, x1, z0, z1] = self.bounds;
        x >= x0 && x < x1 && z >= z0 && z < z1
            && self.quad.is_none_or(|q| q.contains(x, z))
    }
    fn local_surface(&self, x: usize, z: usize) -> [f64; 2] {
        let n = self.surface_resolution() as f64;
        let size = f64::from(self.size());
        self.transform.local(
            ((x as f64 + 0.5) / n - 0.5) * size,
            ((z as f64 + 0.5) / n - 0.5) * size,
        )
    }
    fn source_rock(&self, x: usize, z: usize) -> u8 {
        let s = self.stamp;
        let layers = self.terrain.layers;
        let n = layers.surface_resolution;
        let cells = n * n;
        let i = z * n + x;
        let base_rock = layers.splat[3 * cells + i];
        if s.splat.is_none() || s.splat_mask == 0 || !self.contains_surface_cell(x, z) {
            return base_rock;
        }
        let local = self.local_surface(x, z);
        if !s.source_quad_raster && local.iter().any(|v| v.abs() > s.radius + 1e-9) {
            return base_rock;
        }
        let uv = [local[0] / s.size[0] + 0.5, 0.5 - local[1] / s.size[2]];
        let splat = std::array::from_fn(|c| layers.splat[c * cells + i]);
        source_splat_cell(s, splat, layers.topology[i], uv, radial(s, local), || {
            self.source_slope(x, z)
        })[3]
    }
    fn source_topology(&self, x: usize, z: usize, local: [f64; 2], topology: i32) -> i32 {
        source_topology_cell(
            self.stamp,
            topology,
            local,
            || self.source_slope(x, z),
            || source_rock_sample(self.surface_resolution(), x, z, |sx, sz| {
                self.source_rock(sx, sz)
            }),
        )
    }
    /// Project topology without sampling alpha or biome. Source cliff rules
    /// read projected splat only when the slope alone does not decide the flag.
    pub fn topology(&self, x: usize, z: usize) -> Option<i32> {
        if !self.stamp.source_cliff_generation {
            return self.cell(x, z).map(|cell| cell.topology);
        }
        let n = self.surface_resolution();
        if x >= n || z >= n {
            return None;
        }
        let topology = self.terrain.layers.topology[z * n + x];
        if self.stamp.topology.is_none()
            || self.stamp.topology_mask == 0
            || !self.contains_surface_cell(x, z)
        {
            return Some(topology);
        }
        Some(self.source_topology(x, z, self.local_surface(x, z), topology))
    }
    fn source_slope(&self, x: usize, z: usize) -> f32 {
        let layers = self.terrain.layers;
        source_slope(
            layers.size,
            layers.height_resolution,
            layers.surface_resolution,
            x,
            z,
            |hx, hz| Some(self.height.cell(hx, hz)),
        )
    }
    pub fn cell(&self, x: usize, z: usize) -> Option<Cell> {
        let layers = self.terrain.layers;
        let n = layers.surface_resolution;
        let cells = n * n;
        if x >= n || z >= n {
            return None;
        }
        let i = z * n + x;
        let mut out = Cell {
            splat: std::array::from_fn(|c| layers.splat[c * cells + i]),
            biome: std::array::from_fn(|c| layers.biome[c * cells + i]),
            alpha: self.terrain.alpha[i],
            topology: layers.topology[i],
        };
        if !self.contains_surface_cell(x, z) {
            return Some(out);
        }
        let s = self.stamp;
        let local = self.local_surface(x, z);
        if s.source_cliff_generation {
            out = source_material_cell(s, out, local, || self.source_slope(x, z));
            out.topology = self.source_topology(x, z, local, out.topology);
            return Some(out);
        }
        if local.iter().any(|v| v.abs() > s.radius + 1e-9) {
            return Some(out);
        }
        let u = local[0] / s.size[0] + 0.5;
        let v = 0.5 - local[1] / s.size[2];
        let opacity = radial(s, local);
        let cliff =
            if (s.auto_cliff_splat || s.auto_cliff_topology) && out.topology & 8_389_632 == 0 {
                Some(self.slope(x, z))
            } else {
                None
            };
        if let Some(slope) = cliff.filter(|_| s.auto_cliff_splat) {
            let mut rock = [0.; 8];
            rock[3] = 1.;
            out.splat = mixed_weights_for(
                out.splat,
                rock,
                ((slope - 30.) / 20.).clamp(0., 1.),
                s.source_texture_sampling,
            );
        }
        if opacity > 0. {
            if let Some(splat) = s.splat {
                let values = splat_channels(splat, u, v, s.splat_mask, s.source_texture_sampling);
                out.splat =
                    mixed_weights_for(out.splat, values, opacity, s.source_texture_sampling);
            }
            if let Some(alpha) = s.alpha {
                out.alpha = alpha_for(
                    out.alpha,
                    alpha.channel_for(u, v, 3, s.source_texture_sampling),
                    opacity,
                    s.source_texture_sampling,
                );
            }
            if let Some(biome) = s.biome {
                let rgba = biome.rgba_for(u, v, 15, s.source_texture_sampling);
                let fifth = fifth_for(rgba, s.source_texture_sampling);
                let values = std::array::from_fn(|c| {
                    if s.biome_mask & (1 << c) == 0 {
                        0.
                    } else if c < 4 {
                        rgba[c]
                    } else {
                        fifth
                    }
                });
                out.biome =
                    mixed_weights_for(out.biome, values, opacity, s.source_texture_sampling);
            }
        }
        if let Some(slope) = cliff.filter(|_| s.auto_cliff_topology) {
            if slope > 30. || f64::from(out.splat[3]) / 255. > 0.4 {
                out.topology |= 2;
            } else {
                out.topology &= !2;
            }
        }
        if let Some(topology) = s.topology {
            let bits = topology.topology_for(u, v, s.source_texture_sampling) & s.topology_mask;
            if bits != 0 {
                if s.replace_topology {
                    out.topology &= !s.topology_mask;
                }
                out.topology |= bits;
            }
        }
        Some(out)
    }
    /// Continuous height/materials, with discrete alpha/topology from the native cell.
    pub fn sample(&self, u: f32, v: f32) -> Option<Sample> {
        let height = self.height(u, v)?;
        let n = self.terrain.layers.surface_resolution;
        let x = (u * n as f32 - 0.5).clamp(0., (n - 1) as f32);
        let z = (v * n as f32 - 0.5).clamp(0., (n - 1) as f32);
        let a = x.floor() as usize;
        let b = z.floor() as usize;
        let cells = [
            self.cell(a, b)?,
            self.cell((a + 1).min(n - 1), b)?,
            self.cell(a, (b + 1).min(n - 1))?,
            self.cell((a + 1).min(n - 1), (b + 1).min(n - 1))?,
        ];
        let weights = [
            (1. - x.fract()) * (1. - z.fract()),
            x.fract() * (1. - z.fract()),
            (1. - x.fract()) * z.fract(),
            x.fract() * z.fract(),
        ];
        let at = |c: usize, biome: bool| {
            cells
                .iter()
                .zip(weights)
                .map(|(p, w)| f32::from(if biome { p.biome[c] } else { p.splat[c] }) * w / 255.)
                .sum()
        };
        let nearest = self.cell(
            ((u * n as f32).floor() as usize).min(n - 1),
            ((v * n as f32).floor() as usize).min(n - 1),
        )?;
        Some(Sample {
            height,
            splat: std::array::from_fn(|c| at(c, false)),
            biome: std::array::from_fn(|c| at(c, true)),
            alpha: nearest.alpha,
            topology: nearest.topology,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{base, stamp};
    use super::*;
    #[test]
    fn source_revision_keeps_streamed_committed_and_rotated_projections_identical() {
        fn tile(bytes: &[u8]) -> Image<'_> {
            Image {
                width: 5,
                height: 5,
                rgba: bytes,
            }
        }
        let mut base = base();
        let hr = base.height_resolution;
        let sr = base.surface_resolution;
        let cells = sr * sr;
        let alpha = vec![255; cells];
        for z in 0..hr {
            for x in 0..hr {
                base.height[z * hr + x] = (x as f32 / (hr - 1) as f32 * 32766.).round() as i16;
            }
        }
        let original = base.height.clone();
        let mut heights = vec![0_u8; 100];
        let mut blend = vec![0_u8; 100];
        let mut holes = vec![0_u8; 100];
        let mut topology = vec![0_u8; 100];
        for z in 0..5 {
            for x in 0..5 {
                let i = (z * 5 + x) * 4;
                let encoded = (16383 + x * 13 + z * 27) as i16;
                let [lo, hi] = encoded.to_le_bytes();
                heights[i..i + 4].copy_from_slice(&[lo, 0, hi, 0]);
                blend[i + 3] = (64 + x * 24 + z * 16) as u8;
                holes[i + 3] = if x == 1 && (1..=2).contains(&z) {
                    0
                } else {
                    255
                };
                topology[i..i + 4].copy_from_slice(&(1_i32 << ((x + z) % 8)).to_le_bytes());
            }
        }
        let splat = [64_u8, 64, 0, 0].repeat(25);
        let biome = [64_u8; 4].repeat(25);
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        for (cliffs, raster, height_blend) in [
            (false, false, false),
            (true, false, false),
            (true, true, false),
            (true, true, true),
        ] {
            for yaw in [0., 37., 123., 271.] {
                let mut s = stamp();
                s.source_texture_sampling = true;
                s.source_cliff_generation = cliffs;
                s.source_quad_raster = raster;
                s.source_height_blending = height_blend;
                s.yaw_degrees = yaw;
                s.height = Some(tile(&heights));
                s.blend = Some(tile(&blend));
                s.splat = Some([tile(&splat); 2]);
                s.splat_mask = 255;
                s.biome = Some(tile(&biome));
                s.biome_mask = 31;
                s.alpha = Some(tile(&holes));
                s.topology = Some(tile(&topology));
                s.topology_mask = -1;
                s.auto_cliff_splat = cliffs;
                s.auto_cliff_topology = cliffs;
                let committed = compose(&base, &alpha, std::slice::from_ref(&s), || false)
                    .unwrap()
                    .unwrap();
                let streamed = compose_streamed(&base, &alpha, || false, |apply| apply(&s))
                    .unwrap()
                    .unwrap();
                assert_eq!(streamed.layers.height, committed.layers.height);
                assert_eq!(streamed.layers.splat, committed.layers.splat);
                assert_eq!(streamed.layers.biome, committed.layers.biome);
                assert_eq!(streamed.layers.topology, committed.layers.topology);
                assert_eq!(streamed.alpha, committed.alpha);
                let projected = terrain.projected(&s).unwrap();
                assert_eq!(projected.topology(sr, 0), None);
                assert_eq!(projected.topology(0, sr), None);
                let mut raw_sum = false;
                let mut hole = false;
                for z in 0..sr {
                    for x in 0..sr {
                        let i = z * sr + x;
                        let p = projected.cell(x, z).unwrap();
                        assert_eq!(
                            p.splat,
                            std::array::from_fn(|c| committed.layers.splat[c * cells + i])
                        );
                        assert_eq!(
                            p.biome,
                            std::array::from_fn(|c| committed.layers.biome[c * cells + i])
                        );
                        assert_eq!(p.topology, committed.layers.topology[i]);
                        assert_eq!(projected.topology(x, z), Some(p.topology));
                        assert_eq!(p.alpha, committed.alpha[i]);
                        raw_sum |= p.splat.iter().map(|&v| u32::from(v)).sum::<u32>() != 255;
                        hole |= p.alpha == 0;
                    }
                }
                assert!(raw_sum, "Source writes were silently normalized");
                assert!(hole, "Rotated alpha holes disappeared");
                let bounds = projected.height.bounds;
                for z in bounds[2]..bounds[3] {
                    for x in bounds[0]..bounds[1] {
                        assert_eq!(
                            projected.encoded_height(x, z).unwrap(),
                            committed.layers.height[z * hr + x]
                        );
                    }
                }
                if yaw == 37. {
                    let ocean = projected.ocean(|| false).unwrap().unwrap();
                    let water =
                        compose_with_water(&base, &alpha, std::slice::from_ref(&s), || false)
                            .unwrap()
                            .unwrap();
                    assert_eq!(ocean.classification, water.ocean.unwrap().classification);
                }
            }
        }
        assert_eq!(base.height, original);
    }
    #[test]
    fn projected_ocean_matches_committed_when_rotated_earthwork_opens_or_blocks_channels() {
        let mut base = base();
        let hr = base.height_resolution;
        let sr = base.surface_resolution;
        let alpha = vec![255; sr * sr];
        for z in 0..hr {
            for x in 0..hr {
                let wet = x < hr / 3
                    || (z.abs_diff(hr / 2) < hr / 32 && x < hr * 2 / 3)
                    || (x.abs_diff(hr * 2 / 3) < hr / 12 && z.abs_diff(hr / 2) < hr / 12);
                base.height[z * hr + x] = crate::erosion::encode(if wet { -20. } else { 30. });
            }
        }
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        let initial = crate::ocean::reconstruct(&base.height, hr, &base.topology, sr, || false)
            .unwrap()
            .unwrap();
        let mut changed = 0;
        for y in [-30., 40.] {
            for yaw in [0., 37., 123., 271.] {
                let mut stamp = stamp();
                stamp.position = [-120., y, 0.];
                stamp.offset = [0.; 3];
                stamp.size = [160., 600., 240.];
                stamp.height = Some(Image {
                    width: 1,
                    height: 1,
                    rgba: &[0; 4],
                });
                stamp.yaw_degrees = yaw;
                let projected = terrain.projected(&stamp).unwrap();
                let mask = projected.ocean(|| false).unwrap().unwrap();
                let committed =
                    compose_with_water(&base, &alpha, std::slice::from_ref(&stamp), || false)
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    mask.classification,
                    committed.ocean.unwrap().classification,
                    "sea mask y={y} yaw={yaw}"
                );
                changed += usize::from(mask.classification != initial.classification);
            }
        }
        assert!(
            changed >= 4,
            "The fixture must change actual sea connectivity"
        );
        let stamp = stamp();
        assert!(
            terrain
                .projected(&stamp)
                .unwrap()
                .ocean(|| true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn patch_bounds_include_height_blend_beyond_paint_and_clip_to_the_world() {
        let base = base();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        let mut stamp = stamp();
        stamp.radius = 30.;
        stamp.fade = 10.;
        stamp.size = [600., 600., 180.];
        stamp.offset[0] = 17.;
        stamp.offset[2] = -23.;
        let projected = terrain.projected(&stamp).unwrap();
        let bounds = projected.world_bounds().unwrap();
        let t = Transform::new(&stamp);
        for x in [-300., 300.] {
            for z in [-90., 90.] {
                let world = [
                    t.center[0] + t.cos * x - t.sin * z,
                    t.center[1] + t.sin * x + t.cos * z,
                ];
                assert!(world[0] >= f64::from(bounds[0]) && world[0] <= f64::from(bounds[2]));
                assert!(world[1] >= f64::from(bounds[1]) && world[1] <= f64::from(bounds[3]));
            }
        }
        stamp.position = [490., 20., 490.];
        let b = terrain.projected(&stamp).unwrap().world_bounds().unwrap();
        assert_eq!(b[2], 500.);
        assert_eq!(b[3], 500.);
        stamp.position = [65000., 20., 65000.];
        assert!(terrain.projected(&stamp).unwrap().world_bounds().is_none());
    }
    #[test]
    fn palette_coverage_tracks_biome_paint_instead_of_only_the_original_terrain() {
        let mut base = base();
        let sr = base.surface_resolution;
        let cells = sr * sr;
        let alpha = vec![255; cells];
        base.biome.fill(0);
        base.biome[4 * cells..].fill(255);
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        let mut stamp = stamp();
        stamp.radius = 2000.;
        stamp.fade = 0.;
        stamp.biome_mask = 2;
        assert_eq!(
            terrain
                .projected(&stamp)
                .unwrap()
                .supports_biomes(4, || false),
            Some(true)
        );
        stamp.radius = 100.;
        stamp.fade = 20.;
        assert_eq!(
            terrain
                .projected(&stamp)
                .unwrap()
                .supports_biomes(4, || false),
            Some(false)
        );
        assert_eq!(
            terrain
                .projected(&stamp)
                .unwrap()
                .supports_biomes(4, || true),
            None
        );
        assert_eq!(
            terrain
                .projected(&stamp)
                .unwrap()
                .supports_biomes(0, || false),
            Some(false)
        );
    }

    #[test]
    fn projected_native_cells_match_full_composition_for_rotation_masks_holes_and_cliffs() {
        let mut base = base();
        let sr = base.surface_resolution;
        let cells = sr * sr;
        let hr = base.height_resolution;
        // A slope crossing the stamp exercises cliff paint from projected heights.
        for z in 0..hr {
            for x in 0..hr {
                base.height[z * hr + x] =
                    crate::erosion::encode((x as f32 / hr as f32 * 1000. - 500.) * 0.7);
            }
        }
        // Untouched source river material weights need not sum to 255.
        base.splat[..cells].fill(3);
        let alpha = vec![255; cells];
        let before_height = base.height.clone();
        let before_splat = base.splat.clone();
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        const BLEND: [u8; 16] = [0, 0, 0, 255, 0, 0, 0, 64, 0, 0, 0, 128, 0, 0, 0, 0];
        for (yaw, blend, origin) in [
            (0., true, [12., -19.]),
            (37., true, [12., -19.]),
            (123., false, [12., -19.]),
            (271., false, [12., -19.]),
            (37., true, [470., 470.]),
            (123., false, [-470., -470.]),
        ] {
            for cliffs in [false, true] {
                let mut stamp = stamp();
                stamp.yaw_degrees = yaw;
                stamp.position[0] = origin[0];
                stamp.position[2] = origin[1];
                stamp.offset[0] = 13.;
                stamp.offset[2] = -17.;
                stamp.auto_cliff_splat = cliffs;
                stamp.auto_cliff_topology = cliffs;
                stamp.replace_topology = true;
                stamp.blend = blend.then_some(Image {
                    width: 2,
                    height: 2,
                    rgba: &BLEND,
                });
                let projected = terrain.projected(&stamp).unwrap();
                let committed = compose(&base, &alpha, std::slice::from_ref(&stamp), || false)
                    .unwrap()
                    .unwrap();
                let b = projected.bounds;
                let mut changed = 0;
                for z in b[2].saturating_sub(2)..(b[3] + 2).min(sr) {
                    for x in b[0].saturating_sub(2)..(b[1] + 2).min(sr) {
                        let i = z * sr + x;
                        let cell = projected.cell(x, z).unwrap();
                        assert_eq!(
                            cell.splat,
                            std::array::from_fn(|c| committed.layers.splat[c * cells + i]),
                            "splat yaw={yaw} blend={blend} cliffs={cliffs} {x},{z}"
                        );
                        assert_eq!(
                            cell.biome,
                            std::array::from_fn(|c| committed.layers.biome[c * cells + i])
                        );
                        assert_eq!(cell.alpha, committed.alpha[i]);
                        assert_eq!(cell.topology, committed.layers.topology[i]);
                        changed += usize::from(cell.alpha != alpha[i]);
                    }
                }
                assert!(changed > 20, "The fixture must exercise real holes");
                for z in (0..sr).step_by(31) {
                    for x in (0..sr).step_by(29) {
                        let i = z * sr + x;
                        let cell = projected.cell(x, z).unwrap();
                        assert_eq!(
                            cell.splat,
                            std::array::from_fn(|c| committed.layers.splat[c * cells + i])
                        );
                    }
                }
            }
        }
        assert_eq!(base.height, before_height);
        assert_eq!(base.splat, before_splat);
    }

    #[test]
    fn interpolated_weights_and_discrete_holes_match_committed_cells() {
        let base = base();
        let sr = base.surface_resolution;
        let cells = sr * sr;
        let alpha = vec![255; cells];
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        let mut stamp = stamp();
        stamp.yaw_degrees = 123.;
        let projection = terrain.projected(&stamp).unwrap();
        let committed = compose(&base, &alpha, std::slice::from_ref(&stamp), || false)
            .unwrap()
            .unwrap();
        let (mut holes, mut partial) = (0, 0);
        for z in 0..51 {
            for x in 0..53 {
                let u = 0.35 + x as f32 / 53. * 0.3;
                let v = 0.35 + z as f32 / 51. * 0.3;
                let sample = projection.sample(u, v).unwrap();
                let px = (u * sr as f32 - 0.5).clamp(0., (sr - 1) as f32);
                let pz = (v * sr as f32 - 0.5).clamp(0., (sr - 1) as f32);
                let (a, b) = (px.floor() as usize, pz.floor() as usize);
                let indices = [
                    b * sr + a,
                    b * sr + (a + 1).min(sr - 1),
                    (b + 1).min(sr - 1) * sr + a,
                    (b + 1).min(sr - 1) * sr + (a + 1).min(sr - 1),
                ];
                let weights = [
                    (1. - px.fract()) * (1. - pz.fract()),
                    px.fract() * (1. - pz.fract()),
                    (1. - px.fract()) * pz.fract(),
                    px.fract() * pz.fract(),
                ];
                let expected = |c: usize, data: &[u8]| {
                    indices
                        .iter()
                        .zip(weights)
                        .map(|(&i, w)| f32::from(data[c * cells + i]) * w / 255.)
                        .sum::<f32>()
                };
                assert_eq!(
                    sample.splat,
                    std::array::from_fn(|c| expected(c, &committed.layers.splat))
                );
                assert_eq!(
                    sample.biome,
                    std::array::from_fn(|c| expected(c, &committed.layers.biome))
                );
                let nearest = ((v * sr as f32).floor() as usize).min(sr - 1) * sr
                    + ((u * sr as f32).floor() as usize).min(sr - 1);
                assert_eq!(sample.alpha, committed.alpha[nearest]);
                assert_eq!(sample.topology, committed.layers.topology[nearest]);
                holes += usize::from(sample.alpha < 128);
                partial += usize::from(sample.alpha > 0 && sample.alpha < 255);
            }
        }
        assert!(holes > 100);
        assert!(partial > 20);
        for (u, v) in [
            (f32::NAN, 0.5),
            (0.5, f32::INFINITY),
            (-0.001, 0.5),
            (1.001, 0.5),
        ] {
            assert!(projection.sample(u, v).is_none());
        }
        assert!(projection.cell(sr, 0).is_none());
        assert!(projection.cell(0, sr).is_none());
    }

    #[test]
    fn projection_validation_is_cancellable_and_rejects_invalid_inputs() {
        let mut base = base();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        assert!(Terrain::new(&base, &alpha, || true).unwrap().is_none());
        let calls = std::cell::Cell::new(0);
        assert!(
            Terrain::new(&base, &alpha, || {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .unwrap()
            .is_none()
        );
        assert!(Terrain::new(&base, &alpha[..alpha.len() - 1], || false).is_err());
        base.height[17] = -1;
        assert!(Terrain::new(&base, &alpha, || false).is_err());
        base.height[17] = 18000;
        let terrain = Terrain::new(&base, &alpha, || false).unwrap().unwrap();
        let mut stamp = stamp();
        stamp.yaw_degrees = f64::NAN;
        assert!(terrain.projected(&stamp).is_err());
        assert!(std::ptr::eq(terrain.layers, &base));
        assert!(std::ptr::eq(terrain.alpha.as_ptr(), alpha.as_ptr()));
    }
}
