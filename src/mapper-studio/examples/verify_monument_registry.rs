//! Verify prepared authored volumes against accepted saved monument placements.
use mapper_studio::{
    Project, installation,
    monument_tag_geometry::{Placement, Registry},
    monument_tag_source::{self, Source},
    prefab_assets,
    prefab_export::monument_pose,
};
use papyrus_terrain::{
    decor_stream::Budget,
    environment::{Coverage, Verdict},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

fn read(path: &Path, limit: u64) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("Registry fixture exceeds its input bound".into());
    }
    Ok(bytes)
}
fn scalar(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
        .ok_or("Invalid saved monument pose".into())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(6..=7).contains(&args.len()) || args.get(6).is_some_and(|value| value != "--prepare") {
        return Err(
            "RUST_ROOT CATALOG PROJECT LAYOUT TAG_CAPTURE NEW_OUTPUT_JSON [--prepare]".into(),
        );
    }
    let output = Path::new(&args[5]);
    if output.exists() {
        return Err("Existing registry verification was retained".into());
    }
    let _verification = installation::VerificationScope::default();
    let root = Path::new(&args[0]);
    let project = Project::load(Path::new(&args[2]))?;
    let expected = project
        .installation
        .as_ref()
        .ok_or("Missing saved installation")?;
    let catalog_path = Path::new(&args[1]);
    let catalog = prefab_assets::load(root, catalog_path, expected, || false)?;
    let source = Source::load(root, Path::new(&args[4]), &catalog, || false)?;
    if args.len() == 7 {
        for _ in 0..2 {
            let prepared = monument_tag_source::prepare(root, catalog_path, &catalog, || false)?;
            if prepared.sha256() != source.sha256() {
                return Err("Prepared tag source differs from the verified capture".into());
            }
        }
    }
    let layout_bytes = read(Path::new(&args[3]), 16 * 1024 * 1024)?;
    let layout: Value = serde_json::from_slice(&layout_bytes)?;
    let outcomes = layout["outcomes"]
        .as_array()
        .ok_or("Missing saved placement outcomes")?;
    if outcomes.len() != project.targets.len() {
        return Err("Saved layout roster differs from the project".into());
    }
    let targets: BTreeMap<_, _> = project
        .targets
        .iter()
        .map(|target| (target.instance_id, target))
        .collect();
    let mut placements = Vec::new();
    for outcome in outcomes {
        let position = outcome
            .get("position")
            .ok_or("Missing placement position")?;
        if position.is_null() {
            continue;
        }
        let instance = outcome["id"].as_u64().ok_or("Missing placement instance")?;
        let target = targets.get(&instance).ok_or("Unknown placement instance")?;
        let prefab = target
            .prefab_path
            .as_deref()
            .ok_or("Placed monument lacks a prefab path")?;
        placements.push(Placement {
            instance,
            prefab,
            pose: monument_pose(
                [
                    scalar(&position["x"])?,
                    scalar(&position["y"])?,
                    scalar(&position["z"])?,
                ],
                scalar(&position["yaw"])?,
            )?,
        });
    }
    let registry = Registry::assemble(
        source.prefabs(),
        &placements,
        "water_treatment_plant",
        || false,
    )?
    .ok_or("Registry cancelled")?;
    if registry.coverage() != Coverage::Complete {
        return Err("Accepted monument registry retains unresolved geometry".into());
    }
    let clearance = registry.clearance(7.)?;
    let mut work = Budget::new(1_000_000)?;
    for (_, bounds) in registry.boxes() {
        if !matches!(
            clearance.check(bounds.center, &mut work, || false)?.verdict,
            Verdict::Blocked(_)
        ) {
            return Err("Authored registry failed to block a registered volume centre".into());
        }
    }
    if clearance.check([50000.; 3], &mut work, || false)?.verdict != Verdict::Clear {
        return Err("Authored registry failed its remote clearance fixture".into());
    }
    let without_plant: Vec<_> = placements
        .iter()
        .filter(|placement| {
            !registry
                .matched_monuments()
                .iter()
                .any(|key| key.instance == placement.instance)
        })
        .map(|placement| Placement {
            instance: placement.instance,
            prefab: placement.prefab,
            pose: placement.pose,
        })
        .collect();
    let no_plant = Registry::assemble(
        source.prefabs(),
        &without_plant,
        "water_treatment_plant",
        || false,
    )?
    .ok_or("Registry cancelled")?;
    if !no_plant.matched_monuments().is_empty() {
        return Err("Removed plant still enabled its registry gate".into());
    }
    let plant = source
        .prefabs()
        .iter()
        .find_map(
            |(path, prefab)| match prefab.matching_monuments("water_treatment_plant") {
                Ok(found) if !found.is_empty() => Some(Ok(path.as_str())),
                Err(error) => Some(Err(error)),
                _ => None,
            },
        )
        .ok_or("No captured water-treatment plant for the positive gate fixture")??;
    let mut with_plant: Vec<_> = placements
        .iter()
        .map(|placement| Placement {
            instance: placement.instance,
            prefab: placement.prefab,
            pose: placement.pose,
        })
        .collect();
    let plant_instance = placements
        .iter()
        .map(|placement| placement.instance)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("Fixture instance ID exhausted")?;
    let plant_pose = monument_pose([0., 20., 0.], 30.)?;
    with_plant.push(Placement {
        instance: plant_instance,
        prefab: plant,
        pose: plant_pose,
    });
    let positive = Registry::assemble(
        source.prefabs(),
        &with_plant,
        "water_treatment_plant",
        || false,
    )?
    .ok_or("Positive gate fixture cancelled")?;
    if positive.matched_monuments().len() <= registry.matched_monuments().len()
        || positive.coverage() != Coverage::Complete
    {
        return Err("Added water-treatment plant failed the positive gate fixture".into());
    }
    let report = json!({"format":"mapper-authored-monument-registry-verification","schema":1,
        "source_sha256":source.sha256(),"captured_catalog_sha256":source.captured_catalog_sha256(),
        "selected_catalog_sha256":catalog.sha256,"layout_sha256":format!("{:x}",Sha256::digest(&layout_bytes)),
        "cached_prefabs":source.prefabs().len(),"placements":placements.iter().map(|placement|json!({
            "instance":placement.instance,"prefab":placement.prefab,"pose":placement.pose,
        })).collect::<Vec<_>>(),"registry":registry,"clearance_work":work.used(),
        "removed_plant_gate_is_disabled":true,"scope":"accepted initial layout monuments",
        "positive_gate_fixture":{"added_placement":{"instance":plant_instance,"prefab":plant,"pose":plant_pose},"registry":positive},
        "normal_generation_integrated":false,"game_code_executed":false});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"source_sha256":source.sha256(),"prefabs":source.prefabs().len(),"placements":placements.len(),
        "volumes":registry.boxes().len(),"pending":registry.pending().len(),"matched_monuments":registry.matched_monuments().len()})
    );
    Ok(())
}
