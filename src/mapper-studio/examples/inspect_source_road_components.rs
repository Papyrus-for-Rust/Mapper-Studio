//! SAVED_BUNDLE OWNED_STUDY NEW_REPORT. Frozen-input connectivity diagnosis.
//! Empty placement changes positive prices only; it does not change blocked cells.
use mapper_studio::Project;
use papyrus_terrain::{
    native_layers::Layers,
    rail_costmap::Terrain,
    road_costmap::{self, CellLattice},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    path::Path,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Replay input exceeds bound".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("SAVED_BUNDLE OWNED_STUDY NEW_REPORT".into());
    }
    let bundle = Path::new(&args[0]);
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let study_bytes = read(Path::new(&args[1]), 16 * 1024 * 1024)?;
    let study: Value = serde_json::from_slice(&study_bytes).map_err(|e| e.to_string())?;
    let manifest_bytes = read(&bundle.join("manifest.json"), 4 * 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
    let checked = |name: &str| -> Result<Vec<u8>, String> {
        let bytes = read(&bundle.join(name), 128 * 1024 * 1024)?;
        if manifest["files"][name]["sha256"].as_str() != Some(hash(&bytes).as_str())
            || manifest["files"][name]["bytes"].as_u64() != Some(bytes.len() as u64)
        {
            return Err(format!("Changed replay input {name}"));
        }
        Ok(bytes)
    };
    let project_bytes = checked("project.mapper")?;
    let project = Project::load(&bundle.join("project.mapper"))?;
    let height = checked("height.i16le")?;
    let topology = checked("topology.i32le")?;
    let profile = project
        .roads
        .as_ref()
        .and_then(|r| r.ring_costmap.as_ref())
        .ok_or("Missing frozen cost profile")?;
    profile.validate()?;
    if study["format"] != "mapper-owned-source-road-network-study-1"
        || study["project_sha256"].as_str() != Some(hash(&project_bytes).as_str())
        || study["height_sha256"].as_str() != Some(hash(&height).as_str())
        || study["topology_sha256"].as_str() != Some(hash(&topology).as_str())
        || study["source_sha256"].as_str() != Some(profile.infrastructure_sha256.as_str())
    {
        return Err("Replay differs from owned-input study".into());
    }
    if height.len() % 2 != 0 || topology.len() % 4 != 0 {
        return Err("Malformed encoded replay terrain".into());
    }
    let layers = Layers {
        size: project.size,
        terrain_seed: project.terrain_key().seed,
        biome_seed: project.terrain_key().biome_seed.unwrap_or(0),
        height_resolution: manifest["height_resolution"]
            .as_u64()
            .ok_or("Missing height size")? as usize,
        surface_resolution: manifest["surface_resolution"]
            .as_u64()
            .ok_or("Missing surface size")? as usize,
        height: height
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect(),
        topology: topology
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        biome: checked("biome.u8")?,
        splat: checked("splat.u8")?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let lattice = CellLattice::new(
        project.size as f32,
        profile
            .joins
            .as_ref()
            .ok_or("Missing grid spacing")?
            .source_grid_spacing,
    )?;
    let empty = vec![0; layers.surface_resolution * layers.surface_resolution];
    let grid = road_costmap::build_cell_centred(
        Terrain {
            size: project.size as f32,
            height_resolution: layers.height_resolution,
            height: &layers.height,
            topology_resolution: layers.surface_resolution,
            topology: &layers.topology,
            placement_resolution: layers.surface_resolution,
            placement: &empty,
        },
        &profile.policy,
        lattice,
        project.seeds[3].value,
        false,
        profile.maximum_work,
        || false,
    )?
    .ok_or("Cancelled")?;
    let n = lattice.resolution();
    let mut labels = vec![0_u32; n * n];
    let mut sizes = vec![0usize];
    let mut queue = VecDeque::new();
    for i in 0..n * n {
        if labels[i] != 0 || grid.grid.costs[i] == 0 {
            continue;
        }
        let id = sizes.len() as u32;
        sizes.push(0);
        labels[i] = id;
        queue.push_back([i % n, i / n]);
        while let Some([x, z]) = queue.pop_front() {
            sizes[id as usize] += 1;
            for (dx, dz) in [
                (0, 1),
                (-1, 0),
                (1, 0),
                (0, -1),
                (-1, 1),
                (1, 1),
                (-1, -1),
                (1, -1),
            ] {
                let xx = x.checked_add_signed(dx).filter(|v| *v < n);
                let zz = z.checked_add_signed(dz).filter(|v| *v < n);
                let (Some(xx), Some(zz)) = (xx, zz) else {
                    continue;
                };
                let at = zz * n + xx;
                if labels[at] == 0 && grid.grid.costs[at] != 0 {
                    labels[at] = id;
                    queue.push_back([xx, zz]);
                }
            }
        }
    }
    let mut ranked: Vec<_> = sizes
        .iter()
        .enumerate()
        .skip(1)
        .map(|(id, &size)| json!({"id":id,"cells":size}))
        .collect();
    ranked.sort_by_key(|v| std::cmp::Reverse(v["cells"].as_u64().unwrap_or(0)));
    let waypoints = study["ring"]["budget"]["waypoints"]
        .as_array()
        .or_else(|| study["ring"]["raw"]["audit"]["waypoints"].as_array())
        .ok_or("Study has no captured ring waypoints")?;
    let mut points = Vec::new();
    for p in waypoints {
        let cell: [usize; 2] = serde_json::from_value(p.clone()).map_err(|e| e.to_string())?;
        if cell.iter().any(|&v| v >= n) {
            return Err("Invalid captured waypoint".into());
        }
        let index = cell[1] * n + cell[0];
        let label = labels[index] as usize;
        points.push(json!({"cell":cell,"world":lattice.point(cell),"cost":grid.grid.costs[index],"component":label,"component_cells":sizes[label]}));
    }
    let mut endpoints = Vec::new();
    let mut ring_components = std::collections::BTreeSet::new();
    if let Some(nodes) = study["ring"]["raw"]["nodes"].as_array() {
        for node in nodes {
            let cell: [usize; 2] =
                serde_json::from_value(node["cell"].clone()).map_err(|e| e.to_string())?;
            if cell.iter().any(|&v| v >= n) {
                return Err("Invalid captured ring node".into());
            }
            let label = labels[cell[1] * n + cell[0]];
            if label != 0 {
                ring_components.insert(label);
            }
        }
    }
    if let Some(family) = study["families"]
        .as_array()
        .and_then(|a| a.iter().find(|f| f["trail"] == false))
    {
        let connectors: Vec<papyrus_terrain::source_road_layout::Connector> =
            serde_json::from_value(family["inputs"]["connectors"].clone())
                .map_err(|e| e.to_string())?;
        let unattached: Vec<usize> =
            serde_json::from_value(family["layout"]["unattached_connectors"].clone())
                .map_err(|e| e.to_string())?;
        let settings = papyrus_terrain::source_road_route::Settings {
            maximum_expansions: study["search"]["maximum_expansions"]
                .as_u64()
                .ok_or("Missing search depth")? as usize,
            maximum_work: study["search"]["maximum_work"]
                .as_u64()
                .ok_or("Missing search work")? as usize,
            directional_penalty: study["search"]["directional_penalty"]
                .as_u64()
                .ok_or("Missing search pricing")? as u32,
            directional_radius: study["search"]["directional_radius"]
                .as_u64()
                .ok_or("Missing directional radius")? as usize,
        };
        for (i, c) in connectors.iter().enumerate() {
            let result = papyrus_terrain::source_road_route::find_closest_walkable(
                &grid,
                c.cell,
                settings,
                || false,
            )?;
            let papyrus_terrain::source_road_route::Outcome::Found(path) = result else {
                return Err("Frozen endpoint recovery did not resolve".into());
            };
            let cell = path.cells[0];
            let index = cell[1] * n + cell[0];
            let component = labels[index] as usize;
            let mut endpoint = json!({"index":i,"identity":family["inputs"]["endpoints"][i],"authored_cell":c.cell,
                "recovered_cell":cell,"recovery_cells":path.cells.len(),"recovered_cost_without_placement":grid.grid.costs[index],
                "component":component,"component_cells":sizes[component],"reported_unattached":unattached.contains(&i)});
            if unattached.contains(&i) && !ring_components.is_empty() {
                let mut goal_view = grid.clone();
                for (value, label) in goal_view.grid.costs.iter_mut().zip(&labels) {
                    if !ring_components.contains(label) {
                        *value = 0;
                    }
                }
                // Diagnostic only: change eligible recovery goals, not the
                // original routing grid or the source-layout result.
                if let papyrus_terrain::source_road_route::Outcome::Found(candidate) =
                    papyrus_terrain::source_road_route::find_closest_walkable(
                        &goal_view,
                        c.cell,
                        settings,
                        || false,
                    )?
                {
                    let goal = candidate.cells[0];
                    let at = goal[1] * n + goal[0];
                    endpoint["connected_recovery_candidate"] = json!({"cell":goal,"world":lattice.point(goal),
                        "recovery_cells":candidate.cells.len(),"component":labels[at],"component_cells":sizes[labels[at] as usize],
                        "original_cost_without_placement":grid.grid.costs[at],"goal_policy":"existing-ring-components",
                        "scope":"Independent recovery-goal study only; not applied to layout, terrain or meshes."});
                }
            }
            endpoints.push(endpoint);
        }
    }
    let costs: Vec<u8> = grid
        .grid
        .costs
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let cost_file = output.with_extension("costs.u32le");
    if cost_file.exists() {
        return Err("Cost capture must be new".into());
    }
    fs::File::options()
        .write(true)
        .create_new(true)
        .open(&cost_file)
        .map_err(|e| e.to_string())?
        .write_all(&costs)
        .map_err(|e| e.to_string())?;
    let report = json!({"format":"mapper-frozen-source-road-components-1","owned_study_sha256":hash(&study_bytes),"project_sha256":hash(&project_bytes),
        "lattice":lattice,"counts":grid.grid.counts,"cost_work":grid.grid.work,"cost_file":cost_file,"costs_sha256":hash(&costs),
        "component_count":sizes.len()-1,"walkable_cells":sizes.iter().sum::<usize>(),"largest_components":ranked.into_iter().take(16).collect::<Vec<_>>(),
        "rejected_waypoints":points,"ring_components":ring_components,"paved_endpoints":endpoints,"scope":"Frozen, receipted owned-input walkability replay. Empty placement changes positive costs only. No fresh installation verification, generated map, terrain mutation or runtime validation."});
    if checked("project.mapper")? != project_bytes
        || checked("height.i16le")? != height
        || checked("topology.i32le")? != topology
    {
        return Err("Frozen inputs changed during replay".into());
    }
    fs::File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Replayed source walkability: {} components; report {}",
        sizes.len() - 1,
        output.display()
    );
    Ok(())
}
