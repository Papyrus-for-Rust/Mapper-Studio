//! Resolve independently captured object rosters against the installed scene.
//! Other procedural entries remain explicit; this plan does not execute them.
use crate::road_object_source::Source;
use papyrus_terrain::decor::{Kind, Stage};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-object-workflow-1";
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Handler {
    RoadObjects { source_index: usize },
    Decoration { source_index: usize },
    CliffTopology,
    UniformCliffs { source_index: usize },
    CoastalCliffs { source_index: usize },
    UtilityLayout,
    UtilityTopology,
    UtilityObjects { source_index: usize },
    External,
}
#[derive(Debug, Serialize)]
pub struct Step {
    pub scene_order: usize,
    pub object_id: i64,
    pub class: String,
    pub description: String,
    pub mode: i32,
    pub disabled: Option<bool>,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
    pub handler: Handler,
}
#[derive(Debug, Serialize)]
pub struct Plan {
    pub engine: &'static str,
    pub road_source_sha256: String,
    pub decoration_stages_sha256: String,
    pub steps: Vec<Step>,
    pub executed: bool,
}
pub fn plan(road: &Source, decoration: &[Stage]) -> Result<Plan, String> {
    if road.path_family != crate::road_object_source::PathFamily::Road {
        return Err("Mixed road and decoration workflow requires a road object source".into());
    }
    let workflow = road.procedural_order()?;
    if decoration.len() > 128 {
        return Err("Excessive decoration workflow roster".into());
    }
    let mut decor = BTreeMap::new();
    for (index, stage) in decoration.iter().enumerate() {
        stage.validate()?;
        let id = stage
            .id
            .parse::<i64>()
            .map_err(|_| "Decoration stage lacks its scene object ID")?;
        if stage.order != index || decor.insert(id, (index, stage)).is_some() {
            return Err("Duplicate or unordered decoration workflow stage".into());
        }
    }
    let roads: BTreeMap<_, _> = road
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.object_id, i))
        .collect();
    let mut seen = BTreeSet::new();
    let mut steps = Vec::with_capacity(workflow.len());
    for entry in workflow {
        let handler = if let Some(&index) = roads.get(&entry.object_id) {
            Handler::RoadObjects {
                source_index: index,
            }
        } else if entry.kind.starts_with("PlaceDecor")
            || (entry.kind == "PlaceJungleVines" && decor.contains_key(&entry.object_id))
        {
            let &(index, stage) = decor
                .get(&entry.object_id)
                .ok_or("Procedural workflow contains an uncaptured decoration stage")?;
            let class = match stage.kind {
                Kind::Uniform if entry.kind == "PlaceJungleVines" => "PlaceJungleVines",
                Kind::Uniform => "PlaceDecorUniform",
                Kind::WhiteNoise => "PlaceDecorWhiteNoise",
                Kind::ValueNoise => "PlaceDecorValueNoise",
            };
            if entry.kind != class
                || entry.description != stage.name
                || entry.mode != stage.mode
                || stage.enabled
                    != (!entry.disabled.unwrap_or(false)
                        && (entry.kind == "PlaceJungleVines" || entry.component_enabled)
                        && entry.hierarchy_active)
            {
                return Err("Decoration stage differs from its procedural workflow entry".into());
            }
            seen.insert(entry.object_id);
            Handler::Decoration {
                source_index: index,
            }
        } else {
            if decor.contains_key(&entry.object_id) {
                return Err("Decoration stage resolves to a different procedural component".into());
            }
            Handler::External
        };
        steps.push(Step {
            scene_order: entry.order,
            object_id: entry.object_id,
            class: entry.kind.clone(),
            description: entry.description.clone(),
            mode: entry.mode,
            disabled: entry.disabled,
            component_enabled: entry.component_enabled,
            hierarchy_active: entry.hierarchy_active,
            handler,
        });
    }
    if seen.len() != decor.len() {
        return Err("Decoration roster contains stages absent from this procedural scene".into());
    }
    Ok(Plan {
        engine: VERSION,
        road_source_sha256: road.sha256.clone(),
        decoration_stages_sha256: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(decoration).map_err(|e| e.to_string())?)
        ),
        steps,
        executed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road_object_placement::tests::{source, source_value};
    fn fixture() -> (Source, Vec<Stage>) {
        let mut v = source_value();
        v["schema"] = 2.into();
        v["stages"][0]["scene_order"] = 1.into();
        let mut later = v["stages"][0].clone();
        later["object_id"] = 2.into();
        later["order"] = 1.into();
        later["scene_order"] = 3.into();
        later["name"] = "Roadside".into();
        v["stages"].as_array_mut().unwrap().push(later);
        let rows = [
            (10, "GenerateRoadTerrain", "Terrain"),
            (1, "PlaceRoadObjects", "Objects"),
            (20, "PlaceDecorUniform", "Rocks"),
            (2, "PlaceRoadObjects", "Roadside"),
            (21, "PlaceDecorWhiteNoise", "Wrecks"),
            (99, "GenerateRoadMeshes", "Meshes"),
        ];
        v["workflow"] = serde_json::json!({"format":"papyrus-procedural-order","schema":1,"stages":rows.iter().enumerate().map(|(i,(id,kind,name))|serde_json::json!({"object_id":id,"order":i,"kind":kind,"description":name,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true})).collect::<Vec<_>>()});
        let stages=serde_json::from_value(serde_json::json!([
            {"id":"20","order":0,"name":"Rocks","kind":"uniform","folder":"decor/test","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":1,"anchor_mode":0,"distance":10.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[]},
            {"id":"21","order":1,"name":"Wrecks","kind":"white_noise","folder":"decor/test","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":1,"anchor_mode":0,"distance":0.,"dithering":0.,"density":1.,"cluster":null,"prefabs":[]}
        ])).unwrap();
        (source(v), stages)
    }
    #[test]
    fn plan_preserves_interleaving_and_external_stages_without_executing_them() {
        let (source, stages) = fixture();
        let out = plan(&source, &stages).unwrap();
        assert!(!out.executed);
        assert_eq!(
            out.steps.iter().map(|s| s.object_id).collect::<Vec<_>>(),
            [10, 1, 20, 2, 21, 99]
        );
        assert!(matches!(
            out.steps[1].handler,
            Handler::RoadObjects { source_index: 0 }
        ));
        assert!(matches!(
            out.steps[2].handler,
            Handler::Decoration { source_index: 0 }
        ));
        assert!(matches!(
            out.steps[3].handler,
            Handler::RoadObjects { source_index: 1 }
        ));
        assert!(matches!(out.steps[5].handler, Handler::External));
        assert_eq!(
            out.decoration_stages_sha256,
            plan(&source, &stages).unwrap().decoration_stages_sha256
        );
    }
    #[test]
    fn bound_vines_follow_uniform_workflow_and_unbound_legacy_stages_remain_external() {
        let mut value = source_value();
        value["schema"] = 2.into();
        value["stages"][0]["scene_order"] = 0.into();
        value["workflow"] = serde_json::json!({
            "format":"papyrus-procedural-order", "schema":1, "stages":[
                {"object_id":1,"order":0,"kind":"PlaceRoadObjects","description":"Objects",
                 "mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true},
                {"object_id":20,"order":1,"kind":"PlaceJungleVines","description":"Vines",
                 "mode":2,"disabled":false,"component_enabled":false,"hierarchy_active":true}
            ]
        });
        let source = source(value);
        let unbound = plan(&source, &[]).unwrap();
        assert!(matches!(unbound.steps[1].handler, Handler::External));
        let mut stage = fixture().1.remove(0);
        stage.folder = "resource/vine_swinging".into();
        stage.name = "Vines".into();
        stage.mode = 2;
        let out = plan(&source, &[stage.clone()]).unwrap();
        assert!(matches!(out.steps[1].handler, Handler::Decoration { source_index: 0 }));
        stage.kind = Kind::WhiteNoise;
        assert!(plan(&source, &[stage.clone()]).is_err());
        stage.kind = Kind::Uniform;
        stage.enabled = false;
        assert!(plan(&source, &[stage]).is_err());
    }
    #[test]
    fn missing_or_conflicting_source_stages_cannot_be_silently_omitted() {
        let (source, stages) = fixture();
        assert!(plan(&source, &stages[..1]).is_err());
        for mutate in [0, 1, 2, 3, 4] {
            let mut bad = stages.clone();
            match mutate {
                0 => bad[0].id = "99".into(),
                1 => bad[0].name = "wrong".into(),
                2 => bad[0].enabled = false,
                3 => bad[0].kind = Kind::WhiteNoise,
                _ => bad[1].id = "20".into(),
            }
            assert!(plan(&source, &bad).is_err());
        }
        let legacy = source_value();
        assert!(
            plan(
                &crate::road_object_placement::tests::source(legacy),
                &stages
            )
            .is_err()
        );
    }
}
