//! Mainland shoreline provider sampling over immutable terrain revisions.
//! Selected-installation activation and deep-sea bounds belong to the caller.
use crate::{native_layers::View, shore_field, topology_query, water_map_query::Grid};

pub struct Input<'a> {
    pub terrain: View<'a>,
    pub water: Option<Grid<'a>>,
    pub origin_y: f32,
    pub vertical_span: f32,
    pub ocean_level: f32,
}

/// Sample the centered mainland frame used by Mapper's terrain transport.
/// Borrowed source rasters are outside the allowance. Every owned provider,
/// topology-summary, bitmap and distance buffer is reserved before allocation.
pub fn bounds(input: &Input<'_>) -> Result<(u64, u64), String> {
    let terrain = input.terrain;
    let hr = terrain.height_resolution;
    let sr = terrain.surface_resolution;
    if !(9..=4097).contains(&hr)
        || !(hr - 1).is_power_of_two()
        || terrain.height.len() != hr * hr
        || !(1..=4096).contains(&sr)
        || terrain.topology.len() != sr * sr
        || !(1..=10000).contains(&terrain.size)
        || !input.origin_y.is_finite()
        || !input.vertical_span.is_finite()
        || input.vertical_span <= 0.
        || !input.ocean_level.is_finite()
    {
        return Err("Invalid mainland shoreline source frame".into());
    }
    if let Some(water) = input.water
        && (!(2..=4097).contains(&water.resolution)
            || water.data.len() != water.resolution * water.resolution
            || !water.origin_y.is_finite()
            || !water.vertical_span.is_finite()
            || water.vertical_span <= 0.)
    {
        return Err("Invalid mainland shoreline water frame".into());
    }
    let n = (hr - 1) / 2;
    let cells = (n * n) as u64;
    let (field_bytes, field_work) = shore_field::bounds(n, 1)?;
    let provider_bytes = 12 * cells;
    let index_bytes = topology_query::Index::allocation_bytes(sr)?;
    let provider_work = terrain.height.len() as u64
        + input.water.map_or(0, |w| w.data.len() as u64)
        + terrain.topology.len() as u64
        + 32 * cells;
    // generate_classified reserves one bitmap byte and five work visits/cell.
    Ok((
        field_bytes + cells + provider_bytes + index_bytes,
        field_work + 5 * cells + provider_work,
    ))
}

/// Returns the complete sampled field after all provider stages succeed.
pub fn generate(
    input: Input<'_>,
    limits: shore_field::Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<shore_field::Field>, String> {
    let height = Grid {
        resolution: input.terrain.height_resolution,
        data: input.terrain.height,
        origin_y: input.origin_y,
        vertical_span: input.vertical_span,
    };
    let size = input.terrain.size as f32;
    generate_sampled(
        input,
        |position| {
            height.sample([position[0], position[2]].map(|p| (p + size * 0.5) * (1. / size)))
        },
        limits,
        cancelled,
    )
}

/// A projected source can supply world-height queries through its committed
/// stamp kernel while sharing the immutable baseline and topology rasters.
pub fn generate_sampled(
    input: Input<'_>,
    sample_height: impl Fn([f32; 3]) -> Option<f32>,
    limits: shore_field::Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<shore_field::Field>, String> {
    if cancelled() {
        return Ok(None);
    }
    let (bytes, fixed_work) = bounds(&input)?;
    if bytes > limits.maximum_bytes || fixed_work > limits.maximum_work {
        return Err("Mainland shoreline exceeds provider allowance".into());
    }
    let terrain = input.terrain;
    let hr = terrain.height_resolution;
    let sr = terrain.surface_resolution;
    let n = (hr - 1) / 2;
    let cells = (n * n) as u64;
    let field_work = shore_field::bounds(n, 1)?.1;
    let provider_bytes = 12 * cells;
    let provider_work = fixed_work - field_work - 5 * cells;
    for data in std::iter::once(terrain.height).chain(input.water.map(|w| w.data)) {
        for chunk in data.chunks(32768) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid encoded mainland shoreline height".into());
            }
        }
    }
    let Some(index) = topology_query::Index::new(terrain.topology, sr, &cancelled)? else {
        return Ok(None);
    };
    let tiles = index.view(terrain.topology);
    let size = terrain.size as f32;
    let origin = -size * 0.5;
    let metres_per_cell = size / n as f32;
    let inverse_size = 1. / size;
    let cell = |value: f32| (value * sr as f32).clamp(0., (sr - 1) as f32) as usize;
    let mut heights = Vec::with_capacity(n * n);
    let mut water_levels = Vec::with_capacity(n * n);
    let mut masks = Vec::with_capacity(n * n);
    let mut query_work = 0;
    let query_limit = usize::try_from(limits.maximum_work - field_work - 5 * cells - provider_work)
        .unwrap_or(usize::MAX);
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            let position = [x, z].map(|v| origin + (v as f32 + 0.5) * metres_per_cell);
            let uv = position.map(|p| (p - origin) * inverse_size);
            // In-domain HeightMapData and WaterMapQueryStructure share these
            // four-read normalized interpolation operations. Outside-domain
            // height clamping differs and is deliberately never requested here.
            let h = sample_height([position[0], 0., position[1]])
                .filter(|h| h.is_finite())
                .ok_or("Unavailable mainland shoreline height")?;
            let mut water = match input.water {
                Some(w) => w.sample(uv).ok_or("Unavailable mainland shoreline water")?,
                None => input.origin_y,
            };
            let [cx, cz] = uv.map(cell);
            if terrain.topology[cz * sr + cx] & 384 != 0 && water < input.ocean_level {
                water = water.max(input.ocean_level);
            }
            let radius = 8. + 8. * ((h - 4.) * -0.25).clamp(0., 1.);
            let normalized_radius = inverse_size * radius;
            let [x0, z0] = uv.map(|v| cell(v - normalized_radius));
            let [x1, z1] = uv.map(|v| cell(v + normalized_radius));
            let mask = tiles
                .union(
                    topology_query::Query {
                        rect: [x0, x1, z0, z1],
                        center: [cx, cz],
                        radius,
                        relevant: 384 | 204800 | 49152,
                    },
                    &mut query_work,
                    query_limit,
                )
                .map_err(|_| "Mainland shoreline topology work allowance exceeded")?;
            heights.push(h);
            water_levels.push(water);
            masks.push(mask as i32);
        }
    }
    drop(index);
    shore_field::generate_classified(
        n,
        &heights,
        &water_levels,
        shore_field::Topology::Mainland(&masks),
        shore_field::Limits {
            maximum_bytes: limits.maximum_bytes - provider_bytes,
            maximum_work: limits.maximum_work - provider_work - query_work as u64,
        },
        cancelled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view<'a>(
        height: &'a [i16],
        topology: &'a [i32],
        hr: usize,
        sr: usize,
        size: u32,
    ) -> View<'a> {
        View {
            size,
            height_resolution: hr,
            surface_resolution: sr,
            height,
            topology,
            biome: &[],
            splat: &[],
            placement_blockers: None,
            topology_tiles: None,
        }
    }
    #[test]
    fn provider_assembly_matches_actual_managed_mainland_job_bodies() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/shore-mainland-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            reference["format"],
            "papyrus-owned-mainland-shore-reference"
        );
        assert_eq!(reference["schema"], 1);
        assert_eq!(reference["managed_job_bodies_executed"], true);
        assert_eq!(reference["unity_jobs_scheduled"], false);
        assert_eq!(reference["native_library_loaded"], false);
        assert_eq!(
            reference["sources"]["Assembly-CSharp.dll"],
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        let rows = reference["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 36);
        for (ordinal, row) in rows.iter().enumerate() {
            let heights: Vec<i16> = serde_json::from_value(row["heights"].clone()).unwrap();
            let water: Vec<i16> = serde_json::from_value(row["water"].clone()).unwrap();
            let topology: Vec<i32> = serde_json::from_value(row["topology"].clone()).unwrap();
            let hr = row["height_resolution"].as_u64().unwrap() as usize;
            let sr = row["surface_resolution"].as_u64().unwrap() as usize;
            let origin_y = row["origin_y"].as_f64().unwrap() as f32;
            let vertical_span = row["vertical_span"].as_f64().unwrap() as f32;
            let field = generate(
                Input {
                    terrain: view(
                        &heights,
                        &topology,
                        hr,
                        sr,
                        row["size"].as_u64().unwrap() as u32,
                    ),
                    water: Some(Grid {
                        resolution: hr,
                        data: &water,
                        origin_y,
                        vertical_span,
                    }),
                    origin_y,
                    vertical_span,
                    ocean_level: row["ocean_level"].as_f64().unwrap() as f32,
                },
                shore_field::Limits::default(),
                || false,
            )
            .unwrap()
            .unwrap();
            let expected: Vec<u32> = serde_json::from_value(row["distances_bits"].clone()).unwrap();
            assert_eq!(field.distances.len(), expected.len());
            for (i, (&actual, &bits)) in field.distances.iter().zip(&expected).enumerate() {
                assert!(
                    actual.to_bits() == bits || actual.is_nan() && f32::from_bits(bits).is_nan(),
                    "Distance case {ordinal} cell {i}: {actual} vs {}",
                    f32::from_bits(bits)
                );
            }
            let expected: Vec<[u32; 4]> =
                serde_json::from_value(row["vectors_bits"].clone()).unwrap();
            assert_eq!(field.vectors.len(), expected.len());
            for (i, (actual, bits)) in field.vectors.iter().zip(expected).enumerate() {
                for c in 0..4 {
                    assert!(
                        actual[c].to_bits() == bits[c]
                            || actual[c].is_nan() && f32::from_bits(bits[c]).is_nan(),
                        "Vector case {ordinal} cell {i} channel {c}: {} vs {}",
                        actual[c],
                        f32::from_bits(bits[c])
                    );
                }
            }
        }
    }

    #[test]
    fn provider_budgets_cancellation_and_invalid_sources_publish_no_partial_field() {
        let heights: Vec<_> = (0..17 * 17)
            .map(|i| if i % 17 < 8 { 16000 } else { 17000 })
            .collect();
        let water = [0; 17 * 17];
        let topology = [128; 16 * 16];
        let input = || Input {
            terrain: view(&heights, &topology, 17, 16, 64),
            water: Some(Grid {
                resolution: 17,
                data: &water,
                origin_y: -500.,
                vertical_span: 1000.,
            }),
            origin_y: -500.,
            vertical_span: 1000.,
            ocean_level: 0.,
        };
        let limits = shore_field::Limits::default();
        let field = generate(input(), limits, || false).unwrap().unwrap();
        let sampled = generate_sampled(
            input(),
            |position| {
                assert_eq!(position[1], 0.);
                Grid {
                    resolution: 17,
                    data: &heights,
                    origin_y: -500.,
                    vertical_span: 1000.,
                }
                .sample([position[0], position[2]].map(|p| (p + 32.) * (1. / 64.)))
            },
            limits,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(sampled.distances, field.distances);
        assert_eq!(sampled.vectors, field.vectors);
        for unavailable in [None, Some(f32::NAN), Some(f32::INFINITY)] {
            assert!(generate_sampled(input(), |_| unavailable, limits, || false).is_err());
        }
        assert!(
            generate_sampled(
                input(),
                |_| panic!("Cancelled height query"),
                limits,
                || true
            )
            .unwrap()
            .is_none()
        );
        let mut missing_water = input();
        missing_water.water = None;
        let without = generate(missing_water, limits, || false).unwrap().unwrap();
        assert_eq!(field.distances, without.distances);
        assert_eq!(field.vectors, without.vectors);
        let (bytes, work) = shore_field::bounds(8, 1).unwrap();
        let peak = bytes + 64 + 12 * 64 + topology_query::Index::allocation_bytes(16).unwrap();
        assert!(
            generate(
                input(),
                shore_field::Limits {
                    maximum_bytes: peak,
                    ..limits
                },
                || false
            )
            .unwrap()
            .is_some()
        );
        assert!(
            generate(
                input(),
                shore_field::Limits {
                    maximum_bytes: peak - 1,
                    ..limits
                },
                || false
            )
            .is_err()
        );
        let fixed_work = work + 5 * 64 + 17 * 17 * 2 + 16 * 16 + 32 * 64;
        assert!(
            generate(
                input(),
                shore_field::Limits {
                    maximum_work: fixed_work,
                    ..limits
                },
                || false
            )
            .is_err()
        );
        let count = std::cell::Cell::new(0);
        generate(input(), limits, || {
            count.set(count.get() + 1);
            false
        })
        .unwrap()
        .unwrap();
        let total = count.get();
        for after in [0, 1, 3, total / 2, total - 1] {
            count.set(0);
            assert!(
                generate(input(), limits, || {
                    count.set(count.get() + 1);
                    count.get() > after
                })
                .unwrap()
                .is_none()
            );
        }
        let mut bad_heights = heights.clone();
        bad_heights[0] = -1;
        let mut bad = input();
        bad.terrain.height = &bad_heights;
        assert!(generate(bad, limits, || false).is_err());
        let mut bad_water = water;
        bad_water[0] = 32767;
        let mut bad = input();
        bad.water.as_mut().unwrap().data = &bad_water;
        assert!(generate(bad, limits, || false).is_err());
        let mut bad = input();
        bad.terrain.height_resolution = usize::MAX;
        assert!(generate(bad, limits, || false).is_err());
        let mut bad = input();
        bad.terrain.topology = &topology[..255];
        assert!(generate(bad, limits, || false).is_err());
        let mut bad = input();
        bad.vertical_span = f32::NAN;
        assert!(generate(bad, limits, || false).is_err());
        assert_eq!(heights[0], 16000);
        assert_eq!(water, [0; 17 * 17]);
    }
}
