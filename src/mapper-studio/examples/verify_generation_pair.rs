//! Two normal headless generations in one process for prepared-terrain reuse.
use mapper_studio::{Project, StageSeed, headless, map_export, prefab_assets};
use std::{io::Write, path::Path, sync::atomic::AtomicBool};

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err("RUST_ROOT PROJECT CATALOG BINDINGS NEW_DIRECTORY SECOND_MONUMENT_SEED".into());
    }
    let destination = Path::new(&args[4]);
    if destination.try_exists().map_err(|e| e.to_string())? {
        return Err("Generation-pair output must be new".into());
    }
    let seed = args[5].parse::<u32>().map_err(|e| e.to_string())?;
    if !(1..=mapper_studio::MAX_SEED).contains(&seed) {
        return Err("Invalid second monument seed".into());
    }
    let mut project = Project::load(Path::new(&args[1]))?;
    let key = project.terrain_key();
    let options = headless::Options {
        rust_root: Some((&args[0]).into()),
        catalog: Some((&args[2]).into()),
        bindings: Some((&args[3]).into()),
        ..Default::default()
    };
    let cancelled = AtomicBool::new(false);
    std::fs::create_dir(destination).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for index in 0..2 {
        if index == 1 {
            project.seeds[4] = StageSeed {
                value: seed,
                inherited: false,
            };
        }
        if project.terrain_key() != key {
            return Err("Monument-only recipe changed the terrain key".into());
        }
        eprintln!("Generating pair member {index}");
        let generated = headless::generate(&project, &options, &cancelled, |stage, _| {
            eprintln!("{stage}")
        })?
        .ok_or("Generation cancelled")?;
        let bundle = destination.join(format!("world-{index}"));
        headless::save_bundle(&generated.recipe, &generated, &bundle, &cancelled)?;
        let root = options.rust_root.as_deref().unwrap();
        let catalog = prefab_assets::load(
            root,
            options.catalog.as_deref().unwrap(),
            generated
                .recipe
                .installation
                .as_ref()
                .ok_or("Missing installation")?,
            || false,
        )?;
        let maps = generated
            .composed
            .as_ref()
            .ok_or("Complete composition missing")?;
        let ordered = mapper_studio::native_processing::rivers_follow_terrain(&project);
        let initial_rivers = generated.native.as_ref().is_some_and(|n| n.source.rivers.is_some());
        if ordered && project.terrain_key().rivers.is_some()
            && (initial_rivers || maps.rivers.is_none())
        {
            return Err("Ordered generation must derive rivers only during composition".into());
        }
        let map = destination.join(format!("world-{index}.map"));
        if !map_export::write(
            map_export::Input {
                project: &generated.recipe,
                maps,
                solution: &generated.solution,
                root,
                catalog: &catalog,
            },
            &map,
            false,
            || false,
        )? {
            return Err("Map export cancelled".into());
        }
        rows.push(serde_json::json!({
            "index": index, "bundle":bundle, "map":map,
            "ordered_terrain": ordered, "initial_river_pass": initial_rivers,
            "monument_seed":generated.recipe.seeds[4].value,
            "placed":generated.solution.outcomes.iter().filter(|o|o.position.is_some()).count(),
            "required_missing":generated.solution.required_missing,
            "roads":maps.roads.as_ref().map_or(0,|r|r.roads.len()),
            "rails":maps.rails.as_ref().map_or(0,|r|r.tracks.len()),
        }));
    }
    let report = serde_json::json!({"format":"mapper-owned-generation-pair-1", "worlds":rows});
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination.join("report.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
