//! Ground-cover visualization derived from generated heights and climate, not game assets.
use eframe::egui::{Color32, ColorImage};
use papyrus_terrain::landscape::{Landscape, cover_noise as noise};

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t.clamp(0., 1.))
}
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

pub fn render(land: &Landscape, size: u32, seed: u32, output: usize) -> Result<ColorImage, String> {
    render_cancellable(land, size, seed, output, || false)?.ok_or("Map rendering cancelled".into())
}
pub fn render_cancellable(
    land: &Landscape,
    size: u32,
    seed: u32,
    output: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    if !(33..=513).contains(&land.resolution)
        || !(land.resolution - 1).is_power_of_two()
        || !(2..=4096).contains(&output)
        || !(1000..=6000).contains(&size)
        || land.heights.len() != land.resolution * land.resolution
        || land.biomes.len() != land.heights.len()
        || land
            .heights
            .iter()
            .any(|h| !h.is_finite() || !(-500. ..=500.).contains(h))
    {
        return Err("Invalid landscape raster inputs.".into());
    }
    let n = land.resolution;
    let mut pixels = Vec::with_capacity(output * output);
    let height = |x: f32, z: f32| {
        let x = x.clamp(0., (n - 1) as f32);
        let z = z.clamp(0., (n - 1) as f32);
        let a = x.floor() as usize;
        let b = z.floor() as usize;
        let tx = x - a as f32;
        let tz = z - b as f32;
        land.heights[b * n + a] * (1. - tx) * (1. - tz)
            + land.heights[b * n + (a + 1).min(n - 1)] * tx * (1. - tz)
            + land.heights[(b + 1).min(n - 1) * n + a] * (1. - tx) * tz
            + land.heights[(b + 1).min(n - 1) * n + (a + 1).min(n - 1)] * tx * tz
    };
    let spacing = size as f32 / (n - 1) as f32;
    for row in 0..output {
        if cancelled() {
            return Ok(None);
        }
        let z = (output - 1 - row) as f32 * (n - 1) as f32 / (output - 1) as f32;
        for column in 0..output {
            let x = column as f32 * (n - 1) as f32 / (output - 1) as f32;
            let h = height(x, z);
            let nx = x as f64 / (n - 1) as f64;
            let nz = z as f64 / (n - 1) as f64;
            let mut color = if h <= 0. {
                let deep = mix([75., 132., 139.], [21., 57., 75.], smooth(0., 35., -h));
                mix([155., 187., 174.], deep, smooth(0., 3., -h))
            } else {
                let a = x.floor() as usize;
                let b = z.floor() as usize;
                let tx = x - a as f32;
                let tz = z - b as f32;
                let indices = [
                    b * n + a,
                    b * n + (a + 1).min(n - 1),
                    (b + 1).min(n - 1) * n + a,
                    (b + 1).min(n - 1) * n + (a + 1).min(n - 1),
                ];
                let factors = [
                    (1. - tx) * (1. - tz),
                    tx * (1. - tz),
                    (1. - tx) * tz,
                    tx * tz,
                ];
                let weights: [f32; 4] = std::array::from_fn(|c| {
                    indices
                        .iter()
                        .zip(factors)
                        .map(|(&i, f)| land.biomes[i][c] as f32 * f)
                        .sum()
                });
                let sum: f32 = weights.iter().sum();
                let fine = noise(nx * 190., nz * 190., seed as i32 ^ 0x28b1) as f32;
                let forest = smooth(
                    0.38,
                    0.70,
                    noise(nx * 23., nz * 23., seed as i32 ^ 0x197c) as f32,
                );
                let temperate = mix([128., 141., 94.], [66., 99., 72.], forest * 0.8);
                let palette = [
                    [179., 158., 108.],
                    temperate,
                    [151., 151., 128.],
                    [219., 228., 220.],
                ];
                let mut ground = if sum == 0. {
                    temperate
                } else {
                    std::array::from_fn(|i| {
                        palette
                            .iter()
                            .zip(weights)
                            .map(|(c, w)| c[i] * w / sum)
                            .sum()
                    })
                };
                let gx = (height(x + 1., z) - height(x - 1., z)) / (2. * spacing);
                let gz = (height(x, z + 1.) - height(x, z - 1.)) / (2. * spacing);
                let slope = gx.hypot(gz);
                let rock = mix([123., 126., 115.], [155., 153., 135.], fine);
                ground = mix(ground, rock, smooth(0.22, 0.8, slope) * 0.82);
                ground = mix([193., 180., 139.], ground, smooth(1., 8., h));
                let illumination = (0.76
                    + 0.29 * ((-gx * -0.5 + 0.82 + -gz * 0.28) / (1. + gx * gx + gz * gz).sqrt()))
                .clamp(0.58, 1.10);
                ground.map(|v| v * illumination * (0.96 + fine * 0.08))
            };
            color = color.map(|v| v.clamp(0., 255.));
            pixels.push(Color32::from_rgb(
                color[0] as u8,
                color[1] as u8,
                color[2] as u8,
            ));
        }
    }
    Ok(Some(ColorImage::new([output, output], pixels)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendering_preserves_input_and_north_orientation() {
        let mut land = Landscape {
            resolution: 33,
            heights: vec![-50.; 33 * 33],
            biomes: vec![[0; 4]; 33 * 33],
        };
        for z in 16..33 {
            for x in 0..33 {
                land.heights[z * 33 + x] = 20.;
                land.biomes[z * 33 + x] = [0, 255, 0, 0];
            }
        }
        let before = land.clone();
        let image = render(&land, 4000, 123, 33).unwrap();
        assert_eq!(land, before);
        assert_ne!(image.pixels[16], image.pixels[32 * 33 + 16]);
        assert_eq!(image, render(&land, 4000, 123, 33).unwrap());
        assert_eq!(render(&land, 4000, 123, 129).unwrap().size, [129, 129]);
        assert!(render(&land, 4000, 123, 1).is_err());
    }
}
