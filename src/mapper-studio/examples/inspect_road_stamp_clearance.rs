//! Diagnose protected terrain ownership without launching a game or UI.
//! CONFIG NEW_REPORT WORLD_X WORLD_Z
use mapper_studio::{
    Project, composition, native_processing, placement_preview, prefab_assets, prefab_textures,
    profile, road_planning, roadside_placement, roadside_source, roadside_stamps,
};
use papyrus_terrain::monument_composition::projection::Terrain;
use serde_json::{Value, json};
use std::{fs, path::Path, sync::atomic::AtomicBool};

fn nearest(points: &[[f32; 3]], query: [f32; 2]) -> Value {
    let mut best = (f64::INFINITY, 0, [0.; 3]);
    for (index, pair) in points.windows(2).enumerate() {
        let a = pair[0].map(f64::from);
        let b = pair[1].map(f64::from);
        let delta = [b[0] - a[0], b[2] - a[2]];
        let length = delta[0].powi(2) + delta[1].powi(2);
        let t = if length > 0. {
            ((f64::from(query[0]) - a[0]) * delta[0] + (f64::from(query[1]) - a[2]) * delta[1])
                / length
        } else {
            0.
        }
        .clamp(0., 1.);
        let p = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
        let distance = (p[0] - f64::from(query[0])).hypot(p[2] - f64::from(query[1]));
        if distance < best.0 {
            best = (distance, index, p);
        }
    }
    json!({"distance":best.0,"segment":best.1,"point":best.2})
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("CONFIG NEW_REPORT WORLD_X WORLD_Z".into());
    }
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Report already exists".into());
    }
    let point =
        [args[2].parse::<f32>(), args[3].parse::<f32>()].map(|v| v.map_err(|e| e.to_string()));
    let point = [point[0].clone()?, point[1].clone()?];
    let config: Value = serde_json::from_slice(&fs::read(&args[0]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let project = Project::load(path("project")?)?;
    if point
        .iter()
        .any(|v| !v.is_finite() || v.abs() > project.size as f32 * 0.5)
    {
        return Err("Probe outside map".into());
    }
    let root = path("rust_root")?;
    let catalog = prefab_assets::load(
        root,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, path("bindings")?, &catalog, || false)?;
    let raw = native_processing::build(
        root,
        path("worker")?,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    let solution =
        placement_preview::solve_native(&project, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    let base = composition::compose_monuments(
        &raw.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let n = base.layers.surface_resolution;
    // Same discrete topology lookup as road-bed protection.
    let [x, z] = point.map(|v| {
        (((v + project.size as f32 * 0.5) * (1. / project.size as f32)) * n as f32)
            .clamp(0., (n - 1) as f32) as usize
    });
    let i = z * n + x;
    let raw_alpha = vec![255; n * n];
    let raw_terrain = Terrain::new(&raw.source.layers, &raw_alpha, || false)?.ok_or("Cancelled")?;
    let initial_terrain = Terrain::new(&base.layers, &base.alpha, || false)?.ok_or("Cancelled")?;
    let mut land = Vec::new();
    for outcome in &solution.outcomes {
        let Some(position) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Missing target")?;
        let Some(path) = &target.prefab_path else {
            continue;
        };
        let asset = catalog.entries.get(path).ok_or("Missing land asset")?;
        let prepared =
            composition::prepare(asset, &bindings, project.size, || false)?.ok_or("Cancelled")?;
        let stamp = prepared.stamp(
            [position.x, position.y, position.z].map(f64::from),
            f64::from(position.yaw),
        );
        let cell = raw_terrain
            .projected(&stamp)?
            .cell(x, z)
            .ok_or("Invalid probe")?;
        land.push(json!({"request":outcome.id,"prefab":path,"position":[position.x,position.y,position.z],"yaw":position.yaw,"projected_topology":cell.topology,"added_bits":cell.topology & !raw.source.layers.topology[i],"removed_bits":raw.source.layers.topology[i] & !cell.topology}));
    }
    eprintln!("Preparing production road plan and roadside stamps");
    let plan = road_planning::prepare(&project, &solution, &catalog, &bindings, &base, || false)?
        .ok_or("Cancelled")?;
    let staged = project
        .roads
        .as_ref()
        .and_then(|r| r.staged.as_ref())
        .ok_or("Missing staged recipe")?;
    let source =
        roadside_source::Source::cached(&staged.roadside_source_sha256, root, &catalog, || false)?;
    let requests = roadside_placement::captured_requests(&source, &solution)?;
    let context = || roadside_placement::Context {
        project: &project,
        initial: &solution,
        catalog: &catalog,
        bindings: &bindings,
        maps: &base,
        plan: &plan,
        source: &source,
    };
    let selected = roadside_placement::select(context(), &requests, 128_000_000, || false)?
        .ok_or("Cancelled")?;
    let stamped =
        roadside_stamps::compose(context(), &selected, 1_000_000, || false)?.ok_or("Cancelled")?;
    let original_roads = plan
        .roads()
        .iter()
        .enumerate()
        .map(|(index, road)| json!({"road":index,"nearest":nearest(road.path.points(), point)}))
        .collect::<Vec<_>>();
    let straightened_roads = stamped
        .roads
        .roads()
        .iter()
        .enumerate()
        .map(|(index, road)| json!({"road":index,"nearest":nearest(road.path.points(), point)}))
        .collect::<Vec<_>>();
    let mut roadside = Vec::new();
    for placement in &stamped.report.placements {
        let mut asset = catalog
            .entries
            .get(&placement.prefab)
            .ok_or("Missing roadside asset")?
            .clone();
        if let Some(enabled) = source
            .prefabs()
            .get(&placement.prefab)
            .ok_or("Missing captured prefab")?
            .water_map_enabled()
        {
            let modifier = asset.modifier.as_mut().ok_or("Missing modifier")?;
            if modifier.water_map.is_some_and(|value| value != enabled) {
                return Err("Water policies disagree".into());
            }
            modifier.water_map = Some(enabled);
        }
        let prepared =
            composition::prepare(&asset, &bindings, project.size, || false)?.ok_or("Cancelled")?;
        let stamp = prepared.stamp(placement.position.map(f64::from), f64::from(placement.yaw));
        let cell = initial_terrain
            .projected(&stamp)?
            .cell(x, z)
            .ok_or("Invalid probe")?;
        roadside.push(json!({"placement":placement,"projected_topology":cell.topology,"added_bits":cell.topology & !base.layers.topology[i],"removed_bits":base.layers.topology[i] & !cell.topology}));
    }
    profile::write_document(
        output,
        &json!({"format":"mapper-road-stamp-ownership","schema":1,"point":point,"surface_cell":[x,z],"raw_topology":raw.source.layers.topology[i],"initial_land_topology":base.layers.topology[i],"roadside_topology":stamped.maps.layers.topology[i],"land":land,"roadside":roadside,"selection":selected.selection,"original_roads":original_roads,"straightened_roads":straightened_roads,"straightened_spans":stamped.roads.spans,"scope":"Land stamps projected individually on raw native terrain; roadside stamps projected individually on initial-land terrain. Complete sequential phase topologies recorded separately. No road painting or mesh fitting."}),
        false,
    )?;
    println!("{}", json!({"report":output}));
    Ok(())
}
