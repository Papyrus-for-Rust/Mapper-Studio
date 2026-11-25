use crate::{Field, Settings as FieldSettings, erosion, generate, monuments, paths, stamps, water};
use rayon::prelude::*;
use std::time::Instant;

pub const STAGES: [(&str, f64); 10] = [
    ("Terrain", 0.02),
    ("Biomes", 0.45),
    ("Ground textures", 0.6),
    ("Topology", 0.72),
    ("Terrain erosion", 0.76),
    ("Water", 0.78),
    ("Alpha", 0.8),
    ("Paths", 0.81),
    ("Monument placement", 0.82),
    ("Prefab stamps", 0.84),
];

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub size: usize,
    pub seed: i32,
    pub threads: usize,
    pub biomes: [u32; 4],
    pub erode: bool,
    pub rivers: water::Rivers,
    pub roads: paths::Roads,
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1000..=6000).contains(&self.size) {
            return Err("world size must be between 1000 and 6000".into());
        }
        self.field().validate()?;
        self.rivers.validate(self.size)?;
        self.roads.validate()
    }
    fn field(&self) -> FieldSettings {
        FieldSettings {
            seed: self.seed,
            resolution: 513,
            threads: self.threads,
            biomes: self.biomes,
        }
    }
    pub fn height_resolution(&self) -> usize {
        let upper = self.size.next_power_of_two();
        let closest = if self.size - upper / 2 < upper - self.size {
            upper / 2
        } else {
            upper
        };
        closest.min(4096) + 1
    }
    pub fn surface_resolution(&self) -> usize {
        (self.size / 2).next_power_of_two().min(2048)
    }
}

pub struct Layers {
    pub height: Vec<i16>,
    pub water: Vec<i16>,
    pub biome: Vec<u8>,
    pub splat: Vec<u8>,
    pub topology: Vec<i32>,
    pub alpha: Vec<u8>,
    pub paths: Vec<paths::Path>,
    pub prefabs: Vec<monuments::Placement>,
    pub milliseconds: [f64; 10],
}

pub fn generate_layers(
    settings: Settings,
    monument_request: &monuments::Request,
    mut progress: impl FnMut(&str, f64),
) -> Result<Layers, String> {
    settings.validate()?;
    monument_request.validate()?;
    let mut milliseconds = [0.0; 10];
    progress(STAGES[0].0, STAGES[0].1);
    let clock = Instant::now();
    let source = generate(settings.field())?;
    let resolution = settings.height_resolution();
    let surface = settings.surface_resolution();
    let cells = surface * surface;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(settings.threads.min(513))
        .build()
        .map_err(|e| e.to_string())?;
    let mut height = vec![0; resolution * resolution];
    pool.install(|| {
        height.par_iter_mut().enumerate().for_each(|(i, value)| {
            let h = sample(
                &source,
                (i % resolution) as f32 / (resolution - 1) as f32,
                (i / resolution) as f32 / (resolution - 1) as f32,
            );
            *value = erosion::encode((h - 0.08) * 400.0);
        })
    });
    milliseconds[0] = clock.elapsed().as_secs_f64() * 1000.0;
    progress(STAGES[1].0, STAGES[1].1);
    let clock = Instant::now();
    let mut biome = vec![0; cells * 5];
    pool.install(|| {
        biome
            .par_chunks_mut(cells)
            .enumerate()
            .for_each(|(channel, values)| {
                for (i, value) in values.iter_mut().enumerate() {
                    let x = (i % surface) * 512 / (surface - 1);
                    let z = (i / surface) * 512 / (surface - 1);
                    let b = source.biomes[z * 513 + x];
                    let chosen = if b == 0 { 1 } else { b as usize - 1 };
                    if chosen == channel {
                        *value = 255;
                    }
                }
            })
    });
    milliseconds[1] = clock.elapsed().as_secs_f64() * 1000.0;
    progress(STAGES[2].0, STAGES[2].1);
    let clock = Instant::now();
    let mut channels = vec![0_u8; cells];
    let mut topology = vec![0; cells];
    pool.install(|| {
        channels.par_iter_mut().enumerate().for_each(|(i, value)| {
            let h = sample(
                &source,
                (i % surface) as f32 / (surface - 1) as f32,
                (i / surface) as f32 / (surface - 1) as f32,
            );
            *value = if h < 0.1 || biome[i] != 0 {
                2
            } else if biome[3 * cells + i] != 0 {
                1
            } else if h > 0.48 {
                3
            } else {
                4
            };
        })
    });
    let mut splat = vec![0; cells * 8];
    pool.install(|| {
        splat
            .par_chunks_mut(cells)
            .enumerate()
            .for_each(|(channel, values)| {
                for (i, value) in values.iter_mut().enumerate() {
                    if channels[i] == channel as u8 {
                        *value = 255;
                    }
                }
            })
    });
    drop(channels);
    milliseconds[2] = clock.elapsed().as_secs_f64() * 1000.0;
    progress(STAGES[3].0, STAGES[3].1);
    let clock = Instant::now();
    pool.install(|| {
        topology.par_iter_mut().enumerate().for_each(|(i, value)| {
            let h = sample(
                &source,
                (i % surface) as f32 / (surface - 1) as f32,
                (i / surface) as f32 / (surface - 1) as f32,
            );
            *value = if h < 0.08 {
                256
            } else if h < 0.1 {
                16
            } else {
                1
            };
        })
    });
    drop(source);
    milliseconds[3] = clock.elapsed().as_secs_f64() * 1000.0;
    if settings.erode {
        progress(STAGES[4].0, STAGES[4].1);
        let clock = Instant::now();
        erosion::apply_with_pool(
            &mut height,
            erosion::Settings {
                resolution,
                world_size: settings.size,
                threads: settings.threads,
                iterations: 512,
            },
            &pool,
        )?;
        milliseconds[4] = clock.elapsed().as_secs_f64() * 1000.0;
    }
    progress(STAGES[5].0, STAGES[5].1);
    let clock = Instant::now();
    let water = water::apply(
        &mut height,
        resolution,
        &mut topology,
        surface,
        settings,
        &pool,
    );
    milliseconds[5] = clock.elapsed().as_secs_f64() * 1000.0;
    progress(STAGES[6].0, STAGES[6].1);
    let clock = Instant::now();
    let alpha = vec![255; cells];
    milliseconds[6] = clock.elapsed().as_secs_f64() * 1000.0;
    progress(STAGES[7].0, STAGES[7].1);
    let clock = Instant::now();
    let paths = if settings.rivers.count > 0
        || (settings.roads.enabled && (settings.roads.ring || settings.roads.branches > 0))
    {
        let source = generate(FieldSettings {
            resolution: 257,
            ..settings.field()
        })?;
        paths::generate(
            &source,
            257,
            settings.size,
            settings.seed,
            settings.rivers,
            settings.roads,
        )?
    } else {
        Vec::new()
    };
    milliseconds[7] = clock.elapsed().as_secs_f64() * 1000.0;
    progress(STAGES[8].0, STAGES[8].1);
    let clock = Instant::now();
    let prefabs = monuments::generate(
        monument_request,
        &height,
        resolution,
        settings.size,
        settings.seed,
    )?;
    milliseconds[8] = clock.elapsed().as_secs_f64() * 1000.0;
    let mut layers = Layers {
        height,
        water,
        biome,
        splat,
        topology,
        alpha,
        paths,
        prefabs,
        milliseconds,
    };
    progress(STAGES[9].0, STAGES[9].1);
    let clock = Instant::now();
    stamps::apply(
        &monument_request.stamps,
        &monument_request.entries,
        &layers.prefabs,
        stamps::Grid {
            height: &mut layers.height,
            splat: &mut layers.splat,
            alpha: &mut layers.alpha,
            topology: &mut layers.topology,
            height_resolution: resolution,
            surface_resolution: surface,
            world_size: settings.size,
        },
    )?;
    layers.milliseconds[9] = clock.elapsed().as_secs_f64() * 1000.0;
    Ok(layers)
}

fn sample(source: &Field, x: f32, z: f32) -> f32 {
    let fx = x * 512.0;
    let fz = z * 512.0;
    let ix = fx as usize;
    let iz = fz as usize;
    let nx = (ix + 1).min(512);
    let nz = (iz + 1).min(512);
    let a = source.heights[iz * 513 + ix]
        + (source.heights[iz * 513 + nx] - source.heights[iz * 513 + ix]) * (fx - ix as f32);
    let b = source.heights[nz * 513 + ix]
        + (source.heights[nz * 513 + nx] - source.heights[nz * 513 + ix]) * (fx - ix as f32);
    a + (b - a) * (fz - iz as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(size: usize) -> Settings {
        Settings {
            size,
            seed: 12345,
            threads: 4,
            biomes: [40, 15, 15, 30],
            erode: false,
            rivers: water::Rivers {
                count: 0,
                width: 8.0,
                maximum_length: 0,
            },
            roads: paths::Roads {
                enabled: false,
                ring: true,
                branches: 4,
                width: 12.0,
            },
        }
    }

    #[test]
    fn resolution_rules_include_midpoint_ties_and_caps() {
        for (size, height, surface) in [
            (1000, 1025, 512),
            (1026, 1025, 1024),
            (1535, 1025, 1024),
            (1536, 2049, 1024),
            (3071, 2049, 2048),
            (3072, 4097, 2048),
            (6000, 4097, 2048),
        ] {
            let settings = settings(size);
            settings.validate().unwrap();
            assert_eq!(settings.height_resolution(), height);
            assert_eq!(settings.surface_resolution(), surface);
        }
    }

    #[test]
    fn invalid_settings_fail_before_progress_or_allocation() {
        for mut settings in [
            settings(0),
            settings(999),
            settings(6001),
            settings(usize::MAX),
        ] {
            assert!(
                generate_layers(settings, &monuments::Request::disabled(), |_, _| panic!(
                    "invalid request emitted progress"
                ))
                .is_err()
            );
            settings.size = 1000;
            settings.rivers.width = f64::NAN;
            assert!(settings.validate().is_err());
            settings.rivers.width = f64::INFINITY;
            assert!(settings.validate().is_err());
            settings.rivers.width = 1001.0;
            assert!(settings.validate().is_err());
        }
    }
}
