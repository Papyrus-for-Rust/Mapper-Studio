use super::*;

fn fixture() -> (
    tempfile::TempDir,
    Project,
    Preview,
    Solution,
    Arc<Catalog>,
    Arc<Bindings>,
) {
    let directory = tempfile::tempdir().unwrap();
    let (catalog, bindings) = crate::prepared_assets::load(directory.path());
    let ctx = egui::Context::default();
    let mut app = crate::tests::fixture(&ctx);
    let mut project = std::env::var_os("MAPPER_COMPOSITION_BENCHMARK_PROJECT")
        .map(|path| Project::load(std::path::Path::new(&path)).unwrap())
        .unwrap_or_else(|| app.project.clone());
    project.composition_engine =
        Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
    let solution = Solution {
        outcomes: project
            .targets
            .iter()
            .map(|target| papyrus_terrain::placement::Outcome {
                id: target.instance_id,
                position: Some(papyrus_terrain::placement::Position {
                    x: target.x,
                    y: 5.,
                    z: target.z,
                    yaw: target.yaw.unwrap_or(0.),
                    clearance: target.radius,
                }),
                distance: Some(10.),
                within_radius: true,
                reason: "Source-authored terrain and placement checks passed".into(),
            })
            .collect(),
        required_missing: 0,
        attempts: 3000,
    };
    (
        directory,
        project,
        app.preview.take().unwrap(),
        solution,
        catalog,
        bindings,
    )
}

#[test]
#[ignore = "controlled editor composition maintenance benchmark"]
fn repeated_composition_maintenance_benchmark() {
    let (_directory, project, source, solution, catalog, bindings) = fixture();
    let ctx = egui::Context::default();
    let mut panel = CompositionPanel::default();
    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        for _ in 0..400 {
            panel.tick(
                &project,
                Inputs {
                    source: Some(&source),
                    solution: Some(&solution),
                    catalog: Some(catalog.clone()),
                    bindings: Some(bindings.clone()),
                    monuments: None,
                    identities: None,
                    road_layout: None,
                },
                &ctx,
            );
            std::hint::black_box(&panel);
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    assert!(panel.wanted.is_some());
    assert!(panel.task.is_none());
    samples.sort_by(f64::total_cmp);
    println!("400 composition maintenance ticks, milliseconds: {samples:?}");
    println!("Median milliseconds per tick: {}", samples[2] / 400.);
}

#[test]
#[ignore = "controlled editor placement compatibility benchmark"]
fn repeated_placement_compatibility_benchmark() {
    let (_directory, project, _source, _solution, _catalog, _bindings) = fixture();
    let ctx = egui::Context::default();
    let mut app = crate::tests::fixture(&ctx);
    app.project = project.clone();
    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        for _ in 0..2000 {
            assert!(std::hint::black_box(
                app.placement_recipe_matches(std::hint::black_box(&project))
            ));
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    samples.sort_by(f64::total_cmp);
    println!("2000 placement compatibility checks, milliseconds: {samples:?}");
    println!("Median milliseconds per check: {}", samples[2] / 2000.);
}

#[test]
fn unchanged_requests_reuse_the_recipe_and_layout_snapshots() {
    let (_directory, project, source, solution, catalog, bindings) = fixture();
    let ctx = egui::Context::default();
    let mut panel = CompositionPanel::default();
    for _ in 0..2 {
        panel.tick(
            &project,
            Inputs {
                source: Some(&source),
                solution: Some(&solution),
                catalog: Some(catalog.clone()),
                bindings: Some(bindings.clone()),
                monuments: None,
                identities: None,
                road_layout: None,
            },
            &ctx,
        );
    }
    let recipe_snapshot = panel.wanted.as_ref().unwrap().project.clone();
    let layout_snapshot = panel.layout_cache.as_ref().unwrap().0.outcomes.as_ptr();
    let fingerprint = composition::layout_fingerprint(&solution);
    for _ in 0..20 {
        panel.tick(
            &project,
            Inputs {
                source: Some(&source),
                solution: Some(&solution),
                catalog: Some(catalog.clone()),
                bindings: Some(bindings.clone()),
                monuments: None,
                identities: None,
                road_layout: None,
            },
            &ctx,
        );
        let key = panel.wanted.as_ref().unwrap();
        assert!(Arc::ptr_eq(&recipe_snapshot, &key.project));
        assert_eq!(
            layout_snapshot,
            panel.layout_cache.as_ref().unwrap().0.outcomes.as_ptr()
        );
        assert_eq!(key.layout, Some(fingerprint));
    }
    let mut edited = project.clone();
    edited.targets[0].x += 10.;
    let next = panel.request_key(&edited, &source, Some(&solution), &catalog, &bindings);
    assert!(!Arc::ptr_eq(&recipe_snapshot, &next.project));
    assert_eq!(next.project.as_ref(), &edited);
    assert_eq!(recipe_snapshot.as_ref(), &project);
}

#[test]
fn cached_layout_identity_preserves_every_fingerprint_input() {
    let (_directory, project, source, solution, catalog, bindings) = fixture();
    let mut panel = CompositionPanel::default();
    let original = panel.request_key(&project, &source, Some(&solution), &catalog, &bindings);
    for change in 0..14 {
        let mut changed = solution.clone();
        let first = &mut changed.outcomes[0];
        match change {
            0 => first.id += 1,
            1 => first.position.as_mut().unwrap().x += 1.,
            2 => first.position.as_mut().unwrap().y += 1.,
            3 => first.position.as_mut().unwrap().z += 1.,
            4 => first.position.as_mut().unwrap().yaw += 1.,
            5 => first.position.as_mut().unwrap().clearance += 1.,
            6 => first.position = None,
            7 => first.distance = None,
            8 => first.distance = Some(11.),
            9 => first.within_radius = false,
            10 => first.reason.push('!'),
            11 => changed.required_missing += 1,
            12 => changed.attempts += 1,
            _ => {
                changed.outcomes.pop();
            }
        }
        assert!(!same_layout(&solution, &changed), "Change {change}");
        let key = panel.request_key(&project, &source, Some(&changed), &catalog, &bindings);
        assert_ne!(key.layout, original.layout, "Change {change}");
        assert_eq!(key.layout, Some(composition::layout_fingerprint(&changed)));
    }
    let mut a = solution.clone();
    a.outcomes[0].position.as_mut().unwrap().x = 0.;
    a.outcomes[0].distance = Some(f32::from_bits(0x7fc0_0001));
    assert!(same_layout(&a, &a.clone()));
    for change in 0..2 {
        let mut b = a.clone();
        if change == 0 {
            b.outcomes[0].position.as_mut().unwrap().x = -0.;
        } else {
            b.outcomes[0].distance = Some(f32::from_bits(0x7fc0_0002));
        }
        assert!(!same_layout(&a, &b));
        let previous = panel.request_key(&project, &source, Some(&a), &catalog, &bindings);
        let next = panel.request_key(&project, &source, Some(&b), &catalog, &bindings);
        assert_ne!(previous.layout, next.layout);
        assert_eq!(next.layout, Some(composition::layout_fingerprint(&b)));
    }
}

#[test]
fn borrowed_requests_cancel_stale_work_when_the_recipe_or_sources_change() {
    for change in ["recipe", "layout", "catalog", "bindings", "missing inputs"] {
        let (_directory, mut project, source, mut solution, mut catalog, mut bindings) = fixture();
        let ctx = egui::Context::default();
        let mut panel = CompositionPanel::default();
        let key = panel.request_key(&project, &source, Some(&solution), &catalog, &bindings);
        panel.wanted = Some(key.clone());
        let (sender, receiver) = mpsc::channel();
        sender.send(Err("Obsolete worker failure".into())).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        panel.task = Some(Task {
            key,
            palette: None,
            cancel: cancel.clone(),
            receiver,
            live: None,
        });
        match change {
            "recipe" => project.master_seed += 1,
            "layout" => solution.outcomes[0].position.as_mut().unwrap().yaw += 1.,
            "catalog" => Arc::get_mut(&mut catalog).unwrap().sha256 = "c".repeat(64),
            "bindings" => Arc::get_mut(&mut bindings).unwrap().sha256 = "d".repeat(64),
            _ => {}
        }
        panel.tick(
            &project,
            Inputs {
                source: (change != "missing inputs").then_some(&source),
                solution: Some(&solution),
                catalog: Some(catalog),
                bindings: Some(bindings),
                monuments: None,
                identities: None,
                road_layout: None,
            },
            &ctx,
        );
        assert!(cancel.load(Ordering::Relaxed), "Change {change}");
        assert!(panel.task.is_none());
        assert!(panel.message.is_empty());
        assert!(panel.published_layout.is_none());
    }
}

#[test]
fn borrowed_placement_checks_match_the_previous_normalized_recipe_policy() {
    let ctx = egui::Context::default();
    let mut app = crate::tests::fixture(&ctx);
    app.project.roads = Some(mapper_studio::infrastructure::Recipe::new("a".repeat(64)));
    app.project.rails = Some(mapper_studio::rail_generation::Recipe {
        engine: mapper_studio::rail_generation::VERSION.into(),
        source_sha256: "b".repeat(64),
        mesh_sha256: "c".repeat(64),
        settings: mapper_studio::rail_generation::initial_settings(123),
    });
    app.project.decoration = Some(mapper_studio::decor_source::Recipe::new("d".repeat(64)));
    app.project.objects = Some(mapper_studio::object_generation::Recipe::new(
        "e".repeat(64),
    ));
    let original = app.project.clone();
    let mut changed = original.clone();
    changed.schema += 1;
    changed.seed_derivation += 1;
    changed.preview_engine.push('!');
    changed.placement_engine.push('!');
    changed.master_seed += 1;
    changed.size += 50;
    changed.targets[0].x += 10.;
    changed.performance.monuments = !changed.performance.monuments;
    changed.composition_engine = None;
    changed.roads = None;
    changed.rails = None;
    changed.decoration = None;
    changed.objects = None;
    for seed in &mut changed.seeds {
        seed.value += 1;
        seed.inherited = !seed.inherited;
    }
    let original_json = serde_json::to_value(&original).unwrap();
    let changed_json = serde_json::to_value(&changed).unwrap();
    let mut checked = 0;
    let fields: std::collections::BTreeSet<_> = original_json
        .as_object()
        .unwrap()
        .keys()
        .chain(changed_json.as_object().unwrap().keys())
        .collect();
    for field in fields {
        let mut candidate = original_json.clone();
        candidate[field] = changed_json
            .get(field)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let recipe: Project = serde_json::from_value(candidate).unwrap();
        assert_previous_policy(&app, &recipe);
        checked += 1;
    }
    for index in 0..6 {
        let mut recipe = original.clone();
        recipe.seeds[index] = changed.seeds[index];
        assert_previous_policy(&app, &recipe);
        assert_eq!(
            app.placement_recipe_matches(&recipe),
            [3, 5].contains(&index)
        );
    }
    assert!(checked >= 12);
    assert_previous_policy(&app, &changed);
}

fn assert_previous_policy(app: &crate::Studio, recipe: &Project) {
    let mut previous = app.project.clone();
    previous.roads = recipe.roads.clone();
    previous.rails = recipe.rails.clone();
    previous.seeds[3] = recipe.seeds[3];
    previous.decoration = recipe.decoration.clone();
    previous.topology_edits = recipe.topology_edits.clone();
    previous.seeds[5] = recipe.seeds[5];
    assert_eq!(app.placement_recipe_matches(recipe), previous == *recipe);
}
