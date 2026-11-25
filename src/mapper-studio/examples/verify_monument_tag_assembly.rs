//! Assemble captured tags at saved map placements without executing game code.
use mapper_studio::{Project, monument_tag_geometry::Prepared, prefab_export::monument_pose};
use papyrus_terrain::{
    decor_stream::Budget,
    environment::{Coverage, Verdict},
    water_treatment_clearance::Clearance,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("Assembly fixture input exceeds its bound".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn scalar(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or("Invalid saved monument pose".into())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(5..=6).contains(&args.len())
        || args
            .get(5)
            .is_some_and(|arg| arg != "--first-collider-fixture")
    {
        return Err(
            "TAG_CAPTURE SHA256 PROJECT LAYOUT NEW_OUTPUT_JSON [--first-collider-fixture]".into(),
        );
    }
    if Path::new(&args[4]).exists() {
        return Err("Existing assembly verification was retained".into());
    }
    let first_fixture = args.len() == 6;
    let capture_bytes = read(Path::new(&args[0]), 32 * 1024 * 1024)?;
    let capture_sha = hash(&capture_bytes);
    if args[1].to_str() != Some(capture_sha.as_str()) {
        return Err("Tag capture hash differs".into());
    }
    let capture: Value = serde_json::from_slice(&capture_bytes)?;
    if capture["format"] != "mapper-monument-tag-reference" || capture["schema"] != 1 {
        return Err("Unsupported tag capture".into());
    }
    let project_bytes = read(Path::new(&args[2]), 16 * 1024 * 1024)?;
    let project: Project = serde_json::from_slice(&project_bytes)?;
    project.validate()?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing fixture installation")?;
    let sources = capture["sources"]
        .as_array()
        .ok_or("Missing capture sources")?;
    for pin in [
        Some(&installation.native),
        installation.scene_manifest.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if !sources.contains(&serde_json::to_value(pin)?) {
            return Err("Tag capture differs from the saved installation".into());
        }
    }
    let layout_bytes = read(Path::new(&args[3]), 16 * 1024 * 1024)?;
    let layout: Value = serde_json::from_slice(&layout_bytes)?;
    let outcomes = layout["outcomes"]
        .as_array()
        .ok_or("Missing placement outcomes")?;
    if outcomes.len() != project.targets.len() {
        return Err("Placement roster differs from project targets".into());
    }
    let prefabs = capture["prefabs"]
        .as_object()
        .ok_or("Missing prefab tag capture")?;
    let targets: BTreeMap<_, _> = project
        .targets
        .iter()
        .map(|target| (target.instance_id, target))
        .collect();
    let mut prepared = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut instances = Vec::new();
    let mut boxes = Vec::new();
    let mut pending = 0;
    for outcome in outcomes {
        let id = outcome["id"].as_u64().ok_or("Missing monument instance")?;
        if !seen.insert(id) {
            return Err("Duplicate monument instance".into());
        }
        let target = targets.get(&id).ok_or("Unknown monument instance")?;
        let p = outcome.get("position").ok_or("Missing position outcome")?;
        if p.is_null() {
            continue;
        }
        let path = target
            .prefab_path
            .as_deref()
            .ok_or("Placed monument lacks its prefab path")?;
        let raw = prefabs
            .get(path)
            .ok_or("Placed monument lacks tag capture")?;
        if !prepared.contains_key(path) {
            prepared.insert(path.to_owned(), Prepared::read(&serde_json::to_vec(raw)?)?);
        }
        let pose = monument_pose(
            [scalar(&p["x"])?, scalar(&p["y"])?, scalar(&p["z"])?],
            scalar(&p["yaw"])?,
        )?;
        let mut selected = BTreeMap::new();
        if first_fixture {
            for tag in raw["tags"].as_array().ok_or("Missing tag roster")? {
                if let Some(collider) = tag["colliders"]
                    .as_array()
                    .ok_or("Missing collider roster")?
                    .first()
                {
                    selected.insert(
                        tag["object_id"].as_i64().ok_or("Missing tag ID")?,
                        collider["object_id"]
                            .as_i64()
                            .ok_or("Missing collider ID")?,
                    );
                }
            }
        }
        let assembly = prepared[path]
            .assemble(id, pose, &selected, || false)?
            .ok_or("Fixture cancelled")?;
        pending += assembly.pending.len();
        boxes.extend_from_slice(&assembly.boxes);
        instances.push(json!({"instance":id,"prefab":path,"pose":pose,"assembly":assembly}));
    }
    let clearance = Clearance::new(&boxes, 7., Coverage::Incomplete)?;
    let assessment = clearance.check([0.; 3], &mut Budget::new(10000)?, || false)?;
    if assessment.verdict != Verdict::Incomplete {
        return Err("Partial tag providers established world clearance".into());
    }
    let output = json!({"format":"mapper-monument-tag-assembly-verification","schema":1,
        "capture_sha256":capture_sha,"project_sha256":hash(&project_bytes),"layout_sha256":hash(&layout_bytes),
        "instances":instances,"prepared_prefabs":prepared.len(),"resolved_boxes":boxes.len(),"pending_tags":pending,
        "collider_selection_fixture":if first_fixture {"first serialized collider"} else {"unambiguous colliders only"},
        "partial_world_clearance":assessment,"complete_world_registry":false,"normal_generation_integrated":false,"game_code_executed":false});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[4])?;
    serde_json::to_writer(&mut file, &output)?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"instances":instances.len(),"prepared_prefabs":prepared.len(),"resolved_boxes":boxes.len(),"pending_tags":pending})
    );
    Ok(())
}
