use super::*;
use crate::prepared_assets as assets;
use mapper_studio::prefab_textures::Bindings;

#[test]
#[ignore = "owned installation scenery completion without drawing the recipe panel"]
fn owned_scenery_verification_finishes_while_the_recipe_section_is_closed() {
    let _verification = mapper_studio::installation::VerificationScope::default();
    let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
    let root = path("MAPPER_SCENERY_RUST");
    let mut project = Project::load(&path("MAPPER_SCENERY_PROJECT")).unwrap();
    let catalog = Arc::new(
        mapper_studio::prefab_assets::load(
            &root,
            &path("MAPPER_SCENERY_CATALOG"),
            project.installation.as_ref().unwrap(),
            || false,
        )
        .unwrap(),
    );
    let bindings = Arc::new(
        Bindings::load(&root, &path("MAPPER_SCENERY_BINDINGS"), &catalog, || false).unwrap(),
    );
    let decoration = project
        .decoration
        .take()
        .expect("Recipe must request scenery");
    project.objects = None;
    project.validate().unwrap();
    let before = project.clone();
    let source = Source::cached(&catalog, &bindings, &decoration, || false).unwrap();
    let ctx = egui::Context::default();
    let mut panel = DecorPanel::default();
    panel.accept_prepared(&project, root.clone(), &catalog, Arc::new(source));
    panel.pending_decoration = Some((before.clone(), decoration.clone()));
    assert!(panel.readiness().is_err());
    panel.tick(
        &project,
        Some(root.clone()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    assert!(panel.live_task.running(), "{}", panel.live_message);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut history = History::default();
    while panel.live_task.running() && std::time::Instant::now() < deadline {
        if let Some(previous) = panel.poll_live(&mut project, &ctx) {
            history.record(previous, &project);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(panel.pending_decoration.is_none(), "{}", panel.live_message);
    assert!(panel.readiness().is_ok());
    assert_eq!(project.decoration, Some(decoration.clone()));
    assert!(project.objects.is_some(), "{}", panel.live_message);
    assert_eq!(project.rails, before.rails);
    assert_eq!(project.seeds, before.seeds);
    assert_eq!(project.targets, before.targets);
    project.validate().unwrap();
    let accepted = project.clone();
    assert!(history.can_undo());
    history.undo(&mut project);
    assert_eq!(project, before);
    history.redo(&mut project);
    assert_eq!(project, accepted);

    project = before;
    panel.pending_decoration = Some((project.clone(), decoration));
    panel.tick(
        &project,
        Some(root.clone()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    assert!(panel.live_task.running());
    project.master_seed += 1;
    project.refresh_inherited_seeds();
    let changed = project.clone();
    panel.tick(&project, Some(root), Some(catalog), Some(bindings), &ctx);
    assert!(panel.poll_live(&mut project, &ctx).is_none());
    assert!(!panel.live_task.running());
    assert!(panel.pending_decoration.is_none());
    assert_eq!(project, changed);
    assert!(panel.readiness().is_ok());
}

pub(super) fn inputs(
    root: &std::path::Path,
    supplied: Option<Project>,
) -> (DecorPanel, Project, Arc<Catalog>, Arc<Bindings>) {
    let (catalog, bindings) = assets::load(root);
    let mut project = supplied.unwrap_or_else(super::live_tests::rail_project);
    project.installation = Some(catalog.installation.clone());
    project.monument_assets = Some(mapper_studio::AssetIdentity {
        catalog_sha256: catalog.sha256.clone(),
        bindings_sha256: bindings.sha256.clone(),
    });
    project
        .decoration
        .get_or_insert_with(|| mapper_studio::decor_source::Recipe::new("2".repeat(64)));
    project
        .objects
        .get_or_insert_with(|| mapper_studio::object_generation::Recipe::new("3".repeat(64)));
    project.validate().unwrap();
    let mut source: Source = serde_json::from_value(serde_json::json!({
        "format":"papyrus-decor-source", "schema":4,
        "adapter":"papyrus-static-decor-collider-4", "scene":"fixture",
        "sources":[], "extractor_sha256":"a".repeat(64),
        "stages":[], "prefabs":{}, "issues":[]
    }))
    .unwrap();
    source.sha256 = project.decoration.as_ref().unwrap().source_sha256.clone();
    let mut panel = DecorPanel::default();
    panel.accept_prepared(&project, root.to_owned(), &catalog, Arc::new(source));
    (panel, project, catalog, bindings)
}

#[test]
fn prepared_scenery_stays_ready_through_repeated_editor_updates() {
    let directory = tempfile::tempdir().unwrap();
    let (mut panel, project, catalog, bindings) = inputs(directory.path(), None);
    let ctx = egui::Context::default();
    let before = project.clone();
    for _ in 0..20 {
        panel.tick(
            &project,
            Some(directory.path().to_owned()),
            Some(catalog.clone()),
            Some(bindings.clone()),
            &ctx,
        );
        assert!(panel.live_inputs.is_some());
        assert!(
            panel
                .live_inputs
                .as_ref()
                .unwrap()
                .with_project(&project)
                .is_some()
        );
        assert!(!panel.live_task.running());
        assert!(panel.task.is_none());
    }
    assert_eq!(project, before);
}

#[test]
fn missing_sources_and_disabled_roads_drop_cached_placement_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let (mut panel, mut project, catalog, bindings) = inputs(directory.path(), None);
    let ctx = egui::Context::default();
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    assert!(panel.live_inputs.is_some());
    let roads = project.roads.take();
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    assert!(panel.live_inputs.is_none());
    assert!(panel.task.is_none());
    project.roads = roads;
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    assert!(panel.live_inputs.is_some());
    panel.tick(&project, None, None, None, &ctx);
    assert!(panel.live_inputs.is_none());
    assert!(panel.source.is_none());
    assert!(panel.task.is_none());
}

#[test]
#[ignore = "controlled scenery maintenance benchmark"]
fn repeated_scenery_maintenance_benchmark() {
    let directory = tempfile::tempdir().unwrap();
    let supplied = std::env::var_os("MAPPER_SCENERY_BENCHMARK_PROJECT")
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap());
    let (mut panel, project, catalog, bindings) = inputs(directory.path(), supplied);
    let ctx = egui::Context::default();
    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        for _ in 0..400 {
            panel.tick(
                &project,
                Some(directory.path().to_owned()),
                Some(catalog.clone()),
                Some(bindings.clone()),
                &ctx,
            );
            let controls = panel
                .live_inputs
                .as_ref()
                .and_then(|i| i.with_project(&project));
            std::hint::black_box(controls);
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    assert!(panel.live_inputs.is_some());
    assert!(!panel.live_task.running());
    assert!(panel.task.is_none());
    samples.sort_by(f64::total_cmp);
    println!("400 scenery tick/visible-control input pairs, milliseconds: {samples:?}");
    println!("Median milliseconds per pair: {}", samples[2] / 400.);
}
