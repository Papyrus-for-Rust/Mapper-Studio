//! Encoded slope decisions for native height grids.
use papyrus_terrain::terrain_slope;
use rayon::prelude::*;

const AMBIGUOUS: u32 = 0xaaaa_aaaa;

pub struct Threshold {
    pub degrees: f32,
    pub(crate) low: u32,
    pub(crate) high: u32,
}

impl Threshold {
    pub fn new(size: u32, side: usize, degrees: f32) -> Result<Self, String> {
        if !(1000..=6000).contains(&size)
            || !(3..=4097).contains(&side)
            || !(1. ..=89.).contains(&degrees)
        {
            return Err("Unsupported slope threshold or terrain dimensions".into());
        }
        let norm_y = size as f32 / 1000. / side as f32;
        let energy = |angle: f64| {
            (angle.to_radians().tan() * (2. * f64::from(norm_y) / f64::from(3.051944E-5_f32)))
                .powi(2)
        };
        let degrees64 = f64::from(degrees);
        // Encoded differences precede the CPU's normalized f32 subtraction.
        // Resolve the boundary band with the original CPU calculation.
        Ok(Self {
            degrees,
            low: energy(degrees64 - 0.1).floor() as u32,
            high: energy(degrees64 + 0.1).ceil() as u32,
        })
    }

    fn decision(&self, energy: u32) -> u32 {
        if energy > self.high {
            1
        } else if energy >= self.low {
            2
        } else {
            0
        }
    }
}

/// Two bits per cell. A one is above the threshold and a two needs CPU resolution.
pub fn cpu(
    input: &terrain_slope::Input<'_>,
    threshold: &Threshold,
    pool: &rayon::ThreadPool,
) -> Vec<u32> {
    let n = input.resolution;
    let cells = n * n;
    let mut words = vec![0; cells.div_ceil(16)];
    pool.install(|| {
        words.par_iter_mut().enumerate().for_each(|(index, word)| {
            for lane in 0..16 {
                let i = index * 16 + lane;
                let (x, z) = (i % n, i / n);
                if i >= cells || x == 0 || x + 1 == n || z == 0 || z + 1 == n {
                    continue;
                }
                let base = i32::from(input.heights[i - n - 1]);
                let dx = i32::from(input.heights[i - n + 1]) - base;
                let dz = i32::from(input.heights[i + n - 1]) - base;
                *word |= threshold.decision((dx * dx + dz * dz) as u32) << (2 * lane);
            }
        });
    });
    words
}

pub fn resolve_with(
    words: &mut [u32],
    pool: &rayon::ThreadPool,
    passes: impl Fn(usize) -> bool + Sync,
) -> usize {
    pool.install(|| {
        words
            .par_iter_mut()
            .enumerate()
            .map(|(index, word)| {
                let mut pending = *word & AMBIGUOUS;
                let count = pending.count_ones() as usize;
                *word &= !AMBIGUOUS;
                while pending != 0 {
                    let bit = pending.trailing_zeros();
                    pending &= pending - 1;
                    let cell = index * 16 + (bit / 2) as usize;
                    if passes(cell) {
                        *word |= 1 << (bit - 1);
                    }
                }
                count
            })
            .sum()
    })
}
