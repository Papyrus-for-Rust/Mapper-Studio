//! CONFIG ORDERED_ASSETS NEW_REPORT. Read-only selected-installation input study.
use mapper_studio::{Project, infrastructure, prefab_assets, source_road_assets};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds bound".into());
    }
    Ok(bytes)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("CONFIG ORDERED_ASSETS NEW_REPORT".into());
    }
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let config: Value = serde_json::from_slice(&read(Path::new(&args[0]), 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let project = Project::load(path("project")?)?;
    let root = path("rust_root")?;
    let catalog = prefab_assets::load(
        root,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let infrastructure = infrastructure::Source::load(path("infrastructure")?, &catalog, || false)?;
    let assets = source_road_assets::Assets::load(
        Path::new(&args[1]),
        root,
        &catalog,
        &infrastructure,
        || false,
    )?
    .ok_or("Cancelled")?;
    let mut different_order = Vec::new();
    let mut nonmonuments = Vec::new();
    let mut incomplete = Vec::new();
    let mut types = std::collections::BTreeMap::<i32, usize>::new();
    let mut connector_count = 0;
    let mut inactive = 0;
    let mut max_position_error = 0_f32;
    for (name, prefab) in assets.prefabs() {
        if assets.prefab(name).is_err() {
            incomplete.push(name);
            continue;
        }
        let legacy = infrastructure
            .prefabs
            .get(name)
            .ok_or("Legacy prefab coverage differs")?;
        let new_order: Vec<_> = prefab.connectors.iter().map(|c| c.object_id).collect();
        let old_order: Vec<_> = legacy.connectors.iter().map(|c| c.object_id).collect();
        if new_order != old_order {
            different_order.push(name);
        }
        if new_order.len() != old_order.len() {
            return Err("Ordered capture changed connector coverage".into());
        }
        for c in &prefab.connectors {
            connector_count += 1;
            inactive += usize::from(!c.hierarchy_active_without_root);
            let old = legacy
                .connectors
                .iter()
                .find(|o| o.object_id == c.object_id)
                .ok_or("Ordered capture changed connector identity")?;
            if old.infrastructure_type != c.infrastructure_type
                || old.hierarchy_active_without_root != c.hierarchy_active_without_root
            {
                return Err("Ordered capture changed connector type/activity".into());
            }
            for (a, b) in c
                .position
                .iter()
                .zip(old.position)
                .chain(c.forward.iter().zip(old.forward))
            {
                max_position_error = max_position_error.max((a - b).abs());
            }
        }
        if let Some(m) = &prefab.monument {
            *types.entry(m.monument_type).or_default() += 1;
            let bounds = catalog.entries[name]
                .bounds
                .as_ref()
                .ok_or("Root MonumentInfo missing from catalog")?;
            if m.bounds_center
                .iter()
                .zip(bounds.center)
                .chain(m.bounds_extent.iter().zip(bounds.extent))
                .any(|(a, b)| (a - b).abs() > 0.0001)
            {
                return Err("Root monument bounds differ from selected catalog".into());
            }
        } else {
            nonmonuments.push(name);
        }
    }
    if max_position_error > 0.0001 {
        return Err("Ordered connector poses differ from legacy numeric capture".into());
    }
    if format!(
        "{:x}",
        Sha256::digest(read(Path::new(&args[1]), 16 * 1024 * 1024)?)
    ) != assets.sha256
    {
        return Err("Ordered metadata changed during study".into());
    }
    let report = json!({"format":"mapper-owned-ordered-road-assets-study-1","assets_sha256":assets.sha256,
        "catalog_sha256":catalog.sha256,"infrastructure_sha256":infrastructure.sha256,"prefabs":assets.prefabs().len(),
        "connectors":connector_count,"inactive_connectors_included":inactive,"different_order_count":different_order.len(),
        "different_order":different_order,"nonmonument_prefabs":nonmonuments,"incomplete_prefabs":incomplete,
        "opaque_monument_type_counts":types,"maximum_pose_component_error":max_position_error,
        "scope":"Selected installation receipts, ordered connector capture and root fields only. No enum role guesses, generated network, terrain changes, meshes or gameplay validation.","gui_launched":false});
    fs::File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("Verified ordered road inputs; report {}", output.display());
    Ok(())
}
