//! CONFIG NEW_REPORT. Set an isolated, empty LOCALAPPDATA and supply the normal
//! packaged metadata runtime beside this executable. No game or GUI is launched.
use mapper_studio::{
    Project, infrastructure, loading_progress, prefab_assets, road_extract, source_road_assets,
    source_road_inputs,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > limit {
        return Err("Preparation audit input exceeds limit".into());
    }
    Ok(raw)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("CONFIG NEW_REPORT".into());
    }
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Audit output must be new".into());
    }
    let local = mapper_studio::storage::local_data()?;
    if local.exists() {
        return Err("Use an isolated empty LOCALAPPDATA for this audit".into());
    }
    let config: Value = serde_json::from_slice(&read(Path::new(&args[0]), 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let project = Project::load(path("project")?)?;
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
    let source = infrastructure::Source::load(path("infrastructure")?, &catalog, || false)?;
    let automatic =
        source_road_inputs::Roles::for_source(&source)?.ok_or("Unsupported enum profile")?;
    if let Ok(reference) = path("source_client_roles") {
        let captured =
            source_road_inputs::Roles::load(reference, &source, || false)?.ok_or("Cancelled")?;
        if serde_json::to_value(&automatic).map_err(|e| e.to_string())?
            != serde_json::to_value(captured).map_err(|e| e.to_string())?
        {
            return Err("Automatic roles differ from the owned code capture".into());
        }
    }
    for (cache, id, name, source_path) in [
        (
            "papyrus-prefab-cache",
            &catalog.sha256,
            "catalog.json",
            path("catalog")?,
        ),
        (
            "papyrus-infrastructure-cache",
            &source.sha256,
            "infrastructure.json",
            path("infrastructure")?,
        ),
    ] {
        let folder = local.join(cache).join(id);
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let raw = read(source_path, 16 * 1024 * 1024)?;
        std::fs::File::options()
            .create_new(true)
            .write(true)
            .open(folder.join(name))
            .map_err(|e| e.to_string())?
            .write_all(&raw)
            .map_err(|e| e.to_string())?;
    }
    let phase = Arc::new(AtomicUsize::new(1));
    let stop = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let collector = {
        let phase = phase.clone();
        let stop = stop.clone();
        let observed = observed.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let current_phase = phase.load(Ordering::Relaxed);
                for snapshot in loading_progress::active() {
                    if matches!(
                        snapshot.stage.as_str(),
                        "Processing road layout assets"
                            | "Verifying road-layout sources"
                            | "Reading ordered road connectors"
                            | "Checking road-layout sources"
                            | "Ordered road connectors ready"
                    ) {
                        let mut observations = observed.lock().unwrap();
                        let value = (
                            current_phase,
                            snapshot.stage,
                            snapshot.completed,
                            snapshot.total,
                        );
                        if observations.last() != Some(&value) {
                            observations.push(value);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let result = (|| -> Result<Value, String> {
        let now = Instant::now();
        let first = road_extract::prepare_ordered(root, &catalog, &source, None, || false)?
            .ok_or("Ordered preparation unsupported or cancelled")?;
        let first_ms = now.elapsed().as_secs_f64() * 1000.;
        let doc = local
            .join("papyrus-road-layout-cache")
            .join(&first.assets.sha256)
            .join("layout-assets.json");
        let bytes = read(&doc, 16 * 1024 * 1024)?;
        phase.store(2, Ordering::Relaxed);
        let now = Instant::now();
        let cached = road_extract::prepare_ordered(
            root,
            &catalog,
            &source,
            Some(&first.assets.sha256),
            || false,
        )?
        .ok_or("Pinned cache load cancelled")?;
        let cached_ms = now.elapsed().as_secs_f64() * 1000.;
        if cached.assets.sha256 != first.assets.sha256 || read(&doc, 16 * 1024 * 1024)? != bytes {
            return Err("Cache reuse changed ordered inputs".into());
        }
        if let Ok(reference) = path("source_layout_assets") {
            let captured =
                source_road_assets::Assets::load(reference, root, &catalog, &source, || false)?
                    .ok_or("Cancelled")?;
            if serde_json::to_value(first.assets.prefabs()).map_err(|e| e.to_string())?
                != serde_json::to_value(captured.prefabs()).map_err(|e| e.to_string())?
            {
                return Err(
                    "Prepared prefab data differs from the immutable ordered capture".into(),
                );
            }
        }
        Ok(
            json!({"format":"mapper-owned-ordered-road-preparation-1","catalog_sha256":catalog.sha256,"infrastructure_sha256":source.sha256,
            "assets_sha256":first.assets.sha256,"roles":automatic,"prefabs":first.assets.prefabs().len(),
            "preparation_ms":first_ms,"cache_reuse_ms":cached_ms,"cache_bytes_unchanged":true,"captured_prefabs_equal":config.get("source_layout_assets").is_some(),
            "normal_preparation_entrypoint":true,"manual_ordered_asset_or_role_files_required":false,"game_runtime_validated":false,"gui_launched":false}),
        )
    })();
    stop.store(true, Ordering::Relaxed);
    collector.join().map_err(|_| "Progress collector failed")?;
    let mut report = result?;
    let observed = observed.lock().map_err(|e| e.to_string())?;
    if !observed.iter().any(|(p, _, _, _)| *p == 1) || observed.iter().any(|(p, _, _, _)| *p == 2) {
        return Err("Expected initial worker progress and no extraction during cache reuse".into());
    }
    report["worker_progress"] = serde_json::to_value(&*observed).map_err(|e| e.to_string())?;
    report["cache_reuse_started_no_extractor"] = true.into();
    std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Prepared {} prefabs via packaged worker; pinned cache reuse verified",
        report["prefabs"]
    );
    Ok(())
}
