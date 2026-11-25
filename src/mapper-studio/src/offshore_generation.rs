//! Installed offshore selectors, authored transforms and independent world placement.
use crate::{
    Project, composition, monument_source, offshore_decor, offshore_source, prefab_assets::Catalog,
};
use papyrus_terrain::{
    environment::{self, Box3, Transform},
    monument_composition::Composed,
    offshore_search as search, offshore_tiers as tiers,
    placement::Solution,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const VERSION: &str = "papyrus-offshore-world-1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    native_sha256: String,
    scene_sha256: String,
    stage_profile_id: String,
    lake_type: i32,
}
fn profile(catalog: &Catalog) -> Result<Option<Profile>, String> {
    let Some(stage) = crate::monument_stages::capture_code_profile(catalog)? else {
        return Ok(None);
    };
    let stage: serde_json::Value = serde_json::from_str(stage).map_err(|e| e.to_string())?;
    for raw in [
        include_str!("../../../compatibility/offshore/client-0f703d43.json"),
        include_str!("../../../compatibility/offshore/client-8dd4e235.json"),
        include_str!("../../../compatibility/offshore/client-58f3263f.json"),
    ] {
        let profile: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if profile.native_sha256 != catalog.installation.native.sha256 {
            continue;
        }
        if profile.format != "papyrus-offshore-world-profile"
            || profile.schema != 1
            || profile.id.is_empty()
            || profile.stage_profile_id != stage["id"].as_str().unwrap_or("")
            || profile.lake_type <= 0
            || !catalog.sources.iter().any(|s| {
                s.relative_path == "Bundles/shared/assetscenes.bundle"
                    && s.sha256 == profile.scene_sha256
            })
        {
            return Err("Offshore profile differs from the selected installation".into());
        }
        return Ok(Some(profile));
    }
    Ok(None)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub engine: String,
    pub source_sha256: String,
}
impl Binding {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("Unsupported offshore world binding".into());
        }
        Ok(())
    }
}
pub fn prepare(
    root: &std::path::Path,
    catalog: &Catalog,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Binding>, String> {
    if profile(catalog)?.is_none() {
        return Ok(None);
    }
    if cancelled() {
        return Ok(None);
    }
    let (source, _) = offshore_source::Source::prepare(root, catalog, cancelled)?;
    Ok(Some(Binding {
        engine: VERSION.into(),
        source_sha256: source.sha256,
    }))
}
pub fn cached(
    binding: &Binding,
    root: &std::path::Path,
    catalog: &Catalog,
    cancelled: &dyn Fn() -> bool,
) -> Result<offshore_source::Source, String> {
    binding.validate()?;
    let path = crate::client_cache::pinned_document(
        &crate::storage::local_data()?,
        "papyrus-offshore-stage-cache",
        &binding.source_sha256,
        "stages.json",
    )?;
    if !path.try_exists().map_err(|e| e.to_string())? {
        let restored =
            prepare(root, catalog, cancelled)?.ok_or("Offshore restoration cancelled")?;
        if restored != *binding {
            return Err("Saved offshore source requires its original capture adapter".into());
        }
    }
    let source = offshore_source::Source::load(&path, root, catalog, cancelled)?;
    if source.sha256 != binding.source_sha256 {
        return Err("Pinned offshore source changed".into());
    }
    Ok(source)
}
struct Entry<'a> {
    id: u64,
    path: &'a str,
    bounds: tiers::Bounds,
    tiers: u32,
    minimum_corners: usize,
    decor: offshore_decor::Program,
}
fn frame(pose: &search::Pose) -> Transform {
    Transform {
        position: pose.position,
        rotation: pose.rotation,
        scale: pose.scale,
    }
}
fn bounds(bounds: tiers::Bounds, pose: Transform) -> Result<Box3, String> {
    pose.validate()?;
    if bounds
        .center
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 10000.)
        || bounds
            .extent
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=10000.).contains(v))
    {
        return Err("Invalid offshore monument bounds".into());
    }
    let offset = environment::rotate(
        pose.rotation,
        std::array::from_fn(|i| bounds.center[i] * pose.scale[i]),
    );
    let result = Box3 {
        center: std::array::from_fn(|i| pose.position[i] + offset[i]),
        rotation: pose.rotation,
        extents: std::array::from_fn(|i| bounds.extent[i] * pose.scale[i]),
    };
    if result.center.iter().any(|v| v.abs() > 1_000_000.)
        || result.extents.iter().any(|v| *v > 10000.)
    {
        return Err("Unbounded offshore monument bounds".into());
    }
    Ok(result)
}
fn yaw_frame(position: [f32; 3], yaw: f32) -> Transform {
    let (s, c) = (-yaw.to_radians() * 0.5).sin_cos();
    Transform {
        position,
        rotation: [0., s, 0., c],
        scale: [1.; 3],
    }
}
fn draw(seed: u32, stage: i64, id: u64, point: [f32; 3], operation: usize) -> f32 {
    let mut hash = Sha256::new();
    hash.update(VERSION.as_bytes());
    hash.update(seed.to_le_bytes());
    hash.update(stage.to_le_bytes());
    hash.update(id.to_le_bytes());
    for v in point {
        hash.update(v.to_bits().to_le_bytes());
    }
    hash.update((operation as u64).to_le_bytes());
    let value = u32::from_le_bytes(hash.finalize()[..4].try_into().unwrap());
    (value >> 8) as f32 / 16_777_216.
}
fn occupied(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
) -> Result<Vec<Box3>, String> {
    let mut result = Vec::new();
    for outcome in &solution.outcomes {
        let Some(p) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Unknown offshore world monument")?;
        let Some(b) = target
            .prefab_path
            .as_ref()
            .and_then(|p| catalog.entries.get(p))
            .and_then(|a| a.bounds.as_ref())
        else {
            continue;
        };
        result.push(bounds(
            tiers::Bounds {
                center: b.center,
                extent: b.extent,
            },
            yaw_frame([p.x, p.y, p.z], p.yaw),
        )?);
    }
    Ok(result)
}
/// Bounds use installed monument extents. This independent model does not cook
/// game colliders or reproduce Unity's environment registration lifecycle.
pub fn generate(
    project: &Project,
    solution: &Solution,
    maps: &Composed,
    catalog: &Catalog,
    monuments: &monument_source::Source,
    source: &offshore_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Option<search::Scene>, String> {
    let layers = &maps.layers;
    let execution =
        profile(catalog)?.ok_or("Offshore world adapter unavailable for this installation")?;
    let Some(profile) = &monuments.coastal_profile else {
        return Err("Offshore generation needs installed ocean policy".into());
    };
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if layers.size != project.size
        || layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || layers.topology.len() != sr * sr
    {
        return Err("Invalid offshore terrain inputs".into());
    }
    let rect = search::TerrainRect {
        size: project.size,
        minimum: [-(project.size as f32) * 0.5; 2],
    };
    let mut occupied = occupied(project, solution, catalog)?;
    let mut identities = project
        .targets
        .iter()
        .map(|t| t.instance_id)
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(report) = maps.roads.as_ref().and_then(|r| r.surfaces.as_ref()) {
        let stage = &report.report["staged_pipeline"]["roadside"];
        if let Some(rows) = stage["placements"].as_array() {
            if rows.len() > 4096 {
                return Err("Too many roadside offshore reservations".into());
            }
            for row in rows {
                identities.insert(row["request"].as_u64().ok_or("Missing roadside identity")?);
                let path = row["prefab"].as_str().ok_or("Missing roadside prefab")?;
                if let Some(b) = catalog.entries.get(path).and_then(|a| a.bounds.as_ref()) {
                    let position = serde_json::from_value(row["position"].clone())
                        .map_err(|e| e.to_string())?;
                    let yaw = row["yaw"].as_f64().ok_or("Missing roadside rotation")? as f32;
                    occupied.push(bounds(
                        tiers::Bounds {
                            center: b.center,
                            extent: b.extent,
                        },
                        yaw_frame(position, yaw),
                    )?);
                }
            }
        }
    }
    let mut next_identity = 1_u64 << 40;
    let mut scene = search::Scene {
        engine: VERSION.into(),
        source_sha256: source.sha256.clone(),
        monument_sha256: monuments.sha256.clone(),
        layout_fingerprint: composition::layout_fingerprint(solution),
        seed: project.seeds[4].value,
        instances: Vec::new(),
        stages: Vec::new(),
    };
    for stage in &source.stages {
        if cancelled() {
            return Ok(None);
        }
        let realm = if stage.disabled.is_none() { 1 } else { 2 };
        if !stage.runs(project.size, realm)? {
            continue;
        }
        let mut roster = source.roster(stage.object_id, catalog)?;
        roster.retain(|entry| project.options.prefab_blocker(&entry.path).is_none());
        if roster.is_empty() {
            continue;
        }
        let mut entries = Vec::new();
        for entry in &roster {
            let asset = &catalog.entries[&entry.path];
            if !asset.has_no_terrain_effects() {
                return Err("Offshore prefab needs terrain modifier composition".into());
            }
            let b = asset.bounds.as_ref().ok_or("Missing offshore bounds")?;
            let prefab = monuments
                .prefabs
                .get(&entry.path)
                .ok_or("Missing offshore placement data")?;
            let details = source
                .prefabs
                .as_ref()
                .and_then(|p| p.get(&entry.path))
                .ok_or("Missing installed offshore root and water details")?;
            if details.water_types.contains(&execution.lake_type) {
                return Err(
                    "Offshore lake bodies require their authored trigger-height adapter".into(),
                );
            }
            if !prefab.rules.unsupported.is_empty() {
                return Err("Unsupported offshore placement attributes".into());
            }
            entries.push(Entry {
                id: entry.prefab.id,
                path: &entry.path,
                bounds: tiers::Bounds {
                    center: b.center,
                    extent: b.extent,
                },
                tiers: prefab.rules.tier_mask,
                minimum_corners: prefab.rules.min_corners,
                decor: offshore_decor::Program::from_parts(
                    details.rotation,
                    details.scale,
                    &details.root_decor,
                )?,
            });
        }
        let digest = Sha256::digest(
            format!("{VERSION}/{}/{}", project.seeds[4].value, stage.object_id).as_bytes(),
        );
        let seed = (u32::from_le_bytes(digest[..4].try_into().unwrap()) & i32::MAX as u32).max(1);
        let prefabs = roster.iter().map(|e| e.prefab).collect::<Vec<_>>();
        let policy = stage.policy(10, 10000)?;
        let checks_work = std::cell::Cell::new(0_usize);
        let outcome = search::search(
            rect,
            policy,
            seed,
            &prefabs,
            8_000_000,
            &cancelled,
            |p| {
                papyrus_terrain::spawn::sample_height(
                    layers,
                    (p[0] / project.size as f32 + 0.5).clamp(0., 1.),
                    (p[1] / project.size as f32 + 0.5).clamp(0., 1.),
                )
            },
            |prefab, position| {
                let entry = entries
                    .iter()
                    .find(|e| e.id == prefab.id)
                    .ok_or("Unknown offshore prefab")?;
                let next = checks_work
                    .get()
                    .checked_add(occupied.len() * 32 + entry.decor.required_work() + 32)
                    .filter(|v| *v <= 64_000_000)
                    .ok_or("Offshore validation reached its work allowance")?;
                checks_work.set(next);
                let pose = match entry.decor.apply(
                    position,
                    -500.,
                    entry.decor.required_work(),
                    &cancelled,
                    |point, op| Ok(draw(scene.seed, stage.object_id, entry.id, point, op)),
                    |_| Ok(profile.sea_level),
                )? {
                    Ok(pose) => pose,
                    Err(environment::Verdict::Cancelled) => return Ok(search::Check::Cancelled),
                    Err(other) => return Err(format!("Offshore decoration stopped: {other:?}")),
                };
                let corners = tiers::midplane_corners(entry.bounds, frame(&pose))?;
                let Some(check) = tiers::assess(
                    rect,
                    &layers.topology,
                    sr,
                    corners,
                    entry.tiers,
                    entry.minimum_corners,
                    &cancelled,
                )?
                else {
                    return Ok(search::Check::Cancelled);
                };
                if !check.accepted() {
                    return Ok(search::Check::Rejected);
                }
                let b = bounds(entry.bounds, frame(&pose))?;
                if occupied
                    .iter()
                    .any(|other| environment::overlap(b, *other, 0.))
                {
                    return Ok(search::Check::Rejected);
                }
                Ok(search::Check::Accepted(pose))
            },
        )?;
        let (placements, audit) = match outcome {
            search::Outcome::Found { placements, audit } => (placements, audit),
            search::Outcome::Skipped(audit) => (Vec::new(), audit),
            search::Outcome::Cancelled(_) => return Ok(None),
            search::Outcome::Budget(_) => {
                return Err("Offshore search reached its work allowance".into());
            }
        };
        scene.stages.push(serde_json::json!({"object_id":stage.object_id,"order":stage.order,"policy":policy,"audit":audit,"checks_work":checks_work.get(),"instances":placements.len()}));
        for placement in placements {
            let entry = entries
                .iter()
                .find(|e| e.id == placement.id)
                .ok_or("Unknown winning offshore prefab")?;
            let volume = bounds(entry.bounds, frame(&placement.pose))?;
            if occupied
                .iter()
                .any(|other| environment::overlap(volume, *other, 0.))
            {
                return Err("Offshore winners have overlapping authored bounds".into());
            }
            occupied.push(volume);
            while !identities.insert(next_identity) {
                next_identity += 1;
            }
            let id = next_identity;
            next_identity += 1;
            scene.instances.push(search::Instance {
                id,
                stage: stage.object_id,
                prefab: entry.path.into(),
                name: catalog.entries[entry.path]
                    .display_name
                    .clone()
                    .unwrap_or_else(|| {
                        entry
                            .path
                            .rsplit('/')
                            .next()
                            .unwrap_or(entry.path)
                            .trim_end_matches(".prefab")
                            .replace('_', " ")
                    }),
                pose: placement.pose,
            });
        }
    }
    Ok((!cancelled()).then_some(scene))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offshore_bindings_preserve_legacy_recipes_and_reject_invalid_sources() {
        let old =
            serde_json::json!({"engine":monument_source::VERSION,"source_sha256":"a".repeat(64)});
        let recipe: monument_source::Recipe = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(recipe).unwrap(), old);
        let mut saved = old;
        saved["offshore_source"] =
            serde_json::json!({"engine":VERSION,"source_sha256":"b".repeat(64)});
        let recipe: monument_source::Recipe = serde_json::from_value(saved.clone()).unwrap();
        recipe.validate().unwrap();
        assert_eq!(serde_json::to_value(recipe).unwrap(), saved);
        for (engine, sha) in [
            ("unknown", "b".repeat(64)),
            (VERSION, "../elsewhere".into()),
            (VERSION, "A".repeat(64)),
        ] {
            assert!(
                Binding {
                    engine: engine.into(),
                    source_sha256: sha
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn offshore_authored_bounds_keep_the_rotated_center_and_extents() {
        let frame = yaw_frame([100., 2., 200.], 90.);
        let volume = bounds(
            tiers::Bounds {
                center: [25., 0., 0.],
                extent: [70., 50., 70.],
            },
            frame,
        )
        .unwrap();
        assert!((volume.center[0] - 100.).abs() < 0.001);
        assert!((volume.center[2] - 225.).abs() < 0.001);
        assert_eq!(volume.extents, [70., 50., 70.]);
        assert!(!environment::overlap(
            volume,
            Box3 {
                center: [900., 2., 200.],
                ..volume
            },
            0.
        ));
        assert!(environment::overlap(volume, volume, 0.));
        let point = bounds(
            tiers::Bounds {
                center: [0.; 3],
                extent: [0.; 3],
            },
            frame,
        )
        .unwrap();
        assert_eq!(point.center, frame.position);
        assert_eq!(point.extents, [0.; 3]);
    }
    #[test]
    fn offshore_decorator_draws_are_repeatable_and_separate_stage_seed_and_prefab() {
        let value = draw(7, 10, 1, [2000., -40., 300.], 0);
        assert!((0. ..1.).contains(&value));
        assert_eq!(value, draw(7, 10, 1, [2000., -40., 300.], 0));
        assert_ne!(value, draw(8, 10, 1, [2000., -40., 300.], 0));
        assert_ne!(value, draw(7, 11, 1, [2000., -40., 300.], 0));
        assert_ne!(value, draw(7, 10, 2, [2000., -40., 300.], 0));
    }
}
