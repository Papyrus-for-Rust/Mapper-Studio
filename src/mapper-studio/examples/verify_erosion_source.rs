//! Verify erosion policy/phase binding using prepared installation caches.
use mapper_studio::{
    Project, erosion_source, infrastructure, monument_stages, prefab_assets, road_object_source,
};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};

fn texture_hash(prepared: &mapper_studio::composition::Prepared) -> String {
    let stamp = prepared.stamp([0.; 3], 0.);
    let mut hash = Sha256::new();
    let splat = stamp.splat;
    for (name, image) in [
        ("height", stamp.height),
        ("blend", stamp.blend),
        ("alpha", stamp.alpha),
        ("biome", stamp.biome),
        ("topology", stamp.topology),
        ("splat0", splat.map(|s| s[0])),
        ("splat1", splat.map(|s| s[1])),
    ] {
        hash.update(name.as_bytes());
        if let Some(image) = image {
            hash.update((image.width as u64).to_le_bytes());
            hash.update((image.height as u64).to_le_bytes());
            hash.update(image.rgba);
        }
    }
    format!("{:x}", hash.finalize())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let place_early = args.first().is_some_and(|arg| arg == "--place-early");
    if place_early {
        args.remove(0);
    }
    if !matches!(args.len(), 4 | 5) {
        return Err("[--place-early] RUST_ROOT PROJECT CATALOG [BINDINGS] NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let destination = Path::new(args.last().unwrap());
    if destination.try_exists().map_err(|e| e.to_string())? {
        return Err("Erosion source report must be new".into());
    }
    let project = Project::load(Path::new(&args[1]))?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing project installation")?;
    let _verification = mapper_studio::installation::VerificationScope::default();
    let catalog = prefab_assets::load(root, Path::new(&args[2]), installation, || false)?;
    let infrastructure = infrastructure::Source::cached(
        &catalog,
        project
            .roads
            .as_ref()
            .ok_or("Missing prepared infrastructure")?,
        || false,
    )?;
    let mut objects = road_object_source::Source::cached(
        root,
        &project
            .objects
            .as_ref()
            .ok_or("Missing prepared object workflow")?
            .road_source_sha256,
        &infrastructure,
        || false,
    )?;
    let source = erosion_source::Source::prepare(root, installation, &objects, || false)?
        .ok_or("Erosion source verification cancelled")?;
    if erosion_source::Source::prepare(root, installation, &objects, || true)?.is_some() {
        return Err("Cancelled erosion source published a binding".into());
    }
    let mut unknown = installation.clone();
    unknown.native.sha256 = "0".repeat(64);
    if erosion_source::Source::prepare(root, &unknown, &objects, || false).is_ok() {
        return Err("Unknown Rust native source accepted an erosion binding".into());
    }
    objects.workflow.as_mut().ok_or("Missing workflow")?.stages[0]
        .description
        .push_str(" changed");
    if erosion_source::Source::prepare(root, installation, &objects, || false).is_ok() {
        return Err("Modified workflow accepted an erosion binding".into());
    }
    let stage_binding = project
        .monument_rules
        .as_ref()
        .and_then(|r| r.stage_rules.as_ref())
        .ok_or("Missing prepared monument programs")?;
    let monuments = monument_stages::Source::cached(stage_binding, root, &catalog, || false)?;
    let early_paths = source.early_paths(&monuments)?;
    let early_targets = project
        .targets
        .iter()
        .filter(|target| {
            target
                .prefab_path
                .as_ref()
                .is_some_and(|path| early_paths.contains(path))
        })
        .map(|target| target.instance_id)
        .collect::<Vec<_>>();
    let resolution = papyrus_terrain::native_layers::dimensions(project.size)?.0;
    let policies = source.policies(project.size, resolution, 0.)?;
    let priorities = if args.len() == 5 {
        let bindings = mapper_studio::prefab_textures::Bindings::load(
            root,
            Path::new(&args[3]),
            &catalog,
            || false,
        )?;
        let rules = mapper_studio::monument_source::Source::cached(
            project.monument_rules.as_ref().ok_or("Missing rules")?,
            &catalog,
            &bindings,
            || false,
        )?;
        let mut effective = project.clone();
        effective.monument_rules = Some(rules.recipe_with_source_defaults(&catalog)?);
        mapper_studio::monument_source::Source::cached(
            effective.monument_rules.as_ref().unwrap(),
            &catalog,
            &bindings,
            || false,
        )?;
        let mut probe = catalog
            .entries
            .iter()
            .filter(|(_, a)| {
                a.modifier.as_ref().is_some_and(|m| m.height_map)
                    && a.texture_guids.contains_key("height")
            })
            .min_by_key(|(path, _)| *path)
            .ok_or("Missing water-hook probe asset")?
            .1
            .clone();
        let unused_guid = "f".repeat(32);
        if bindings.textures().contains_key(&unused_guid) {
            return Err("Water-hook probe GUID unexpectedly resolves".into());
        }
        probe.texture_guids.insert("water".into(), unused_guid);
        probe.modifier.as_mut().unwrap().water_map = Some(true);
        let enabled =
            mapper_studio::composition::prepare(&probe, &bindings, project.size, || false)?
                .ok_or("Water-enabled preparation cancelled")?;
        let bytes = enabled.bytes();
        let hash = texture_hash(&enabled);
        drop(enabled);
        probe.modifier.as_mut().unwrap().water_map = Some(false);
        let disabled =
            mapper_studio::composition::prepare(&probe, &bindings, project.size, || false)?
                .ok_or("Water-disabled preparation cancelled")?;
        if bytes == 0 || disabled.bytes() != bytes || texture_hash(&disabled) != hash {
            return Err("Empty water hook changed prepared terrain textures".into());
        }
        let water_hook_probe = serde_json::json!({"prepared_bytes": bytes,
            "terrain_texture_sha256": hash, "unused_water_guid_unresolved": true, "enabled_disabled_identical": true});
        let rows = project.targets.iter().filter(|target| early_targets.contains(&target.instance_id)).map(|target| {
            let asset = catalog.entries.get(target.prefab_path.as_deref().unwrap()).ok_or("Missing early asset")?;
            let before = mapper_studio::placement_preview::bounds_for_recipe(&project, asset).err();
            let (bounds, priority) = mapper_studio::placement_preview::bounds_for_recipe(&effective, asset)?;
            let prepared = mapper_studio::composition::prepare(asset, &bindings, project.size, || false)?
                .ok_or("Early asset preparation cancelled")?;
            Ok(serde_json::json!({"prepared_bytes": prepared.bytes(), "terrain_texture_sha256": texture_hash(&prepared), "id": target.instance_id, "previous_error": before, "priority": priority,
                "footprints": bounds.iter().map(|b| serde_json::json!({"center": b.center, "extent": b.extent})).collect::<Vec<_>>() }))
        }).collect::<Result<Vec<_>, String>>()?;
        Some(
            serde_json::json!({"revision": effective.monument_rules.as_ref().unwrap().surface_modifiers, "bounds": rows, "water_hook_probe": water_hook_probe}),
        )
    } else {
        None
    };
    let early_generation = if place_early {
        if args.len() != 5 {
            return Err("Early terrain placement requires texture bindings".into());
        }
        let bindings = mapper_studio::prefab_textures::Bindings::load(
            root,
            Path::new(&args[3]),
            &catalog,
            || false,
        )?;
        let cache =
            mapper_studio::terrain_cache::directory().ok_or("Missing native terrain cache")?;
        let raw = mapper_studio::terrain_cache::load(&cache, &project.terrain_key(), || false)?
            .ok_or("Missing saved pre-river terrain")?;
        let generated = mapper_studio::terrain_placement::generate(
            &project,
            &raw.layers,
            &catalog,
            &bindings,
            &source,
            || false,
            |stage, count| eprintln!("Early terrain phase {stage}/{count}"),
        )?
        .ok_or("Early terrain placement cancelled")?;
        if mapper_studio::terrain_placement::generate(
            &project,
            &raw.layers,
            &catalog,
            &bindings,
            &source,
            || true,
            |_, _| {},
        )?
        .is_some()
        {
            return Err("Cancelled early placement published terrain".into());
        }
        let changed_height = generated
            .layers
            .height
            .iter()
            .zip(&raw.layers.height)
            .filter(|(a, b)| a != b)
            .count();
        let height_sha256 = format!(
            "{:x}",
            Sha256::digest(bytemuck::cast_slice::<i16, u8>(&generated.layers.height))
        );
        let outcomes = generated.solution.outcomes.iter().map(|o| serde_json::json!({
            "id":o.id, "position":o.position.as_ref().map(|p| [p.x,p.y,p.z,p.yaw,p.clearance]), "reason":o.reason,
        })).collect::<Vec<_>>();
        Some(
            serde_json::json!({"stages":generated.stages, "outcomes":outcomes,
                "original_proposals":generated.proposals,
            "surface_modifiers":generated.recipe.monument_rules.as_ref().unwrap().surface_modifiers, "changed_height_samples":changed_height,
                "height_sha256":height_sha256, "alpha_sha256":format!("{:x}",Sha256::digest(&generated.alpha)),
                "splat_sha256":format!("{:x}",Sha256::digest(&generated.layers.splat)),
                "topology_sha256":format!("{:x}",Sha256::digest(bytemuck::cast_slice::<i32,u8>(&generated.layers.topology))),
                "biome_sha256":format!("{:x}",Sha256::digest(&generated.layers.biome)),
                "cancellation_verified":true,
            }),
        )
    } else {
        None
    };
    let report = serde_json::json!({
        "format": "mapper-owned-erosion-source-verification",
        "plan": source.plan(),
        "frame": {"size": project.size, "height_resolution": resolution, "ocean_level": 0.},
        "grid": policies.grid, "bounds": policies.bounds, "paint": policies.paint,
        "early_paths": early_paths, "early_target_ids": early_targets,
        "cancellation_verified": true, "unknown_native_rejected": true,
        "modified_workflow_rejected": true,
        "priority_admission": priorities, "early_generation": early_generation,
        "map_generation_executed": false, "normal_generate_integrated": false,
    });
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    output.write_all(&bytes).map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
