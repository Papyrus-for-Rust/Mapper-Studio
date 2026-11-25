//! Source-bound underground generation between the rail ring and later rail passes.
use crate::{
    Project, composition, environment_provider as provider, environment_runtime, installation,
    monument_source, prefab_assets::Catalog, prefab_textures::Bindings, rail_generation,
    tunnel_source,
};
use papyrus_terrain::{
    dungeon_base::Pose,
    environment::{self, Coverage, Transform, scene},
    monument_composition::Composed,
    path_terrain::Grid as Heights,
    placement::Solution,
    tunnel_depth, tunnel_entrances,
    tunnel_grid::{Direction, Grid},
    tunnel_layout, tunnel_links, tunnel_portals, tunnel_scene, tunnel_spawn, tunnel_stations,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const INITIAL: i64 = -1;
const LINKS: i64 = -2;
const CELLS: i64 = -3;
const ENVIRONMENT_WORK: usize = 128_000_000;
const TERRAIN_WORK: usize = 128_000_000;
pub(crate) const MAX_SURFACE_PORTALS: usize = 2;

/// A rejected prefab at its exact dungeon root. Another orientation or burial
/// depth can expose a different rail approach and remains eligible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PortalSite {
    prefab: u32,
    position: [u32; 3],
}
pub(crate) fn portal_site(prefab: u32, position: [f32; 3]) -> PortalSite {
    PortalSite {
        prefab,
        position: position.map(|value| if value == 0. { 0 } else { value.to_bits() }),
    }
}

/// Covers independent environment, terrain, routing and spawn grants for a
/// complete retry. Source reading and raster scans use the enclosing restamp bound.
pub(crate) fn rebuild_work_bound() -> u64 {
    16 * ENVIRONMENT_WORK as u64 + 4 * TERRAIN_WORK as u64 + 4 * 32_000_000
}

pub struct Generated {
    pub maps: Composed,
    pub source: Option<Arc<tunnel_source::Source>>,
    pub portals: Vec<rail_generation::Portal>,
    pub(crate) retry_terrain: papyrus_terrain::prefab_terrain_effects::Undo,
}

/// Own the candidate so cancellation or a rejected later phase cannot expose
/// partial terrain or environment publication to the caller.
pub fn generate(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: Composed,
    ring: Option<&rail_generation::RingPlan>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Generated>, String> {
    generate_with_exclusions(
        project, solution, catalog, bindings, maps, ring, &BTreeSet::new(), 0, None, cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_with_exclusions(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    mut maps: Composed,
    ring: Option<&rail_generation::RingPlan>,
    excluded: &BTreeSet<PortalSite>,
    layout_variant: usize,
    detour: Option<[f32; 3]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Generated>, String> {
    if cancelled() {
        return Ok(None);
    }
    let mut retry_terrain = papyrus_terrain::prefab_terrain_effects::Undo::default();
    let Some(recipe) = project
        .monument_rules
        .as_ref()
        .filter(|_| project.performance.monuments && project.options.includes_underground_tunnels())
    else {
        return Ok(Some(Generated {
            maps,
            source: None,
            portals: vec![],
            retry_terrain,
        }));
    };
    let _verification = installation::VerificationScope::default();
    let progress = crate::loading_progress::begin("Generating underground rails", None);
    let monuments = monument_source::Source::cached_shared(recipe, catalog, bindings, &cancelled)?;
    let Some(source) = tunnel_source::Source::for_recipe(
        bindings.installation_root(),
        catalog,
        recipe,
        &cancelled,
    )?
    else {
        return Ok(Some(Generated {
            maps,
            source: None,
            portals: vec![],
            retry_terrain,
        }));
    };
    let base_seed = crate::derive_seed(project.seeds[4].value, "Underground rails");
    let seed = if layout_variant == 0 {
        base_seed
    } else {
        crate::derive_seed(base_seed, &format!("Underground layout {layout_variant}"))
    };
    let mut retained = tunnel_scene::Scene {
        engine: tunnel_scene::VERSION.into(),
        source_sha256: source.sha256.clone(),
        monument_sha256: monuments.sha256.clone(),
        seed: base_seed,
        layout_fingerprint: composition::layout_fingerprint(solution),
        instances: vec![],
        report: json!({"stage_order":source.order,"game_runtime_validated":false}),
    };
    if !source.active {
        retained.report["status"] = "source_disabled".into();
        maps.tunnels = Some(retained);
        return Ok(Some(Generated {
            maps,
            source: Some(source),
            portals: vec![],
            retry_terrain,
        }));
    }
    let mut actors = river_actors(project, &maps)?;
    actors.extend(initial_actors(project, solution)?);
    let inputs = Inputs::new(&source, project.size, &actors)?;
    if inputs.entrances.is_empty() {
        retained.report["status"] = "no_entrances".into();
        maps.tunnels = Some(retained);
        return Ok(Some(Generated {
            maps,
            source: Some(source),
            portals: vec![],
            retry_terrain,
        }));
    }
    let (environment, mut world, link_query, cell_query) = world(
        project,
        solution,
        catalog,
        bindings,
        source.clone(),
        &actors,
        &maps,
        &cancelled,
    )?;
    let snapshot = world.snapshot();
    let prefix = link_query
        .query_prefix(&snapshot, ENVIRONMENT_WORK, &cancelled)?
        .map_err(|e| format!("Underground entrance world is unavailable ({e:?})"))?;
    let grid = Grid::new(project.size as f32, source.fields.cell_size)?;
    let mut blocked = vec![false; grid.count() * grid.count()];
    let stations = tunnel_stations::select(
        tunnel_stations::Request {
            grid,
            stations: &inputs.stations,
            entrances: &inputs.entrances,
            blocked: &blocked,
            seed: seed.into(),
            maximum_work: 8_000_000,
        },
        &cancelled,
    )
    .map_err(|e| format!("Underground station selection failed ({e:?})"))?;
    let roots: Vec<_> = stations
        .selections
        .iter()
        .map(|s| tunnel_layout::Station {
            cell: s.cell,
            module: &inputs.stations[s.station].cell,
        })
        .collect();
    let entrances: Vec<_> = stations
        .selections
        .iter()
        .map(|s| tunnel_entrances::Entrance {
            station_cell: s.cell,
            entrance: inputs.entrances[s.entrance].end,
            station_socket: inputs.station_links[s.station].up,
        })
        .collect();
    let tunnels = inputs.cells(&source, "TunnelFolder")?;
    let transitions = inputs.cells(&source, "TransitionFolder")?;
    let assemble_layout = |blocked: &[bool]| {
        tunnel_layout::assemble(
            tunnel_layout::Request {
                grid,
                tunnels: &tunnels,
                stations: &roots,
                transitions: &transitions,
                blocked,
                seed: seed.into(),
                maximum_work: 16_000_000,
            },
            &cancelled,
        )
    };
    let mut avoided = None;
    if let Some(position) = detour {
        let cell = grid.cell(position)?;
        // Station exits must stay usable. Only reroute free tunnel cells.
        if !roots.iter().any(|station| {
            station.cell == cell
                || Direction::ALL.into_iter().any(|direction| {
                    station.module.connections[direction as usize] != 0
                        && grid.neighbour(station.cell, direction) == Some(cell)
                })
        }) {
            let index = cell[1] * grid.count() + cell[0];
            blocked[index] = true;
            avoided = Some(index);
        }
    }
    let layout = match assemble_layout(&blocked) {
        Err(
            tunnel_layout::Error::Routing(papyrus_terrain::tunnel_network::Failure::Disconnected)
            | tunnel_layout::Error::MissingCell(_)
            | tunnel_layout::Error::MissingTransition(_, _),
        ) if avoided.is_some() => {
            blocked[avoided.take().unwrap()] = false;
            assemble_layout(&blocked)
        }
        result => result,
    };
    let mut layout = layout.map_err(|e| format!("Underground routing failed ({e:?})"))?;
    let link_policy = tunnel_links::Policy {
        default_type: 0,
        elevator_type: 1,
        transition_type: 2,
        link_radius: source.fields.link_radius,
        link_transition: source.fields.link_transition,
    };
    // Source::prepare accepts the reviewed gameplay profiles. These predicates
    // model that program; installation fields supply authored dimensions.
    let depth_policy = tunnel_depth::Policy {
        underground: 1,
        train_tunnels: 32,
        entrance: 4096,
        forbidden: 3,
        altitude: 0.,
        terrain_raise: 9.,
        collision_raise: source.fields.link_transition + 1.,
        step: 3.,
        maximum_drop: 1000.,
    };
    let height = Heights {
        size: project.size as f32,
        resolution: maps.layers.height_resolution,
        origin_y: -500.,
        vertical_span: 1000.,
        height: &maps.layers.height,
    };
    let assembly = tunnel_entrances::assemble(
        tunnel_entrances::Request {
            burial: tunnel_depth::Request {
                placements: &layout.placements,
                geometry: &inputs.geometry,
                terrain: Some(height),
                world: prefix.world(),
                policy: depth_policy,
                maximum_work: ENVIRONMENT_WORK,
            },
            initial_offset: 0.,
            entrances: &entrances,
            modules: &inputs.links,
            policy: link_policy,
            seed: seed.into(),
            maximum_segments: 16384,
        },
        &cancelled,
    )
    .map_err(|e| format!("Underground entrance assembly failed ({e:?})"))?;
    let occupied: Vec<_> = project
        .targets
        .iter()
        .map(|t| t.instance_id)
        .chain(actors.iter().map(|actor| actor.id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let plan = |layout: &tunnel_layout::Layout| {
        tunnel_spawn::plan(
            tunnel_spawn::Request {
                grid,
                layout,
                entrances: &assembly,
                occupied: &occupied,
                first_instance: 1,
                maximum_work: 8_000_000,
            },
            &cancelled,
        )
        .map_err(|e| format!("Underground spawn planning failed ({e:?})"))
    };
    let first = plan(&layout)?;
    let link_actors = inputs.actors(&first, false)?;
    let link_request = environment.request(snapshot.context(), LINKS, &link_actors)?;
    let link_report = environment
        .publish_tunnels_recording(
            &mut world,
            &link_request,
            &prefix,
            &mut maps.layers,
            provider::TunnelLimits {
                environment_work: ENVIRONMENT_WORK,
                terrain_work: TERRAIN_WORK,
            },
            ring.is_some().then_some(&mut retry_terrain),
            &cancelled,
        )
        .map_err(|e| format!("Underground link publication failed ({e:?})"))?;
    progress
        .reporter()
        .stage("Placing underground rail entrances");
    let snapshot = world.snapshot();
    let prefix = cell_query
        .query_prefix(&snapshot, ENVIRONMENT_WORK, &cancelled)?
        .map_err(|e| format!("Underground portal world is unavailable ({e:?})"))?;
    let checks = tunnel_source::PortalChecks::new(
        &source,
        maps.layers.view(),
        prefix.world(),
        depth_policy,
    )?;
    let rail_access = ring
        .map(|ring| {
            let settings = project
                .rails
                .as_ref()
                .ok_or("Tunnel rail checkpoint has no recipe")?
                .settings()?;
            let shape = ring.source().profile.bed_shape()?;
            let approaches = ring.source().profile.approaches.as_ref()
                .ok_or("Tunnel rail checkpoint has no approach policy")?;
            Ok::<_, String>(tunnel_source::PortalRailAccess {
                rail_type: ring.source().profile.rail_type,
                maximum_grade: settings.bed.grade_fit
                    .ok_or("Tunnel rail checkpoint has no grade limit")?.maximum_grade,
                extension_spacing: approaches.extension_spacing,
                minimum_extension_steps: approaches.minimum_extension_steps,
                half_width: shape.width * 0.5 + shape.outer_padding,
                maximum_cut: papyrus_terrain::rail_bed_fit::control_allowance(
                    settings.bed.maximum_cut_fill,
                    shape.terrain_offset,
                ),
            })
        })
        .transpose()?;
    let sites: Vec<_> = layout
        .placements
        .iter()
        .filter(|p| !matches!(p.role, tunnel_layout::Role::Transition(_)))
        .map(|p| {
            let prefab = inputs.prefabs[&p.prefab_id].1;
            tunnel_portals::Site {
                placement: p,
                module: prefab.cell.as_ref().unwrap(),
                replaceable: prefab.replaceable,
            }
        })
        .collect();
    let portals: Vec<_> = source.selectors["UpwardsFolder"]
        .prefabs
        .iter()
        .map(|path| {
            let prefab = &source.prefabs[path];
            tunnel_portals::Portal {
                module: prefab.cell.as_ref().unwrap(),
                surface_offset: prefab.surface_offset.unwrap_or([0.; 3]),
            }
        })
        .collect();
    let rails: Vec<_> = ring.map(|r| r.path().points()).into_iter().collect();
    let mut detail = None;
    let mut rail_detail = None;
    let mut rail_rejections = 0;
    let selection = tunnel_portals::select_with_constraints(
        tunnel_portals::Request {
            sites: &sites,
            portals: &portals,
            rails: &rails,
            vertical_offset: assembly.vertical_offset,
            policy: tunnel_portals::Policy {
                minimum_height: 1.,
                minimum_rail_distance: 200.,
                minimum_spacing: 1500.,
                maximum_portals: MAX_SURFACE_PORTALS,
            },
            seed: seed.into(),
            maximum_work: ENVIRONMENT_WORK,
        },
        |id, position, remaining| {
            checks
                .check(id, position, remaining, &cancelled)
                .map(|r| (r.rejection.is_none(), r.work))
                .map_err(|e| {
                    let failure = match &e {
                        tunnel_source::PortalError::Work(failure) => failure.clone(),
                        _ => papyrus_terrain::tunnel_network::Failure::Invalid(
                            "Portal eligibility failed",
                        ),
                    };
                    detail = Some(format!("{e:?}"));
                    failure
                })
        },
        |id, position, remaining| {
            if excluded.contains(&portal_site(id, position)) {
                return Ok((false, 1));
            }
            let Some(policy) = rail_access else {
                return Ok((true, 0));
            };
            checks
                .check_rail_access(id, position, policy, remaining, &cancelled)
                .map(|result| {
                    let accepted = result.rejection.is_none();
                    rail_rejections += usize::from(!accepted);
                    (accepted, result.work)
                })
                .map_err(|error| {
                    rail_detail = Some(format!("{error:?}"));
                    match error {
                        tunnel_source::PortalError::Work(failure) => failure,
                        _ => papyrus_terrain::tunnel_network::Failure::Invalid(
                            "Portal rail access failed",
                        ),
                    }
                })
        },
        &cancelled,
    )
    .map_err(|e| format!("Underground portal selection failed ({e:?}; {detail:?}; {rail_detail:?})"))?;
    drop(checks);
    let replacements: Vec<_> = selection
        .selections
        .iter()
        .map(|s| tunnel_layout::Replacement {
            cell: s.cell,
            original_prefab_id: s.original_prefab_id,
            module: inputs.prefabs[&s.prefab_id].1.cell.as_ref().unwrap(),
        })
        .collect();
    let definitions: Vec<_> = source
        .prefabs
        .values()
        .filter_map(|p| p.cell.as_ref())
        .collect();
    layout = tunnel_layout::replace(
        grid,
        &layout,
        &replacements,
        &definitions,
        &transitions,
        32_000_000,
        &cancelled,
    )
    .map_err(|e| format!("Underground portal transition rebuilding failed ({e:?})"))?;
    let final_plan = plan(&layout)?;
    let first_links = first
        .spawns
        .iter()
        .filter(|s| !matches!(s.origin, tunnel_spawn::Origin::Grid(_)));
    let final_links = final_plan
        .spawns
        .iter()
        .filter(|s| !matches!(s.origin, tunnel_spawn::Origin::Grid(_)));
    if !first_links.eq(final_links) {
        return Err("Underground replacement changed the published entrance chains".into());
    }
    let cell_actors = inputs.actors(&final_plan, true)?;
    let cell_request = environment.request(snapshot.context(), CELLS, &cell_actors)?;
    let cell_report = environment
        .publish_tunnels_recording(
            &mut world,
            &cell_request,
            &prefix,
            &mut maps.layers,
            provider::TunnelLimits {
                environment_work: ENVIRONMENT_WORK,
                terrain_work: TERRAIN_WORK,
            },
            ring.is_some().then_some(&mut retry_terrain),
            &cancelled,
        )
        .map_err(|e| format!("Underground cell publication failed ({e:?})"))?;
    let mut generated_portals = Vec::new();
    for spawn in &final_plan.spawns {
        let (path, prefab) = inputs.prefabs[&spawn.prefab_id];
        let surface_portal = source.selectors["UpwardsFolder"].prefabs.contains(path);
        if surface_portal {
            generated_portals.push(rail_generation::Portal {
                instance: spawn.instance,
                prefab: path.clone(),
                pose: spawn.pose,
            });
        }
        let surface_position = if surface_portal {
            Some(
                spawn.pose.point(
                    prefab
                        .surface_offset
                        .ok_or("Generated portal has no surface marker frame")?,
                ),
            )
        } else {
            None
        };
        retained.instances.push(tunnel_scene::Instance {
            instance: spawn.instance,
            prefab: path.clone(),
            prefab_id: prefab.id,
            pose: spawn.pose,
            surface_position,
        });
    }
    retained.report = json!({"status":"generated","stage_order":source.order,"stations":stations.selections.len(),
        "vertical_offset":assembly.vertical_offset,"routing_work":layout.work,"entrance_work":assembly.work,
        "portal_candidates":selection.candidates,"portal_work":selection.work,"surface_portals":generated_portals.len(),"portal_rail_access":rail_access,"portal_rail_rejections":rail_rejections,
        "spawn_work":final_plan.work,"network":{"cell_size":source.fields.cell_size,"count":grid.count(),"connections":layout.network.connections,"vertical_offset":assembly.vertical_offset},"links":link_report,"cells":cell_report,"environment":world.snapshot().summary(),
        "game_runtime_validated":false});
    if let Some(index) = avoided {
        retained.report["rerouted_cell"] = json!([index % grid.count(), index / grid.count()]);
    }
    if layout_variant != 0 {
        retained.report["layout_variant"] = layout_variant.into();
        retained.report["layout_seed"] = seed.into();
    }
    maps.tunnels = Some(retained);
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Generated {
        maps,
        source: Some(source),
        portals: generated_portals,
        retry_terrain,
    }))
}

/// Allocate river roots before dungeon occurrences, preserving upstream order.
/// Export uses the same assignment to validate world-global occurrence IDs.
pub(crate) fn river_actors(
    project: &Project,
    maps: &Composed,
) -> Result<Vec<provider::Occurrence>, String> {
    river_occurrences(
        project,
        maps.rivers
            .as_ref()
            .and_then(|world| world.objects.as_ref()),
    )
}

fn river_occurrences(
    project: &Project,
    objects: Option<&papyrus_terrain::river_pipeline::Objects>,
) -> Result<Vec<provider::Occurrence>, String> {
    let Some(objects) = objects else {
        return Ok(vec![]);
    };
    let recipe = project
        .rivers
        .as_ref()
        .and_then(|recipe| recipe.objects.as_ref())
        .ok_or("Generated river objects have no source recipe")?;
    if objects.source_sha256 != recipe.source_sha256 || objects.instances.len() > 65536 {
        return Err("Generated river objects differ from the accepted source".into());
    }
    let reserved: BTreeSet<_> = project
        .targets
        .iter()
        .map(|target| target.instance_id)
        .collect();
    let mut next = 1u64;
    let mut actors = Vec::with_capacity(objects.instances.len());
    for object in &objects.instances {
        while reserved.contains(&next) {
            next = next
                .checked_add(1)
                .ok_or("River occurrence identities exhausted")?;
        }
        let pose = Transform {
            position: object.position,
            rotation: object.rotation,
            scale: object.scale,
        };
        pose.validate()?;
        actors.push(provider::Occurrence {
            id: next,
            prefab: object.prefab.clone(),
            pose,
            lifecycle: environment_runtime::Lifecycle::ActivatedAuthored,
        });
        next = next
            .checked_add(1)
            .ok_or("River occurrence identities exhausted")?;
    }
    Ok(actors)
}

fn initial_actors(
    project: &Project,
    solution: &Solution,
) -> Result<Vec<provider::Occurrence>, String> {
    let targets: BTreeMap<_, _> = project.targets.iter().map(|t| (t.instance_id, t)).collect();
    solution
        .outcomes
        .iter()
        .filter_map(|o| o.position.as_ref().map(|p| (o, p)))
        .map(|(o, p)| {
            let prefab = targets
                .get(&o.id)
                .and_then(|t| t.prefab_path.as_ref())
                .ok_or("Placed monument has no source prefab")?;
            let angle = -p.yaw.to_radians() * 0.5;
            Ok(provider::Occurrence {
                id: o.id,
                prefab: prefab.clone(),
                pose: Transform {
                    position: [p.x, p.y, p.z],
                    rotation: [0., angle.sin(), 0., angle.cos()],
                    scale: [1.; 3],
                },
                lifecycle: environment_runtime::Lifecycle::ActivatedAuthored,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn world(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    tunnels: Arc<tunnel_source::Source>,
    actors: &[provider::Occurrence],
    maps: &Composed,
    cancelled: &impl Fn() -> bool,
) -> Result<
    (
        provider::Source,
        scene::Scene,
        provider::Request,
        provider::Request,
    ),
    String,
> {
    let runtime = environment_runtime::Verified::available(
        bindings.installation_root(),
        &catalog.installation,
        cancelled,
    )?
    .ok_or("This Rust build has no underground environment execution profile")?;
    let rules = project.monument_rules.as_ref().unwrap();
    let path = crate::client_cache::pinned_document(
        &crate::storage::local_data()?,
        "papyrus-monument-cache",
        &rules.source_sha256,
        "monuments.json",
    )?;
    let mut source = provider::Source::load(
        runtime,
        catalog,
        bindings,
        &path,
        &rules.source_sha256,
        cancelled,
    )?
    .ok_or("Underground environment loading cancelled")?
    .with_monument_geometry(catalog, bindings, cancelled)?
    .ok_or("Underground monument geometry binding cancelled")?
    .with_tunnels(tunnels, cancelled)?
    .ok_or("Underground environment binding cancelled")?;
    if maps
        .rivers
        .as_ref()
        .and_then(|world| world.objects.as_ref())
        .is_some_and(|objects| !objects.instances.is_empty())
    {
        let recipe = project
            .rivers
            .as_ref()
            .and_then(|recipe| recipe.objects.as_ref())
            .ok_or("River environment recipe is unavailable")?;
        source = source
            .with_river_objects(catalog, recipe, cancelled)?
            .ok_or("River environment binding cancelled")?;
    }
    let raw = serde_json::to_vec(&(
        tunnel_scene::VERSION,
        project,
        composition::layout_fingerprint(solution),
    ))
    .map_err(|e| e.to_string())?;
    let context = source.context(format!("{:x}", Sha256::digest(raw)))?;
    let initial = source.request(&context, INITIAL, actors)?;
    let links = source.request(&context, LINKS, &[])?;
    let cells = source.request(&context, CELLS, &[])?;
    let mut scene = scene::Scene::new_ordered(
        context,
        vec![
            initial.requirement(vec![]),
            links.requirement(vec![INITIAL]),
            cells.requirement(vec![LINKS]),
        ],
        vec![INITIAL, LINKS, CELLS],
        Coverage::Complete,
    )?;
    let update = source
        .prepare_update(&scene.snapshot(), &initial, ENVIRONMENT_WORK, cancelled)?
        .map_err(|e| format!("Monument environment registration failed ({e:?})"))?;
    if !scene.publish(update.into_parts().0, cancelled)? {
        return Err("Underground environment registration cancelled".into());
    }
    Ok((source, scene, links, cells))
}

struct Inputs<'a> {
    prefabs: BTreeMap<u32, (&'a String, &'a tunnel_source::Prefab)>,
    stations: Vec<tunnel_stations::Station>,
    station_links: Vec<&'a tunnel_links::Module>,
    entrances: Vec<tunnel_stations::Entrance>,
    links: Vec<tunnel_links::Module>,
    geometry: BTreeMap<u32, Vec<tunnel_depth::Volume>>,
}
impl<'a> Inputs<'a> {
    fn new(
        source: &'a tunnel_source::Source,
        size: u32,
        actors: &[provider::Occurrence],
    ) -> Result<Self, String> {
        let mut prefabs = BTreeMap::new();
        for (path, prefab) in &source.prefabs {
            if prefab.id == 0 || prefabs.insert(prefab.id, (path, prefab)).is_some() {
                return Err("Duplicate tunnel prefab identity".into());
            }
        }
        let mut stations = Vec::new();
        let mut station_links = Vec::new();
        for path in &source.selectors["StationFolder"].prefabs {
            let prefab = &source.prefabs[path];
            let link = prefab
                .link
                .as_ref()
                .ok_or("Tunnel station has no upper link socket")?;
            stations.push(tunnel_stations::Station {
                cell: prefab.cell.clone().ok_or("Tunnel station has no cell")?,
                volume: prefab.volume.ok_or("Tunnel station has no volume")?,
                up_position: link.up.pose.position,
            });
            station_links.push(link);
        }
        let mut entrances = Vec::new();
        for actor in actors {
            let Some(prefab) = source.prefabs.get(&actor.prefab) else {
                continue;
            };
            let Some(entrance) = prefab.entrance.as_ref() else {
                continue;
            };
            let root = Pose {
                position: actor.pose.position,
                rotation: actor.pose.rotation,
            };
            let position = root.point(entrance.frame.position);
            let snapped = entrance.snap.position(position)?;
            if position
                .iter()
                .zip(snapped)
                .any(|(a, b)| (*a - b).abs() > 0.01)
            {
                return Err(
                    "Placed tunnel entrance was not snapped before terrain composition".into(),
                );
            }
            let volume = prefab
                .volume
                .ok_or("Tunnel entrance has no placement volume")?;
            if !tunnel_stations::valid_spawn(
                Grid::new(size as f32, entrance.cell_size)?,
                volume,
                snapped,
            )? {
                return Err("Placed tunnel entrance intersects a dungeon station cell".into());
            }
            entrances.push(tunnel_stations::Entrance {
                end: tunnel_links::End {
                    socket: prefab
                        .link
                        .as_ref()
                        .ok_or("Tunnel entrance has no lower socket")?
                        .down,
                    pose: Pose {
                        position: snapped,
                        rotation: environment::multiply(root.rotation, entrance.frame.rotation),
                    },
                },
                connector: root.point(entrance.connector.position),
                volume,
            });
        }
        let links = source.selectors["LinkFolder"]
            .prefabs
            .iter()
            .map(|p| {
                source.prefabs[p]
                    .link
                    .clone()
                    .ok_or("Missing tunnel link module".into())
            })
            .collect::<Result<Vec<_>, String>>()?;
        let geometry = prefabs
            .iter()
            .filter(|(_, (_, p))| p.cell.is_some())
            .map(|(&id, (_, p))| (id, p.environments.clone()))
            .collect();
        Ok(Self {
            prefabs,
            stations,
            station_links,
            entrances,
            links,
            geometry,
        })
    }
    fn cells(
        &self,
        source: &'a tunnel_source::Source,
        role: &str,
    ) -> Result<Vec<&'a papyrus_terrain::tunnel_grid::Cell>, String> {
        source
            .selectors
            .get(role)
            .ok_or("Missing tunnel module pool")?
            .prefabs
            .iter()
            .map(|p| {
                source.prefabs[p]
                    .cell
                    .as_ref()
                    .ok_or("Missing tunnel cell definition".into())
            })
            .collect()
    }
    fn actors(
        &self,
        plan: &tunnel_spawn::Plan,
        grid: bool,
    ) -> Result<Vec<provider::Occurrence>, String> {
        plan.spawns
            .iter()
            .filter(|s| matches!(s.origin, tunnel_spawn::Origin::Grid(_)) == grid)
            .map(|s| {
                let (path, _) = self
                    .prefabs
                    .get(&s.prefab_id)
                    .ok_or("Spawned tunnel prefab is outside the installed pool")?;
                Ok(provider::Occurrence {
                    id: s.instance,
                    prefab: (*path).clone(),
                    pose: Transform {
                        position: s.pose.position,
                        rotation: s.pose.rotation,
                        scale: [1.; 3],
                    },
                    lifecycle: environment_runtime::Lifecycle::ActivatedAuthored,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn portal_recovery_keys_bind_prefabs_and_world_roots_and_normalize_signed_zero() {
        assert_eq!(super::portal_site(1, [0., 0., -0.]), super::portal_site(1, [-0., 0., 0.]));
        assert_ne!(super::portal_site(1, [1., 0., 2.]), super::portal_site(2, [1., 0., 2.]));
        assert_ne!(super::portal_site(1, [1., 0., 2.]), super::portal_site(1, [1., 1., 2.]));
        assert_ne!(super::portal_site(1, [1., 0., 2.]), super::portal_site(1, [1.001, 0., 2.]));
    }
    use super::*;
    use papyrus_terrain::river_pipeline::{Object, Objects};

    #[test]
    fn river_roots_preserve_frames_and_reserve_all_target_identities() {
        let mut project = Project::default();
        let expected_first = (1..)
            .find(|id| !project.targets.iter().any(|t| t.instance_id == *id))
            .unwrap();
        project.rivers = Some(crate::river_generation::Recipe {
            engine: papyrus_terrain::river_pipeline::VERSION.into(),
            source_sha256: "a".repeat(64),
            source_document: None,
            objects: Some(crate::river_objects::Recipe {
                source_sha256: "b".repeat(64),
                infrastructure_sha256: "c".repeat(64),
                catalog_sha256: "d".repeat(64),
                documents: None,
            }),
        });
        let mut objects = Objects {
            source_sha256: "b".repeat(64),
            seed: 3,
            fitting: json!({}),
            terrain: Default::default(),
            instances: vec![
                Object {
                    river: 0,
                    prefab: "river-rock".into(),
                    position: [2., 3., 4.],
                    rotation: [0., 0., 0., 1.],
                    scale: [2., 3., 4.],
                },
                Object {
                    river: 0,
                    prefab: "river-tree".into(),
                    position: [5., 6., 7.],
                    rotation: [0., 0., 0., 1.],
                    scale: [1.; 3],
                },
            ],
        };
        let actors = river_occurrences(&project, Some(&objects)).unwrap();
        assert_eq!(actors[0].id, expected_first);
        assert!(actors[1].id > actors[0].id);
        for (actor, object) in actors.iter().zip(&objects.instances) {
            assert!(
                !project
                    .targets
                    .iter()
                    .any(|target| target.instance_id == actor.id)
            );
            assert_eq!(actor.prefab, object.prefab);
            assert_eq!(actor.pose.position, object.position);
            assert_eq!(actor.pose.rotation, object.rotation);
            assert_eq!(actor.pose.scale, object.scale);
        }
        assert_eq!(
            river_occurrences(&project, Some(&objects)).unwrap()[1].id,
            actors[1].id
        );
        objects.source_sha256 = "e".repeat(64);
        assert!(river_occurrences(&project, Some(&objects)).is_err());
        objects.source_sha256 = "b".repeat(64);
        objects.instances[0].rotation = [0.; 4];
        assert!(river_occurrences(&project, Some(&objects)).is_err());
        project.rivers = None;
        assert!(river_occurrences(&project, Some(&objects)).is_err());
        assert!(river_occurrences(&project, None).unwrap().is_empty());
    }
}
