//! Local, windowless cold-cache verification of the packaged metadata runtime.
use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 && !(args.len() == 4 && args[3] == "--cancel-only") {
        return Err(
            "verify_palette_preparation RUST_ROOT PROJECT NEW_CACHE [--cancel-only]".into(),
        );
    }
    let project = mapper_studio::Project::load(Path::new(&args[1]))?;
    let cache = Path::new(&args[2]);
    if cache.exists() {
        return Err("Cold verification cache must not exist".into());
    }
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    let cancelled_cache = cache.with_extension("cancelled");
    if cancelled_cache.exists() {
        return Err("Cancellation verification cache must not exist".into());
    }
    let start = std::time::Instant::now();
    let cancelled = mapper_studio::palette_extract::prepare_in(
        &cancelled_cache,
        Path::new(&args[0]),
        installation,
        || start.elapsed() > std::time::Duration::from_millis(100),
    )?;
    if cancelled.is_some()
        || (cancelled_cache.exists()
            && std::fs::read_dir(&cancelled_cache)
                .map_err(|e| e.to_string())?
                .next()
                .is_some())
        || start.elapsed() > std::time::Duration::from_secs(5)
    {
        return Err("Cancelled preparation left output or did not stop promptly".into());
    }
    if args.len() == 4 {
        println!(
            "{{\"cancelled_preparation_left_no_output\":true,\"elapsed_ms\":{}}}",
            start.elapsed().as_millis()
        );
        return Ok(());
    }
    let palette =
        mapper_studio::palette_extract::prepare_in(cache, Path::new(&args[0]), installation, || {
            false
        })?
        .ok_or("Preparation cancelled")?;
    let reused =
        mapper_studio::palette_extract::prepare_in(cache, Path::new(&args[0]), installation, || {
            false
        })?
        .ok_or("Reuse cancelled")?;
    if palette != reused {
        return Err("Reused palette differs".into());
    }
    println!(
        "{}",
        serde_json::json!({"format":"mapper-palette-preparation-verification","sha256":palette.sha256,"biomes":palette.biome_count(),"materials":palette.materials().len(),"cold_cache_prepared":true,"verified_cache_reused":true,"cancelled_preparation_left_no_output":true})
    );
    Ok(())
}
