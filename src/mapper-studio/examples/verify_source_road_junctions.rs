//! STUDY NEW_REPORT MAX_SNAP_METRES MAX_WORK. Frozen geometry only; no game worker.
use mapper_studio::{
    source_road_handoff as handoff,
    source_road_inputs::{self, Endpoint, Prepared},
};
use papyrus_terrain::{
    path_geometry::Path as Curve, road_costmap::CellLattice, source_road_junctions as junctions,
    source_road_ring::RawNode,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn field<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(v[key].clone()).map_err(|e| format!("{key}: {e}"))
}
fn optional_field<T: serde::de::DeserializeOwned + Default>(
    v: &Value,
    key: &str,
) -> Result<T, String> {
    v.get(key)
        .map(|_| field(v, key))
        .transpose()
        .map(|v| v.unwrap_or_default())
}
fn inputs(v: &Value, lattice: CellLattice) -> Result<Prepared, String> {
    if v["engine"] != source_road_inputs::VERSION {
        return Err("Unknown source input snapshot".into());
    }
    let endpoints = v["endpoints"]
        .as_array()
        .ok_or("Missing endpoints")?
        .iter()
        .map(|e| {
            Ok(Endpoint {
                monument: field(e, "monument")?,
                connector: field(e, "connector")?,
                prefab: field(e, "prefab")?,
                world: field(e, "world")?,
                hierarchy_active_without_root: field(e, "hierarchy_active_without_root")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let p = Prepared {
        engine: source_road_inputs::VERSION,
        assets_sha256: field(v, "assets_sha256")?,
        roles_sha256: field(v, "roles_sha256")?,
        source_sha256: field(v, "source_sha256")?,
        lattice,
        trail: field(v, "trail")?,
        connectors: field(v, "connectors")?,
        endpoints,
        skipped_roadside: field(v, "skipped_roadside")?,
        skipped_nonmonuments: field(v, "skipped_nonmonuments")?,
    };
    // The snapshot's coordinates are source f32s serialized through Value.
    // Compare their f32 values rather than the parser's intermediate f64s.
    let mut expected = v.clone();
    for e in expected["endpoints"]
        .as_array_mut()
        .ok_or("Missing endpoints")?
    {
        e["world"] =
            serde_json::to_value(field::<[f32; 3]>(e, "world")?).map_err(|e| e.to_string())?;
    }
    if serde_json::to_value(&p).map_err(|e| e.to_string())? != expected {
        return Err("Incomplete source input reconstruction".into());
    }
    Ok(p)
}
fn layout(
    v: &Value,
    lattice: CellLattice,
    roads: &[Owned],
) -> Result<papyrus_terrain::source_road_layout::Layout, String> {
    use papyrus_terrain::source_road_layout::{self, Audit, Layout, Segment};
    let a = &v["audit"];
    let engine = match a["engine"].as_str() {
        Some(source_road_layout::VERSION) => source_road_layout::VERSION,
        Some(source_road_layout::CONNECTED_VERSION) => source_road_layout::CONNECTED_VERSION,
        Some(source_road_layout::TERRAIN_CONNECTED_VERSION) => {
            source_road_layout::TERRAIN_CONNECTED_VERSION
        }
        Some(source_road_layout::TERRAIN_ROUTED_VERSION) => {
            source_road_layout::TERRAIN_ROUTED_VERSION
        }
        Some(source_road_layout::TERRAIN_BOOTSTRAP_VERSION) => {
            source_road_layout::TERRAIN_BOOTSTRAP_VERSION
        }
        _ => return Err("Invalid source layout audit".into()),
    };
    let raw = v["segments"]
        .as_array()
        .filter(|v| v.len() == roads.len())
        .ok_or("Invalid segment count")?;
    let segments = raw
        .iter()
        .zip(roads)
        .map(|(v, r)| {
            Ok(Segment {
                lattice,
                nodes: r.nodes.clone(),
                origin: field(v, "origin")?,
                target: field(v, "target")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = Layout {
        lattice,
        segments,
        network_targets: field(v, "network_targets")?,
        recovered_entrances: optional_field(v, "recovered_entrances")?,
        unrecovered_connectors: field(v, "unrecovered_connectors")?,
        unattached_connectors: field(v, "unattached_connectors")?,
        audit: Audit {
            engine,
            policy: field(a, "policy")?,
            search: papyrus_terrain::source_road_route::Settings {
                maximum_expansions: field(&a["search"], "maximum_expansions")?,
                maximum_work: field(&a["search"], "maximum_work")?,
                directional_penalty: field(&a["search"], "directional_penalty")?,
                directional_radius: field(&a["search"], "directional_radius")?,
            },
            maximum_work: field(a, "maximum_work")?,
            work: field(a, "work")?,
            entrance_queries: field(a, "entrance_queries")?,
            terrain_entrance_queries: optional_field(a, "terrain_entrance_queries")?,
            terrain_route_queries: optional_field(a, "terrain_route_queries")?,
            terrain_route_traces: optional_field(a, "terrain_route_traces")?,
            terrain_route_fallback: optional_field(a, "terrain_route_fallback")?,
            shared_entrance_branches: optional_field(a, "shared_entrance_branches")?,
            terrain_entrance_replacements: optional_field(a, "terrain_entrance_replacements")?,
            terrain_preparation_work: optional_field(a, "terrain_preparation_work")?,
            terrain_replaced_connectors: optional_field(a, "terrain_replaced_connectors")?,
            terrain_fallback_connectors: optional_field(a, "terrain_fallback_connectors")?,
            connected_goal_preparations: field(a, "connected_goal_preparations")?,
            connected_goal_work: field(a, "connected_goal_work")?,
            bootstrap_recovery_targets: optional_field(a, "bootstrap_recovery_targets")?,
            layout_queries: field(a, "layout_queries")?,
            depth_rejections: field(a, "depth_rejections")?,
            failed_connections: field(a, "failed_connections")?,
            skipped_roadside: field(a, "skipped_roadside")?,
            promoted_groups: field(a, "promoted_groups")?,
            accepted_connectors: field(a, "accepted_connectors")?,
            discarded_singletons: field(a, "discarded_singletons")?,
        },
    };
    if serde_json::to_value(&result).map_err(|e| e.to_string())? != *v {
        return Err("Incomplete source layout reconstruction".into());
    }
    Ok(result)
}
struct Owned {
    lattice: CellLattice,
    nodes: Vec<RawNode>,
    path: Curve,
    terminals: [bool; 2],
    hierarchy: u8,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 32 * 1024 * 1024 {
        return Err("Junction study exceeds 32 MiB".into());
    }
    Ok(raw)
}
fn parse(
    raw: &Value,
    shaped: &Value,
    lattice: &Value,
    terminals: [bool; 2],
    hierarchy: u8,
) -> Result<Owned, String> {
    let size = lattice["size"].as_f64().ok_or("Missing lattice size")? as f32;
    let spacing = lattice["requested_spacing"]
        .as_f64()
        .ok_or("Missing lattice spacing")? as f32;
    let parsed = CellLattice::new(size, spacing)?;
    if serde_json::to_value(parsed).map_err(|e| e.to_string())? != *lattice {
        return Err("Invalid saved lattice".into());
    }
    let nodes = raw
        .as_array()
        .ok_or("Missing raw nodes")?
        .iter()
        .map(|v| {
            Ok(RawNode {
                cell: serde_json::from_value(v["cell"].clone()).map_err(|e| e.to_string())?,
                cost: v["cost"].as_u64().ok_or("Missing node cost")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let points: Vec<[f32; 3]> =
        serde_json::from_value(shaped["points"].clone()).map_err(|e| e.to_string())?;
    let path = Curve::from_controls(&points, 1_000_000, || false)
        .map_err(|e| format!("Path: {e:?}"))?
        .ok_or("Cancelled")?;
    let tangents: Vec<[f32; 3]> =
        serde_json::from_value(shaped["tangents"].clone()).map_err(|e| e.to_string())?;
    if Some(path.range().0 as u64) != shaped["min_index"].as_u64()
        || Some(path.range().1 as u64) != shaped["max_index"].as_u64()
        || path.tangents() != tangents
        || Some(path.is_circular()) != shaped["circular"].as_bool()
        || path.length() != shaped["length"].as_f64().ok_or("Missing path length")? as f32
        || path.step_size() != shaped["step_size"].as_f64().ok_or("Missing step size")? as f32
    {
        return Err("Reconstructed controls differ from original shaped geometry".into());
    }
    Ok(Owned {
        lattice: parsed,
        nodes,
        path,
        terminals,
        hierarchy,
    })
}
fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(4..=5).contains(&args.len()) {
        return Err(
            "STUDY NEW_REPORT MAX_SNAP_METRES MAX_WORK [FROZEN_INFRASTRUCTURE_JSON | --raw]".into(),
        );
    }
    let input = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let bytes = read(input)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if value["format"] != "mapper-owned-source-road-network-study-1"
        || value["layout_study_succeeded"] != true
    {
        return Err("Expected successful source road study".into());
    }
    let ring = &value["ring"];
    let mut owned = vec![parse(
        &ring["raw"]["nodes"],
        &ring["shaped"],
        &ring["raw"]["lattice"],
        [false, false],
        0,
    )?];
    for family in value["families"].as_array().ok_or("Missing families")? {
        let segments = family["layout"]["segments"]
            .as_array()
            .ok_or("Missing raw segments")?;
        let shapes = family["shaped"]
            .as_array()
            .filter(|v| v.len() == segments.len())
            .ok_or("Shaped coverage differs")?;
        let hierarchy = if family["trail"].as_bool().ok_or("Missing family")? {
            2
        } else {
            1
        };
        for (raw, shape) in segments.iter().zip(shapes) {
            let terminals = ["origin", "target"].map(|key| !raw[key].is_null());
            owned.push(parse(
                &raw["nodes"],
                shape,
                &family["layout"]["lattice"],
                terminals,
                hierarchy,
            )?);
        }
    }
    let roads = owned
        .iter()
        .map(|r| junctions::Road {
            lattice: r.lattice,
            nodes: &r.nodes,
            path: &r.path,
            terminals: r.terminals,
            hierarchy: r.hierarchy,
        })
        .collect::<Vec<_>>();
    let settings = junctions::Settings {
        maximum_snap_distance: args[2].parse().map_err(|_| "Invalid snap distance")?,
        maximum_work: args[3].parse().map_err(|_| "Invalid work budget")?,
    };
    let raw_mode = args.get(4).is_some_and(|s| s == "--raw");
    let adapted = if raw_mode {
        junctions::attach_raw(&roads, settings, || false)
    } else {
        junctions::attach(&roads, settings, || false)
    };
    let result = adapted
        .map_err(|e| format!("Junction adaptation: {e:?}"))?
        .ok_or("Cancelled")?;
    if raw_mode {
        for (before, after) in owned.iter().zip(&result.paths) {
            if serde_json::to_vec(before.path.points()).map_err(|e| e.to_string())?
                != serde_json::to_vec(after.points()).map_err(|e| e.to_string())?
                || before.path.tangents() != after.tangents()
                || before.path.length().to_bits() != after.length().to_bits()
                || before.path.step_size().to_bits() != after.step_size().to_bits()
            {
                return Err("Raw handoff moved source geometry".into());
            }
        }
        for proof in &result.raw_connections {
            let points = result.paths[proof.road()].points();
            let p = points[if proof.endpoint() == 0 {
                0
            } else {
                points.len() - 1
            }];
            if !proof.matches(proof.road(), proof.endpoint(), result.paths.len(), p) {
                return Err("Raw graph token differs from preserved source endpoint".into());
            }
        }
        if read(input)? != bytes {
            return Err("Frozen source study changed".into());
        }
        let mut links = vec![Vec::new(); result.paths.len()];
        for proof in &result.raw_connections {
            links[proof.road()].push(proof.parent_road());
            links[proof.parent_road()].push(proof.road());
        }
        let mut reached = std::collections::BTreeSet::new();
        let mut pending = vec![0usize];
        while let Some(road) = pending.pop() {
            if reached.insert(road) {
                pending.extend(links[road].iter().copied());
            }
        }
        let report = json!({"format":"mapper-owned-raw-road-junctions-1","accepted":false,
            "input_sha256":format!("{:x}",Sha256::digest(&bytes)),"roads":owned.len(),
            "all_source_points_and_frames_preserved":true,"raw_edges":result.raw_connections.len(),
            "roads_reached_from_ring":reached.len(),"every_raw_branch_reaches_ring":reached.len()==result.paths.len(),
            "maximum_reference_distance":result.maximum_horizontal_snap,
            "adaptation":result,"normal_generation_integrated":false,"logical_graph_only":true,
            "monument_passages_validated":false,"terrain_support_validated":false,
            "native_mesh_validated":false,"full_map_validated":false});
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mut offset = 1;
    let mut families = Vec::new();
    for f in value["families"].as_array().ok_or("Missing families")? {
        let count = f["shaped"].as_array().ok_or("Missing shapes")?.len();
        let part = &owned[offset..offset + count];
        let lattice = owned[0].lattice;
        let prepared = inputs(&f["inputs"], lattice)?;
        let layout = layout(&f["layout"], lattice, part)?;
        families.push((
            prepared,
            layout,
            part.iter().map(|p| p.path.clone()).collect::<Vec<_>>(),
        ));
        offset += count;
    }
    let family_refs = families
        .iter()
        .map(|(inputs, layout, shaped)| handoff::Family {
            inputs,
            layout,
            shaped,
        })
        .collect::<Vec<_>>();
    let retained = handoff::prepare(
        Some(handoff::Ring {
            lattice: owned[0].lattice,
            nodes: &owned[0].nodes,
            path: &owned[0].path,
        }),
        &family_refs,
        settings,
        || false,
    )?
    .ok_or("Cancelled")?;
    if serde_json::to_value(retained.paths()).map_err(|e| e.to_string())?
        != serde_json::to_value(&result.paths).map_err(|e| e.to_string())?
    {
        return Err("Typed handoff changed the verified junction geometry".into());
    }
    let destinations = retained
        .roads()
        .iter()
        .map(|r| {
            let endpoint = |d| match d {
                papyrus_terrain::road_network::Destination::Loop => json!({"loop":true}),
                papyrus_terrain::road_network::Destination::Monument {
                    monument,
                    connector,
                } => json!({"monument":monument,"connector":connector}),
                papyrus_terrain::road_network::Destination::Network { road, point } => {
                    json!({"parent_road":road,"parent_point":point})
                }
                papyrus_terrain::road_network::Destination::RawNetwork(connection) => {
                    json!({"source_network":connection})
                }
            };
            json!({"from":endpoint(r.from),"to":endpoint(r.to)})
        })
        .collect::<Vec<_>>();
    let same = |a: [f32; 3], b: [f32; 3]| {
        a.into_iter()
            .zip(b)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    };
    let mut terminals = 0;
    for (before, after) in owned.iter().zip(&result.paths) {
        for i in 0..before.path.points().len() {
            let last = before.path.points().len() - 1;
            if (before.path.is_circular()
                || (i > 0 && i < last)
                || (i == 0 && before.terminals[0])
                || (i == last && before.terminals[1]))
                && !same(before.path.points()[i], after.points()[i])
            {
                return Err("Interior, ring or authored terminal moved".into());
            }
        }
        terminals += before.terminals.iter().filter(|v| **v).count();
    }
    for a in &result.attachments {
        let endpoint = if a.endpoint == 0 {
            0
        } else {
            result.paths[a.road].points().len() - 1
        };
        if !same(
            result.paths[a.road].points()[endpoint],
            result.paths[a.parent_road].points()[a.parent_point],
        ) {
            return Err("Attachment coordinate mismatch".into());
        }
    }
    // Check the entire undirected network, including roads with two authored
    // terminals. Checking only the first parent of each branch misses islands.
    let mut links = vec![Vec::new(); result.paths.len()];
    for attachment in &result.attachments {
        links[attachment.road].push(attachment.parent_road);
        links[attachment.parent_road].push(attachment.road);
    }
    let mut reached = std::collections::BTreeSet::new();
    let mut pending = vec![0usize];
    while let Some(road) = pending.pop() {
        if reached.insert(road) {
            pending.extend(links[road].iter().copied());
        }
    }
    if reached.len() != result.paths.len() {
        return Err("Road network has a component disconnected from its ring".into());
    }
    if read(input)? != bytes {
        return Err("Frozen source study changed".into());
    }
    let mut report = json!({"format":"mapper-owned-shaped-road-junctions-1","input_sha256":format!("{:x}",Sha256::digest(&bytes)),"roads":owned.len(),"authored_terminals":terminals,"maximum_snap_metres":settings.maximum_snap_distance,"maximum_work":settings.maximum_work,"ring_and_interiors_preserved":true,"authored_terminals_preserved":true,"attachments_bitwise_exact":true,"every_branch_reaches_ring":true,"adaptation":result,"typed_handoff":retained.receipt(),"destinations":destinations,"typed_handoff_geometry_exact":true,"native_mesh_validated":false,"full_map_validated":false});
    if let Some(source_path) = args.get(4) {
        let source_bytes = read(Path::new(source_path))?;
        let source_hash = format!("{:x}", Sha256::digest(&source_bytes));
        if Some(source_hash.as_str()) != value["source_sha256"].as_str() {
            return Err("Frozen infrastructure differs from the captured source receipt".into());
        }
        let mut source: mapper_studio::infrastructure::Source =
            serde_json::from_slice(&source_bytes).map_err(|e| e.to_string())?;
        source.sha256 = source_hash.clone();
        let plan = retained
            .into_retained_plan(&source, field(&value["families"][0], "seed")?, || false)?
            .ok_or("Cancelled")?;
        for (family, saved) in plan
            .planning_report()
            .families
            .iter()
            .filter(|f| f.hierarchy != 0)
            .zip(value["families"].as_array().ok_or("Missing families")?)
        {
            if Some(u64::from(family.seed)) != saved["seed"].as_u64() {
                return Err("Source family seed changed during handoff".into());
            }
        }
        let paths = serde_json::to_value(plan.roads().iter().map(|r| r.path).collect::<Vec<_>>())
            .map_err(|e| e.to_string())?;
        if paths != report["adaptation"]["paths"]
            || !plan.planning_report().entrance_joins.is_empty()
            || plan.planning_report().entrance_policy.is_some()
        {
            return Err(
                "Retained plan changed source curves or fabricated an entrance certificate".into(),
            );
        }
        if read(Path::new(source_path))? != source_bytes {
            return Err("Captured infrastructure changed during handoff".into());
        }
        report["retained_plan"] = plan.planning_report().json();
        report["retained_plan_geometry_exact"] = true.into();
        report["retained_plan_input_bound"] = false.into();
        report["frozen_infrastructure_sha256"] = source_hash.into();
    }
    fs::File::options()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Prepared {} roads, {} exact junctions; largest movement {:.3} m",
        owned.len(),
        report["adaptation"]["attachments"]
            .as_array()
            .unwrap()
            .len(),
        report["adaptation"]["maximum_horizontal_snap"]
            .as_f64()
            .unwrap()
    );
    Ok(())
}
