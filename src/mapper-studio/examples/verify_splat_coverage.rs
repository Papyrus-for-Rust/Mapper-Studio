//! Explicit local check of preview filtering using user-owned verified textures.
//! Generated images remain local; this does not validate game terrain rendering.
use eframe::egui::{Color32, ColorImage};
use mapper_studio::{installation, prefab_assets, prefab_textures, splat_preview};
use std::{fs::File, io::BufWriter, path::Path};

fn png(path: &Path, image: &ColorImage) -> Result<(), String> {
    let mut encoder = png::Encoder::new(
        BufWriter::new(File::create(path).map_err(|e| e.to_string())?),
        image.size[0] as u32,
        image.size[1] as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let bytes = image
        .pixels
        .iter()
        .flat_map(Color32::to_srgba_unmultiplied)
        .collect::<Vec<_>>();
    writer.write_image_data(&bytes).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}
fn mean_alpha(image: &ColorImage) -> f64 {
    image.pixels.iter().map(|p| f64::from(p.a())).sum::<f64>() / image.pixels.len() as f64
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("verify_splat_coverage RUST_ROOT CATALOG BINDINGS NEW_OUTPUT_DIRECTORY".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[3]);
    if output.exists() {
        return Err("Choose a new preview directory".into());
    }
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::Builder::new()
        .prefix(".splat-coverage-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut assets = catalog.entries.iter().collect::<Vec<_>>();
    assets.sort_by_key(|(p, _)| *p);
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    let mut pictures = 0;
    let mut biome_previews = 0;
    let mut biome_pictures = 0;
    let terrain = papyrus_terrain::native_layers::Layers {
        size: 4000,
        terrain_seed: 1,
        biome_seed: 2,
        height_resolution: 2,
        surface_resolution: 2,
        height: vec![18000; 4],
        biome: [vec![0; 12], vec![255; 4], vec![0; 4]].concat(),
        splat: [vec![0; 16], vec![255; 4], vec![0; 12]].concat(),
        topology: vec![0; 4],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    for (path, asset) in assets {
        let preview = match splat_preview::load(&bindings, asset, || false) {
            Ok(Some(p)) => p,
            Ok(None) => return Err("Unexpected cancellation".into()),
            Err(error) => {
                failures.push(serde_json::json!({"prefab":path,"error":error}));
                continue;
            }
        };
        let reduced = splat_preview::contextual_base(&preview.image);
        let before = mean_alpha(&preview.image);
        let after = mean_alpha(&reduced);
        if (before - after).abs() > 0.501 {
            return Err(format!("Coverage changed excessively: {path}"));
        }
        let biome = preview.biome.as_ref().map(splat_preview::contextual_base);
        let mut context_ms = None;
        if let Some(biome) = &biome {
            biome_previews += 1;
            let start = std::time::Instant::now();
            let after = splat_preview::contextual_image_with_biome(
                &reduced,
                Some(biome),
                preview.geometry,
                &terrain,
                [0., 0., 37.],
            );
            context_ms = Some(start.elapsed().as_secs_f64() * 1000.);
            let coverage = splat_preview::contextual_coverage(&reduced, biome);
            assert!(
                after
                    .pixels
                    .iter()
                    .zip(&coverage.pixels)
                    .all(|(a, b)| a.a() == b.a())
            );
            if biome_pictures < 3
                && (path.ends_with("compound.prefab")
                    || path.contains("bandit_town")
                    || path.contains("arctic_research"))
            {
                let before = splat_preview::contextual_image(
                    &reduced,
                    preview.geometry,
                    &terrain,
                    [0., 0., 37.],
                );
                png(
                    &stage
                        .path()
                        .join(format!("biome-{biome_pictures}-before.png")),
                    &before,
                )?;
                png(
                    &stage
                        .path()
                        .join(format!("biome-{biome_pictures}-after.png")),
                    &after,
                )?;
                biome_pictures += 1;
            }
        }
        let [w, h] = reduced.size;
        let mut old = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let sx = (x as f32 / (w - 1).max(1) as f32 * (preview.image.size[0] - 1) as f32)
                    .round() as usize;
                let sy = (y as f32 / (h - 1).max(1) as f32 * (preview.image.size[1] - 1) as f32)
                    .round() as usize;
                old.push(preview.image.pixels[sy * preview.image.size[0] + sx]);
            }
        }
        let old = ColorImage::new([w, h], old);
        entries.push(serde_json::json!({"prefab":path,"source_size":preview.image.size,"filtered_size":reduced.size,
            "biome_paint":biome.is_some(),"contextual_cpu_ms":context_ms,"source_mean_alpha":before,"filtered_mean_alpha":after,"old_nearest_mean_alpha":mean_alpha(&old)}));
        if pictures < 4
            && (path.contains("airfield")
                || path.contains("harbor_1")
                || path.contains("satellite_dish"))
        {
            png(
                &stage.path().join(format!("{pictures}-source.png")),
                &preview.image,
            )?;
            png(
                &stage.path().join(format!("{pictures}-filtered.png")),
                &reduced,
            )?;
            png(&stage.path().join(format!("{pictures}-nearest.png")), &old)?;
            pictures += 1;
        }
    }
    if entries.is_empty() {
        return Err("No supported material stamps were verified".into());
    }
    let report = serde_json::json!({"verified":entries.len(),"biome_previews":biome_previews,"biome_example_background":"flat synthetic snowy biome with grass material; yaw 37 degrees","entries":entries,"unsupported":failures,
        "coverage_error_bound_alpha_units":0.501,"terrain_modified":false,"game_rendering_validated":false});
    std::fs::write(
        stage.path().join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(stage.path(), output).map_err(|e| e.to_string())?;
    println!(
        "Verified {} source-backed material previews",
        report["verified"]
    );
    Ok(())
}
