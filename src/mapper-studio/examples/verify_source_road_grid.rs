//! CONFIG SAVED_BUNDLE NEW_REPORT. Read-only installation-backed lattice study.
//! Empty placement mask by design; this is not a generated road network.
use mapper_studio::{Project, infrastructure::Source, prefab_assets};
use papyrus_terrain::native_layers::Layers;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Instant,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Input exceeds diagnostic limit".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn cost_hash(costs: &[u32]) -> String {
    let mut hash = Sha256::new();
    for cost in costs {
        hash.update(cost.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("CONFIG SAVED_BUNDLE NEW_REPORT".into());
    }
    let config: Value = serde_json::from_slice(&read(Path::new(&args[0]), 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let bundle = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let manifest: Value =
        serde_json::from_slice(&read(&bundle.join("manifest.json"), 4 * 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    let checked = |name: &str| -> Result<Vec<u8>, String> {
        let bytes = read(&bundle.join(name), 128 * 1024 * 1024)?;
        if Some(hash(&bytes).as_str()) != manifest["files"][name]["sha256"].as_str() {
            return Err(format!("Changed bundle input: {name}"));
        }
        Ok(bytes)
    };
    let project_bytes = checked("project.mapper")?;
    let project = Project::load(&bundle.join("project.mapper"))?;
    let profile = project
        .roads
        .as_ref()
        .and_then(|r| r.ring_costmap.as_ref())
        .ok_or("Saved fixture lacks road costs")?;
    let path = |key: &str| -> Result<&Path, String> {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let catalog = prefab_assets::load(
        path("rust_root")?,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let source = Source::load(path("infrastructure")?, &catalog, || false)?;
    profile.check(&source)?;
    let height = checked("height.i16le")?;
    let topology = checked("topology.i32le")?;
    let layers = Layers {
        size: project.size,
        terrain_seed: project.terrain_key().seed,
        biome_seed: project.terrain_key().biome_seed.unwrap_or(0),
        height_resolution: manifest["height_resolution"]
            .as_u64()
            .ok_or("Missing height resolution")? as usize,
        surface_resolution: manifest["surface_resolution"]
            .as_u64()
            .ok_or("Missing surface resolution")? as usize,
        height: height
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect(),
        topology: topology
            .chunks_exact(4)
            .map(|v| i32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .collect(),
        biome: checked("biome.u8")?,
        splat: checked("splat.u8")?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let placement = vec![0; layers.surface_resolution * layers.surface_resolution];
    let mut results = Vec::new();
    for trail in [false, true] {
        let before = Instant::now();
        let legacy = profile
            .build_family(&layers, &placement, project.seeds[3].value, trail, || false)?
            .ok_or("Cancelled")?;
        let legacy_ms = before.elapsed().as_millis();
        let before = Instant::now();
        let cells = profile
            .build_cell_family(
                &source,
                &layers,
                &placement,
                project.seeds[3].value,
                trail,
                || false,
            )?
            .ok_or("Cancelled")?;
        results.push(json!({"trail":trail,
            "legacy":{"engine":legacy.engine,"resolution":legacy.resolution,"costs_sha256":cost_hash(&legacy.costs),"counts":legacy.counts,"work":legacy.work,"elapsed_ms":legacy_ms},
            "source_cells":{"lattice":cells.lattice,"engine":cells.grid.engine,"resolution":cells.grid.resolution,
                "costs_sha256":cost_hash(&cells.grid.costs),"counts":cells.grid.counts,"work":cells.grid.work,"elapsed_ms":before.elapsed().as_millis(),
                "first_world_point":cells.lattice.point([0,0]),"last_world_point":cells.lattice.point([cells.grid.resolution-1;2])}}));
    }
    if checked("project.mapper")? != project_bytes
        || hash(&checked("height.i16le")?) != hash(&height)
        || hash(&checked("topology.i32le")?) != hash(&topology)
    {
        return Err("Bundle changed during study".into());
    }
    let report = json!({"format":"mapper-installation-cell-road-grid-study-1","size":project.size,
        "source_sha256":source.sha256,"profile":profile,"project_sha256":hash(&project_bytes),
        "height_sha256":hash(&height),"topology_sha256":hash(&topology),"results":results,
        "scope":"Saved terrain/topology, explicitly empty placement mask; no new network, mesh, terrain variant or project is published",
        "gui_launched":false,"runtime_validated":false});
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    fs::File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&bytes)
        .map_err(|e| e.to_string())?;
    println!(
        "Verified source-lattice costs for both paved and dirt profiles; report {}",
        output.display()
    );
    Ok(())
}
