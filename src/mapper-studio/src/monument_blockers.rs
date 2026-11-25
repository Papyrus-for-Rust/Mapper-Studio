//! Build the initial placement map from verified monument attributes and accepted poses.
use crate::{Project, monument_attributes::Attributes, monument_source::Source};
use papyrus_terrain::{placement, placement_blockers};
use std::collections::{BTreeMap, BTreeSet};

/// Legacy source snapshots remain readable, but cannot claim verified blocker coverage.
pub fn generate(
    project: &Project,
    layout: &placement::Solution,
    source: &Source,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<placement_blockers::Raster>, String> {
    if project.targets.len() > placement::MAX_TARGETS
        || layout.outcomes.len() != project.targets.len()
    {
        return Err("Monument blocker layout does not cover the target roster".into());
    }
    if project
        .monument_rules
        .as_ref()
        .is_none_or(|r| r.source_sha256 != source.sha256)
    {
        return Err("Monument blocker source differs from the project recipe".into());
    }
    let mut targets = BTreeMap::new();
    for target in &project.targets {
        if targets.insert(target.instance_id, target).is_some() {
            return Err("Duplicate monument target identity".into());
        }
    }
    let mut seen = BTreeSet::new();
    let mut data = Vec::new();
    let mut positions = Vec::new();
    for outcome in &layout.outcomes {
        if cancelled() {
            return Ok(None);
        }
        if !seen.insert(outcome.id) {
            return Err("Duplicate monument outcome identity".into());
        }
        let target = targets
            .get(&outcome.id)
            .ok_or("Unknown monument outcome identity")?;
        let Some(position) = &outcome.position else {
            continue;
        };
        let path = target
            .prefab_path
            .as_ref()
            .ok_or("Placed monument has no source prefab path")?;
        let prefab = source
            .prefabs
            .get(path)
            .ok_or("Placed monument is absent from the selected source")?;
        let attributes = prefab
            .placement_attributes
            .as_ref()
            .ok_or("Monument blocker coverage is unavailable; extract its source again")?;
        data.push(modifiers(attributes)?);
        positions.push(position);
    }
    let instances: Vec<_> = positions
        .iter()
        .zip(&data)
        .map(|(p, modifiers)| {
            // Studio yaw turns +X toward +Z; Unity quaternion yaw has the opposite sign.
            let angle = -p.yaw.to_radians() * 0.5;
            placement_blockers::Instance {
                position: [p.x, p.y, p.z],
                rotation: [0., angle.sin(), 0., angle.cos()],
                scale: [1.; 3],
                modifiers,
            }
        })
        .collect();
    placement_blockers::generate(
        project.size,
        papyrus_terrain::native_layers::dimensions(project.size)?.1,
        &instances,
        maximum_work,
        cancelled,
    )
}
fn modifiers(attributes: &Attributes) -> Result<Vec<placement_blockers::Modifier>, String> {
    attributes.validate()?;
    if !attributes.issues.is_empty() {
        return Err("Monument attribute capture has unresolved issues".into());
    }
    let blockers = attributes
        .placement_blockers
        .as_ref()
        .ok_or("Monument blocker coverage is unavailable; extract its source again")?;
    // TerrainModifierEx applies registered attributes without checking active or opacity.
    Ok(blockers
        .iter()
        .map(|b| placement_blockers::Modifier {
            position: b.position,
            radius: b.radius,
            fade: b.fade,
        })
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_layout_uses_studio_yaw_and_requires_complete_bound_identities() {
        let mut project = Project {
            size: 1000,
            ..Project::default()
        };
        project.targets.truncate(1);
        project.targets[0].prefab_path = Some("test.prefab".into());
        let mut source = Source {
            sha256: "a".repeat(64),
            prefabs: BTreeMap::new(),
            models_requested: false,
            stage_source: None,
            dungeon_source: None, offshore_source: None,
            code_sources: Vec::new(),
            extractor_sha256: "a".repeat(64),
            profile_sha256: "a".repeat(64),
            coastal_profile: None,
        };
        source.prefabs.insert("test.prefab".into(), serde_json::from_value(serde_json::json!({
            "rules":{"tier_mask":1,"min_corners":3,"anchors":[],"filters":[],"unsupported":[],"pending":[]},
            "model":{"vertices":[],"triangles":[],"issues":[],"complete":false},
            "placement_attributes":{"version":2,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"issues":[],
                "placement_blockers":[{"object_id":1,"active":true,"position":[40,0,0],"radius":10,"fade":0,"opacity":1}]}
        })).unwrap());
        project.monument_rules = Some(source.recipe());
        let mut layout = placement::Solution {
            outcomes: vec![placement::Outcome {
                id: project.targets[0].instance_id,
                position: Some(placement::Position {
                    x: 100.,
                    y: 20.,
                    z: 50.,
                    yaw: 90.,
                    clearance: 0.,
                }),
                distance: None,
                within_radius: true,
                reason: String::new(),
            }],
            required_missing: 0,
            attempts: 1,
        };
        let raster = generate(&project, &layout, &source, 100000, || false)
            .unwrap()
            .unwrap();
        let n = papyrus_terrain::native_layers::dimensions(project.size)
            .unwrap()
            .1;
        let at = |x: f32, z: f32| {
            raster.cells[((z / 1000. + 0.5) * n as f32) as usize * n
                + ((x / 1000. + 0.5) * n as f32) as usize]
        };
        assert_eq!(at(100., 90.), 1);
        assert_eq!(at(140., 50.), 0);
        assert!(
            generate(&project, &layout, &source, 100000, || true)
                .unwrap()
                .is_none()
        );
        layout.outcomes[0].id += 100;
        assert!(generate(&project, &layout, &source, 100000, || false).is_err());
        layout.outcomes.clear();
        assert!(generate(&project, &layout, &source, 100000, || false).is_err());
        project.targets.clear();
        project.monument_rules = None;
        assert!(generate(&project, &layout, &source, 100000, || false).is_err());
    }
    #[test]
    fn legacy_coverage_fails_and_authored_zero_opacity_is_not_discarded() {
        let mut doc = serde_json::json!({"version":1,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"issues":[]});
        let read = |d: &serde_json::Value| serde_json::from_value::<Attributes>(d.clone()).unwrap();
        assert!(modifiers(&read(&doc)).is_err());
        doc["version"] = 2.into();
        doc["placement_blockers"] = serde_json::json!([]);
        assert!(modifiers(&read(&doc)).unwrap().is_empty());
        doc["placement_blockers"] = serde_json::json!([{"object_id":1,"active":false,"position":[10,0,20],"radius":30,"fade":5,"opacity":0}]);
        let captured = modifiers(&read(&doc)).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].position, [10., 0., 20.]);
        assert_eq!(captured[0].radius, 30.);
        doc["issues"] = serde_json::json!(["Unknown attribute field"]);
        assert!(modifiers(&read(&doc)).is_err());
    }
}
