//! Verify exact river metadata restoration with an empty local cache.
//! RUST_ROOT SOURCE_DOCUMENT PROJECT NEW_REPORT. No GUI or native DLL execution.
use mapper_studio::{Project, installation, profile, river_generation, storage};
use serde_json::json;
use std::{fs::File, io::Write, path::Path};

fn main() -> Result<(), String> {
    let _verification = installation::VerificationScope::default();
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("RUST_ROOT SOURCE_DOCUMENT PROJECT NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let mut project = Project::load(Path::new(&args[2]))?;
    let identity = project.installation.clone().ok_or("Missing installation")?;
    if installation::inspect(root, || false)? != identity {
        return Err("Selected installation differs from project".into());
    }
    let source = river_generation::Source::load(root, Path::new(&args[1]), &identity, || false)?
        .ok_or("Cancelled")?;
    let original = project.rivers.clone().ok_or("Missing river recipe")?;
    if original.source_sha256 != source.sha256 {
        return Err("Explicit source document differs from the saved river pin".into());
    }
    let cache = storage::local_data()?
        .join("papyrus-river-cache")
        .join(&source.sha256)
        .join("rivers.json");
    if cache.exists() {
        return Err("Run with an empty LOCALAPPDATA cache".into());
    }
    let raw_key = mapper_studio::native_preview::source_key(&project.terrain_key());
    let seeds = project.seeds;
    project.rivers = Some(source.recipe());
    project.validate()?;
    let private = tempfile::tempdir().map_err(|e| e.to_string())?;
    let saved_project = private.path().join("portable.mapper");
    project.save(&saved_project)?;
    let restored = Project::load(&saved_project)?;
    let saved_profile = private.path().join("portable.rec");
    profile::Profile::new("Portable rivers".into(), project.clone()).save(&saved_profile)?;
    let imported = profile::Profile::load(&saved_profile)?.recipe;
    let recipe = imported
        .rivers
        .as_ref()
        .ok_or("Profile lost river recipe")?;
    if restored.rivers.as_ref() != Some(recipe)
        || recipe != project.rivers.as_ref().unwrap()
        || restored.seeds != seeds
        || imported.seeds != seeds
        || mapper_studio::native_preview::source_key(&imported.terrain_key()) != raw_key
    {
        return Err(
            "Project/profile roundtrip changed river metadata, seeds or raw terrain".into(),
        );
    }
    let loaded =
        river_generation::Source::cached(root, &identity, recipe, || false)?.ok_or("Cancelled")?;
    if loaded.recipe() != *recipe || loaded.sha256 != original.source_sha256 {
        return Err("Embedded source did not reproduce the exact saved metadata".into());
    }
    let prepared = mapper_studio::river_extract::prepare_in(
        root,
        &identity,
        Some(recipe),
        &private.path().join("empty-cache"),
        || false,
    )?
    .ok_or("Preparation cancelled")?;
    if prepared.source.recipe() != *recipe || private.path().join("empty-cache").exists() {
        return Err("Pinned preparation changed metadata or required a cache".into());
    }
    let mut corrupt = recipe.clone();
    corrupt.source_document = Some(format!("{} ", recipe.source_document.as_ref().unwrap()).into());
    let error = river_generation::Source::cached(root, &identity, &corrupt, || false)
        .err()
        .ok_or("Modified document accepted")?;
    if !error.contains("Saved river source digest changed") {
        return Err(error);
    }
    if river_generation::Source::cached(root, &identity, recipe, || true)?.is_some() {
        return Err("Cancellation was ignored".into());
    }
    if river_generation::Source::cached(private.path(), &identity, recipe, || false).is_ok() {
        return Err("Embedded metadata bypassed the selected installation checks".into());
    }
    let mut legacy = recipe.clone();
    legacy.source_document = None;
    let error = river_generation::Source::cached(root, &identity, &legacy, || false)
        .err()
        .ok_or("Missing legacy cache was accepted")?;
    if !error.contains("Cannot load saved river data") || !error.contains(&legacy.source_sha256) {
        return Err(error);
    }
    if cache.exists() {
        return Err("Restoration unexpectedly wrote a cache entry".into());
    }
    let report = json!({
        "format":"mapper-portable-river-recipe-verification","schema":1,
        "source_sha256":source.sha256,"source_document_bytes":recipe.source_document.as_ref().unwrap().len(),
        "cache_absent_before_and_after":true,"project_and_profile_roundtrip":true,
        "exact_source_restored":true,"modified_document_rejected":true,
        "missing_installation_rejected":true,"cancellation_preserved":true,
        "seeds_and_raw_terrain_identity_preserved":true,
        "portable_project":restored,"gui_launched":false,"game_runtime_validated":false
    });
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(&args[3])
        .map_err(|e| e.to_string())?;
    output
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("Verified exact river recipe restoration without a local source cache");
    Ok(())
}
