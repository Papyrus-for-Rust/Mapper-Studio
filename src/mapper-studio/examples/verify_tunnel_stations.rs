//! Replay installed station selection and tunnel assembly on exported map placements.
use mapper_studio::{Project, installation::SourceFile};
use papyrus_terrain::{
    dungeon_base::Pose,
    environment::{self, Box3, Coverage, Transform, World},
    path_terrain::Grid as HeightGrid,
    tunnel_depth::{self, Volume},
    tunnel_entrances,
    tunnel_grid::{Cell, Grid},
    tunnel_layout,
    tunnel_links::{End, Module, Policy, Socket},
    tunnel_spawn,
    tunnel_stations::{self, Snap, Station},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::Path,
};

fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > limit {
        return Err("Station verification input exceeds its bound".into());
    }
    Ok(raw)
}
fn decode<T: DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}
fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn yaw(degrees: f32) -> [f32; 4] {
    let half = -degrees.to_radians() * 0.5;
    [0., half.sin(), 0., half.cos()]
}
fn first_active<'a>(prefab: &'a Value, kind: &str) -> Result<&'a Value, String> {
    prefab[kind]
        .as_array()
        .ok_or("Missing captured hierarchy role")?
        .iter()
        .find(|v| v["active_without_root"] == true)
        .ok_or_else(|| format!("Missing active {kind}"))
}
fn local(record: &Value, scale: bool) -> Result<Transform, String> {
    mapper_studio::environment_checks::local_transform(
        decode(&record["matrix"])?,
        &decode::<Vec<[f32; 4]>>(&record["rotation_chain"])?,
        scale,
    )
}
fn volume(record: &Value) -> Result<Box3, String> {
    // DungeonVolume.GetBounds uses the component's local pose and unscaled bounds.
    environment::volume_box(
        Transform {
            position: decode(&record["position"])?,
            rotation: decode(&record["rotation"])?,
            scale: [1.; 3],
        },
        decode(&record["center"])?,
        decode(&record["size"])?,
        Transform {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        },
    )
}
fn socket(v: &Value, side: &str) -> Result<Socket, String> {
    Ok(Socket {
        kind: decode(&v[format!("{side}_type")])?,
        pose: Pose {
            position: decode(&v[format!("{side}_socket")]["position"])?,
            rotation: decode(&v[format!("{side}_socket")]["rotation"])?,
        },
    })
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("Expected CAPTURE EXPORTED_BUNDLE NEW_REPORT".into());
    }
    let bundle = Path::new(&args[1]);
    let capture = read(Path::new(&args[0]), 64 * 1024 * 1024)?;
    let doc: Value = serde_json::from_slice(&capture).map_err(|e| e.to_string())?;
    let manifest: Value =
        serde_json::from_slice(&read(&bundle.join("manifest.json"), 4 * 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    let project = Project::load(&bundle.join("project.mapper"))?;
    let project_raw = read(&bundle.join("project.mapper"), 4 * 1024 * 1024)?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    let sources: Vec<SourceFile> = decode(&doc["sources"])?;
    let terrain_sources: Vec<SourceFile> = decode(&manifest["assets"]["sources"])?;
    if doc["format"] != "mapper-tunnel-grid-source-inspection"
        || doc["schema"] != 1
        || sources.get(..3) != Some(terrain_sources.as_slice())
        || sources.first() != Some(&installation.native)
        || sources.get(1) != installation.scene_manifest.as_ref()
        || manifest["format"] != "papyrus-preview-bundle"
        || manifest["size"].as_u64() != Some(project.size as u64)
        || manifest["height_encoding"] != "signed i16 normalized by 32766 over -500..500 metres"
    {
        return Err("Captured modules differ from the exported installation".into());
    }
    let profiles = [
        include_str!("../../../compatibility/monuments/client-8dd4e235-stages.json"),
        include_str!("../../../compatibility/monuments/client-0f703d43-coastal.json"),
    ];
    let profile_raw = profiles
        .iter()
        .find(|raw| hash(raw.as_bytes()) == doc["code_profile_sha256"].as_str().unwrap_or(""))
        .ok_or("Capture does not use a reviewed source profile")?;
    let profile: Value = serde_json::from_str(profile_raw).map_err(|e| e.to_string())?;
    let scene: SourceFile = decode(&profile["map_scene"])?;
    let code: Vec<SourceFile> = decode(&profile["code_sources"])?;
    if profile["native_sha256"] != installation.native.sha256
        || profile["scene_sha256"] != sources[2].sha256
        || sources.get(3) != Some(&scene)
        || sources.get(4..) != Some(code.as_slice())
    {
        return Err("Capture gameplay-code receipts differ from the reviewed profile".into());
    }
    let n = manifest["height_resolution"]
        .as_u64()
        .filter(|n| (2..=4097).contains(n))
        .ok_or("Invalid height resolution")? as usize;
    let raw_height = read(&bundle.join("height.i16le"), (n * n * 2) as u64)?;
    let layout_raw = read(&bundle.join("layout.json"), 16 * 1024 * 1024)?;
    for (name, raw) in [
        ("height.i16le", &raw_height),
        ("layout.json", &layout_raw),
        ("project.mapper", &project_raw),
    ] {
        if manifest["files"][name]["sha256"] != hash(raw)
            || manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
        {
            return Err(format!("Exported {name} receipt differs"));
        }
    }
    if raw_height.len() != n * n * 2 {
        return Err("Invalid exported height map".into());
    }
    let height: Vec<_> = raw_height
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let terrain = HeightGrid {
        size: project.size as f32,
        resolution: n,
        origin_y: -500.,
        vertical_span: 1000.,
        height: &height,
    };
    let stages: Vec<_> = doc["programs"]
        .as_array()
        .ok_or("Missing programs")?
        .iter()
        .filter(|v| v["identity"][2] == "GenerateDungeonGrid")
        .collect();
    if stages.len() != 1 {
        return Err("Expected one grid program".into());
    }
    let stage = stages[0];
    let grid = Grid::new(project.size as f32, decode(&stage["fields"]["CellSize"])?)?;
    let selected = |role: &str| -> Result<Vec<&str>, String> {
        stage["selectors"][role]["prefabs"]
            .as_array()
            .ok_or("Missing selector")?
            .iter()
            .map(|v| v.as_str().ok_or_else(|| "Invalid module path".into()))
            .collect()
    };
    let mut cells = BTreeMap::new();
    let mut geometry = BTreeMap::new();
    let mut links = BTreeMap::new();
    let mut replaceable = BTreeSet::new();
    for role in [
        "TunnelFolder",
        "StationFolder",
        "TransitionFolder",
        "UpwardsFolder",
        "LinkFolder",
    ] {
        for path in selected(role)? {
            let prefab = &doc["prefabs"][path];
            let id: u32 = decode(&doc["identities"]["prefabs"][path])?;
            if id == 0 {
                return Err("Invalid installed module ID".into());
            }
            if role == "LinkFolder" || role == "StationFolder" {
                let link = first_active(prefab, "links")?;
                links.insert(
                    id,
                    Module {
                        id,
                        up: socket(link, "up")?,
                        down: socket(link, "down")?,
                        priority: decode(&link["priority"])?,
                        rotation_limit: decode::<u16>(&link["rotation_limit"])? as f32,
                    },
                );
            }
            if role == "LinkFolder" {
                continue;
            }
            let record = first_active(prefab, "cells")?;
            if record["replaceable"] == true {
                replaceable.insert(id);
            }
            let mut connections = [0; 4];
            let mut variants = [0; 4];
            for (i, side) in ["north", "south", "west", "east"].iter().enumerate() {
                connections[i] = decode(&record["connections"][side])?;
                variants[i] = decode(&record["variants"][side])?;
            }
            let avoid_neighbours = record["avoid_neighbours"]
                .as_array()
                .ok_or("Missing neighbour constraints")?
                .iter()
                .map(|v| {
                    decode(
                        &doc["identities"]["neighbours"]
                            [v["guid"].as_str().ok_or("Missing GUID")?]["prefab_id"],
                    )
                })
                .collect::<Result<_, String>>()?;
            cells.insert(
                path,
                Cell {
                    id,
                    connections,
                    variants,
                    avoid_neighbours,
                },
            );
            let mut volumes = Vec::new();
            for v in prefab["environments"]
                .as_array()
                .ok_or("Missing authored environment volumes")?
            {
                volumes.push(Volume {
                    mask: decode::<i32>(&v["mask"])? as u32,
                    geometry: environment::volume_box(
                        local(v, true)?,
                        decode(&v["center"])?,
                        decode(&v["size"])?,
                        Transform {
                            position: [0.; 3],
                            rotation: [0., 0., 0., 1.],
                            scale: [1.; 3],
                        },
                    )?,
                });
            }
            geometry.insert(id, volumes);
        }
    }
    let station_paths = selected("StationFolder")?;
    let stations: Vec<_> = station_paths
        .iter()
        .map(|path| {
            let cell = cells[*path].clone();
            Ok(Station {
                volume: volume(first_active(&doc["prefabs"][*path], "volumes")?)?,
                up_position: links[&cell.id].up.pose.position,
                cell,
            })
        })
        .collect::<Result<_, String>>()?;
    let link_modules: Vec<_> = selected("LinkFolder")?
        .iter()
        .map(|path| {
            let id: u32 = decode(&doc["identities"]["prefabs"][*path])?;
            Ok(links[&id].clone())
        })
        .collect::<Result<_, String>>()?;
    let tunnel_pool: Vec<_> = selected("TunnelFolder")?
        .iter()
        .map(|p| &cells[*p])
        .collect();
    let transitions: Vec<_> = selected("TransitionFolder")?
        .iter()
        .map(|p| &cells[*p])
        .collect();
    let layout: Value = serde_json::from_slice(&layout_raw).map_err(|e| e.to_string())?;
    let mut entrances = Vec::new();
    let mut adjustments = Vec::new();
    for outcome in layout["outcomes"]
        .as_array()
        .ok_or("Missing exported placement outcomes")?
    {
        if outcome["position"].is_null() {
            continue;
        }
        let id: u64 = decode(&outcome["id"])?;
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == id)
            .ok_or("Unknown placement owner")?;
        let Some(path) = target.prefab_path.as_deref() else {
            continue;
        };
        let prefab = &doc["prefabs"][path];
        if prefab["entrances"].as_array().is_none_or(|v| v.is_empty()) {
            continue;
        }
        let info = first_active(prefab, "entrances")?;
        let source_volume = volume(first_active(prefab, "volumes")?)?;
        let record = first_active(prefab, "links")?;
        let p = &outcome["position"];
        let root = Pose {
            position: [decode(&p["x"])?, decode(&p["y"])?, decode(&p["z"])?],
            rotation: yaw(decode(&p["yaw"])?),
        };
        let info_frame = local(info, false)?;
        let original = root.point(info_frame.position);
        let snapped = Snap {
            radius: decode(&info["link_radius"])?,
            height: decode(&info["link_height"])?,
        }
        .position(original)?;
        let delta: [f32; 3] = std::array::from_fn(|i| snapped[i] - original[i]);
        let entrance_grid = Grid::new(project.size as f32, decode(&info["cell_size"])?)?;
        if !tunnel_stations::valid_spawn(entrance_grid, source_volume, snapped)? {
            return Err(format!(
                "Exported owner {id} requires a new entrance placement"
            ));
        }
        let connector_type: i32 = decode(&stage["fields"]["ConnectionType"])?;
        let connectors: Vec<_> = prefab["connectors"]
            .as_array()
            .ok_or("Missing connectors")?
            .iter()
            .filter(|c| c["infrastructure_type"].as_i64() == Some(connector_type as i64))
            .collect();
        if connectors.len() != 1 {
            return Err("Fixture needs one tunnel connector per entrance".into());
        }
        let connector = root.point(local(connectors[0], false)?.position);
        entrances.push(tunnel_stations::Entrance {
            end: End {
                socket: socket(record, "down")?,
                pose: Pose {
                    position: snapped,
                    rotation: environment::multiply(root.rotation, info_frame.rotation),
                },
            },
            connector: std::array::from_fn(|i| connector[i] + delta[i]),
            volume: source_volume,
        });
        adjustments.push(json!({"owner":id,"prefab":target.prefab_path,"root_delta":delta}));
    }
    let blocked = vec![false; grid.count() * grid.count()];
    let world = World::build(Vec::new(), Coverage::Complete, 1000, || false)?
        .map_err(|e| format!("{e:?}"))?;
    let link_policy = Policy {
        default_type: 0,
        elevator_type: 1,
        transition_type: 2,
        link_radius: decode(&stage["fields"]["LinkRadius"])?,
        link_transition: decode(&stage["fields"]["LinkTransition"])?,
    };
    let occupied = layout["outcomes"]
        .as_array()
        .ok_or("Missing world occurrences")?
        .iter()
        .map(|o| decode::<u64>(&o["id"]))
        .collect::<Result<Vec<_>, _>>()?;
    let mut cases = Vec::new();
    for seed in 0..8 {
        let plan = tunnel_stations::select(
            tunnel_stations::Request {
                grid,
                stations: &stations,
                entrances: &entrances,
                blocked: &blocked,
                seed,
                maximum_work: 1_000_000,
            },
            || false,
        )
        .map_err(|e| format!("{e:?}"))?;
        let roots: Vec<_> = plan
            .selections
            .iter()
            .map(|s| tunnel_layout::Station {
                cell: s.cell,
                module: &stations[s.station].cell,
            })
            .collect();
        let bindings: Vec<_> = plan
            .selections
            .iter()
            .map(|s| tunnel_entrances::Entrance {
                station_cell: s.cell,
                entrance: entrances[s.entrance].end,
                station_socket: links[&stations[s.station].cell.id].up,
            })
            .collect();
        let modules = tunnel_layout::assemble(
            tunnel_layout::Request {
                grid,
                tunnels: &tunnel_pool,
                stations: &roots,
                transitions: &transitions,
                blocked: &blocked,
                seed,
                maximum_work: 2_000_000,
            },
            || false,
        )
        .map_err(|e| format!("{e:?}"))?;
        let assembled = tunnel_entrances::assemble(
            tunnel_entrances::Request {
                burial: tunnel_depth::Request {
                    placements: &modules.placements,
                    geometry: &geometry,
                    terrain: Some(terrain),
                    world: &world,
                    policy: tunnel_depth::Policy {
                        underground: 1,
                        train_tunnels: 32,
                        entrance: 4096,
                        forbidden: 3,
                        altitude: 0.,
                        terrain_raise: 9.,
                        collision_raise: link_policy.link_transition + 1.,
                        step: 3.,
                        maximum_drop: 1000.,
                    },
                    maximum_work: 32_000_000,
                },
                initial_offset: 0.,
                entrances: &bindings,
                modules: &link_modules,
                policy: link_policy,
                seed,
                maximum_segments: 16384,
            },
            || false,
        )
        .map_err(|e| format!("{e:?}"))?;
        let spawns = tunnel_spawn::plan(
            tunnel_spawn::Request {
                grid,
                layout: &modules,
                entrances: &assembled,
                occupied: &occupied,
                first_instance: 1,
                maximum_work: 8_000_000,
            },
            || false,
        )
        .map_err(|e| format!("Tunnel spawn publication: {e:?}"))?;
        let placements: Vec<_> = modules.placements.iter().map(|p| {
            let (kind, direction) = match p.role {
                tunnel_layout::Role::Tunnel => ("tunnel", None),
                tunnel_layout::Role::Station => ("station", None),
                tunnel_layout::Role::Transition(direction) => ("transition", Some(direction as usize)),
            };
            json!({"prefab_id":p.prefab_id,"cell":p.cell,"position":p.position,"kind":kind,"direction":direction})
        }).collect();
        let entrances: Vec<_> = assembled.entrances.iter().map(|e| {
            let segment = |s: &papyrus_terrain::tunnel_links::Segment| json!({"prefab_id":s.prefab_id,"pose":s.pose});
            json!({"cell":e.station_cell,"downwards":e.connection.downwards.iter().map(segment).collect::<Vec<_>>(),
                "upwards":e.connection.upwards.iter().map(segment).collect::<Vec<_>>()})
        }).collect();
        let expected: Vec<_> = spawns.spawns.iter().map(|s| {
            let (phase, cell) = match s.origin {
                tunnel_spawn::Origin::Downwards(cell) => ("downwards", cell),
                tunnel_spawn::Origin::Upwards(cell) => ("upwards", cell),
                tunnel_spawn::Origin::Grid(role) => {
                    let phase = match role { tunnel_layout::Role::Tunnel=>"tunnel", tunnel_layout::Role::Station=>"station", _=>"transition" };
                    let placement = modules.placements.iter().find(|p| p.prefab_id==s.prefab_id && p.role==role
                        && p.position[0]==s.pose.position[0] && p.position[2]==s.pose.position[2]).unwrap();
                    (phase, placement.cell)
                }
            };
            json!({"instance":s.instance,"prefab_id":s.prefab_id,"pose":s.pose,"phase":phase,"cell":cell})
        }).collect();
        let spawn_fixture = json!({"grid_count":grid.count(),"occupied":occupied,"first_instance":1,
            "vertical_offset":assembled.vertical_offset,"placements":placements,"entrances":entrances,
            "expected":expected,"next_instance":spawns.next_instance,"work":spawns.work});
        // Structural regression only. Surface/environment/filter eligibility is
        // a separate installed-program adapter and cannot be inferred here.
        let definitions: Vec<_> = cells.values().collect();
        let mut portal_replacements = 0;
        for path in selected("UpwardsFolder")? {
            let replacement = &cells[path];
            let site = modules.placements.iter().find(|placement| {
                !matches!(placement.role, tunnel_layout::Role::Transition(_))
                    && replaceable.contains(&placement.prefab_id)
                    && cells.values().any(|cell| {
                        cell.id == placement.prefab_id
                            && cell.connections == replacement.connections
                    })
            });
            let Some(site) = site else { continue };
            let trial = tunnel_layout::replace(
                grid,
                &modules,
                &[tunnel_layout::Replacement {
                    cell: site.cell,
                    original_prefab_id: site.prefab_id,
                    module: replacement,
                }],
                &definitions,
                &transitions,
                4_000_000,
                || false,
            )
            .map_err(|error| format!("Portal variant replacement {path}: {error:?}"))?;
            if trial.network != modules.network
                || trial
                    .placements
                    .iter()
                    .filter(|p| {
                        p.cell == site.cell && !matches!(p.role, tunnel_layout::Role::Transition(_))
                    })
                    .any(|p| p.prefab_id != replacement.id)
            {
                return Err(
                    "Portal replacement changed routing or retained its original cell".into(),
                );
            }
            portal_replacements += 1;
        }
        cases.push(json!({"seed":seed,"stations":plan.selections.len(),"grid_modules":modules.placements.len(),
            "link_modules":assembled.entrances.iter().map(|e|e.connection.downwards.len()+e.connection.upwards.len()).sum::<usize>(),
            "depth":assembled.vertical_offset,"attempts":assembled.attempts,"station_work":plan.work,
            "routing_work":modules.work,"burial_and_links_work":assembled.work,"terrain_samples":assembled.terrain_samples,
            "structural_portal_replacements":portal_replacements,"portal_surface_checks_run":false,"spawn_fixture":spawn_fixture}));
    }
    let report = json!({"format":"mapper-tunnel-station-world-verification","schema":1,"size":project.size,
        "capture_sha256":hash(&capture),"height_sha256":hash(&raw_height),"layout_sha256":hash(&layout_raw),"project_sha256":hash(&project_raw),
        "policy_origin":"inspected owned managed server reference","registered_world":"empty fixture",
        "game_runtime_validated":false,"normal_generation_integrated":false,"snap_adjustments":adjustments,"cases":cases});
    std::fs::File::create_new(Path::new(&args[2]))
        .and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(&report).unwrap()))
        .map_err(|e| e.to_string())?;
    println!(
        "Replayed {} registered entrance candidates across eight layouts",
        entrances.len()
    );
    Ok(())
}
