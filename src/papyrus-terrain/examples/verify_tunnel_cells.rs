//! Check installation-authored tunnel connections with the Rust grid primitives.
use papyrus_terrain::tunnel_grid::{Cell, Direction, Grid, choose, transition_matches};
use papyrus_terrain::tunnel_layout::{self, Role, Station};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::Path,
};

fn sides(value: &Value) -> Result<[u8; 4], String> {
    let mut result = [0; 4];
    for (index, name) in ["north", "south", "west", "east"].iter().enumerate() {
        result[index] = value[name]
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .ok_or("Invalid authored tunnel side")?;
    }
    Ok(result)
}

fn resource(value: &Value) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|&value| value != 0)
        .ok_or_else(|| "Invalid installed prefab identity".into())
}

fn routed_matrix(
    cell_size: f32,
    tunnels: &[&Cell],
    stations: &[&Cell],
    transitions: &[&Cell],
) -> Result<Value, String> {
    if tunnels.is_empty() || stations.is_empty() {
        return Err("Routing verification needs tunnel and station modules".into());
    }
    let mut runs = 0;
    let mut modules = 0;
    let mut boundaries = 0;
    let mut maximum_work = 0;
    let mut maximum_assembly_work = 0;
    let by_id: BTreeMap<_, _> = tunnels
        .iter()
        .chain(stations)
        .chain(transitions)
        .copied()
        .map(|module| (module.id, module))
        .collect();
    for size in [1000., 4000., 4250., 6000.] {
        let grid = Grid::new(size, cell_size)?;
        let n = grid.count();
        if n < 4 {
            return Err("Routing fixtures need at least four cells per axis".into());
        }
        let mut positions = vec![[1, 1], [n - 2, n - 2]];
        if n >= 8 {
            positions.extend([[1, n - 2], [n - 2, 1]]);
        }
        let station_modules = positions
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let required = if i % 2 == 0 {
                    [true, true, false, false]
                } else {
                    [false, false, true, true]
                };
                choose(stations.iter().copied(), required, [None; 4])
                    .ok_or("Missing station orientation")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let roots: Vec<_> = positions
            .iter()
            .zip(&station_modules)
            .map(|(&cell, module)| Station { cell, module })
            .collect();
        let blocked = vec![false; n * n];
        for seed in 0..16 {
            let layout = tunnel_layout::assemble(
                tunnel_layout::Request {
                    grid,
                    tunnels,
                    stations: &roots,
                    transitions,
                    blocked: &blocked,
                    seed,
                    maximum_work: 1_000_000,
                },
                || false,
            )
            .map_err(|e| format!("Tunnel assembly failed for size {size}, seed {seed}: {e:?}"))?;
            let graph = &layout.network;
            let mut chosen = vec![None; n * n];
            let mut emitted_boundaries = BTreeMap::new();
            let mut emitted_stations = 0;
            for placement in &layout.placements {
                let module = *by_id
                    .get(&placement.prefab_id)
                    .ok_or("Unknown emitted module identity")?;
                if let Role::Transition(direction) = placement.role {
                    if !transitions
                        .iter()
                        .any(|module| module.id == placement.prefab_id)
                        || emitted_boundaries
                            .insert((placement.cell, direction as usize), placement)
                            .is_some()
                    {
                        return Err("Invalid or duplicate emitted transition".into());
                    }
                    continue;
                }
                if placement.position != grid.world(placement.cell)? {
                    return Err("Tunnel cell has an incorrect world position".into());
                }
                let index = placement.cell[1] * n + placement.cell[0];
                if chosen[index].replace(module).is_some() {
                    return Err("Duplicate emitted tunnel cell".into());
                }
                if placement.role == Role::Station
                    && !roots.iter().any(|root| {
                        root.cell == placement.cell && root.module.id == placement.prefab_id
                    })
                {
                    return Err("Emitted station differs from its fixed module".into());
                }
                if placement.role == Role::Station {
                    emitted_stations += 1;
                } else if !tunnels
                    .iter()
                    .any(|module| module.id == placement.prefab_id)
                {
                    return Err("Emitted tunnel is outside its selector pool".into());
                }
            }
            if emitted_stations != roots.len() {
                return Err("Tunnel assembly dropped a fixed station".into());
            }
            let mut required_boundaries = 0;
            for (index, module) in chosen.iter().enumerate() {
                let Some(module) = module else {
                    continue;
                };
                let mask = module
                    .connections
                    .iter()
                    .enumerate()
                    .fold(0u8, |mask, (side, &kind)| {
                        mask | (u8::from(kind != 0) << side)
                    });
                if mask != graph.connections[index] {
                    return Err("Selected tunnel changed its routed sides".into());
                }
                for direction in Direction::ALL {
                    let kind = module.connections[direction as usize];
                    if kind == 0 {
                        continue;
                    }
                    let [x, z] = grid
                        .neighbour([index % n, index / n], direction)
                        .ok_or("Authored module exits the grid")?;
                    let other =
                        chosen[z * n + x].ok_or("Authored module has an unfilled neighbour")?;
                    if kind != other.connections[direction.opposite() as usize] {
                        return Err("Authored connection kinds disagree".into());
                    }
                    if module.avoid_neighbours.contains(&other.id) {
                        return Err(
                            "Selected tunnel pair violates an authored neighbour exclusion".into(),
                        );
                    }
                    if matches!(direction, Direction::East | Direction::North)
                        && module.variants[direction as usize]
                            != other.variants[direction.opposite() as usize]
                    {
                        let placement = emitted_boundaries
                            .get(&([index % n, index / n], direction as usize))
                            .ok_or("Routed style boundary lacks an emitted transition")?;
                        let candidate = by_id[&placement.prefab_id];
                        let mut expected = grid.world([index % n, index / n])?;
                        let axis = if direction == Direction::East { 0 } else { 2 };
                        expected[axis] += cell_size * 0.5;
                        if !transition_matches(candidate, module, other, direction)
                            || placement.position != expected
                        {
                            return Err("Emitted transition does not fit its boundary".into());
                        }
                        required_boundaries += 1;
                        boundaries += 1;
                    }
                }
            }
            if required_boundaries != emitted_boundaries.len() {
                return Err("Extra unneeded transition modules".into());
            }
            let start = chosen
                .iter()
                .position(Option::is_some)
                .ok_or("Empty routed fixture")?;
            let mut visited = vec![false; chosen.len()];
            let mut stack = vec![start];
            visited[start] = true;
            while let Some(index) = stack.pop() {
                let module = chosen[index].ok_or("Disconnected cell in routed audit")?;
                for direction in Direction::ALL {
                    if module.connections[direction as usize] == 0 {
                        continue;
                    }
                    let [x, z] = grid
                        .neighbour([index % n, index / n], direction)
                        .ok_or("Invalid audited neighbour")?;
                    let next = z * n + x;
                    if !visited[next] {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
            let count = chosen.iter().filter(|cell| cell.is_some()).count();
            if visited.iter().filter(|&&value| value).count() != count {
                return Err("Routed prefab assembly has disconnected components".into());
            }
            modules += count;
            maximum_work = maximum_work.max(graph.work);
            maximum_assembly_work = maximum_assembly_work.max(layout.work);
            runs += 1;
        }
    }
    Ok(
        json!({"networks":runs,"selected_cells":modules,"style_transitions":boundaries,
        "maximum_routing_work":maximum_work,"connected":true,"reciprocal_sides":true,
        "maximum_assembly_work":maximum_assembly_work,
        "neighbour_exclusions":true}),
    )
}

fn check(path: &Path) -> Result<Value, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Authored tunnel capture exceeds 64 MiB".into());
    }
    let document: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if document["format"] != "mapper-tunnel-grid-source-inspection" || document["schema"] != 1 {
        return Err("Unsupported authored tunnel capture".into());
    }
    let prefabs = document["prefabs"]
        .as_object()
        .ok_or("Missing authored tunnel prefabs")?;
    if prefabs.is_empty() || prefabs.len() > 1024 {
        return Err("Invalid tunnel prefab roster".into());
    }
    let programs = document["programs"]
        .as_array()
        .ok_or("Missing tunnel programs")?;
    let grids: Vec<_> = programs
        .iter()
        .filter(|p| p["identity"][2] == "GenerateDungeonGrid")
        .collect();
    if grids.len() != 1 {
        return Err("This verification requires one dungeon-grid stage".into());
    }
    let stage = grids[0];
    let identities = &document["identities"];
    if identities["format"] != "mapper-tunnel-identities-1"
        || identities["prefabs"]
            .as_object()
            .is_none_or(|ids| ids.len() != prefabs.len())
    {
        return Err("Missing installed tunnel identities".into());
    }
    let mut seen = BTreeSet::new();
    for name in prefabs.keys() {
        if !seen.insert(resource(&identities["prefabs"][name])?) {
            return Err("Ambiguous installed tunnel identities".into());
        }
    }
    let mut cells = BTreeMap::new();
    for (name, prefab) in prefabs.iter() {
        let records = prefab["cells"]
            .as_array()
            .ok_or("Missing tunnel cell records")?;
        if records.len() == 1 {
            let avoid_neighbours = records[0]["avoid_neighbours"]
                .as_array()
                .ok_or("Missing tunnel neighbour exclusions")?
                .iter()
                .map(|reference| {
                    let guid = reference["guid"].as_str().ok_or("Invalid neighbour GUID")?;
                    resource(&identities["neighbours"][guid]["prefab_id"])
                })
                .collect::<Result<Vec<_>, String>>()?;
            cells.insert(
                name.as_str(),
                Cell {
                    id: resource(&identities["prefabs"][name])?,
                    connections: sides(&records[0]["connections"])?,
                    variants: sides(&records[0]["variants"])?,
                    avoid_neighbours,
                },
            );
        } else if !records.is_empty() {
            return Err("Ambiguous authored tunnel cell".into());
        }
    }
    let roster = |role: &str| -> Result<Vec<&Cell>, String> {
        stage["selectors"][role]["prefabs"]
            .as_array()
            .ok_or("Missing tunnel selector")?
            .iter()
            .map(|name| {
                let name = name.as_str().ok_or("Invalid tunnel prefab path")?;
                cells
                    .get(name)
                    .ok_or_else(|| "Selected prefab has no dungeon-grid cell".into())
            })
            .collect()
    };
    let tunnels = roster("TunnelFolder")?;
    let stations = roster("StationFolder")?;
    let upwards = roster("UpwardsFolder")?;
    let transitions = roster("TransitionFolder")?;
    let cell_size = stage["fields"]["CellSize"]
        .as_f64()
        .ok_or("Missing installed grid size")? as f32;
    let routing = routed_matrix(cell_size, &tunnels, &stations, &transitions)?;
    let mut roundtrips = 0;
    for size in [1000., 4000., 4250., 6000.] {
        let grid = Grid::new(size, cell_size)?;
        for z in 0..grid.count() {
            for x in 0..grid.count() {
                let cell = [x, z];
                let world = grid.world(cell)?;
                if grid.cell(world)? != cell {
                    return Err("Authored lattice roundtrip failed".into());
                }
                for direction in Direction::ALL {
                    if let Some(other) = grid.neighbour(cell, direction)
                        && grid.neighbour(other, direction.opposite()) != Some(cell)
                    {
                        return Err("Asymmetric tunnel lattice adjacency".into());
                    }
                }
                roundtrips += 1;
            }
        }
    }
    let mut masks = Vec::new();
    for mask in 1u8..16 {
        let required = std::array::from_fn(|side| mask & (1 << side) != 0);
        if choose(tunnels.iter().copied(), required, [None; 4]).is_some() {
            masks.push(mask);
        } else if mask.count_ones() >= 2 {
            return Err(format!(
                "Installed tunnel roster cannot cover connection mask {mask}"
            ));
        }
    }
    let mut variant_boundaries = 0;
    for first in &tunnels {
        for second in &tunnels {
            for direction in [Direction::East, Direction::North] {
                let side = direction as usize;
                let opposite = direction.opposite() as usize;
                if first.connections[side] != 0
                    && first.connections[side] == second.connections[opposite]
                    && first.variants[side] != second.variants[opposite]
                {
                    if !transitions
                        .iter()
                        .any(|module| transition_matches(module, first, second, direction))
                    {
                        return Err(
                            "Installed tunnel style boundary has no transition module".into()
                        );
                    }
                    variant_boundaries += 1;
                }
            }
        }
    }
    Ok(json!({
        "input":path, "prefabs":prefabs.len(), "tunnel_cells":tunnels.len(),
        "station_cells":stations.len(), "upwards_cells":upwards.len(),
        "transition_cells":transitions.len(), "covered_connection_masks":masks,
        "lattice_roundtrips":roundtrips, "style_boundaries_verified":variant_boundaries,
        "routing":routing,
        "neighbour_guid_resolution_verified":true, "complete_network_generated":false,
    }))
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 2 {
        return Err("verify_tunnel_cells NEW_REPORT CAPTURE [CAPTURE ...]".into());
    }
    let captures = args[1..]
        .iter()
        .map(|path| check(Path::new(path)))
        .collect::<Result<Vec<_>, _>>()?;
    let report =
        json!({"format":"mapper-tunnel-cell-verification", "schema":1, "captures":captures});
    let encoded = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::File::create_new(Path::new(&args[0]))
        .and_then(|mut file| file.write_all(&encoded))
        .map_err(|e| e.to_string())?;
    println!("{}", String::from_utf8_lossy(&encoded));
    Ok(())
}
