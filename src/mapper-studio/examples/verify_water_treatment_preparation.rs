//! Verify installation-bound pipe loading and optional persistent preparation.
use mapper_studio::water_treatment_source::{self, Source};
use mapper_studio::{Project, infrastructure, installation, prefab_assets, road_object_source};
use serde_json::json;
use std::{fs::OpenOptions, io::Write, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(7..=8).contains(&args.len()) || args.get(7).is_some_and(|arg| arg != "--prepare") {
        return Err("RUST_ROOT CATALOG PROJECT INFRASTRUCTURE ROAD_OBJECTS PIPE_CAPTURE NEW_OUTPUT_JSON [--prepare]".into());
    }
    let output = Path::new(&args[6]);
    if output.exists() {
        return Err("Existing preparation verification was retained".into());
    }
    let _verification = installation::VerificationScope::default();
    let root = Path::new(&args[0]);
    let project = Project::load(Path::new(&args[2]))?;
    let expected = project
        .installation
        .as_ref()
        .ok_or("Missing project installation")?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), expected, || false)?;
    let infrastructure = infrastructure::Source::load(Path::new(&args[3]), &catalog, || false)?;
    let recipe = project.roads.as_ref().ok_or("Missing project roads")?;
    if infrastructure.sha256 != recipe.source_sha256 {
        return Err("Infrastructure differs from the saved project".into());
    }
    let road =
        road_object_source::Source::load(root, Path::new(&args[4]), &infrastructure, || false)?;
    if project
        .objects
        .as_ref()
        .is_none_or(|recipe| recipe.road_source_sha256 != road.sha256)
    {
        return Err("Road-object capture differs from the saved project".into());
    }
    let flags = infrastructure
        .profile
        .topology_flags
        .as_ref()
        .ok_or("Missing topology roles")?;
    let protected = infrastructure.profile.protected_topology as u32;
    let captured = Source::load(
        root,
        Path::new(&args[5]),
        &catalog,
        &road,
        flags,
        protected,
        || false,
    )?;
    let prepared = if args.len() == 8 {
        let source =
            water_treatment_source::prepare(root, &catalog, &road, flags, protected, || false)?;
        if source.sha256() != captured.sha256() {
            return Err("Prepared pipe source differs from the verified capture".into());
        }
        let reused =
            water_treatment_source::prepare(root, &catalog, &road, flags, protected, || false)?;
        if reused.sha256() != source.sha256() {
            return Err("Warm pipe preparation changed its source".into());
        }
        Some(source.sha256().to_owned())
    } else {
        None
    };
    let report = json!({
        "format":"mapper-water-treatment-preparation-verification","schema":1,
        "source_sha256":captured.sha256(),"catalog_sha256":catalog.sha256,
        "infrastructure_sha256":infrastructure.sha256,"road_source_sha256":road.sha256,
        "prepared_and_reused_sha256":prepared,"collider_backfaces":captured.collider_backfaces(),
        "programs":captured.programs(),
        "prefabs":captured.prefabs().iter().map(|(path,prefab)| json!({
            "path":path,"runtime_holes":prefab.runtime_holes.len(),
        })).collect::<Vec<_>>(),
        "generation_executed":false,"game_code_executed":false,"desktop_windows_opened":0,
    });
    let bytes = serde_json::to_vec_pretty(&report)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
