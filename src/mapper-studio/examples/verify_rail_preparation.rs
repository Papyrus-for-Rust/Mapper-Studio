//! Cold selected-installation rail preparation and immutable cache/pin reuse.
use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        return Err(
            "verify_rail_preparation RUST_ROOT CATALOG INFRASTRUCTURE SETTINGS NEW_DATA_ROOT"
                .into(),
        );
    }
    let root = Path::new(&args[0]);
    let local = Path::new(&args[4]);
    if local.exists() {
        return Err("Rail verification cache must not exist".into());
    }
    let installation = mapper_studio::installation::inspect(root, || false)?;
    let catalog =
        mapper_studio::prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let parent = mapper_studio::infrastructure::Source::load(Path::new(&args[2]), &catalog, || false)?;
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&args[3]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let prepared = mapper_studio::rail_extract::prepare_in(
        root,
        &catalog,
        Path::new(&args[1]),
        &parent,
        Path::new(&args[2]),
        None,
        local,
        || false,
    )?;
    if prepared.reused {
        return Err("Cold rail extraction unexpectedly reused a cache".into());
    }
    mapper_studio::rail_generation::check_source(&prepared.source, 6000)?;
    let recipe = mapper_studio::rail_generation::Recipe {
        engine: mapper_studio::rail_generation::VERSION.into(),
        source_sha256: prepared.source.sha256.clone(),
        mesh_sha256: prepared.meshes.sha256.clone(),
        settings,
    };
    recipe.validate()?;
    for pin in [None, Some(&recipe)] {
        let repeated = mapper_studio::rail_extract::prepare_in(
            root,
            &catalog,
            Path::new(&args[1]),
            &parent,
            Path::new(&args[2]),
            pin,
            local,
            || false,
        )?;
        if !repeated.reused
            || repeated.source.sha256 != recipe.source_sha256
            || repeated.meshes.sha256 != recipe.mesh_sha256
        {
            return Err("Prepared rail cache/pin reuse changed the selected identity".into());
        }
    }
    let report = serde_json::json!({"format":"mapper-rail-preparation-verification","catalog_sha256":catalog.sha256,
        "infrastructure_sha256":parent.sha256,"source_sha256":recipe.source_sha256,"mesh_sha256":recipe.mesh_sha256,
        "stages":prepared.source.stages.len(),"mesh_templates":prepared.meshes.meshes.len(),
        "cache_reuse_verified":true,"pinned_reuse_verified":true,"recipe_valid":true,"meshes":prepared.meshes.audit()});
    std::fs::write(
        local.join("verification.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
