use super::*;
mod assets {
    use crate as mapper_studio;
    include!("../../examples/support/prepared_assets.rs");
}

fn fixture(root: &std::path::Path) -> (Project, Arc<Catalog>, Arc<Bindings>) {
    let (catalog, bindings) = assets::load(root);
    let project = Project {
        preview_engine: crate::native_preview::VERSION.into(),
        composition_engine: Some(papyrus_terrain::monument_composition::VERSION.into()),
        installation: Some(catalog.installation.clone()),
        monument_assets: Some(crate::AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: bindings.sha256.clone(),
        }),
        monument_rules: Some(
            serde_json::from_value(serde_json::json!({
                "engine":crate::monument_source::VERSION, "source_sha256":"c".repeat(64)
            }))
            .unwrap(),
        ),
        roads: Some(crate::infrastructure::Recipe::new("d".repeat(64))),
        decoration: Some(crate::decor_source::Recipe::new("e".repeat(64))),
        ..Default::default()
    };
    project.validate().unwrap();
    (project, catalog, bindings)
}

fn copy_catalog(catalog: &Catalog) -> Catalog {
    Catalog {
        sha256: catalog.sha256.clone(),
        installation: catalog.installation.clone(),
        sources: catalog.sources.clone(),
        entries: catalog.entries.clone(),
    }
}

#[test]
fn repeated_inputs_share_snapshots_and_keep_current_asset_handles() {
    let directory = tempfile::tempdir().unwrap();
    let (project, catalog, bindings) = fixture(directory.path());
    let mut inputs = Inputs::new(&project, catalog.clone(), bindings.clone()).unwrap();
    let snapshot = inputs.key.project.clone();
    for _ in 0..20 {
        inputs = Inputs::refresh(
            Some(inputs),
            &project,
            catalog.clone(),
            bindings.clone(),
            None,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&snapshot, &inputs.key.project));
        let controls = inputs.with_project(&project).unwrap();
        assert!(Arc::ptr_eq(&snapshot, &controls.key.project));
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
    inputs = Inputs::refresh(
        Some(inputs),
        &project,
        replacement_catalog.clone(),
        replacement_bindings.clone(),
        None,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&snapshot, &inputs.key.project));
    assert!(Arc::ptr_eq(&replacement_catalog, &inputs.catalog));
    assert!(Arc::ptr_eq(&replacement_bindings, &inputs.bindings));
    assert_eq!(inputs.key.project.as_ref(), &project);
}

#[test]
fn recipe_edits_revalidate_both_cached_and_same_frame_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let (project, catalog, bindings) = fixture(directory.path());
    for edit in ["size", "seed", "position", "decoration", "roads"] {
        let inputs = Inputs::new(&project, catalog.clone(), bindings.clone()).unwrap();
        let snapshot = inputs.key.project.clone();
        let mut next = project.clone();
        match edit {
            "size" => next.size += 50,
            "seed" => {
                next.master_seed += 1;
                next.refresh_inherited_seeds();
            }
            "position" => next.targets[0].x += 1.,
            "decoration" => next.decoration.as_mut().unwrap().source_sha256 = "9".repeat(64),
            _ => next.roads.as_mut().unwrap().source_sha256 = "9".repeat(64),
        }
        let controls = inputs.with_project(&next).unwrap();
        assert!(!Arc::ptr_eq(&snapshot, &controls.key.project), "{edit}");
        assert_eq!(controls.key.project.as_ref(), &next);
        let refreshed =
            Inputs::refresh(Some(inputs), &next, catalog.clone(), bindings.clone(), None).unwrap();
        assert!(!Arc::ptr_eq(&snapshot, &refreshed.key.project), "{edit}");
        assert_eq!(refreshed.key.project.as_ref(), &next);
    }
}

#[test]
fn invalid_or_unavailable_inputs_cannot_reuse_a_valid_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let (project, catalog, bindings) = fixture(directory.path());
    for edit in [
        "seed",
        "roads",
        "decoration",
        "monuments",
        "binding pin",
        "installation",
        "objects",
    ] {
        let inputs = Inputs::new(&project, catalog.clone(), bindings.clone()).unwrap();
        let mut next = project.clone();
        match edit {
            "seed" => next.master_seed = 0,
            "roads" => next.roads = None,
            "decoration" => next.decoration = None,
            "monuments" => next.monument_rules = None,
            "binding pin" => {
                next.monument_assets.as_mut().unwrap().bindings_sha256 = "9".repeat(64)
            }
            "installation" => next.installation.as_mut().unwrap().native.sha256 = "9".repeat(64),
            _ => {
                let mut objects = Recipe::new("9".repeat(64));
                objects.maximum_terrain_work = 0;
                next.objects = Some(objects);
            }
        }
        assert!(inputs.with_project(&next).is_none(), "{edit}");
        assert!(
            Inputs::refresh(Some(inputs), &next, catalog.clone(), bindings.clone(), None).is_none(),
            "{edit}"
        );
    }
    let inputs = Inputs::new(&project, catalog.clone(), bindings.clone()).unwrap();
    let mut altered_catalog = copy_catalog(&catalog);
    altered_catalog.installation.native.bytes += 1;
    assert!(
        Inputs::refresh(
            Some(inputs),
            &project,
            Arc::new(altered_catalog),
            bindings,
            None
        )
        .is_none()
    );
}

#[test]
fn decoration_overrides_remain_private_and_revalidate_when_changed() {
    let directory = tempfile::tempdir().unwrap();
    let (mut project, catalog, bindings) = fixture(directory.path());
    project.decoration = None;
    let original = project.clone();
    let decoration = crate::decor_source::Recipe::new("e".repeat(64));
    let inputs = Inputs::with_decoration(
        &project,
        catalog.clone(),
        bindings.clone(),
        decoration.clone(),
    )
    .unwrap();
    let snapshot = inputs.key.project.clone();
    let inputs = Inputs::refresh(
        Some(inputs),
        &project,
        catalog.clone(),
        bindings.clone(),
        Some(&decoration),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&snapshot, &inputs.key.project));
    assert!(inputs.key.project.decoration.is_none());
    assert_eq!(inputs.key.decoration, Some(decoration));
    let replacement = crate::decor_source::Recipe::new("f".repeat(64));
    let inputs = Inputs::refresh(
        Some(inputs),
        &project,
        catalog.clone(),
        bindings.clone(),
        Some(&replacement),
    )
    .unwrap();
    assert!(!Arc::ptr_eq(&snapshot, &inputs.key.project));
    assert_eq!(inputs.key.decoration, Some(replacement));
    assert_eq!(project, original);
    assert!(Inputs::refresh(Some(inputs), &project, catalog, bindings, None).is_none());
}

#[test]
fn relocated_assets_invalidate_work_despite_matching_file_identities() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (project, catalog, bindings) = fixture(first.path());
    let inputs = Inputs::new(&project, catalog, bindings).unwrap();
    let snapshot = inputs.key.project.clone();
    let signal = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let mut state = Preparation {
        task: Some(Task {
            key: inputs.key.clone(),
            cancel: signal.clone(),
            receiver,
        }),
    };
    sender.send(Ok(Recipe::new("9".repeat(64)))).unwrap();
    let (catalog, bindings) = assets::load(second.path());
    let next = Inputs::refresh(Some(inputs), &project, catalog, bindings, None).unwrap();
    assert!(!Arc::ptr_eq(&snapshot, &next.key.project));
    assert_eq!(next.key.root, second.path());
    assert!(state.discard_changed(Some(&next)));
    assert!(signal.load(Ordering::Relaxed));
    assert!(state.poll(Some(&next)).is_none());
}

#[test]
fn completed_work_survives_matching_inputs_and_discards_changed_recipes() {
    let directory = tempfile::tempdir().unwrap();
    let (project, catalog, bindings) = fixture(directory.path());
    for changed in [false, true] {
        let inputs = Inputs::new(&project, catalog.clone(), bindings.clone()).unwrap();
        let signal = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let mut state = Preparation {
            task: Some(Task {
                key: inputs.key.clone(),
                cancel: signal.clone(),
                receiver,
            }),
        };
        sender.send(Ok(Recipe::new("9".repeat(64)))).unwrap();
        let mut next = project.clone();
        if changed {
            next.size += 50;
        }
        let inputs =
            Inputs::refresh(Some(inputs), &next, catalog.clone(), bindings.clone(), None).unwrap();
        assert_eq!(state.discard_changed(Some(&inputs)), changed);
        if changed {
            assert!(signal.load(Ordering::Relaxed));
            assert!(state.poll(Some(&inputs)).is_none());
        } else {
            assert!(
                matches!(state.poll(Some(&inputs)), Some(Event::Ready(recipe)) if recipe.road_source_sha256 == "9".repeat(64))
            );
            assert!(state.poll(Some(&inputs)).is_none());
        }
    }
}
