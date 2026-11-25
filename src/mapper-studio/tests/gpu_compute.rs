//! GPU compute correctness and timing; never launches a GUI or game runtime.
use mapper_studio::map_image::{render_with_ocean_mask, render_with_palette};
use papyrus_terrain::native_layers::{Layers, dimensions};
/// Opt-in headless hardware check; never opens a window or loads game DLLs.
#[test]
#[ignore = "requires a hardware GPU; run alone with --test-threads=1 --nocapture"]
fn benchmark_cpu_gpu_cartography() {
    use mapper_studio::generation_resources::{ImageCompute, Limits, configure};
    let (hr, sr) = dimensions(1000).unwrap();
    let cells = sr * sr;
    let layers = Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 2,
        height_resolution: hr,
        surface_resolution: sr,
        height: (0..hr * hr)
            .map(|i| {
                let h = ((i % hr) as f32 / hr as f32 * 18.).sin() * 35.
                    + (i / hr) as f32 / hr as f32 * 45.
                    - 30.;
                ((h + 500.) / 1000. * 32766.).round() as i16
            })
            .collect(),
        splat: (0..8 * cells)
            .map(|i| ((i.wrapping_mul(13) + i / cells * 71) % 256) as u8)
            .collect(),
        biome: vec![0; 5 * cells],
        topology: vec![0; cells],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let (installation, mut doc) = palette_fixture();
    let material = material_fixture();
    doc["cartography"] = serde_json::to_value(&material).unwrap();
    let palette = mapper_studio::terrain_palette::Palette::decode(
        &serde_json::to_vec(&doc).unwrap(),
        &installation,
    )
    .unwrap();
    let mut limits = Limits::default();
    let saved = mapper_studio::generation_resources::current();
    // Limits reject before adapter startup or any image allocation.
    limits.gpu_memory_mib = 32;
    assert!(
        mapper_studio::map_compute::render(&layers, None, None, 4096, &material, limits, || false)
            .unwrap_err()
            .contains("budget")
    );
    limits.gpu_memory_mib = 256;
    for output in [256, 1024, 2048] {
        let mut baseline = None;
        for workers in [1, 8] {
            limits.cpu_workers = workers;
            limits.image_compute = ImageCompute::Cpu;
            configure(limits).unwrap();
            let now = std::time::Instant::now();
            let image = render_with_palette(&layers, None, None, output, Some(&palette), || false)
                .unwrap()
                .unwrap();
            println!(
                "cpu output={output} workers={workers} ms={:.3}",
                now.elapsed().as_secs_f64() * 1000.
            );
            if let Some(ref base) = baseline {
                assert_eq!(&image, base);
            } else {
                baseline = Some(image);
            }
        }
        for run in 0..3 {
            let now = std::time::Instant::now();
            let gpu = mapper_studio::map_compute::render(
                &layers,
                None,
                None,
                output,
                &material,
                limits,
                || false,
            )
            .unwrap()
            .unwrap();
            let ms = now.elapsed().as_secs_f64() * 1000.;
            let max = gpu
                .pixels
                .iter()
                .zip(&baseline.as_ref().unwrap().pixels)
                .flat_map(|(a, b)| {
                    a.to_array()
                        .into_iter()
                        .zip(b.to_array())
                        .map(|(a, b)| a.abs_diff(b))
                })
                .max()
                .unwrap();
            println!("gpu output={output} run={run} ms={ms:.3} max_channel_delta={max}");
            assert!(max <= 1, "GPU cartography differs from CPU by {max}");
        }
    }
    let ocean: Vec<u8> = (0..cells).map(|i| if i % 3 == 0 { 1 } else { 0 }).collect();
    let water: Vec<i16> = (0..hr * hr)
        .map(|i| if i % 4 == 0 { 0 } else { 17039 })
        .collect();
    let mask = papyrus_terrain::ocean::Mask {
        resolution: sr,
        classification: ocean.clone(),
    };
    for shader_id in [
        None,
        Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07"),
    ] {
        let mut material = material.clone();
        material.shader_blob_sha256 = shader_id.map(str::to_owned);
        // Deliberately stronger contrast so a skipped GPU branch cannot pass due
        // to rounding or an almost-neutral color. Exercise ocean and local water.
        material.contrast = 0.4;
        doc["cartography"] = serde_json::to_value(&material).unwrap();
        let palette = mapper_studio::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        for (mask, water) in [
            (Some(&mask), None),
            (None, Some(water.as_slice())),
            (Some(&mask), Some(water.as_slice())),
        ] {
            let cpu =
                render_with_ocean_mask(&layers, None, mask, water, 256, Some(&palette), || false)
                    .unwrap()
                    .unwrap();
            let gpu = mapper_studio::map_compute::render(
                &layers,
                mask.map(|m| m.classification.as_slice()),
                water,
                256,
                &material,
                limits,
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(cpu.pixels.iter().zip(gpu.pixels).all(|(a, b)| {
                a.to_array()
                    .into_iter()
                    .zip(b.to_array())
                    .all(|(a, b)| a.abs_diff(b) <= 1)
            }));
        }
    }
    assert!(
        mapper_studio::map_compute::render(&layers, None, None, 32, &material, limits, || true)
            .unwrap()
            .is_none()
    );
    configure(saved).unwrap();
}

// Synthetic materials and source receipts; no installed game files required.
fn material_fixture() -> mapper_studio::map_colors::Material {
    mapper_studio::map_colors::Material {
        material_path: "assets/prefabs/tools/map/papermap.mat".into(),
        serialized_file: "CAB-synthetic".into(),
        material_object: 1,
        shader_object: 2,
        shader_name: "Custom/Map".into(),
        shader_blob_sha256: None,
        colors: std::array::from_fn(|i| [i as f32 / 12., 0.3, 0.5, 1.]),
        sun_direction: [0.95, 2.87, 2.37],
        sun_power: 0.65,
        brightness: 1.05,
        contrast: 0.94,
    }
}
fn palette_fixture() -> (mapper_studio::installation::Installation, serde_json::Value) {
    use mapper_studio::installation::{Installation, InstallationKind, SourceFile};
    use serde_json::json;
    let source = SourceFile {
        relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
        sha256: "a".repeat(64),
        bytes: 1,
    };
    let install = Installation {
        kind: InstallationKind::Client,
        native: source.clone(),
        scene_manifest: None,
    };
    let materials=(0..8).map(|i|json!({"name":format!("Material {i}"),"biome_colors":(0..5).map(|j|[i as f32/8.,j as f32/5.,0.5,1.]).collect::<Vec<_>>()})).collect::<Vec<_>>();
    let doc = json!({"format":"papyrus-terrain-palette","schema":1,"adapter":"papyrus-terrain-palette-1","extractor_sha256":"b".repeat(64),
        "sources":[source,{"relative_path":"Bundles/maps/maps.bundle","sha256":"c".repeat(64),"bytes":1},{"relative_path":"Bundles/shared/content.bundle","sha256":"d".repeat(64),"bytes":1}],
        "scene":"BuildPlayer-Procedural Map","binding":{"scene_file":"BuildPlayer-Procedural Map","world_setup":1,"terrain":2,"generator":3,"config_file":"CAB-config","config_object":4},
        "config_paths":["assets/test.asset"],"numeric_space":"unity-serialized-color","biomes":["Arid","Temperate","Tundra","Arctic","Jungle"],"materials":materials});
    (install, doc)
}
