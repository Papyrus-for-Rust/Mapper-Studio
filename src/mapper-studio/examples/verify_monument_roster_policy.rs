//! Verify the roster action against an owned installation without opening the app.
use mapper_studio::{Project, placement_preview, prefab_assets};
use std::path::Path;

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("verify_monument_roster_policy PROJECT RUST_ROOT CATALOG NEW_REPORT".into());
    }
    let original = Project::load(Path::new(&args[0]))?;
    if !mapper_studio::monument_source::supported(&original.placement_engine) {
        return Err("Choose a project with installed monument rules".into());
    }
    let catalog = prefab_assets::load(
        Path::new(&args[1]),
        Path::new(&args[2]),
        original
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let mut updated = original.clone();
    let added = placement_preview::populate_targets(&mut updated, &catalog)?;
    if updated.placement_engine != original.placement_engine
        || updated.monument_rules != original.monument_rules
        || updated.monument_assets != original.monument_assets
        || updated.composition_engine != original.composition_engine
        || updated.terrain_key() != original.terrain_key()
        || updated.seeds != original.seeds
        || updated.targets[..original.targets.len()] != original.targets
    {
        return Err("Roster action changed installed rules, seeds or existing preferences".into());
    }
    let before_noop = updated.clone();
    if placement_preview::populate_targets(&mut updated, &catalog)? != 0 || updated != before_noop {
        return Err("Repeated roster action was not a no-op".into());
    }
    updated
        .monument_assets
        .as_mut()
        .ok_or("Missing asset pins")?
        .catalog_sha256 = "e".repeat(64);
    let mismatched = updated.clone();
    if placement_preview::populate_targets(&mut updated, &catalog).is_ok() || updated != mismatched
    {
        return Err("Mismatched catalog was admitted or partially changed the project".into());
    }
    let report = serde_json::json!({"format":"mapper-monument-roster-policy-verification","schema":1,
        "added":added,"original_targets":original.targets.len(),"updated_targets":before_noop.targets.len(),
        "placement_engine":original.placement_engine,"monument_rules":original.monument_rules,
        "source_policy_preserved":true,"existing_preferences_preserved":true,"seeds_preserved":true,
        "terrain_identity_preserved":true,"repeated_action_is_noop":true,"mismatch_rejected_atomically":true,
        "desktop_windows_opened":0,"world_generated":false});
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(Path::new(&args[3]))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &report).map_err(|e| e.to_string())?;
    println!("Verified {added} added monuments; installed policy preserved");
    Ok(())
}
