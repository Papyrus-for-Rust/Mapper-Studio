//! Exercise cold, warm and pinned river-object preparation through the app API.
use mapper_studio::{
    infrastructure, installation, object_extract, prefab_assets, road_object_source::PathFamily,
};
use std::{io::Write, path::Path, time::Instant};

fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !matches!(args.len(), 4 | 5) {
        return Err("Expected RUST_ROOT CATALOG INFRASTRUCTURE DATA_ROOT [--cached-only]".into());
    }
    let root = Path::new(&args[0]);
    let local = Path::new(&args[3]);
    let cached_only = args.get(4).is_some_and(|arg| arg == "--cached-only");
    if args.len() == 5 && !cached_only || local.exists() != cached_only {
        return Err("Expected a new data root or an existing root with --cached-only".into());
    }
    let _scope = installation::VerificationScope::default();
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let parent = infrastructure::Source::load(Path::new(&args[2]), &catalog, || false)?;
    let start = Instant::now();
    let source =
        object_extract::prepare_rivers_in(root, &parent, Path::new(&args[2]), None, local, || {
            false
        })?;
    let cold_seconds = start.elapsed().as_secs_f64();
    if source.path_family != PathFamily::River || source.environments.is_none() {
        return Err("Preparation returned incomplete river object metadata".into());
    }
    let path = local
        .join("papyrus-river-object-cache")
        .join(&source.sha256)
        .join("objects.json");
    let before = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map_err(|e| e.to_string())?;
    let raw = std::fs::read(&path).map_err(|e| e.to_string())?;
    let start = Instant::now();
    let warm =
        object_extract::prepare_rivers_in(root, &parent, Path::new(&args[2]), None, local, || {
            false
        })?;
    let warm_seconds = start.elapsed().as_secs_f64();
    let pinned = object_extract::prepare_rivers_in(
        root,
        &parent,
        Path::new(&args[2]),
        Some(&source.sha256),
        local,
        || false,
    )?;
    if source.sha256 != warm.sha256
        || source.sha256 != pinned.sha256
        || raw != std::fs::read(&path).map_err(|e| e.to_string())?
        || before
            != std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map_err(|e| e.to_string())?
        || object_extract::prepare_rivers_in(
            root,
            &parent,
            Path::new(&args[2]),
            None,
            local,
            || true,
        )
        .is_ok()
    {
        return Err("River cache reuse or cancellation failed".into());
    }
    let report = serde_json::json!({"format":"mapper-river-object-preparation-verification","schema":1,
        "source_sha256":source.sha256,"prefabs":source.prefabs.len(),"stages":source.stages.len(),
        "cold_seconds":cold_seconds,"warm_seconds":warm_seconds,"warm_and_pin_reused":true,
        "metadata_write_time_retained":true,"cancellation_retains_input":true});
    let name = if cached_only {
        "verification-warm-process.json"
    } else {
        "verification.json"
    };
    let mut file = std::fs::File::create_new(local.join(name)).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
