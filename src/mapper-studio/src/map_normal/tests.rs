use super::*;
#[test]
fn configured_pool_accepts_non_sync_cancellation_between_bounded_batches() {
    let calls = std::cell::Cell::new(0usize);
    let mut s = settings();
    s.height_resolution = 257;
    s.maximum_work = 20_000_000;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let cancelled = || {
        calls.set(calls.get() + 1);
        calls.get() > 260
    };
    assert!(
        generate_sampled_in(|_, _| Some(18000), s, Some(&pool), cancelled)
            .unwrap()
            .is_none()
    );
    assert!(calls.get() > 260);
}
fn settings() -> Settings {
    Settings {
        height_resolution: 17,
        size: 1000,
        origin_y: -500.,
        vertical_span: 1000.,
        maximum_work: 1_000_000,
        maximum_bytes: 1_000_000,
    }
}
#[test]
fn packed_decode_and_lighting_match_the_owned_shader_including_nonunit_inputs() {
    #[derive(serde::Deserialize)]
    struct Case {
        sampled_rgba: [f32; 4],
        rgba: [f32; 4],
    }
    #[derive(serde::Deserialize)]
    struct Probe {
        rows: Vec<Case>,
    }
    #[derive(serde::Deserialize)]
    struct Evidence {
        paper_program_sha256: String,
        synthetic_shader_probe: Probe,
    }
    let evidence: Evidence = serde_json::from_str(include_str!(
        "../../tests/fixtures/client-normal-construction-e7156192.json"
    ))
    .unwrap();
    assert_eq!(
        evidence.paper_program_sha256,
        "b48a03a5b8b079f0fbc660e098a6430ac31ed25f1c4659d96ada88cb94a924d8"
    );
    let mut material = crate::map_colors::tests::fixture();
    material.shader_blob_sha256 =
        Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07".into());
    for case in evidence.synthetic_shader_probe.rows {
        let normal = decode(case.sampled_rgba).unwrap();
        let actual = material.land([0.86274517, 0.9294118, 0.94117653, 1.], normal);
        for (a, b) in actual.into_iter().zip(case.rgba) {
            assert!(
                (a - b * 255.).abs() < 0.0002,
                "{normal:?}: {a} vs {}",
                b * 255.
            );
        }
    }
    assert_eq!(decode([1.; 4]), Some([1., 0., 1.]));
    assert_eq!(decode([0.5, 0.5, 0., 0.5]), decode([0.5; 4]));
    for bad in [[f32::NAN; 4], [-0.01; 4], [1.01; 4]] {
        assert!(decode(bad).is_none());
    }
}
#[test]
fn authored_steep_flattening_and_rgba_lanes_are_preserved() {
    assert_eq!(encode([0., 1., 0.]), [128; 4]);
    let normal = |degrees: f32| {
        let a = degrees.to_radians();
        [a.sin(), a.cos(), 0.]
    };
    assert_eq!(encode(normal(80.)), [128; 4]);
    assert_eq!(encode(normal(60.)), [128, 128, 128, 191]);
    // Normal encoding differs from opaque canonical DXT5nm red=1.
    assert_ne!(encode(normal(30.))[0], 255);
}
#[test]
fn block_worker_replays_in_thread_limits_and_lazy_reads_match_slices() {
    let mut s = settings();
    s.height_resolution = 33;
    let height = (0..33 * 33)
        .map(|i| (18000 + (i % 33) * 70 + (i / 33) * 20) as i16)
        .collect::<Vec<_>>();
    let build = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| generate(&height, s, || false).unwrap().unwrap())
    };
    let a = build(1);
    let b = build(2);
    assert_eq!(a.pixels, b.pixels);
    assert_eq!(a.report.work_reserved, b.report.work_reserved);
    let lazy = generate_sampled(|x, z| Some(height[z * 33 + x]), s, || false)
        .unwrap()
        .unwrap();
    assert_eq!(a.pixels, lazy.pixels);
    assert_eq!(a.resolution, 16);
    assert_eq!(a.report.blocks, 16);
    assert_eq!(a.report.pixel_bytes, 16 * 16 * 4);
    let mut exact = s;
    exact.maximum_work = a.report.work_reserved;
    exact.maximum_bytes = a.report.pixel_bytes;
    assert!(generate(&height, exact, || false).unwrap().is_some());
    exact.maximum_work -= 1;
    assert!(generate(&height, exact, || false).is_err());
    exact = s;
    exact.maximum_bytes = a.report.pixel_bytes - 1;
    assert!(generate(&height, exact, || false).is_err());
    assert!(a.sample([f32::NAN, 0.]).is_none());
    assert_eq!(a.sample([-1., -1.]), a.sample([0., 0.]));
    assert_eq!(a.sample([2., 2.]), a.sample([1., 1.]));
}
#[test]
fn sampler_filters_encoded_lanes_before_decoding_and_never_normalizes() {
    let mut pixels = vec![[128; 4]; 64];
    pixels[1] = [255, 255, 255, 255];
    let grid = Grid {
        resolution: 8,
        pixels: Arc::new(pixels),
        levels: vec![Level {
            resolution: 8,
            offset: 0,
        }],
        report: Report {
            engine: VERSION,
            resolution: 8,
            blocks: 4,
            work_reserved: 1,
            pixel_bytes: 256,
            working_bytes: 256,
        },
    };
    let middle = [1. / 8., 0.5 / 8.];
    let packed = grid.sample_rgba(middle).unwrap();
    assert!(
        packed
            .iter()
            .all(|v| (*v - (128. / 255. + 1.) / 2.).abs() < 1e-7)
    );
    assert_eq!(grid.sample(middle), decode(packed));
    let a = grid.sample([0.5 / 8., 0.5 / 8.]).unwrap();
    let b = grid.sample([1.5 / 8., 0.5 / 8.]).unwrap();
    let blended = std::array::from_fn::<_, 3, _>(|i| (a[i] + b[i]) * 0.5);
    assert_ne!(grid.sample(middle).unwrap(), blended);
    assert_eq!(b, [1., 0., 1.]);
}
#[test]
fn cancellation_and_invalid_readers_never_return_partial_textures() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let height = vec![18000; 17 * 17];
    let s = settings();
    assert!(generate(&height, s, || true).unwrap().is_none());
    let reads = AtomicUsize::new(0);
    assert!(
        generate_sampled(
            |_, _| {
                reads.fetch_add(1, Ordering::Relaxed);
                Some(18000)
            },
            s,
            || reads.load(Ordering::Relaxed) > 17 * 17 + 12
        )
        .unwrap()
        .is_none()
    );
    assert!(generate_sampled(|_, _| None, s, || false).is_err());
    assert!(generate_sampled(|_, _| Some(-1), s, || false).is_err());
    let reads = AtomicUsize::new(0);
    assert!(
        generate_sampled(
            |_, _| if reads.fetch_add(1, Ordering::Relaxed) < 17 * 17 {
                Some(18000)
            } else {
                None
            },
            s,
            || false
        )
        .is_err()
    );
}

#[test]
fn unorm_mip_sampling_matches_owned_current_shader_at_fractional_lods() {
    let probe: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/client-papermap-unorm-mips-20261108.json"
    ))
    .unwrap();
    assert_eq!(probe["format"], "mapper-owned-shader-mip-unorm-probe");
    assert_eq!(probe["schema"], 1);
    assert_eq!(probe["driver"], "d3d11-warp");
    assert_eq!(
        probe["pixel_program_sha256"],
        "ac12266cb1b50ca7c47cc37a9585a3af5109682ebc96049e838e228c67ce7aff"
    );
    let mut material = crate::map_colors::tests::fixture();
    material.shader_blob_sha256 = Some(probe["shader_blob_sha256"].as_str().unwrap().into());
    let inputs = &probe["material"];
    material.colors[1] = serde_json::from_value(inputs["snow"].clone()).unwrap();
    material.colors[9] = serde_json::from_value(inputs["water"].clone()).unwrap();
    material.colors[10] = serde_json::from_value(inputs["deep_water"].clone()).unwrap();
    assert_eq!(
        material.sun_direction,
        serde_json::from_value::<[f32; 3]>(inputs["sun_direction"].clone()).unwrap()
    );
    for (actual, field) in [
        (material.sun_power, "sun_power"),
        (material.brightness, "brightness"),
        (material.contrast, "contrast"),
    ] {
        assert_eq!(actual, inputs[field].as_f64().unwrap() as f32);
    }
    let mut cases = 0;
    for profile in probe["profiles"].as_array().unwrap() {
        let colors: [[u8; 4]; 4] =
            serde_json::from_value(profile["normal_mips_u8"].clone()).unwrap();
        let mut pixels = Vec::new();
        let mut levels = Vec::new();
        for (level, color) in colors.into_iter().enumerate() {
            let resolution = 8 >> level;
            levels.push(Level {
                resolution,
                offset: pixels.len(),
            });
            pixels.extend(vec![color; resolution * resolution]);
        }
        let bytes = pixels.len() * 4;
        let grid = Grid {
            resolution: 8,
            pixels: Arc::new(pixels),
            levels,
            report: Report {
                engine: "owned-unorm-input",
                resolution: 8,
                blocks: 0,
                work_reserved: 0,
                pixel_bytes: bytes,
                working_bytes: bytes,
            },
        };
        let normal = grid.sample([0.5, 0.5]).unwrap();
        for row in profile["rows"].as_array().unwrap() {
            let lod = row["normal_lod"].as_f64().unwrap() as f32;
            let packed = grid.sample_rgba_lod([0.5, 0.5], lod).unwrap();
            let slope = material.sampled_slope(0., Some(packed));
            let weights = [0., 1., 0., 0., 0., 0., 0., 0.];
            let actual = if let Some(depth) = row["water_depth"].as_f64() {
                material.water_with_slope(weights, depth as f32, slope)
            } else {
                material.land_with_slope(material.ground(weights), normal, slope)
            };
            let expected: [f32; 4] = serde_json::from_value(row["rgba"].clone()).unwrap();
            for (a, b) in actual.into_iter().zip(expected) {
                assert!(
                    (a - b * 255.).abs() < 0.0002,
                    "LOD {lod}: {a} differs from {}",
                    b * 255.
                );
            }
            let footprint = row["uv_scale"].as_f64().unwrap() as f32 * 3.;
            assert!((grid.lod_for_footprint(footprint) - lod.max(0.)).abs() < 1e-6);
            cases += 1;
        }
        assert_eq!(
            grid.sample_rgba_lod([-1., 2.], -1.),
            grid.sample_rgba([0., 1.])
        );
        assert!(grid.sample_rgba_lod([f32::NAN, 0.], 0.).is_none());
        assert!(grid.sample_rgba_lod([0.; 2], f32::INFINITY).is_none());
    }
    assert_eq!(cases, 102);
}

#[test]
fn mip_builder_preserves_base_pixels_filters_before_compression_and_bounds_work() {
    let mut settings = settings();
    settings.height_resolution = 65;
    settings.maximum_work = 20_000_000;
    let height = (0..65 * 65)
        .map(|i| (18000 + (i % 65) * 3 + (i / 65) * 5) as i16)
        .collect::<Vec<_>>();
    let legacy = generate(&height, settings, || false).unwrap().unwrap();
    let run = |workers| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap();
        generate_mipped_sampled_in(
            |x, z| Some(height[z * 65 + x]),
            settings,
            Some(&pool),
            || false,
        )
        .unwrap()
        .unwrap()
    };
    let mip = run(1);
    let parallel = run(2);
    assert_eq!(mip.pixels(), legacy.pixels());
    assert_eq!(mip.all_pixels(), parallel.all_pixels());
    assert_eq!(
        mip.levels.iter().map(|l| l.resolution).collect::<Vec<_>>(),
        [32, 16, 8, 4, 2, 1]
    );
    let raw = generate_base_in(
        |x, z| Some(height[z * 65 + x]),
        settings,
        None,
        false,
        || false,
    )
    .unwrap()
    .unwrap();
    let mut raw = (*raw.pixels).clone();
    for level in &mip.levels {
        let size = level.resolution;
        for block_z in 0..size.div_ceil(4) {
            for block_x in 0..size.div_ceil(4) {
                let mut block = [[0; 4]; 16];
                let mut mask = 0;
                for (i, p) in block.iter_mut().enumerate() {
                    let x = block_x * 4 + i % 4;
                    let z = block_z * 4 + i / 4;
                    if x < size && z < size {
                        *p = raw[z * size + x];
                        mask |= 1 << i;
                    }
                }
                for (i, p) in compress(block, mask).into_iter().enumerate() {
                    let x = block_x * 4 + i % 4;
                    let z = block_z * 4 + i / 4;
                    if x < size && z < size {
                        assert_eq!(mip.all_pixels()[level.offset + z * size + x], p);
                    }
                }
            }
        }
        if size > 1 {
            let half = size / 2;
            raw = (0..half * half)
                .map(|i| {
                    std::array::from_fn(|c| {
                        let x = (i % half) * 2;
                        let z = (i / half) * 2;
                        let total = u16::from(raw[z * size + x][c])
                            + u16::from(raw[z * size + x + 1][c])
                            + u16::from(raw[(z + 1) * size + x][c])
                            + u16::from(raw[(z + 1) * size + x + 1][c]);
                        (f32::from(total) / 4.).round_ties_even() as u8
                    })
                })
                .collect();
        }
    }
    assert_eq!(mip.report.pixel_bytes, mip.all_pixels().len() * 4);
    assert!(mip.report.working_bytes > mip.report.pixel_bytes);
    let mut exact = settings;
    exact.maximum_work = mip.report.work_reserved;
    exact.maximum_bytes = mip.report.working_bytes;
    assert!(
        generate_mipped_sampled_in(|x, z| Some(height[z * 65 + x]), exact, None, || false)
            .unwrap()
            .is_some()
    );
    exact.maximum_bytes -= 1;
    assert!(
        generate_mipped_sampled_in(
            |_, _| panic!("Budget check must precede reads"),
            exact,
            None,
            || false
        )
        .is_err()
    );
    exact.maximum_bytes = mip.report.working_bytes;
    exact.maximum_work -= 1;
    assert!(
        generate_mipped_sampled_in(
            |_, _| panic!("Work check must precede reads"),
            exact,
            None,
            || false
        )
        .is_err()
    );
    let calls = std::cell::Cell::new(0);
    assert!(
        generate_mipped_sampled_in(
            |x, z| Some(height[z * 65 + x]),
            settings,
            None,
            || {
                calls.set(calls.get() + 1);
                calls.get() > 72
            }
        )
        .unwrap()
        .is_none()
    );
}
