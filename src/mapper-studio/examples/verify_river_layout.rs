//! Source-bound headless study of river layout over a verified saved native base.
//! Does not change a project, its cached terrain or the normal generation path.
use mapper_studio::{Project, native_noise, native_preview, native_profile, terrain_cache};
use papyrus_terrain::{native_layers::Layers, river_layout};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
    time::Instant,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Study {
    format: String,
    schema: u32,
    assembly_sha256: String,
    scope: String,
    large_world_above: u32,
    large_world_maximum_rivers: usize,
    policy: river_layout::Policy,
}
fn digest(path: &Path) -> Result<String, String> {
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
fn ocean(l: &Layers, p: [f32; 3], mask: u32) -> bool {
    let n = l.surface_resolution;
    let [x, z] = [p[0], p[2]]
        .map(|v| ((v / l.size as f32 + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize);
    l.topology[z * n + x] as u32 & mask != 0
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 7 {
        return Err("verify_river_layout RUST_ROOT WORKER PROJECT TERRAIN_CACHE STUDY_JSON SERVER_ASSEMBLY NEW_DIRECTORY".into());
    }
    let root = Path::new(&args[0]);
    let worker = Path::new(&args[1]);
    let project = Project::load(Path::new(&args[2]))?;
    let key = project.terrain_key();
    let profile = native_profile::resolve(&key)?;
    if Path::new(&args[6]).exists() {
        return Err("Verification output already exists".into());
    }
    let cancel = AtomicBool::new(false);
    if !native_preview::verify_source(root, &key, &cancel)? {
        return Err("Source verification cancelled".into());
    }
    let mut bytes = Vec::new();
    File::open(&args[4])
        .map_err(|e| e.to_string())?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("Study exceeds 64 KiB".into());
    }
    let mut study: Study = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if study.format != "papyrus-river-layout-study"
        || study.schema != 1
        || study.scope.is_empty()
        || digest(Path::new(&args[5]))? != study.assembly_sha256
        || !(1000..=6000).contains(&study.large_world_above)
        || study.large_world_maximum_rivers > 64
    {
        return Err("River study is not bound to the supplied server assembly".into());
    }
    if project.size > study.large_world_above {
        study.policy.maximum_rivers = study.large_world_maximum_rivers;
    }
    let source = terrain_cache::load(Path::new(&args[3]), &key, || false)?
        .ok_or("Missing verified terrain cache; generate this exact project first")?;
    let original_height = source.layers.height.clone();
    let original_topology = source.layers.topology.clone();
    let noise = native_noise::sample(
        root,
        worker,
        key.source.as_ref().unwrap(),
        &profile,
        &study.policy.noise_coordinates()?,
        || false,
    )?
    .ok_or("Noise cancelled")?;
    let mut layers = source.layers.as_ref().clone();
    let ocean_map = papyrus_terrain::ocean::reconstruct(
        &layers.height,
        layers.height_resolution,
        &layers.topology,
        layers.surface_resolution,
        || false,
    )?
    .ok_or("Ocean cancelled")?;
    layers.topology = ocean_map.topology;
    let start = Instant::now();
    let run = |seed| {
        river_layout::generate(
            &layers,
            &study.policy,
            &noise,
            &river_layout::Settings {
                seed,
                maximum_work: 512_000_000,
                maximum_candidates: 4096,
                maximum_controls: 1_048_576,
            },
            || false,
        )?
        .ok_or_else(|| "Layout cancelled".to_string())
    };
    let baseline = run(None)?;
    let seeded = run(Some(project.seeds[2].value))?;
    let replay = run(Some(project.seeds[2].value))?;
    if serde_json::to_vec(&seeded).map_err(|e| e.to_string())?
        != serde_json::to_vec(&replay).map_err(|e| e.to_string())?
        || source.layers.height != original_height
        || source.layers.topology != original_topology
    {
        return Err("River replay differs or immutable native source changed".into());
    }
    for path in baseline.paths.iter().chain(&seeded.paths) {
        if !ocean(
            &layers,
            *path.points().last().ok_or("Empty river")?,
            study.policy.ocean_topology,
        ) {
            return Err("River lacks an ocean endpoint".into());
        }
    }
    if digest(Path::new(&args[5]))? != study.assembly_sha256 {
        return Err("Server assembly changed during study".into());
    }
    let directory = Path::new(&args[6]);
    fs::create_dir(directory).map_err(|e| e.to_string())?;
    let write = |name: &str, value: &serde_json::Value| -> Result<(), String> {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(directory.join(name))
            .map_err(|e| e.to_string())?;
        let data = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        file.write_all(&data).map_err(|e| e.to_string())
    };
    write(
        "source-order.json",
        &serde_json::to_value(&baseline).map_err(|e| e.to_string())?,
    )?;
    write(
        "seeded.json",
        &serde_json::to_value(&seeded).map_err(|e| e.to_string())?,
    )?;
    write(
        "verification.json",
        &serde_json::json!({"format":"papyrus-river-layout-study-verification","schema":1,
        "assembly_sha256":study.assembly_sha256,"native":key.source.as_ref().unwrap().native,
        "study_sha256":format!("{:x}",Sha256::digest(&bytes)),"policy":study.policy,
        "size":project.size,"river_seed":project.seeds[2].value,"source_order":baseline.report,"seeded":seeded.report,
        "replay_identical":true,"cached_source_preserved":true,"ocean_endpoints_verified":true,
        "elapsed_ms":start.elapsed().as_millis(),"automatic_client_binding":false,
        "terrain_applied":false,"connected_to_normal_generation":false,"game_validated":false}),
    )?;
    println!(
        "{} source-order rivers; {} seeded rivers; exact replay",
        baseline.paths.len(),
        seeded.paths.len()
    );
    Ok(())
}
