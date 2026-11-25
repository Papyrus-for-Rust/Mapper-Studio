//! Source-bound topology/height/material study with explicit triangle-proxy water.
//! Final access, earthwork and runtime acceptance remain separate.
use mapper_studio::{
    Project, native_noise, native_profile, road_paint_source, road_paint_stage,
    road_planning::Plan, road_terrain_source, roadside_source, roadside_stamps::StampedStage,
};
use papyrus_terrain::{
    monument_composition::{self, Composed},
    road_grade::Junction,
    road_paint,
    water_mesh::Surface,
};
use sha2::{Digest, Sha256};
use std::path::Path;
pub struct Study {
    pub maps: Composed,
    pub report: serde_json::Value,
}
pub fn run(
    project: &Project,
    root: &Path,
    worker: &Path,
    source: &roadside_source::Source,
    plan: &Plan,
    stamped: &StampedStage,
) -> Result<Study, String> {
    if source.sha256 != stamped.report.roadside_source_sha256 {
        return Err("Road paint study source differs from stamped stage".into());
    }
    let profile = road_paint_source::Profile::for_source(source)?
        .ok_or("No road paint compatibility profile")?;
    if project.rails.is_some() {
        return Err("The native study needs a bound retained-rail terrain adapter before enabling a rail recipe".into());
    }
    let terrain_profile = road_terrain_source::Profile::for_paint(&profile)?
        .ok_or("No road terrain compatibility profile")?;
    for family in &plan.planning_report().families {
        let shape = profile
            .policy
            .families
            .iter()
            .find(|f| u32::from(f.hierarchy) == family.hierarchy)
            .ok_or("Missing road paint family")?;
        if shape.width.to_bits() != family.width.to_bits() {
            return Err("Road paint width differs from retained routing family".into());
        }
    }
    let paths = stamped.roads.roads();
    let roads: Vec<_> = paths
        .iter()
        .map(|r| road_paint::Road {
            path: r.path,
            hierarchy: r.hierarchy,
        })
        .collect();
    let mut world = Vec::new();
    let mut water_work = 0;
    if let Some(rivers) = &stamped.maps.rivers {
        if rivers.water_chunks.len() > 4096 {
            return Err("Too many source water surfaces".into());
        }
        for chunk in &rivers.water_chunks {
            water_work += chunk.geometry.mesh.vertices.len() + chunk.geometry.mesh.triangles.len();
            if water_work > 4_194_304 {
                return Err("Water proxy conversion allowance exceeded".into());
            }
            world.push(
                chunk
                    .geometry
                    .mesh
                    .vertices
                    .iter()
                    .map(|p| std::array::from_fn(|i| f64::from(p[i] + chunk.geometry.origin[i])))
                    .collect::<Vec<_>>(),
            );
        }
    }
    let surfaces = if let Some(rivers) = &stamped.maps.rivers {
        rivers
            .water_chunks
            .iter()
            .zip(&world)
            .map(|(chunk, vertices)| Surface::new(vertices, &chunk.geometry.mesh.triangles))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![]
    };

    let native = native_profile::resolve(&project.terrain_key())?;
    let joins = plan
        .planning_report()
        .roads
        .iter()
        .enumerate()
        .flat_map(|(i, r)| {
            let paths = &paths;
            [r.from, r.to]
                .into_iter()
                .enumerate()
                .filter_map(move |(side, d)| {
                    if let papyrus_terrain::road_network::Destination::Network { road, point } = d {
                        Some(Junction {
                            a: (
                                i,
                                if side == 0 {
                                    0
                                } else {
                                    paths[i].path.points().len() - 1
                                },
                            ),
                            b: (road, point),
                        })
                    } else {
                        None
                    }
                })
        })
        .collect::<Vec<_>>();
    let input = || road_paint_stage::Input {
        layers: &stamped.maps.layers,
        roads: &roads,
        junctions: &joins,
        paint: &profile,
        terrain: &terrain_profile,
    };
    let mut batches = Vec::new();
    let output = road_paint_stage::paint(
        input(),
        512_000_000,
        |coordinates| {
            let values = native_noise::sample(
                root,
                worker,
                project
                    .installation
                    .as_ref()
                    .ok_or("Missing terrain installation")?,
                &native,
                coordinates,
                || false,
            )?
            .ok_or("Terrain noise cancelled")?;
            batches.push(serde_json::json!({"coordinates":coordinates,"values":values}));
            Ok(Some(values))
        },
        |p| {
            let mut level: Option<f64> = None;
            for surface in &surfaces {
                water_work += 1;
                if water_work > 128_000_000 {
                    return Err("Water proxy query allowance exceeded".into());
                }
                if let Some(value) = surface.highest(
                    f64::from(p[0]),
                    f64::from(p[2]),
                    stamped
                        .maps
                        .rivers
                        .as_ref()
                        .is_some_and(|r| r.water_policy.two_sided),
                    &mut water_work,
                    128_000_000,
                    || false,
                )? {
                    level = Some(level.map_or(value, |old| old.max(value)));
                }
            }
            Ok(Some(
                level.map_or(terrain_profile.water_no_hit_level, |v| v as f32),
            ))
        },
        || false,
    )?
    .ok_or("Shared road paint cancelled")?;
    let mut replay_index = 0;
    let replay = road_paint_stage::paint(
        input(),
        512_000_000,
        |coordinates| {
            let batch = batches
                .get(replay_index)
                .ok_or("Unexpected replay noise request")?;
            if serde_json::to_value(coordinates).map_err(|e| e.to_string())? != batch["coordinates"]
            {
                return Err("Road paint replay noise coordinates differ".into());
            }
            replay_index += 1;
            Ok(Some(
                serde_json::from_value(batch["values"].clone()).map_err(|e| e.to_string())?,
            ))
        },
        |p| {
            let mut level: Option<f64> = None;
            for surface in &surfaces {
                water_work += 1;
                if water_work > 128_000_000 {
                    return Err("Water proxy query allowance exceeded".into());
                }
                if let Some(value) = surface.highest(
                    f64::from(p[0]),
                    f64::from(p[2]),
                    stamped
                        .maps
                        .rivers
                        .as_ref()
                        .is_some_and(|r| r.water_policy.two_sided),
                    &mut water_work,
                    128_000_000,
                    || false,
                )? {
                    level = Some(level.map_or(value, |old| old.max(value)));
                }
            }
            Ok(Some(
                level.map_or(terrain_profile.water_no_hit_level, |v| v as f32),
            ))
        },
        || false,
    )?
    .ok_or("Shared road paint replay cancelled")?;
    if replay_index != batches.len()
        || output.height != replay.height
        || output.topology != replay.topology
        || output.splat != replay.splat
        || serde_json::to_value(&output.roads).map_err(|e| e.to_string())?
            != serde_json::to_value(&replay.roads).map_err(|e| e.to_string())?
    {
        return Err("Shared road paint failed exact replay".into());
    }
    let mut maps =
        monument_composition::compose(&stamped.maps.layers, &stamped.maps.alpha, &[], || false)?
            .ok_or("Road study copy cancelled")?;
    maps.layers.height = output.height;
    maps.layers.topology = output.topology;
    maps.layers.splat = output.splat;
    maps.rivers = stamped.maps.rivers.clone();
    if let Some(previous) = &stamped.maps.ocean {
        let mut ocean = papyrus_terrain::ocean::reconstruct(
            &maps.layers.height,
            maps.layers.height_resolution,
            &maps.layers.topology,
            maps.layers.surface_resolution,
            || false,
        )?
        .ok_or("Final road ocean reconstruction cancelled")?;
        ocean.local_water.clone_from(&previous.local_water);
        maps.layers.topology.clone_from(&ocean.topology);
        maps.ocean = Some(ocean);
    }

    let first = batches.first().ok_or("Missing topology noise batch")?;
    let last = batches.last().ok_or("Missing texture noise batch")?;
    let texture_noise: Vec<f32> =
        serde_json::from_value(last["values"].clone()).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    for value in &texture_noise {
        hash.update(value.to_le_bytes());
    }
    let report = serde_json::json!({"format":"mapper-road-height-and-surface-study","schema":1,"playable":false,
        "height_pass_ported":true,"normal_generator_activated":false,
        "shared_stage":output.report.engine,"profile":profile,"profile_sha256":profile.sha256()?,
        "roadside_source_sha256":source.sha256,"native_profile_sha256":native.sha256()?,
        "noise":texture_noise,"noise_sha256":format!("{:x}",hash.finalize()),"noise_coordinates":last["coordinates"],
        "topology_noise":first["values"],"topology_noise_coordinates":first["coordinates"],
        "terrain_profile":terrain_profile,"terrain_profile_sha256":terrain_profile.sha256()?,
        "height_report":output.report.height,"height_noise_batches":&batches[1..batches.len()-1],
        "texture_paths":output.roads,"network_attachments_checked_after_height":output.report.exact_joins,
        "water_proxy_work":water_work,"water_scope":"generated river raw triangles; bound ocean fallback; no PhysX cooking parity",
        "topology":output.report.topology,"roadside_marking":output.report.marking,"texture":output.report.texture,
        "exact_surface_replay":true,"alpha_biome_unchanged":maps.alpha==stamped.maps.alpha && maps.layers.biome==stamped.maps.layers.biome,
        "runtime_access_validated":false});
    Ok(Study { maps, report })
}
