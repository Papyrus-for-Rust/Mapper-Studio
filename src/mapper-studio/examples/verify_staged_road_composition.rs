//! Run the real headless/GUI composition entry point with its saved staged policy.
//! CONFIG NEW_BUNDLE. No game, server or desktop window is launched.
//! Optional road_engine selects strict or source completion explicitly; omitted
//! keeps the historical strict verifier. Source inputs use normal preparation.
//! normal_defaults instead leaves staged unset and exercises normal resolution.
use mapper_studio::{Project, headless, road_generation};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::atomic::AtomicBool};
fn main() -> Result<(), String> {
    let started = std::time::Instant::now();
    let _verification = mapper_studio::installation::VerificationScope::default();
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("CONFIG NEW_BUNDLE".into());
    }
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(&args[0]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let path = |key: &str| -> Result<&Path, String> {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Bundle must be new".into());
    }
    let mut project = Project::load(path("project")?)?;
    if config
        .get("fresh_roster")
        .map(|v| v.as_bool().ok_or("fresh_roster must be boolean"))
        .transpose()?
        .unwrap_or(false)
    {
        if !project.targets.is_empty() {
            return Err("Fresh-roster regression requires an empty target list; saved preferences must not be discarded".into());
        }
        let catalog = mapper_studio::prefab_assets::load(
            path("rust_root")?,
            path("catalog")?,
            project
                .installation
                .as_ref()
                .ok_or("Missing installation")?,
            || false,
        )?;
        let count = if let Some(binding) = project
            .monument_rules
            .as_ref()
            .and_then(|rules| rules.stage_rules.as_ref())
        {
            let stages = mapper_studio::monument_stages::Source::cached(
                binding,
                path("rust_root")?,
                &catalog,
                || false,
            )?;
            mapper_studio::placement_preview::populate_stage_targets(&mut project, &catalog, &stages)?
        } else {
            mapper_studio::placement_preview::populate_targets(&mut project, &catalog)?
        };
        eprintln!(
            "Prepared {count} installation-backed targets for fresh world size {}",
            project.size
        );
    }
    let normal_defaults = config
        .get("normal_defaults")
        .map(|v| v.as_bool().ok_or("normal_defaults must be boolean"))
        .transpose()?
        .unwrap_or(false);
    if normal_defaults && config.get("road_engine").is_some() {
        return Err("normal_defaults cannot override road_engine".into());
    }
    let engine = match config.get("road_engine") {
        None if normal_defaults => None,
        None => Some(road_generation::VERSION),
        Some(serde_json::Value::String(engine))
            if matches!(
                engine.as_str(),
                road_generation::VERSION
                    | road_generation::SOURCE_VERSION
                    | road_generation::RAW_SOURCE_VERSION
            ) =>
        {
            Some(engine.as_str())
        }
        _ => return Err("road_engine must name a supported strict or source road mode".into()),
    };
    let layout = if engine.is_some_and(road_generation::is_source_engine) {
        let catalog = mapper_studio::prefab_assets::load(
            path("rust_root")?,
            path("catalog")?,
            project
                .installation
                .as_ref()
                .ok_or("Missing installation")?,
            || false,
        )?;
        let infrastructure = mapper_studio::infrastructure::Source::cached(
            &catalog,
            project.roads.as_ref().ok_or("Missing road recipe")?,
            || false,
        )?;
        project
            .roads
            .as_mut()
            .unwrap()
            .apply_source_defaults(&infrastructure)?;
        let inputs = mapper_studio::road_extract::prepare_ordered(
            path("rust_root")?,
            &catalog,
            &infrastructure,
            None,
            || false,
        )?
        .ok_or("Ordered source road inputs unavailable")?;
        Some(mapper_studio::source_road_planning::Policy::bind(&inputs))
    } else {
        None
    };
    project.roads.as_mut().ok_or("Missing road recipe")?.staged = if let Some(engine) = engine {
        let source = fs::read(path("roadside_source")?).map_err(|e| e.to_string())?;
        Some(road_generation::Policy {
            layout,
            engine: engine.into(),
            roadside_source_sha256: format!("{:x}", Sha256::digest(&source)),
            roadside_preferences: Vec::new(),
        })
    } else {
        None
    };
    project.validate()?;
    let cancel = AtomicBool::new(false);
    let generated = headless::generate(
        &project,
        &headless::Options {
            rust_root: Some(path("rust_root")?.to_path_buf()),
            worker: Some(path("worker")?.to_path_buf()),
            catalog: Some(path("catalog")?.to_path_buf()),
            bindings: Some(path("bindings")?.to_path_buf()),
            skip_composition: false,
        },
        &cancel,
        |label, _| eprintln!("{:.3}s {label}", started.elapsed().as_secs_f64()),
    )?
    .ok_or("Cancelled")?;
    let resolved = generated
        .recipe
        .roads
        .as_ref()
        .and_then(|roads| roads.staged.as_ref())
        .ok_or("Generation did not resolve a staged road contract")?;
    if engine.is_some_and(|expected| resolved.engine != expected) {
        return Err("Normal generation selected an unexpected saved road contract".into());
    }
    let maps = generated
        .composed
        .as_ref()
        .ok_or("Missing normal composed maps")?;
    let roads = maps.roads.as_ref().ok_or("Missing normal staged roads")?;
    let stage = &roads
        .surfaces
        .as_ref()
        .ok_or("Missing normal staged surfaces")?
        .report["staged_pipeline"];
    if stage["engine"] != resolved.engine || stage["post_decoration_validated"] != true {
        return Err("Normal composition did not complete its staged road gates".into());
    }
    eprintln!(
        "Normal composition accepted {} roads and {} roadside stamps",
        roads.roads.len(),
        stage["roadside"]["placements"]
            .as_array()
            .map_or(0, Vec::len)
    );
    headless::save_bundle(&generated.recipe, &generated, output, &cancel)?;
    eprintln!(
        "{:.3}s Saved verified composition bundle",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
