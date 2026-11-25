//! Current encoded-terrain slopes for the late erosion material pass.
use crate::terrain_normal;
use rayon::prelude::*;
use serde::Serialize;

pub const VERSION: &str = "papyrus-current-terrain-slopes-1";

pub struct Input<'a> {
    pub size: u32,
    pub resolution: usize,
    pub heights: &'a [i16],
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub maximum_bytes: usize,
    pub maximum_work: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub cells: usize,
    pub sampled_cells: usize,
    pub buffer_bytes: usize,
    pub work: usize,
}

pub struct Output {
    pub slopes_degrees: Vec<f32>,
    pub report: Report,
}

/// Resolve an interior cell against the original CPU slope calculation.
/// The caller supplies a validated raster and an interior linear index.
pub fn interior_angle(input: &Input<'_>, cell: usize) -> f32 {
    let n = input.resolution;
    let norm_y = input.size as f32 / 1000. / n as f32;
    terrain_normal::slope_degrees(terrain_normal::normal_from_heights(
        norm_y,
        input.heights[cell - n - 1],
        input.heights[cell - n + 1],
        input.heights[cell + n - 1],
    ))
}

/// Resolve a cliff cutoff at an exact interior height vertex. The cliff stage
/// uses the interpolated normal convention, including its normalizations.
pub fn interior_cliff_angle(input: &Input<'_>, cell: usize) -> Result<f32, String> {
    let n = input.resolution;
    terrain_normal::slope_at_vertex(
        n,
        input.size as f32 / 1000. / n as f32,
        input.heights,
        cell % n,
        cell / n,
    )
}

/// Compute each interior normal once, without interpolation or a temporary
/// normal grid. The source late pass refreshes only interior angles. This pass
/// explicitly initializes the outer ring to zero.
/// Results remain private until all rows succeed, including cancellation.
pub fn build(
    input: Input<'_>,
    limits: Limits,
    pool: &rayon::ThreadPool,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<Output>, String> {
    let n = input.resolution;
    if !(1000..=6000).contains(&input.size)
        || !(3..=4097).contains(&n)
        || input.heights.len() != n * n
    {
        return Err("Invalid current terrain slope dimensions".into());
    }
    let cells = n * n;
    let bytes = cells * std::mem::size_of::<f32>();
    // Charge a complete input scan and output sweep, including boundary cells.
    let work = cells * 2;
    if bytes > limits.maximum_bytes || work > limits.maximum_work {
        return Err("Current terrain slopes exceed the buffer or work allowance".into());
    }
    for row in input.heights.chunks(n) {
        if cancelled() {
            return Ok(None);
        }
        if row.iter().any(|h| !(0..=32766).contains(h)) {
            return Err("Invalid current terrain height encoding".into());
        }
    }
    let mut slopes = Vec::new();
    slopes
        .try_reserve_exact(cells)
        .map_err(|_| "Unable to allocate current terrain slopes")?;
    slopes.resize(cells, 0.);
    let norm_y = input.size as f32 / 1000. / n as f32;
    let complete = pool.install(|| {
        slopes
            .par_chunks_mut(n)
            .enumerate()
            .map(|(z, row)| {
                if cancelled() {
                    return false;
                }
                if z == 0 || z == n - 1 {
                    return true;
                }
                let above = &input.heights[(z - 1) * n..z * n];
                let below = &input.heights[(z + 1) * n..(z + 2) * n];
                let corners = above.iter().zip(&above[2..]).zip(below);
                for (slope, ((&base, &right), &lower)) in row[1..n - 1].iter_mut().zip(corners) {
                    let normal = terrain_normal::normal_from_heights(norm_y, base, right, lower);
                    *slope = terrain_normal::slope_degrees(normal);
                }
                true
            })
            .all(|row_complete| row_complete)
    });
    if !complete || cancelled() {
        return Ok(None);
    }
    Ok(Some(Output {
        slopes_degrees: slopes,
        report: Report {
            engine: VERSION,
            cells,
            sampled_cells: (n - 2) * (n - 2),
            buffer_bytes: bytes,
            work,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(n: usize) -> Limits {
        Limits {
            maximum_bytes: n * n * 4,
            maximum_work: n * n * 2,
        }
    }

    #[test]
    fn current_slope_grids_match_owned_height_normals_and_vector_angles() {
        #[derive(serde::Deserialize)]
        struct Case {
            size: u32,
            resolution: usize,
            heights: Vec<i16>,
            slopes: Vec<f32>,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            assembly_sha256: String,
            slope_grids: Vec<Case>,
        }
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../tests/fixtures/terrain-slope-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            fixture.assembly_sha256,
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(fixture.slope_grids.len(), 18);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        for case in fixture.slope_grids {
            let actual = build(
                Input {
                    size: case.size,
                    resolution: case.resolution,
                    heights: &case.heights,
                },
                limits(case.resolution),
                &pool,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(actual.slopes_degrees.len(), case.slopes.len());
            for (i, (a, b)) in actual.slopes_degrees.iter().zip(case.slopes).enumerate() {
                assert!(
                    (a - b).abs() < 2e-5,
                    "slope {i}, size {}, res {}: {a} != {b}",
                    case.size,
                    case.resolution
                );
            }
        }
    }

    #[test]
    fn diagonal_samples_and_worker_counts_preserve_current_height_slopes() {
        let n = 17;
        let heights: Vec<_> = (0..n * n)
            .map(|i| (15000 + (i * 127 + i * i * 13) % 2500) as i16)
            .collect();
        let snapshot = heights.clone();
        let run = |threads| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            build(
                Input {
                    size: 4000,
                    resolution: n,
                    heights: &heights,
                },
                limits(n),
                &pool,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let one = run(1);
        assert_eq!(one.slopes_degrees, run(3).slopes_degrees);
        assert_eq!(one.report.sampled_cells, 225);
        assert_eq!(one.report.buffer_bytes, n * n * 4);
        assert_eq!(one.report.work, n * n * 2);
        for z in 0..n {
            for x in 0..n {
                let actual = one.slopes_degrees[z * n + x];
                if x == 0 || z == 0 || x == n - 1 || z == n - 1 {
                    assert_eq!(actual, 0.);
                } else {
                    let norm_y = 4000_f32 / 1000. / n as f32;
                    let checked =
                        terrain_normal::normal_at(n, norm_y, x, z, |x, z| Some(heights[z * n + x]))
                            .unwrap();
                    assert_eq!(
                        actual.to_bits(),
                        terrain_normal::slope_degrees(checked).to_bits()
                    );
                    let expected = terrain_normal::slope(
                        4000,
                        n,
                        &heights,
                        [x as f32 / (n - 1) as f32, z as f32 / (n - 1) as f32],
                    )
                    .unwrap();
                    assert!((actual - expected).abs() < 2e-5);
                }
            }
        }
        assert_eq!(heights, snapshot);
    }

    #[test]
    fn cancellation_invalid_encoding_and_limits_never_publish_a_grid() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let mut heights = vec![16000; 25];
        let run = |heights: &[i16], limits, cancel| {
            build(
                Input {
                    size: 1000,
                    resolution: 5,
                    heights,
                },
                limits,
                &pool,
                cancel,
            )
        };
        assert!(
            run(&heights, limits(5), (|| true) as fn() -> bool)
                .unwrap()
                .is_none()
        );
        let allowance = limits(5);
        for limit in [
            Limits {
                maximum_bytes: allowance.maximum_bytes - 1,
                ..allowance
            },
            Limits {
                maximum_work: allowance.maximum_work - 1,
                ..allowance
            },
        ] {
            assert!(run(&heights, limit, (|| false) as fn() -> bool).is_err());
        }
        for h in [-1, 32767] {
            heights[24] = h;
            assert!(run(&heights, limits(5), (|| false) as fn() -> bool).is_err());
        }
        heights[24] = 16000;
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            build(
                Input {
                    size: 1000,
                    resolution: 5,
                    heights: &heights
                },
                limits(5),
                &pool,
                || calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 6,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            build(
                Input {
                    size: 1000,
                    resolution: 5,
                    heights: &heights[..24]
                },
                limits(5),
                &pool,
                || false,
            )
            .is_err()
        );
    }
}
