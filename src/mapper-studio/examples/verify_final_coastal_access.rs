//! Check saved final terrain through the production coastal gate, with negative controls.
//! CONFIG EXISTING_BUNDLE NEW_REPORT. Read-only inputs; no GUI, native DLL or server.
use mapper_studio::{Project, coastal_placement, prefab_assets, prefab_textures};
use papyrus_terrain::{monument_composition, native_layers::Layers, placement};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path},
};

#[derive(Deserialize)]
struct Layout {
    required_missing: usize,
    attempts: usize,
    outcomes: Vec<Outcome>,
}
#[derive(Deserialize)]
struct Outcome {
    id: u64,
    position: Option<Position>,
    distance: Option<f32>,
    within_radius: bool,
    reason: String,
}
#[derive(Deserialize)]
struct Position {
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    clearance: f32,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| e.to_string())
}
fn digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn main() -> Result<(), String> {
    // Match normal generation's operation-scoped source leases. Rechecking
    // several harbors must not rehash the same multi-GB bundles each time.
    let _source_verification = mapper_studio::installation::VerificationScope::default();
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("CONFIG EXISTING_BUNDLE NEW_REPORT".into());
    }
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report already exists".into());
    }
    let config: Value =
        serde_json::from_slice(&read(Path::new(&args[0]))?).map_err(|e| e.to_string())?;
    let path = |key: &str| -> Result<&Path, String> {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let bundle = Path::new(&args[1]);
    let manifest: Value =
        serde_json::from_slice(&read(&bundle.join("manifest.json"))?).map_err(|e| e.to_string())?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["composed"] != true
    {
        return Err("Expected composed preview bundle".into());
    }
    let receipts = manifest["files"].as_object().ok_or("Missing receipts")?;
    for (name, receipt) in receipts {
        if name.contains('\\')
            || Path::new(name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("Unsafe receipt path".into());
        }
        let file = bundle.join(name);
        let bytes = receipt["bytes"].as_u64().ok_or("Missing receipt size")?;
        if bytes > 1024 * 1024 * 1024
            || fs::metadata(&file).map_err(|e| e.to_string())?.len() != bytes
            || digest(&file)? != receipt["sha256"].as_str().ok_or("Missing receipt hash")?
        {
            return Err(format!("Receipt mismatch: {name}"));
        }
    }
    for required in [
        "project.mapper",
        "layout.json",
        "height.i16le",
        "biome.u8",
        "splat.u8",
        "topology.i32le",
        "alpha.u8",
    ] {
        if !receipts.contains_key(required) {
            return Err(format!("Missing receipt: {required}"));
        }
    }
    let project = Project::load(&bundle.join("project.mapper"))?;
    if project.placement_engine != coastal_placement::BUDGET_VERSION {
        return Err("Expected saved size-aware coastal revision 7".into());
    }
    let catalog = prefab_assets::load(
        path("rust_root")?,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings =
        prefab_textures::Bindings::load(path("rust_root")?, path("bindings")?, &catalog, || false)?;
    let layout: Layout =
        serde_json::from_slice(&read(&bundle.join("layout.json"))?).map_err(|e| e.to_string())?;
    let solution = placement::Solution {
        required_missing: layout.required_missing,
        attempts: layout.attempts,
        outcomes: layout
            .outcomes
            .into_iter()
            .map(|o| placement::Outcome {
                id: o.id,
                position: o.position.map(|p| placement::Position {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    yaw: p.yaw,
                    clearance: p.clearance,
                }),
                distance: o.distance,
                within_radius: o.within_radius,
                reason: o.reason,
            })
            .collect(),
    };
    let height = read(&bundle.join("height.i16le"))?;
    let topology = read(&bundle.join("topology.i32le"))?;
    if !height.len().is_multiple_of(2) || !topology.len().is_multiple_of(4) {
        return Err("Truncated raster words".into());
    }
    let layers = Layers {
        size: project.size,
        terrain_seed: project.terrain_key().seed,
        biome_seed: project
            .terrain_key()
            .biome_seed
            .unwrap_or(project.terrain_key().seed),
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
    let mut maps =
        monument_composition::compose(&layers, &read(&bundle.join("alpha.u8"))?, &[], || false)?
            .ok_or("Cancelled")?;
    if !coastal_placement::validate_final(&project, &solution, &catalog, &bindings, &maps, || {
        false
    })? {
        return Err("Validation cancelled".into());
    }
    let accepted_height = digest(&bundle.join("height.i16le"))?;
    // Test each placed harbor separately, so an earlier coastal target cannot mask a skipped one.
    maps.layers.height.fill(18000);
    let mut negative = Vec::new();
    for target in project.targets.iter().filter(|t| {
        t.prefab_path
            .as_deref()
            // Ferry Terminal shares this folder but has no Boat connector in
            // the captured installation. This fixture targets the two harbors.
            .is_some_and(|p| p.ends_with("/harbor_1.prefab") || p.ends_with("/harbor_2.prefab"))
    }) {
        let outcome = solution
            .outcomes
            .iter()
            .find(|o| o.id == target.instance_id && o.position.is_some())
            .ok_or("Expected placed harbor")?;
        let isolated = placement::Solution {
            outcomes: vec![outcome.clone()],
            required_missing: 0,
            attempts: 1,
        };
        let error = coastal_placement::validate_final(
            &project,
            &isolated,
            &catalog,
            &bindings,
            &maps,
            || false,
        )
        .expect_err("Blocked harbor was accepted");
        if !error.contains("Final terrain blocks boat access") {
            return Err(error);
        }
        negative.push(json!({"target":target.name,"id":target.instance_id,"error":error}));
    }
    if negative.len() != 2 {
        return Err("Expected both installed harbor variants".into());
    }
    if digest(&bundle.join("height.i16le"))? != accepted_height {
        return Err("Input height changed".into());
    }
    let report = json!({"format":"mapper-final-coastal-verification","schema":1,"engine":project.placement_engine,"bundle":bundle,"manifest_sha256":digest(&bundle.join("manifest.json"))?,"receipts_verified":receipts.len(),"final_terrain_accepted":true,"height_sha256":accepted_height,"isolated_blocked_harbors":negative,"inputs_unchanged":true,"gui_launched":false,"game_runtime_validated":false});
    File::options()
        .write(true)
        .create_new(true)
        .open(output)
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
