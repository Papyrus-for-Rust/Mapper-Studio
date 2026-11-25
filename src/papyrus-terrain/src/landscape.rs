//! Versioned editor landscape: irregular coast, lowlands, connected ranges and coherent climate.
//! The historical field/worker path remains unchanged for existing recipes.
use crate::{fbm, noise, smooth_step};

pub const VERSION: &str = "papyrus-landscape-preview-1";
/// Stable procedural ground-cover variation used by the editor rasterizer.
pub fn cover_noise(x: f64, z: f64, seed: i32) -> f64 {
    noise(x, z, seed)
}
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub terrain_seed: i32,
    pub biome_seed: i32,
    pub resolution: usize,
    pub size: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    CoastAndRelief,
    TerrainRefinement,
    Climate,
}
impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::CoastAndRelief => "Coastline and relief",
            Self::TerrainRefinement => "Terrain refinement",
            Self::Climate => "Climate and biome blending",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Landscape {
    pub resolution: usize,
    /// World elevation in metres, Z increases towards the north.
    pub heights: Vec<f32>,
    /// Arid, temperate, tundra, arctic weights. Integer weights sum to 255 on land.
    pub biomes: Vec<[u8; 4]>,
}
impl Settings {
    pub fn validate(self) -> Result<(), String> {
        if self.terrain_seed < 1
            || self.biome_seed < 1
            || !(33..=513).contains(&self.resolution)
            || !(self.resolution - 1).is_power_of_two()
            || !(1000..=6000).contains(&self.size)
        {
            return Err("Invalid landscape seeds, resolution or world size.".into());
        }
        Ok(())
    }
}

/// Three stage callbacks borrow the result; cancellation is checked at bounded row batches/passes.
pub fn generate(
    settings: Settings,
    mut progress: impl FnMut(Stage, &Landscape),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Landscape>, String> {
    generate_inner(settings, &mut progress, cancelled, None)
}
/// Parallel rows preserve each cell's math and the order of stage callbacks.
/// Work is bounded to 32 rows between cancellation checks.
pub fn generate_parallel(
    settings: Settings,
    workers: usize,
    mut progress: impl FnMut(Stage, &Landscape),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Landscape>, String> {
    settings.validate()?;
    if !(1..=1024).contains(&workers) {
        return Err("Invalid terrain worker limit".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers.min(settings.resolution))
        .build()
        .map_err(|e| e.to_string())?;
    generate_inner(settings, &mut progress, cancelled, Some(&pool))
}
/// Use a caller-owned pool so the workstation can share its execution allowance.
pub fn generate_with_pool(
    settings: Settings,
    pool: &rayon::ThreadPool,
    mut progress: impl FnMut(Stage, &Landscape),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Landscape>, String> {
    generate_inner(settings, &mut progress, cancelled, Some(pool))
}
fn generate_inner(
    settings: Settings,
    progress: &mut impl FnMut(Stage, &Landscape),
    cancelled: impl Fn() -> bool,
    pool: Option<&rayon::ThreadPool>,
) -> Result<Option<Landscape>, String> {
    settings.validate()?;
    if cancelled() {
        return Ok(None);
    }
    let n = settings.resolution;
    let seed = settings.terrain_seed;
    let phase = noise(0.6, 0.2, seed) * std::f64::consts::TAU;
    let offset = (noise(2.1, 1.4, seed) - 0.5) * 0.4;
    let mut land = Landscape {
        resolution: n,
        heights: vec![0.; n * n],
        biomes: vec![[0; 4]; n * n],
    };
    if !rows(&mut land.heights, n, pool, &cancelled, |z, row| {
        let nz = z as f64 / (n - 1) as f64 * 2. - 1.;
        for (x, value) in row.iter_mut().enumerate() {
            let nx = x as f64 / (n - 1) as f64 * 2. - 1.;
            let wx = nx + (fbm(nx * 1.8 + 2., nz * 1.8, seed ^ 0x31a92, 3, 0.5) - 0.5) * 0.26;
            let wz = nz + (fbm(nx * 1.8, nz * 1.8 + 3., seed ^ 0x183af, 3, 0.5) - 0.5) * 0.26;
            let angle = wz.atan2(wx);
            let radius =
                0.78 + 0.075 * (angle * 3. + phase).sin() + 0.05 * (angle * 5. - phase).sin();
            let distance = (wx.abs().powi(4) + (wz * 1.06).abs().powi(4)).powf(0.25);
            let coast =
                radius - distance + (fbm(wx * 4.2, wz * 4.2, seed ^ 0x72b31, 3, 0.5) - 0.5) * 0.16;
            let world_y = if coast <= 0. {
                -2. - smooth_step(0., 0.28, -coast) * 48.
            } else {
                let inland = smooth_step(0., 0.10, coast);
                let rolling = fbm(wx * 2.4 + 4., wz * 2.4, seed ^ 0x4f3b1, 3, 0.45);
                let plains = 8. + rolling * 18.;
                let spine = wx - 0.22 * (wz * 3. + phase).sin() - offset;
                let band =
                    (1. - smooth_step(0.06, 0.31, spine.abs())) * smooth_step(0.06, 0.20, coast);
                let ridge = 1. - (fbm(wx * 5.4, wz * 5.4, seed ^ 0x21b3c, 4, 0.5) * 2. - 1.).abs();
                let range = band * (30. + ridge.powf(2.2) * 145.);
                let detail = (noise(wx * 28., wz * 28., seed ^ 0x118ab) - 0.5) * 1.4;
                1. + inland * (plains + range + detail)
            };
            // Preserve a navigable ocean border for every seed.
            *value = if x == 0 || z == 0 || x == n - 1 || z == n - 1 {
                -50.
            } else {
                world_y as f32
            };
        }
    }) {
        return Ok(None);
    }
    progress(Stage::CoastAndRelief, &land);
    // Conservative smoothing refines the geometry; it is not claimed as hydraulic erosion.
    for _ in 0..2 {
        let mut next = land.heights.clone();
        for z in 1..n - 1 {
            if cancelled() {
                return Ok(None);
            }
            for x in 1..n - 1 {
                let i = z * n + x;
                if land.heights[i] > 8. {
                    let average = (land.heights[i - 1]
                        + land.heights[i + 1]
                        + land.heights[i - n]
                        + land.heights[i + n])
                        * 0.25;
                    next[i] = land.heights[i] * 0.82 + average * 0.18;
                }
            }
        }
        land.heights = next;
    }
    progress(Stage::TerrainRefinement, &land);
    let biome_seed = settings.biome_seed;
    let angle = (noise(2.5, 0.8, biome_seed) - 0.5) * 0.75;
    let (s, c) = angle.sin_cos();
    if !rows(&mut land.biomes, n, pool, &cancelled, |z, row| {
        let nz = z as f64 / (n - 1) as f64 * 2. - 1.;
        for (x, cell) in row.iter_mut().enumerate() {
            let i = z * n + x;
            let h = land.heights[i];
            if h <= 0. {
                continue;
            }
            let nx = x as f64 / (n - 1) as f64 * 2. - 1.;
            let climate = (nz * c + nx * s) * 0.46
                + 0.5
                + (fbm(nx * 2.1 + 1.3, nz * 2.1, biome_seed, 3, 0.5) - 0.5) * 0.34
                + f64::from((h - 70.).max(0.)) / 700.;
            let arid = 1. - smooth_step(0.26, 0.42, climate);
            let snow = smooth_step(0.72, 0.84, climate);
            let tundra = smooth_step(0.57, 0.70, climate) * (1. - snow);
            let temperate = (1. - arid) * (1. - snow) * (1. - tundra);
            let weights = [arid, temperate, tundra, snow];
            let total = weights.iter().sum::<f64>();
            let mut remaining = 255_u16;
            for (channel, value) in weights.into_iter().enumerate() {
                let weight = if channel == 3 {
                    remaining as u8
                } else {
                    ((value / total * 255.).round() as u16).min(remaining) as u8
                };
                cell[channel] = weight;
                remaining -= u16::from(weight);
            }
        }
    }) {
        return Ok(None);
    }
    progress(Stage::Climate, &land);
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(land))
}

fn rows<T: Send>(
    values: &mut [T],
    n: usize,
    pool: Option<&rayon::ThreadPool>,
    cancelled: &impl Fn() -> bool,
    fill: impl Fn(usize, &mut [T]) + Sync,
) -> bool {
    use rayon::prelude::*;
    for (batch, chunk) in values.chunks_mut(n * 32).enumerate() {
        if cancelled() {
            return false;
        }
        if let Some(pool) = pool {
            pool.install(|| {
                chunk
                    .par_chunks_mut(n)
                    .enumerate()
                    .for_each(|(row, dst)| fill(batch * 32 + row, dst))
            });
        } else {
            for (row, dst) in chunk.chunks_mut(n).enumerate() {
                fill(batch * 32 + row, dst);
            }
        }
    }
    !cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_rows_match_serial_stages_and_world_bits() {
        let mut stages = Vec::new();
        let original = generate(settings(), |s, l| stages.push((s, l.clone())), || false).unwrap();
        for workers in [1, 4] {
            let mut parallel_stages = Vec::new();
            let parallel = generate_parallel(
                settings(),
                workers,
                |s, l| parallel_stages.push((s, l.clone())),
                || false,
            )
            .unwrap();
            assert_eq!(original, parallel);
            assert_eq!(stages, parallel_stages);
        }
        assert!(
            generate_parallel(settings(), 4, |_, _| panic!("cancelled"), || true)
                .unwrap()
                .is_none()
        );
        assert!(generate_parallel(settings(), 0, |_, _| {}, || false).is_err());
    }
    fn settings() -> Settings {
        Settings {
            terrain_seed: 12345,
            biome_seed: 9876,
            resolution: 65,
            size: 4000,
        }
    }
    #[test]
    fn stable_geometry_with_independent_biome_seed_and_ocean_border() {
        let a = generate(settings(), |_, _| {}, || false).unwrap().unwrap();
        assert_eq!(
            a,
            generate(settings(), |_, _| {}, || false).unwrap().unwrap()
        );
        let b = generate(
            Settings {
                biome_seed: 111,
                ..settings()
            },
            |_, _| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.heights, b.heights);
        assert_ne!(a.biomes, b.biomes);
        for z in 0..65 {
            assert!(a.heights[z * 65] < 0.);
            assert!(a.heights[z * 65 + 64] < 0.);
        }
        assert!(a.heights[..65].iter().all(|h| *h < 0.));
        assert!(a.heights.iter().any(|h| *h > 80.));
        assert!(a.heights.iter().any(|h| (8. ..35.).contains(h)));
        for (i, w) in a.biomes.iter().enumerate() {
            assert_eq!(
                w.iter().map(|v| u16::from(*v)).sum::<u16>(),
                if a.heights[i] > 0. { 255 } else { 0 }
            );
        }
    }
    #[test]
    fn callback_order_and_cancellation_are_bounded() {
        let mut stages = Vec::new();
        generate(settings(), |s, _| stages.push(s), || false).unwrap();
        assert_eq!(
            stages,
            vec![
                Stage::CoastAndRelief,
                Stage::TerrainRefinement,
                Stage::Climate
            ]
        );
        let cancelled = std::cell::Cell::new(false);
        assert!(
            generate(settings(), |_, _| cancelled.set(true), || cancelled.get())
                .unwrap()
                .is_none()
        );
        assert!(
            generate(
                Settings {
                    resolution: usize::MAX,
                    ..settings()
                },
                |_, _| {},
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn coastline_is_asymmetric_and_seed_changes_actual_heights() {
        let a = generate(settings(), |_, _| {}, || false).unwrap().unwrap();
        let b = generate(
            Settings {
                terrain_seed: 12346,
                ..settings()
            },
            |_, _| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert_ne!(a.heights, b.heights);
        assert!(
            (0..65 * 65)
                .any(|i| (a.heights[i] > 0.) != (a.heights[i / 65 * 65 + 64 - i % 65] > 0.))
        );
    }
}
