//! Road PathList records from receipt-verified, completed staged snapshots.
//! Callers verify file receipts before calling; unsupported policies fail closed.
use crate::{Project, road_paint_source, road_surface_source, road_terrain_source};
use papyrus_terrain::{map_container::Path, road_network::Family};
use serde_json::Value;
use std::collections::BTreeSet;

pub const VERSION: &str = "papyrus-road-path-export-1";
fn terminal(endpoint: &Value, roads: &[Value], circular: bool) -> Result<bool, String> {
    let object = endpoint
        .as_object()
        .ok_or("Missing road endpoint identity")?;
    if object.len() == 1 && endpoint["loop"] == true && circular {
        return Ok(false);
    }
    if !circular && object.len() == 2 {
        if endpoint["monument"].as_u64().is_some_and(|v| v > 0)
            && endpoint["connector"].as_i64().is_some_and(|v| v != 0)
        {
            return Ok(true);
        }
        if let (Some(road), Some(point)) = (
            endpoint["network_road"].as_u64(),
            endpoint["network_point"].as_u64(),
        ) && roads
            .get(usize::try_from(road).map_err(|_| "Invalid network road")?)
            .and_then(|v| v["points"].as_array())
            .is_some_and(|v| point < v.len() as u64)
        {
            return Ok(false);
        }
    }
    Err("Invalid road endpoint identity".into())
}
pub fn from_snapshot(
    project: &Project,
    roads: &Value,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<Path>>, String> {
    if cancelled() {
        return Ok(None);
    }
    let recipe = project.roads.as_ref().ok_or("Missing road recipe")?;
    recipe.validate()?;
    let staged = recipe
        .staged
        .as_ref()
        .ok_or("Path export requires staged road policies")?;
    let surface = &roads["surfaces"];
    let stage = &surface["staged_pipeline"];
    if stage["engine"] != staged.engine
        || stage["roadside_source_sha256"] != staged.roadside_source_sha256
        || stage["post_decoration_validated"] != true
        || surface["source_sha256"].as_str() != recipe.surface_sha256.as_deref()
        || recipe.surface_sha256.is_none()
        || surface["infrastructure_sha256"] != recipe.source_sha256
        || !matches!(
            stage["paint"]["engine"].as_str(),
            Some(crate::road_paint_stage::VERSION | crate::road_paint_stage::RAIL_STAGES_VERSION)
        )
    {
        return Err("Missing or mismatched completed road path source policies".into());
    }
    let paint = road_paint_source::Profile::from_receipt(
        stage["paint"]["paint_profile_sha256"]
            .as_str()
            .ok_or("Missing paint receipt")?,
    )?;
    let terrain = road_terrain_source::Profile::for_paint(&paint)?
        .ok_or("Unsupported saved road terrain policy")?;
    if stage["paint"]["terrain_profile_sha256"] != terrain.sha256()? {
        return Err("Unsupported saved road terrain policy receipt".into());
    }
    let policy: road_surface_source::Policy =
        serde_json::from_value(surface["policy"].clone()).map_err(|e| e.to_string())?;
    policy.validate()?;
    let list = roads["roads"].as_array().ok_or("Missing road roster")?;
    let groups = roads["families"]
        .as_array()
        .ok_or("Missing road families")?;
    if list.len() > 4096 || groups.len() > 16 {
        return Err("Road path snapshot allowance exceeded".into());
    }
    // Combined networks retain per-family policies instead of a single corridor
    // Settings value. The verified paint/terrain receipts above supply each
    // family's serialization modifiers. Check any retained shared settings too.
    let settings = &roads["settings"];
    let mut assigned = vec![None; list.len()];
    let mut seen = BTreeSet::new();
    for group in groups {
        if cancelled() {
            return Ok(None);
        }
        let f: Family = serde_json::from_value(group.clone()).map_err(|e| e.to_string())?;
        if !seen.insert(f.hierarchy) {
            return Err("Duplicate road path family".into());
        }
        let p = paint
            .policy
            .families
            .iter()
            .find(|p| u32::from(p.hierarchy) == f.hierarchy)
            .ok_or("Unknown road path family")?;
        let t = terrain
            .families
            .iter()
            .find(|t| u32::from(t.hierarchy) == f.hierarchy)
            .ok_or("Missing road terrain family")?;
        if f.width != p.width
            || Some(f.splat_channel) != p.splat_channel
            || p.random_scale != policy.random_scale
            || (!settings.is_null()
                && (settings["road_topology"] != p.topology_mask
                    || settings["terrain_offset"].as_f64().map(|v| v as f32)
                        != Some(t.terrain_offset)))
        {
            return Err("Road family differs from its saved source policies".into());
        }
        let end = f
            .first_road
            .checked_add(f.road_count)
            .filter(|end| *end <= list.len())
            .ok_or("Invalid road family range")?;
        if f.first_road > end {
            return Err("Invalid road family range".into());
        }
        for slot in &mut assigned[f.first_road..end] {
            if slot.replace((p, t)).is_some() {
                return Err("Overlapping road path families".into());
            }
        }
    }
    let mut output = Vec::with_capacity(list.len());
    let mut total = 0usize;
    for (i, (road, family)) in list.iter().zip(assigned).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let (p, t) = family.ok_or("Missing road path family")?;
        let values = road["points"].as_array().ok_or("Missing road controls")?;
        total = total
            .checked_add(values.len())
            .filter(|v| *v <= 262144)
            .ok_or("Road path control allowance exceeded")?;
        if !(2..=65536).contains(&values.len()) {
            return Err("Invalid road path control count".into());
        }
        let mut nodes = Vec::with_capacity(values.len());
        for value in values {
            if cancelled() {
                return Ok(None);
            }
            let point: [f32; 3] =
                serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            if point.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
                return Err("Invalid road path control".into());
            }
            nodes.push(point);
        }
        let circular = nodes.first() == nodes.last();
        let endpoint = |side, identity: &Value| -> Result<bool, String> {
            if identity.get("source_network").is_some() {
                if circular || !crate::road_generation::is_raw_source_engine(&staged.engine) {
                    return Err("Raw road export requires its matching source contract".into());
                }
                crate::source_road_handoff::raw_json_endpoint(roads, i, side, identity)?;
                return Ok(false);
            }
            terminal(identity, list, circular)
        };
        output.push(Path {
            name: format!("Road {i}"),
            spline: policy.spline,
            // PathList flags identify monument terminals, not every open end.
            start: endpoint(0, &road["from"])?,
            end: endpoint(1, &road["to"])?,
            width: p.width,
            inner_padding: p.padding[0],
            outer_padding: p.padding[1],
            inner_fade: t.fade[0],
            outer_fade: t.fade[1],
            random_scale: p.random_scale,
            mesh_offset: policy.mesh_offset,
            terrain_offset: t.terrain_offset,
            splat: 1i32 << p.splat_channel.ok_or("Missing road splat channel")?,
            topology: p.topology_mask,
            hierarchy: i32::from(p.hierarchy),
            nodes,
        });
    }
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (Project, Value) {
        let p: Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-paint.json"
        ))
        .unwrap();
        let paint = road_paint_source::Profile::from_receipt(
            "fe2abc8739e73e0dd4ede67176983baa5a674e02cf25772827d5de376720b66a",
        )
        .unwrap();
        let terrain = road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        let mut recipe = crate::infrastructure::Recipe::new("a".repeat(64));
        recipe.engine = papyrus_terrain::road_network::COST_VERSION.into();
        recipe.surface_sha256 = Some("c".repeat(64));
        recipe.staged = Some(crate::road_generation::Policy {
            layout: None,
            engine: crate::road_generation::VERSION.into(),
            roadside_source_sha256: "b".repeat(64),
            roadside_preferences: Vec::new(),
        });
        let project = Project {
            roads: Some(recipe),
            ..Default::default()
        };
        let surface_profile: Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-surfaces.json"
        ))
        .unwrap();
        let families = p["policy"]["families"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "infrastructure_type": if i == 2 {2} else {0}, "hierarchy":i, "seed":1,
                    "first_road":i,"road_count":1,"socket_count":0,"unresolved":[],
                    "width":f["width"],"splat_channel":f["splat_channel"]
                })
            })
            .collect::<Vec<_>>();
        (
            project,
            json!({"families":families,"settings":{"road_topology":2048,"terrain_offset":-0.125},
            "roads":[
                {"from":{"loop":true},"to":{"loop":true},"points":[[0.,0.,0.],[10.,0.,10.],[0.,0.,0.]]},
                {"from":{"monument":1,"connector":10},"to":{"network_road":0,"network_point":1},"points":[[20.,1.,20.],[10.,0.,10.]]},
                {"from":{"monument":2,"connector":11},"to":{"monument":3,"connector":12},"points":[[30.,1.,30.],[40.,1.,40.]]}
            ],"surfaces":{"source_sha256":"c".repeat(64),"infrastructure_sha256":"a".repeat(64),
                "policy":surface_profile["policy"],"staged_pipeline":{
                    "engine":crate::road_generation::VERSION,"roadside_source_sha256":"b".repeat(64),
                    "post_decoration_validated":true,"paint":{"engine":crate::road_paint_stage::VERSION,
                    "paint_profile_sha256":paint.sha256().unwrap(),"terrain_profile_sha256":terrain.sha256().unwrap()}
                }}}),
        )
    }
    #[test]
    fn verified_families_export_without_shared_legacy_corridor_settings() {
        let (project, mut roads) = fixture();
        let expected = from_snapshot(&project, &roads, || false).unwrap().unwrap();
        roads["settings"] = Value::Null;
        let before = roads.clone();
        let paths = from_snapshot(&project, &roads, || false).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&paths).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[2].splat, 1);
        assert_eq!(paths[2].width, 4.);
        assert_eq!(roads, before);
        roads["surfaces"]["staged_pipeline"]["paint"]["engine"] =
            crate::road_paint_stage::RAIL_STAGES_VERSION.into();
        let combined = from_snapshot(&project, &roads, || false).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&paths).unwrap(),
            serde_json::to_value(combined).unwrap()
        );
        roads["families"][1]["width"] = 11.into();
        assert!(from_snapshot(&project, &roads, || false).is_err());
    }
    #[test]
    fn source_families_keep_paved_and_dirt_modifiers_and_monument_terminal_flags() {
        let (project, roads) = fixture();
        let paths = from_snapshot(&project, &roads, || false).unwrap().unwrap();
        assert_eq!(
            paths
                .iter()
                .map(|p| (p.width, p.inner_padding, p.splat, p.hierarchy))
                .collect::<Vec<_>>(),
            vec![(12., 1., 128, 0), (10., 1., 128, 1), (4., 0.4, 1, 2)]
        );
        assert_eq!(
            paths.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(),
            vec![(false, false), (true, false), (true, true)]
        );
        for (i, p) in paths.iter().enumerate() {
            assert_eq!(
                (
                    p.outer_padding,
                    p.inner_fade,
                    p.outer_fade,
                    p.random_scale,
                    p.mesh_offset,
                    p.terrain_offset
                ),
                (1., 1., 8., 0.75, 0., -0.125)
            );
            assert_eq!(
                serde_json::to_value(&p.nodes).unwrap(),
                roads["roads"][i]["points"]
            );
        }
    }
    #[test]
    fn missing_policies_incomplete_families_and_invalid_attachments_never_publish_partial_paths() {
        let (project, roads) = fixture();
        for (pointer, value) in [
            (
                "/surfaces/staged_pipeline/paint/paint_profile_sha256",
                json!("d".repeat(64)),
            ),
            (
                "/surfaces/staged_pipeline/paint/terrain_profile_sha256",
                json!("d".repeat(64)),
            ),
            (
                "/surfaces/staged_pipeline/post_decoration_validated",
                json!(false),
            ),
            ("/surfaces/source_sha256", json!("d".repeat(64))),
            ("/families/2/width", json!(10.)),
            ("/families/2/first_road", json!(1)),
            ("/families/2/road_count", json!(0)),
            ("/roads/1/to/network_point", json!(10000)),
            ("/roads/0/from", json!({"monument":1,"connector":10})),
            ("/roads/2/points/1/0", json!(100001.)),
        ] {
            let mut changed = roads.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                from_snapshot(&project, &changed, || false).is_err(),
                "{pointer}"
            );
        }
        let mut old = project.clone();
        old.roads.as_mut().unwrap().staged = None;
        assert!(from_snapshot(&old, &roads, || false).is_err());
        let before = roads.clone();
        let calls = std::cell::Cell::new(0);
        assert!(
            from_snapshot(&project, &roads, || {
                calls.set(calls.get() + 1);
                calls.get() > 6
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(roads, before);
    }
}
