//! Worker-only normal-texture reference. Source binding belongs to the caller.
//! BC3 range fitting is independent; it is not Unity compressor byte parity.
use rayon::prelude::*;
use std::sync::Arc;

pub(crate) fn working_bytes(resolution: usize, mipped: bool) -> usize {
    let cells = resolution * resolution;
    retained_bytes(resolution, mipped) + if mipped { cells * 5 } else { 0 }
}

pub(crate) fn retained_bytes(resolution: usize, mipped: bool) -> usize {
    let cells = resolution * resolution;
    // Mipped textures retain every square mip level down to one texel.
    if mipped {
        (4 * cells - 1) / 3 * 4
    } else {
        cells * 4
    }
}
mod mips;

pub(crate) const MIP_VERSION: &str = mips::VERSION;
pub const VERSION: &str = "papyrus-map-normal-bc3-rangefit-1";
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub height_resolution: usize,
    pub size: u32,
    pub origin_y: f32,
    pub vertical_span: f32,
    pub maximum_work: usize,
    pub maximum_bytes: usize,
}
#[derive(Clone, Debug)]
pub struct Report {
    pub engine: &'static str,
    pub resolution: usize,
    pub blocks: usize,
    pub work_reserved: usize,
    /// Retained decoded pixel bytes, including lower levels when present.
    /// Working bytes also include raw mip buffers. Each worker uses a small
    /// block/compressor stack outside these explicit image-buffer counts.
    pub pixel_bytes: usize,
    pub working_bytes: usize,
}
#[derive(Clone, Debug)]
pub struct Grid {
    resolution: usize,
    pixels: Arc<Vec<[u8; 4]>>,
    levels: Vec<Level>,
    pub report: Report,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Level {
    pub resolution: usize,
    pub offset: usize,
}
impl Grid {
    pub fn resolution(&self) -> usize {
        self.resolution
    }
    pub fn pixels(&self) -> &[[u8; 4]] {
        &self.pixels[..self.resolution * self.resolution]
    }
    pub(crate) fn all_pixels(&self) -> &[[u8; 4]] {
        &self.pixels
    }
    pub(crate) fn mip_selection(&self, lod: f32) -> (Level, Level, f32) {
        let lod = lod.clamp(0., (self.levels.len() - 1) as f32);
        let low = lod as usize;
        let high = (low + 1).min(self.levels.len() - 1);
        (self.levels[low], self.levels[high], lod.fract())
    }
    pub(crate) fn lod_for_footprint(&self, uv_per_pixel: f32) -> f32 {
        (self.resolution as f32 * uv_per_pixel).max(1.).log2()
    }
    /// Trilinear clamp filtering of encoded channels. Lighting uses sample(),
    /// which always reads level zero independently of this slope input.
    pub(crate) fn sample_rgba_lod(&self, uv: [f32; 2], lod: f32) -> Option<[f32; 4]> {
        if !lod.is_finite() {
            return None;
        }
        let (low, high, blend) = self.mip_selection(lod);
        let a = self.sample_level(uv, low)?;
        let b = self.sample_level(uv, high)?;
        Some(std::array::from_fn(|c| a[c] + (b[c] - a[c]) * blend))
    }
    /// Linear, cell-centered Clamp sampling at mip zero. Decode after filtering;
    /// interpolating decoded normals would implement a different shader.
    pub fn sample_rgba(&self, uv: [f32; 2]) -> Option<[f32; 4]> {
        self.sample_level(uv, self.levels[0])
    }
    fn sample_level(&self, uv: [f32; 2], level: Level) -> Option<[f32; 4]> {
        if uv.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let n = level.resolution;
        let [x, z] = uv.map(|v| (v.clamp(0., 1.) * n as f32 - 0.5).clamp(0., (n - 1) as f32));
        let [a, b] = [x as usize, z as usize];
        let [c, d] = [(a + 1).min(n - 1), (b + 1).min(n - 1)];
        Some(std::array::from_fn(|i| {
            let at =
                |x: usize, z: usize| f32::from(self.pixels[level.offset + z * n + x][i]) / 255.;
            let lo = at(a, b) + (at(c, b) - at(a, b)) * x.fract();
            let hi = at(a, d) + (at(c, d) - at(a, d)) * x.fract();
            lo + (hi - lo) * z.fract()
        }))
    }
    pub fn sample(&self, uv: [f32; 2]) -> Option<[f32; 3]> {
        decode(self.sample_rgba(uv)?)
    }
}
/// PaperMap's sampled-channel decoder, returned in world X/up/Z order for the
/// material lighting API. Preserve nonunit results when XY exceeds the circle.
pub fn decode(rgba: [f32; 4]) -> Option<[f32; 3]> {
    if rgba
        .iter()
        .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
    {
        return None;
    }
    let x = rgba[0] * rgba[3] * 2. - 1.;
    let z = rgba[1] * 2. - 1.;
    Some([x, (1. - (x * x + z * z).min(1.)).sqrt(), z])
}
fn encode(mut normal: [f32; 3]) -> [u8; 4] {
    let angle = normal[1].clamp(-1., 1.).acos().to_degrees();
    let t = ((angle - 50.) / 20.).clamp(0., 1.);
    normal = papyrus_terrain::rail_costmap::slerp(normal, [0., 1., 0.], t);
    let byte = |v: f32| (((v + 1.) * 0.5).clamp(0., 1.) * 255.).round_ties_even() as u8;
    let z = byte(normal[2]);
    [z, z, z, byte(normal[0])]
}
pub fn generate(
    height: &[i16],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if settings
        .height_resolution
        .checked_mul(settings.height_resolution)
        != Some(height.len())
    {
        return Err("Invalid normal-texture source dimensions".into());
    }
    generate_sampled(
        |x, z| height.get(z * settings.height_resolution + x).copied(),
        settings,
        cancelled,
    )
}
/// The encoded-vertex reader must be stable for the whole operation. This allows
/// projected terrain to share the same kernel without copying a full height map.
pub(crate) fn generate_sampled(
    height: impl Fn(usize, usize) -> Option<i16> + Sync,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    generate_sampled_in(height, settings, None, cancelled)
}
/// Bounded parallel batches keep cancellation on the calling worker, allowing
/// non-Sync cancellation probes without sending them into a Rayon worker.
pub(crate) fn generate_sampled_in(
    height: impl Fn(usize, usize) -> Option<i16> + Sync,
    settings: Settings,
    pool: Option<&rayon::ThreadPool>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    generate_base_in(height, settings, pool, true, cancelled)
}
pub(crate) fn generate_mipped_sampled_in(
    height: impl Fn(usize, usize) -> Option<i16> + Sync,
    settings: Settings,
    pool: Option<&rayon::ThreadPool>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    mips::generate(height, settings, pool, cancelled)
}
fn generate_base_in(
    height: impl Fn(usize, usize) -> Option<i16> + Sync,
    settings: Settings,
    pool: Option<&rayon::ThreadPool>,
    compressed: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if cancelled() {
        return Ok(None);
    }
    let n = settings.height_resolution;
    if !(17..=4097).contains(&n)
        || !(n - 1).is_power_of_two()
        || !(1..=10000).contains(&settings.size)
        || !settings.origin_y.is_finite()
        || settings.origin_y.abs() > 30000.
        || !settings.vertical_span.is_finite()
        || !(1. ..=30000.).contains(&settings.vertical_span)
        || !(1..=4_000_000_000).contains(&settings.maximum_work)
    {
        return Err("Unsupported normal-texture dimensions or allowance".into());
    }
    let resolution = (n - 1) / 2;
    let blocks = (resolution / 4).pow(2);
    let pixel_bytes = working_bytes(resolution, false);
    let work = n * n + resolution * resolution * 128 + if compressed { blocks * 4096 } else { 0 };
    if work > settings.maximum_work || pixel_bytes > settings.maximum_bytes {
        return Err("Normal texture exceeds its work or pixel memory allowance".into());
    }
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            if height(x, z).is_none_or(|v| !(0..=32766).contains(&v)) {
                return Err("Invalid encoded normal-texture height".into());
            }
        }
    }
    let mut pixels = vec![[0; 4]; resolution * resolution];
    #[derive(Debug)]
    enum Failure {
        InvalidHeight,
    }
    for (batch, pixels) in pixels.chunks_mut(resolution * 32).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let mut run = || {
            pixels
                .par_chunks_mut(resolution * 4)
                .enumerate()
                .try_for_each(|(block_z, rows)| {
                    for block_x in 0..resolution / 4 {
                        let mut raw = [[0; 4]; 16];
                        for (i, pixel) in raw.iter_mut().enumerate() {
                            let x = block_x * 4 + i % 4;
                            let z = batch * 32 + block_z * 4 + i / 4;
                            let normal = crate::map_colors::normal_sampled(
                                settings.size,
                                n,
                                (x as f32 + 0.5) / resolution as f32,
                                (z as f32 + 0.5) / resolution as f32,
                                |u, v| {
                                    height(
                                        (u * (n - 1) as f32) as usize,
                                        (v * (n - 1) as f32) as usize,
                                    )
                                    .filter(|h| (0..=32766).contains(h))
                                    .map_or(f32::NAN, |h| {
                                        f32::from(h) / 32766. * settings.vertical_span
                                            + settings.origin_y
                                    })
                                },
                            );
                            if normal.iter().any(|v| !v.is_finite()) {
                                return Err(Failure::InvalidHeight);
                            }
                            *pixel = encode(normal);
                        }
                        let decoded = if compressed {
                            compress(raw, 0xffff)
                        } else {
                            raw
                        };
                        for (i, pixel) in decoded.into_iter().enumerate() {
                            rows[(i / 4) * resolution + block_x * 4 + i % 4] = pixel;
                        }
                    }
                    Ok(())
                })
        };
        let status = if let Some(pool) = pool {
            pool.install(run)
        } else {
            run()
        };
        match status {
            Err(Failure::InvalidHeight) => {
                return Err("Normal-texture reader changed or returned invalid data".into());
            }
            Ok(()) => {}
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Grid {
        resolution,
        pixels: Arc::new(pixels),
        levels: vec![Level {
            resolution,
            offset: 0,
        }],
        report: Report {
            engine: VERSION,
            resolution,
            blocks,
            work_reserved: work,
            pixel_bytes,
            working_bytes: pixel_bytes,
        },
    }))
}

fn compress(raw: [[u8; 4]; 16], mask: u32) -> [[u8; 4]; 16] {
    let mut compressed = [0; 16];
    texpresso::Format::Bc3.compress_block_masked(
        raw,
        mask,
        texpresso::Params {
            algorithm: texpresso::Algorithm::RangeFit,
            weights: texpresso::COLOUR_WEIGHTS_UNIFORM,
            weigh_colour_by_alpha: false,
        },
        &mut compressed,
    );
    texpresso::Format::Bc3.decompress_block(&compressed)
}
#[cfg(test)]
mod tests;
