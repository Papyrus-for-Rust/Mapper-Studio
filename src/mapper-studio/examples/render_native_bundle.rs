//! Re-render a verified composed native preview bundle without scanning game assets.
use mapper_studio::{Project, map_image};
use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io::BufWriter, path::Path};
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let require_prepared = args
        .get(4)
        .is_some_and(|s| s == "--require-prepared-inputs");
    if !(2..=5).contains(&args.len()) || (args.len() == 5 && !require_prepared) {
        return Err(
            "render_native_bundle BUNDLE NEW_PNG [RUST_ROOT [NEW_SURFACE_REPORT_JSON [--require-prepared-inputs]]]".into(),
        );
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    let project = Project::load(&root.join("project.mapper"))?;
    // Image inspection only needs retained terrain channels, not the additional
    // placement-blocker declarations required by rail-routing diagnostics.
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["size"] != project.size
        || manifest["height_resolution"] != hr
        || manifest["surface_resolution"] != sr
        || manifest["biome_channels"] != 5
        || manifest["splat_channels"] != 8
    {
        return Err("Unsupported native bundle layout".into());
    }
    let checked = |name: &str, bytes: usize| -> Result<Vec<u8>, String> {
        use std::io::Read;
        let mut raw = Vec::new();
        std::fs::File::open(root.join(name))
            .map_err(|e| e.to_string())?
            .take(bytes as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() != bytes
            || manifest["files"][name]
                != serde_json::json!({"bytes":bytes,"sha256":format!("{:x}",Sha256::digest(&raw))})
        {
            return Err(format!("Bundle receipt mismatch: {name}"));
        }
        Ok(raw)
    };
    let layers = papyrus_terrain::native_layers::Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: checked("height.i16le", hr * hr * 2)?
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect(),
        topology: checked("topology.i32le", sr * sr * 4)?
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        biome: checked("biome.u8", sr * sr * 5)?,
        splat: checked("splat.u8", sr * sr * 8)?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let alpha = checked("alpha.u8", sr * sr)?;
    let palette = if let Some(rust) = args.get(2) {
        mapper_studio::terrain_palette::Palette::discover(
            Path::new(rust),
            project
                .installation
                .as_ref()
                .ok_or("Missing saved installation")?,
            || false,
        )?
    } else {
        None
    };
    let mut ocean = papyrus_terrain::ocean::reconstruct(
        &layers.height,
        layers.height_resolution,
        &layers.topology,
        layers.surface_resolution,
        || false,
    )?
    .ok_or("Ocean reconstruction cancelled")?;
    // Keep receipt-covered registered water when repainting a composed bundle.
    // Reconstructing only the ocean would silently drop its freshwater levels.
    let stored_ocean = manifest["files"].get("ocean-classification.u8").is_some();
    let stored_water = manifest["files"].get("local-water.i16le").is_some();
    if stored_ocean != stored_water {
        return Err("Incomplete retained water layers".into());
    }
    if stored_ocean {
        ocean.classification = checked("ocean-classification.u8", sr * sr)?;
        if ocean.classification.iter().any(|v| *v > 2) {
            return Err("Invalid retained ocean classification".into());
        }
        ocean.local_water = checked("local-water.i16le", hr * hr * 2)?
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect();
        ocean.ocean_cells = ocean
            .classification
            .iter()
            .filter(|v| **v == papyrus_terrain::ocean::OCEAN)
            .count();
    }
    let gameplay_height = if manifest["files"].get("gameplay-height.i16le").is_some() {
        Some(papyrus_terrain::gameplay_height::Output {
            height: checked("gameplay-height.i16le", hr * hr * 2)?
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect(),
            report: papyrus_terrain::gameplay_height::Report {
                engine: papyrus_terrain::gameplay_height::VERSION,
                contributors: 0,
                changed_samples: 0,
                work: 0,
                physics_cooking_validated: false,
            },
        })
    } else {
        None
    };
    let maps = papyrus_terrain::monument_composition::Composed {
        custom_baked: Vec::new(),
        layers,
        alpha,
        ocean: Some(ocean),
        gameplay_height,
        rivers: None,
        rails: None,
        objects: None,
        dungeons: None, tunnels: None,
        offshore: None,
        clutter_topology: None,
        cliff_topology: None,
        topology_before_overrides: None,
        topology_overrides: None,
        spawn_audit: None,
        decoration: None,
        roads: None,
        clamped_height_samples: 0,
    };
    let maps = std::sync::Arc::new(maps);
    let started = std::time::Instant::now();
    let rendered = map_image::render_composed_owned(
        &maps,
        map_image::DISPLAY_RESOLUTION,
        palette.as_ref(),
        || false,
    )?
    .ok_or("Image rendering cancelled")?;
    if require_prepared
        && rendered
            .surface
            .as_ref()
            .and_then(|f| f.composed(&maps))
            .and_then(|g| g.normal())
            .is_none()
    {
        return Err(
            "Required prepared map inputs are absent; this palette is using the legacy renderer"
                .into(),
        );
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    let mut point_samples = 0;
    let mut maximum_point_delta = 0;
    if let Some(palette) = &palette {
        for row in (0..map_image::DISPLAY_RESOLUTION).step_by(32) {
            for col in (0..map_image::DISPLAY_RESOLUTION).step_by(32) {
                let point = [
                    ((col as f32 + 0.5) / map_image::DISPLAY_RESOLUTION as f32 - 0.5)
                        * project.size as f32,
                    (0.5 - (row as f32 + 0.5) / map_image::DISPLAY_RESOLUTION as f32)
                        * project.size as f32,
                ];
                let sampled = map_image::sample_composed_surface(
                    &maps,
                    point,
                    palette,
                    rendered.surface.as_deref(),
                )
                .ok_or("Missing prepared point surface")?;
                let pixel = rendered.image.pixels[row * map_image::DISPLAY_RESOLUTION + col];
                for (a, b) in sampled.to_array().into_iter().zip(pixel.to_array()) {
                    maximum_point_delta = maximum_point_delta.max(a.abs_diff(b));
                }
                point_samples += 1;
            }
        }
    }
    if maximum_point_delta > 1 {
        return Err(format!(
            "Overview/point query mismatch: {maximum_point_delta}"
        ));
    }
    let again = map_image::prepare_composed_surface(
        &maps,
        map_image::DISPLAY_RESOLUTION,
        palette.as_ref(),
        || false,
    )?;
    let reused = rendered
        .surface
        .as_ref()
        .zip(again.as_ref())
        .is_some_and(|(a, b)| std::sync::Arc::ptr_eq(a, b));
    let surface_report=rendered.surface.as_ref().and_then(|f|f.composed(&maps)).map(|g|serde_json::json!({
        "profile":mapper_studio::map_surface_frame::PROFILE,"channels_used":["filtered_slope","packed_bc3_normal"],"resolution":g.resolution(),"slope_work":g.report.work,
        "working_bytes":g.working_bytes(),"retained_bytes":g.retained_bytes(),"cache_reused":reused,
        "normal":g.normal().map(|n|serde_json::json!({"engine":n.report.engine,"resolution":n.resolution(),"pixel_bytes":n.report.pixel_bytes,
            "work_reserved":n.report.work_reserved,"pixel_sha256":format!("{:x}",Sha256::digest(n.pixels().as_flattened()))}))}));
    let image = rendered.image;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(
        BufWriter::new(file),
        image.size[0] as u32,
        image.size[1] as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let pixels = image
        .pixels
        .iter()
        .flat_map(eframe::egui::Color32::to_srgba_unmultiplied)
        .collect::<Vec<_>>();
    writer
        .write_image_data(&pixels)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    println!(
        "{}x{} verified native overview: {}",
        image.size[0],
        image.size[1],
        output.display()
    );
    if let Some(path) = args.get(3) {
        use std::io::Write;
        let report = serde_json::json!({"format":"mapper-bound-surface-render-check","schema":1,
            "bundle":root,"source_root":args.get(2).map(|s|s.to_string_lossy().into_owned()),"png_sha256":format!("{:x}",Sha256::digest(std::fs::read(output).map_err(|e|e.to_string())?)),
            "render_ms":elapsed_ms,"surface":surface_report,"point_samples":point_samples,"maximum_point_channel_delta":maximum_point_delta,
            "world_regenerated":false,"game_world_launched":false});
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    if let Some(palette) = palette {
        println!(
            "Installation colors: {} ({} biomes)",
            palette.sha256,
            palette.biome_count()
        );
    }
    Ok(())
}
