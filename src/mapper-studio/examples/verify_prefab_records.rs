//! Prepare authoritative prefab records from a receipted composed preview. No .map is published.
use mapper_studio::{Project, prefab_assets, prefab_export, prefab_identities};
use papyrus_terrain::prefab_records;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
fn json(path: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    std::fs::File::open(path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 64 * 1024 * 1024 {
        return Err("Record input exceeds 64 MiB".into());
    }
    Ok(serde_json::from_slice(&raw)?)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().map(PathBuf::from).collect();
    if args.len() != 6 {
        return Err(
            "usage: verify_prefab_records BUNDLE ROOT CATALOG IDENTITIES NEW_RECORDS_JSON".into(),
        );
    }
    if args[5].exists()
        || args[5]
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("map"))
    {
        return Err("Choose a new JSON record file; playable map export is not implemented".into());
    }
    let manifest = json(&args[1].join("manifest.json"))?;
    for name in [
        "objects.json",
        "layout.json",
        "roads.json",
        "project.mapper",
    ] {
        let raw = std::fs::read(args[1].join(name))?;
        let receipt = &manifest["files"][name];
        if receipt["sha256"] != format!("{:x}", Sha256::digest(&raw))
            || receipt["bytes"] != raw.len() as u64
        {
            return Err(format!("Invalid {name} receipt").into());
        }
    }
    let project = Project::load(&args[1].join("project.mapper"))?;
    let catalog = prefab_assets::load(
        &args[2],
        &args[3],
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let identities = prefab_identities::Source::load(&args[2], &args[4], &catalog, || false)?;
    let objects = json(&args[1].join("objects.json"))?;
    let layout = json(&args[1].join("layout.json"))?;
    let roads = json(&args[1].join("roads.json"))?;
    let prepared = prefab_export::from_snapshot(
        &project,
        &layout,
        &roads,
        &objects,
        |path| identities.resolve(path),
        || false,
    )?
    .ok_or("Record preparation canceled")?;
    let monuments = prepared.initial_monuments + prepared.roadside_monuments;
    let records = prepared.records;
    let result = serde_json::json!({"input_manifest_sha256":format!("{:x}",Sha256::digest(std::fs::read(args[1].join("manifest.json"))?)),"format":prefab_records::VERSION,"schema":1,"playable":false,"scope":"root prefab serialization records; runtime spawn, nested procedural objects, cable assembly and map export not performed","rotation_convention":"Unity Euler Z-X-Y, degrees","prefab_identity_source_sha256":identities.sha256,"game_manifest_source":identities.source,"monuments":monuments,"initial_monuments":prepared.initial_monuments,"roadside_monuments":prepared.roadside_monuments,"objects":prepared.objects,"road_category_policy":"Road {index}; future path serialization must use these same names","records":records});
    let parent = args[5].parent().ok_or("Missing output parent")?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(stage.as_file_mut(), &result)?;
    stage.as_file_mut().flush()?;
    stage.as_file().sync_all()?;
    stage.persist_noclobber(&args[5])?;
    println!(
        "{}",
        serde_json::json!({"output":args[5],"records":records.len(),"monuments":monuments,"identity_source":identities.sha256,"playable":false})
    );
    Ok(())
}
