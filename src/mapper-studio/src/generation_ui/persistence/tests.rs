use super::*;
use mapper_studio::project_bundle::{load_workspace_parallel, load_workspace_selected};

#[test]
fn unsupported_workspace_is_rejected_before_reading_map_payloads() {
    use std::io::{Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unsupported.mapper");
    let mut workspace = Workspace {
        schema: NAMED_RECIPES_SCHEMA + 1,
        results: Vec::new(),
        queue: Vec::new(),
        count: 1,
        randomize: [false; 7],
        open: Vec::new(),
        active: None,
        fonts: Vec::new(),
        edits: Vec::new(),
        recipes: None,
    };
    let sources = [Source::Bytes(vec![37; 2 * 1024 * 1024].into())];
    for schema in [NAMED_RECIPES_SCHEMA + 1, NAMED_RECIPES_SCHEMA] {
        workspace.schema = schema;
        mapper_studio::project_bundle::write(
            &path, Project::default(), &workspace, &sources, || false,
        ).unwrap();
        let mut file = std::fs::File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&[0]).unwrap();
        drop(file);
        let error = Generation::restore(&path, &egui::Context::default(), &|| false)
            .err().expect("Invalid workspace should fail");
        assert_eq!(error, if schema > NAMED_RECIPES_SCHEMA {
            "Unsupported saved map workspace"
        } else {
            "A saved map payload failed its project checksum"
        });
    }
}

#[test]
fn workspace_payload_reads_preserve_order_and_check_duplicate_spans() {
    use std::io::{Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large-workspace.mapper");
    let data: Arc<[u8]> = vec![42; 2 * 1024 * 1024].into();
    let other: Arc<[u8]> = vec![87; 3 * 1024 * 1024].into();
    let sources = [
        Source::Bytes(data.clone()),
        Source::Bytes(Arc::from(b"first preview".as_slice())),
        Source::Bytes(other.clone()),
        Source::Bytes(Arc::from(b"second preview".as_slice())),
        Source::Bytes(data.clone()),
    ];
    assert!(mapper_studio::project_bundle::write(
        &path, Project::default(), serde_json::Value::Null, &sources, || false,
    ).unwrap());
    let restored = load_workspace_parallel::<serde_json::Value>(&path, || false).unwrap().unwrap();
    assert_eq!(restored.blobs.len(), sources.len());
    assert_eq!(std::fs::read(restored.blobs[0].as_ref()).unwrap(), *data);
    assert_eq!(std::fs::read(restored.blobs[1].as_ref()).unwrap(), b"first preview");
    assert_eq!(std::fs::read(restored.blobs[2].as_ref()).unwrap(), *other);
    assert_eq!(std::fs::read(restored.blobs[3].as_ref()).unwrap(), b"second preview");
    assert!(Arc::ptr_eq(&restored.blobs[0], &restored.blobs[4]));
    // An active generation keeps the pool busy while the workspace opens.
    let selected = mapper_studio::generation_resources::with_cpu_pool(|| false, |_, _| {
        load_workspace_selected::<serde_json::Value>(
            &path,
            |_, _| Ok(vec![false, true, false, false, true]),
            || false,
        )
    })
    .unwrap()
    .unwrap();
    assert!(selected.blobs[0].is_none());
    assert!(selected.blobs[2].is_none());
    assert!(selected.blobs[3].is_none());
    let preview = selected.blobs[1].as_ref().unwrap();
    let map = selected.blobs[4].as_ref().unwrap();
    assert_eq!(std::fs::read(preview.as_ref()).unwrap(), b"first preview");
    assert_eq!(std::fs::read(map.as_ref()).unwrap(), *data);

    let checks = std::sync::atomic::AtomicUsize::new(0);
    assert!(
        load_workspace_parallel::<serde_json::Value>(&path, || {
            checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 5
        })
        .unwrap()
        .is_none()
    );

    let original = std::fs::read(&path).unwrap();
    let probe = directory.path().join("counted-save.mapper");
    let calls = std::cell::Cell::new(0);
    let count_check = || calls.set(calls.get() + 1);
    assert!(mapper_studio::project_bundle::write(
        &probe, Project::default(), serde_json::json!({"saved": true}), &sources,
        || { count_check(); false },
    ).unwrap());
    let before_flush = calls.replace(0) - 1;
    assert!(!mapper_studio::project_bundle::write(
        &path, Project::default(), serde_json::json!({"saved": true}), &sources,
        || { count_check(); calls.get() >= before_flush },
    ).unwrap());
    assert_eq!(calls.get(), before_flush);
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let mut changed = std::fs::File::options().write(true).open(&path).unwrap();
    changed.seek(SeekFrom::End(-1)).unwrap();
    changed.write_all(&[0]).unwrap();
    drop(changed);
    let failure = load_workspace_parallel::<serde_json::Value>(&path, || false).err();
    assert_eq!(failure.as_deref(), Some("A saved map payload failed its project checksum"));
    let failure = load_workspace_selected::<serde_json::Value>(
        &path, |_, _| Ok(vec![true, false, false, false, false]), || false,
    ).err();
    assert_eq!(failure.as_deref(), Some("A saved map payload failed its project checksum"));
    assert_eq!(std::fs::read(restored.blobs[4].as_ref()).unwrap(), *data);
}

#[test]
fn incomplete_maps_reopen_and_legacy_ready_errors_are_migrated() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let current = directory.path().join("incomplete.mapper");
    let legacy = directory.path().join("legacy.mapper");
    let mut app = generated(&ctx);
    let id = app.generation.results[0].id;
    let issue = "Requested scenery output is missing";
    app.generation.results[0].archive_issue = issue.into();
    app.generation.results[0].archive = Some(archive());
    app.generation.queue[0].state = State::Incomplete;
    app.generation.queue[0].detail = issue.into();
    assert!(app.focused_map_archive().is_none());
    app.save_project_to(current.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    let mut saved = mapper_studio::project_bundle::load(&current, || false)
        .unwrap()
        .unwrap();
    assert_eq!(saved.manifest.workspace["schema"], NAMED_RECIPES_SCHEMA);
    assert_eq!(saved.manifest.workspace["queue"][0]["origin"]["name"], "Recipe");
    saved.manifest.workspace["schema"] = 1.into();
    saved.manifest.workspace["queue"][0].as_object_mut().unwrap().remove("origin");
    saved.manifest.workspace["queue"][0]["state"] = "Ready".into();
    saved.manifest.workspace["queue"][0]["detail"] = "".into();
    let sources = saved
        .blobs
        .iter()
        .cloned()
        .map(Source::File)
        .collect::<Vec<_>>();
    mapper_studio::project_bundle::write(
        &legacy,
        saved.manifest.recipe,
        saved.manifest.workspace,
        &sources,
        || false,
    )
    .unwrap();
    for path in [current, legacy] {
        let mut reopened = crate::tests::fixture(&ctx);
        reopened.open_project_from(path, &ctx);
        wait_io(&mut reopened, &ctx);
        assert_eq!(reopened.generation.queue[0].state, State::Incomplete);
        assert_eq!(reopened.generation.queue[0].detail, issue);
        assert_eq!(reopened.visible_generated_tab(), Some(id));
        assert!(reopened.generated_image_ready(id));
        assert!(reopened.focused_map_archive().is_none());
        assert!(reopened.task.is_none());
        assert!(reopened.placement_task.is_none());
    }
}
#[test]
fn saving_after_a_working_map_edit_keeps_the_accepted_placement_and_recipe() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edited.mapper");
    let mut app = generated(&ctx);
    app.show_tab(Tab::Home);
    app.apply_tab_request();
    app.project.targets[0].x += 20.;
    app.start_placement(&ctx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while app.placement_task.is_some() && std::time::Instant::now() < deadline {
        app.poll_placement(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(app.current_layout().is_some(), "{}", app.status);
    let expected = map_monuments::initial(&app.project, &app.current_layout().unwrap().solution);
    let recipe = app.project.clone();
    app.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    assert!(!app.dirty(), "{}", app.status);
    let mut reopened = crate::tests::fixture(&ctx);
    reopened.open_project_from(path, &ctx);
    wait_io(&mut reopened, &ctx);
    assert_eq!(reopened.project, recipe);
    assert_eq!(reopened.generation.results.len(), 2, "{}", reopened.status);
    let result = reopened.generation.results.last().unwrap();
    assert_eq!(result.label,format!("Seed {} · {}",recipe.seeds[0].value,result.id));
    assert_eq!(result.recipe, recipe);
    assert_eq!(reopened.visible_generated_tab(), Some(result.id));
    assert_eq!(
        serde_json::to_value(&result.monuments).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert!(reopened.preview.is_none());
    assert!(reopened.task.is_none());
}

#[test]
#[ignore = "owned native complete-world generation and working-map snapshot"]
fn owned_working_snapshot_exports_the_current_complete_world() {
    use mapper_studio::{headless, prefab_assets};
    let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
    let project = Project::load(&path("MAPPER_PROJECT_PERSISTENCE_PROJECT")).unwrap();
    let output = path("MAPPER_PROJECT_PERSISTENCE_LIVE_OUTPUT");
    let options = headless::Options {
        rust_root: Some(path("MAPPER_PROJECT_PERSISTENCE_RUST")),
        worker: Some(path("MAPPER_PROJECT_PERSISTENCE_WORKER")),
        catalog: Some(path("MAPPER_PROJECT_PERSISTENCE_CATALOG")),
        bindings: Some(path("MAPPER_PROJECT_PERSISTENCE_BINDINGS")),
        skip_composition: false,
    };
    let cancel = AtomicBool::new(false);
    let mut generated = headless::generate(&project, &options, &cancel, |label, fraction| {
        println!("{:.0}% {label}", fraction * 100.)
    })
    .unwrap()
    .unwrap();
    let recipe = generated.recipe.clone();
    let palette = generated.native.as_ref().unwrap().source.palette.clone();
    let maps = Arc::new(generated.composed.take().unwrap());
    let prepared_surface = mapper_studio::map_image::prepare_composed_surface(
        &maps,
        mapper_studio::map_image::DISPLAY_RESOLUTION,
        palette.as_deref(),
        || false,
    )
    .unwrap();
    let expects_surface = palette.as_ref().is_some_and(|palette| {
        palette.has_surface_profile() && palette.covers_layers(&maps.layers)
    });
    assert_eq!(prepared_surface.is_some(), expects_surface);
    let surface_key = prepared_surface.as_ref().map(Arc::as_ptr);
    let mut uncached_seconds = Vec::new();
    let mut reused_seconds = Vec::new();
    let mut expected_overview = None;
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let original = mapper_studio::map_image::render_composed(
            &maps,
            mapper_studio::map_image::DISPLAY_RESOLUTION,
            palette.as_deref(),
            || false,
        )
        .unwrap()
        .unwrap();
        uncached_seconds.push(started.elapsed().as_secs_f64());
        let started = std::time::Instant::now();
        let reused = mapper_studio::map_image::render_composed_owned(
            &maps,
            mapper_studio::map_image::DISPLAY_RESOLUTION,
            palette.as_deref(),
            || false,
        )
        .unwrap()
        .unwrap();
        reused_seconds.push(started.elapsed().as_secs_f64());
        assert_eq!(original.pixels, reused.image.pixels);
        assert_eq!(reused.surface.as_ref().map(Arc::as_ptr), surface_key);
        expected_overview = Some(original);
    }
    println!(
        "Overview seconds, direct {uncached_seconds:?}, retained {reused_seconds:?}; surface cache: {}",
        prepared_surface.is_some()
    );
    let mut monuments = map_monuments::initial(&recipe, &generated.solution);
    monuments.extend(map_monuments::generated(&maps).unwrap());
    let count = monuments.len();
    let root = options.rust_root.as_ref().unwrap();
    let catalog = Arc::new(
        prefab_assets::load(
            root,
            options.catalog.as_ref().unwrap(),
            recipe.installation.as_ref().unwrap(),
            || false,
        )
        .unwrap(),
    );
    let candidate = CapturedCandidate {
        saved: SavedCandidate {
            id: 1,
            label: "Working map".into(),
            recipe: recipe.clone(),
            zoom: 1.,
            pan: [0.; 2],
            missing: generated.solution.required_missing,
            monuments,
            routes: Vec::new(),
            grid: false,
            no_build: false,
            background: [24, 66, 95, 255],
            overview: 0,
            map: None,
            render: None,
            archive_issue: String::new(),
            image_issue: String::new(),
            palette: None,
            restrictions: None,
            font: None,
            font_ref: None,
        },
        overview: None,
        archive: None,
        palette: palette.clone(),
        restrictions: Default::default(),
        font: None,
        working: Some(WorkingCapture {
            native: Some(WorkingWorld {
                maps: maps.clone(),
                solution: generated.solution.clone(),
                root: root.clone(),
                catalog,
            }),
            image: Arc::new(generated.image),
            repaint_overview: false,
        }),
    };
    let capture = Capture {
        workspace: Workspace {
            schema: 1,
            results: Vec::new(),
            edits: Vec::new(),
            recipes: None,
            queue: Vec::new(),
            count: 4,
            randomize: [false; 7],
            open: vec![1],
            active: Some(1),
            fonts: Vec::new(),
        },
        candidates: vec![candidate],
    };
    let expected =
        mapper_studio::map_image::render_composed_owned(&maps, 64, palette.as_deref(), || false)
            .unwrap()
            .unwrap()
            .image;
    println!("Saving accepted working world");
    let started = std::time::Instant::now();
    let (workspace, sources) = capture.encode(&|| false).unwrap();
    let overview_index = workspace.results[0].overview;
    let Source::File(overview) = &sources[overview_index] else {
        panic!("Saved overview must be disk-backed");
    };
    let saved_overview = read_overview(overview.as_ref(), &|| false).unwrap().unwrap();
    assert_eq!(expected_overview.unwrap().pixels, saved_overview.pixels);
    assert!(
        mapper_studio::project_bundle::write(&output, recipe.clone(), workspace, &sources, || {
            false
        })
        .unwrap()
    );
    let save_seconds = started.elapsed().as_secs_f64();
    let after_save = mapper_studio::map_image::prepare_composed_surface(
        &maps,
        mapper_studio::map_image::DISPLAY_RESOLUTION,
        palette.as_deref(),
        || false,
    )
    .unwrap();
    assert_eq!(after_save.as_ref().map(Arc::as_ptr), surface_key);
    let started = std::time::Instant::now();
    let ctx = egui::Context::default();
    let mut reopened = crate::tests::fixture(&ctx);
    reopened.open_project_from(output.clone(), &ctx);
    wait_io(&mut reopened, &ctx);
    assert_eq!(reopened.generation.results.len(), 1, "{}", reopened.status);
    let result = &reopened.generation.results[0];
    assert_eq!(result.monuments.len(), count);
    assert_eq!(result.recipe, recipe);
    let archive = result
        .archive
        .as_ref()
        .expect("Working map lost its accepted game container");
    let retained=archive
            .image
            .as_ref()
            .unwrap()
            .render(64, result.palette.as_deref(), || false)
            .unwrap()
            .unwrap();
    let changed=expected.pixels.iter().zip(&retained.pixels).filter(|(a,b)|a!=b).count();
    mapper_studio::image_export::write_png(&output.with_extension("expected.png"),&expected,||false).unwrap();
    mapper_studio::image_export::write_png(&output.with_extension("retained.png"),&retained,||false).unwrap();
    assert_eq!(changed,0,"Saved working-map render changed pixels");
    assert_eq!(expected.source_size,retained.source_size);
    std::fs::copy(archive.file.as_ref(), output.with_extension("export.map")).unwrap();
    println!(
        "{} markers; {:.3}s save, {:.3}s reopen; {} project bytes; map and retained render exported",
        count,
        save_seconds,
        started.elapsed().as_secs_f64(),
        std::fs::metadata(&output).unwrap().len()
    );
}
fn wait_io(app: &mut Studio, ctx: &egui::Context) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while app.project_io.busy() && std::time::Instant::now() < deadline {
        app.poll_project_io(ctx);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!app.project_io.busy(), "{}", app.status);
}

#[test]
fn generated_overview_restarts_when_the_accepted_pixels_change() {
    let ctx = egui::Context::default();
    let mut app = crate::tests::fixture(&ctx);
    let retain = |app: &mut Studio| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(file) = app.retain_generated_overview(1, &ctx).unwrap() {
                return file;
            }
            assert!(std::time::Instant::now() < deadline, "Overview worker did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    };
    let original = retain(&mut app);
    let display = |color| Some(Arc::new(egui::ColorImage::filled([8, 8], color)));
    app.preview.as_mut().unwrap().display_image = display(egui::Color32::BLUE);
    assert!(app.retain_generated_overview(1, &ctx).unwrap().is_none());
    app.preview.as_mut().unwrap().display_image = display(egui::Color32::RED);

    let accepted = retain(&mut app);
    assert!(!Arc::ptr_eq(&original, &accepted));
    assert!(Arc::ptr_eq(&accepted, &retain(&mut app)));
    let decoded = read_overview(&accepted, &|| false).unwrap().unwrap();
    assert_eq!(decoded.pixels, vec![egui::Color32::RED; 64]);
}

fn generated(ctx: &egui::Context) -> Studio {
    let mut app = crate::tests::fixture(ctx);
    app.project.size = 1000;
    // This fixture exercises portable previews with optional monument placement.
    for target in &mut app.project.targets {
        target.presence = Presence::Allowed;
    }
    mapper_studio::generation_defaults::apply(&mut app.project);
    app.saved = app.project.clone();
    app.begin_generation(BatchPlan::exact(app.project.clone()), ctx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while app.generation.busy() && std::time::Instant::now() < deadline {
        app.poll(ctx);
        app.poll_placement(ctx);
        app.tick_generation(ctx);
        app.apply_tab_request();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!app.generation.busy(), "{}", app.status);
    assert_eq!(app.generation.results.len(), 1, "{}", app.status);
    app
}
fn archive() -> Arc<map_archive::Archive> {
    use papyrus_terrain::{map_container, monument_composition, native_layers};
    let (hr, sr) = native_layers::dimensions(1000).unwrap();
    let mut layers = native_layers::Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 2,
        height_resolution: hr,
        surface_resolution: sr,
        height: vec![18000; hr * hr],
        biome: vec![0; sr * sr * 5],
        splat: vec![0; sr * sr * 8],
        topology: vec![0; sr * sr],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    layers.biome[..sr * sr].fill(255);
    layers.splat[..sr * sr].fill(255);
    let alpha = vec![255; sr * sr];
    let maps = monument_composition::compose(&layers, &alpha, &[], || false)
        .unwrap()
        .unwrap();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    map_container::write_v10(
        map_container::World {
            layers: &layers,
            height: &layers.height,
            water: Some(&vec![16383; hr * hr]),
            alpha: &alpha,
            prefabs: &[],
            paths: &[],
        },
        123,
        file.as_file_mut(),
        || false,
    )
    .unwrap()
    .unwrap();
    map_archive::Archive::new(
        Arc::new(file.into_temp_path()),
        mapper_studio::image_snapshot::Snapshot::capture(&maps, || false).unwrap(),
        String::new(),
    ).unwrap()
}
#[test]
fn project_save_rejects_a_changed_accepted_map_and_preserves_the_previous_file() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("world.mapper");
    let mut app = generated(&ctx);
    let accepted = archive();
    app.generation.results[0].archive = Some(accepted.clone());
    app.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    let original = std::fs::read(&path).unwrap();
    assert!(!app.dirty());

    std::fs::OpenOptions::new().write(true).open(accepted.file.as_ref()).unwrap()
        .set_len(12).unwrap();
    app.project.master_seed += 1;
    app.project.refresh_inherited_seeds();
    app.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);

    assert!(app.project_io.issue().is_some_and(|issue| issue.contains("retained map changed")));
    assert!(app.dirty());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(directory.path().read_dir().unwrap().count(), 1);
}

#[test]
fn generated_maps_save_and_reopen_through_normal_project_operations_without_generation() {
    use mapper_studio::path_edit::{Change, Key, Kind, Operation, Route};
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("world.mapper");
    let mut app = generated(&ctx);
    assert_eq!(app.project, app.saved);
    assert!(app.dirty(), "New maps must mark the project unsaved");
    drop(super::super::tests::require_matching_rust_installation(&mut app));
    app.saved = app.project.clone();
    app.generation.queue[0].recipe = app.project.clone();
    let result = &mut app.generation.results[0];
    result.recipe = app.project.clone();
    result.archive = Some(archive());
    result.grid = true;
    result.no_build = true;
    result.pan = Vec2::new(17., -4.);
    result.zoom = 1.75;
    let route = Key { kind: Kind::Road, index: 0 };
    result.routes.push(Route {
        key: route,
        controls: vec![[-20., 0.], [0., 0.], [20., 0.]],
        fixed: vec![0, 2],
    });
    let draft_path = Change { route, control: 1, expected: [0., 0.], operation: Operation::Move([0., 3.]) };
    // Generated families can use their own occurrence-ID namespaces.
    let mut roadside=result.monuments[0].clone();roadside.id=0;
    result.monuments.push(roadside);
    result.monuments.push(result.monuments[0].clone());
    let expected_monuments=serde_json::to_value(&result.monuments).unwrap();
    let id = result.id;
    let recipe = result.recipe.clone();
    let original = result.archive.as_ref().unwrap().clone();
    let index = result.monuments.iter().position(|monument| movable_monument(&recipe, monument)).unwrap();
    let [x, _, z] = result.monuments[index].position;
    let pose = [x * 0.9, z * 0.9, 45.];
    let expected_map = std::fs::read(original.file.as_ref()).unwrap();
    let expected_overview = std::fs::read(result.overview.as_ref().unwrap().as_ref()).unwrap();
    let expected_render = original
        .image
        .as_ref()
        .unwrap()
        .render(64, None, || false)
        .unwrap()
        .unwrap();
    app.generation.edits.entry(id).or_default();
    let edit = app.generation.edits.get_mut(&id).unwrap();
    let template = super::super::MonumentTemplate {
        map: id, name: "New landmark".into(), prefab: "assets/test/landmark.prefab".into(),
    };
    assert!(edit.place_template(result, &template, [10., 20.], None).unwrap());
    edit.paths.record(&result.routes, draft_path, recipe.size).unwrap();
    let failed_id = id + 1;
    let failure = QueueItem { origin: None,
        id: failed_id, label: "Rebuild".into(), recipe: recipe.clone(),
        state: State::Failed, detail: "Road fitting failed".into(),
    };
    edit.last_issue = EditIssue::from_item(&failure);
    app.generation.queue.push(failure);
    // Save while a second drag is in progress. Reopening should restore the settled move.
    edit.move_limit_m = Some(75);
    edit.poses.insert(index, [pose[0], pose[1], 90.]);
    edit.drag = Some(super::super::MonumentDrag {
        index,
        press: Pos2::ZERO,
        origin: pose,
        previous: Some(pose),
    });
    app.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    assert!(!app.dirty(), "{}", app.status);
    assert_eq!(Project::load(&path).unwrap(), recipe);
    drop(app);
    drop(original);
    let mut reopened = crate::tests::fixture(&ctx);
    reopened.open_project_from(path.clone(), &ctx);
    wait_io(&mut reopened, &ctx);
    assert_eq!(reopened.generation.results.len(), 1, "{}", reopened.status);
    assert_eq!(reopened.visible_generated_tab(), Some(id));
    assert!(reopened.preferences.rust_folder.is_none());
    assert!(!reopened.installation_setup_open);
    assert!(reopened.source.bound_root_ref(&recipe).is_none());
    assert!(!reopened.dirty());
    assert!(reopened.task.is_none());
    assert!(reopened.placement_task.is_none());
    assert!(reopened.preview.is_none());
    let result = &reopened.generation.results[0];
    assert!(matches!(result.image, Some(Ok(_))));
    assert_eq!(result.recipe, recipe);
    assert_eq!(serde_json::to_value(&result.monuments).unwrap(),expected_monuments);
    assert!(result.grid && result.no_build);
    assert_eq!(result.pan, Vec2::new(17., -4.));
    assert_eq!(result.zoom, 1.75);
    assert_eq!(
        std::fs::read(result.overview.as_ref().unwrap().as_ref()).unwrap(),
        expected_overview
    );
    assert_eq!(reopened.generation.queue[0].state, State::Ready);
    let retained = reopened.focused_map_archive().unwrap().0;
    let edit = &reopened.generation.edits[&id];
    assert_eq!(edit.poses, [(index, pose)].into());
    assert_eq!(edit.move_limit_m, Some(75));
    assert_eq!(edit.last_issue.as_ref().unwrap().id, failed_id);
    assert_eq!(result.routes[0].controls, [[-20., 0.], [0., 0.], [20., 0.]]);
    assert_eq!(edit.paths.changes(), [draft_path]);
    assert_eq!(edit.monuments.additions.len(), 1);
    assert_eq!(edit.monuments.additions[0].position, [10., 20.]);
    assert!(edit.drag.is_none() && edit.job.is_none());
    assert!(!reopened.generation.busy());
    assert!(!reopened.settings_open);
    reopened.apply_map_edit(id, &ctx);
    reopened.apply_tab_request();
    assert!(reopened.settings_open);
    assert!(matches!(reopened.settings_page, crate::settings::Page::Installations));
    assert_eq!(reopened.visible_generated_tab(), Some(id));
    assert_eq!(reopened.project, recipe);
    assert!(!reopened.dirty());
    assert!(!reopened.generation.busy());
    assert!(Arc::ptr_eq(&retained, &reopened.focused_map_archive().unwrap().0));
    reopened.generation.discard_queue_prefix(reopened.generation.history_len());
    reopened.generation.revision += 1;
    reopened.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut reopened, &ctx);
    let detached = Generation::restore(&path, &ctx, &|| false).unwrap().unwrap().generation;
    assert!(detached.queue.is_empty());
    assert!(detached.next_id > failed_id);
    let issue = detached.edits[&id].last_issue.as_ref().unwrap();
    assert_eq!(issue.id, failed_id);
    assert_eq!(issue.detail, "Road fitting failed");

    reopened.map_edit_action(id, Some(MapEditAction::Discard), &ctx);
    assert!(reopened.dirty());
    reopened.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut reopened, &ctx);
    assert!(!reopened.dirty());
    let mut without_draft = crate::tests::fixture(&ctx);
    without_draft.open_project_from(path.clone(), &ctx);
    wait_io(&mut without_draft, &ctx);
    assert!(without_draft.generation.edits.is_empty());
    assert_eq!(without_draft.visible_generated_tab(), Some(id));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(std::fs::read(retained.file.as_ref()).unwrap(), expected_map);
    let decoded = papyrus_terrain::content::read_map(&expected_map, || false).unwrap();
    assert_eq!(decoded.size, 1000);
    assert_eq!(
        retained
            .image
            .as_ref()
            .unwrap()
            .render(64, None, || false)
            .unwrap()
            .unwrap(),
        expected_render
    );
}

#[test]
fn duplicate_workspace_payloads_share_assets_and_still_validate_each_render_size() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("shared.mapper");
    let incorrect = directory.path().join("wrong-size.mapper");
    let ctx = egui::Context::default();
    let recipe = Project { size: 1000, ..Project::default() };
    let image = egui::ColorImage::filled([8, 8], Color32::from_rgb(24, 66, 95));
    let overview = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    assert!(mapper_studio::image_export::write_png(&overview, &image, || false).unwrap());
    let archive = archive();
    let mut sources = vec![
        Source::File(Arc::new(overview)),
        Source::Retained(archive.file.clone()),
        Source::File(archive.image.as_ref().unwrap().retained_file()),
    ];
    sources.extend_from_within(..);
    let results = (0..2).map(|index| SavedCandidate {
        id: index as u64 + 1,
        label: format!("Shared map {}", index + 1),
        recipe: recipe.clone(),
        zoom: 1.,
        pan: [0., 0.],
        missing: 0,
        monuments: Vec::new(),
        routes: Vec::new(),
        grid: index == 1,
        no_build: false,
        background: [24, 66, 95, 255],
        overview: index * 3,
        map: Some(index * 3 + 1),
        render: Some(index * 3 + 2),
        archive_issue: String::new(),
        image_issue: String::new(),
        palette: None,
        restrictions: None,
        font: None,
        font_ref: None,
    }).collect();
    let mut workspace = Workspace {
        schema: 2,
        results,
        edits: Vec::new(),
        recipes: None,
        queue: Vec::new(),
        count: 2,
        randomize: [false; 7],
        open: vec![1, 2],
        active: Some(2),
        fonts: Vec::new(),
    };
    assert!(mapper_studio::project_bundle::write(
        &output, recipe.clone(), serde_json::to_value(&workspace).unwrap(), &sources, || false,
    ).unwrap());
    let mut saved = Generation::restore(&output, &ctx, &|| false).unwrap().unwrap().generation;
    assert!(saved.results[0].image.is_none());
    let texture = saved.results[1].image.as_ref().unwrap().as_ref().unwrap().id();
    saved.prepare_preview(1, &ctx);
    assert!(saved.preview_load.is_none());
    let [first, second] = saved.results.as_slice() else { panic!("Expected two saved maps") };
    assert_eq!(first.image.as_ref().unwrap().as_ref().unwrap().id(), texture);
    assert!(!first.grid && second.grid);
    assert!(Arc::ptr_eq(first.overview.as_ref().unwrap(), second.overview.as_ref().unwrap()));
    let first = first.archive.as_ref().unwrap();
    let second = second.archive.as_ref().unwrap();
    assert!(first.file.same_allocation(&second.file));
    assert!(Arc::ptr_eq(first.image.as_ref().unwrap(), second.image.as_ref().unwrap()));

    workspace.active = None;
    assert!(mapper_studio::project_bundle::write(
        &output, recipe.clone(), serde_json::to_value(&workspace).unwrap(), &sources, || false,
    ).unwrap());
    let restored = Generation::restore(&output, &ctx, &|| false).unwrap().unwrap();
    assert_eq!(restored.active, Some(2));
    assert!(restored.generation.results[0].image.is_none());
    assert!(matches!(restored.generation.results[1].image, Some(Ok(_))));
    workspace.active = Some(2);

    workspace.results[1].recipe.size = 1500;
    assert!(mapper_studio::project_bundle::write(
        &incorrect, recipe, serde_json::to_value(&workspace).unwrap(), &sources, || false,
    ).unwrap());
    assert_eq!(
        Generation::restore(&incorrect, &ctx, &|| false).err().as_deref(),
        Some("Saved map render dimensions differ from its recipe"),
    );

    // Give two inactive maps separate overviews with valid bundle receipts.
    // A damaged PNG must prevent publication even when its tab is hidden.
    workspace.results[1].recipe.size = 1000;
    let recipe = workspace.results[1].recipe.clone();
    let next = sources.len();
    let other = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let image = egui::ColorImage::filled([9, 9], Color32::BLACK);
    assert!(mapper_studio::image_export::write_png(&other, &image, || false).unwrap());
    sources.push(Source::File(Arc::new(other)));
    let damaged = Arc::from(b"damaged map overview".as_slice());
    sources.push(Source::Bytes(damaged));
    workspace.results[0].overview = next;
    let mut saved = serde_json::to_value(&workspace).unwrap();
    let mut hidden = saved["results"][0].clone();
    hidden["id"] = 3.into();
    hidden["overview"] = (next + 1).into();
    saved["results"].as_array_mut().unwrap().push(hidden);
    let output = directory.path().join("damaged-overview.mapper");
    assert!(mapper_studio::project_bundle::write(&output, recipe, saved, &sources, || false).unwrap());
    assert_eq!(Generation::restore(&output, &ctx, &|| false).err().as_deref(), Some("Invalid PNG signature."));
}

#[test]
fn workspace_font_table_round_trips_and_validates_references() {
    use mapper_studio::installation::{Installation, InstallationKind, SourceFile};
    use serde_json::json;

    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fonts.mapper");
    let installation = Installation {
        kind: InstallationKind::Client,
        native: SourceFile {
            relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
            sha256: "a".repeat(64),
            bytes: 1,
        },
        scene_manifest: None,
    };
    let transform = json!({
        "m_SizeDelta": {"x":16,"y":16}, "m_LocalScale": {"x":1,"y":1},
        "m_LocalRotation": {"x":0,"y":0,"z":0,"w":1},
        "m_AnchorMin": {"x":0.5,"y":0.5}, "m_AnchorMax": {"x":0.5,"y":0.5},
        "m_Pivot": {"x":0.5,"y":0.5}, "m_AnchoredPosition": {"x":0,"y":0}
    });
    let metadata = json!({
        "font": {
            "m_Name": "Test Marker", "m_AtlasTextures": [0], "m_AtlasWidth": 16,
            "m_AtlasHeight": 16, "m_AtlasPadding": 0,
            "m_FaceInfo": {"m_PointSize": 16, "m_Scale": 1, "m_AscentLine": 12, "m_LineHeight": 16},
            "m_GlyphTable": [{
                "m_Index": 0, "m_AtlasIndex": 0, "m_Scale": 1,
                "m_GlyphRect": {"m_X": 0, "m_Y": 0, "m_Width": 8, "m_Height": 8},
                "m_Metrics": {"m_Width": 8, "m_Height": 8, "m_HorizontalBearingX": 0,
                              "m_HorizontalBearingY": 8, "m_HorizontalAdvance": 8}
            }],
            "m_CharacterTable": [{"m_Unicode": 65, "m_GlyphIndex": 0, "m_Scale": 1}],
            "m_FontFeatureTable": {"m_GlyphPairAdjustmentRecords": []}
        },
        "atlas": {"m_Width": 16, "m_Height": 16, "m_TextureFormat": 1, "m_MipCount": 1},
        "text": {
            "m_fontStyle": 0, "m_fontWeight": 400, "m_enableAutoSizing": 0, "m_fontSize": 16,
            "m_HorizontalAlignment": 1, "m_VerticalAlignment": 256, "m_enableKerning": 0,
            "m_characterHorizontalScale": 1, "m_lineSpacing": 0, "m_characterSpacing": 0, "m_wordSpacing": 0
        },
        "material": {"m_SavedProperties": {"m_Floats": [
            {"key": "_FaceDilate", "value": 0}, {"key": "_OutlineWidth", "value": 0},
            {"key": "_WeightNormal", "value": 0}, {"key": "_Sharpness", "value": 0},
            {"key": "_GradientScale", "value": 8}
        ]}},
        "root_transform": transform, "text_transform": transform,
        "marker": {"dayColor": {"r":0,"g":0,"b":0,"a":1}}
    });
    let style = mapper_studio::map_labels::Style::decode(&metadata).unwrap();
    let alpha: Arc<[u8]> = vec![255; 256].into();
    let font = Arc::new(mapper_studio::map_labels::Prepared {
        metadata: Arc::new(metadata),
        installation: installation.clone(),
        sha256: "test-font".into(),
        image: Arc::new(style.image(&alpha).unwrap()),
        image_sha256: String::new(),
        style,
        alpha,
        grid: None,
    });
    let image = egui::ColorImage::filled([8, 8], Color32::BLACK);
    let overview = Arc::new(mapper_studio::image_export::retain_overview(&image, || false).unwrap().unwrap());
    let recipe = Project { size: 1000, installation: Some(installation), ..Project::default() };
    let generation = Generation {
        results: (1..=2).map(|id| Candidate {
            id, label: format!("Map {id}"), recipe: recipe.clone(), identity: None,
            overview: Some(overview.clone()), archive: None, archive_issue: String::new(),
            image: None, zoom: 1., pan: Vec2::ZERO, placed: 0, missing: 0,
            monuments: Vec::new(), routes: Vec::new(), restrictions: Default::default(),
            grid: false, no_build: false, map_font: Some(font.clone()), background: MAP_BACKGROUND,
            palette: None,
        }).collect(),
        ..Generation::default()
    };
    let (workspace, sources) = generation.capture(vec![1, 2], Some(2)).unwrap().encode(&|| false).unwrap();
    assert_eq!(workspace.schema, SHARED_FONT_SCHEMA);
    assert_eq!(workspace.fonts.len(), 1);
    assert!(workspace.results.iter().all(|map| map.font_ref == Some(0) && map.font.is_none()));
    let current = serde_json::to_value(workspace).unwrap();
    let mut legacy = current.clone();
    let record = legacy.as_object_mut().unwrap().remove("fonts").unwrap().as_array().unwrap()[0].clone();
    legacy["schema"] = 5.into();
    for map in legacy["results"].as_array_mut().unwrap() {
        map.as_object_mut().unwrap().remove("font_ref");
        map["font"] = record.clone();
    }
    assert!(serde_json::to_vec(&current).unwrap().len() < serde_json::to_vec(&legacy).unwrap().len());
    for manifest in [&current, &legacy] {
        assert!(mapper_studio::project_bundle::write(&path, recipe.clone(), manifest, &sources, || false).unwrap());
        let saved = Generation::restore(&path, &ctx, &|| false).unwrap().unwrap();
        let a = saved.generation.results[0].map_font.as_ref().unwrap();
        let b = saved.generation.results[1].map_font.as_ref().unwrap();
        assert!(Arc::ptr_eq(a, b));
        assert_eq!(a.image, font.image);
        assert_eq!(saved.active, Some(2));
    }

    let mut wrong = current.clone();
    wrong["results"][1]["font_ref"] = 1.into();
    assert!(mapper_studio::project_bundle::write(&path, recipe.clone(), &wrong, &sources, || false).unwrap());
    assert_eq!(Generation::restore(&path, &ctx, &|| false).err().as_deref(), Some("Saved map references a missing font"));
    let mut wrong = current;
    wrong["results"][1]["recipe"]["installation"]["native"]["sha256"] = "b".repeat(64).into();
    assert!(mapper_studio::project_bundle::write(&path, recipe, &wrong, &sources, || false).unwrap());
    assert_eq!(
        Generation::restore(&path, &ctx, &|| false).err().as_deref(),
        Some("Saved maps share a font across different Rust installations")
    );
}

#[test]
fn background_overview_scan_checks_pixels_and_can_stop_mid_image() {
    let good = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let mut image = egui::ColorImage::filled([32, 64], Color32::from_rgb(17, 32, 64));
    image.pixels[33] = Color32::TRANSPARENT;
    image.pixels[1050] = Color32::from_rgba_unmultiplied(241, 201, 49, 127);
    assert!(mapper_studio::image_export::write_png(&good, &image, || false).unwrap());
    assert!(validate_overview(&good, &|| false).unwrap());
    let file = std::io::BufReader::new(std::fs::File::open(&good).unwrap());
    let mut decoder = png::Decoder::new(file).read_info().unwrap();
    let mut rgba = vec![0; 32 * 64 * 4];
    decoder.next_frame(&mut rgba).unwrap();
    let expected = egui::ColorImage::from_rgba_unmultiplied([32, 64], &rgba);
    assert_eq!(read_overview(&good, &|| false).unwrap().unwrap(), expected);
    let inactive = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let other = egui::ColorImage::filled([8, 8], Color32::RED);
    assert!(mapper_studio::image_export::write_png(&inactive, &other, || false).unwrap());
    let inactive = [Arc::new(inactive)];
    assert_eq!(
        load_overviews(&inactive, Some(&good), &|| false).unwrap(),
        Some(Some(expected)),
    );
    assert_eq!(load_overviews(&inactive, None, &|| false).unwrap(), Some(None));
    assert!(load_overviews(&inactive, Some(&good), &|| true).unwrap().is_none());

    let rows = std::cell::Cell::new(0);
    assert!(!validate_overview(&good, &|| {
        rows.set(rows.get() + 1);
        rows.get() == 16
    }).unwrap());
    assert_eq!(rows.get(), 16);
    rows.set(0);
    assert!(read_overview(&good, &|| {
        rows.set(rows.get() + 1);
        rows.get() == 16
    }).unwrap().is_none());
    assert_eq!(rows.get(), 16);

    let mut bytes = std::fs::read(&good).unwrap();
    let data_chunk = bytes.windows(4).position(|chunk| chunk == b"IDAT").unwrap();
    let length = u32::from_be_bytes(bytes[data_chunk - 4..data_chunk].try_into().unwrap());
    bytes.truncate(data_chunk + 4 + length as usize / 2);
    let truncated = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    std::fs::write(&truncated, bytes).unwrap();
    assert!(read_overview(&truncated, &|| false).is_err());
    assert!(validate_overview(&truncated, &|| false).is_err());
}

#[test]
fn working_overview_reuses_matching_pixels_and_repaints_stale_colors() {
    let ctx = egui::Context::default();
    let mut app = crate::tests::fixture(&ctx);
    let path = app.project.targets[0].prefab_path.clone().unwrap();
    let palette = app.prefabs.seed_test_cartographic_stamp(&ctx, &mut app.project, &path, Some(0));
    let mut colors = mapper_studio::map_colors::Preferences::default();
    colors.overrides[0] = Some([255, 0, 255]);
    let (base, repaint) = overview_style(&palette, &colors, true);
    assert!(!repaint);
    assert_eq!(base.cartography(), palette.cartography());

    colors.enabled = true;
    let (styled, repaint) = overview_style(&palette, &colors, true);
    assert!(repaint);
    assert_ne!(styled.cartography(), palette.cartography());
    assert!(!overview_style(&palette, &colors, false).1);
    assert!(!overview_style(&styled, &colors, true).1);
}

#[test]
fn older_map_previews_release_their_texture_and_reload_from_disk() {
    let ctx = egui::Context::default();
    let mut generation = Generation::default();
    let recipe = Project::default();
    let count = RESIDENT_PREVIEWS as u64 + 1;
    let mut original_textures = Vec::new();
    for id in 1..=count {
        let image = egui::ColorImage::filled([8, 8], Color32::from_rgb(id as u8, 16, 32));
        let path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        assert!(mapper_studio::image_export::write_png(&path, &image, || false).unwrap());
        let texture = ctx.load_texture(format!("preview-test-{id}"), image, Default::default());
        original_textures.push(texture.id());
        generation.results.push(Candidate {
            identity: None,
            overview: Some(Arc::new(path)),
            archive: None,
            archive_issue: String::new(),
            id,
            label: id.to_string(),
            recipe: recipe.clone(),
            image: Some(Ok(texture)),
            zoom: 1.,
            pan: Vec2::ZERO,
            placed: 0,
            missing: 0,
            monuments: Vec::new(),
            routes: Vec::new(),
            restrictions: Default::default(),
            grid: false,
            no_build: false,
            map_font: None,
            background: MAP_BACKGROUND,
            palette: None,
        });
        generation.remember_preview(id);
    }
    let mut split = DockState::new(vec![Tab::Generated(1)]);
    for id in 2..=count {
        let active = split.find_tab(&Tab::Generated(id - 1)).unwrap();
        split.main_surface_mut().split_right(active.node, 0.5, vec![Tab::Generated(id)]);
    }
    for _ in 0..3 {
        for id in 1..=count {
            generation.prepare_preview(id, &ctx);
        }
        generation.finish_preview_frame(&split);
        assert!(generation.preview_load.is_none());
        for (candidate, &texture) in generation.results.iter().zip(&original_textures) {
            assert_eq!(candidate.image.as_ref().unwrap().as_ref().unwrap().id(), texture);
        }
    }
    let single = DockState::new(vec![Tab::Generated(count)]);
    generation.finish_preview_frame(&single);
    assert!(generation.results[0].image.is_none());
    assert_eq!(generation.results.len(), count as usize);

    // Viewing the second tab makes the third tab the oldest resident.
    generation.prepare_preview(2, &ctx);
    generation.prepare_preview(1, &ctx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while generation.preview_load.is_some() && std::time::Instant::now() < deadline {
        // Another dock leaf can poll the worker first without dropping the result.
        generation.prepare_preview(2, &ctx);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let texture = generation.results[0].image.as_ref().unwrap().as_ref().unwrap();
    assert_ne!(texture.id(), original_textures[0]);
    assert_eq!(texture.size(), [8, 8]);
    let texture_id = texture.id();
    generation.finish_preview_frame(&single);
    assert!(generation.results[1].image.is_some());
    assert!(generation.results[2].image.is_none());
    assert_eq!(generation.results.iter().filter(|map| matches!(map.image, Some(Ok(_)))).count(),
        RESIDENT_PREVIEWS);

    let mut output = ctx.run_ui(Default::default(), |_| {});
    let delta = output.textures_delta.set.get(&texture_id).unwrap().first().unwrap();
    let egui::ImageData::Color(image) = &delta.image;
    assert!(image.pixels.iter().all(|&pixel| pixel == Color32::from_rgb(1, 16, 32)));
    output.textures_delta.clear();

    // Keep the sender alive to model a worker waiting on disk even after cancel.
    let (hidden_sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    generation.preview_load = Some(PreviewLoad {
        id: 3,
        overview: generation.results[2].overview.as_ref().unwrap().clone(),
        cancel: cancel.clone(),
        receiver,
    });
    let mut dock = DockState::new(vec![Tab::Generated(3), Tab::Generated(2)]);
    generation.finish_preview_frame(&dock);
    assert!(!cancel.load(Ordering::Relaxed));
    dock.set_active_tab(dock.find_tab(&Tab::Generated(2)).unwrap()).unwrap();
    generation.finish_preview_frame(&dock);
    assert!(cancel.load(Ordering::Relaxed));
    assert!(generation.preview_load.is_none());
    assert_eq!(generation.cancelled_preview.as_ref().unwrap().id, 3);

    generation.results[0].image = None;
    let (second_sender, receiver) = mpsc::channel();
    let second_cancel = Arc::new(AtomicBool::new(false));
    generation.preview_load = Some(PreviewLoad {
        id: 1,
        overview: generation.results[0].overview.as_ref().unwrap().clone(),
        cancel: second_cancel.clone(),
        receiver,
    });
    generation.finish_preview_frame(&dock);
    assert!(second_cancel.load(Ordering::Relaxed));
    generation.prepare_preview(1, &ctx);
    assert!(Arc::ptr_eq(&generation.preview_load.as_ref().unwrap().cancel, &second_cancel));
    drop(second_sender);

    generation.prepare_preview(1, &ctx);
    assert_eq!(generation.preview_load.as_ref().unwrap().id, 1);
    assert!(!generation.preview_load.as_ref().unwrap().cancel.load(Ordering::Relaxed));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while generation.preview_load.is_some() && std::time::Instant::now() < deadline {
        generation.prepare_preview(1, &ctx);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(generation.preview_load.is_none());
    assert!(matches!(generation.results[0].image, Some(Ok(_))));
    assert!(generation.cancelled_preview.is_some());
    hidden_sender.send(Err("Cancelled preview".into())).unwrap();
    generation.finish_preview_frame(&dock);
    assert!(generation.cancelled_preview.is_none());
    assert!(generation.results[2].image.is_none());

    // A completed decode belongs to the retained file, even when a rebuild
    // has reused the tab ID for a different overview.
    let obsolete = generation.results[2].overview.as_ref().unwrap().clone();
    let (sender, receiver) = mpsc::channel();
    generation.preview_load = Some(PreviewLoad {
        id: 3,
        overview: obsolete,
        cancel: Default::default(),
        receiver,
    });
    let shared = generation.results[0].overview.as_ref().unwrap().clone();
    generation.results[2].overview = Some(shared.clone());
    sender.send(Err("Obsolete overview".into())).unwrap();
    generation.prepare_preview(2, &ctx);
    assert!(generation.preview_load.is_none());
    assert!(generation.results[2].image.is_none());

    // A pending request can follow a different tab which shares the file.
    generation.results[0].image = None;
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    generation.preview_load = Some(PreviewLoad {
        id: 1,
        overview: shared,
        cancel: cancel.clone(),
        receiver,
    });
    let dock = DockState::new(vec![Tab::Generated(3)]);
    generation.finish_preview_frame(&dock);
    assert_eq!(generation.preview_load.as_ref().unwrap().id, 3);
    assert!(!cancel.load(Ordering::Relaxed));
    let image = egui::ColorImage::filled([8, 8], Color32::from_rgb(1, 16, 32));
    sender.send(Ok(Some(image))).unwrap();
    generation.prepare_preview(3, &ctx);
    let texture = generation.results[2].image.as_ref().unwrap().as_ref().unwrap().id();
    generation.prepare_preview(1, &ctx);
    assert!(generation.preview_load.is_none());
    assert_eq!(generation.results[0].image.as_ref().unwrap().as_ref().unwrap().id(), texture);
}
#[test]
fn save_completion_keeps_edits_made_after_the_snapshot_dirty() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("world.mapper");
    let mut app = crate::tests::fixture(&ctx);
    let original = app.project.clone();
    app.save_project_to(path.clone(), &ctx).unwrap();
    app.project.targets[0].x += 10.;
    wait_io(&mut app, &ctx);
    assert!(app.dirty());
    assert_eq!(app.saved, original);
    assert_eq!(Project::load(&path).unwrap(), original);
}
#[test]
fn edits_during_open_require_a_decision_and_save_before_switching() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let incoming = directory.path().join("incoming.mapper");
    let outgoing = directory.path().join("outgoing.mapper");
    let next = Project {
        size: 2000,
        ..Default::default()
    };
    next.save(&incoming).unwrap();
    let mut app = crate::tests::fixture(&ctx);
    app.open_project_from(incoming.clone(), &ctx);
    app.project.targets[0].x += 20.;
    let edited = app.project.clone();
    wait_io(&mut app, &ctx);
    assert_eq!(app.project, edited);
    assert!(matches!(app.pending, Some(Action::AcceptOpened)));
    app.save_project_to(outgoing.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    assert_eq!(app.project, next);
    assert_eq!(app.path.as_ref(), Some(&incoming));
    assert_eq!(Project::load(&outgoing).unwrap(), edited);
    assert!(!app.dirty());
}
fn project_with_running_map(ctx: &egui::Context) -> Studio {
    let mut app = crate::tests::fixture(ctx);
    app.generation.queue.push(QueueItem { origin: None,
        id: 1,
        label: "Running map".into(),
        recipe: app.project.clone(),
        state: State::Running,
        detail: String::new(),
    });
    app.generation.run = Some(Run {
        active_id: 1,
        template: None,
        start: 0,
        finished: 0,
        terrain_ready: false,
    });
    app.generation.next_id = 2;
    app.generation.revision += 1;
    app
}

#[test]
fn reopened_cancelled_job_requeues_the_exact_recipe() {
    mod snapshot { include!("../../../examples/support/egui_snapshot.rs"); }
    let ctx = egui::Context::default();
    workbench_style::install(&ctx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interrupted.mapper");
    let mut original = project_with_running_map(&ctx);
    let recipe = original.generation.queue[0].recipe.clone();
    original.save_project_to(path.clone(), &ctx).unwrap();
    wait_io(&mut original, &ctx);

    let mut app = crate::tests::fixture(&ctx);
    app.open_project_from(path, &ctx);
    wait_io(&mut app, &ctx);
    assert_eq!(app.generation.queue[0].state, State::Cancelled);
    app.project.master_seed += 1;
    app.project.refresh_inherited_seeds();
    let draft = app.project.clone();

    let mut textures = snapshot::Textures::default();
    let mut render = |app: &mut Studio, events| {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(250., 118.))),
            focused: true, events, ..Default::default()
        }, |ui| app.queue(ui));
        textures.update(&output.textures_delta);
        output.textures_delta.clear();
        output
    };
    render(&mut app, vec![]);
    let output = render(&mut app, vec![]);
    let point = output.shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.job.text == "Requeue" => {
            let center = text.pos + text.galley.size() * 0.5;
            assert!(shape.clip_rect.contains(center));
            Some(center)
        }
        _ => None,
    }).expect("Cancelled queue card should offer requeue");
    for pressed in [true, false] {
        render(&mut app, vec![egui::Event::PointerMoved(point), egui::Event::PointerButton {
            pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
        }]);
    }
    if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
        let image = textures.raster(&ctx, output, [250, 118]);
        mapper_studio::image_export::write_png(&PathBuf::from(dir).join("queue-requeue.png"), &image, || false).unwrap();
    }
    assert_eq!(app.generation.queue.len(), 1);
    assert_eq!(app.generation.queue[0].state, State::Running);
    assert_eq!(app.generation.queue[0].recipe, recipe);
    assert_eq!(app.generation.queue[0].label, "Running map");
    assert_eq!(app.document_recipe(), &draft);
    app.cancel_generation();
    assert_eq!(app.project, draft);
}

#[test]
fn project_open_keeps_generation_until_a_valid_replacement_is_accepted() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("bad.mapper");
    std::fs::write(&invalid, b"invalid project").unwrap();
    let incoming = directory.path().join("incoming.mapper");
    let next = Project { size: 2000, ..Default::default() };
    next.save(&incoming).unwrap();
    let mut app = project_with_running_map(&ctx);
    let original = app.project.clone();

    app.open_project_from(invalid, &ctx);
    wait_io(&mut app, &ctx);
    assert_eq!(app.project, original);
    assert!(app.status.contains("failed"));
    assert!(app.pending.is_none());
    assert!(app.path.is_none());
    assert_eq!(app.generation.queue[0].state, State::Running);
    assert_eq!(app.generation.active_recipe(), Some(&original));

    app.open_project_from(incoming.clone(), &ctx);
    app.cancel_project_io();
    wait_io(&mut app, &ctx);
    assert!(app.status.contains("cancelled"));
    assert!(app.pending.is_none());
    assert_eq!(app.generation.active_recipe(), Some(&original));

    app.open_project_from(incoming.clone(), &ctx);
    wait_io(&mut app, &ctx);
    assert_eq!(app.project, original);
    assert_eq!(app.generation.active_recipe(), Some(&original));
    assert!(matches!(app.pending, Some(Action::AcceptOpened)));
    let action = app.pending.take().unwrap();
    app.perform(action, &ctx);
    assert_eq!(app.project, next);
    assert_eq!(app.path.as_ref(), Some(&incoming));
    assert!(!app.generation.busy());
    assert!(!app.dirty());
}

#[test]
fn failed_save_and_continue_preserves_the_running_map() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let unavailable = directory.path().join("missing-folder").join("world.mapper");
    let mut app = project_with_running_map(&ctx);
    let original = app.project.clone();
    app.request(Action::New, &ctx);
    app.save_project_to(unavailable, &ctx).unwrap();
    wait_io(&mut app, &ctx);

    assert!(app.project_io.issue().is_some());
    assert!(app.path.is_none());
    assert!(app.dirty());
    assert!(matches!(app.pending, Some(Action::New)));
    assert_eq!(app.generation.active_recipe(), Some(&original));
    assert_eq!(app.generation.queue[0].state, State::Running);

    let output = directory.path().join("world.mapper");
    app.save_project_to(output.clone(), &ctx).unwrap();
    assert!(app.generation.busy());
    wait_io(&mut app, &ctx);

    assert!(app.pending.is_none());
    assert!(!app.generation.busy());
    assert!(!app.dirty());
    assert_eq!(Project::load(&output).unwrap(), original);
}

#[test]
fn a_queue_result_during_save_and_continue_requires_saving_again() {
    let ctx = egui::Context::default();
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("world.mapper");
    let mut app = project_with_running_map(&ctx);
    app.request(Action::New, &ctx);
    app.save_project_to(output.clone(), &ctx).unwrap();
    app.fail_generation("New map failed during save".into(), &ctx);
    wait_io(&mut app, &ctx);

    assert_eq!(app.path.as_ref(), Some(&output));
    assert!(app.dirty());
    assert!(matches!(app.pending, Some(Action::New)));
    assert_eq!(app.generation.queue[0].state, State::Failed);

    app.save_project_to(output.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    assert!(app.pending.is_none());
    let restored = Generation::restore(&output, &ctx, &|| false).unwrap().unwrap();
    assert_eq!(restored.generation.queue[0].state, State::Failed);
    assert_eq!(restored.generation.queue[0].detail, "New map failed during save");
}

#[test]
fn restriction_snapshots_keep_source_geometry_without_loading_a_rust_folder() {
    let directory = tempfile::tempdir().unwrap();
    let (mut catalog, _) = crate::prepared_assets::load(directory.path());
    let report:mapper_studio::no_build::Report=serde_json::from_value(serde_json::json!({
        "version":2,"space":"prefab-root-local-unity","layerIndex":8,"nodesVisited":1,
        "volumes":[{"objectId":1,"active":true,"geometry":{"shape":"box","center":[0,0,0],
        "halfAxes":[[10,0,0],[0,5,0],[0,0,8]],"flatOrthogonal":true}}],"issues":[],"complete":true})).unwrap();
    for asset in Arc::get_mut(&mut catalog).unwrap().entries.values_mut() {
        asset.no_build_geometry = Some(report.clone());
        asset.display_name = Some("Test monument".into());
    }
    let path = catalog.entries.keys().next().unwrap().clone();
    let project = Project {
        installation: Some(catalog.installation.clone()),
        ..Default::default()
    };
    let monuments = vec![map_monuments::Monument {
        id: 1,
        name: "Test monument".into(),
        prefab: Some(path),
        position: [0., 5., 0.],
        yaw: 37.,
        restriction_origin: None,
    }];
    let panel = restriction_panel::Panel::from_catalog(Some(catalog));
    let raw = serde_json::to_vec(&panel.snapshot(&monuments)).unwrap();
    let restored =
        restriction_panel::Panel::from_snapshot(serde_json::from_slice(&raw).unwrap(), &project)
            .unwrap();
    assert_eq!(panel.extent(&monuments), restored.extent(&monuments));
    assert_eq!(
        panel.export_bytes(&monuments),
        restored.export_bytes(&monuments)
    );
    assert_eq!(restored.incomplete_count(&project, &monuments), 0);
    assert_eq!(
        serde_json::to_vec(&restored.snapshot(&monuments)).unwrap(),
        raw
    );
}
#[test]
#[ignore = "requires the owned pre-R48 saved world and its matching Rust installation"]
fn owned_legacy_tunnel_restrictions_reopen_without_generation() {
    let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
    let root = path("MAPPER_LEGACY_TUNNEL_TEST_RUST");
    let project_path = path("MAPPER_LEGACY_TUNNEL_TEST_PROJECT");
    let output = path("MAPPER_LEGACY_TUNNEL_TEST_OUTPUT");
    let ctx = egui::Context::default();
    let mut app = crate::tests::fixture(&ctx);
    app.open_project_from(project_path, &ctx);
    wait_io(&mut app, &ctx);
    let id = app.visible_generated_tab().expect("saved result");
    assert_eq!(app.generated_image_warnings(id).0, 1);
    let source = app.project.installation.as_ref().unwrap();
    let (catalog, _, prepared) = mapper_studio::client_preparation::load_for_editor(
        &root, source, &app.project, || false,
    ).unwrap();
    assert!(prepared.steps.iter().all(|step| step.error.is_none()));
    assert!(prepared.restrictions.is_none());
    assert!(prepared.steps.iter().all(|step| step.name != "Preparing no-build zones"));
    assert!(prepared.tunnel_markers.is_some());
    app.restrictions.set_tunnel_markers(&app.project, prepared.tunnel_markers);
    let catalog = Arc::new(catalog);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !app.restrictions.tick(true, &app.project, Some(root.clone()), Some(catalog.clone()), &ctx) {
        assert!(app.restrictions.pending_for(&app.project), "{}", app.restrictions.message);
        assert!(std::time::Instant::now() < deadline, "No-build preparation timed out");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.generation.sync_restrictions(&app.restrictions);
    assert_eq!(app.generated_image_warnings(id).0, 0);
    let monuments = app.generation.result_monuments(id).unwrap();
    let tunnel = monuments.iter().find(|monument| {
        monument.prefab.as_deref().is_some_and(|path| path.contains("/tunnel-upwards/"))
    }).unwrap();
    assert_eq!(tunnel.position, [-36., 15., 108.]);
    assert_eq!(tunnel.restriction_origin, Some([324., 108.]));
    assert!(app.generated_image_ready(id));
    assert!(app.task.is_none() && app.placement_task.is_none() && !app.generation.busy());
    assert!(!app.dirty());

    app.save_project_to(output.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    let mut offline = crate::tests::fixture(&ctx);
    offline.open_project_from(output, &ctx);
    wait_io(&mut offline, &ctx);
    assert_eq!(offline.visible_generated_tab(), Some(id));
    assert_eq!(offline.generated_image_warnings(id).0, 0);
    assert!(offline.generated_image_ready(id));
    assert!(offline.task.is_none() && offline.placement_task.is_none());
}

#[test]
#[ignore = "owned world and cached map typography integration"]
fn owned_world_bytes_palette_and_shared_font_survive_project_reopening() {
    use sha2::{Digest, Sha256};
    let world = PathBuf::from(
        std::env::var_os("MAPPER_PROJECT_PERSISTENCE_WORLD")
            .expect("MAPPER_PROJECT_PERSISTENCE_WORLD"),
    );
    let map = PathBuf::from(
        std::env::var_os("MAPPER_PROJECT_PERSISTENCE_MAP").expect("MAPPER_PROJECT_PERSISTENCE_MAP"),
    );
    let labels = PathBuf::from(
        std::env::var_os("MAPPER_PROJECT_PERSISTENCE_LABELS")
            .expect("MAPPER_PROJECT_PERSISTENCE_LABELS"),
    );
    let output = PathBuf::from(
        std::env::var_os("MAPPER_PROJECT_PERSISTENCE_OUTPUT")
            .expect("MAPPER_PROJECT_PERSISTENCE_OUTPUT"),
    );
    let retain = |path: &std::path::Path| {
        let file = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        std::fs::copy(path, &file).unwrap();
        Arc::new(file)
    };
    let ctx = egui::Context::default();
    let recipe = Project::load(&world.join("project.mapper")).unwrap();
    let palette = Arc::new(
        mapper_studio::terrain_palette::Palette::decode(
            &std::fs::read(world.join("terrain-palette.json")).unwrap(),
            recipe.installation.as_ref().unwrap(),
        )
        .unwrap(),
    );
    let metadata =
        serde_json::from_slice(&std::fs::read(labels.join("labels.json")).unwrap()).unwrap();
    let alpha = std::fs::read(labels.join("map-font.atlas")).unwrap();
    let style = mapper_studio::map_labels::Style::decode(&metadata).unwrap();
    let font_image = style.image(&alpha).unwrap();
    let font = Arc::new(mapper_studio::map_labels::Prepared {
        metadata: Arc::new(metadata),
        installation: recipe.installation.clone().unwrap(),
        sha256: format!(
            "{:x}",
            Sha256::digest(std::fs::read(labels.join("receipt.json")).unwrap())
        ),
        style,
        image_sha256: format!(
            "{:x}",
            Sha256::digest(bytemuck::cast_slice(&font_image.pixels))
        ),
        image: Arc::new(font_image),
        alpha: alpha.into(),
        grid: std::env::var_os("MAPPER_TEST_PAPER_MAP_GRID").map(|path| {
            Arc::new(mapper_studio::map_grid::Appearance::read_capture(std::path::Path::new(&path)).unwrap())
        }),
    });
    let overview = retain(&world.join("overview.png"));
    let archive = map_archive::Archive::new(retain(&map), None, "Owned raw-world fixture".into()).unwrap();
    let map_hash = format!("{:x}", Sha256::digest(std::fs::read(&map).unwrap()));
    let mut app = crate::tests::fixture(&ctx);
    app.project = recipe.clone();
    app.saved = recipe.clone();
    for id in 1..=2 {
        app.generation.results.push(Candidate {
            identity: None,
            overview: Some(overview.clone()),
            archive: Some(archive.clone()),
            archive_issue: String::new(),
            id,
            label: format!("Retained world {id}"),
            recipe: recipe.clone(),
            image: Some(Ok(ctx.load_texture(
                format!("owned-world-{id}"),
                read_overview(overview.as_ref(), &|| false).unwrap().unwrap(),
                egui::TextureOptions::LINEAR,
            ))),
            zoom: 1.,
            pan: Vec2::ZERO,
            placed: 0,
            missing: 0,
            monuments: Vec::new(),
            routes: Vec::new(),
            restrictions: Default::default(),
            grid: true,
            no_build: false,
            map_font: Some(font.clone()),
            background: Color32::from_rgb(24, 66, 95),
            palette: Some(palette.clone()),
        });
        app.generation.queue.push(QueueItem { origin: None,
            id,
            label: format!("Retained world {id}"),
            recipe: recipe.clone(),
            state: State::Ready,
            detail: String::new(),
        });
        app.show_tab(Tab::Generated(id));
        app.apply_tab_request();
    }
    app.generation.next_id = 3;
    app.generation.revision = 1;
    app.show_tab(Tab::Generated(1));
    app.apply_tab_request();
    app.save_project_to(output.clone(), &ctx).unwrap();
    wait_io(&mut app, &ctx);
    assert!(!app.dirty(), "{}", app.status);
    let loaded = mapper_studio::project_bundle::load(&output, || false)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.blobs.len(), 3 + usize::from(font.grid.is_some()), "Shared payloads were duplicated");
    drop(loaded);
    drop(app);
    let mut reopened = crate::tests::fixture(&ctx);
    reopened.open_project_from(output.clone(), &ctx);
    wait_io(&mut reopened, &ctx);
    assert_eq!(reopened.generation.results.len(), 2, "{}", reopened.status);
    assert_eq!(reopened.visible_generated_tab(), Some(1));
    assert!(reopened.preview.is_none());
    assert!(reopened.task.is_none());
    assert!(reopened.source.bound_root(&recipe).is_none());
    let a = &reopened.generation.results[0];
    let b = &reopened.generation.results[1];
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(std::fs::read(a.archive.as_ref().unwrap().file.as_ref()).unwrap())
        ),
        map_hash
    );
    assert!(
        Arc::ptr_eq(a.map_font.as_ref().unwrap(), b.map_font.as_ref().unwrap()),
        "Shared fonts consumed duplicate memory"
    );
    assert_eq!(a.map_font.as_ref().unwrap().image_sha256, font.image_sha256);
    assert_eq!(
        a.map_font.as_ref().unwrap().grid.as_ref().map(|grid| grid.atlas_id()),
        font.grid.as_ref().map(|grid| grid.atlas_id()),
    );
    assert!(Arc::ptr_eq(a.palette.as_ref().unwrap(), b.palette.as_ref().unwrap()));
    assert_eq!(
        a.palette.as_ref().unwrap().render_key(),
        palette.render_key()
    );
    assert_eq!(Project::load(&output).unwrap(), recipe);
    println!(
        "Retained map SHA-256 {map_hash}; {} project bytes; shared map, overview and font payloads; no terrain generation",
        std::fs::metadata(&output).unwrap().len()
    );
}
