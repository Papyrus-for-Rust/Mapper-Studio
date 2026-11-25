//! Rebuild final water classification on a hash-checked saved preview, without loading game binaries.
use mapper_studio::{Project, profile};
use papyrus_terrain::{
    monument_composition,
    native_layers::{Layers, dimensions},
};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};
fn read(
    root: &Path,
    manifest: &serde_json::Value,
    name: &str,
    expected: usize,
) -> Result<Vec<u8>, String> {
    let path = root.join(name);
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() != expected as u64 {
        return Err(format!("Invalid {name} length"));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if manifest["files"][name]["bytes"] != expected as u64
        || manifest["files"][name]["sha256"] != format!("{:x}", Sha256::digest(&bytes))
    {
        return Err(format!("Invalid {name} receipt"));
    }
    Ok(bytes)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("verify_composed_water <existing preview bundle> <report>".into());
    }
    let root = Path::new(&args[0]);
    let manifest_path = root.join("manifest.json");
    if std::fs::metadata(&manifest_path)
        .map_err(|e| e.to_string())?
        .len()
        > 1024 * 1024
    {
        return Err("Manifest too large".into());
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["playable"] != false
    {
        return Err("Unsupported preview bundle".into());
    }
    let project = Project::load(&root.join("project.mapper"))?;
    let (hr, sr) = dimensions(project.size)?;
    let cells = sr * sr;
    if manifest["height_resolution"] != hr || manifest["surface_resolution"] != sr {
        return Err("Unexpected grid dimensions".into());
    }
    let height = read(root, &manifest, "height.i16le", hr * hr * 2)?
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let topology = read(root, &manifest, "topology.i32le", cells * 4)?
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let layers = Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height,
        topology,
        biome: read(root, &manifest, "biome.u8", cells * 5)?,
        splat: read(root, &manifest, "splat.u8", cells * 8)?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let alpha = read(root, &manifest, "alpha.u8", cells)?;
    let start = Instant::now();
    let result = monument_composition::compose_with_water(&layers, &alpha, &[], || false)?
        .ok_or("Cancelled")?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.;
    let again = monument_composition::compose_with_water(&layers, &alpha, &[], || false)?
        .ok_or("Cancelled")?;
    if result.ocean != again.ocean || result.layers.topology != again.layers.topology {
        return Err("Not repeatable".into());
    }
    if result.layers.height != layers.height
        || result.layers.splat != layers.splat
        || result.layers.biome != layers.biome
        || result.alpha != alpha
    {
        return Err("Water reconstruction changed unrelated layers".into());
    }
    if layers
        .topology
        .iter()
        .zip(&result.layers.topology)
        .any(|(&a, &b)| a & !papyrus_terrain::ocean::OCEAN_BIT != b & !papyrus_terrain::ocean::OCEAN_BIT)
    {
        return Err("Changed unrelated topology bits".into());
    }
    let ocean = result.ocean.as_ref().unwrap();
    if ocean.topology != result.layers.topology || ocean.local_water.iter().any(|&h| h != 0) {
        return Err("Water output mismatch".into());
    }
    let report = serde_json::json!({"engine":monument_composition::WATER_VERSION,"input":"hash-checked previously saved composed preview; no live installation revalidation","size":project.size,"height_resolution":hr,"surface_resolution":sr,"ocean_cells":ocean.ocean_cells,"enclosed_cells":ocean.enclosed_cells,"changed_ocean_topology_cells":ocean.changed_topology_cells,"local_water_body_reconstruction":false,"unrelated_layers_preserved":true,"repeatable":true,"reconstruction_ms":elapsed,"playable":false,"game_validated":false});
    profile::write_document(Path::new(&args[1]), &report, true)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}
