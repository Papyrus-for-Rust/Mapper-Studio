//! Audit captured entrance/socket chains at synthetic snapped world poses.
use papyrus_terrain::{
    dungeon_base::Pose,
    environment::{Coverage, Transform, World, rotate},
    tunnel_depth,
    tunnel_entrances::{self, Entrance},
    tunnel_layout::{Placement, Role},
    tunnel_links::{Connection, End, Module, Policy, Socket},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};

fn decode<T: DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
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
fn yaw(degrees: f32) -> [f32; 4] {
    let half = degrees.to_radians() * 0.5;
    [0., half.sin(), 0., half.cos()]
}
fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f32>()
        .sqrt()
}
fn aligned(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
    let dot = a
        .iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .abs()
        .min(1.);
    dot >= (tolerance.to_radians() * 0.5).cos() - 0.000001
}
fn audit(
    result: &Connection,
    entrance: End,
    station: End,
    modules: &BTreeMap<u32, &Module>,
) -> Result<(), String> {
    let mut ends = [entrance.world(), station.world()];
    for (side, segments) in [&result.downwards, &result.upwards].into_iter().enumerate() {
        for segment in segments {
            Transform {
                position: segment.pose.position,
                rotation: segment.pose.rotation,
                scale: [1.; 3],
            }
            .validate()?;
            let module = modules
                .get(&segment.prefab_id)
                .ok_or("Unknown emitted link ID")?;
            let (input, output) = if side == 0 {
                (module.up, module.down)
            } else {
                (module.down, module.up)
            };
            let input = End {
                socket: input,
                pose: segment.pose,
            }
            .world();
            if input.kind != ends[side].kind
                || distance(input.pose.position, ends[side].pose.position) > 0.001
                || !aligned(
                    input.pose.rotation,
                    ends[side].pose.rotation,
                    module.rotation_limit.max(0.1),
                )
            {
                return Err("Emitted chain breaks its preceding socket".into());
            }
            ends[side] = End {
                socket: output,
                pose: segment.pose,
            }
            .world();
        }
    }
    if ends[0].kind != ends[1].kind
        || distance(ends[0].pose.position, ends[1].pose.position) > 1.
        || !aligned(ends[0].pose.rotation, ends[1].pose.rotation, 10.)
    {
        return Err("Emitted chains do not meet".into());
    }
    Ok(())
}
fn verify(path: &str, depth: f32) -> Result<Value, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 64 * 1024 * 1024 {
        return Err("Capture exceeds its bound".into());
    }
    let document: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if document["format"] != "mapper-tunnel-grid-source-inspection" || document["schema"] != 1 {
        return Err("Unsupported capture format".into());
    }
    let stages: Vec<_> = document["programs"]
        .as_array()
        .ok_or("Missing stages")?
        .iter()
        .filter(|v| v["identity"][2] == "GenerateDungeonGrid")
        .collect();
    if stages.len() != 1 {
        return Err("Expected one grid stage".into());
    }
    let stage = stages[0];
    let prefab = |path: &str| -> Result<Module, String> {
        let links = document["prefabs"][path]["links"]
            .as_array()
            .ok_or("Missing sockets")?;
        if links.len() != 1 {
            return Err("Ambiguous link socket pair".into());
        }
        let v = &links[0];
        Ok(Module {
            id: decode(&document["identities"]["prefabs"][path])?,
            up: socket(v, "up")?,
            down: socket(v, "down")?,
            priority: decode(&v["priority"])?,
            rotation_limit: decode::<u16>(&v["rotation_limit"])? as f32,
        })
    };
    let pool = |role: &str| -> Result<Vec<Module>, String> {
        stage["selectors"][role]["prefabs"]
            .as_array()
            .ok_or("Missing selector")?
            .iter()
            .map(|v| prefab(v.as_str().ok_or("Invalid module path")?))
            .collect()
    };
    let modules = pool("LinkFolder")?;
    let stations = pool("StationFolder")?;
    let by_id = modules
        .iter()
        .map(|m| (m.id, m))
        .collect::<BTreeMap<_, _>>();
    if by_id.len() != modules.len() || by_id.contains_key(&0) {
        return Err("Invalid module identities".into());
    }
    let entrances: Vec<_> = document["prefabs"]
        .as_object()
        .ok_or("Missing prefabs")?
        .iter()
        .filter(|(_, v)| v["entrances"].as_array().is_some_and(|v| !v.is_empty()))
        .map(|(path, _)| Ok((path.as_str(), prefab(path)?)))
        .collect::<Result<_, String>>()?;
    if entrances.len() != 16 || stations.len() != 8 || modules.len() != 22 {
        return Err("Owned verification fixture roster changed".into());
    }
    let policy = Policy {
        default_type: 0,
        elevator_type: 1,
        transition_type: 2,
        link_radius: decode(&stage["fields"]["LinkRadius"])?,
        link_transition: decode(&stage["fields"]["LinkTransition"])?,
    };
    let mut cases = 0;
    let mut segments = 0;
    let mut maximum_work = 0;
    let mut maximum_gap: f32 = 0.;
    let mut retried = 0;
    let mut maximum_attempts = 0;
    let mut lowest_offset = depth;
    let mut failures = Vec::new();
    let world = World::build(Vec::new(), Coverage::Complete, 1000, || false)?
        .map_err(|e| format!("{e:?}"))?;
    for (path, entry) in entrances {
        for position in [
            [-60., 12., 60.],
            [60., 18., 60.],
            [-60., 30., -60.],
            [60., 3., -60.],
        ] {
            for rotation in [0., 15., 45., 73., 90., 137., 180., 270., 359.] {
                let entrance = End {
                    socket: entry.down,
                    pose: Pose {
                        position,
                        rotation: yaw(rotation),
                    },
                };
                let station = stations
                    .iter()
                    .min_by(|a, b| {
                        let horizontal_distance = |s: &Module| {
                            let p = s.up.pose.position;
                            (position[0] - p[0]).powi(2) + (position[2] - p[2]).powi(2)
                        };
                        horizontal_distance(a).total_cmp(&horizontal_distance(b))
                    })
                    .ok_or("Missing station")?;
                let placements = [Placement {
                    prefab_id: station.id,
                    cell: [0, 0],
                    position: [0.; 3],
                    role: Role::Station,
                }];
                let geometry = BTreeMap::from([(station.id, Vec::new())]);
                let bindings = [Entrance {
                    station_cell: [0, 0],
                    entrance,
                    station_socket: station.up,
                }];
                for seed in 0..4 {
                    let request = || tunnel_entrances::Request {
                        burial: tunnel_depth::Request {
                            placements: &placements,
                            geometry: &geometry,
                            terrain: None,
                            world: &world,
                            policy: tunnel_depth::Policy {
                                underground: 1,
                                train_tunnels: 32,
                                entrance: 4096,
                                forbidden: 3,
                                altitude: 0.,
                                terrain_raise: 9.,
                                collision_raise: policy.link_transition + 1.,
                                step: 3.,
                                maximum_drop: 120.,
                            },
                            maximum_work: 100000,
                        },
                        initial_offset: depth,
                        entrances: &bindings,
                        modules: &modules,
                        policy,
                        seed,
                        maximum_segments: 256,
                    };
                    match tunnel_entrances::assemble(request(),||false) {
                        Ok(assembly) => {
                            let station = End { socket: station.up, pose: Pose { position: [0.,assembly.vertical_offset,0.], rotation: [0.,0.,0.,1.] } };
                            let result = &assembly.entrances[0].connection;
                            audit(result,entrance,station,&by_id)?;
                            if tunnel_entrances::assemble(request(),||false) != Ok(assembly.clone()) {
                                return Err("Link assembly is not repeatable".into());
                            }
                            retried += usize::from(assembly.attempts > 1);
                            maximum_attempts = maximum_attempts.max(assembly.attempts);
                            lowest_offset = lowest_offset.min(assembly.vertical_offset);
                            maximum_work = maximum_work.max(assembly.work);
                            maximum_gap = maximum_gap.max(result.gap);
                            segments += result.downwards.len()+result.upwards.len();
                        },
                        Err(e) => failures.push(json!({"entrance":path,"position":position,"yaw":rotation,"seed":seed,"error":format!("{e:?}")})),
                    }
                    cases += 1;
                }
            }
        }
    }
    // A rotated world frame must carry its socket position with it.
    let rotation_probe = rotate(yaw(90.), [3., 0., 0.]);
    if (rotation_probe[2] + 3.).abs() > 0.00001 {
        return Err("Invalid verification frame".into());
    }
    Ok(
        json!({"capture":path,"capture_sha256":format!("{:x}",Sha256::digest(&raw)),
        "station_offset":depth,"cases":cases,"segments":segments,"maximum_work":maximum_work,
        "maximum_gap":maximum_gap,"retried":retried,"maximum_attempts":maximum_attempts,
        "lowest_offset":lowest_offset,"failures":failures}),
    )
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("Expected CURRENT_CAPTURE OLD_CAPTURE NEW_REPORT".into());
    }
    let builds = [verify(&args[0], -33.)?, verify(&args[1], -24.)?];
    let failures: usize = builds
        .iter()
        .map(|b| b["failures"].as_array().unwrap().len())
        .sum();
    let report = json!({"format":"mapper-tunnel-link-verification","schema":1,
        "fixture":"synthetic snapped entrance poses and nearest station socket",
        "burial_fixture":"empty station volume lists, empty registered world, no terrain",
        "policy_origin":"inspected owned managed server reference","game_runtime_validated":false,
        "full_network_generated":false,"builds":builds});
    std::fs::File::create_new(Path::new(&args[2]))
        .and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(&report).unwrap()))
        .map_err(|e| e.to_string())?;
    println!("Audited two installed catalogs, {failures} rejected connections");
    if failures > 0 {
        return Err("Synthetic entrance connections were rejected".into());
    }
    Ok(())
}
