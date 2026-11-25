//! Coarse height/slope construction and channel-selective half-precision blur.
//! This is a numeric component, not an installation adapter. Callers must bind
//! the producer/frame, blur shader and sampler to their selected client before
//! using this as an authoritative rendering input.
use half::f16;
use std::sync::Arc;

pub const VERSION: &str = "papyrus-coarse-map-surface-1";
#[derive(Clone, Copy, Debug)]
pub enum HalfRounding {
    NearestEven,
    TowardZero,
}
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub height_resolution: usize,
    pub size_x: f32,
    pub origin_y: f32,
    pub vertical_span: f32,
    pub gradient_gain: f32,
    pub blur_pairs: usize,
    pub half_rounding: HalfRounding,
    pub maximum_work: usize,
    pub maximum_bytes: usize,
}
#[derive(Clone, Debug)]
pub struct Report {
    pub engine: &'static str,
    pub resolution: usize,
    pub source_height_reads: usize,
    pub blur_passes: usize,
    pub work: usize,
    pub working_bytes: usize,
}
/// The half bits remain owned and immutable after successful construction.
#[derive(Clone, Debug)]
pub struct Grid {
    resolution: usize,
    pixels: Arc<Vec<[u16; 2]>>,
    normal_lod: f32,
    pub(crate) normal: Option<crate::map_normal::Grid>,
    pub(crate) shore: Option<Arc<papyrus_terrain::shore_field::Texture>>,
    pub(crate) shore_working_bytes: usize,
    pub report: Report,
}
impl Grid {
    pub(crate) fn normal_lod(&self) -> f32 {
        self.normal_lod
    }
    pub(crate) fn for_footprint(&self, uv_per_pixel: f32) -> Self {
        let mut result = self.clone();
        result.normal_lod = self
            .normal
            .as_ref()
            .map_or(0., |n| n.lod_for_footprint(uv_per_pixel));
        result
    }
    pub fn normal(&self) -> Option<&crate::map_normal::Grid> {
        self.normal.as_ref()
    }
    pub fn shore(&self) -> Option<&papyrus_terrain::shore_field::Texture> {
        self.shore.as_deref()
    }
    pub fn retained_bytes(&self) -> usize {
        self.base_bytes() + self.shore.as_ref().map_or(0, |s| s.retained_bytes())
    }
    fn base_bytes(&self) -> usize {
        self.pixels.len() * 4 + self.normal.as_ref().map_or(0, |n| n.report.pixel_bytes)
    }
    pub fn working_bytes(&self) -> usize {
        self.report
            .working_bytes
            .max(self.pixels.len() * 4 + self.normal.as_ref().map_or(0, |n| n.report.working_bytes))
            .max(self.base_bytes() + self.shore_working_bytes)
    }
    pub fn resolution(&self) -> usize {
        self.resolution
    }
    pub fn pixels(&self) -> &[[u16; 2]] {
        &self.pixels
    }

    /// Bilinear, cell-centered clamp sampling. Never interprets slope as degrees.
    pub fn sample(&self, uv: [f32; 2]) -> Option<[f32; 2]> {
        if uv.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let n = self.resolution;
        let [x, z] = uv.map(|v| (v.clamp(0., 1.) * n as f32 - 0.5).clamp(0., (n - 1) as f32));
        let [a, b] = [x as usize, z as usize];
        let [c, d] = [(a + 1).min(n - 1), (b + 1).min(n - 1)];
        Some(std::array::from_fn(|channel| {
            let at = |x: usize, z: usize| f16::from_bits(self.pixels[z * n + x][channel]).to_f32();
            let lo = at(a, b) + (at(c, b) - at(a, b)) * x.fract();
            let hi = at(a, d) + (at(c, d) - at(a, d)) * x.fract();
            lo + (hi - lo) * z.fract()
        }))
    }
}

pub(crate) fn working_bytes(height_resolution: usize, blur: bool) -> usize {
    let pixels = ((height_resolution - 1) / 2).pow(2) * std::mem::size_of::<[u16; 2]>();
    let scanlines = 3 * height_resolution * std::mem::size_of::<f32>();
    pixels + scanlines.max(if blur { pixels } else { 0 })
}

/// Each output cell averages four overlapping 2x2 blocks. All accumulators are
/// f32 and retain the inspected producer's addition order. Eight blur passes
/// correspond to four horizontal/vertical pairs; every target write rounds to
/// half. Terrain, water and the unfiltered height channel are never rewritten.
pub fn generate(
    height: &[i16],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if cancelled() {
        return Ok(None);
    }
    if settings
        .height_resolution
        .checked_mul(settings.height_resolution)
        != Some(height.len())
    {
        return Err("Invalid coarse map surface dimensions".into());
    }
    generate_sampled(
        |x, z| height.get(z * settings.height_resolution + x).copied(),
        settings,
        cancelled,
    )
}

/// Worker-only construction from immutable encoded vertices, without a full
/// projected height allocation. The reader must return stable values throughout
/// validation and construction, just as an immutable slice does.
pub(crate) fn generate_sampled(
    height: impl Fn(usize, usize) -> Option<i16>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if cancelled() {
        return Ok(None);
    }
    let n = settings.height_resolution;
    // The caller's shared image allowance can exceed this component's buffers.
    // Only the bounded dimensions below determine allocations.
    if !(17..=4097).contains(&n)
        || !(n - 1).is_power_of_two()
        || !settings.size_x.is_finite()
        || !(1. ..=10000.).contains(&settings.size_x)
        || !settings.origin_y.is_finite()
        || settings.origin_y.abs() > 30000.
        || !settings.vertical_span.is_finite()
        || !(1. ..=30000.).contains(&settings.vertical_span)
        || !settings.gradient_gain.is_finite()
        || !(0. ..=4.).contains(&settings.gradient_gain)
        || settings.blur_pairs > 8
        || !(1..=1_000_000_000).contains(&settings.maximum_work)
    {
        return Err(format!(
            "Invalid coarse map surface inputs or limits (resolution {n}, size {}, origin {}, span {}, gradient {}, blur pairs {}, work {}, memory {} bytes)",
            settings.size_x,
            settings.origin_y,
            settings.vertical_span,
            settings.gradient_gain,
            settings.blur_pairs,
            settings.maximum_work,
            settings.maximum_bytes,
        ));
    }
    let resolution = (n - 1) / 2;
    let cells = resolution * resolution;
    let passes = settings.blur_pairs * 2;
    // Each source vertex is validated once. Per cell, allow four sums of
    // four values and one output write. Blur still needs seven slope samples,
    // one center height and one output write.
    let work = n * n + cells * (17 + passes * 9);
    let bytes = working_bytes(n, passes > 0);
    if bytes > settings.maximum_bytes {
        return Err("Terrain shading exceeds the image memory limit. Increase Image memory in Settings.".into());
    }
    if work > settings.maximum_work {
        return Err("Coarse map surface exceeds its work limit".into());
    }
    let read_row = |row: &mut [f32], z| {
        for (x, value) in row.iter_mut().enumerate() {
            *value = height(x, z)
                .filter(|h| (0..=32766).contains(h))
                .map(f32::from)
                .ok_or("Invalid coarse map height encoding")?;
        }
        Ok::<(), String>(())
    };
    let block_inverse = 1.0f32 / 4.;
    let gradient = block_inverse * 3.051944E-5 * settings.gradient_gain;
    let height_scale = block_inverse * settings.vertical_span * 0.25 * 3.051944E-5;
    let ny = settings.size_x / settings.vertical_span / n as f32;
    let mut pixels = vec![[0; 2]; cells];
    let mut rows = [vec![0.; n], vec![0.; n], vec![0.; n]];
    read_row(&mut rows[0], 0)?;
    for (z, output) in pixels.chunks_exact_mut(resolution).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        read_row(&mut rows[1], z * 2 + 1)?;
        read_row(&mut rows[2], z * 2 + 2)?;
        let [top, middle, bottom] = rows.each_ref().map(|row| row.array_windows::<3>().step_by(2));
        for (((pixel, &[t0, t1, t2]), &[m0, m1, m2]), &[b0, b1, b2]) in
            output.iter_mut().zip(top).zip(middle).zip(bottom)
        {
            // Heights are nonnegative integers exactly representable in f32.
            let a = ((t0 + t1) + m0) + m1;
            let b = ((t1 + t2) + m1) + m2;
            let c = ((m0 + m1) + b0) + b1;
            let d = ((m1 + m2) + b1) + b2;
            let elevation = ((b + a) + c + d) * height_scale + settings.origin_y;
            let slope = if gradient == 0. {
                0.
            } else {
                let mut nx = ((d + b) - a - c) * gradient;
                let mut nz = ((d + c) - a - b) * gradient;
                let length = ((nx * nx + ny * ny) + nz * nz).sqrt();
                if length > 1E-5 {
                    nx /= length;
                    nz /= length;
                    nx * nx + nz * nz
                } else {
                    0.
                }
            };
            *pixel = [
                encode(elevation, settings.half_rounding),
                encode(slope, settings.half_rounding),
            ];
        }
        rows.swap(0, 2);
    }
    drop(rows);
    let mut temporary = if passes > 0 {
        vec![[0; 2]; cells]
    } else {
        Vec::new()
    };
    for pass in 0..passes {
        if !blur(
            &pixels,
            &mut temporary,
            resolution,
            pass % 2 == 0,
            settings.half_rounding,
            &cancelled,
        ) {
            return Ok(None);
        }
        std::mem::swap(&mut pixels, &mut temporary);
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Grid {
        resolution,
        pixels: Arc::new(pixels),
        normal_lod: 0.,
        normal: None,
        shore: None,
        shore_working_bytes: 0,
        report: Report {
            engine: VERSION,
            resolution,
            source_height_reads: n * n,
            blur_passes: passes,
            work,
            working_bytes: bytes,
        },
    }))
}

fn blur(
    source: &[[u16; 2]],
    output: &mut [[u16; 2]],
    n: usize,
    horizontal: bool,
    rounding: HalfRounding,
    cancelled: &impl Fn() -> bool,
) -> bool {
    for z in 0..n {
        if cancelled() {
            return false;
        }
        for x in 0..n {
            let at = |offset: isize| {
                let (xx, zz) = if horizontal {
                    (x.saturating_add_signed(offset).min(n - 1), z)
                } else {
                    (x, z.saturating_add_signed(offset).min(n - 1))
                };
                f16::from_bits(source[zz * n + xx][1]).to_f32()
            };
            let center = at(0);
            // The captured WARP program evaluates mad as separately rounded
            // f32 multiply/add. Fusing it changes some half-target results.
            let mut value = center * 0.4 + at(1) * 0.15;
            value += at(-1) * 0.15;
            value += at(2) * 0.1;
            value += at(-2) * 0.1;
            value += at(3) * 0.05;
            value += at(-3) * 0.05;
            // The selected blur pass changes green only. Preserve its final
            // subtract/add too; rounding can matter near a half boundary.
            let value = (value - center) + center;
            output[z * n + x] = [source[z * n + x][0], encode(value, rounding)];
        }
    }
    true
}

fn encode(value: f32, rounding: HalfRounding) -> u16 {
    let mut bits = f16::from_f32(value).to_bits();
    if matches!(rounding, HalfRounding::TowardZero)
        && f16::from_bits(bits).to_f32().abs() > value.abs()
    {
        bits -= 1;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn half_conversion_and_every_blur_pass_match_the_owned_shader_probe() {
        #[derive(serde::Deserialize)]
        struct Case {
            resolution: usize,
            input: Vec<[f32; 2]>,
            passes: Vec<Vec<[u16; 2]>>,
        }
        #[derive(serde::Deserialize)]
        struct Probe {
            format: String,
            schema: u32,
            pixel_program_sha256: String,
            rows: Vec<Case>,
        }
        let probe: Probe = serde_json::from_str(include_str!(
            "../tests/fixtures/client-map-surface-blur-e7156192.json"
        ))
        .unwrap();
        assert_eq!(probe.format, "mapper-owned-green-blur-half-probe");
        assert_eq!(probe.schema, 1);
        assert_eq!(
            probe.pixel_program_sha256,
            "88237ae5590ff0d758c9e286ceb65a211bee53225619fcd3471cd0234f07aa5a"
        );
        assert_eq!(probe.rows.len(), 5);
        for (case_id, case) in probe.rows.into_iter().enumerate() {
            assert_eq!(case.input.len(), case.resolution * case.resolution);
            assert_eq!(case.passes.len(), 9);
            let mut actual: Vec<_> = case
                .input
                .iter()
                .map(|v| v.map(|v| encode(v, HalfRounding::TowardZero)))
                .collect();
            assert_eq!(
                actual, case.passes[0],
                "initial half target pattern {case_id}"
            );
            let mut temporary = vec![[0; 2]; actual.len()];
            for pass in 0..8 {
                assert!(blur(
                    &actual,
                    &mut temporary,
                    case.resolution,
                    pass % 2 == 0,
                    HalfRounding::TowardZero,
                    &|| false
                ));
                std::mem::swap(&mut actual, &mut temporary);
                assert_eq!(
                    actual,
                    case.passes[pass + 1],
                    "pattern {case_id}, pass {pass}"
                );
                assert!(
                    actual
                        .iter()
                        .zip(&case.passes[0])
                        .all(|(a, b)| a[0] == b[0])
                );
            }
        }
    }
    fn settings() -> Settings {
        Settings {
            height_resolution: 17,
            size_x: 1000.,
            origin_y: -500.,
            vertical_span: 1000.,
            gradient_gain: 0.375,
            blur_pairs: 4,
            half_rounding: HalfRounding::TowardZero,
            maximum_work: 1_000_000,
            maximum_bytes: 1_000_000,
        }
    }
    #[test]
    fn flat_and_planar_fields_have_registered_heights_and_filtered_slopes() {
        let p = settings();
        let h = vec![16000; 17 * 17];
        let g = generate(&h, p, || false).unwrap().unwrap();
        assert_eq!(g.resolution(), 8);
        let expected = encode(
            16000. * 3.051944E-5 * 1000. - 500.,
            HalfRounding::TowardZero,
        );
        assert!(g.pixels.iter().all(|v| *v == [expected, 0]));
        assert_eq!(g.sample([-2., 3.]), g.sample([0., 1.]));
        assert!(g.sample([f32::NAN, 0.]).is_none());
        let h: Vec<_> = (0..17 * 17)
            .map(|i| (12000 + i % 17 * 321 + i / 17 * 171) as i16)
            .collect();
        let mut raw = p;
        raw.blur_pairs = 0;
        let unfiltered = generate(&h, raw, || false).unwrap().unwrap();
        let filtered = generate(&h, p, || false).unwrap().unwrap();
        assert!(
            unfiltered
                .pixels
                .iter()
                .all(|v| v[1] == unfiltered.pixels[0][1])
        );
        assert!(
            unfiltered
                .pixels
                .iter()
                .zip(filtered.pixels.iter())
                .all(|(a, b)| a[0] == b[0])
        );
        let nx = 321.0 * 0.75 * 3.051944E-5;
        let nz = 171.0 * 0.75 * 3.051944E-5;
        let ny = 1.0f32 / 17.;
        let expected_slope = (nx * nx + nz * nz) / (nx * nx + ny * ny + nz * nz);
        assert_eq!(
            unfiltered.pixels[0][1],
            encode(expected_slope, HalfRounding::TowardZero)
        );
        for (x, z) in [(0, 0), (3, 5), (7, 7)] {
            let mean = 12000. + 321. * (2 * x + 1) as f64 + 171. * (2 * z + 1) as f64;
            let expected = mean / 32766. * 1000. - 500.;
            let actual = unfiltered
                .sample([(x as f32 + 0.5) / 8., (z as f32 + 0.5) / 8.])
                .unwrap()[0];
            assert!((actual as f64 - expected).abs() <= expected.abs() / 1024. + 0.0001);
        }
        assert!(unfiltered.pixels[0][1] > 0);
        assert!(filtered.pixels.windows(2).any(|v| v[0][0] != v[1][0]));
    }
    #[test]
    fn impulse_preserves_height_channel_and_rejects_limits_invalid_data_and_cancellation() {
        let p = settings();
        let mut h = vec![16000; 17 * 17];
        h[8 * 17 + 8] = 30000;
        let original = h.clone();
        let mut raw = p;
        raw.blur_pairs = 0;
        let base = generate(&h, raw, || false).unwrap().unwrap();
        let output = generate(&h, p, || false).unwrap().unwrap();
        assert_eq!(h, original);
        assert!(
            base.pixels
                .iter()
                .zip(output.pixels.iter())
                .all(|(a, b)| a[0] == b[0])
        );
        assert_ne!(base.pixels, output.pixels);
        let mut limited = p;
        limited.maximum_work = output.report.work - 1;
        assert!(generate(&h, limited, || false).is_err());
        limited = p;
        limited.maximum_bytes = output.report.working_bytes - 1;
        assert!(generate(&h, limited, || false).is_err());
        limited.maximum_bytes = 0;
        assert!(generate(&h, limited, || false).unwrap_err().contains("Image memory in Settings"));
        assert!(generate(&h, p, || true).unwrap().is_none());
        let calls = std::cell::Cell::new(0);
        assert!(
            generate(&h, p, || {
                calls.set(calls.get() + 1);
                calls.get() > 15
            })
            .unwrap()
            .is_none()
        );
        h[0] = -1;
        assert!(generate(&h, p, || false).is_err());
        h[0] = 32767;
        assert!(generate(&h, p, || false).is_err());
        assert!(generate(&h[..12], p, || false).is_err());
        for gain in [f32::NAN, -1., 5.] {
            let mut bad = p;
            bad.gradient_gain = gain;
            assert!(generate(&original, bad, || false).is_err());
        }
    }
    #[test]
    fn lazy_encoded_vertices_match_slice_words_and_reject_missing_or_cancelled_inputs() {
        let settings = settings();
        let height: Vec<_> = (0..17 * 17)
            .map(|i| (9000 + (i * 193) % 19000) as i16)
            .collect();
        let slice = generate(&height, settings, || false).unwrap().unwrap();
        let reads = std::cell::Cell::new(0);
        let sampled = generate_sampled(|x, z| {
            assert_eq!(reads.get(), z * 17 + x);
            reads.set(reads.get() + 1);
            Some(height[z * 17 + x])
        }, settings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(slice.pixels(), sampled.pixels());
        assert_eq!(reads.get(), height.len());
        assert_eq!(sampled.report.source_height_reads, height.len());
        assert_eq!(slice.report.work, sampled.report.work);
        assert_eq!(slice.report.working_bytes, sampled.report.working_bytes);
        assert!(
            generate_sampled(
                |x, z| (x != 8 || z != 8).then_some(height[z * 17 + x]),
                settings,
                || false
            )
            .is_err()
        );
        reads.set(0);
        assert!(
            generate_sampled(
                |x, z| {
                    reads.set(reads.get() + 1);
                    Some(height[z * 17 + x])
                },
                settings,
                || reads.get() >= 3 * 17
            )
            .unwrap()
            .is_none()
        );
    }
}
