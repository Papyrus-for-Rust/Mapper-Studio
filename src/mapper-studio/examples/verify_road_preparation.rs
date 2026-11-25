//! Verify road setup using packaged metadata tools and an independent cache root.
use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("verify_road_preparation RUST_ROOT CATALOG NEW_DATA_ROOT".into());
    }
    let root = Path::new(&args[0]);
    let cache = Path::new(&args[2]);
    if cache.exists() {
        return Err("Road verification cache must not exist".into());
    }
    let installation = mapper_studio::installation::inspect(root, || false)?;
    let catalog =
        mapper_studio::prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let (source, surface) =
        mapper_studio::road_extract::prepare_in(root, &catalog, Path::new(&args[1]), cache, || false)?;
    let routing = mapper_studio::road_cost_source::Profile::bundled_joined(&source)?
        .ok_or("Fresh road metadata has no matching bundled routing policy")?;
    routing.check(&source)?;
    let mut recipe = mapper_studio::infrastructure::Recipe::for_source(&source);
    recipe.ring_costmap = Some(routing.clone());
    recipe.access_costmap = Some(routing);
    recipe.access_smoothing = Some(papyrus_terrain::road_smoothing::VERSION.into());
    recipe.surface_sha256 = Some(surface.sha256.clone());
    recipe.validate()?;
    let report = serde_json::json!({"format":"mapper-road-preparation-verification","infrastructure_sha256":source.sha256,"semantic_sha256":source.semantic_sha256()?,"catalog_sha256":catalog.sha256,"surface_sha256":surface.sha256,"revision":source.profile.revision,"prefabs":source.prefabs.len(),"mesh_templates":surface.meshes.len(),"routing_policy_matches":true,"complete_recipe_valid":true,"recipe":recipe});
    std::fs::write(
        cache.join("verification.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
