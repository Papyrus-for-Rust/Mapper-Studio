//! Prefab records from a receipt-verified preview snapshot. Callers must verify
//! the project/layout/road/object receipts before using this adapter.
use crate::Project;
use papyrus_terrain::prefab_records::{self, Instance, Record};
use serde_json::Value;
use std::collections::BTreeSet;

pub struct Output {
    pub records: Vec<Record>,
    pub initial_monuments: usize,
    pub roadside_monuments: usize,
    pub objects: usize,
}
struct Owned {
    path: String,
    category: String,
    position: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}
fn scalar(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or("Invalid monument transform".into())
}
/// Shared map-to-Unity boundary for a placed monument root.
pub fn monument_pose(
    position: [f32; 3],
    yaw: f32,
) -> Result<papyrus_terrain::environment::Transform, String> {
    if !yaw.is_finite() {
        return Err("Invalid monument yaw".into());
    }
    // Studio +X turns toward +Z; the Unity boundary negates that yaw.
    let angle = -yaw.to_radians() * 0.5;
    Ok(papyrus_terrain::environment::Transform {
        position,
        rotation: [0., angle.sin(), 0., angle.cos()],
        scale: [1.; 3],
    })
}
fn monument(path: String, position: [f32; 3], yaw: f32) -> Result<Owned, String> {
    let pose = monument_pose(position, yaw)?;
    Ok(Owned {
        path,
        category: "Monument".into(),
        position: pose.position,
        rotation: pose.rotation,
        scale: pose.scale,
    })
}
pub fn from_snapshot(
    project: &Project,
    layout: &Value,
    roads: &Value,
    objects: &Value,
    resolve: impl Fn(&str) -> Option<u32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    let outcomes = layout["outcomes"]
        .as_array()
        .ok_or("Missing monument outcomes")?;
    let paths = roads["roads"].as_array().ok_or("Missing road roster")?;
    let instances = objects["instances"]
        .as_array()
        .ok_or("Missing object roster")?;
    if outcomes.len() != project.targets.len()
        || outcomes.len() > papyrus_terrain::placement::MAX_TARGETS
        || paths.len() > 4096
        || instances.len() > papyrus_terrain::content::MAX_PREFABS
    {
        return Err("Prefab snapshot allowance exceeded".into());
    }
    let mut owned = Vec::new();
    let mut identities = BTreeSet::new();
    for outcome in outcomes {
        if cancelled() {
            return Ok(None);
        }
        let id = outcome["id"].as_u64().ok_or("Invalid monument identity")?;
        if !identities.insert(id) {
            return Err("Duplicate monument outcome".into());
        }
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == id)
            .ok_or("Unknown monument outcome")?;
        if outcome
            .get("position")
            .ok_or("Missing monument position")?
            .is_null()
        {
            continue;
        }
        let p = &outcome["position"];
        owned.push(monument(
            target
                .prefab_path
                .clone()
                .ok_or("Missing monument prefab")?,
            [scalar(&p["x"])?, scalar(&p["y"])?, scalar(&p["z"])?],
            scalar(&p["yaw"])?,
        )?);
    }
    let initial_monuments = owned.len();
    let stage = &roads["surfaces"]["staged_pipeline"];
    let roadside = &stage["roadside"];
    if let Some(policy) = project.roads.as_ref().and_then(|r| r.staged.as_ref()) {
        policy.validate()?;
        if stage["engine"] != policy.engine
            || stage["post_decoration_validated"] != true
            || roadside["engine"] != crate::roadside_stamps::VERSION
            || roadside["roadside_source_sha256"] != policy.roadside_source_sha256
        {
            return Err("Missing or mismatched completed roadside stage".into());
        }
        let placements = roadside["placements"]
            .as_array()
            .ok_or("Missing roadside monument placements")?;
        if placements.len() > 128 {
            return Err("Roadside monument allowance exceeded".into());
        }
        for placement in placements {
            if cancelled() {
                return Ok(None);
            }
            let id = placement["request"]
                .as_u64()
                .ok_or("Invalid roadside request identity")?;
            if !identities.insert(id) {
                return Err("Duplicate initial/roadside monument identity".into());
            }
            let index = placement["road"]
                .as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .ok_or("Invalid roadside road")?;
            let points = paths
                .get(index)
                .and_then(|r| r["points"].as_array())
                .ok_or("Dangling roadside road")?;
            let start = placement["start_index"]
                .as_u64()
                .ok_or("Invalid roadside span")?;
            let end = placement["end_index"]
                .as_u64()
                .ok_or("Invalid roadside span")?;
            if start >= end || end >= points.len() as u64 {
                return Err("Dangling roadside span".into());
            }
            owned.push(monument(
                placement["prefab"]
                    .as_str()
                    .ok_or("Missing roadside prefab")?
                    .into(),
                serde_json::from_value(placement["position"].clone()).map_err(|e| e.to_string())?,
                scalar(&placement["yaw"])?,
            )?);
        }
    } else if !roadside.is_null() {
        return Err("Unexpected unbound roadside placements".into());
    }
    let roadside_monuments = owned.len() - initial_monuments;
    if owned
        .len()
        .checked_add(instances.len())
        .is_none_or(|n| n > papyrus_terrain::content::MAX_PREFABS)
    {
        return Err("Combined prefab allowance exceeded".into());
    }
    for instance in instances {
        if cancelled() {
            return Ok(None);
        }
        let category = match instance["kind"].as_str() {
            Some("decoration") => "Decor".into(),
            Some("road") => {
                let index = instance["road"]
                    .as_u64()
                    .ok_or("Missing object road ownership")?;
                if index >= paths.len() as u64 {
                    return Err("Dangling object road ownership".into());
                }
                format!("Road {index}")
            }
            Some("utility") => {
                let index=instance["road"].as_u64().ok_or("Missing object utility path ownership")?;
                if objects["utility"]["paths"].as_array().is_none_or(|paths|index>=paths.len() as u64) {
                    return Err("Dangling utility path ownership".into());
                }
                format!("Powerline {index}")
            }
            _ => return Err("Unknown object kind".into()),
        };
        let p = &instance["pose"];
        owned.push(Owned {
            path: instance["prefab"]
                .as_str()
                .ok_or("Missing object prefab")?
                .into(),
            category,
            position: serde_json::from_value(p["position"].clone()).map_err(|e| e.to_string())?,
            rotation: serde_json::from_value(p["rotation"].clone()).map_err(|e| e.to_string())?,
            scale: serde_json::from_value(p["scale"].clone()).map_err(|e| e.to_string())?,
        });
    }
    let rows = owned
        .iter()
        .map(|p| Instance {
            path: &p.path,
            category: &p.category,
            position: p.position,
            rotation: p.rotation,
            scale: p.scale,
        })
        .collect::<Vec<_>>();
    if !stage["pipes"].is_null() && project.objects.is_none() {
        return Err("Unexpected unbound pipe placements".into());
    }
    crate::water_treatment_stage::validate_committed(&stage["pipes"], project)?;
    let Some(pipes) = crate::water_treatment_stage::receipt_records(
        &stage["pipes"], &resolve, &cancelled,
    )? else {
        return Ok(None);
    };
    if rows.len().checked_add(pipes.len()).is_none_or(|count| count > papyrus_terrain::content::MAX_PREFABS) {
        return Err("Combined map and pipe prefab allowance exceeded".into());
    }
    let pipe_count = pipes.len();
    Ok(
        prefab_records::assemble(&rows, resolve, cancelled)?.map(|mut records| {
            records.extend(pipes);
            Output {
                records,
                initial_monuments,
                roadside_monuments,
                objects: instances.len() + pipe_count,
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (Project, Value, Value, Value) {
        let mut project = Project::default();
        project.targets.truncate(1);
        project.targets[0].prefab_path = Some("assets/test/root.prefab".into());
        let mut recipe = crate::infrastructure::Recipe::new("a".repeat(64));
        recipe.staged = Some(crate::road_generation::Policy {
            layout: None,
            engine: crate::road_generation::VERSION.into(),
            roadside_source_sha256: "b".repeat(64),
            roadside_preferences: Vec::new(),
        });
        project.roads = Some(recipe);
        let layout = json!({"outcomes":[{"id":1,"position":{"x":1.,"y":2.,"z":3.,"yaw":37.5}}]});
        let roads = json!({"roads":[{"points":[[0.,0.,0.],[20.,0.,0.],[40.,0.,0.]]}],"surfaces":{"staged_pipeline":{
            "engine":crate::road_generation::VERSION,"post_decoration_validated":true,"roadside":{
                "engine":crate::roadside_stamps::VERSION,"roadside_source_sha256":"b".repeat(64),
                "placements":[{"request":100,"prefab":"assets/test/roadside.prefab","position":[12.,4.,16.],
                    "yaw":123.25,"road":0,"start_index":0,"end_index":1}]
            }
        }}});
        let objects = json!({"instances":[{"kind":"road","road":0,"prefab":"assets/test/sign.prefab",
            "pose":{"position":[0.,1.,2.],"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.]}}]});
        (project, layout, roads, objects)
    }
    fn resolve(path: &str) -> Option<u32> {
        match path {
            "assets/test/root.prefab" => Some(1),
            "assets/test/roadside.prefab" => Some(2),
            "assets/test/sign.prefab" => Some(3),
            _ => None,
        }
    }
    #[test]
    fn records_include_roadside_roots_with_their_own_position_and_unity_rotation() {
        let (project, layout, roads, objects) = fixture();
        let out = from_snapshot(&project, &layout, &roads, &objects, resolve, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            (out.initial_monuments, out.roadside_monuments, out.objects),
            (1, 1, 1)
        );
        assert_eq!(
            out.records.iter().map(|r| r.id).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(out.records[1].category, "Monument");
        assert_eq!(out.records[1].position, [12., 4., 16.]);
        assert!((out.records[1].rotation[1] - 236.75).abs() < 0.0001);
        assert_eq!(out.records[2].category, "Road 0");
        assert_eq!(out.records[1].scale, [1.; 3]);
        let mut legacy = project;
        legacy.roads.as_mut().unwrap().staged = None;
        let mut roads = roads;
        roads.as_object_mut().unwrap().remove("surfaces");
        let old = from_snapshot(&legacy, &layout, &roads, &objects, resolve, || false)
            .unwrap()
            .unwrap();
        assert_eq!(old.records.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(old.roadside_monuments, 0);
    }
    #[test]
    fn normal_snapshot_exports_committed_pipe_roots_and_checks_their_recipe_binding() {
        let (mut project, layout, mut roads, objects) = fixture();
        project.objects = Some(crate::object_generation::Recipe::new("c".repeat(64)));
        project.monument_assets = Some(crate::AssetIdentity {
            catalog_sha256: "d".repeat(64), bindings_sha256: "e".repeat(64),
        });
        let settings = crate::water_treatment_stage::Settings {
            seed: project.seeds[5].value, realm_mask: 2, maximum_terrain_work: 32_000_000,
            maximum_sampling_attempts: 1000, maximum_fitting_samples: 1000, maximum_clearance_work: 1000,
        };
        let resolution = papyrus_terrain::native_layers::dimensions(project.size).unwrap().1;
        let height_resolution = papyrus_terrain::native_layers::dimensions(project.size).unwrap().0;
        roads["surfaces"]["staged_pipeline"]["pipes"] = json!({
            "engine":"mapper-committed-water-treatment-1","complete":true,
            "tag_source_sha256":"f".repeat(64),"identities_sha256":"f".repeat(64),
            "road_source_sha256":"c".repeat(64),"catalog_sha256":"d".repeat(64),
            "surface_resolution":resolution,"reserved_spans":[],"height_resolution":height_resolution,"height_spans":[],
            "report":{"engine":crate::water_treatment_stage::VERSION,"source_sha256":"f".repeat(64),
                "eligible_monuments":1,"admitted_instances":1,"settings":settings},
            "prefabs":[{"path":"assets/test/sign.prefab","identity":3,
                "position":[55.,19.,-80.],"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.]}]
        });
        let output = from_snapshot(&project, &layout, &roads, &objects, resolve, || false).unwrap().unwrap();
        assert_eq!(output.objects, 2);
        assert_eq!(output.records.len(), 4);
        assert_eq!(output.records[3].category, "Monument");
        assert_eq!(output.records[3].position, [55.,19.,-80.]);
        project.seeds[5].value = project.seeds[5].value.wrapping_add(1);
        assert!(from_snapshot(&project, &layout, &roads, &objects, resolve, || false).is_err());
    }
    #[test]
    fn utility_records_keep_path_ownership_and_reject_dangling_paths() {
        let (project, layout, roads, mut objects) = fixture();
        objects["utility"] = json!({"paths":[{"name":"Powerline 0"}]});
        objects["instances"][0]["kind"] = "utility".into();
        let output = from_snapshot(&project, &layout, &roads, &objects, resolve, || false)
            .unwrap().unwrap();
        assert_eq!(output.records[2].category, "Powerline 0");
        assert_eq!(output.records[2].position, [0., 1., 2.]);
        objects["instances"][0]["road"] = 1.into();
        assert!(from_snapshot(&project, &layout, &roads, &objects, resolve, || false).is_err());
        objects["instances"][0]["road"] = 0.into();
        objects["utility"]["paths"] = json!([]);
        assert!(from_snapshot(&project, &layout, &roads, &objects, resolve, || false).is_err());
    }
    #[test]
    fn missing_mismatched_or_dangling_staged_records_fail_instead_of_omitting_monuments() {
        let (project, layout, roads, objects) = fixture();
        for (pointer, value) in [
            ("/surfaces/staged_pipeline/roadside/placements", Value::Null),
            (
                "/surfaces/staged_pipeline/roadside/roadside_source_sha256",
                json!("c".repeat(64)),
            ),
            (
                "/surfaces/staged_pipeline/post_decoration_validated",
                json!(false),
            ),
            (
                "/surfaces/staged_pipeline/roadside/placements/0/request",
                json!(1),
            ),
            (
                "/surfaces/staged_pipeline/roadside/placements/0/road",
                json!(99),
            ),
            (
                "/surfaces/staged_pipeline/roadside/placements/0/end_index",
                json!(99),
            ),
            (
                "/surfaces/staged_pipeline/roadside/placements/0/yaw",
                Value::Null,
            ),
        ] {
            let mut invalid = roads.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(
                from_snapshot(&project, &layout, &invalid, &objects, resolve, || false).is_err(),
                "{pointer}"
            );
        }
        let mut absent = layout.clone();
        absent["outcomes"] = json!([]);
        assert!(from_snapshot(&project, &absent, &roads, &objects, resolve, || false).is_err());
        assert!(from_snapshot(&project, &layout, &roads, &objects, |_| None, || false).is_err());
    }
    #[test]
    fn cancellation_and_combined_record_limits_leave_snapshot_inputs_unchanged() {
        let (project, layout, roads, objects) = fixture();
        let before = (
            project.clone(),
            layout.clone(),
            roads.clone(),
            objects.clone(),
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            from_snapshot(&project, &layout, &roads, &objects, resolve, || {
                calls.set(calls.get() + 1);
                calls.get() == 4
            })
            .unwrap()
            .is_none()
        );
        let huge = json!({"instances":vec![Value::Null;papyrus_terrain::content::MAX_PREFABS]});
        assert!(from_snapshot(&project, &layout, &roads, &huge, resolve, || false).is_err());
        assert_eq!((project, layout, roads, objects), before);
    }
}
