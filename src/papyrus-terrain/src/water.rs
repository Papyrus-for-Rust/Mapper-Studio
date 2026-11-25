use crate::erosion::{decode, encode};
use rayon::prelude::*;
use std::collections::{HashSet, VecDeque};

const OCEAN: i32 = 128;
const RIVER: i32 = 16384;
const LAKE: i32 = 65536;

#[derive(Clone, Copy, Debug)]
pub struct Rivers {
    pub count: usize,
    pub width: f64,
    pub maximum_length: usize,
}

impl Rivers {
    pub fn validate(&self, world_size: usize) -> Result<(), String> {
        if self.count > i32::MAX as usize
            || self.maximum_length > i32::MAX as usize
            || !self.width.is_finite()
            || self.width <= 0.0
            || self.width > world_size as f64
        {
            return Err(
                "invalid river settings; width must be finite and between 0 and world size".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn apply(
    height: &mut [i16],
    resolution: usize,
    topology: &mut [i32],
    surface_resolution: usize,
    settings: crate::world::Settings,
    pool: &rayon::ThreadPool,
) -> Vec<i16> {
    let size = settings.size;
    let seed = settings.seed;
    let rivers = settings.rivers;
    let mut ocean = vec![false; topology.len()];
    let mut queue = VecDeque::new();
    let try_queue = |x: usize, z: usize, ocean: &mut [bool], queue: &mut VecDeque<usize>| {
        let index = z * surface_resolution + x;
        let hi = (z * (resolution - 1) / (surface_resolution - 1)) * resolution
            + x * (resolution - 1) / (surface_resolution - 1);
        if !ocean[index] && decode(height[hi]) <= 0.0 {
            ocean[index] = true;
            queue.push_back(index);
        }
    };
    for i in 0..surface_resolution {
        try_queue(i, 0, &mut ocean, &mut queue);
        try_queue(i, surface_resolution - 1, &mut ocean, &mut queue);
        try_queue(0, i, &mut ocean, &mut queue);
        try_queue(surface_resolution - 1, i, &mut ocean, &mut queue);
    }
    while let Some(index) = queue.pop_front() {
        let x = index % surface_resolution;
        let z = index / surface_resolution;
        if x > 0 {
            try_queue(x - 1, z, &mut ocean, &mut queue);
        }
        if x + 1 < surface_resolution {
            try_queue(x + 1, z, &mut ocean, &mut queue);
        }
        if z > 0 {
            try_queue(x, z - 1, &mut ocean, &mut queue);
        }
        if z + 1 < surface_resolution {
            try_queue(x, z + 1, &mut ocean, &mut queue);
        }
    }
    for (i, value) in topology.iter_mut().enumerate() {
        *value = if ocean[i] {
            *value | OCEAN
        } else {
            *value & !OCEAN
        };
    }
    drop(ocean);
    let mut water = vec![0; height.len()];
    let mut random = seed as u32 ^ 0x9E3779B9;
    let width = (rivers.width / size as f64 * resolution as f64)
        .round_ties_even()
        .max(1.0) as i32;
    for _ in 0..rivers.count {
        let start = find_high_point(height, resolution, &mut random);
        let surface = decode(height[start]);
        let mut grid = RiverGrid {
            height,
            water: &mut water,
            topology,
            resolution,
            surface_resolution,
        };
        grid.carve(start, surface, (width * 2).max(2), true);
        let mut current = start;
        let mut visited = HashSet::new();
        let maximum = if rivers.maximum_length > 0 {
            (resolution * 3).min(rivers.maximum_length.saturating_mul(resolution) / size)
        } else {
            resolution * 3
        };
        for _ in 0..maximum {
            if !visited.insert(current) {
                break;
            }
            let surface = decode(grid.height[current]);
            grid.carve(current, surface, width, false);
            if surface <= 0.0 {
                break;
            }
            let next = downhill(current, grid.height, resolution, &mut random);
            if next == current {
                break;
            }
            current = next;
        }
    }
    pool.install(|| {
        topology
            .par_iter_mut()
            .enumerate()
            .for_each(|(index, value)| {
                if *value & OCEAN != 0 {
                    return;
                }
                let x = index % surface_resolution;
                let z = index / surface_resolution;
                let hx = x * (resolution - 1) / (surface_resolution - 1);
                let hz = z * (resolution - 1) / (surface_resolution - 1);
                let metres = decode(height[hz * resolution + hx]);
                let slope = slope(height, resolution, hx, hz);
                *value |= (1 << 29) | 1;
                if metres > 0.0 && metres < 6.0 {
                    *value |= 16;
                }
                if metres > 6.0 && metres < 18.0 {
                    *value |= 8 | 256;
                }
                if slope > 28.0 {
                    *value |= 2;
                }
                if metres > 170.0 {
                    *value |= 1 << 23;
                }
                let noise = hash(x, z, seed);
                // The C# reference compares these two thresholds as doubles, but forestside as floats.
                if metres > 12.0 && metres < 150.0 && slope < 22.0 && noise as f64 > 0.54 {
                    *value |= 32 | (1 << 24) | 512;
                }
                if noise > 0.49 && noise < 0.54 {
                    *value |= 64;
                }
            })
    });
    water
}

struct RiverGrid<'a> {
    height: &'a mut [i16],
    water: &'a mut [i16],
    topology: &'a mut [i32],
    resolution: usize,
    surface_resolution: usize,
}

impl RiverGrid<'_> {
    fn carve(&mut self, center: usize, surface: f32, radius: i32, lake: bool) {
        let cx = (center % self.resolution) as i32;
        let cz = (center / self.resolution) as i32;
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let distance = ((dx * dx + dz * dz) as f32).sqrt();
                if if lake {
                    dx * dx + dz * dz > radius * radius
                } else {
                    distance > radius as f32
                } {
                    continue;
                }
                let x = (cx + dx).clamp(0, self.resolution as i32 - 1) as usize;
                let z = (cz + dz).clamp(0, self.resolution as i32 - 1) as usize;
                let index = z * self.resolution + x;
                let bed = if lake {
                    surface - 3.5
                } else {
                    surface - (1.5 + (1.0 - distance / radius.max(1) as f32) * 2.5)
                };
                if decode(self.height[index]) > bed {
                    self.height[index] = encode(bed);
                }
                let encoded_surface = encode(surface);
                if lake || self.water[index] < encoded_surface {
                    self.water[index] = encoded_surface;
                }
                let tx = x * (self.surface_resolution - 1) / (self.resolution - 1);
                let tz = z * (self.surface_resolution - 1) / (self.resolution - 1);
                self.topology[tz * self.surface_resolution + tx] |= if lake { LAKE } else { RIVER };
            }
        }
    }
}

fn find_high_point(height: &[i16], resolution: usize, seed: &mut u32) -> usize {
    for _ in 0..256 {
        let x = 8 + (next(seed) % (resolution - 16).max(1) as u32) as usize;
        let z = 8 + (next(seed) % (resolution - 16).max(1) as u32) as usize;
        let index = z * resolution + x;
        if decode(height[index]) > 35.0 {
            return index;
        }
    }
    let mut highest = 0;
    for index in 1..height.len() {
        if height[index] > height[highest] {
            highest = index;
        }
    }
    highest
}

fn downhill(index: usize, height: &[i16], resolution: usize, seed: &mut u32) -> usize {
    let x = (index % resolution) as i32;
    let z = (index / resolution) as i32;
    let mut result = index;
    let mut best = decode(height[index]);
    for dz in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dz == 0 {
                continue;
            }
            let nx = (x + dx).clamp(0, resolution as i32 - 1) as usize;
            let nz = (z + dz).clamp(0, resolution as i32 - 1) as usize;
            let candidate = nz * resolution + nx;
            let value = decode(height[candidate]);
            if value < best || (value == best && next(seed) & 1 == 0) {
                best = value;
                result = candidate;
            }
        }
    }
    result
}

pub(crate) fn next(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

fn slope(height: &[i16], resolution: usize, x: usize, z: usize) -> f32 {
    let left = decode(height[z * resolution + x.saturating_sub(1)]);
    let right = decode(height[z * resolution + (x + 1).min(resolution - 1)]);
    let down = decode(height[z.saturating_sub(1) * resolution + x]);
    let up = decode(height[(z + 1).min(resolution - 1) * resolution + x]);
    (((right - left) * (right - left) + (up - down) * (up - down)).sqrt() / 2.0).atan() * 57.29578
}

fn hash(x: usize, z: usize, seed: i32) -> f32 {
    let mut value = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((z as u32).wrapping_mul(668265263))
        ^ seed as u32;
    value = (value ^ (value >> 13)).wrapping_mul(1274126177);
    (value ^ (value >> 16)) as f32 / u32::MAX as f32
}
