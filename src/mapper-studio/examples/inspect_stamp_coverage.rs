//! Inspect actual paint coverage independently of a rendered preview's alpha.
use mapper_studio::{installation, prefab_assets, prefab_textures, splat_preview};
use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 4 {
        return Err("inspect_stamp_coverage RUST_ROOT CATALOG BINDINGS PREFAB_NAME...".into());
    }
    let root = Path::new(&args[0]);
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    let mut results = Vec::new();
    for name in &args[3..] {
        let suffix = format!("/{name}.prefab");
        let (path, asset) = catalog
            .entries
            .iter()
            .find(|(p, _)| p.ends_with(&suffix))
            .ok_or_else(|| format!("Missing source prefab: {name}"))?;
        let preview = splat_preview::load(&bindings, asset, || false)?.ok_or("Cancelled")?;
        let material = preview
            .material_weights
            .as_ref()
            .ok_or("No material weights")?;
        let [w, h] = material.size;
        let union = splat_preview::palette_coverage(material, preview.biome_weights.as_ref());
        let mut partial = [0_usize; 2];
        let mut nonzero = [0_usize; 2];
        for (weights, pixel) in material.pixels.iter().zip(&union.pixels) {
            let paint = weights.iter().map(|v| u32::from(*v)).sum::<u32>().min(255);
            for (i, value) in [paint, u32::from(pixel.a())].into_iter().enumerate() {
                partial[i] += usize::from(value > 0 && value < 255);
                nonzero[i] += usize::from(value > 0);
            }
        }
        results.push(serde_json::json!({"prefab":path,"resolution":[w,h],"samples":w*h,"partial_material_union":partial,"nonzero_material_union":nonzero}));
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"format":"mapper-source-stamp-coverage","schema":1,"catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"results":results})).map_err(|e|e.to_string())?);
    Ok(())
}
