//! Replay captured module burial against an exported terrain in an empty-world fixture.
use papyrus_terrain::{
    environment::{self, Coverage, Transform, World},
    path_terrain::Grid as HeightGrid,
    source_stamp_raster::Quad,
    tunnel_depth::{self, Policy, Volume},
    tunnel_grid::{Cell, Grid, choose},
    tunnel_layout::{self, Placement, Role, Station},
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
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Depth verification input exceeds its bound".into());
    }
    Ok(bytes)
}
fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}
fn reference_valid(
    volumes: &[Volume],
    position: [f32; 3],
    offset: f32,
    terrain: HeightGrid<'_>,
    policy: Policy,
    samples: &mut usize,
) -> Result<bool, String> {
    let boxes: Vec<_> = volumes
        .iter()
        .map(|volume| {
            let mut bounds = volume.geometry;
            bounds.center = std::array::from_fn(|axis| bounds.center[axis] + position[axis]);
            bounds.center[1] =
                ((position[1] + offset) + policy.terrain_raise) + volume.geometry.center[1];
            (volume.mask, bounds)
        })
        .collect();
    let below = boxes
        .iter()
        .filter(|(mask, _)| mask & policy.underground != 0 && mask & policy.entrance == 0)
        .all(|(_, bounds)| (bounds.bounds().maximum[1] as f32) < policy.altitude);
    if below {
        return Ok(true);
    }
    for (_, bounds) in boxes
        .iter()
        .filter(|(mask, _)| mask & policy.train_tunnels != 0 && mask & policy.entrance == 0)
    {
        let corners = [[-1., -1.], [1., -1.], [-1., 1.], [1., 1.]].map(|[x, z]| {
            let point = environment::rotate(
                bounds.rotation,
                [x * bounds.extents[0], 0., z * bounds.extents[2]],
            );
            [bounds.center[0] + point[0], bounds.center[2] + point[2]]
        });
        let raster = Quad::from_world(
            terrain.resolution,
            [-terrain.size * 0.5; 2],
            [1. / terrain.size; 2],
            corners,
        )?;
        let [a, b, c, d] = raster.bounds();
        let top = bounds.bounds().maximum[1] as f32;
        let mut failed = false;
        for z in c..d {
            for x in a..b {
                if raster.contains(x, z) {
                    let height = terrain.origin_y
                        + (terrain.height[z * terrain.resolution + x] as f32 * 3.051944E-5)
                            * terrain.vertical_span;
                    *samples += 1;
                    failed |= height <= top;
                }
            }
        }
        if failed {
            return Ok(false);
        }
    }
    Ok(true)
}
fn reference(
    placements: &[Placement],
    geometry: &BTreeMap<u32, Vec<Volume>>,
    terrain: HeightGrid<'_>,
    policy: Policy,
) -> Result<(f32, usize), String> {
    let mut offset = 0.;
    let mut samples = 0;
    loop {
        let before = offset;
        for placement in placements
            .iter()
            .filter(|p| !matches!(p.role, Role::Transition(_)))
        {
            while !reference_valid(
                &geometry[&placement.prefab_id],
                placement.position,
                offset,
                terrain,
                policy,
                &mut samples,
            )? {
                offset -= policy.step;
                if offset < -policy.maximum_drop {
                    return Err("Reference depth exceeded its fixture bound".into());
                }
            }
        }
        if offset == before {
            return Ok((offset, samples));
        }
    }
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("verify_tunnel_depth CAPTURE BUNDLE NEW_REPORT".into());
    }
    let capture = read(Path::new(&args[0]), 64 * 1024 * 1024)?;
    let document: Value = serde_json::from_slice(&capture).map_err(|e| e.to_string())?;
    let bundle = Path::new(&args[1]);
    let manifest: Value =
        serde_json::from_slice(&read(&bundle.join("manifest.json"), 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    let project = mapper_studio::Project::load(&bundle.join("project.mapper"))?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing terrain installation")?;
    let captured_sources: Vec<mapper_studio::installation::SourceFile> = decode(&document["sources"])?;
    let terrain_sources: Vec<mapper_studio::installation::SourceFile> =
        decode(&manifest["assets"]["sources"])?;
    if captured_sources.get(..3) != Some(terrain_sources.as_slice()) {
        return Err("Terrain assets differ from the captured module installation".into());
    }
    if document["format"] != "mapper-tunnel-grid-source-inspection"
        || document["schema"] != 1
        || decode::<mapper_studio::installation::SourceFile>(&document["sources"][0])?
            != installation.native
        || Some(decode::<mapper_studio::installation::SourceFile>(
            &document["sources"][1],
        )?) != installation.scene_manifest
        || manifest["format"] != "papyrus-preview-bundle"
        || manifest["size"].as_u64() != Some(project.size as u64)
        || manifest["height_encoding"] != "signed i16 normalized by 32766 over -500..500 metres"
    {
        return Err("Depth fixture and captured installation differ".into());
    }
    let n = manifest["height_resolution"]
        .as_u64()
        .filter(|&n| (2..=4097).contains(&n))
        .ok_or("Invalid terrain resolution")? as usize;
    let raw = read(&bundle.join("height.i16le"), (n * n * 2) as u64)?;
    let height_hash = format!("{:x}", Sha256::digest(&raw));
    if raw.len() != n * n * 2
        || manifest["files"]["height.i16le"]["sha256"] != height_hash
        || manifest["files"]["height.i16le"]["bytes"].as_u64() != Some(raw.len() as u64)
    {
        return Err("Terrain height receipt differs".into());
    }
    let height: Vec<_> = raw
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]))
        .collect();
    let terrain = HeightGrid {
        size: project.size as f32,
        resolution: n,
        origin_y: -500.,
        vertical_span: 1000.,
        height: &height,
    };
    let programs = document["programs"]
        .as_array()
        .ok_or("Missing installed grid stage")?;
    let stages: Vec<_> = programs
        .iter()
        .filter(|p| p["identity"][2] == "GenerateDungeonGrid")
        .collect();
    if stages.len() != 1 {
        return Err("Expected one installed grid stage".into());
    }
    let stage = stages[0];
    let mut selected = BTreeSet::new();
    for role in ["TunnelFolder", "StationFolder", "TransitionFolder"] {
        for path in stage["selectors"][role]["prefabs"]
            .as_array()
            .ok_or("Missing grid module pool")?
        {
            selected.insert(path.as_str().ok_or("Invalid module path")?);
        }
    }
    let mut cells = BTreeMap::new();
    let mut geometry = BTreeMap::new();
    for path in selected {
        let prefab = &document["prefabs"][path];
        let id = decode::<u32>(&document["identities"]["prefabs"][path])?;
        if id == 0 {
            return Err("Invalid prefab identity".into());
        }
        let record = &prefab["cells"][0];
        let mut connections = [0; 4];
        let mut variants = [0; 4];
        for (side, name) in ["north", "south", "west", "east"].into_iter().enumerate() {
            connections[side] = decode(&record["connections"][name])?;
            variants[side] = decode(&record["variants"][name])?;
        }
        let avoid = record["avoid_neighbours"]
            .as_array()
            .ok_or("Missing neighbour constraints")?
            .iter()
            .map(|r| {
                decode(
                    &document["identities"]["neighbours"]
                        [r["guid"].as_str().ok_or("Missing neighbour GUID")?]["prefab_id"],
                )
            })
            .collect::<Result<Vec<u32>, String>>()?;
        cells.insert(
            path,
            Cell {
                id,
                connections,
                variants,
                avoid_neighbours: avoid,
            },
        );
        let mut volumes = Vec::new();
        for volume in prefab["environments"]
            .as_array()
            .ok_or("Missing volume capture")?
        {
            let matrix = decode(&volume["matrix"])?;
            let rotations: Vec<[f32; 4]> = decode(&volume["rotation_chain"])?;
            let local = mapper_studio::environment_checks::local_transform(matrix, &rotations, true)?;
            let pose = Transform {
                position: [0.; 3],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
            };
            volumes.push(Volume {
                mask: decode::<i32>(&volume["mask"])? as u32,
                geometry: environment::volume_box(
                    local,
                    decode(&volume["center"])?,
                    decode(&volume["size"])?,
                    pose,
                )?,
            });
        }
        geometry.insert(id, volumes);
    }
    let pool = |role: &str| -> Result<Vec<&Cell>, String> {
        stage["selectors"][role]["prefabs"]
            .as_array()
            .ok_or("Missing selector pool")?
            .iter()
            .map(|v| {
                cells
                    .get(v.as_str().ok_or("Invalid selector path")?)
                    .ok_or_else(|| "Missing selected cell".into())
            })
            .collect()
    };
    let tunnels = pool("TunnelFolder")?;
    let stations = pool("StationFolder")?;
    let transitions = pool("TransitionFolder")?;
    let grid = Grid::new(terrain.size, decode(&stage["fields"]["CellSize"])?)?;
    let width = grid.count();
    if width < 4 {
        return Err("Depth fixture needs at least four grid cells".into());
    }
    let roots = [
        Station {
            cell: [width / 4, width / 4],
            module: choose(
                stations.iter().copied(),
                [true, true, false, false],
                [None; 4],
            )
            .ok_or("Missing station")?,
        },
        Station {
            cell: [width * 3 / 4, width * 3 / 4],
            module: choose(
                stations.iter().copied(),
                [false, false, true, true],
                [None; 4],
            )
            .ok_or("Missing station")?,
        },
    ];
    let blocked = vec![false; width * width];
    let world = World::build(Vec::new(), Coverage::Complete, 1000, || false)?
        .map_err(|v| format!("{v:?}"))?;
    let policy = Policy {
        underground: 1,
        train_tunnels: 32,
        entrance: 4096,
        forbidden: 3,
        altitude: 0.,
        terrain_raise: 9.,
        collision_raise: decode::<f32>(&stage["fields"]["LinkTransition"])? + 1.,
        step: 3.,
        maximum_drop: 1000.,
    };
    let mut cases = Vec::new();
    for seed in 0..8 {
        let layout = tunnel_layout::assemble(
            tunnel_layout::Request {
                grid,
                tunnels: &tunnels,
                stations: &roots,
                transitions: &transitions,
                blocked: &blocked,
                seed,
                maximum_work: 2_000_000,
            },
            || false,
        )
        .map_err(|e| format!("{e:?}"))?;
        let depth = tunnel_depth::place(
            tunnel_depth::Request {
                placements: &layout.placements,
                geometry: &geometry,
                terrain: Some(terrain),
                world: &world,
                policy,
                maximum_work: 32_000_000,
            },
            || false,
        )
        .map_err(|e| format!("{e:?}"))?;
        let (reference_offset, reference_samples) =
            reference(&layout.placements, &geometry, terrain, policy)?;
        if depth.vertical_offset != reference_offset {
            return Err("Cached and direct depth traversal differ".into());
        }
        cases.push(json!({"seed":seed,"placements":layout.placements.len(),"offset":depth.vertical_offset,
            "passes":depth.passes,"work":depth.work,"terrain_samples":depth.terrain_samples,"reference_terrain_samples":reference_samples}));
    }
    let report = json!({"format":"mapper-tunnel-depth-verification","schema":1,"capture_sha256":format!("{:x}",Sha256::digest(&capture)),
        "height_sha256":height_hash,"size":project.size,"world_fixture":"empty registered environment",
        "policy_origin":"inspected owned managed server reference","game_runtime_validated":false,"cases":cases});
    std::fs::File::create_new(Path::new(&args[2]))
        .and_then(|mut file| file.write_all(&serde_json::to_vec_pretty(&report).unwrap()))
        .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
