use mapper_studio::{installation, prefab_assets, prefab_textures};
use std::path::Path;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "Usage: inspect_prefab_textures <Rust root> <catalog.json> <bindings.json>".into(),
        );
    }
    let root = Path::new(&args[0]);
    let source = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &source, || false)?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    println!(
        "Verified {} exact GUID textures; receipt {}",
        bindings.textures().len(),
        bindings.sha256
    );
    for path in [
        "xlarge/launch_site_1",
        "small/satellite_dish",
        "large/airfield_1",
    ] {
        let asset =
            &catalog.entries[&format!("assets/bundled/prefabs/autospawn/monument/{path}.prefab")];
        println!("{path}: {} resolved channels", asset.texture_guids.len());
        for (channel, guid) in &asset.texture_guids {
            let texture = &bindings.textures()[guid];
            println!(
                "  {channel}: {} ({} x {})",
                texture.asset_path, texture.width, texture.height
            );
        }
        let preview = mapper_studio::splat_preview::load(&bindings, asset, || false)?
            .ok_or("Preview cancelled")?;
        let painted = preview.image.pixels.iter().filter(|p| p.a() > 0).count();
        if painted == 0 {
            return Err(format!("No visible splat pixels for {path}"));
        }
        println!(
            "  Cached splat preview: {:?}; {painted} painted pixels; geometry {:?}",
            preview.image.size, preview.geometry
        );
    }
    Ok(())
}
