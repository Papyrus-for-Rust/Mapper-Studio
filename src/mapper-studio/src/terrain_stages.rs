//! Shared ordered terrain preparation for complete native worlds.
use crate::{
    Project, erosion_source, generation_resources, infrastructure,
    native_processing::Source,
    placement_preview::{self, CanonicalPlacement, RawProposal},
    prefab_assets::Catalog,
    prefab_textures::Bindings,
    road_object_source,
};
use papyrus_terrain::{erosion_pipeline, monument_composition::Composed, native_layers::Layers};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::BTreeSet,
    path::Path,
    sync::{Arc, LazyLock},
};

struct Cached {
    key: [u8; 32],
    source: Source,
}
impl Cached {
    fn matches(&self, key: [u8; 32], base: &Arc<Layers>) -> bool {
        self.key == key
            && self
                .source
                .native_base
                .as_ref()
                .is_some_and(|saved| Arc::ptr_eq(saved, base) || saved == base)
    }
}
mod variants;

static VARIANTS: LazyLock<variants::Variants> = LazyLock::new(variants::Variants::default);

#[derive(serde::Serialize)]
struct EarlyRoadInputs<'a> {
    source_sha256: &'a str,
    include_trails: bool,
    access_costmap: Option<&'a crate::road_cost_source::Profile>,
    entrance_launch: Option<papyrus_terrain::road_network::LaunchPolicy>,
    connected_entrances: bool,
}

impl<'a> From<&'a infrastructure::Recipe> for EarlyRoadInputs<'a> {
    fn from(roads: &'a infrastructure::Recipe) -> Self {
        // Early monument placement reads coastal entrance policy and the
        // infrastructure snapshot. Road fitting and roadside placement run later.
        Self {
            source_sha256: &roads.source_sha256,
            include_trails: roads.include_trails.unwrap_or(true),
            access_costmap: roads.access_costmap.as_ref(),
            entrance_launch: roads.entrance_launch,
            connected_entrances: roads.staged.as_ref().is_some_and(|staged| {
                crate::road_generation::is_connected_source_engine(&staged.engine)
            }),
        }
    }
}

fn cache_key(
    source: &Source,
    project: &Project,
    paths: &BTreeSet<String>,
    plan: &erosion_source::Plan,
    catalog: &str,
    bindings: &str,
    memory: usize,
) -> Result<[u8; 32], String> {
    let mut targets = project
        .targets
        .iter()
        .filter(|t| t.prefab_path.as_ref().is_some_and(|p| paths.contains(p)))
        .collect::<Vec<_>>();
    targets.sort_by_key(|t| t.instance_id);
    let identity = serde_json::json!({
        "version":"mapper-prepared-terrain-2", "terrain":project.terrain_key(),
        "placement":project.placement_engine, "composition":project.composition_engine,
        "rules":project.monument_rules, "targets":targets, "monuments":project.performance.monuments,
        "placement_options":project.options.early_placement_key(),
        "roads":project.roads.as_ref().map(EarlyRoadInputs::from),
        "object_source":project.objects.as_ref().map(|objects| &objects.road_source_sha256),
        "profile":plan.profile_sha256, "workflow":plan.workflow_sha256,
        "gpu_erosion":project.gpu_erosion,
        "terrain_graph":project.terrain_graph.as_deref().map(papyrus_terrain::terrain_graph::Graph::evaluation_key),
        "catalog":catalog, "bindings":bindings, "memory":memory,
        "palette":source.palette.as_ref().map(|p| &p.sha256),
    });
    Ok(Sha256::digest(serde_json::to_vec(&identity).map_err(|e| e.to_string())?).into())
}

pub struct Prepared {
    early: CanonicalPlacement,
    pub(crate) alpha: Vec<u8>,
    painting: Option<erosion_pipeline::Painting>,
    paint_active: bool,
    pub(crate) ocean: Option<Arc<papyrus_terrain::ocean::Ocean>>,
}

pub fn enabled(project: &Project) -> bool {
    crate::native_preview::is_native(&project.preview_engine)
        && project
            .monument_rules
            .as_ref()
            .and_then(|r| r.surface_modifiers.as_deref())
            == Some(crate::monument_source::SOURCE_PRIORITY_VERSION)
}

pub(crate) fn prepare<'a>(
    source: &'a Source,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    worker: Option<&Path>,
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<Option<Cow<'a, Source>>, String> {
    if !enabled(project) || source.terrain.is_some() {
        return Ok((!cancelled()).then_some(Cow::Borrowed(source)));
    }
    if cancelled() {
        return Ok(None);
    }
    let timing = crate::composition::StageTimer::start("terrain_preparation");
    let progress = crate::loading_progress::begin("Preparing terrain", None);
    let reporter = progress.reporter();
    let resources = generation_resources::current();
    let terrain_memory_mib = resources.terrain_memory_mib;
    let root = bindings.installation_root();
    let infrastructure = if let Some(roads) = &project.roads {
        infrastructure::Source::cached_shared(catalog, roads, cancelled)?
    } else {
        Arc::new(infrastructure::Source::discover(catalog, None, cancelled)?)
    };
    let objects = if let Some(objects) = &project.objects {
        road_object_source::Source::cached(
            root,
            &objects.road_source_sha256,
            &infrastructure,
            cancelled,
        )?
    } else {
        road_object_source::Source::discover(root, &infrastructure, cancelled)?
            .ok_or("Prepare this installation's terrain programs before generation")?
    };
    let Some(erosion) = erosion_source::Source::prepare(
        root,
        project
            .installation
            .as_ref()
            .ok_or("Missing terrain installation")?,
        &objects,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    let monuments = crate::monument_source::Source::cached_shared(
        project
            .monument_rules
            .as_ref()
            .ok_or("Missing terrain placement rules")?,
        catalog,
        bindings,
        cancelled,
    )?;
    if project.gpu_erosion.is_some() && !erosion.plan().erosion.active {
        return Err("The selected Rust workflow has no hydraulic erosion stage".into());
    }
    let paths = erosion
        .early_paths(
            monuments
                .stage_source
                .as_ref()
                .ok_or("Missing terrain programs")?,
        )?
        .into_iter()
        .collect();
    let base = source.native_base_arc()?;
    let variant_key = cache_key(
        source,
        project,
        &paths,
        erosion.plan(),
        &catalog.sha256,
        &bindings.sha256,
        terrain_memory_mib,
    )?;
    if cancelled() {
        return Ok(None);
    }
    // Check the recipe and its catalog pins before either kind of reuse.
    project.validate()?;
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    let timing_enabled = std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some();
    let result = match VARIANTS.request(variant_key, base) {
        variants::Request::Cached(source) => {
            if timing_enabled {
                eprintln!("Terrain variant cache hit");
            }
            Ok(Some(source))
        }
        variants::Request::Wait(waiter) => {
            reporter.stage("Waiting for shared terrain");
            if timing_enabled {
                eprintln!("Joined in-flight terrain variant");
            }
            waiter.wait(cancelled)
        }
        variants::Request::Build(builder) => {
            if timing_enabled {
                eprintln!("Terrain variant cache miss");
            }
            let result = build_variant(
                BuildInputs { source, project, catalog, bindings, worker, erosion: &erosion, resources },
                timing,
                &reporter,
                &|| builder.should_cancel(cancelled()),
            );
            builder.finish(&result);
            result
        }
    };
    if cancelled() {
        Ok(None)
    } else {
        result.map(|source| source.map(Cow::Owned))
    }
}

struct BuildInputs<'a> {
    source: &'a Source,
    project: &'a Project,
    catalog: &'a Catalog,
    bindings: &'a Bindings,
    worker: Option<&'a Path>,
    erosion: &'a erosion_source::Source,
    resources: generation_resources::Limits,
}

fn build_variant(
    inputs: BuildInputs<'_>,
    mut timing: Option<crate::composition::StageTimer>,
    reporter: &crate::loading_progress::Reporter,
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<Option<Source>, String> {
    let BuildInputs { source, project, catalog, bindings, worker, erosion, resources } = inputs;
    let base = source.native_base_arc()?;
    let root = bindings.installation_root();
    let terrain_memory_mib = resources.terrain_memory_mib;
    if project.gpu_erosion.is_some()
        && let Err(error) = crate::erosion_compute::check_buffer_allowance(
            base.height_resolution,
            generation_resources::current().gpu_memory_mib as u64 * 1024 * 1024,
        )
        && !crate::terrain_cache::erosion::may_have_checkpoint(base.height.len())
    {
        return Err(error);
    }
    crate::composition::record_timing(&mut timing, "prepared_inputs");
    reporter.stage("Placing terrain monuments");
    let Some(mut early) = crate::terrain_placement::generate(
        project,
        base,
        catalog,
        bindings,
        erosion,
        cancelled,
        |done, total| reporter.update("", done as u64, Some(total as u64), crate::loading_progress::Unit::Items),
    )?
    else {
        return Ok(None);
    };
    if early.recipe != *project {
        return Err("Resolve installation terrain defaults before generation".into());
    }
    crate::composition::record_timing(&mut timing, "terrain_monuments");
    let (painting, checkpoint) = if erosion.plan().erosion.active {
        let retained = early.layers.bytes() + early.alpha.len();
        let maximum_bytes = (terrain_memory_mib * 1024 * 1024).checked_sub(retained)
            .ok_or("Terrain processing exceeds its memory allowance. Increase Terrain memory in Settings.")?;
        let Some(eroded) = crate::terrain_cache::erosion::prepare(
            erosion,
            &early.layers,
            erosion_pipeline::Limits {
                maximum_bytes,
                maximum_work: 64_000_000_000,
            },
            project.gpu_erosion.as_ref(),
            cancelled,
        )?
        else {
            return Ok(None);
        };
        early.layers.height = eroded.height;
        (Some(eroded.painting), eroded.checkpoint)
    } else {
        (None, None)
    };
    crate::composition::record_timing(&mut timing, "erosion_ready");
    let key = project.terrain_key();
    let prepared = Source {
        terrain: None,
        native_base: Some(base.clone()),
        rivers: None,
        palette: source.palette.clone(),
        key: crate::native_preview::source_key(&key),
        // Composition reads the grids. The viewport and CLI render from the accepted world.
        image: eframe::egui::ColorImage::new([0, 0], Vec::new()),
        heights: Arc::new(
            early
                .layers
                .sample_heights(crate::native_preview::RESOLUTION)?,
        ),
        layers: Arc::new(early.layers),
    };
    let default_worker;
    let worker = match worker {
        Some(worker) => worker,
        None => {
            default_worker = crate::native_preview::worker_path()?;
            &default_worker
        }
    };
    let Some(processed) = std::thread::scope(|scope| {
        // Rivers read the eroded layers and produce their own layers. The writer can
        // borrow the original height and delta during that work without copying either.
        let writer = checkpoint.zip(painting.as_ref()).and_then(|(checkpoint, painting)| {
            if resources.effective_workers() == 1 {
                reporter.stage("Saving erosion checkpoint");
                checkpoint.write(&prepared.layers.height, painting, cancelled);
                return None;
            }
            let layers = Arc::clone(&prepared.layers);
            match std::thread::Builder::new().spawn_scoped(scope, move || {
                checkpoint.write(&layers.height, painting, cancelled);
            }) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    if std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some() {
                        eprintln!("Could not start erosion checkpoint writer: {error}");
                    }
                    None
                }
            }
        });
        let result = (|| {
            let Some(mut derived) = crate::native_processing::derive_rivers(
                root,
                worker,
                &key,
                prepared,
                cancelled,
                |label, _| reporter.stage(label),
            )?
            else {
                return Ok(None);
            };
            crate::composition::record_timing(&mut timing, "rivers");
            derived.key = crate::native_preview::processing_key(&key);
            let processed = crate::native_processing::process_with_cancel(
                &key,
                derived,
                true,
                cancelled,
                |label, _| reporter.stage(label),
            )?;
            crate::composition::record_timing(&mut timing, "water_classification");
            Ok::<_, String>(processed)
        })();
        if let Some(writer) = writer {
            if matches!(&result, Ok(Some(_))) && !cancelled() && !writer.is_finished() {
                reporter.stage("Saving erosion checkpoint");
            }
            let mut timing = crate::composition::StageTimer::start("erosion_checkpoint_join");
            writer.join().expect("Erosion checkpoint writer panicked");
            crate::composition::record_timing(&mut timing, "joined");
        }
        result
    })? else {
        return Ok(None);
    };
    let mut prepared = processed.source;
    prepared.terrain = Some(Arc::new(Prepared {
        early: CanonicalPlacement {
            solution: early.solution,
            proposals: early
                .proposals
                .into_iter()
                .map(|(id, [x, z, yaw])| (id, RawProposal { x, z, yaw }))
                .collect(),
        },
        alpha: early.alpha,
        painting,
        paint_active: erosion.plan().materials.active,
        ocean: processed.ocean,
    }));
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(prepared))
}

pub(crate) fn solve(
    source: &Source,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<CanonicalPlacement>, String> {
    match &source.terrain {
        Some(terrain) => placement_preview::solve_native_phase(
            project,
            &source.layers,
            catalog,
            bindings,
            &terrain.early,
            cancelled,
        ),
        None => placement_preview::solve_native_canonical(
            project,
            &source.layers,
            catalog,
            bindings,
            cancelled,
        ),
    }
}

pub(crate) fn already_stamped(
    source: &Source,
    id: u64,
    pose: &Option<papyrus_terrain::placement::Position>,
) -> Result<bool, String> {
    let Some(earlier) = source
        .terrain
        .as_ref()
        .and_then(|t| t.early.solution.outcomes.iter().find(|o| o.id == id))
    else {
        return Ok(false);
    };
    if earlier.position != *pose {
        return Err("Early terrain placement changed after terrain preparation".into());
    }
    Ok(true)
}

pub(crate) fn paint(
    source: &Source,
    maps: &mut Composed,
    cancelled: &(impl Fn() -> bool + Sync),
) -> Result<bool, String> {
    let Some(terrain) = &source.terrain else {
        return Ok(!cancelled());
    };
    let Some(painting) = terrain.painting.as_ref().filter(|_| terrain.paint_active) else {
        return Ok(!cancelled());
    };
    let Some(painted) = generation_resources::with_cpu_pool(cancelled, |pool, _| {
        painting.paint(&maps.layers, pool, cancelled)
    })?
    else {
        return Ok(false);
    };
    maps.layers.splat = painted.output.splat;
    Ok(!cancelled())
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::placement::{Outcome, Position, Solution};
    use std::collections::BTreeMap;

    #[test]
    fn early_placements_are_preserved_and_skipped_during_later_composition() {
        let pose = Position {
            x: 10.,
            y: 20.,
            z: 30.,
            yaw: 45.,
            clearance: 60.,
        };
        let layers = Arc::new(papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 3,
            surface_resolution: 2,
            height: vec![18000; 9],
            biome: vec![0; 20],
            splat: vec![0; 32],
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        let source = Source {
            terrain: Some(Arc::new(Prepared {
                early: CanonicalPlacement {
                    solution: Solution {
                        outcomes: vec![
                            Outcome {
                                id: 1,
                                position: Some(pose.clone()),
                                distance: None,
                                within_radius: true,
                                reason: "Earlier phase".into(),
                            },
                            Outcome {
                                id: 2,
                                position: None,
                                distance: None,
                                within_radius: false,
                                reason: "Earlier unplaced target".into(),
                            },
                        ],
                        required_missing: 0,
                        attempts: 8,
                    },
                    proposals: BTreeMap::new(),
                },
                alpha: vec![255; 4],
                painting: None,
                paint_active: false,
                ocean: None,
            })),
            native_base: Some(layers.clone()),
            rivers: None,
            palette: None,
            key: Project::default().terrain_key(),
            image: eframe::egui::ColorImage::filled([1, 1], eframe::egui::Color32::BLACK),
            heights: Arc::new(vec![18000; 9]),
            layers,
        };
        let mut project = Project::default();
        let paths = [project.targets[0].prefab_path.clone().unwrap()]
            .into_iter()
            .collect();
        let stage = erosion_source::Stage {
            object_id: 1,
            order: 1,
            active: true,
        };
        let plan = erosion_source::Plan {
            engine: erosion_source::VERSION,
            profile_sha256: "p".into(),
            workflow_sha256: "w".into(),
            early_monuments: vec![],
            erosion: stage.clone(),
            river: stage.clone(),
            materials: stage,
        };
        let key = cache_key(
            &source, &project, &paths, &plan, "catalog", "bindings", 2048,
        )
        .unwrap();
        project.seeds[4] = crate::StageSeed {
            value: 7654321,
            inherited: false,
        };
        project.targets[2].x += 10.;
        assert_eq!(
            key,
            cache_key(
                &source, &project, &paths, &plan, "catalog", "bindings", 2048
            )
            .unwrap()
        );
        project.targets[0].x += 10.;
        assert_ne!(
            key,
            cache_key(
                &source, &project, &paths, &plan, "catalog", "bindings", 2048
            )
            .unwrap()
        );
        project.targets[0].x -= 10.;
        assert_ne!(
            key,
            cache_key(
                &source, &project, &paths, &plan, "catalog", "bindings", 1024
            )
            .unwrap()
        );
        let cached = Cached { key, source: source.clone() };
        let base = source.native_base_arc().unwrap();
        assert!(cached.matches(key, base));
        let equivalent = Arc::new(base.as_ref().clone());
        assert!(!Arc::ptr_eq(base, &equivalent));
        assert!(cached.matches(key, &equivalent));
        assert!(!cached.matches([0; 32], base));
        let mut modified = equivalent;
        Arc::make_mut(&mut modified).height[0] += 1;
        assert!(!cached.matches(key, &modified));

        let hash = |project: &Project| {
            cache_key(&source, project, &paths, &plan, "catalog", "bindings", 2048).unwrap()
        };
        let mut gpu_recipe = project.clone();
        gpu_recipe.gpu_erosion = Some(crate::erosion_compute::Identity {
            engine: crate::erosion_compute::VERSION.into(),
            backend: "Vulkan".into(),
            vendor_id: 1, device_id: 2, name: "Test adapter".into(),
            driver: "Test driver".into(), driver_info: "1".into(),
        });
        assert_ne!(hash(&project), hash(&gpu_recipe));
        let vulkan = hash(&gpu_recipe);
        gpu_recipe.gpu_erosion.as_mut().unwrap().backend = "Dx12".into();
        assert_ne!(vulkan, hash(&gpu_recipe));
        let dx12 = hash(&gpu_recipe);
        gpu_recipe.gpu_erosion.as_mut().unwrap().driver_info = "2".into();
        assert_ne!(dx12, hash(&gpu_recipe));
        project.roads = Some(infrastructure::Recipe {
            staged: Some(crate::road_generation::Policy {
                engine: crate::road_generation::CONNECTED_SOURCE_VERSION.into(),
                roadside_source_sha256: "a".repeat(64),
                layout: None,
                roadside_preferences: Vec::new(),
            }),
            ..infrastructure::Recipe::new("b".repeat(64))
        });
        project.objects = Some(crate::object_generation::Recipe::new("c".repeat(64)));
        project.rails = Some(crate::rail_generation::Recipe {
            engine: crate::rail_generation::BEFORE_ROADS_VERSION.into(),
            source_sha256: "d".repeat(64),
            mesh_sha256: "e".repeat(64),
            settings: crate::rail_generation::initial_settings(project.master_seed),
        });
        let prepared_key = hash(&project);
        let mut edited = project.clone();
        let roads = edited.roads.as_mut().unwrap();
        roads.maximum_cut_fill += 1.;
        roads.staged.as_mut().unwrap().roadside_preferences.push(
            crate::road_generation::RoadsidePreference {
                request: 1,
                prefab: "assets/roadside.prefab".into(),
                position: [100., 200.],
                yaw: 45.,
                max_displacement_m: None,
                presence: None,
            },
        );
        edited.rails.as_mut().unwrap().settings =
            crate::rail_generation::initial_settings(project.master_seed + 1);
        edited.objects.as_mut().unwrap().maximum_terrain_work += 1;
        assert_eq!(prepared_key, hash(&edited));
        assert!(Cached { key: prepared_key, source: source.clone() }.matches(hash(&edited), base));

        for change in 0..5 {
            let mut edited = project.clone();
            let roads = edited.roads.as_mut().unwrap();
            match change {
                0 => roads.include_trails = Some(false),
                1 => roads.source_sha256 = "1".repeat(64),
                2 => roads.staged.as_mut().unwrap().engine = crate::road_generation::VERSION.into(),
                3 => {
                    roads.entrance_launch = Some(papyrus_terrain::road_network::LaunchPolicy {
                        engine: papyrus_terrain::road_network::LaunchEngine::Checked,
                        minimum_run_widths: 1.,
                        sample_spacing: 2.,
                        maximum_work: 4096,
                    });
                }
                _ => edited.objects.as_mut().unwrap().road_source_sha256 = "2".repeat(64),
            }
            assert_ne!(prepared_key, hash(&edited), "Early input {change} must invalidate the cache");
        }
        assert!(already_stamped(&source, 1, &Some(pose.clone())).unwrap());
        assert!(already_stamped(&source, 2, &None).unwrap());
        assert!(!already_stamped(&source, 3, &Some(pose.clone())).unwrap());
        assert!(already_stamped(&source, 1, &None).is_err());
        assert!(already_stamped(&source, 2, &Some(pose.clone())).is_err());
        assert!(already_stamped(&source, 1, &Some(Position { yaw: 90., ..pose })).is_err());
    }

    #[test]
    fn ordered_terrain_requires_a_native_recipe_with_the_source_priority_revision() {
        let mut project = Project::default();
        assert!(!enabled(&project));
        project.monument_rules = Some(
            serde_json::from_value(serde_json::json!({
                "engine": crate::monument_source::VERSION, "source_sha256": "a".repeat(64),
                "surface_modifiers": crate::monument_source::SOURCE_PRIORITY_VERSION,
            }))
            .unwrap(),
        );
        assert!(!enabled(&project));
        project.preview_engine = crate::native_preview::VERSION.into();
        assert!(enabled(&project));
        project.monument_rules.as_mut().unwrap().surface_modifiers = None;
        assert!(!enabled(&project));
    }
}
