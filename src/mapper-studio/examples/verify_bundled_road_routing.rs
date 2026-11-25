//! Check desktop road-profile discovery against a user-owned installation/cache.
use mapper_studio::{infrastructure, installation, prefab_assets, road_cost_source};
use std::{io::Write, path::Path};
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("verify_bundled_road_routing RUST_ROOT CATALOG NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let identity = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &identity, || false)?;
    let source = infrastructure::Source::discover(&catalog, None, || false)?;
    source.verify_installation(root, || false)?;
    let joined = road_cost_source::Profile::bundled_joined(&source)?
        .ok_or("No joined road profile matches this infrastructure and installation")?;
    joined.check(&source)?;
    let mut recipe = infrastructure::Recipe::for_source(&source);
    recipe.ring_costmap = Some(joined.clone());
    recipe.validate()?;
    let report = serde_json::json!({
        "format":"mapper-bundled-road-routing-verification", "schema":1,
        "infrastructure_sha256":source.sha256, "profile_id":joined.id,
        "joined_engine":joined.joins.as_ref().unwrap().engine,
        "receipts":joined.sources, "default_recipe_with_joined_routing":recipe,
        "game_loading_verified":false
    });
    let output = Path::new(&args[2]);
    let mut file = tempfile::NamedTempFile::new_in(output.parent().unwrap_or(Path::new(".")))
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.persist_noclobber(output).map_err(|e| e.to_string())?;
    println!(
        "Matched source-bound joined road routing: {}",
        output.display()
    );
    Ok(())
}
