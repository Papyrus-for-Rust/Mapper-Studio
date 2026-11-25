use super::*;
use std::sync::Arc;
use papyrus_terrain::{
    native_layers,
    placement::{Outcome, Position},
};

fn fixture() -> (Project, Composed, Solution) {
    let mut project = Project {
        size: 1000,
        targets: Vec::new(),
        preview_engine: crate::native_preview::VERSION.into(),
        composition_engine: Some(papyrus_terrain::monument_composition::WATER_VERSION.into()),
        installation: Some(crate::installation::Installation {
            kind: crate::installation::InstallationKind::Client,
            native: crate::installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        }),
        monument_assets: Some(crate::AssetIdentity {
            catalog_sha256: "b".repeat(64),
            bindings_sha256: "c".repeat(64),
        }),
        ..Default::default()
    };
    project.performance.monuments = false;
    let (hr, sr) = native_layers::dimensions(project.size).unwrap();
    let layers = native_layers::Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: vec![16000; hr * hr],
        biome: vec![0; sr * sr * 5],
        splat: vec![0; sr * sr * 8],
        topology: vec![0; sr * sr],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let maps = papyrus_terrain::monument_composition::compose_with_water(
        &layers,
        &vec![255; sr * sr],
        &[],
        || false,
    )
    .unwrap()
    .unwrap();
    let solution = Solution {
        outcomes: Vec::new(),
        required_missing: 0,
        attempts: 0,
    };
    project.validate().unwrap();
    (project, maps, solution)
}

#[test]
fn shared_objects_own_the_scenery_sources_and_both_stage_seeds() {
    let (mut project, mut maps, solution) = fixture();
    project.roads = Some(crate::infrastructure::Recipe::new("d".repeat(64)));
    project.decoration = Some(crate::decor_source::Recipe::new("e".repeat(64)));
    project.objects = Some(crate::object_generation::Recipe::new("f".repeat(64)));
    project.monument_rules = Some(serde_json::from_value(serde_json::json!({"engine":crate::monument_source::VERSION, "source_sha256":"1".repeat(64)})).unwrap());
    maps.roads = papyrus_terrain::road_network::build(
        &mut maps.layers,
        &maps.alpha,
        &[],
        &[],
        papyrus_terrain::road_network::Settings {
            seed: project.seeds[3].value,
            width: 10.,
            shoulder: 8.,
            maximum_grade: 0.2,
            maximum_cut_fill: 4.,
            terrain_offset: -0.125,
            road_topology: 2048,
            roadside_topology: 4096,
            protected_topology: 1024,
            splat_channel: 7,
        },
        || false,
    )
    .unwrap();
    maps.roads.as_mut().unwrap().recipe = Some(
        project.roads.as_ref().unwrap().result_receipt(project.seeds[3].value).unwrap(),
    );
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Requested shared objects")
    );
    let terrain = papyrus_terrain::footprints::RoadTransaction::new_objects(
        &maps.layers,
        &maps.alpha,
        Default::default(),
        u32::MAX,
        1024,
        || false,
    )
    .unwrap()
    .unwrap()
    .finish(|| false)
    .unwrap()
    .unwrap()
    .report;
    maps.objects = Some(papyrus_terrain::object_scene::Scene {
        utility: None,
        uniform_cliffs_source_sha256: None,
        coastal_cliffs_source_sha256: None,
        jungle_vines_source_sha256: None,
        initial_rail_reservations: None,
        engine: crate::object_generation::VERSION,
        road_seed: project.seeds[3].value,
        decoration_seed: project.seeds[5].value,
        road_source_sha256: "f".repeat(64),
        decoration_source_sha256: "e".repeat(64),
        monument_source_sha256: "1".repeat(64),
        initial_blockers_sha256: None,
        initial_blockers: papyrus_terrain::placement_blockers::Report {
            engine: papyrus_terrain::placement_blockers::VERSION,
            instances: 0,
            modifiers: 0,
            clipped_modifiers: 0,
            sample_visits: 0,
            blocked_cells: 0,
        },
        instances: Vec::new(),
        sockets: Vec::new(),
        decoration: Vec::new(),
        terrain,
        external_phases: Vec::new(),
        assembled_instances: 0,
        physics_validated: false,
    });
    validate(&project, &maps, &solution).unwrap();
    for field in ["road", "decoration", "monuments"] {
        let scene = maps.objects.as_mut().unwrap();
        let source = match field {
            "road" => &mut scene.road_source_sha256,
            "decoration" => &mut scene.decoration_source_sha256,
            _ => &mut scene.monument_source_sha256,
        };
        let saved = std::mem::replace(source, "a".repeat(64));
        assert!(
            validate(&project, &maps, &solution)
                .unwrap_err()
                .contains("sources or seeds")
        );
        let scene = maps.objects.as_mut().unwrap();
        match field {
            "road" => scene.road_source_sha256 = saved,
            "decoration" => scene.decoration_source_sha256 = saved,
            _ => scene.monument_source_sha256 = saved,
        };
    }
    project
        .objects
        .as_mut()
        .unwrap()
        .uniform_cliffs_source_sha256 = Some("a".repeat(64));
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("sources or seeds")
    );
    maps.objects.as_mut().unwrap().uniform_cliffs_source_sha256 = Some("a".repeat(64));
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("hill-cliff placement is incomplete")
    );
    project
        .objects
        .as_mut()
        .unwrap()
        .uniform_cliffs_source_sha256 = None;
    maps.objects.as_mut().unwrap().uniform_cliffs_source_sha256 = None;
    project.objects.as_mut().unwrap().coastal_cliffs_source_sha256 = Some("f".repeat(64));
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("sources or seeds"));
    maps.objects.as_mut().unwrap().coastal_cliffs_source_sha256 = Some("f".repeat(64));
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("coastal cliff placement is incomplete"));
    project.objects.as_mut().unwrap().coastal_cliffs_source_sha256 = None;
    maps.objects.as_mut().unwrap().coastal_cliffs_source_sha256 = None;
    project.objects.as_mut().unwrap().jungle_vines_source_sha256 = Some("a".repeat(64));
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("sources or seeds"));
    maps.objects.as_mut().unwrap().jungle_vines_source_sha256 = Some("a".repeat(64));
    maps.objects.as_mut().unwrap().external_phases.push(
        papyrus_terrain::object_scene::PendingPhase {
            scene_order: 81, class: "PlaceJungleVines".into(), description: "Vines".into(),
        },
    );
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("vine-tree placement is incomplete"));
    maps.objects.as_mut().unwrap().external_phases.clear();
    project.objects.as_mut().unwrap().jungle_vines_source_sha256 = None;
    maps.objects.as_mut().unwrap().jungle_vines_source_sha256 = None;
    maps.objects.as_mut().unwrap().road_seed += 1;
    assert!(validate(&project, &maps, &solution).is_err());
    maps.objects.as_mut().unwrap().road_seed -= 1;
    maps.objects.as_mut().unwrap().decoration_seed += 1;
    assert!(validate(&project, &maps, &solution).is_err());
    maps.objects.as_mut().unwrap().decoration_seed -= 1;
    validate(&project, &maps, &solution).unwrap();
    project.objects = None;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("absent from the recipe")
    );
}

#[test]
fn road_exports_reject_changed_sources_seeds_and_earthwork_limits() {
    let (mut project, mut maps, solution) = fixture();
    let recipe = crate::infrastructure::Recipe::new("d".repeat(64));
    project.roads = Some(recipe.clone());
    maps.roads = Some(papyrus_terrain::road_network::Report::default());
    let message = "Generated road source, seed or settings differ from the recipe";
    assert_eq!(validate(&project, &maps, &solution).unwrap_err(), message);
    maps.roads.as_mut().unwrap().recipe =
        Some(recipe.result_receipt(project.seeds[3].value).unwrap());
    validate(&project, &maps, &solution).unwrap();

    let mut changed = project.clone();
    changed.roads.as_mut().unwrap().source_sha256 = "e".repeat(64);
    assert_eq!(validate(&changed, &maps, &solution).unwrap_err(), message);
    let mut changed = project.clone();
    changed.roads.as_mut().unwrap().maximum_cut_fill += 1.;
    assert_eq!(validate(&changed, &maps, &solution).unwrap_err(), message);
    let mut changed = project;
    changed.seeds[3].value += 1;
    changed.seeds[3].inherited = false;
    assert_eq!(validate(&changed, &maps, &solution).unwrap_err(), message);
}

#[test]
fn omitted_requested_outputs_and_unrequested_outputs_are_rejected() {
    let (mut project, mut maps, solution) = fixture();
    validate(&project, &maps, &solution).unwrap();
    project.roads = Some(crate::infrastructure::Recipe::new("d".repeat(64)));
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Requested roads")
    );
    project.roads = None;
    project.rivers = Some(crate::river_generation::Recipe {
        engine: papyrus_terrain::river_pipeline::VERSION.into(),
        source_sha256: "e".repeat(64),
        source_document: None,
        objects: None,
    });
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Requested rivers")
    );
    project.rivers = None;
    project.decoration = Some(crate::decor_source::Recipe::new("f".repeat(64)));
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Requested scenery")
    );
    maps.decoration = Some(papyrus_terrain::decor::Report {
        engine: papyrus_terrain::decor::VERSION,
        source_sha256: "f".repeat(64),
        seed: project.seeds[5].value,
        candidates: Vec::new(),
        stages: Vec::new(),
        validated_instances: 0,
        budget_exhausted: false,
        source_issues: Vec::new(),
        pending_checks: Vec::new(),
        fitting: None,
        terrain_placement: None,
    });
    validate(&project, &maps, &solution).unwrap();
    maps.decoration.as_mut().unwrap().seed += 1;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("source or seed")
    );
    maps.decoration.as_mut().unwrap().seed -= 1;
    maps.decoration.as_mut().unwrap().source_sha256 = "a".repeat(64);
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("source or seed")
    );
    project.decoration = None;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("absent from the recipe")
    );
    maps.decoration = None;
    maps.ocean = None;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("completed water")
    );
}

#[test]
fn export_rejects_unfinished_surface_rail_outputs() {
    use papyrus_terrain::{gameplay_height, path_geometry, path_mesh, rail_bed, rail_reservations, rail_scene};
    use sha2::{Digest, Sha256};
    use serde_json::json;

    let (mut project, mut maps, solution) = fixture();
    let roads = crate::infrastructure::Recipe::new("f".repeat(64));
    maps.roads = Some(papyrus_terrain::road_network::Report {
        recipe: Some(roads.result_receipt(project.seeds[3].value).unwrap()),
        ..Default::default()
    });
    project.roads = Some(roads);
    project.monument_rules = Some(serde_json::from_value(json!({
        "engine": crate::monument_source::VERSION,
        "source_sha256": "1".repeat(64),
    })).unwrap());
    let mut recipe = crate::rail_generation::Recipe {
        engine: crate::rail_generation::VERSION.into(),
        source_sha256: "d".repeat(64),
        mesh_sha256: "e".repeat(64),
        settings: crate::rail_generation::initial_settings(project.master_seed),
    };
    recipe.settings.sort_all_objects();
    let settings = serde_json::to_vec(&recipe.settings).unwrap();
    let report = json!({
        "composition_engine": recipe.engine,
        "source_sha256": recipe.source_sha256,
        "mesh_source_sha256": recipe.mesh_sha256,
        "recipe_settings_sha256": format!("{:x}", Sha256::digest(settings)),
    });
    project.rails = Some(recipe);
    let path = path_geometry::Path::from_controls(
        &[[0., 40., 0.], [20., 40., 0.]], 1000, || false,
    ).unwrap().unwrap();
    maps.rails = Some(rail_scene::Scene {
        report,
        tracks: vec![rail_bed::FittedTrack { id: 1, preserve_start: false, path, maximum_discrete_grade: 0. }],
        meshes: vec![],
        reservations: rail_reservations::Output {
            raster: vec![], samples: vec![],
            report: rail_reservations::Report {
                engine: rail_reservations::VERSION, tracks: 1, sweeps: 0,
                initial_blocked: 0, newly_blocked: 0, final_blocked: 0, cell_visits: 0, work: 0,
            },
        },
    });
    let height = gameplay_height::generate(
        &maps.layers.height, maps.layers.height_resolution, maps.layers.size as f32, &[],
        1000, || false,
    ).unwrap().unwrap();
    maps.gameplay_height = Some(height);
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("completed track meshes"));

    let rails = maps.rails.as_mut().unwrap();
    rails.meshes.push(rail_scene::Chunk {
        track_id: 1, lod: 0, chunk_index: 0,
        geometry: path_mesh::Chunk {
            first_repetition: 0, repetitions: 1, origin: [0.; 3], work: 1,
            mesh: path_mesh::Template {
                vertices: vec![[0., 0., 0.], [1., 0., 0.], [0., 0., 1.]],
                normals: vec![[0., 1., 0.]; 3], tangents: vec![[1., 0., 0., 1.]; 3],
                uv: vec![[0.; 2]; 3], triangles: vec![[0, 1, 2]], center: [0.; 3],
                extents: [1., 0., 1.],
            },
        },
    });
    rails.report["meshes"] = json!({"chunks": 1, "tracks": [{"track_id": 1}]});
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("completed gameplay height"));
    maps.rails.as_mut().unwrap().report["gameplay_height"] = json!({
        "scope": "rail collision triangles",
        "changes": {"engine": gameplay_height::VERSION},
    });
    validate(&project, &maps, &solution).unwrap();

    maps.gameplay_height.as_mut().unwrap().height.pop();
    assert!(validate(&project, &maps, &solution).unwrap_err().contains("completed gameplay height"));
}

#[test]
fn map_export_requires_the_underground_stage_on_supported_installations() {
    use serde_json::{Value, json};
    let (mut project, mut maps, solution) = fixture();
    let (mut catalog, _) = crate::infrastructure::tests::fixture();
    project.performance.monuments = true;
    let monument_hash = "d".repeat(64);
    project.monument_rules = Some(serde_json::from_value(json!({
        "engine": crate::monument_source::VERSION,
        "source_sha256": monument_hash,
    })).unwrap());
    let check = |project: &Project, maps: &Composed, catalog: &crate::prefab_assets::Catalog| {
        crate::map_export::validate(&crate::map_export::Input {
            project, maps, solution: &solution, catalog, root: std::path::Path::new(""),
        })
    };
    project.installation = Some(catalog.installation.clone());
    check(&project, &maps, &catalog).unwrap();

    let profile: Value = serde_json::from_str(crate::metadata_runtime::CURRENT_MONUMENT_STAGE_CODE_PROFILE).unwrap();
    catalog.installation.native.sha256 = profile["native_sha256"].as_str().unwrap().into();
    catalog.sources.iter_mut().find(|source| source.relative_path == "Bundles/shared/assetscenes.bundle")
        .unwrap().sha256 = profile["scene_sha256"].as_str().unwrap().into();
    project.installation = Some(catalog.installation.clone());
    assert!(check(&project, &maps, &catalog).unwrap_err().contains("completed underground rail assembly"));

    project.performance.monuments = false;
    check(&project, &maps, &catalog).unwrap();
    project.performance.monuments = true;
    maps.tunnels = Some(papyrus_terrain::tunnel_scene::Scene {
        engine: papyrus_terrain::tunnel_scene::VERSION.into(),
        source_sha256: "e".repeat(64),
        monument_sha256: monument_hash,
        seed: crate::derive_seed(project.seeds[4].value, "Underground rails"),
        layout_fingerprint: crate::composition::layout_fingerprint(&solution),
        instances: vec![],
        report: json!({"status": "no_entrances"}),
    });
    check(&project, &maps, &catalog).unwrap();
}

#[test]
fn export_requires_requested_monument_assemblies_from_this_world() {
    let (mut project, mut maps, mut solution) = fixture();
    project.performance.monuments = true;
    let monument_hash = "d".repeat(64);
    let dungeon_hash = "e".repeat(64);
    let offshore_hash = "f".repeat(64);
    project.monument_rules = Some(
        serde_json::from_value(serde_json::json!({
            "engine": crate::monument_source::VERSION,
            "source_sha256": monument_hash,
            "stage_rules": {
                "engine": papyrus_terrain::monument_stage::VERSION,
                "source_sha256": "1".repeat(64),
            },
            "dungeon_source": {
                "engine": papyrus_terrain::dungeon_base::VERSION,
                "source_sha256": dungeon_hash,
            },
            "offshore_source": {
                "engine": crate::offshore_generation::VERSION,
                "source_sha256": offshore_hash,
            },
        }))
        .unwrap(),
    );
    let layout = crate::composition::layout_fingerprint(&solution);
    let seed = project.seeds[4].value;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("dungeon assembly")
    );
    maps.dungeons = Some(papyrus_terrain::dungeon_base::Scene {
        engine: papyrus_terrain::dungeon_base::VERSION.into(),
        source_sha256: dungeon_hash,
        monument_sha256: monument_hash.clone(),
        layout_fingerprint: layout,
        seed,
        stages: Vec::new(),
        instances: Vec::new(),
    });
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("offshore stage")
    );
    maps.offshore = Some(papyrus_terrain::offshore_search::Scene {
        engine: crate::offshore_generation::VERSION.into(),
        source_sha256: offshore_hash,
        monument_sha256: monument_hash.clone(),
        layout_fingerprint: layout,
        seed,
        stages: Vec::new(),
        instances: Vec::new(),
    });
    // Completed stages can legitimately emit no instances.
    validate(&project, &maps, &solution).unwrap();

    let offshore = maps.offshore.as_mut().unwrap();
    let saved = std::mem::replace(&mut offshore.source_sha256, "2".repeat(64));
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("offshore stage")
    );
    maps.offshore.as_mut().unwrap().source_sha256 = saved;
    maps.offshore.as_mut().unwrap().seed ^= 1;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Offshore assembly differs")
    );
    maps.offshore.as_mut().unwrap().seed = seed;

    solution.attempts += 1;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Dungeon assembly differs")
    );
    solution.attempts -= 1;
    let tunnels = papyrus_terrain::tunnel_scene::Scene {
        engine: papyrus_terrain::tunnel_scene::VERSION.into(),
        source_sha256: "3".repeat(64),
        monument_sha256: monument_hash,
        layout_fingerprint: layout,
        seed: crate::derive_seed(seed, "Underground rails"),
        instances: Vec::new(),
        report: serde_json::Value::Null,
    };
    maps.tunnels = Some(tunnels);
    validate(&project, &maps, &solution).unwrap();
    maps.tunnels.as_mut().unwrap().seed ^= 1;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Underground assembly differs")
    );
    maps.tunnels.as_mut().unwrap().seed ^= 1;

    project.performance.monuments = false;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("dungeon assembly")
    );
    maps.dungeons = None;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("offshore stage")
    );
    maps.offshore = None;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Underground assembly")
    );
    maps.tunnels = None;
    validate(&project, &maps, &solution).unwrap();
}

#[test]
fn missing_required_monuments_are_detected_even_with_a_stale_summary_count() {
    let (mut project, maps, mut solution) = fixture();
    let mut target = Project::default().targets.remove(0);
    target.x = 0.;
    target.z = 0.;
    target.radius = 100.;
    target.presence = Presence::Required;
    project.targets.push(target);
    project.performance.monuments = true;
    solution.outcomes.push(Outcome {
        id: project.targets[0].instance_id,
        position: None,
        distance: None,
        within_radius: false,
        reason: "No feasible site".into(),
    });
    assert_eq!(
        validate(&project, &maps, &solution).unwrap_err(),
        format!("1 required monument could not be placed\n{}", project.targets[0].name)
    );
    project.targets[0].presence = Presence::Allowed;
    validate(&project, &maps, &solution).unwrap();
    solution.required_missing = 1;
    assert!(validate(&project, &maps, &solution).is_err());
    solution.required_missing = 0;
    solution.outcomes[0].position = Some(Position {
        x: 0.,
        y: 0.,
        z: 0.,
        yaw: 0.,
        clearance: 1.,
    });
    project.targets[0].presence = Presence::Excluded;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Disabled monument")
    );
    project.targets[0].presence = Presence::Allowed;
    project.performance.monuments = false;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Disabled monument")
    );
    solution.outcomes[0].position = None;
    let mut duplicate_target = project.targets[0].clone();
    duplicate_target.instance_id += 1;
    project.targets.push(duplicate_target);
    solution.outcomes.push(solution.outcomes[0].clone());
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Duplicate")
    );
    solution.outcomes.pop();
    project.targets.pop();
    solution.outcomes[0].id += 1;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Missing monument outcome")
    );
}

#[test]
fn terrain_identity_and_topology_ownership_follow_the_exact_recipe() {
    let (mut project, mut maps, solution) = fixture();
    maps.layers.terrain_seed += 1;
    assert!(validate(&project, &maps, &solution).is_err());
    maps.layers.terrain_seed -= 1;
    maps.layers.biome_seed += 1;
    assert!(validate(&project, &maps, &solution).is_err());
    maps.layers.biome_seed -= 1;
    project.topology_edits = Some(Arc::new(papyrus_terrain::topology_paint::Edits {
        engine: papyrus_terrain::topology_paint::VERSION.into(),
        source_sha256: "d".repeat(64),
        size: project.size,
        enabled: true,
        strokes: vec![papyrus_terrain::topology_paint::Stroke {
            operation: papyrus_terrain::topology_paint::Operation::Add,
            mask: 1,
            radius: 8.,
            points: vec![[0., 0.]],
            shape: Default::default(),
        }],
    }));
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("Requested topology edits")
    );
    maps.topology_overrides = Some(papyrus_terrain::topology_paint::Report {
        engine: papyrus_terrain::topology_paint::VERSION,
        source_sha256: "d".repeat(64),
        strokes: 1,
        changed_cells: 0,
        skipped_hole_visits: 0,
        sample_visits: 0,
        object_filters_revalidated: false,
    });
    validate(&project, &maps, &solution).unwrap();
    maps.topology_overrides.as_mut().unwrap().strokes = 2;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("differ")
    );
    Arc::make_mut(project.topology_edits.as_mut().unwrap()).enabled = false;
    assert!(
        validate(&project, &maps, &solution)
            .unwrap_err()
            .contains("absent from the recipe")
    );
}
