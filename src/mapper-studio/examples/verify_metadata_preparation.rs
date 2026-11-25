//! Windowless cold-cache check of packaged catalog, stamp and model preparation.
use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("verify_metadata_preparation RUST_ROOT PROJECT NEW_DATA_ROOT".into());
    }
    let rust = Path::new(&args[0]);
    let project = mapper_studio::Project::load(Path::new(&args[1]))?;
    let local = Path::new(&args[2]);
    if local.exists() {
        return Err("Cold metadata verification requires a new data directory".into());
    }
    let source = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    let started = std::time::Instant::now();
    let (catalog, bindings) =
        mapper_studio::asset_discovery::prepare_in(rust, source, None, local, || false)?;
    println!(
        "Catalog/stamps verified: {} / {}",
        catalog.sha256, bindings.sha256
    );
    let catalog_path = std::fs::read_dir(local.join("papyrus-prefab-cache"))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("catalog.json"))
        .find(|p| p.is_file())
        .ok_or("Missing published catalog")?;
    let mut monuments = mapper_studio::monument_extract::prepare_in(
        rust,
        &catalog,
        &bindings,
        &catalog_path,
        &local.join("papyrus-monument-cache"),
        || false,
    )?
    .ok_or("Monument preparation cancelled")?;
    monuments.prepare_coastal(&catalog, rust, || false)?;
    let reused = mapper_studio::asset_discovery::prepare_in(
        rust,
        source,
        Some(&mapper_studio::AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: bindings.sha256.clone(),
        }),
        local,
        || false,
    )?;
    assert_eq!(catalog.sha256, reused.0.sha256);
    assert_eq!(bindings.sha256, reused.1.sha256);
    let report = serde_json::json!({"format":"mapper-cold-metadata-verification","catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"prefabs":catalog.entries.len(),"textures":bindings.textures().len(),"monument_sha256":monuments.sha256,"monument_prefabs":monuments.prefabs.len(),"models_requested":monuments.models_requested,"coastal_adapter_ready":monuments.coastal_profile.is_some(),"pinned_cache_reuse_verified":true,"elapsed_seconds":started.elapsed().as_secs_f64()});
    std::fs::write(
        local.join("verification.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
