//! Replay dungeon assembly over an existing verified world without regenerating terrain.
use mapper_studio::{Project, dungeon_source, monument_source, prefab_assets, prefab_textures};
use papyrus_terrain::placement::{Outcome, Position, Solution};
use sha2::{Digest, Sha256};
use std::path::Path;

fn layers(bundle: &Path, project: &Project) -> Result<papyrus_terrain::native_layers::Layers, String> {
    use std::io::Read;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["size"] != project.size
        || manifest["height_resolution"] != hr
        || manifest["surface_resolution"] != sr
    {
        return Err("Dungeon terrain bundle dimensions differ".into());
    }
    let read = |name: &str, bytes: usize| -> Result<Vec<u8>, String> {
        let mut data = Vec::new();
        std::fs::File::open(bundle.join(name))
            .map_err(|e| e.to_string())?
            .take(bytes as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() != bytes
            || manifest["files"][name]
                != serde_json::json!({"bytes":bytes,"sha256":format!("{:x}",Sha256::digest(&data))})
        {
            return Err(format!("Dungeon input receipt differs: {name}"));
        }
        Ok(data)
    };
    Ok(papyrus_terrain::native_layers::Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: read("height.i16le", hr * hr * 2)?
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect(),
        topology: read("topology.i32le", sr * sr * 4)?
            .chunks_exact(4)
            .map(|v| i32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .collect(),
        splat: read("splat.u8", sr * sr * 8)?,
        biome: read("biome.u8", sr * sr * 5)?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    })
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 6 {
        return Err(
            "inspect_dungeon_assembly BUNDLE RUST_ROOT CATALOG BINDINGS DUNGEON_CAPTURE NEW_REPORT"
                .into(),
        );
    }
    let bundle = Path::new(&args[0]);
    let root = Path::new(&args[1]);
    let output = Path::new(&args[5]);
    if output.exists() {
        return Err("Dungeon report must be new".into());
    }
    let _verification = mapper_studio::installation::VerificationScope::default();
    let project = Project::load(&bundle.join("project.mapper"))?;
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[2]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[3]), &catalog, || false)?;
    let monuments = monument_source::Source::cached(
        project
            .monument_rules
            .as_ref()
            .ok_or("Missing monument rules")?,
        &catalog,
        &bindings,
        || false,
    )?;
    let source =
        dungeon_source::Source::load(Path::new(&args[4]), root, &catalog, &monuments, || false)?;
    let layers = layers(bundle, &project)?;
    let layout: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle.join("layout.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut outcomes = Vec::new();
    for row in layout["outcomes"]
        .as_array()
        .ok_or("Missing layout outcomes")?
    {
        let p = &row["position"];
        let number = |name: &str| {
            p[name]
                .as_f64()
                .map(|v| v as f32)
                .ok_or_else(|| format!("Missing position {name}"))
        };
        outcomes.push(Outcome {
            id: row["id"].as_u64().ok_or("Missing target identity")?,
            position: if p.is_null() {
                None
            } else {
                Some(Position {
                    x: number("x")?,
                    y: number("y")?,
                    z: number("z")?,
                    yaw: number("yaw")?,
                    clearance: number("clearance")?,
                })
            },
            distance: row["distance"].as_f64().map(|v| v as f32),
            within_radius: row["within_radius"]
                .as_bool()
                .ok_or("Missing radius assessment")?,
            reason: row["reason"]
                .as_str()
                .ok_or("Missing outcome reason")?
                .into(),
        });
    }
    let solution = Solution {
        outcomes,
        required_missing: layout["required_missing"]
            .as_u64()
            .ok_or("Missing required count")? as usize,
        attempts: layout["attempts"].as_u64().ok_or("Missing search count")? as usize,
    };
    let started = std::time::Instant::now();
    let scene = source
        .generate(&project, &solution, &layers, || false)?
        .ok_or("Dungeon assembly cancelled")?;
    let elapsed = started.elapsed().as_secs_f64();
    let repeated = source
        .generate(&project, &solution, &layers, || false)?
        .ok_or("Repeated assembly cancelled")?;
    if scene != repeated {
        return Err("Dungeon assembly changed during deterministic replay".into());
    }
    if source
        .generate(&project, &solution, &layers, || true)?
        .is_some()
    {
        return Err("Cancelled assembly published a scene".into());
    }
    let report = serde_json::json!({"source":source.audit(),"seconds":elapsed,"repeatable":true,"cancelled_without_publication":true,"scene":scene,"game_runtime_validated":false});
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &report).map_err(|e| e.to_string())?;
    println!(
        "{} dungeon instances in {:.3} seconds",
        scene.instances.len(),
        elapsed
    );
    Ok(())
}
