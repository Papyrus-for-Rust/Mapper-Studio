use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

fn policy() -> Policy {
    let profile: serde_json::Value = serde_json::from_str(include_str!(
        "../../../compatibility/erosion/client-8dd4e235.json"
    ))
    .unwrap();
    let mut grid = profile["coefficients"].clone();
    grid["cell_area"] = serde_json::json!(1000_f32 / 17. * 1000. / 17.);
    grid["normal_y"] = serde_json::json!(1_f32 / 17.);
    grid["ocean_level"] = serde_json::json!(0.);
    Policy {
        grid: serde_json::from_value(grid).unwrap(),
        bounds: serde_json::from_value(profile["bounds"].clone()).unwrap(),
        paint: serde_json::from_value(profile["paint"].clone()).unwrap(),
    }
}
fn limits() -> Limits {
    Limits {
        maximum_bytes: 16 * 1024 * 1024,
        maximum_work: 32_000_000,
    }
}
fn layers() -> Layers {
    let n = 17;
    let s = 16;
    let height = (0..n * n)
        .map(|i| 16500 + ((i % n) * 70 + (i / n) * 40) as i16)
        .collect();
    let mut splat = vec![0; s * s * 8];
    for pixel in splat.chunks_mut(8) {
        pixel[0] = 255;
    }
    Layers {
        size: 1000,
        terrain_seed: 123,
        biome_seed: 456,
        height_resolution: n,
        surface_resolution: s,
        height,
        biome: vec![0; s * s * 5],
        splat,
        topology: vec![0; s * s],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    }
}
fn pool(workers: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .unwrap()
}

#[test]
fn terrain_codec_keeps_source_reciprocal_and_round_trip_for_every_height_word() {
    let mut division_differences = 0;
    for height in 0..=32766 {
        let source = -500. + (f32::from(height) * 3.051944E-05) * 1000.;
        assert_eq!(decode(height).to_bits(), source.to_bits());
        assert_eq!(encode(source).unwrap(), height);
        division_differences += usize::from(decode(height) != crate::erosion::decode(height));
    }
    assert!(division_differences > 0);
    assert!(encode(f32::NAN).is_err());
    assert!(encode(600.).is_err());
}

#[test]
fn split_stages_retain_float_delta_and_use_the_current_terrain_for_late_paint() {
    let original = layers();
    let pool = pool(2);
    let eroded = erode(&original, policy(), limits(), &pool, || false, |_, _| {})
        .unwrap()
        .unwrap();
    let input = original
        .height
        .iter()
        .map(|&height| decode(height))
        .collect::<Vec<_>>();
    let bounds = erosion_bounds::build(
        erosion_bounds::Input {
            height_resolution: 17,
            heights: &input,
            topology_resolution: 16,
            topology: &original.topology,
            terrain_one_over_size: 1. / 1000.,
            ocean_level: 0.,
        },
        policy().bounds,
        erosion_bounds::Limits {
            maximum_bytes: limits().maximum_bytes,
            maximum_work: limits().maximum_work as usize,
        },
        || false,
    )
    .unwrap()
    .unwrap();
    let grid = erosion_grid::run(
        erosion_grid::Input {
            resolution: 17,
            heights: &input,
            minimum_heights: &bounds.minimum_heights,
        },
        policy().grid,
        erosion_grid::Limits {
            maximum_bytes: limits().maximum_bytes,
            maximum_work: limits().maximum_work,
        },
        &pool,
        || false,
        |_, _| {},
    )
    .unwrap()
    .unwrap();
    let expected = grid
        .heights
        .iter()
        .map(|&height| ((0.001 * (height + 500.)) * 32766. + 0.5) as i16)
        .collect::<Vec<_>>();
    assert_eq!(eroded.height, expected);
    assert_eq!(eroded.painting.delta, grid.height_delta);
    assert_eq!(
        eroded.report.work,
        (2 * input.len()) as u64 + bounds.report.work as u64 + grid.report.work
    );
    assert!(eroded.painting.delta.iter().any(|v| *v != 0.));
    assert!(
        eroded
            .painting
            .delta
            .iter()
            .zip(&original.height)
            .zip(&eroded.height)
            .any(|((&delta, &old), &new)| delta != decode(old) - decode(new))
    );
    assert_eq!(original.height, layers().height);
    let delta = eroded.painting.delta.clone();
    let mut current = original.clone();
    current.height = eroded.height;
    for z in 1..16 {
        current.height[z * 17 + 8] += 500;
    }
    current.topology[0] = policy().paint.protected_topology as i32;
    current.splat[0] = 128;
    current.splat[1] = 127;
    let painted = eroded
        .painting
        .paint(&current, &pool, || false)
        .unwrap()
        .unwrap();
    let restored = Painting::restore(
        &original,
        delta.clone(),
        policy().paint,
        limits(),
        eroded.report.work,
        eroded.report.peak_buffer_bytes,
        || false,
    )
    .unwrap()
    .unwrap()
    .paint(&current, &pool, || false)
    .unwrap()
    .unwrap();
    assert_eq!(restored.output.splat, painted.output.splat);
    assert_eq!(restored.work, painted.work);
    assert_eq!(restored.peak_buffer_bytes, painted.peak_buffer_bytes);
    let slopes = terrain_slope::build(
        terrain_slope::Input {
            size: current.size,
            resolution: 17,
            heights: &current.height,
        },
        terrain_slope::Limits {
            maximum_bytes: 1_000_000,
            maximum_work: 1_000_000,
        },
        &pool,
        || false,
    )
    .unwrap()
    .unwrap();
    let expected = erosion_splat::paint(
        erosion_splat::Input {
            height_resolution: 17,
            height_delta: &delta,
            slopes_degrees: &slopes.slopes_degrees,
            topology_resolution: 16,
            topology: &current.topology,
            splat_resolution: 16,
            splat: &current.splat,
            terrain_one_over_size: 1. / 1000.,
        },
        policy().paint,
        erosion_splat::Limits {
            maximum_bytes: 1_000_000,
            maximum_work: 1_000_000,
        },
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(painted.output.splat, expected.splat);
    assert_eq!(
        painted.work,
        eroded.report.work + slopes.report.work as u64 + expected.report.work as u64
    );
    assert!(painted.work <= limits().maximum_work);
    assert!(painted.peak_buffer_bytes <= limits().maximum_bytes);
    assert_eq!(eroded.painting.delta, delta);
    let retry = eroded
        .painting
        .paint(&current, &pool, || false)
        .unwrap()
        .unwrap();
    assert_eq!(retry.output.splat, painted.output.splat);
}

#[test]
fn phase_cancellation_and_shared_limits_preserve_inputs() {
    let original = layers();
    let pool = pool(1);
    assert!(
        erode(&original, policy(), limits(), &pool, || true, |_, _| {})
            .unwrap()
            .is_none()
    );
    let cancelled = AtomicBool::new(false);
    let result = erode(
        &original,
        policy(),
        limits(),
        &pool,
        || cancelled.load(Ordering::Relaxed),
        |iteration, _| {
            if iteration >= 3 {
                cancelled.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    assert!(result.is_none());
    for limits in [
        Limits {
            maximum_bytes: 0,
            ..limits()
        },
        Limits {
            maximum_work: 0,
            ..limits()
        },
    ] {
        assert!(erode(&original, policy(), limits, &pool, || false, |_, _| {}).is_err());
    }
    let eroded = erode(&original, policy(), limits(), &pool, || false, |_, _| {})
        .unwrap()
        .unwrap();
    assert!(
        eroded
            .painting
            .paint(&original, &pool, || true)
            .unwrap()
            .is_none()
    );
    let tight_memory = Limits {
        maximum_bytes: eroded.report.peak_buffer_bytes - 1,
        ..limits()
    };
    assert!(
        erode(
            &original,
            policy(),
            tight_memory,
            &pool,
            || false,
            |_, _| {}
        )
        .is_err()
    );
    let mut wrong_world = original.clone();
    wrong_world.terrain_seed += 1;
    assert!(
        eroded
            .painting
            .paint(&wrong_world, &pool, || false)
            .is_err()
    );
    let tight = Limits {
        maximum_work: eroded.report.work,
        ..limits()
    };
    let tight = erode(&original, policy(), tight, &pool, || false, |_, _| {})
        .unwrap()
        .unwrap();
    assert!(tight.painting.paint(&original, &pool, || false).is_err());
    assert_eq!(original.height, layers().height);
    assert_eq!(original.splat, layers().splat);
}

#[test]
fn split_pipeline_is_identical_across_worker_counts() {
    let original = layers();
    let run = |workers| {
        let pool = pool(workers);
        let eroded = erode(&original, policy(), limits(), &pool, || false, |_, _| {})
            .unwrap()
            .unwrap();
        let mut current = original.clone();
        current.height.clone_from(&eroded.height);
        let painted = eroded
            .painting
            .paint(&current, &pool, || false)
            .unwrap()
            .unwrap();
        (
            eroded.height,
            eroded.painting.delta,
            painted.output.splat,
            painted.work,
        )
    };
    assert_eq!(run(1), run(2));
}
