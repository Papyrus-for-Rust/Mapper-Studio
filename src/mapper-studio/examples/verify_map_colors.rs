//! Repaint a saved bundle using newly extracted client map colors, without generation.
//! EXISTING_BUNDLE PALETTE_JSON RUST_ROOT NEW_DIRECTORY
use mapper_studio::{Project, map_colors, map_image, terrain_palette::Palette};
use papyrus_terrain::native_layers::Layers;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};
fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| e.to_string())
}
fn hash(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = f.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("EXISTING_BUNDLE PALETTE_JSON RUST_ROOT NEW_DIRECTORY".into());
    }
    let bundle = Path::new(&args[0]);
    let output = Path::new(&args[3]);
    if output.exists() {
        return Err("Output directory already exists".into());
    }
    let project = Project::load(&bundle.join("project.mapper"))?;
    let palette = Palette::load(
        Path::new(&args[2]),
        Path::new(&args[1]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?
    .ok_or("Cancelled")?;
    if palette.cartography().is_none() {
        return Err("Palette lacks extracted map material".into());
    }
    let manifest: Value =
        serde_json::from_slice(&read(&bundle.join("manifest.json"))?).map_err(|e| e.to_string())?;
    for name in [
        "project.mapper",
        "height.i16le",
        "biome.u8",
        "splat.u8",
        "topology.i32le",
        "alpha.u8",
    ] {
        if hash(&bundle.join(name))?
            != manifest["files"][name]["sha256"]
                .as_str()
                .ok_or("Missing receipt")?
        {
            return Err(format!("Changed bundle input: {name}"));
        }
    }
    let height = read(&bundle.join("height.i16le"))?;
    let topology = read(&bundle.join("topology.i32le"))?;
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
        biome: read(&bundle.join("biome.u8"))?,
        splat: read(&bundle.join("splat.u8"))?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let alpha = read(&bundle.join("alpha.u8"))?;
    let mut old: Value = serde_json::from_slice(palette.bytes()).map_err(|e| e.to_string())?;
    old.as_object_mut().unwrap().remove("cartography");
    let old = Palette::decode(
        &serde_json::to_vec(&old).map_err(|e| e.to_string())?,
        project.installation.as_ref().unwrap(),
    )?;
    let mut previous_water: Value =
        serde_json::from_slice(palette.bytes()).map_err(|e| e.to_string())?;
    previous_water["cartography"]
        .as_object_mut()
        .ok_or("Missing map material")?
        .remove("shader_blob_sha256");
    let previous_water = Palette::decode(
        &serde_json::to_vec(&previous_water).map_err(|e| e.to_string())?,
        project.installation.as_ref().unwrap(),
    )?;
    fs::create_dir(output).map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    let mut custom = map_colors::Preferences {
        enabled: true,
        ..Default::default()
    };
    custom.overrides[4] = Some([72, 119, 46]);
    let styled = palette.with_display_colors(&custom);
    let restored = styled.with_display_colors(&map_colors::Preferences::default());
    for (name, colors) in [
        ("previous-terrain-colors", &old),
        ("installation-map-colors", &palette),
        ("custom-grass", &styled),
        ("restored-defaults", &restored),
        ("previous-water-response", &previous_water),
    ] {
        let start = std::time::Instant::now();
        let image = map_image::render_with_palette(
            &layers,
            Some(&alpha),
            None,
            2048,
            Some(colors),
            || false,
        )?
        .ok_or("Cancelled")?;
        let path = output.join(format!("{name}.png"));
        mapper_studio::image_export::write_png(&path, &image, || false)?;
        results.push(json!({"name":name,"sha256":hash(&path)?,"elapsed_ms":start.elapsed().as_millis(),"resolution":image.size}));
    }
    if results[1]["sha256"] != results[3]["sha256"] {
        return Err("Restored colors differ from exact installation defaults".into());
    }
    if results[1]["sha256"] == results[2]["sha256"] {
        return Err("Custom colors had no effect".into());
    }
    let report = json!({"format":"mapper-cartographic-image-verification","palette_sha256":palette.sha256,"material":palette.cartography(),"height_sha256":hash(&bundle.join("height.i16le"))?,"results":results,"terrain_regenerated":false,"gui_launched":false,"game_runtime_validated":false,"scope":"Existing composed terrain and alpha; ocean-plane water, no local-water or shoreline-distance parity claim"});
    File::options()
        .write(true)
        .create_new(true)
        .open(output.join("report.json"))
        .map_err(|e| e.to_string())?
        .write_all(
            serde_json::to_string_pretty(&report)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}
