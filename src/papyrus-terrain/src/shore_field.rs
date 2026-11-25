//! Inspected bd19c4ac managed shoreline assembly and distance/blur/vector primitives.
//! World-space provider queries and selected-build compatibility belong to the
//! stage owner; this module does not activate an installation profile.
pub const VERSION: &str = "papyrus-shore-field-bd19c4ac-1";
const OFFSETS: [isize; 7] = [-6, -4, -2, 0, 2, 4, 6];
const WEIGHTS: [f32; 7] = [
    1. / 32.,
    7. / 64.,
    7. / 32.,
    9. / 32.,
    7. / 32.,
    7. / 64.,
    1. / 32.,
];

#[derive(Clone, Copy)]
pub struct Limits {
    pub maximum_bytes: u64,
    /// Conservative cell/neighbor/tap processing allowance, checked before allocation.
    pub maximum_work: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            maximum_bytes: 256 * 1024 * 1024,
            maximum_work: 512 * 1024 * 1024,
        }
    }
}

/// Topology queries belong to the selected installation's terrain provider.
/// Mainland masks must already include its height-dependent radius query.
pub enum Topology<'a> {
    Mainland(&'a [i32]),
    DeepSea,
    Unavailable,
}

/// Assemble the inspected bitmap, distance/blur/vector stages and water-type
/// lane from cell-center provider samples. Height and water slices are world
/// metres in matching row order. Borrowed inputs are outside this allowance.
/// Installation binding and provider sampling remain the caller's responsibility.
pub fn generate_classified(
    resolution: usize,
    heights: &[f32],
    water_levels: &[f32],
    topology: Topology<'_>,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Field>, String> {
    if cancelled() {
        return Ok(None);
    }
    let (bytes, work) = bounds(resolution, 1)?;
    let cells = resolution * resolution;
    if heights.len() != cells
        || water_levels.len() != cells
        || matches!(&topology, Topology::Mainland(masks) if masks.len() != cells)
    {
        return Err("Shoreline provider sample dimensions differ".into());
    }
    // The bitmap remains borrowed by generate while all distance scratch
    // buffers exist. Reserve its entire allocation before constructing it.
    let bitmap_bytes = cells as u64;
    let classification_work = 5 * cells as u64;
    if bytes + bitmap_bytes > limits.maximum_bytes
        || work + classification_work > limits.maximum_work
    {
        return Err("Classified shoreline exceeds generation allowance".into());
    }
    let mut bitmap = Vec::with_capacity(cells);
    for y in 0..resolution {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..resolution {
            let i = y * resolution + x;
            if !heights[i].is_finite() || !water_levels[i].is_finite() {
                return Err("Shoreline provider returned a nonfinite height".into());
            }
            bitmap.push(if (water_levels[i] - heights[i]).max(0.) <= 0. {
                255
            } else {
                0
            });
        }
    }
    let Some(mut field) = generate(
        resolution,
        127,
        &bitmap,
        1,
        Limits {
            maximum_bytes: limits.maximum_bytes - bitmap_bytes,
            maximum_work: limits.maximum_work - classification_work,
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    drop(bitmap);
    for y in 0..resolution {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..resolution {
            let i = y * resolution + x;
            field.vectors[i][3] = match &topology {
                Topology::Mainland(masks) => {
                    let mask = masks[i];
                    if mask & 384 != 0 {
                        1.
                    } else if mask & 204800 != 0 {
                        2.
                    } else if mask & 49152 != 0 {
                        3.
                    } else {
                        field.vectors[i][3]
                    }
                }
                Topology::DeepSea => 1.,
                Topology::Unavailable => -1.,
            };
        }
    }
    Ok((!cancelled()).then_some(field))
}

pub struct Field {
    pub resolution: usize,
    /// Signed distances in grid cells: dry negative, wet positive.
    pub distances: Vec<f32>,
    /// Source Sobel direction, signed distance and unassigned topology channel.
    /// Edge vectors (including their distance) copy the adjacent interior row/column.
    pub vectors: Vec<[f32; 4]>,
    pub boundary_seeds: usize,
}
impl Field {
    /// PopulateTextureDataJob packs the separate distance array, rather than
    /// vectors.z, whose edge values were copied from the interior. Topology
    /// tagging must already have populated vectors.w before this upload.
    /// The caller accounts for the borrowed field separately from this allowance.
    pub fn texture(
        &self,
        maximum_bytes: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Texture>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = self.resolution;
        if !(4..=2048).contains(&n) || self.distances.len() != n * n || self.vectors.len() != n * n
        {
            return Err("Invalid shoreline texture dimensions".into());
        }
        if (n * n * 8) as u64 > maximum_bytes {
            return Err("Shoreline texture exceeds its memory allowance".into());
        }
        let mut texels = Vec::with_capacity(n * n);
        for y in 0..n {
            if cancelled() {
                return Ok(None);
            }
            for x in 0..n {
                let i = y * n + x;
                let [dx, dz, _, kind] = self.vectors[i];
                let channels = [dx, dz, self.distances[i], kind];
                // Preserve source no-boundary infinities/NaNs in the upload.
                // Finite overflow conversion remains outside the verified range.
                if channels.iter().any(|v| v.is_finite() && v.abs() > 65504.) {
                    return Err("Shoreline texture contains oversized finite values".into());
                }
                texels.push(channels.map(source_half));
            }
        }
        Ok((!cancelled()).then_some(Texture {
            resolution: n,
            texels,
        }))
    }

    /// Source interior-UV bilinear vector query, without renormalizing direction.
    /// No-boundary/nonfinite results remain unavailable, never fabricated as zero.
    pub fn sample(&self, uv: [f32; 2], metres_per_cell: f32) -> Option<[f32; 3]> {
        let n = self.resolution;
        if !(4..=2048).contains(&n)
            || self.vectors.len() != n * n
            || uv.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            || !metres_per_cell.is_finite()
            || !(0. ..=10000.).contains(&metres_per_cell)
            || metres_per_cell == 0.
        {
            return None;
        }
        let [x, y] = uv.map(|v| v * (n - 1) as f32);
        let [a, b] = [x as usize, y as usize];
        let c = (a + 1).min(n - 1);
        let d = (b + 1).min(n - 1);
        let query = std::array::from_fn(|channel| {
            let v = |x: usize, y: usize| self.vectors[y * n + x][channel];
            let low = (v(c, b) - v(a, b)) * x.fract() + v(a, b);
            let high = (v(c, d) - v(a, d)) * x.fract() + v(a, d);
            (high - low) * y.fract() + low
        });
        let [dx, dz, distance]: [f32; 3] = query;
        let result = [dx, dz, distance * metres_per_cell];
        result.iter().all(|v| v.is_finite()).then_some(result)
    }
}

/// Linear, clamped RGBAHalf shoreline input with one mip, matching the inspected
/// upload. This is separate from Field::sample, which is a CPU gameplay query.
#[derive(Debug)]
pub struct Texture {
    resolution: usize,
    texels: Vec<[u16; 4]>,
}
impl Texture {
    pub fn resolution(&self) -> usize {
        self.resolution
    }
    pub fn texels(&self) -> &[[u16; 4]] {
        &self.texels
    }
    pub fn retained_bytes(&self) -> usize {
        self.texels.capacity() * std::mem::size_of::<[u16; 4]>()
    }
    /// Explicit LOD-zero sampling uses texel centers. Distance remains in cells,
    /// and the topology lane is interpolated without rounding to an integer.
    pub fn sample(&self, uv: [f32; 2]) -> Option<[f32; 4]> {
        if uv.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v)) {
            return None;
        }
        let n = self.resolution;
        let [x, y] = uv.map(|v| (v * n as f32 - 0.5).clamp(0., (n - 1) as f32));
        let [a, b] = [x as usize, y as usize];
        let c = (a + 1).min(n - 1);
        let d = (b + 1).min(n - 1);
        Some(std::array::from_fn(|channel| {
            let v = |x: usize, y: usize| decode_half(self.texels[y * n + x][channel]);
            let low = (v(c, b) - v(a, b)) * x.fract() + v(a, b);
            let high = (v(c, d) - v(a, d)) * x.fract() + v(a, d);
            (high - low) * y.fract() + low
        }))
    }
}

// Unity.Mathematics f32tof16 over representable finite and source nonfinite
// values. Masking before scaling is significant at midpoints.
fn source_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let absolute = bits & 0x7fff_f000;
    if absolute >= 0x7f80_0000 {
        return ((bits >> 16) & 0x8000) as u16
            | if absolute > 0x7f80_0000 {
                0x7e00
            } else {
                0x7c00
            };
    }
    let magnitude = f32::from_bits(absolute) * f32::from_bits(0x0780_0000);
    (((magnitude.to_bits() + 0x1000) >> 13) | ((bits >> 16) & 0x8000)) as u16
}

fn decode_half(bits: u16) -> f32 {
    let sign = (u32::from(bits) & 0x8000) << 16;
    let exponent = (u32::from(bits) >> 10) & 31;
    let fraction = u32::from(bits) & 1023;
    let value = if exponent == 0 {
        fraction as f32 * (1. / 16_777_216.)
    } else if exponent == 31 {
        f32::from_bits(0x7f80_0000 | (fraction << 13))
    } else {
        f32::from_bits(((exponent + 112) << 23) | (fraction << 13))
    };
    f32::from_bits(value.to_bits() | sign)
}

/// A worst-case bound retaining all buffers simultaneously; excludes the borrowed bitmap.
pub fn bounds(resolution: usize, blur_steps: usize) -> Result<(u64, u64), String> {
    if !(4..=2048).contains(&resolution) || blur_steps > 4 {
        return Err("Unsupported shoreline field dimensions/blur count".into());
    }
    let n = resolution as u64;
    let cells = n * n;
    Ok((
        12 * (n + 2) * (n + 2) + 24 * cells,
        cells * (32 + 14 * blur_steps as u64) + 4 * n,
    ))
}

/// Ordered two-sweep source scan, not an ideal Euclidean distance transform.
/// Uniform/edge-only inputs may yield infinities/NaNs just as the source does.
/// Raw diagnostic arrays retain these; `sample` refuses nonfinite queries.
/// Cancellation or rejected limits publish no partial field and never alter input.
pub fn generate(
    resolution: usize,
    threshold: u8,
    image: &[u8],
    blur_steps: usize,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Field>, String> {
    if cancelled() {
        return Ok(None);
    }
    let (bytes, work) = bounds(resolution, blur_steps)?;
    let n = resolution;
    let cells = n * n;
    if image.len() != cells {
        return Err("Shoreline bitmap dimensions differ".into());
    }
    if bytes > limits.maximum_bytes || work > limits.maximum_work {
        return Err("Shoreline field exceeds generation allowance".into());
    }
    let p = n + 2;
    let mut nearest = vec![[-1_i32; 2]; p * p];
    let mut grid = vec![f32::INFINITY; p * p];
    let mut boundary_seeds = 0;
    for y in 1..n - 2 {
        if cancelled() {
            return Ok(None);
        }
        for x in 1..n - 2 {
            let i = y * n + x;
            if image[i] > threshold
                && [i - 1, i + 1, i - n, i + n]
                    .iter()
                    .any(|&i| image[i] <= threshold)
            {
                let at = (y + 1) * p + x + 1;
                nearest[at] = [x as i32 + 1, y as i32 + 1];
                grid[at] = 0.;
                boundary_seeds += 1;
            }
        }
    }
    let relax = |i: usize,
                 x: usize,
                 y: usize,
                 candidates: &[(usize, f32)],
                 nearest: &mut [[i32; 2]],
                 grid: &mut [f32]| {
        let mut distance = grid[i];
        for &(other, step) in candidates {
            if grid[other] + step < distance {
                nearest[i] = nearest[other];
                let dx = x as f32 - nearest[i][0] as f32;
                let dy = y as f32 - nearest[i][1] as f32;
                distance = (dx * dx + dy * dy).sqrt();
                grid[i] = distance;
            }
        }
    };
    #[allow(clippy::approx_constant)]
    const DIAGONAL: f32 = 1.414_213_5;
    for y in 1..p - 1 {
        if cancelled() {
            return Ok(None);
        }
        for x in 1..p - 1 {
            let i = y * p + x;
            relax(
                i,
                x,
                y,
                &[
                    (i - p - 1, DIAGONAL),
                    (i - p, 1.),
                    (i - p + 1, DIAGONAL),
                    (i - 1, 1.),
                ],
                &mut nearest,
                &mut grid,
            );
        }
    }
    for y in (1..p - 1).rev() {
        if cancelled() {
            return Ok(None);
        }
        for x in (1..p - 1).rev() {
            let i = y * p + x;
            // The inspected final lower-right comparison uses 1, not sqrt(2).
            relax(
                i,
                x,
                y,
                &[
                    (i + 1, 1.),
                    (i + p - 1, DIAGONAL),
                    (i + p, 1.),
                    (i + p + 1, 1.),
                ],
                &mut nearest,
                &mut grid,
            );
        }
    }
    let mut distances = vec![0.; cells];
    for y in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            let i = y * n + x;
            let value = grid[(y + 1) * p + x + 1];
            distances[i] = if image[i] > threshold { -value } else { value };
        }
    }
    drop(nearest);
    drop(grid);
    if blur_steps > 0 {
        let mut scratch = vec![0.; cells];
        for _ in 0..blur_steps {
            for y in 0..n {
                if cancelled() {
                    return Ok(None);
                }
                for x in 0..n {
                    let mut sum = 0.;
                    for (offset, weight) in OFFSETS.into_iter().zip(WEIGHTS) {
                        let at = (x as isize + offset).clamp(0, n as isize - 1) as usize;
                        sum += distances[y * n + at] * weight;
                    }
                    scratch[y * n + x] = sum;
                }
            }
            for y in 0..n {
                if cancelled() {
                    return Ok(None);
                }
                for x in 0..n {
                    let mut sum = 0.;
                    for (offset, weight) in OFFSETS.into_iter().zip(WEIGHTS) {
                        let at = (y as isize + offset).clamp(0, n as isize - 1) as usize;
                        sum += scratch[at * n + x] * weight;
                    }
                    distances[y * n + x] = sum;
                }
            }
        }
    }
    let mut vectors = vec![[0.; 4]; cells];
    for y in 1..n - 1 {
        if cancelled() {
            return Ok(None);
        }
        for x in 1..n - 1 {
            let at = |x, y| distances[y * n + x];
            let dx = at(x + 1, y - 1) + 2. * at(x + 1, y) + at(x + 1, y + 1)
                - (at(x - 1, y - 1) + 2. * at(x - 1, y) + at(x - 1, y + 1));
            let dy = at(x - 1, y + 1) + 2. * at(x, y + 1) + at(x + 1, y + 1)
                - (at(x - 1, y - 1) + 2. * at(x, y - 1) + at(x + 1, y - 1));
            let dx = -dx;
            let dy = -dy;
            let magnitude = (dx * dx + dy * dy).sqrt();
            let [dx, dy] = if magnitude > 0.00001 {
                [dx / magnitude, dy / magnitude]
            } else {
                [0., 0.]
            };
            vectors[y * n + x] = [dx, dy, at(x, y), 0.];
        }
    }
    for x in 1..n - 1 {
        vectors[x] = vectors[n + x];
        vectors[(n - 1) * n + x] = vectors[(n - 2) * n + x];
    }
    for y in 0..n {
        vectors[y * n] = vectors[y * n + 1];
        vectors[y * n + n - 1] = vectors[y * n + n - 2];
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Field {
        resolution: n,
        distances,
        vectors,
        boundary_seeds,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classified_bitmap_and_ordered_topology_match_the_inspected_jobs() {
        let heights: Vec<_> = (0..64).map(|i| if i % 8 < 4 { 2. } else { -2. }).collect();
        let water = vec![0.; 64];
        let bits = [128, 256, 8192, 65536, 131072, 16384];
        let masks: Vec<i32> = (0..64)
            .map(|i| {
                bits.iter()
                    .enumerate()
                    .filter(|(b, _)| i & (1 << b) != 0)
                    .fold(1 << 29, |mask, (_, bit)| mask | bit)
            })
            .collect();
        let bitmap: Vec<_> = heights
            .iter()
            .map(|h| if *h >= 0. { 255 } else { 0 })
            .collect();
        let raw = generate(8, 127, &bitmap, 1, Limits::default(), || false)
            .unwrap()
            .unwrap();
        let field = generate_classified(
            8,
            &heights,
            &water,
            Topology::Mainland(&masks),
            Limits::default(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(field.distances, raw.distances);
        for (i, mask) in masks.iter().enumerate() {
            assert_eq!(&field.vectors[i][..3], &raw.vectors[i][..3]);
            let expected = if i & 3 != 0 {
                1.
            } else if i & 28 != 0 {
                2.
            } else if i & 32 != 0 {
                3.
            } else {
                0.
            };
            assert_eq!(field.vectors[i][3], expected, "mask {mask}");
        }
        let river = [32768; 64];
        for (topology, expected) in [
            (Topology::DeepSea, 1.),
            (Topology::Unavailable, -1.),
            (Topology::Mainland(&river), 3.),
        ] {
            let field =
                generate_classified(8, &heights, &water, topology, Limits::default(), || false)
                    .unwrap()
                    .unwrap();
            assert_eq!(field.distances, raw.distances);
            assert!(field.vectors.iter().all(|v| v[3] == expected));
            assert!(field.texture(512, || false).unwrap().is_some());
        }
        // Equal water/ground heights stay dry, including signed zero.
        let field = generate_classified(
            8,
            &[-0.; 64],
            &[0.; 64],
            Topology::DeepSea,
            Limits::default(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(field.boundary_seeds, 0);
        let texture = field.texture(512, || false).unwrap().unwrap();
        assert!(texture.sample([0.5, 0.5]).unwrap()[2].is_nan());
    }

    #[test]
    fn classified_shoreline_accounts_for_bitmap_and_cancels_without_mutation() {
        let (bytes, work) = bounds(8, 1).unwrap();
        let limits = Limits {
            maximum_bytes: bytes + 64,
            maximum_work: work + 320,
        };
        let heights = [1.; 64];
        let water = [0.; 64];
        let calls = std::cell::Cell::new(0);
        assert!(
            generate_classified(8, &heights, &water, Topology::Unavailable, limits, || {
                calls.set(calls.get() + 1);
                false
            })
            .unwrap()
            .is_some()
        );
        let total = calls.get();
        for after in [0, 1, total / 2, total - 2, total - 1] {
            calls.set(0);
            assert!(
                generate_classified(8, &heights, &water, Topology::Unavailable, limits, || {
                    calls.set(calls.get() + 1);
                    calls.get() > after
                })
                .unwrap()
                .is_none()
            );
        }
        for limits in [
            Limits {
                maximum_bytes: limits.maximum_bytes - 1,
                ..limits
            },
            Limits {
                maximum_work: limits.maximum_work - 1,
                ..limits
            },
        ] {
            assert!(
                generate_classified(8, &heights, &water, Topology::Unavailable, limits, || false)
                    .is_err()
            );
        }
        assert!(
            generate_classified(usize::MAX, &[], &[], Topology::Unavailable, limits, || {
                false
            })
            .is_err()
        );
        assert!(
            generate_classified(
                8,
                &heights,
                &water[..63],
                Topology::Unavailable,
                limits,
                || false
            )
            .is_err()
        );
        assert!(
            generate_classified(
                8,
                &heights,
                &water,
                Topology::Mainland(&[0; 63]),
                limits,
                || false
            )
            .is_err()
        );
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bad = heights;
            bad[63] = value;
            assert!(
                generate_classified(8, &bad, &water, Topology::Unavailable, limits, || false)
                    .is_err()
            );
            assert!(
                generate_classified(8, &heights, &bad, Topology::Unavailable, limits, || false)
                    .is_err()
            );
        }
        assert_eq!(heights, [1.; 64]);
        assert_eq!(water, [0.; 64]);
    }

    #[test]
    fn texture_half_conversion_matches_owned_managed_reference() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/shore-half-managed-reference-e42e77b1.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], "mapper-owned-shore-half-reference");
        assert_eq!(fixture["schema"], 1);
        assert_eq!(
            fixture["source"]["sha256"],
            "e42e77b1a486d5d49090bae44abc2dc6f2662b6127c7d3f1a026f8264b6821fd"
        );
        let rows = fixture["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2821);
        let mut compared = 0;
        let mut differing_rounding = 0;
        for row in rows {
            let value = f32::from_bits(row["input_bits"].as_u64().unwrap() as u32);
            if !value.is_finite() || value.abs() <= 65504. {
                let actual = source_half(value);
                assert_eq!(
                    u64::from(actual),
                    row["result"].as_u64().unwrap(),
                    "{value}"
                );
                differing_rounding += usize::from(row["result"] != row["nearest_even"]);
                assert_eq!(decode_half(actual).is_finite(), value.is_finite());
                compared += 1;
            }
        }
        assert_eq!(compared, 2821);
        assert_eq!(differing_rounding, 282);
        assert_eq!(decode_half(source_half(-0.)).to_bits(), (-0_f32).to_bits());
        for bits in 0_u16..=0x7bff {
            let value = decode_half(bits);
            assert_eq!(source_half(value), bits);
            assert_eq!(source_half(-value), bits | 0x8000);
        }
    }

    fn texture_field() -> Field {
        Field {
            resolution: 4,
            distances: (0..16).map(|i| i as f32).collect(),
            vectors: (0..16)
                .map(|i| [0.5, -0.5, 9998., (i % 4) as f32])
                .collect(),
            boundary_seeds: 1,
        }
    }

    #[test]
    fn texture_upload_uses_separate_distances_and_centered_clamped_filtering() {
        let field = texture_field();
        let texture = field.texture(128, || false).unwrap().unwrap();
        assert_eq!(texture.resolution(), 4);
        assert_eq!(texture.retained_bytes(), 128);
        for (i, texel) in texture.texels().iter().enumerate() {
            assert_eq!(
                texel.map(decode_half),
                [0.5, -0.5, i as f32, (i % 4) as f32]
            );
        }
        assert_eq!(texture.sample([0., 0.]), Some([0.5, -0.5, 0., 0.]));
        assert_eq!(texture.sample([1., 1.]), Some([0.5, -0.5, 15., 3.]));
        assert_eq!(texture.sample([0.125, 0.125]), Some([0.5, -0.5, 0., 0.]));
        assert_eq!(texture.sample([0.25, 0.25]), Some([0.5, -0.5, 2.5, 0.5]));
        for uv in [[f32::NAN, 0.], [-0.1, 0.], [0., 1.1]] {
            assert!(texture.sample(uv).is_none());
        }
    }

    #[test]
    fn rejected_or_cancelled_texture_does_not_change_its_field() {
        let mut field = texture_field();
        assert!(field.texture(127, || false).is_err());
        for after in 0..=5 {
            let calls = std::cell::Cell::new(0);
            assert!(
                field
                    .texture(128, || {
                        calls.set(calls.get() + 1);
                        calls.get() > after
                    })
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(field.vectors[0], [0.5, -0.5, 9998., 0.]);
        assert_eq!(field.distances[15], 15.);
        for value in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 65536.] {
            field.distances[15] = value;
            assert_eq!(field.texture(128, || false).is_err(), value.is_finite());
            field.distances[15] = 15.;
            field.vectors[15][3] = value;
            assert_eq!(field.texture(128, || false).is_err(), value.is_finite());
            field.vectors[15][3] = 3.;
        }
        field.vectors.pop();
        assert!(field.texture(128, || false).is_err());
    }

    #[test]
    fn owned_managed_distance_blur_and_vector_reference() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/shore-field-bd19c4ac.json"))
                .unwrap();
        assert_eq!(fixture["format"], "papyrus-owned-shore-field-reference");
        assert_eq!(fixture["schema"], 1);
        assert_eq!(
            fixture["sources"]["Assembly-CSharp.dll"],
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(
            fixture["sources"]["Rust.Global.dll"],
            "44dd8a1323b12d7dd55aece202d8f2a587c75bd825cd218509f47c326cc409f0"
        );
        assert_eq!(
            fixture["sources"]["UnityEngine.CoreModule.dll"],
            "9bf9488b3f60fec06c45ff049ebb489e40e4d401565ff92237c411e2b8ae19b2"
        );
        let cases = fixture["rows"].as_array().unwrap();
        assert_eq!(cases.len(), 72);
        for (ordinal, c) in cases.iter().enumerate() {
            let image: Vec<u8> = serde_json::from_value(c["image"].clone()).unwrap();
            let field = generate(
                c["resolution"].as_u64().unwrap() as usize,
                127,
                &image,
                c["blur_steps"].as_u64().unwrap() as usize,
                Limits::default(),
                || false,
            )
            .unwrap()
            .unwrap();
            let expected: Vec<u32> = serde_json::from_value(c["distances_bits"].clone()).unwrap();
            assert_eq!(field.distances.len(), expected.len());
            for (i, (&actual, &expected)) in field.distances.iter().zip(&expected).enumerate() {
                assert!(
                    actual.to_bits() == expected
                        || (actual.is_nan() && f32::from_bits(expected).is_nan()),
                    "Distance case {ordinal} cell {i}: {actual} vs {}",
                    f32::from_bits(expected)
                );
            }
            let vectors: Vec<[u32; 4]> = serde_json::from_value(c["vectors_bits"].clone()).unwrap();
            assert_eq!(field.vectors.len(), vectors.len());
            for (i, (actual, expected)) in field.vectors.iter().zip(vectors).enumerate() {
                for k in 0..4 {
                    assert!(
                        actual[k].to_bits() == expected[k]
                            || (actual[k].is_nan() && f32::from_bits(expected[k]).is_nan()),
                        "Vector case {ordinal} cell {i} channel {k}: {} vs {}",
                        actual[k],
                        f32::from_bits(expected[k])
                    );
                }
            }
            let texture = field.texture(u64::MAX, || false).unwrap().unwrap();
            for (i, texel) in texture.texels().iter().enumerate() {
                let source = field.vectors[i];
                assert_eq!(
                    *texel,
                    [source[0], source[1], field.distances[i], source[3]].map(source_half)
                );
            }
            let queries = c["queries"].as_array().unwrap();
            assert_eq!(queries.len(), 18);
            for query in queries {
                let uv: [f32; 2] = serde_json::from_value(query["uv"].clone()).unwrap();
                let expected: [u32; 3] =
                    serde_json::from_value(query["values_bits"].clone()).unwrap();
                let actual = field.sample(uv, query["metres_per_cell"].as_f64().unwrap() as f32);
                if expected.iter().any(|v| !f32::from_bits(*v).is_finite()) {
                    assert!(actual.is_none());
                } else {
                    assert_eq!(
                        actual.unwrap().map(f32::to_bits),
                        expected,
                        "Shore query case {ordinal}: {query}"
                    );
                }
            }
        }
    }
    #[test]
    fn limits_cancellation_and_unavailable_shore_never_publish_a_partial_result() {
        assert!(bounds(usize::MAX, 1).is_err());
        assert!(bounds(0, 0).is_err());
        assert!(bounds(32, 5).is_err());
        assert!(generate(8, 127, &[0; 63], 1, Limits::default(), || false).is_err());
        let limits = Limits {
            maximum_bytes: 1,
            ..Limits::default()
        };
        assert!(generate(8, 127, &[0; 64], 1, limits, || false).is_err());
        let limits = Limits {
            maximum_work: 0,
            ..Limits::default()
        };
        assert!(generate(8, 127, &[0; 64], 1, limits, || false).is_err());
        for cancel_after in [0, 8, 45, 95, 140, 175, 210] {
            let calls = std::cell::Cell::new(0);
            assert!(
                generate(32, 127, &[0; 1024], 1, Limits::default(), || {
                    calls.set(calls.get() + 1);
                    calls.get() > cancel_after
                })
                .unwrap()
                .is_none()
            );
        }
        let field = generate(8, 127, &[0; 64], 1, Limits::default(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(field.boundary_seeds, 0);
        assert!(field.sample([0.5, 0.5], 1.).is_none());
        let (bytes, work) = bounds(8, 1).unwrap();
        assert!(
            generate(
                8,
                127,
                &[0; 64],
                1,
                Limits {
                    maximum_bytes: bytes,
                    maximum_work: work
                },
                || false
            )
            .unwrap()
            .is_some()
        );
        assert!(
            generate(
                8,
                127,
                &[0; 64],
                1,
                Limits {
                    maximum_bytes: bytes - 1,
                    maximum_work: work
                },
                || false
            )
            .is_err()
        );
        assert!(
            generate(
                8,
                127,
                &[0; 64],
                1,
                Limits {
                    maximum_bytes: bytes,
                    maximum_work: work - 1
                },
                || false
            )
            .is_err()
        );
        for uv in [[-0.01, 0.5], [0.5, 1.01], [f32::NAN, 0.5]] {
            assert!(field.sample(uv, 1.).is_none());
        }
        for scale in [0., -1., f32::INFINITY] {
            assert!(field.sample([0.5, 0.5], scale).is_none());
        }
        let malformed = Field {
            resolution: 8,
            distances: vec![],
            vectors: vec![],
            boundary_seeds: 0,
        };
        assert!(malformed.sample([0.5, 0.5], 1.).is_none());
    }
}
