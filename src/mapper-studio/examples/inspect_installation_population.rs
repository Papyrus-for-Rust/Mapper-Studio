//! Headless source/catalog/scene audit. Does not start Rust or generate a map.
use mapper_studio::{installation, monument_stages, prefab_assets};
use std::path::Path;

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("inspect_installation_population <Rust root> <catalog.json> <stages.json> <new report.json>".into());
    }
    let root = Path::new(&args[0]);
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let stages = monument_stages::Source::load(Path::new(&args[2]), root, &catalog, || false)?;
    let populations: Vec<_> = [1000, 3500, 3999, 4000, 4500, 5000, 6000]
        .into_iter()
        .map(|size| {
            stages.population(&catalog, size).map(|pool|
            serde_json::json!({"size":size,"candidate_count":pool.len(),"candidates":pool}))
        })
        .collect::<Result<_, _>>()?;
    let report = serde_json::json!({
        "format":"papyrus-installation-population-audit", "schema":1,
        "catalog_sha256":catalog.sha256, "stage_sha256":stages.sha256,
        "catalog_count":catalog.entries.len(),"land_stage_count":stages.program_count(),
        "separate_monument_program_count":stages.unsupported_family_count(),
        "full_world_execution_validated":false,"populations":populations,
    });
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[3])
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Verified {} catalog roots and {} land-stage programs; report: {}",
        catalog.entries.len(),
        stages.program_count(),
        Path::new(&args[3]).display()
    );
    Ok(())
}
