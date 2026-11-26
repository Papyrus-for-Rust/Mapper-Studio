//! Required normal-generation behavior. Historical switches remain readable.
use crate::Project;

pub(crate) fn set_version(slot: &mut Option<String>, version: &str) {
    if slot.as_deref() != Some(version) {
        *slot = Some(version.into());
    }
}

pub fn apply(project: &mut Project) {
    if !crate::native_preview::is_native(&project.preview_engine) {
        return;
    }
    if project
        .composition_engine
        .as_deref()
        .is_none_or(papyrus_terrain::monument_composition::supported)
    {
        set_version(
            &mut project.composition_engine,
            papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION,
        );
    }
    if let Some(roads) = &mut project.roads {
        roads.apply_correctness_defaults();
    }
    if let Some(rules) = &mut project.monument_rules
        && crate::monument_source::supported(&rules.engine)
    {
        set_version(
            &mut rules.canonical_yaw,
            crate::monument_source::CANONICAL_YAW_VERSION,
        );
        if let Some(stages) = &mut rules.stage_rules {
            set_version(&mut stages.spacing, crate::monument_stages::SPACING_VERSION);
        }
        if rules.coastal_profile.is_some() {
            if rules.engine != crate::coastal_placement::BUDGET_VERSION {
                rules.engine = crate::coastal_placement::BUDGET_VERSION.into();
            }
            if project.placement_engine != rules.engine {
                project.placement_engine.clone_from(&rules.engine);
            }
            set_version(
                &mut rules.coastal_normals,
                papyrus_terrain::terrain_normal::VERSION,
            );
            set_version(
                &mut rules.coastal_navigation,
                papyrus_terrain::boat_navigation::SOURCE_VERSION,
            );
            rules.coastal_search.get_or_insert_with(Default::default);
            set_version(
                &mut rules.tier_search,
                crate::monument_source::TIER_RETRY_VERSION,
            );
            set_version(
                &mut rules.earthwork,
                crate::monument_source::AUTHORED_EARTHWORK_VERSION,
            );
        }
    }
}

/// Resolve data-dependent defaults on a worker, against the selected installation.
pub fn resolve(
    project: &Project,
    root: &std::path::Path,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Project, String> {
    let mut next = project.clone();
    apply(&mut next);
    if let Some(rules) = &next.monument_rules {
        let mut source =
            crate::monument_source::Source::cached(rules, catalog, bindings, &cancelled)?;
        if source.coastal_profile.is_none() {
            source.prepare_coastal(catalog, root, &cancelled)?;
        }
        if source.stage_source.is_none() {
            source.prepare_stages(catalog, root, &cancelled)?;
        }
        let population = rules
            .stage_rules
            .as_ref()
            .and_then(|s| s.population.clone());
        let mut rules = source.recipe();
        rules.land_alignment = next.monument_rules.as_ref().unwrap().land_alignment.clone();
        rules.offshore_source = if let Some(binding) = &rules.offshore_source {
            crate::offshore_generation::cached(binding, root, catalog, &cancelled)?;
            Some(binding.clone())
        } else {
            crate::offshore_generation::prepare(root, catalog, &cancelled)?
        };
        rules.dungeon_source = if let Some(sha) = &rules.dungeon_source {
            Some(
                crate::dungeon_source::Source::cached(
                    &sha.source_sha256,
                    root,
                    catalog,
                    &source,
                    &cancelled,
                )?
                .binding(),
            )
        } else {
            crate::dungeon_source::Source::prepare(root, catalog, &source, &cancelled)?
                .map(|s| s.binding())
        };
        // Keep the generation policy recorded in the saved recipe.
        rules.surface_modifiers = next
            .monument_rules
            .as_ref()
            .unwrap()
            .surface_modifiers
            .clone();
        if let Some(saved) = next
            .monument_rules
            .as_ref()
            .and_then(|r| r.coastal_search.clone())
        {
            rules.coastal_search = Some(saved);
        }
        if let Some(stages) = &mut rules.stage_rules {
            stages.population = population;
        }
        let retain_source = next.monument_rules.as_ref() == Some(&rules);
        next.placement_engine = rules.engine.clone();
        next.monument_rules = Some(rules);
        if !cancelled()
            && next.performance.monuments
            && next.options.requests_landmarks()
            && let Some(stages) = &source.stage_source
        {
            next.monument_assets.get_or_insert_with(|| crate::AssetIdentity {
                catalog_sha256: catalog.sha256.clone(),
                bindings_sha256: bindings.sha256.clone(),
            });
            crate::placement_preview::populate_requested_landmarks(&mut next, catalog, stages)?;
        }
        // Retain the verified source when resolution kept the saved binding.
        if retain_source && !cancelled() {
            source.retain_verified(next.monument_rules.as_ref().unwrap(), catalog, bindings);
        }
    }
    if let Some(roads) = &mut next.roads {
        let previous = crate::infrastructure::Source::cached(catalog, roads, &cancelled)?;
        let available = crate::infrastructure::Source::discover(catalog, None, &cancelled)?;
        let candidate =
            (available.profile.revision > previous.profile.revision).then_some(available);
        let source = if let Some(source) = candidate {
            let surface =
                crate::road_extract::prepare_surface(root, &source, &cancelled)?.map(|s| s.sha256);
            let roadside =
                crate::roadside_source::prepare(root, catalog, &cancelled)?.map(|s| s.sha256);
            let layout =
                crate::road_extract::prepare_ordered(root, catalog, &source, None, &cancelled)?
                    .map(|inputs| crate::source_road_planning::Policy::bind(&inputs));
            *roads = roads.rebind_source(&previous, &source, surface, roadside, layout)?;
            source
        } else {
            previous
        };
        roads.apply_source_defaults(&source)?;
        if roads.surface_sha256.is_none() {
            roads.surface_sha256 =
                crate::road_extract::prepare_surface(root, &source, &cancelled)?.map(|s| s.sha256);
        }
        if roads.engine == papyrus_terrain::road_network::COST_VERSION
            && roads.surface_sha256.is_some()
            && (next.rails.is_none() || crate::rail_generation::before_roads(project))
            && next.performance.monuments
            && next
                .monument_rules
                .as_ref()
                .is_some_and(|r| r.coastal_profile.is_some())
        {
            let adding_staged = roads.staged.is_none();
            if adding_staged
                && let Some(side) = crate::roadside_source::prepare(root, catalog, &cancelled)?
            {
                let paint = crate::road_paint_source::Profile::for_source(&side)?
                    .ok_or("No matching roadside paint rules")?;
                crate::road_terrain_source::Profile::for_paint(&paint)?
                    .ok_or("No matching road terrain rules")?;
                roads.staged = Some(crate::road_generation::Policy {
                    engine: crate::road_generation::VERSION.into(),
                    roadside_source_sha256: side.sha256,
                    roadside_preferences: Vec::new(),
                    layout: None,
                });
            }
            if roads.staged.is_some() {
                roads.set_global_earthwork(true)?;
                if roads.staged.as_ref().unwrap().layout.is_none()
                    && let Some(inputs) = crate::road_extract::prepare_ordered(
                        root, catalog, &source, None, &cancelled,
                    )?
                {
                    roads.staged.as_mut().unwrap().layout =
                        Some(crate::source_road_planning::Policy::bind(&inputs));
                }
                roads.apply_correctness_defaults();
                // GUI and headless generation share the new-recipe contract.
                // Existing source recipes receive junction correctness updates above.
                if adding_staged {
                    let staged = roads.staged.as_ref().unwrap();
                    roads.staged = Some(crate::road_generation::Policy::for_new_recipe(
                        staged.roadside_source_sha256.clone(),
                        staged.layout.clone(),
                        false, // this branch excludes rails generated after roads
                    )?);
                }
            }
        }
    }
    if let Some(objects) = next.objects.as_mut()
        && objects.engine == crate::object_generation::VERSION
        && (objects.uniform_cliffs_source_sha256.is_none()
            || objects.coastal_cliffs_source_sha256.is_none()
            || objects.jungle_vines_source_sha256.is_none())
    {
        let roads = crate::infrastructure::Source::cached(
            catalog,
            next.roads
                .as_ref()
                .ok_or("Missing roads for hill-cliff placement")?,
            &cancelled,
        )?;
        let source = crate::road_object_source::Source::cached(
            root,
            &objects.road_source_sha256,
            &roads,
            &cancelled,
        )?;
        if objects.uniform_cliffs_source_sha256.is_none() {
            objects.uniform_cliffs_source_sha256 = Some(
                crate::uniform_cliff_source::available(root, catalog, &source, &cancelled)?
                    .sha256()
                    .to_owned(),
            );
        }
        if objects.coastal_cliffs_source_sha256.is_none() {
            objects.coastal_cliffs_source_sha256 = Some(
                crate::coastal_cliff_source::available(root, catalog, &source, &cancelled)?
                    .sha256()
                    .to_owned(),
            );
        }
        if objects.jungle_vines_source_sha256.is_none() {
            objects.jungle_vines_source_sha256 = Some(
                crate::jungle_vine_source::available(root, catalog, &source, &cancelled)?
                    .sha256().to_owned(),
            );
        }
    }
    next.validate()?;
    Ok(next)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_junction_upgrades_keep_saved_layout_inputs_and_coupling() {
        use crate::road_generation;
        for (saved, expected) in [
            (
                road_generation::RAW_SOURCE_VERSION,
                road_generation::CONNECTED_SOURCE_VERSION,
            ),
            (
                road_generation::SOURCE_COUPLED_VERSION,
                road_generation::CONNECTED_COUPLED_VERSION,
            ),
            (
                road_generation::SOURCE_VERSION,
                road_generation::SOURCE_VERSION,
            ),
            (road_generation::VERSION, road_generation::VERSION),
        ] {
            let mut roads = crate::infrastructure::Recipe::new("a".repeat(64));
            roads.staged = Some(road_generation::Policy {
                engine: saved.into(),
                roadside_source_sha256: "b".repeat(64),
                roadside_preferences: Vec::new(),
                layout: Some(crate::source_road_planning::Policy {
                    engine: crate::source_road_planning::VERSION.into(),
                    assets_sha256: "c".repeat(64),
                    roles_sha256: "d".repeat(64),
                    ring_clearance: None,
                    terrain_entrances: Some(
                        papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION.into(),
                    ),
                }),
            });
            roads.apply_correctness_defaults();
            let mut expected_policy = roads.staged.clone().unwrap();
            expected_policy.engine = expected.into();
            roads.staged.as_mut().unwrap().engine = saved.into();
            let mut project = Project {
                preview_engine: crate::native_preview::VERSION.into(),
                roads: Some(roads),
                ..Default::default()
            };
            apply(&mut project);
            assert_eq!(
                project.roads.as_ref().unwrap().staged.as_ref(),
                Some(&expected_policy)
            );
            let stable = project.clone();
            apply(&mut project);
            assert_eq!(project, stable);
        }
    }
    #[test]
    fn native_defaults_preserve_bootstrap_entrances_and_upgrade_older_policies() {
        use papyrus_terrain::source_road_route::{
            TERRAIN_ENTRANCE_BOOTSTRAP_VERSION, TERRAIN_ENTRANCE_PINNED_VERSION,
            TERRAIN_ENTRANCE_ROUTED_VERSION, TERRAIN_ENTRANCE_VERSION,
        };
        for saved in [
            None,
            Some(TERRAIN_ENTRANCE_VERSION),
            Some(TERRAIN_ENTRANCE_ROUTED_VERSION),
            Some(TERRAIN_ENTRANCE_BOOTSTRAP_VERSION),
            Some(TERRAIN_ENTRANCE_PINNED_VERSION),
        ] {
            let mut roads = crate::infrastructure::Recipe::new("a".repeat(64));
            roads.staged = Some(crate::road_generation::Policy {
                engine: crate::road_generation::SOURCE_COUPLED_VERSION.into(),
                roadside_source_sha256: "b".repeat(64),
                roadside_preferences: Vec::new(),
                layout: Some(crate::source_road_planning::Policy {
                    engine: crate::source_road_planning::VERSION.into(),
                    assets_sha256: "c".repeat(64),
                    roles_sha256: "d".repeat(64),
                    ring_clearance: None,
                    terrain_entrances: saved.map(str::to_owned),
                }),
            });
            let mut project = Project {
                preview_engine: crate::native_preview::VERSION.into(),
                roads: Some(roads),
                ..Default::default()
            };
            apply(&mut project);
            let expected = match saved {
                Some(
                    engine @ (TERRAIN_ENTRANCE_BOOTSTRAP_VERSION | TERRAIN_ENTRANCE_PINNED_VERSION),
                ) => engine,
                _ => TERRAIN_ENTRANCE_ROUTED_VERSION,
            };
            assert_eq!(
                project
                    .roads
                    .as_ref()
                    .unwrap()
                    .staged
                    .as_ref()
                    .unwrap()
                    .layout
                    .as_ref()
                    .unwrap()
                    .terrain_entrances
                    .as_deref(),
                Some(expected)
            );
            let stable = project.clone();
            apply(&mut project);
            assert_eq!(project, stable);
        }
    }
    #[test]
    #[ignore = "requires owned installation, cached generation inputs and a complete saved recipe"]
    fn owned_resolution_preserves_pinned_monument_metadata_and_fills_missing_stages() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_MODEL_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_MODEL_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let bindings = crate::prefab_textures::Bindings::load(
            &root,
            &path("MAPPER_MODEL_TEST_BINDINGS"),
            &catalog,
            || false,
        )
        .unwrap();
        let mut project = Project::load(&path("MAPPER_DEFAULTS_TEST_PROJECT")).unwrap();
        apply(&mut project);
        let original = project.clone();
        let saved = project.monument_rules.as_ref().unwrap();
        assert!(saved.coastal_profile.is_some() && saved.stage_rules.is_some());
        let resolved = resolve(&project, &root, &catalog, &bindings, || false).unwrap();
        assert_eq!(resolved, original);
        let mut surface_enabled = project.clone();
        surface_enabled
            .monument_rules
            .as_mut()
            .unwrap()
            .surface_modifiers = Some(crate::monument_source::SURFACE_MODIFIER_VERSION.into());
        assert_eq!(
            resolve(&surface_enabled, &root, &catalog, &bindings, || false).unwrap(),
            surface_enabled
        );
        let mut incomplete = project.clone();
        incomplete.monument_rules.as_mut().unwrap().stage_rules = None;
        // Underground bindings require their monument-stage dependency.
        incomplete.monument_rules.as_mut().unwrap().dungeon_source = None;
        let completed = resolve(&incomplete, &root, &catalog, &bindings, || false).unwrap();
        assert!(
            completed
                .monument_rules
                .as_ref()
                .unwrap()
                .stage_rules
                .is_some()
        );
        assert_eq!(
            completed.monument_rules.as_ref().unwrap().source_sha256,
            saved.source_sha256
        );
        assert_eq!(
            completed.monument_rules.as_ref().unwrap().coastal_profile,
            saved.coastal_profile
        );
        assert!(resolve(&project, &root, &catalog, &bindings, || true).is_err());
        assert_eq!(project, original);
    }
    #[test]
    fn native_defaults_are_idempotent_and_preserve_map_choices() {
        let mut p = Project {
            preview_engine: crate::native_preview::VERSION.into(),
            composition_engine: None,
            ..Default::default()
        };
        let old = p.clone();
        apply(&mut p);
        assert_eq!(
            p.composition_engine.as_deref(),
            Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION)
        );
        let mut expected = old;
        expected.composition_engine = p.composition_engine.clone();
        assert_eq!(p, expected);
        let stable = p.clone();
        apply(&mut p);
        assert_eq!(p, stable);
    }
    #[test]
    fn unknown_future_versions_are_not_silently_reinterpreted() {
        let mut p = Project {
            preview_engine: crate::native_preview::VERSION.into(),
            composition_engine: Some("future-composer".into()),
            ..Default::default()
        };
        apply(&mut p);
        assert_eq!(p.composition_engine.as_deref(), Some("future-composer"));
        assert!(p.validate().is_err());
    }
}
