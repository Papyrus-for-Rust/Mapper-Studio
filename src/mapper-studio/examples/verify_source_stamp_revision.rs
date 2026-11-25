//! Same-pose comparison only: never publishes or certifies a playable map.
use mapper_studio::{
    Project, composition, installation, native_processing, prefab_assets, prefab_textures::Bindings,
};
use papyrus_terrain::{
    monument_composition::{self, Composed},
    placement::{Outcome, Position, Solution},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds bound".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn layout(value: &Value) -> Result<Solution, String> {
    let rows = value["outcomes"]
        .as_array()
        .filter(|r| r.len() <= 128)
        .ok_or("Invalid outcome count")?;
    let mut outcomes = Vec::new();
    for row in rows {
        let p = &row["position"];
        let scalar = |key: &str| -> Result<f32, String> {
            let v = p[key].as_f64().ok_or("Missing pose")? as f32;
            if !v.is_finite() {
                Err("Invalid pose".into())
            } else {
                Ok(v)
            }
        };
        outcomes.push(Outcome {
            id: row["id"].as_u64().ok_or("Missing id")?,
            position: if p.is_null() {
                None
            } else {
                Some(Position {
                    x: scalar("x")?,
                    y: scalar("y")?,
                    z: scalar("z")?,
                    yaw: scalar("yaw")?,
                    clearance: scalar("clearance")?,
                })
            },
            distance: row["distance"].as_f64().map(|v| v as f32),
            within_radius: row["within_radius"]
                .as_bool()
                .ok_or("Missing radius flag")?,
            reason: row["reason"].as_str().ok_or("Missing reason")?.into(),
        });
    }
    Ok(Solution {
        outcomes,
        required_missing: value["required_missing"]
            .as_u64()
            .ok_or("Missing deficit")? as usize,
        attempts: value["attempts"].as_u64().ok_or("Missing attempts")? as usize,
    })
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("CONFIG FROZEN_FIXTURE NEW_REPORT".into());
    }
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let _verification = installation::VerificationScope::default();
    let config: Value = serde_json::from_slice(&read(args[0].as_ref(), 4 * 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let revision = match config.get("stamp_revision").and_then(Value::as_u64) {
        None if config.get("stamp_revision").is_none() => {
            monument_composition::SOURCE_TEXTURE_VERSION
        }
        Some(3) => monument_composition::SOURCE_TEXTURE_VERSION,
        Some(4) => monument_composition::SOURCE_CLIFF_VERSION,
        Some(5) => monument_composition::SOURCE_RASTER_VERSION,
        Some(6) => monument_composition::SOURCE_HEIGHT_VERSION,
        _ => return Err("stamp_revision must be 3, 4, 5 or 6".into()),
    };
    let path = |key: &str| -> Result<PathBuf, String> {
        config[key]
            .as_str()
            .map(PathBuf::from)
            .ok_or(format!("Missing {key}"))
    };
    let fixture = Path::new(&args[1]);
    let manifest: Value =
        serde_json::from_slice(&read(&fixture.join("manifest.json"), 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    if manifest["format"] != "mapper-private-pre-road-fixture-1" {
        return Err("Wrong fixture format".into());
    }
    let checked = |name: &str| -> Result<Vec<u8>, String> {
        let bytes = read(&fixture.join(name), 512 * 1024 * 1024)?;
        if manifest["files"][name]["sha256"] != hash(&bytes)
            || manifest["files"][name]["bytes"] != bytes.len() as u64
        {
            return Err(format!("Fixture changed: {name}"));
        }
        Ok(bytes)
    };
    let project: Project =
        serde_json::from_slice(&checked("project.mapper")?).map_err(|e| e.to_string())?;
    project.validate()?;
    let solution =
        layout(&serde_json::from_slice(&checked("layout.json")?).map_err(|e| e.to_string())?)?;
    let root = path("rust_root")?;
    let catalog = prefab_assets::load(
        &root,
        &path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = Bindings::load(&root, &path("bindings")?, &catalog, || false)?;
    let processed = native_processing::build(
        &root,
        &path("worker")?,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    let legacy = composition::compose_monuments(
        &processed.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let height_bytes = legacy
        .layers
        .height
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let topo_bytes = legacy
        .layers
        .topology
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    for (name, bytes) in [
        ("height.i16le", height_bytes.as_slice()),
        ("topology.i32le", topo_bytes.as_slice()),
        ("splat.u8", legacy.layers.splat.as_slice()),
        ("biome.u8", legacy.layers.biome.as_slice()),
        ("alpha.u8", legacy.alpha.as_slice()),
    ] {
        if checked(name)? != bytes {
            return Err(format!("Legacy baseline differs: {name}"));
        }
    }
    let mut revised = project.clone();
    revised.composition_engine = Some(revision.into());
    revised.validate()?;
    let changed = composition::compose_monuments(
        &processed.source,
        &revised,
        &solution,
        &catalog,
        &bindings,
        || false,
    );
    let mut source_control = Value::Null;
    let mut raster_control = Value::Null;
    let mut height_control = Value::Null;
    let report = match changed {
        Ok(Some(ref maps)) => {
            if matches!(
                revision,
                monument_composition::SOURCE_CLIFF_VERSION
                    | monument_composition::SOURCE_RASTER_VERSION
                    | monument_composition::SOURCE_HEIGHT_VERSION
            ) {
                let prior = if revision == monument_composition::SOURCE_HEIGHT_VERSION {
                    monument_composition::SOURCE_RASTER_VERSION
                } else if revision == monument_composition::SOURCE_RASTER_VERSION {
                    monument_composition::SOURCE_CLIFF_VERSION
                } else {
                    monument_composition::SOURCE_TEXTURE_VERSION
                };
                let mut control = revised.clone();
                control.composition_engine = Some(prior.into());
                let control = composition::compose_monuments(
                    &processed.source,
                    &control,
                    &solution,
                    &catalog,
                    &bindings,
                    || false,
                )?
                .ok_or("Control cancelled")?;
                if revision == monument_composition::SOURCE_CLIFF_VERSION
                    && (control.layers.height != maps.layers.height
                        || control.alpha != maps.alpha
                        || control.layers.biome != maps.layers.biome)
                {
                    return Err("Cliff revision changed unrelated height/alpha/biome output".into());
                }
                if revision == monument_composition::SOURCE_HEIGHT_VERSION {
                    if control.alpha != maps.alpha || control.layers.biome != maps.layers.biome {
                        return Err("Height revision changed unrelated alpha/biome output".into());
                    }
                    height_control = serde_json::json!({"engine":prior,"against_legacy":compare(&legacy,&control),"height_against_raster":compare(&control,maps)});
                } else if revision == monument_composition::SOURCE_RASTER_VERSION {
                    raster_control = serde_json::json!({"engine":prior,"against_legacy":compare(&legacy,&control),"raster_against_cliff":compare(&control,maps)});
                } else {
                    source_control = serde_json::json!({"engine":prior,
                        "against_legacy":compare(&legacy,&control),"cliff_against_texture":compare(&control,maps)});
                }
            }
            compare(&legacy, maps)
        }
        Ok(None) => return Err("Cancelled".into()),
        Err(error) => serde_json::json!({"completed":false,"error":error}),
    };
    let completed = report["completed"] == true;
    let report = serde_json::json!({"format":"mapper-source-stamp-revision-audit-1","legacy_fixture_exact":true,"engine":revision,"source_texture_control":source_control,"source_cliff_control":raster_control,"source_raster_control":height_control,
  "terrain_seed_unchanged":project.terrain_key()==revised.terrain_key(),"fixed_poses":true,"placement_reassessed":false,"comparison":report,"full_map_validated":false});
    serde_json::to_writer_pretty(
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(output)
            .map_err(|e| e.to_string())?,
        &report,
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    if !completed {
        return Err("Source revision composition rejected; audit saved".into());
    }
    Ok(())
}
fn compare(a: &Composed, b: &Composed) -> Value {
    let count = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(a, b)| a != b).count();
    let height = a
        .layers
        .height
        .iter()
        .zip(&b.layers.height)
        .filter(|(a, b)| a != b)
        .count();
    let maximum_height = a
        .layers
        .height
        .iter()
        .zip(&b.layers.height)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
        .max()
        .unwrap_or(0);
    serde_json::json!({"completed":true,"height_samples_changed":height,"maximum_height_units_changed":maximum_height,
  "splat_bytes_changed":count(&a.layers.splat,&b.layers.splat),"biome_bytes_changed":count(&a.layers.biome,&b.layers.biome),"alpha_cells_changed":count(&a.alpha,&b.alpha),
  "topology_cells_changed":a.layers.topology.iter().zip(&b.layers.topology).filter(|(a,b)|a!=b).count(),"ocean_rebuilt":b.ocean.is_some()})
}
