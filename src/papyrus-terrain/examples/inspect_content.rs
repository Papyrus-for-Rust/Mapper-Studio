//! Read-only diagnostic for real map content fixtures.
use std::{fs::File, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: inspect_content <map>")?,
    );
    let bytes = papyrus_terrain::content::read_source(File::open(path)?)?;
    let map = papyrus_terrain::content::read_map(&bytes, || false)?;
    println!(
        "{}",
        serde_json::json!({"size":map.size,"source_bytes":bytes.len(),"prefabs":map.prefabs.len(),"paths":map.paths.len(),"layers":map.layers.iter().map(|l|serde_json::json!({"name":l.name,"bytes":l.data.len()})).collect::<Vec<_>>(),"unknown_world_fields":map.unknown_fields})
    );
    Ok(())
}
