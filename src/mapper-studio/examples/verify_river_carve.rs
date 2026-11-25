//! Headless river layout + terrain study on a verified immutable native cache.
//! No project, released app, source cache or selected installation is modified.
use mapper_studio::{Project, native_noise, native_preview, native_profile, terrain_cache};
use papyrus_terrain::{river_carve, river_layout, river_pipeline, river_surface, river_water};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
    time::Instant,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutStudy {
    format: String,
    schema: u32,
    assembly_sha256: String,
    scope: String,
    large_world_above: u32,
    large_world_maximum_rivers: usize,
    policy: river_layout::Policy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TerrainStudy {
    format: String,
    schema: u32,
    assembly_sha256: String,
    layout_study_sha256: String,
    scope: String,
    terrain_origin_y: f32,
    terrain_height: f32,
    policy: river_carve::Policy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceStudy {
    format: String,
    schema: u32,
    assembly_sha256: String,
    layout_study_sha256: String,
    terrain_study_sha256: String,
    scope: String,
    policy: river_surface::Policy,
}
fn read<T: DeserializeOwned>(path: &Path) -> Result<(T, Vec<u8>), String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("Study exceeds 64 KiB".into());
    }
    Ok((
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        bytes,
    ))
}
fn hash(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut buffer = [0; 65536];
    let mut hash = Sha256::new();
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn words(path: &Path, height: &[i16]) -> Result<String, String> {
    let mut file = BufWriter::new(
        File::options()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?,
    );
    let mut hash = Sha256::new();
    let mut buffer = Vec::with_capacity(65536);
    for chunk in height.chunks(32768) {
        buffer.clear();
        for h in chunk {
            buffer.extend(h.to_le_bytes());
        }
        hash.update(&buffer);
        file.write_all(&buffer).map_err(|e| e.to_string())?;
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(format!("{:x}", hash.finalize()))
}
fn json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, value).map_err(|e| e.to_string())
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(8..=10).contains(&args.len()) && args.len() != 12 {
        return Err("verify_river_carve RUST_ROOT WORKER PROJECT TERRAIN_CACHE LAYOUT_STUDY TERRAIN_STUDY SERVER_ASSEMBLY NEW_DIRECTORY [SURFACE_STUDY [WATER_MESH_SOURCE [CATALOG BINDINGS]]]".into());
    }
    let output = Path::new(&args[7]);
    if output.exists() {
        return Err("Output already exists".into());
    }
    let root = Path::new(&args[0]);
    let worker = Path::new(&args[1]);
    let assembly = Path::new(&args[6]);
    let (mut shape, layout_bytes): (LayoutStudy, _) = read(Path::new(&args[4]))?;
    let (terrain, terrain_bytes): (TerrainStudy, _) = read(Path::new(&args[5]))?;
    let assembly_hash = hash(assembly)?;
    let surface_study = if args.len() >= 9 {
        let (s, bytes): (SurfaceStudy, _) = read(Path::new(&args[8]))?;
        s.policy.validate()?;
        if s.format != "papyrus-river-surface-study"
            || s.schema != 1
            || s.scope.is_empty()
            || s.assembly_sha256 != assembly_hash
            || s.layout_study_sha256 != format!("{:x}", Sha256::digest(&layout_bytes))
            || s.terrain_study_sha256 != format!("{:x}", Sha256::digest(&terrain_bytes))
        {
            return Err("Surface study does not match terrain/layout source receipts".into());
        }
        Some((s, bytes))
    } else {
        None
    };
    if shape.format != "papyrus-river-layout-study"
        || terrain.format != "papyrus-river-terrain-study"
        || shape.schema != 1
        || terrain.schema != 1
        || shape.scope.is_empty()
        || terrain.scope.is_empty()
        || shape.assembly_sha256 != assembly_hash
        || terrain.assembly_sha256 != assembly_hash
        || terrain.layout_study_sha256 != format!("{:x}", Sha256::digest(&layout_bytes))
        || !(1000..=6000).contains(&shape.large_world_above)
        || shape.large_world_maximum_rivers > 64
    {
        return Err("River studies do not match each other and the reference assembly".into());
    }
    shape.policy.validate()?;
    terrain.policy.validate()?;
    let project = Project::load(Path::new(&args[2]))?;
    let key = project.terrain_key();
    let profile = native_profile::resolve(&key)?;
    let cancel = AtomicBool::new(false);
    if !native_preview::verify_source(root, &key, &cancel)? {
        return Err("Source verification cancelled".into());
    }
    let water_source = if args.len() >= 10 {
        let source = mapper_studio::river_mesh_source::Source::load(
            root,
            Path::new(&args[9]),
            key.source.as_ref().ok_or("Missing installation")?,
            || false,
        )?
        .ok_or("Water template loading cancelled")?;
        if source.reference_assembly() != assembly_hash {
            return Err("Water mesh profile has a different reference assembly".into());
        }
        Some(source)
    } else {
        None
    };
    let source = terrain_cache::load(Path::new(&args[3]), &key, || false)?
        .ok_or("Missing exact saved terrain variant")?;
    if project.size > shape.large_world_above {
        shape.policy.maximum_rivers = shape.large_world_maximum_rivers;
    }
    let mut layers = source.layers.as_ref().clone();
    layers.topology = papyrus_terrain::ocean::reconstruct(
        &layers.height,
        layers.height_resolution,
        &layers.topology,
        layers.surface_resolution,
        || false,
    )?
    .ok_or("Ocean cancelled")?
    .topology;
    let sample = |coordinates: &[f32]| -> Result<Vec<f32>, String> {
        if coordinates.is_empty() {
            return Ok(vec![]);
        }
        native_noise::sample(
            root,
            worker,
            key.source.as_ref().ok_or("Missing installation")?,
            &profile,
            coordinates,
            || false,
        )?
        .ok_or_else(|| "Noise sampling cancelled".into())
    };
    let layout_noise = sample(&shape.policy.noise_coordinates()?)?;
    let layout = river_layout::generate(
        &layers,
        &shape.policy,
        &layout_noise,
        &river_layout::Settings {
            seed: Some(project.seeds[2].value),
            maximum_work: 512_000_000,
            maximum_candidates: 4096,
            maximum_controls: 1_048_576,
        },
        || false,
    )?
    .ok_or("Layout cancelled")?;
    let topology_pass = if let (Some((s, _)), Some(_)) = (&surface_study, &water_source) {
        let plan =
            river_surface::prepare(&layout.paths, &shape.policy, &s.policy, 512_000_000, || {
                false
            })?
            .ok_or("Topology preparation cancelled")?;
        let noise = sample(&plan.noise_coordinates())?;
        Some(
            plan.apply_pass(
                river_surface::Grid {
                    size: project.size as f32,
                    height_resolution: layers.height_resolution,
                    surface_resolution: layers.surface_resolution,
                    origin_y: terrain.terrain_origin_y,
                    vertical_span: terrain.terrain_height,
                    height: &layers.height,
                    splat: &layers.splat,
                    topology: &layers.topology,
                },
                &noise,
                river_surface::Pass::Topology,
                || false,
            )?
            .ok_or("Topology cancelled")?,
        )
    } else {
        None
    };
    let prepared = river_carve::prepare(
        &layout.paths,
        &shape.policy,
        &terrain.policy,
        512_000_000,
        || false,
    )?
    .ok_or("Preparation cancelled")?;
    let noise = sample(&prepared.noise_coordinates())?;
    let grid = || river_carve::Grid {
        size: project.size as f32,
        resolution: layers.height_resolution,
        origin_y: terrain.terrain_origin_y,
        vertical_span: terrain.terrain_height,
        height: &layers.height,
    };
    let start = Instant::now();
    let carved = prepared
        .carve(grid(), &noise, || false)?
        .ok_or("Carving cancelled")?;
    let first_ms = start.elapsed().as_millis();
    let replay = prepared
        .carve(grid(), &noise, || false)?
        .ok_or("Replay cancelled")?;
    if carved.height != replay.height
        || source.layers.height != layers.height
        || carved.report.changed_samples == 0
        || layout.paths.is_empty()
        || hash(assembly)? != assembly_hash
    {
        return Err(
            "Carving replay differs, source changed or study generated no terrain changes".into(),
        );
    }
    let mut surface_ms = None;
    let surfaces = if let Some((s, _)) = &surface_study {
        let plan =
            river_surface::prepare(&carved.paths, &shape.policy, &s.policy, 512_000_000, || {
                false
            })?
            .ok_or("Surface preparation cancelled")?;
        let noise = sample(&plan.noise_coordinates())?;
        let grid = || river_surface::Grid {
            size: project.size as f32,
            height_resolution: layers.height_resolution,
            surface_resolution: layers.surface_resolution,
            origin_y: terrain.terrain_origin_y,
            vertical_span: terrain.terrain_height,
            height: &carved.height,
            splat: &layers.splat,
            topology: topology_pass
                .as_ref()
                .map_or(&layers.topology, |v| &v.topology),
        };
        let start = Instant::now();
        let result = plan
            .apply_pass(
                grid(),
                &noise,
                if topology_pass.is_some() {
                    river_surface::Pass::Texture
                } else {
                    river_surface::Pass::Combined
                },
                || false,
            )?
            .ok_or("Surface pass cancelled")?;
        surface_ms = Some(start.elapsed().as_millis());
        let replay = plan
            .apply_pass(
                grid(),
                &noise,
                if topology_pass.is_some() {
                    river_surface::Pass::Texture
                } else {
                    river_surface::Pass::Combined
                },
                || false,
            )?
            .ok_or("Surface replay cancelled")?;
        if result.splat != replay.splat
            || result.topology != replay.topology
            || result.report.changed_splat_cells == 0
            || topology_pass
                .as_ref()
                .map_or(result.report.changed_topology_cells, |v| {
                    v.report.changed_topology_cells
                })
                == 0
            || source.layers.splat != layers.splat
        {
            return Err("Surface replay differs or generated no changes".into());
        }
        Some(result)
    } else {
        None
    };
    let mut water_ms = None;
    let water = if let Some(source) = &water_source {
        let plan = river_water::prepare(
            &carved.paths,
            &source.mesh.buffer,
            &shape.policy,
            source.policy(),
            river_water::Settings {
                maximum_work: 128_000_000,
                maximum_chunks: 4096,
                maximum_geometry_bytes: 64 * 1024 * 1024,
            },
            || false,
        )?
        .ok_or("Water preparation cancelled")?;
        let noise = sample(&plan.noise_coordinates())?;
        let dry = vec![0; carved.height.len()];
        let grid = || river_water::Grid {
            size: project.size as f32,
            resolution: layers.height_resolution,
            height: &carved.height,
            existing_water: &dry,
        };
        let start = Instant::now();
        let result = plan
            .build(grid(), &noise, || false)?
            .ok_or("Water generation cancelled")?;
        water_ms = Some(start.elapsed().as_millis());
        let replay = plan
            .build(grid(), &noise, || false)?
            .ok_or("Water replay cancelled")?;
        if result.water != replay.water
            || serde_json::to_vec(&result.chunks).map_err(|e| e.to_string())?
                != serde_json::to_vec(&replay.chunks).map_err(|e| e.to_string())?
            || result.report.changed_samples == 0
            || dry.iter().any(|&h| h != 0)
        {
            return Err("Water replay differs or generated no registered water".into());
        }
        Some(result)
    } else {
        None
    };
    let mut placement_metadata = None;
    let mut placement_outcomes = None;
    let pipeline_metadata = if let (
        Some((surface, _)),
        Some(water_source),
        Some(painted),
        Some(water),
    ) = (&surface_study, &water_source, &surfaces, &water)
    {
        if terrain.terrain_origin_y != -500. || terrain.terrain_height != 1000. {
            return Err("Pipeline uses the native transport vertical frame".into());
        }
        let dry = vec![0; layers.height.len()];
        let start = Instant::now();
        let pipeline = river_pipeline::generate(
            &layers,
            &dry,
            river_pipeline::Policies {
                layout: &shape.policy,
                carve: &terrain.policy,
                surface: &surface.policy,
                water: water_source.policy(),
                template: &water_source.mesh.buffer,
            },
            river_pipeline::Settings {
                seed: Some(project.seeds[2].value),
                maximum_stage_work: 512_000_000,
                maximum_candidates: 4096,
                maximum_controls: 1_048_576,
                water: river_water::Settings {
                    maximum_work: 128_000_000,
                    maximum_chunks: 4096,
                    maximum_geometry_bytes: 64 * 1024 * 1024,
                },
            },
            |_, coordinates| sample(coordinates).map(Some),
            || false,
        )?
        .ok_or("Pipeline cancelled")?;
        let elapsed = start.elapsed().as_millis();
        if pipeline.layers.height != carved.height
            || pipeline.layers.splat != painted.splat
            || pipeline.layers.topology != painted.topology
            || pipeline.layers.biome != layers.biome
            || pipeline.local_water != water.water
            || serde_json::to_vec(&pipeline.paths).map_err(|e| e.to_string())?
                != serde_json::to_vec(&carved.paths).map_err(|e| e.to_string())?
            || serde_json::to_vec(&pipeline.water_chunks).map_err(|e| e.to_string())?
                != serde_json::to_vec(&water.chunks).map_err(|e| e.to_string())?
        {
            return Err("Transactional pipeline differs from separately verified stages".into());
        }
        if args.len() == 12 {
            let catalog = mapper_studio::prefab_assets::load(
                root,
                Path::new(&args[10]),
                key.source.as_ref().unwrap(),
                || false,
            )?;
            let bindings = mapper_studio::prefab_textures::Bindings::load(
                root,
                Path::new(&args[11]),
                &catalog,
                || false,
            )?;
            let start = Instant::now();
            let solution = mapper_studio::placement_preview::solve_native(
                &project,
                &pipeline.layers,
                &catalog,
                &bindings,
                || false,
            )?
            .ok_or("Placement after rivers cancelled")?;
            placement_metadata = Some(serde_json::json!({
                "catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,
                "targets":project.targets.len(),"outcomes":solution.outcomes.len(),
                "accepted":solution.outcomes.iter().filter(|o|o.position.is_some()).count(),
                "required_missing":solution.required_missing,"attempts":solution.attempts,
                "elapsed_ms":start.elapsed().as_millis(),
                "input":"transactional river pipeline height, splat, biome and topology",
            }));
            placement_outcomes = Some(
                serde_json::json!({"outcomes":solution.outcomes.iter().map(|o|serde_json::json!({
                    "id":o.id,"position":o.position.as_ref().map(|p|serde_json::json!({"x":p.x,"y":p.y,"z":p.z,"yaw":p.yaw,"clearance":p.clearance})),
                    "distance":o.distance,"within_radius":o.within_radius,"reason":o.reason,
                })).collect::<Vec<_>>()}),
            );
        }
        Some(
            serde_json::json!({"report":pipeline.report,"full_pipeline_ms":elapsed,
            "all_channels_paths_and_geometry_identical_to_separate_stages":true,
            "native_noise_batches_included_in_timing":true}),
        )
    } else {
        None
    };
    if hash(assembly)? != assembly_hash {
        return Err("Reference assembly changed during surface study".into());
    }
    fs::create_dir(output).map_err(|e| e.to_string())?;
    if let Some(outcomes) = placement_outcomes {
        json(&output.join("river-monument-placement.json"), &outcomes)?;
    }
    let base_hash = words(&output.join("base-height.i16le"), &layers.height)?;
    let carved_hash = words(&output.join("carved-height.i16le"), &carved.height)?;
    json(
        &output.join("paths.json"),
        &serde_json::json!({"layout":layout,"carved_paths":carved.paths}),
    )?;
    let mut surface_metadata = serde_json::Value::Null;
    let mut water_metadata = serde_json::Value::Null;
    if let (Some(water), Some(source)) = (&water, &water_source) {
        let digest = words(&output.join("local-water.i16le"), &water.water)?;
        json(
            &output.join("water-geometry.json"),
            &serde_json::json!({"chunks":water.chunks}),
        )?;
        water_metadata = serde_json::json!({"source_sha256":source.sha256,"template":source.mesh.name,"policy":source.policy(),
            "report":water.report,"water_sha256":digest,"first_water_ms":water_ms,"replay_identical":true});
    }
    if let (Some(surfaces), Some((study, bytes))) = (&surfaces, &surface_study) {
        let payload = |name: &str, bytes: &[u8]| -> Result<String, String> {
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(output.join(name))
                .map_err(|e| e.to_string())?;
            file.write_all(bytes).map_err(|e| e.to_string())?;
            Ok(format!("{:x}", Sha256::digest(bytes)))
        };
        let base_splat = payload("base-splat.u8", &layers.splat)?;
        let final_splat = payload("painted-splat.u8", &surfaces.splat)?;
        let base_topology = payload(
            "base-topology.i32le",
            &layers
                .topology
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )?;
        let final_topology = payload(
            "river-topology.i32le",
            &surfaces
                .topology
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )?;
        let palette = mapper_studio::terrain_palette::Palette::discover(
            root,
            key.source.as_ref().unwrap(),
            || false,
        )?;
        let mut display = layers.clone();
        display.height.clone_from(&carved.height);
        for (name, painted) in [("before-paint.png", false), ("painted.png", true)] {
            if painted {
                display.splat.clone_from(&surfaces.splat);
                display.topology.clone_from(&surfaces.topology);
            }
            let image = mapper_studio::map_image::render_with_palette(
                &display,
                None,
                None,
                1024,
                palette.as_ref(),
                || false,
            )?
            .ok_or("Image cancelled")?;
            if !mapper_studio::image_export::write_png(&output.join(name), &image, || false)? {
                return Err("PNG cancelled".into());
            }
        }
        if let Some(water) = &water {
            let image = mapper_studio::map_image::render_with_water(
                &display,
                None,
                None,
                Some(&water.water),
                1024,
                palette.as_ref(),
                || false,
            )?
            .ok_or("Freshwater image cancelled")?;
            if !mapper_studio::image_export::write_png(&output.join("freshwater.png"), &image, || {
                false
            })? {
                return Err("Freshwater PNG cancelled".into());
            }
        }
        surface_metadata = serde_json::json!({"study_sha256":format!("{:x}",Sha256::digest(bytes)),"policy":study.policy,
            "topology_pass":topology_pass.as_ref().map(|v|&v.report),
            "report":surfaces.report,"base_splat_sha256":base_splat,"painted_splat_sha256":final_splat,
            "base_topology_sha256":base_topology,"river_topology_sha256":final_topology,
            "palette_sha256":palette.as_ref().map(|p|&p.sha256),"first_surface_ms":surface_ms,"replay_identical":true});
    }
    json(
        &output.join("verification.json"),
        &serde_json::json!({
            "format":if water.is_some(){"papyrus-river-water-study-verification"}else if surfaces.is_some(){"papyrus-river-surface-study-verification"}else{"papyrus-river-terrain-study-verification"},"schema":1,"size":project.size,
            "resolution":layers.height_resolution,"terrain_origin_y":terrain.terrain_origin_y,"terrain_height":terrain.terrain_height,
            "layout_study_sha256":terrain.layout_study_sha256,"terrain_study_sha256":format!("{:x}",Sha256::digest(terrain_bytes)),
            "assembly_sha256":assembly_hash,"native":key.source.as_ref().unwrap().native,"river_seed":project.seeds[2].value,
            "base_height_sha256":base_hash,"carved_height_sha256":carved_hash,"layout_policy":shape.policy,
            "terrain_policy":terrain.policy,"carve":carved.report,"first_carve_ms":first_ms,"replay_identical":true,
            "cached_source_preserved":true,"surface_resolution":layers.surface_resolution,"surface":surface_metadata,"water":water_metadata,"pipeline":pipeline_metadata,"placement_after_rivers":placement_metadata,
            "paint_applied":surfaces.is_some(),"topology_applied":surfaces.is_some(),"water_surfaces_created":water.is_some(),
            "topology_before_carving":topology_pass.is_some(),
            "automatic_client_binding":false,"connected_to_normal_generation":false,"game_validated":false
        }),
    )?;
    println!(
        "{} carved samples across {} rivers; {} ms; exact replay",
        carved.report.changed_samples,
        carved.paths.len(),
        first_ms
    );
    Ok(())
}
