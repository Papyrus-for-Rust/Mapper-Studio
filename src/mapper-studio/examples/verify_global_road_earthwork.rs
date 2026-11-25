//! Export receipt-covered route-preflight terrain for independent budget checking.
//! CONFIG NEW_DIRECTORY. No game, server or desktop window.
use mapper_studio::{
    Project, composition, infrastructure, native_processing, placement_preview, prefab_assets,
    prefab_textures,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufWriter, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
fn write_height(root: &Path, name: &str, height: &[i16]) -> Result<Value, String> {
    let mut file = BufWriter::new(fs::File::create(root.join(name)).map_err(|e| e.to_string())?);
    let mut hash = Sha256::new();
    for value in height {
        let b = value.to_le_bytes();
        file.write_all(&b).map_err(|e| e.to_string())?;
        hash.update(b);
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(json!({"bytes":height.len()*2,"sha256":format!("{:x}",hash.finalize())}))
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("CONFIG NEW_DIRECTORY".into());
    }
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Output already exists".into());
    }
    let config: Value = serde_json::from_slice(&fs::read(&args[0]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let project = Project::load(path("project")?)?;
    let recipe = project.roads.as_ref().ok_or("Missing roads")?;
    if recipe
        .entrance_launch
        .is_none_or(|p| p.engine != papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)
    {
        return Err("Expected global earthwork revision 8".into());
    }
    let root = path("rust_root")?;
    let catalog = prefab_assets::load(
        root,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, path("bindings")?, &catalog, || false)?;
    let raw = native_processing::build(
        root,
        path("worker")?,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    let solution =
        placement_preview::solve_native(&project, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    let mut maps = composition::compose_monuments(
        &raw.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let ground = maps.layers.height.clone();
    let source = infrastructure::Source::cached(&catalog, recipe, || false)?;
    let costs = recipe
        .ring_costmap
        .as_ref()
        .map(|p| {
            p.prepare(
                &project,
                &solution,
                &catalog,
                &bindings,
                &source,
                &maps.layers,
                || false,
            )
        })
        .transpose()?
        .flatten();
    let blockers = recipe
        .access_costmap
        .as_ref()
        .map(|p| p.prepare_blockers(&project, &solution, &catalog, &bindings, &source, || false))
        .transpose()?
        .flatten();
    let report = infrastructure::generate_with_source_costs(
        &project,
        &solution,
        &catalog,
        &source,
        root,
        &mut maps,
        costs.as_ref(),
        blockers.as_deref(),
        || false,
    )?
    .ok_or("Cancelled")?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let private = tempfile::Builder::new()
        .prefix(".global-road-earthwork-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let files = json!({"ground.i16le":write_height(private.path(),"ground.i16le",&ground)?,"planned.i16le":write_height(private.path(),"planned.i16le",&maps.layers.height)?});
    let doc = json!({"format":"mapper-global-road-earthwork-capture","schema":1,"project":project,
        "size":maps.layers.size,"height_resolution":maps.layers.height_resolution,"maximum_cut_fill":recipe.maximum_cut_fill,
        "height_encoding":"signed i16 normalized by 32766 over -500..500 metres","quantum":1000_f32/32766.,
        "reference":"terrain after initial monuments and before main/access/trail planning",
        "scope":"route preflight only; later source road painting and mesh fitting audited separately",
        "roads":report.json(),"files":files,"game_runtime_validated":false});
    fs::write(
        private.path().join("capture.json"),
        serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(private.path(), output).map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"output":output,"roads":report.roads.len(),"unresolved":report.unresolved})
    );
    Ok(())
}
