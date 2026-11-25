use mapper_studio::{installation, prefab_assets};
use std::path::Path;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: inspect_prefab_catalog <Rust root> <catalog.json>".into());
    }
    let root = Path::new(&args[0]);
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    println!(
        "Source-verified numeric catalog {}: {} entries",
        catalog.sha256,
        catalog.entries.len()
    );
    for name in [
        "xlarge/launch_site_1",
        "small/satellite_dish",
        "large/airfield_1",
    ] {
        let path = format!("assets/bundled/prefabs/autospawn/monument/{name}.prefab");
        if let Some(asset) = catalog.entries.get(&path) {
            println!(
                "{name}: bounds {:?}; {} GUID references; modifier {:?}",
                asset.bounds,
                asset.texture_guids.len(),
                asset.modifier_size
            );
        }
    }
    Ok(())
}
