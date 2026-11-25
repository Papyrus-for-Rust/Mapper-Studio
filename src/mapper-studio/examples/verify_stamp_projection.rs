//! Opt-in native-grid projection oracle. Uses owned, verified cached assets only.
use mapper_studio::{Project, composition, prefab_assets, prefab_textures};
use papyrus_terrain::monument_composition::{self, projection};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::Path,
};

fn png(path: &Path, image: &eframe::egui::ColorImage) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        image.size[0] as u32,
        image.size[1] as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let bytes = image
        .pixels
        .iter()
        .flat_map(eframe::egui::Color32::to_srgba_unmultiplied)
        .collect::<Vec<_>>();
    writer.write_image_data(&bytes).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

fn checked(
    root: &Path,
    manifest: &serde_json::Value,
    name: &str,
    length: usize,
) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(root.join(name))
        .map_err(|e| e.to_string())?
        .take(length as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() != length
        || manifest["files"][name]
            != serde_json::json!({"bytes":length,"sha256":format!("{:x}",Sha256::digest(&raw))})
    {
        return Err(format!("Bundle receipt mismatch: {name}"));
    }
    Ok(raw)
}

fn layers(
    root: &Path,
    manifest: &serde_json::Value,
    project: &Project,
) -> Result<(papyrus_terrain::native_layers::Layers, Vec<u8>), String> {
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["playable"] != false
        || manifest["composed"] != true
        || manifest["size"] != project.size
        || manifest["height_resolution"] != hr
        || manifest["surface_resolution"] != sr
        || manifest["biome_channels"] != 5
        || manifest["splat_channels"] != 8
    {
        return Err("Unsupported composed native terrain bundle".into());
    }
    Ok((
        papyrus_terrain::native_layers::Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: checked(root, manifest, "height.i16le", hr * hr * 2)?
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect(),
            topology: checked(root, manifest, "topology.i32le", sr * sr * 4)?
                .chunks_exact(4)
                .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
            biome: checked(root, manifest, "biome.u8", sr * sr * 5)?,
            splat: checked(root, manifest, "splat.u8", sr * sr * 8)?,
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        },
        checked(root, manifest, "alpha.u8", sr * sr)?,
    ))
}

fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !(5..=6).contains(&args.len()) {
        return Err(
            "verify_stamp_projection BUNDLE RUST_ROOT CATALOG BINDINGS NEW_REPORT_JSON [NEW_IMAGES_DIRECTORY]".into(),
        );
    }
    let bundle = Path::new(&args[0]);
    let root = Path::new(&args[1]);
    let output = Path::new(&args[4]);
    if output.exists() {
        return Err("Choose a new diagnostic report path".into());
    }
    let image_output = args.get(5).map(|p| Path::new(p).to_path_buf());
    if image_output.as_ref().is_some_and(|p| p.exists()) {
        return Err("Choose a new image directory".into());
    }
    let image_stage = if let Some(path) = &image_output {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Some(
            tempfile::Builder::new()
                .prefix(".stamp-water-")
                .tempdir_in(parent)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let project = Project::load(&bundle.join("project.mapper"))?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing saved installation")?;
    let catalog = prefab_assets::load(root, Path::new(&args[2]), installation, || false)?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[3]), &catalog, || false)?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let (layers, alpha) = layers(bundle, &manifest, &project)?;
    let project_bytes = manifest["files"]["project.mapper"]["bytes"]
        .as_u64()
        .filter(|n| *n <= 1024 * 1024)
        .ok_or("Invalid project receipt")? as usize;
    let saved: Project = serde_json::from_slice(&checked(
        bundle,
        &manifest,
        "project.mapper",
        project_bytes,
    )?)
    .map_err(|e| e.to_string())?;
    if saved != project {
        return Err("Project changed while loading the diagnostic".into());
    }
    let raw = std::fs::read(bundle.join("layout.json")).map_err(|e| e.to_string())?;
    if manifest["files"]["layout.json"]
        != serde_json::json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))})
    {
        return Err("Layout receipt mismatch".into());
    }
    let layout: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let source = projection::Terrain::new(&layers, &alpha, || false)?.ok_or("Cancelled")?;
    let n = layers.surface_resolution;
    let cells = n * n;
    let hr = layers.height_resolution;
    let local_water = if manifest["files"].get("local-water.i16le").is_some() {
        Some(
            checked(bundle, &manifest, "local-water.i16le", hr * hr * 2)?
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let palette = if manifest["files"].get("terrain-palette.json").is_some() {
        let bytes = manifest["files"]["terrain-palette.json"]["bytes"]
            .as_u64()
            .filter(|n| *n <= 1024 * 1024)
            .ok_or("Invalid palette receipt")? as usize;
        checked(bundle, &manifest, "terrain-palette.json", bytes)?;
        mapper_studio::terrain_palette::Palette::load(
            root,
            &bundle.join("terrain-palette.json"),
            installation,
            || false,
        )?
    } else {
        None
    };
    let connected =
        project.composition_engine.as_deref() == Some(monument_composition::WATER_VERSION);
    let mut cases = Vec::new();
    for target in &project.targets {
        let Some(path) = &target.prefab_path else {
            continue;
        };
        let Some(outcome) = layout["outcomes"]
            .as_array()
            .ok_or("Missing layout outcomes")?
            .iter()
            .find(|o| o["id"].as_u64() == Some(target.instance_id))
        else {
            continue;
        };
        let p = &outcome["position"];
        if p.is_null() {
            continue;
        }
        let position = [
            p["x"].as_f64().ok_or("Missing placement x")?,
            p["y"].as_f64().ok_or("Missing placement y")?,
            p["z"].as_f64().ok_or("Missing placement z")?,
        ];
        let asset = catalog.entries.get(path).ok_or("Missing saved prefab")?;
        let prepared =
            composition::prepare(asset, &bindings, project.size, || false)?.ok_or("Cancelled")?;
        let radius = prepared.placement_clearance([0.; 2]);
        let mut rotations = vec![0., 37., 123., 271.];
        if image_stage.is_some() {
            rotations.insert(0, p["yaw"].as_f64().ok_or("Missing saved heading")?);
        }
        for (case_index, yaw) in rotations.into_iter().enumerate() {
            let stamp = prepared.stamp(position, yaw);
            let projected = source.projected(&stamp)?;
            let committed = monument_composition::compose(
                &layers,
                &alpha,
                std::slice::from_ref(&stamp),
                || false,
            )?
            .ok_or("Cancelled")?;
            let mut probes = BTreeSet::new();
            for z in 0..128 {
                for x in 0..128 {
                    let cell = |center: f64, i: usize| {
                        (((center as f32 + (i as f32 / 127. * 2. - 1.) * radius)
                            / layers.size as f32
                            + 0.5)
                            * n as f32)
                            .floor()
                            .clamp(0., (n - 1) as f32) as usize
                    };
                    probes.insert((cell(position[0], x), cell(position[2], z)));
                }
            }
            probes.extend([(0, 0), (n - 1, 0), (0, n - 1), (n - 1, n - 1)]);
            let mut changed = 0;
            let mut maximum_height_error = 0_f32;
            for &(x, z) in &probes {
                let i = z * n + x;
                let actual = projected.cell(x, z).ok_or("Invalid projection cell")?;
                let expected = projection::Cell {
                    splat: std::array::from_fn(|c| committed.layers.splat[c * cells + i]),
                    biome: std::array::from_fn(|c| committed.layers.biome[c * cells + i]),
                    alpha: committed.alpha[i],
                    topology: committed.layers.topology[i],
                };
                if actual != expected {
                    return Err(format!(
                        "Projection mismatch for {path} at yaw {yaw}, cell {x},{z}"
                    ));
                }
                changed += usize::from(
                    actual.splat != std::array::from_fn(|c| layers.splat[c * cells + i])
                        || actual.biome != std::array::from_fn(|c| layers.biome[c * cells + i])
                        || actual.alpha != alpha[i]
                        || actual.topology != layers.topology[i],
                );
                let hx = x * (hr - 1) / (n - 1);
                let hz = z * (hr - 1) / (n - 1);
                let actual = projected
                    .height(hx as f32 / (hr - 1) as f32, hz as f32 / (hr - 1) as f32)
                    .ok_or("Invalid projection height")?;
                let expected = papyrus_terrain::erosion::decode(committed.layers.height[hz * hr + hx]);
                maximum_height_error = maximum_height_error.max((actual - expected).abs());
            }
            if maximum_height_error > 0.0002 {
                return Err(format!(
                    "Projected native height mismatch: {maximum_height_error}"
                ));
            }
            if connected {
                let mask = projected.ocean(|| false)?.ok_or("Cancelled")?;
                let full = papyrus_terrain::ocean::reconstruct(
                    &committed.layers.height,
                    hr,
                    &committed.layers.topology,
                    n,
                    || false,
                )?
                .ok_or("Cancelled")?;
                if mask.classification != full.classification {
                    return Err(format!("Projected ocean differs for {path} at yaw {yaw}"));
                }
            }
            let mut image = None;
            if let Some(stage) = &image_stage {
                let start = std::time::Instant::now();
                let patch = mapper_studio::stamp_projection::render(
                    &projected,
                    mapper_studio::stamp_projection::Settings {
                        maximum_dimension: 256,
                        palette: palette.as_ref(),
                        connected_ocean: connected,
                        local_water: local_water.as_deref(),
                    },
                    || false,
                )?
                .ok_or("Cancelled or fully clipped projection")?;
                let cpu_ms = start.elapsed().as_secs_f64() * 1000.;
                let name = format!("monument-{}-case-{case_index}.png", target.instance_id);
                png(&stage.path().join(&name), &patch.image)?;
                image = Some(
                    serde_json::json!({"file":name,"world_bounds":patch.world_bounds,"dimensions":patch.image.size,"approximate_colors":patch.approximate_colors,"cpu_ms":cpu_ms}),
                );
            }
            cases.push(serde_json::json!({"prefab":path,"position":position,"yaw":yaw,"surface_cells":probes.len(),"changed_surface_cells":changed,"maximum_native_height_error_metres":maximum_height_error,"surface_cells_byte_equal":true,"ocean_cells_compared":if connected {n*n}else{0},"image":image}));
        }
    }
    if cases.is_empty() {
        return Err("No accepted source-bound monuments were exercised".into());
    }
    let report = serde_json::json!({"format":"mapper-native-stamp-projection-verification","schema":1,"bundle_manifest_sha256":format!("{:x}",Sha256::digest(std::fs::read(bundle.join("manifest.json")).map_err(|e|e.to_string())?)),"catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"case_count":cases.len(),"cases":cases,"background":"retained verified composed native terrain; each new stamp is applied independently over that terrain; original stamps are not removed","ocean_reclassification_compared":connected,"water_meshes_compared":false,"image_directory":image_output,"desktop_windows_opened":0,"viewport_integrated":false});
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    temp.as_file_mut().sync_all().map_err(|e| e.to_string())?;
    if let Some(stage) = image_stage {
        std::fs::rename(stage.path(), image_output.as_ref().unwrap()).map_err(|e| e.to_string())?;
    }
    temp.persist_noclobber(output).map_err(|e| e.to_string())?;
    println!("Verified {} native projection cases", report["case_count"]);
    Ok(())
}
