//! Export the accepted world through the inspected version-10 game container.
use crate::{Project, prefab_assets::Catalog};
use papyrus_terrain::{
    map_container::{self, Path as MapPath},
    monument_composition::Composed,
    placement::Solution,
    prefab_records::{self, Instance, Record},
};
use serde_json::json;
use std::{
    io::{BufWriter, Write},
    path::Path,
};

pub struct Input<'a> {
    pub project: &'a Project,
    pub maps: &'a Composed,
    pub solution: &'a Solution,
    pub root: &'a Path,
    pub catalog: &'a Catalog,
}

fn decoration_records(
    maps: &Composed,
    identities: &crate::prefab_identities::Source,
    cancel: impl Fn() -> bool,
) -> Result<Option<Vec<Record>>, String> {
    let Some(decor) = &maps.decoration else {
        return Ok(Some(Vec::new()));
    };
    let fitting = decor
        .fitting
        .as_ref()
        .ok_or("Decoration fitting is incomplete")?;
    let committed = decor
        .terrain_placement
        .as_ref()
        .ok_or("Decoration placement is incomplete")?;
    let mut inputs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for applied in &committed.applied {
        if !seen.insert(applied.pose) {
            return Err("Duplicate committed decoration pose".into());
        }
        let pose = fitting
            .poses
            .get(applied.pose)
            .ok_or("Unknown committed decoration pose")?;
        let candidate = decor
            .candidates
            .get(pose.candidate)
            .ok_or("Unknown decoration candidate")?;
        let path = decor
            .stages
            .get(candidate.stage)
            .and_then(|s| s.prefabs.get(candidate.prefab))
            .ok_or("Unknown decoration prefab")?;
        inputs.push(Instance {
            category: "Decor",
            path,
            position: pose.position,
            rotation: pose.rotation,
            scale: pose.scale,
        });
    }
    prefab_records::assemble(&inputs, |p| identities.resolve(p), cancel)
}

fn paths(
    input: &Input<'_>,
    roads: &serde_json::Value,
    cancel: impl Fn() -> bool,
) -> Result<Option<Vec<MapPath>>, String> {
    let mut paths = if input.maps.roads.is_some() {
        let Some(paths) = crate::path_export::from_snapshot(input.project, roads, &cancel)? else {
            return Ok(None);
        };
        paths
    } else {
        if input.project.roads.is_some() {
            return Err("Requested roads are missing".into());
        }
        Vec::new()
    };
    if let Some(utility) = input
        .maps
        .objects
        .as_ref()
        .and_then(|scene| scene.utility.as_ref())
    {
        if utility["engine"] != crate::powerline_generation::VERSION {
            return Err("Unknown utility generation report".into());
        }
        let utility_paths = utility["paths"]
            .as_array()
            .filter(|paths| paths.len() <= 4096)
            .ok_or("Missing utility path roster")?;
        let mut control_count = 0usize;
        for (index, path) in utility_paths.iter().enumerate() {
            if cancel() {
                return Ok(None);
            }
            if path["name"] != format!("Powerline {index}") {
                return Err("Utility path identity mismatch".into());
            }
            let nodes: Vec<[f32; 3]> = serde_json::from_value(path["points"].clone())
                .map_err(|error| error.to_string())?;
            if nodes.len() < 8 || nodes.len() > 262144 {
                return Err("Invalid utility path size".into());
            }
            control_count = control_count
                .checked_add(nodes.len())
                .filter(|count| *count <= 1_000_000)
                .ok_or("Utility path control allowance exhausted")?;
            paths.push(MapPath {
                name: format!("Powerline {index}"),
                spline: true,
                start: true,
                end: true,
                width: 0.,
                inner_padding: 0.,
                outer_padding: 0.,
                inner_fade: 0.,
                outer_fade: 0.,
                random_scale: 0.,
                mesh_offset: 0.,
                terrain_offset: 0.,
                splat: 0,
                topology: 0,
                hierarchy: 0,
                nodes,
            });
        }
    }
    if let Some(rivers) = &input.maps.rivers {
        let recipe = input
            .project
            .rivers
            .as_ref()
            .ok_or("Missing river recipe")?;
        let Some(source) = crate::river_generation::Source::cached(
            input.root,
            &input.catalog.installation,
            recipe,
            &cancel,
        )?
        else {
            return Ok(None);
        };
        if rivers.layout_policy
            != source.profile.layout_for_recipe(input.project.size, input.project.options.river_limit)
        {
            return Err("River path policies differ from the accepted source".into());
        }
        for (index, path) in rivers.paths.iter().enumerate() {
            let policy = &rivers.layout_policy;
            paths.push(MapPath {
                name: format!("River {index}"),
                spline: true,
                start: true,
                end: true,
                width: policy.width,
                inner_padding: policy.padding[0],
                outer_padding: policy.padding[1],
                inner_fade: policy.fade[0],
                outer_fade: policy.fade[1],
                random_scale: policy.random_scale,
                mesh_offset: rivers.water_policy.mesh_offset,
                terrain_offset: source.profile.carve.terrain_offset,
                splat: source
                    .profile
                    .surface
                    .splat_channel
                    .map_or(0, |v| 1i32 << v),
                topology: source.profile.surface.topology_mask,
                nodes: path.points().to_vec(),
                hierarchy: 0,
            });
        }
    } else if input.project.rivers.is_some() {
        return Err("Requested rivers are missing".into());
    }
    if let Some(rails) = &input.maps.rails {
        let recipe = input.project.rails.as_ref().ok_or("Missing rail recipe")?;
        let road_recipe = input
            .project
            .roads
            .as_ref()
            .ok_or("Missing rail infrastructure")?;
        let infrastructure =
            crate::infrastructure::Source::cached(input.catalog, road_recipe, &cancel)?;
        let path = crate::client_cache::pinned_document(
            &crate::storage::local_data()?,
            "papyrus-rail-cache",
            &recipe.source_sha256,
            "rails.json",
        )?;
        let source = crate::rail_source::Source::load(
            input.root,
            &path,
            input.catalog,
            &infrastructure,
            &cancel,
        )?;
        if source.sha256 != recipe.source_sha256 {
            return Err("Rail source differs from the accepted recipe".into());
        }
        let shape = source.profile.bed_shape()?;
        let mesh = source
            .profile
            .mesh
            .as_ref()
            .ok_or("Missing rail path policy")?;
        let sidings = rails.report["sidings"]["fitted_joins"]
            .as_array()
            .ok_or("Missing rail sidings")?;
        let flags = rails.report["meshes"]["tracks"]
            .as_array()
            .ok_or("Missing rail endpoint flags")?;
        for track in &rails.tracks {
            let flag = flags
                .iter()
                .find(|f| f["track_id"] == track.id)
                .ok_or("Missing rail path flags")?;
            let hierarchy = if sidings.iter().any(|s| s["track_id"] == track.id) {
                source
                    .profile
                    .siding
                    .as_ref()
                    .ok_or("Missing siding policy")?
                    .hierarchy as i32
            } else if track.id == 1 {
                0
            } else {
                1
            };
            paths.push(MapPath {
                name: format!("Rail {}", track.id),
                spline: mesh.spline,
                start: flag["source_start_flag"]
                    .as_bool()
                    .ok_or("Missing rail start flag")?,
                end: flag["source_end_flag"]
                    .as_bool()
                    .ok_or("Missing rail end flag")?,
                width: shape.width,
                inner_padding: shape.inner_padding,
                outer_padding: shape.outer_padding,
                inner_fade: shape.inner_fade,
                outer_fade: shape.outer_fade,
                random_scale: mesh.random_scale,
                mesh_offset: mesh.mesh_offset,
                terrain_offset: shape.terrain_offset,
                splat: 1i32 << shape.splat_channel,
                topology: shape.rail as i32,
                nodes: track.path.points().to_vec(),
                hierarchy,
            });
        }
    } else if input.project.rails.is_some() {
        return Err("Requested surface rails are missing".into());
    }
    if cancel() {
        return Ok(None);
    }
    Ok(Some(paths))
}

pub(crate) struct PreparedPrefabs {
    pub records: Vec<Record>,
    roads: serde_json::Value,
}

pub(crate) fn validate(input: &Input<'_>) -> Result<(), String> {
    crate::generation_validation::validate(input.project, input.maps, input.solution)?;
    if input.maps.layers.size != input.project.size
        || input.catalog.installation
            != *input
                .project
                .installation
                .as_ref()
                .ok_or("Missing installation")?
    {
        return Err("Map export inputs belong to different installations or map sizes".into());
    }
    if input.project.performance.monuments
        && input.project.monument_rules.is_some()
        && input.project.options.includes_underground_tunnels()
        && input.maps.tunnels.is_none()
        && crate::monument_stages::capture_code_profile(input.catalog)?.is_some()
    {
        return Err("Map export requires the completed underground rail assembly".into());
    }
    Ok(())
}

/// Shared accepted-world prefab order for export and derived scene inspection.
/// Callers validate the accepted composition before resolving source identities.
pub(crate) fn prepare_prefabs(
    input: &Input<'_>,
    identities: &crate::prefab_identities::Source,
    cancel: impl Fn() -> bool,
) -> Result<Option<PreparedPrefabs>, String> {
    let layout = json!({"outcomes":input.solution.outcomes.iter().map(|o|json!({"id":o.id,
        "position":o.position.as_ref().map(|p|json!({"x":p.x,"y":p.y,"z":p.z,"yaw":p.yaw,"clearance":p.clearance}))})).collect::<Vec<_>>()});
    let roads = input
        .maps
        .roads
        .as_ref()
        .map_or_else(|| json!({"roads":[]}), |r| r.json());
    let objects = input
        .maps
        .objects
        .as_ref()
        .map_or_else(|| Ok(json!({"instances":[]})), serde_json::to_value)
        .map_err(|e| e.to_string())?;
    let Some(mut records) = crate::prefab_export::from_snapshot(
        input.project,
        &layout,
        &roads,
        &objects,
        |p| identities.resolve(p),
        &cancel,
    )?
    else {
        return Ok(None);
    };
    let Some(decor) = decoration_records(input.maps, identities, &cancel)? else {
        return Ok(None);
    };
    records.records.extend(decor);
    let river_objects = input.maps.rivers.as_ref().and_then(|r| r.objects.as_ref());
    if let Some(scene) = river_objects {
        let river_count = input.maps.rivers.as_ref().map_or(0, |r| r.paths.len());
        let categories = (0..river_count).map(|i| format!("River {i}")).collect::<Vec<_>>();
        let instances = scene.instances.iter().map(|p| Instance {
            category: &categories[p.river], path: &p.prefab,
            position: p.position, rotation: p.rotation, scale: p.scale,
        }).collect::<Vec<_>>();
        let Some(river_records) = prefab_records::assemble(&instances, |p| identities.resolve(p), &cancel)? else {
            return Ok(None);
        };
        records.records.extend(river_records);
    }
    if let Some(rules) = input
        .project
        .monument_rules
        .as_ref()
        .filter(|_| input.project.performance.monuments)
    {
        if let Some(tunnels) = &input.maps.tunnels {
            let source = crate::tunnel_source::Source::for_export(
                input.root,
                input.catalog,
                rules,
                &tunnels.source_sha256,
                &cancel,
            )?;
            let mut occupied: std::collections::BTreeSet<_> = input
                .project
                .targets
                .iter()
                .map(|target| target.instance_id)
                .chain(
                    crate::tunnel_generation::river_actors(input.project, input.maps)?
                        .iter()
                        .map(|actor| actor.id),
                )
                .collect();
            for instance in &tunnels.instances {
                if instance.instance == 0
                    || !occupied.insert(instance.instance)
                    || source
                        .prefabs
                        .get(&instance.prefab)
                        .is_none_or(|p| p.id != instance.prefab_id)
                    || identities.resolve(&instance.prefab) != Some(instance.prefab_id)
                {
                    return Err("Underground prefab or occurrence identity differs from the installed source".into());
                }
            }
            let instances: Vec<_> = tunnels
                .instances
                .iter()
                .map(|instance| Instance {
                    category: "Dungeon",
                    path: &instance.prefab,
                    position: instance.pose.position,
                    rotation: instance.pose.rotation,
                    scale: [1.; 3],
                })
                .collect();
            let Some(tunnel_records) =
                prefab_records::assemble(&instances, |p| identities.resolve(p), &cancel)?
            else {
                return Ok(None);
            };
            records.records.extend(tunnel_records);
        } else if input.project.options.includes_underground_tunnels() && crate::tunnel_source::Source::for_recipe(
            input.root,
            input.catalog,
            rules,
            &cancel,
        )?
        .is_some()
        {
            return Err("Map export requires the completed underground rail stage".into());
        }
    }
    if let Some(dungeons) = &input.maps.dungeons {
        let instances: Vec<_> = dungeons.instances.iter().map(|instance| Instance {
            category: "DungeonBase", path: &instance.prefab,
            position: instance.pose.position, rotation: instance.pose.rotation, scale: [1.; 3],
        }).collect();
        let Some(dungeon_records) = prefab_records::assemble(&instances, |path|identities.resolve(path), &cancel)? else {
            return Ok(None);
        };
        records.records.extend(dungeon_records);
    }
    if let Some(scene) = &input.maps.offshore {
        let instances: Vec<_> = scene.instances.iter().map(|instance| Instance {
            category:"Monument",path:&instance.prefab,position:instance.pose.position,
            rotation:instance.pose.rotation,scale:instance.pose.scale,
        }).collect();
        let Some(offshore_records) = prefab_records::assemble(&instances, |path|identities.resolve(path), &cancel)? else {
            return Ok(None);
        };
        records.records.extend(offshore_records);
    }
    let options = &input.project.options;
    if !options.is_default() {
        records.records.retain(|record| !options.export_blocked(&record.path));
    }
    if input.project.performance.monuments && input.project.has_monument_addons() {
        let selected = crate::monument_addons::resolve(input.project)?;
        let Some(addons) = crate::monument_addons::assemble(
            &selected,
            &records.records,
            &identities.prefabs,
            input.catalog,
            &cancel,
        )? else {
            return Ok(None);
        };
        records.records.extend(addons.into_iter().filter(|record| !options.export_blocked(&record.path)));
    }
    if input.maps.custom_baked.len() > papyrus_terrain::content::MAX_PREFABS.saturating_sub(records.records.len()) {
        return Err("Custom monuments exceed the playable map prefab limit".into());
    }
    records.records.extend_from_slice(&input.maps.custom_baked);
    let Some(custom) = crate::custom_monuments::assemble(
        input.project, records.records.len(), &identities.prefabs, &cancel,
    )? else {
        return Ok(None);
    };
    records.records.extend(custom);
    Ok((!cancel()).then_some(PreparedPrefabs { records: records.records, roads }))
}

/// A private file is published only after every serialization step succeeds.
/// The caller selects an accepted composition and keeps it immutable throughout.
pub fn write(
    input: Input<'_>,
    destination: &Path,
    overwrite: bool,
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    write_prepared(input, destination, overwrite, None, cancel)
}

pub fn write_prepared(
    input: Input<'_>,
    destination: &Path,
    overwrite: bool,
    prepared: Option<&crate::client_preparation::ReadyIdentities>,
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    let _verification = crate::installation::VerificationScope::default();
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Some(temporary) = build_temporary_map(input, Some(parent), prepared, &cancel)? else {
        return Ok(false);
    };
    let mut timing = crate::composition::StageTimer::start("map_publication");
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    crate::composition::record_timing(&mut timing, "file_sync");
    if cancel() {
        return Ok(false);
    }
    if overwrite {
        temporary.persist(destination).map_err(|e| e.error.to_string())?;
    } else {
        temporary.persist_noclobber(destination).map_err(|e| e.error.to_string())?;
    }
    crate::composition::record_timing(&mut timing, "published");
    Ok(true)
}

/// Keep the accepted map available for the session. Saving the project or
/// exporting the map flushes its own final file.
pub fn retain(
    input: Input<'_>,
    prepared: Option<&crate::client_preparation::ReadyIdentities>,
    cancel: impl Fn() -> bool,
) -> Result<Option<tempfile::TempPath>, String> {
    let _verification = crate::installation::VerificationScope::default();
    Ok(build_temporary_map(input, None, prepared, &cancel)?
        .map(tempfile::NamedTempFile::into_temp_path))
}

fn build_temporary_map(
    input: Input<'_>,
    parent: Option<&Path>,
    identities: Option<&crate::client_preparation::ReadyIdentities>,
    cancel: &impl Fn() -> bool,
) -> Result<Option<tempfile::NamedTempFile>, String> {
    if cancel() {
        return Ok(None);
    }
    let mut timing = crate::composition::StageTimer::start("map_serialization");
    let _identities =
        crate::prefab_identities::Scope::with_prepared(input.root, input.catalog, identities);
    validate(&input)?;
    let Some(identities) =
        crate::prefab_identities::Source::prepare(input.root, input.catalog, cancel)?
    else {
        return Ok(None);
    };
    let Some(prepared) = prepare_prefabs(&input, &identities, cancel)? else {
        return Ok(None);
    };
    let records = &prepared.records;
    let roads = &prepared.roads;
    let Some(paths) = paths(&input, roads, cancel)? else {
        return Ok(None);
    };
    crate::composition::record_timing(&mut timing, "records");
    let mut temporary = match parent {
        Some(parent) => tempfile::NamedTempFile::new_in(parent),
        None => tempfile::Builder::new().prefix("mapper-map-").suffix(".map").tempfile(),
    }.map_err(|e| e.to_string())?;
    let water = if let Some(ocean) = &input.maps.ocean {
        Some(ocean.local_water.as_slice())
    } else {
        input.maps.rivers.as_ref().map(|rivers| rivers.local_water.as_slice())
    };
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        let world = map_container::World {
            layers: &input.maps.layers,
            height: input
                .maps
                .gameplay_height
                .as_ref()
                .map_or(input.maps.layers.height.as_slice(), |h| h.height.as_slice()),
            water,
            alpha: &input.maps.alpha,
            prefabs: records,
            paths: &paths,
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let cargo = if input.project.options.cargo_path.unwrap_or_default() {
            let Some(data) = crate::cargo_route::bake(
                input.project.size, world.layers.height_resolution, world.height, cancel,
            )? else { return Ok(None); };
            Some(data)
        } else {
            None
        };
        let mut metadata = Vec::with_capacity(2);
        if let Some(data) = cargo.as_deref() {
            metadata.push((crate::cargo_route::LAYER, data));
        }
        let allow_building_on_roads = input.project.options.build_on_roads.unwrap_or_default();
        if allow_building_on_roads {
            metadata.push(("papyrus.world-rules", b"v1:build-on-roads".as_slice()));
        }
        let serialized = crate::generation_resources::with_cpu_pool(cancel, |pool, _| {
            map_container::write_v10_with_options(
                world, timestamp, &mut writer, cancel, Some(pool),
                map_container::Options {
                    extra_layers: &metadata,
                    allow_building_on_roads,
                },
            ).map_err(|e| e.to_string())
        })?;
        if serialized.is_none() {
            return Ok(None);
        }
        writer.flush().map_err(|e| e.to_string())?;
    }
    crate::composition::record_timing(&mut timing, "container_written");
    Ok((!cancel()).then_some(temporary))
}
