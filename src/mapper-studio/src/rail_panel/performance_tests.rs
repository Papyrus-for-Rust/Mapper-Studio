use super::*;
use crate::prepared_assets as assets;

fn copy_catalog(catalog: &Catalog) -> Catalog {
    Catalog {
        sha256: catalog.sha256.clone(),
        installation: catalog.installation.clone(),
        sources: catalog.sources.clone(),
        entries: catalog.entries.clone(),
    }
}

fn inputs(
    root: &std::path::Path,
    supplied: Option<Project>,
) -> (RailPanel, Project, Arc<Catalog>, Arc<Bindings>) {
    let (mut panel, mut project, mut key, recipe) = super::tests::fixture();
    if let Some(supplied) = supplied {
        project = supplied;
    }
    let (catalog, bindings) = assets::load(root);
    project.installation = Some(catalog.installation.clone());
    project.monument_assets = Some(mapper_studio::AssetIdentity {
        catalog_sha256: catalog.sha256.clone(),
        bindings_sha256: bindings.sha256.clone(),
    });
    // Keep any supplied project's size and settings payload, with matching test assets.
    key.project = Arc::new(project.clone());
    key.root = root.to_owned();
    key.catalog = catalog.sha256.clone();
    key.bindings = bindings.sha256.clone();
    let ready = project.rails.clone().unwrap_or(recipe);
    panel.ready = Some((key, ready));
    (panel, project, catalog, bindings)
}

#[test]
fn unchanged_maintenance_reuses_snapshots_and_refreshes_asset_handles() {
    let directory = tempfile::tempdir().unwrap();
    let (mut panel, project, catalog, bindings) = inputs(directory.path(), None);
    let ctx = egui::Context::default();
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    let snapshot = panel.inputs.as_ref().unwrap().key.project.clone();
    let ready = panel.ready.as_ref().unwrap().0.project.clone();
    for _ in 0..20 {
        panel.tick(
            &project,
            Some(directory.path().to_owned()),
            Some(catalog.clone()),
            Some(bindings.clone()),
            &ctx,
        );
        panel.refresh(&project);
        assert!(Arc::ptr_eq(
            &snapshot,
            &panel.inputs.as_ref().unwrap().key.project
        ));
        assert!(Arc::ptr_eq(
            &snapshot,
            &panel.prepared_attempt.as_ref().unwrap().project
        ));
        assert!(Arc::ptr_eq(
            &ready,
            &panel.ready.as_ref().unwrap().0.project
        ));
    }
    let replacement_catalog = Arc::new(copy_catalog(&catalog));
    let replacement_bindings = Arc::new(
        Bindings::load(
            directory.path(),
            &directory.path().join("bindings.json"),
            &replacement_catalog,
            || false,
        )
        .unwrap(),
    );
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(replacement_catalog.clone()),
        Some(replacement_bindings.clone()),
        &ctx,
    );
    let current = panel.inputs.as_ref().unwrap();
    assert!(Arc::ptr_eq(&snapshot, &current.key.project));
    assert!(Arc::ptr_eq(&replacement_catalog, &current.catalog));
    assert!(Arc::ptr_eq(&replacement_bindings, &current.bindings));
    assert!(panel.ready.is_some());
}

#[test]
fn cached_attempts_invalidate_on_recipe_and_installation_changes() {
    for change in [
        "size",
        "seed",
        "rail pin",
        "road pin",
        "binding pin",
        "catalog",
        "root",
        "roads disabled",
        "installation",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (mut panel, mut project, mut catalog, bindings) = inputs(directory.path(), None);
        let ctx = egui::Context::default();
        let mut root = directory.path().to_owned();
        panel.tick(
            &project,
            Some(root.clone()),
            Some(catalog.clone()),
            Some(bindings.clone()),
            &ctx,
        );
        let previous = panel.inputs.as_ref().unwrap().key.project.clone();
        match change {
            "size" => project.size += 50,
            "seed" => project.master_seed += 1,
            "rail pin" => {
                let mut recipe = panel.ready.as_ref().unwrap().1.clone();
                recipe.mesh_sha256 = "9".repeat(64);
                project.rails = Some(recipe);
            }
            "road pin" => project.roads.as_mut().unwrap().source_sha256 = "9".repeat(64),
            "binding pin" => {
                project.monument_assets.as_mut().unwrap().bindings_sha256 = "9".repeat(64);
            }
            "catalog" => {
                let mut replacement = copy_catalog(&catalog);
                replacement.sha256 = "9".repeat(64);
                catalog = Arc::new(replacement);
            }
            "root" => root.push("other installation"),
            "roads disabled" => project.roads = None,
            _ => project.installation.as_mut().unwrap().native.sha256 = "9".repeat(64),
        }
        panel.tick(
            &project,
            Some(root.clone()),
            Some(catalog.clone()),
            Some(bindings.clone()),
            &ctx,
        );
        assert!(panel.ready.is_none(), "{change}");
        if let Some(current) = &panel.inputs {
            assert!(!Arc::ptr_eq(&previous, &current.key.project), "{change}");
            let failed_attempt = panel.prepared_attempt.as_ref().unwrap().project.clone();
            let message = panel.message.clone();
            panel.tick(&project, Some(root), Some(catalog), Some(bindings), &ctx);
            assert!(Arc::ptr_eq(
                &failed_attempt,
                &panel.prepared_attempt.as_ref().unwrap().project
            ));
            assert_eq!(panel.message, message);
        } else {
            assert!(panel.prepared_attempt.is_none(), "{change}");
        }
    }
}

#[test]
fn accepting_prepared_data_rechecks_an_unchanged_recipe() {
    let directory = tempfile::tempdir().unwrap();
    let (mut panel, project, catalog, bindings) = inputs(directory.path(), None);
    let ctx = egui::Context::default();
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog.clone()),
        Some(bindings.clone()),
        &ctx,
    );
    assert!(panel.prepared_attempt.is_some());
    assert!(panel.ready.is_some());
    panel.accept_prepared(None);
    assert!(panel.prepared_attempt.is_none());
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog),
        Some(bindings),
        &ctx,
    );
    assert!(panel.prepared_attempt.is_some());
    assert!(panel.ready.is_none());
    assert!(panel.message.contains("Rail data is unavailable"));
}

#[test]
fn same_frame_recipe_edits_invalidate_the_cached_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let (mut panel, mut project, catalog, bindings) = inputs(directory.path(), None);
    let ctx = egui::Context::default();
    panel.tick(
        &project,
        Some(directory.path().to_owned()),
        Some(catalog),
        Some(bindings),
        &ctx,
    );
    let before = panel.prepared_attempt.as_ref().unwrap().project.clone();
    project.size += 50;
    panel.refresh(&project);
    assert!(panel.ready.is_none());
    let current = &panel.prepared_attempt.as_ref().unwrap().project;
    assert!(!Arc::ptr_eq(&before, current));
    assert_eq!(current.as_ref(), &project);
}

#[test]
#[ignore = "controlled editor maintenance benchmark"]
fn repeated_rail_maintenance_benchmark() {
    let directory = tempfile::tempdir().unwrap();
    let supplied = std::env::var_os("MAPPER_RAIL_BENCHMARK_PROJECT")
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
            panel.refresh(&project);
            std::hint::black_box(&panel);
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    assert!(panel.ready.is_some());
    samples.sort_by(f64::total_cmp);
    println!("400 rail tick/visible-panel refresh pairs, milliseconds: {samples:?}");
    println!("Median milliseconds per pair: {}", samples[2] / 400.);
}
