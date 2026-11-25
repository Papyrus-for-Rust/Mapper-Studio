//! Fresh decoration/roadside preparation against independently retained parents.
use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("verify_object_metadata RUST_ROOT CATALOG INFRASTRUCTURE NEW_DATA_ROOT".into());
    }
    let root = Path::new(&args[0]);
    let local = Path::new(&args[3]);
    if local.exists() {
        return Err("Verification data root must not exist".into());
    }
    let installation = mapper_studio::installation::inspect(root, || false)?;
    let catalog =
        mapper_studio::prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let parent = mapper_studio::infrastructure::Source::load(Path::new(&args[2]), &catalog, || false)?;
    let decor =
        mapper_studio::object_extract::prepare_decor_in(root, &catalog, None, local, || false)?;
    let road = mapper_studio::object_extract::prepare_roads_in(
        root,
        &parent,
        Path::new(&args[2]),
        None,
        local,
        || false,
    )?;
    if decor.gameplay_sources().len() != 2
        || decor.recipe().engine != mapper_studio::decor_source::COLLIDER_VERSION
    {
        return Err("Fresh decoration capture lacks complete gameplay/geometry binding".into());
    }
    let plan = mapper_studio::object_workflow::plan(&road, &decor.stages)?;
    for pin in [None, Some(decor.sha256.as_str())] {
        let repeated =
            mapper_studio::object_extract::prepare_decor_in(root, &catalog, pin, local, || false)?;
        if repeated.sha256 != decor.sha256 {
            return Err("Decoration cache reuse changed identity".into());
        }
    }
    for pin in [None, Some(road.sha256.as_str())] {
        let repeated = mapper_studio::object_extract::prepare_roads_in(
            root,
            &parent,
            Path::new(&args[2]),
            pin,
            local,
            || false,
        )?;
        if repeated.sha256 != road.sha256 {
            return Err("Road object cache reuse changed identity".into());
        }
    }
    let report = serde_json::json!({"format":"mapper-object-metadata-verification","decoration_sha256":decor.sha256,"road_object_sha256":road.sha256,"infrastructure_sha256":parent.sha256,"decoration_stages":decor.stages.len(),"decoration_prefabs":decor.prefab_paths().count(),"gameplay_receipts":decor.gameplay_sources(),"road_stages":road.stages.len(),"road_prefabs":road.prefabs.len(),"workflow_steps":plan.steps.len(),"cache_and_pin_reuse_verified":true});
    std::fs::write(
        local.join("verification.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
