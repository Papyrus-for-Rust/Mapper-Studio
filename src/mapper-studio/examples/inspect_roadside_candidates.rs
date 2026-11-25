//! Receipt-verified candidate geometry study, never an accepted-world publisher.
use mapper_studio::{Project, installation::SourceFile};
use papyrus_terrain::{
    path_geometry::Path as Curve,
    roadside_monuments::{self, Road, RoadMode, Settings},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path};

fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds bound".into());
    }
    Ok(bytes)
}
fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn receipt(bundle: &Path, manifest: &Value, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    let raw = bounded(&bundle.join(name), limit)?;
    let r = &manifest["files"][name];
    if r["bytes"].as_u64() != Some(raw.len() as u64)
        || r["sha256"].as_str() != Some(digest(&raw).as_str())
    {
        return Err(format!("Bundle receipt mismatch: {name}"));
    }
    Ok(raw)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Prefab {
    version: u32,
    space: String,
    complete: bool,
    connectors: Vec<mapper_studio::infrastructure::Connector>,
    issues: Vec<Value>,
    nodes_visited: usize,
    connector_order: String,
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "inspect_roadside_candidates RUST_ROOT CATALOG ORDERED_CONNECTORS BUNDLE NEW_REPORT"
                .into(),
        );
    }
    let root = Path::new(&args[0]);
    let bundle = Path::new(&args[3]);
    let output = Path::new(&args[4]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let installation = mapper_studio::installation::inspect(root, || false)?;
    let catalog =
        mapper_studio::prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let profile = mapper_studio::coastal_placement::Profile::available(root, &catalog, || false)?
        .ok_or("No matched gameplay-code adapter")?;
    let profile_value = serde_json::to_value(&profile).map_err(|e| e.to_string())?;
    let raw = bounded(Path::new(&args[2]), 4 * 1024 * 1024)?;
    let source: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let mut expected = catalog.sources.clone();
    expected.push(
        serde_json::from_value::<SourceFile>(profile_value["map_scene"].clone())
            .map_err(|e| e.to_string())?,
    );
    expected.extend(
        serde_json::from_value::<Vec<SourceFile>>(profile_value["code_sources"].clone())
            .map_err(|e| e.to_string())?,
    );
    let expected_profile = digest(include_bytes!(
        "../../../compatibility/monuments/client-0f703d43-coastal.json"
    ));
    if source["format"] != "papyrus-roadside-connector-source"
        || source["schema"] != 1
        || source["adapter"] != "papyrus-ordered-roadside-sockets-1"
        || source["connector_order"] != "root-first-depth-first-component-order"
        || source["catalog_sha256"] != catalog.sha256
        || source["placement_execution_validated"] != false
        || source["sources"] != serde_json::to_value(&expected).map_err(|e| e.to_string())?
        || source["code_profile_sha256"] != expected_profile
    {
        return Err("Ordered connector source/profile identity mismatch".into());
    }
    let manifest_raw = bounded(&bundle.join("manifest.json"), 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&manifest_raw).map_err(|e| e.to_string())?;
    if manifest["format"] != "papyrus-preview-bundle" || manifest["schema"] != 1 {
        return Err("Unsupported bundle".into());
    }
    let project: Project = serde_json::from_slice(&receipt(
        bundle,
        &manifest,
        "project.mapper",
        1024 * 1024,
    )?)
    .map_err(|e| e.to_string())?;
    project.validate()?;
    if project.installation.as_ref() != Some(&catalog.installation)
        || project
            .monument_assets
            .as_ref()
            .is_none_or(|a| a.catalog_sha256 != catalog.sha256)
    {
        return Err("Bundle belongs to a different installation/catalog".into());
    }
    let roads_raw = receipt(bundle, &manifest, "roads.json", 128 * 1024 * 1024)?;
    let roads_doc: Value = serde_json::from_slice(&roads_raw).map_err(|e| e.to_string())?;
    let roads = roads_doc["roads"]
        .as_array()
        .ok_or("Missing road controls")?;
    if roads.len() > 4096 {
        return Err("Too many roads".into());
    }
    let mut curves = Vec::new();
    for road in roads {
        let points: Vec<[f32; 3]> =
            serde_json::from_value(road["points"].clone()).map_err(|e| e.to_string())?;
        curves.push(
            Curve::from_controls(&points, 1_000_000, || false)
                .map_err(|e| format!("Invalid road: {e:?}"))?
                .ok_or("Cancelled")?,
        );
    }
    let mut hierarchy = vec![None; curves.len()];
    for family in roads_doc["families"]
        .as_array()
        .ok_or("Missing typed road families")?
    {
        let start = family["first_road"].as_u64().ok_or("Invalid road start")? as usize;
        let count = family["road_count"].as_u64().ok_or("Invalid road count")? as usize;
        let h = family["hierarchy"]
            .as_u64()
            .filter(|&h| h <= 2)
            .ok_or("Invalid hierarchy")? as u8;
        let end = start
            .checked_add(count)
            .filter(|&n| n <= curves.len())
            .ok_or("Road family exceeds controls")?;
        for v in &mut hierarchy[start..end] {
            if v.replace(h).is_some() {
                return Err("Overlapping road families".into());
            }
        }
    }
    let roads = curves
        .iter()
        .zip(hierarchy)
        .map(|(path, h)| {
            Ok(Road {
                path,
                hierarchy: h.ok_or("Untyped road")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let prefabs: BTreeMap<String, Prefab> =
        serde_json::from_value(source["prefabs"].clone()).map_err(|e| e.to_string())?;
    if prefabs.is_empty() || prefabs.len() > 128 {
        return Err("Invalid roadside roster".into());
    }
    let road_source = mapper_studio::infrastructure::Source::cached(
        &catalog,
        project.roads.as_ref().ok_or("Missing source road recipe")?,
        || false,
    )?;
    road_source.verify_installation(root, || false)?;
    let mut cases = Vec::new();
    for (path, prefab) in prefabs {
        if !catalog.entries.contains_key(&path)
            || prefab.version != 2
            || prefab.space != "prefab-root-local-unity"
            || !prefab.complete
            || !prefab.issues.is_empty()
            || prefab.nodes_visited > 65536
            || prefab.connector_order != "root-first-depth-first-component-order"
            || prefab.connectors.len() > 256
        {
            return Err("Invalid ordered prefab metadata".into());
        }
        // The infrastructure enum belongs to the verified selected installation.
        let sockets: Vec<_> = prefab
            .connectors
            .iter()
            .filter(|c| c.infrastructure_type == road_source.profile.road_type)
            .map(|c| c.position)
            .collect();
        let ids: Vec<_> = prefab
            .connectors
            .iter()
            .filter(|c| c.infrastructure_type == road_source.profile.road_type)
            .map(|c| c.object_id)
            .collect();
        for mode in [
            RoadMode::SideRoadOrRingRoad,
            RoadMode::SideRoad,
            RoadMode::RingRoad,
            RoadMode::SideRoadOrDesireTrail,
            RoadMode::DesireTrail,
        ] {
            let result = roadside_monuments::generate(
                &sockets,
                &roads,
                mode,
                Settings {
                    sample_spacing: 5.,
                    end_padding: 5.,
                    maximum_work: 2_000_000,
                    maximum_candidates: 100_000,
                },
                || false,
            )
            .map_err(|e| format!("Roadside proposals for {path}: {e:?}"))?
            .ok_or("Cancelled")?;
            let center = [0, 2]
                .map(|axis| sockets.iter().map(|p| p[axis]).sum::<f32>() / sockets.len() as f32);
            let mut error = 0_f32;
            for c in &result.candidates {
                let curve = roads[c.road].path;
                let (min, max) = curve.range();
                let a = curve.points()[c.start_index.clamp(min, max)];
                let b = curve.points()[c.end_index.clamp(min, max)];
                let (sin, cos) = c.yaw.to_radians().sin_cos();
                error = error.max(
                    (c.position[0] + cos * center[0] - sin * center[1] - (a[0] + b[0]) * 0.5).abs(),
                );
                error = error.max(
                    (c.position[2] + sin * center[0] + cos * center[1] - (a[2] + b[2]) * 0.5).abs(),
                );
            }
            if error > 0.001 {
                return Err("Authored socket midpoint detached from road span".into());
            }
            cases.push(json!({"path":path,"mode":mode,"ordered_road_socket_ids":ids,"candidates":result.candidates.len(),"work":result.work,"degenerate_spans":result.degenerate_spans,"maximum_socket_midpoint_error_metres":error,"examples":result.candidates.iter().take(2).collect::<Vec<_>>() }));
        }
    }
    profile.verify(root, &catalog, || false)?;
    if bounded(Path::new(&args[2]), 4 * 1024 * 1024)? != raw
        || bounded(&bundle.join("manifest.json"), 1024 * 1024)? != manifest_raw
    {
        return Err("Diagnostic inputs changed".into());
    }
    let report = json!({"format":"mapper-native-roadside-candidate-study","schema":1,"source_sha256":digest(&raw),"road_payload_sha256":digest(&roads_raw),"road_count":roads.len(),"cases":cases,"source_road_mode_captured":false,"query_modes":"all five source modes, not a captured scene policy","terrain_eligibility_checked":false,"grouped_selection_complete":false,"path_straightening_complete":false,"accepted_world_published":false,"desktop_windows_opened":0});
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &report).map_err(|e| e.to_string())?;
    println!("{} roadside candidate cases checked", cases.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundle_controls_require_matching_receipts_and_bounded_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("roads.json");
        std::fs::write(&path, b"[]").unwrap();
        let manifest = json!({"files":{"roads.json":{"bytes":2,"sha256":digest(b"[]")}}});
        assert_eq!(
            receipt(directory.path(), &manifest, "roads.json", 32).unwrap(),
            b"[]"
        );
        std::fs::write(&path, b"{}").unwrap();
        assert!(receipt(directory.path(), &manifest, "roads.json", 32).is_err());
        std::fs::write(&path, [0_u8; 33]).unwrap();
        assert!(bounded(&path, 32).is_err());
    }
}
