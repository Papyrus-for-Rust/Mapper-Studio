//! Verified numeric placement attributes. Capture is not execution or coastal validation.
use serde::{Deserialize, Serialize};

/// Authored facts only. Runtime trigger construction and world-volume overlap
/// checks must be supplied before this data can certify a placement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootTransform {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentObject {
    pub object_id: i64,
    pub layer: u8,
    /// Exact GetComponent order for EnvironmentVolume on this GameObject.
    pub volume_components: Vec<i64>,
    /// Serialized flags in the record's collider component order.
    pub collider_enabled: Vec<bool>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRecord {
    pub object_id: i64,
    pub hierarchy_active: bool,
    pub component_enabled: bool,
    pub type_mask: i32,
    pub center: [f32; 3],
    pub size: [f32; 3],
    /// Root-local hierarchy matrix; excludes the prefab root transform.
    pub local_to_root: [[f32; 4]; 4],
    /// Component order is retained. No collider means the inspected runtime
    /// creates a box; a non-box existing collider needs a different adapter.
    pub collider_types: Vec<i32>,
    /// IDs are local to this prefab's catalog assetScene, not global IDs.
    pub collider_ids: Vec<i64>,
    /// Raw child-to-parent rotations in hierarchy order, excluding the root.
    /// EnvironmentVolumeCheck uses rotation separately from scale.
    pub rotation_chain: Vec<[f32; 4]>,
    pub field_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_object: Option<EnvironmentObject>,
}
/// Raw collider-local mesh buffers. Convex/cooking flags are authored facts,
/// not a claim that PhysX cooking has been reproduced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentColliderMesh {
    pub collider_id: i64,
    pub mesh_asset: String,
    pub mesh_id: i64,
    pub enabled: bool,
    pub is_trigger: bool,
    pub convex: bool,
    pub cooking_options: u32,
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub root_transform: Option<RootTransform>,
    pub volumes: Vec<EnvironmentRecord>,
    pub checks: Vec<EnvironmentRecord>,
    pub issues: Vec<String>,
    /// None is legacy/unknown coverage; Some(empty) is a captured empty set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collider_meshes: Option<Vec<EnvironmentColliderMesh>>,
    /// None is an older capture, not a proof that no water bodies exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water_placement: Option<WaterPlacement>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterPlacement {
    pub version: u32,
    /// Exact script identity, depth-first hierarchy/component capture order.
    pub bodies: Vec<WaterBody>,
    pub issues: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterBody {
    pub object_id: i64,
    pub body_type: i32,
    pub component_enabled: bool,
    pub hierarchy_active_without_root: bool,
    pub position: [f32; 3],
    /// Raw references, including nulls, duplicates and external files. They are
    /// not runtime collider bounds and are never silently dropped.
    pub triggers: Vec<WaterTrigger>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterTrigger {
    pub file_id: i32,
    pub path_id: i64,
}
impl WaterPlacement {
    pub(crate) fn validate(&self, ids: &mut std::collections::BTreeSet<i64>) -> Result<(), String> {
        if self.version != 1
            || self.bodies.len() > 1024
            || self.issues.len() > 256
            || self.issues.iter().any(|s| s.is_empty() || s.len() > 1024)
            || self.bodies.iter().map(|b| b.triggers.len()).sum::<usize>() > 4096
        {
            return Err("Invalid water placement capture coverage".into());
        }
        for body in &self.bodies {
            if body.object_id == 0
                || body.object_id == i64::MIN
                || !ids.insert(body.object_id)
                || body
                    .position
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || body.triggers.len() > 1024
                || body.triggers.iter().any(|r| {
                    r.file_id < 0 || r.path_id == i64::MIN || (r.path_id == 0 && r.file_id != 0)
                })
            {
                return Err("Invalid water placement body or trigger reference".into());
            }
        }
        Ok(())
    }
}
impl Environment {
    fn validate(&self, ids: &mut std::collections::BTreeSet<i64>) -> Result<(), String> {
        if let Some(water) = &self.water_placement {
            water.validate(ids)?;
        }
        if self.volumes.len() > 4096
            || self.checks.len() > 4096
            || self.issues.len() > 256
            || self.issues.iter().any(|s| s.is_empty() || s.len() > 1024)
            || (self.root_transform.is_none() && self.issues.is_empty())
        {
            return Err("Invalid environment capture coverage".into());
        }
        if let Some(root) = &self.root_transform
            && (root.rotation.iter().any(|v| !v.is_finite())
                || root
                    .position
                    .as_ref()
                    .is_some_and(|p| p.iter().any(|v| !v.is_finite() || v.abs() > 10000.))
                || (root.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
                || root
                    .scale
                    .iter()
                    .any(|v| !v.is_finite() || !(0.000001..=10000.).contains(&v.abs())))
        {
            return Err("Invalid environment prefab root transform".into());
        }
        let volumes = self
            .volumes
            .iter()
            .map(|r| (r.object_id, r))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut objects = std::collections::BTreeMap::<i64, &EnvironmentRecord>::new();
        let mut collider_owners = std::collections::BTreeMap::new();
        for record in self.volumes.iter().chain(&self.checks) {
            if record.object_id == 0
                || !ids.insert(record.object_id)
                || record
                    .center
                    .iter()
                    .chain(record.size.iter())
                    .chain(record.local_to_root.iter().flatten())
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || record.size.iter().any(|v| *v <= 0.)
                || record.local_to_root[3] != [0., 0., 0., 1.]
                || record.collider_types.len() > 16
                || record.collider_ids.len() != record.collider_types.len()
                || record.collider_ids.contains(&0)
                || record
                    .collider_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != record.collider_ids.len()
                || record.rotation_chain.len() > 128
                || record.rotation_chain.iter().any(|q| {
                    q.iter().any(|v| !v.is_finite())
                        || (q.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
                })
                || record
                    .collider_types
                    .iter()
                    .any(|v| !matches!(v, 64 | 65 | 135 | 136 | 154))
                || record.field_names.len() > 256
                || record
                    .field_names
                    .iter()
                    .any(|n| n.is_empty() || n.len() > 256 || n.contains('\0'))
                || record.field_names.windows(2).any(|p| p[0] >= p[1])
                || ["Center", "Size", "Type"]
                    .iter()
                    .any(|name| !record.field_names.iter().any(|n| n == name))
            {
                return Err("Invalid authored environment component".into());
            }
            if let Some(object) = &record.game_object {
                for id in &record.collider_ids {
                    if collider_owners
                        .insert(*id, object.object_id)
                        .is_some_and(|previous| previous != object.object_id)
                    {
                        return Err(
                            "Environment collider belongs to conflicting GameObjects".into()
                        );
                    }
                }
                if object.object_id == 0
                    || object.layer > 31
                    || object.volume_components.len() > 64
                    || object.volume_components.contains(&0)
                    || object
                        .volume_components
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != object.volume_components.len()
                    || object.collider_enabled.len() != record.collider_ids.len()
                    || (volumes.contains_key(&record.object_id)
                        && !object.volume_components.contains(&record.object_id))
                    || object
                        .volume_components
                        .iter()
                        .any(|id| match volumes.get(id) {
                            None => self.issues.is_empty(),
                            Some(other) => other
                                .game_object
                                .as_ref()
                                .is_some_and(|o| o.object_id != object.object_id),
                        })
                {
                    return Err("Invalid environment GameObject associations".into());
                }
                if let Some(previous) = objects.insert(object.object_id, record)
                    && (previous.game_object != record.game_object
                        || previous.collider_ids != record.collider_ids
                        || previous.collider_types != record.collider_types
                        || previous.local_to_root != record.local_to_root
                        || previous.rotation_chain != record.rotation_chain
                        || previous.hierarchy_active != record.hierarchy_active)
                {
                    return Err("Conflicting environment GameObject facts".into());
                }
            }
        }
        if objects
            .keys()
            .any(|id| ids.contains(id) || collider_owners.contains_key(id))
        {
            return Err("Environment GameObject aliases a captured component".into());
        }
        if self
            .volumes
            .iter()
            .chain(&self.checks)
            .flat_map(|r| &r.collider_ids)
            .any(|id| ids.contains(id))
        {
            return Err("Environment collider reference aliases a captured attribute".into());
        }
        if self
            .checks
            .iter()
            .any(|r| r.field_names != ["Center", "Size", "Type"])
        {
            return Err("Unsupported environment check fields".into());
        }
        if let Some(meshes) = &self.collider_meshes {
            if meshes.len() > 4096
                || meshes.iter().map(|m| m.vertices.len()).sum::<usize>() > 262144
                || meshes.iter().map(|m| m.triangles.len()).sum::<usize>() > 262144
            {
                return Err("Environment collider mesh budget exceeded".into());
            }
            let mut mesh_ids = std::collections::BTreeSet::new();
            for mesh in meshes {
                let referenced = self.volumes.iter().any(|record| {
                    record
                        .collider_ids
                        .iter()
                        .zip(&record.collider_types)
                        .any(|(&id, &kind)| id == mesh.collider_id && kind == 64)
                });
                if mesh.collider_id == 0
                    || mesh.mesh_id == 0
                    || !mesh_ids.insert(mesh.collider_id)
                    || !referenced
                    || mesh.mesh_asset.is_empty()
                    || mesh.mesh_asset.len() > 256
                    || mesh.mesh_asset.contains(['\0', '/', '\\'])
                    || !(3..=65536).contains(&mesh.vertices.len())
                    || !(1..=131072).contains(&mesh.triangles.len())
                    || mesh
                        .vertices
                        .iter()
                        .flatten()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || mesh
                        .triangles
                        .iter()
                        .flatten()
                        .any(|&i| i as usize >= mesh.vertices.len())
                {
                    return Err("Invalid environment collider mesh geometry or reference".into());
                }
                if self.volumes.iter().any(|record| {
                    record.game_object.as_ref().is_some_and(|object| {
                        record
                            .collider_ids
                            .iter()
                            .position(|id| *id == mesh.collider_id)
                            .is_some_and(|index| object.collider_enabled[index] != mesh.enabled)
                    })
                }) {
                    return Err("Environment collider enabled flags disagree".into());
                }
            }
            let missing = self.volumes.iter().any(|record| {
                record
                    .collider_ids
                    .iter()
                    .zip(&record.collider_types)
                    .any(|(&id, &kind)| kind == 64 && !mesh_ids.contains(&id))
            });
            if missing && self.issues.is_empty() {
                return Err("Environment collider mesh capture is incomplete".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootDecor {
    pub object_id: i64,
    pub active: bool,
    pub kind: String,
    /// Source order: Align = normal/gradient, offset xyz, scale xyz;
    /// Transform = position xyz, Euler rotation xyz, scale xyz;
    /// Scale/Rotate = min xyz, max xyz. Swim has no parameters.
    pub values: Vec<f32>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainCheck {
    pub object_id: i64,
    pub active: bool,
    pub position: [f32; 3],
    pub rotate: bool,
    pub extents: f32,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WaterCheck {
    pub object_id: i64,
    pub active: bool,
    pub position: [f32; 3],
    pub rotate: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathConnector {
    pub object_id: i64,
    pub active: bool,
    pub position: [f32; 3],
    pub infrastructure_type: i32,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementBlocker {
    pub object_id: i64,
    pub active: bool,
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
    pub opacity: f32,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attributes {
    pub version: u32,
    pub root_decor: Vec<RootDecor>,
    pub terrain_checks: Vec<TerrainCheck>,
    pub water_checks: Vec<WaterCheck>,
    pub path_connectors: Vec<PathConnector>,
    #[serde(default)]
    pub placement_blockers: Option<Vec<PlacementBlocker>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    pub issues: Vec<String>,
}
impl Attributes {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = self.placement_ids()?;
        if let Some(environment) = &self.environment {
            environment.validate(&mut ids)?;
        }
        Ok(())
    }
    /// Existing coastal evaluation does not execute environment checks. Validate
    /// its consumed fields per candidate; full captures are checked at load time.
    pub(crate) fn validate_coastal_inputs(&self) -> Result<(), String> {
        self.placement_ids().map(|_| ())
    }
    fn placement_ids(&self) -> Result<std::collections::BTreeSet<i64>, String> {
        let valid = |v: f32| v.is_finite() && v.abs() <= 10000.;
        let mut ids = std::collections::BTreeSet::new();
        if !matches!(self.version, 1..=3)
            || (self.version >= 2) != self.placement_blockers.is_some()
            || (self.version == 3) != self.environment.is_some()
            || self
                .placement_blockers
                .as_ref()
                .is_some_and(|b| b.len() > 4096)
            || self.root_decor.len() > 4096
            || self.terrain_checks.len() > 4096
            || self.water_checks.len() > 4096
            || self.path_connectors.len() > 4096
            || self.issues.len() > 256
            || self.issues.iter().any(|s| s.is_empty() || s.len() > 1024)
        {
            return Err("Invalid monument placement-attribute limits".into());
        }
        for d in &self.root_decor {
            let count = match d.kind.as_str() {
                "DecorSwim" => Some(0),
                "DecorAlign" => Some(8),
                "DecorTransform" => Some(9),
                "DecorScale" | "DecorRotate" => Some(6),
                _ => None,
            };
            if d.object_id == 0
                || !ids.insert(d.object_id)
                || !d.kind.starts_with("Decor")
                || d.kind.len() > 128
                || d.values.len() > 9
                || !d.values.iter().copied().all(valid)
                || count.is_some_and(|n| d.values.len() != n)
                || count.is_none() && (!d.values.is_empty() || self.issues.is_empty())
            {
                return Err("Invalid root monument decorator record".into());
            }
        }
        for c in &self.terrain_checks {
            if c.object_id == 0
                || !ids.insert(c.object_id)
                || !c.position.iter().copied().all(valid)
                || !valid(c.extents)
                || c.extents < 0.
            {
                return Err("Invalid authored terrain check".into());
            }
        }
        for c in &self.water_checks {
            if c.object_id == 0
                || !ids.insert(c.object_id)
                || !c.position.iter().copied().all(valid)
            {
                return Err("Invalid authored water check".into());
            }
        }
        for c in &self.path_connectors {
            if c.object_id == 0
                || !ids.insert(c.object_id)
                || !c.position.iter().copied().all(valid)
                || !(0..=64).contains(&c.infrastructure_type)
            {
                return Err("Invalid authored path connector".into());
            }
        }
        for b in self.placement_blockers.iter().flatten() {
            if b.object_id == 0
                || !ids.insert(b.object_id)
                || !b.position.iter().copied().all(valid)
                || !b.radius.is_finite()
                || !(0. ..=2048.).contains(&b.radius)
                || !b.fade.is_finite()
                || !(0. ..=2048.).contains(&b.fade)
                || !b.opacity.is_finite()
                || !(0. ..=1.).contains(&b.opacity)
            {
                return Err("Invalid authored monument placement blocker".into());
            }
        }
        Ok(ids)
    }
    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({"version":self.version,"root_decor_order":self.root_decor.iter().map(|d|&d.kind).collect::<Vec<_>>(),
            "blocker_coverage":self.placement_blockers.is_some(),"placement_blockers":self.placement_blockers.as_ref().map(Vec::len),
            "environment": self.environment.as_ref().map(|e| serde_json::json!({
                "root_transform_captured":e.root_transform.is_some(),"volumes":e.volumes.len(),"checks":e.checks.len(),
                "root_position_captured":e.root_transform.as_ref().is_some_and(|t|t.position.is_some()),
                "collider_meshes_captured":e.collider_meshes.as_ref().map(Vec::len),
                "water_placement":e.water_placement.as_ref().map(|w|serde_json::json!({
                    "version":w.version,"bodies":w.bodies.len(),"issues":w.issues,
                    "trigger_references":w.bodies.iter().map(|b|b.triggers.len()).sum::<usize>(),
                    "runtime_bounds_captured":false,"executed":false})),
                "game_objects_captured":e.volumes.iter().chain(&e.checks).filter(|v|v.game_object.is_some()).count(),
                "inactive_or_disabled":e.volumes.iter().chain(&e.checks).filter(|r|!r.hierarchy_active || !r.component_enabled).count(),
                "existing_non_box_colliders":e.volumes.iter().filter(|r|r.collider_types.first().is_some_and(|&t|t!=65)).count(),
                "issues":e.issues,"identity_scope":"EnvironmentVolume and EnvironmentVolumeCheck in Assembly-CSharp global namespace","executed":false
            })),
            "terrain_checks":self.terrain_checks.len(),"water_checks":self.water_checks.len(),
            "path_connectors":self.path_connectors.len(),"inactive_records":self.root_decor.iter().filter(|d|!d.active).count()+self.terrain_checks.iter().filter(|d|!d.active).count()+self.water_checks.iter().filter(|d|!d.active).count()+self.path_connectors.iter().filter(|d|!d.active).count()+self.placement_blockers.iter().flatten().filter(|d|!d.active).count(),
            "issues":self.issues,"executed":false})
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn water_placement_limits_bound_records_references_and_issues() {
        use super::{WaterBody, WaterPlacement, WaterTrigger};
        let body = |id, count| WaterBody {
            object_id: id,
            body_type: 1,
            component_enabled: true,
            hierarchy_active_without_root: true,
            position: [0.; 3],
            triggers: vec![
                WaterTrigger {
                    file_id: 0,
                    path_id: 0
                };
                count
            ],
        };
        let mut water = WaterPlacement {
            version: 1,
            bodies: (1..=4).map(|id| body(id, 1024)).collect(),
            issues: vec!["missing".into(); 256],
        };
        water.validate(&mut Default::default()).unwrap();
        water.bodies.push(body(5, 1));
        assert!(water.validate(&mut Default::default()).is_err());
        water.bodies = vec![body(1, 1025)];
        assert!(water.validate(&mut Default::default()).is_err());
        water.bodies = (1..=1025).map(|id| body(id, 0)).collect();
        assert!(water.validate(&mut Default::default()).is_err());
        water.bodies = vec![body(1, 0)];
        water.issues.push("missing".into());
        assert!(water.validate(&mut Default::default()).is_err());
        water.issues.clear();
        let mut occupied = std::collections::BTreeSet::from([1]);
        assert!(water.validate(&mut occupied).is_err());
    }
    #[test]
    fn water_placement_capture_preserves_unknown_legacy_and_ordered_raw_facts() {
        use super::Attributes;
        let mut data = serde_json::json!({"version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],
            "environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},"volumes":[],"checks":[],"issues":[]}});
        let legacy: Attributes = serde_json::from_value(data.clone()).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.environment.unwrap().water_placement.is_none());
        data["environment"]["water_placement"] =
            serde_json::json!({"version":1,"bodies":[],"issues":[]});
        let empty: Attributes = serde_json::from_value(data.clone()).unwrap();
        empty.validate().unwrap();
        assert!(
            empty
                .environment
                .unwrap()
                .water_placement
                .unwrap()
                .bodies
                .is_empty()
        );
        data["environment"]["water_placement"]["bodies"] = serde_json::json!([
            {"object_id":10,"body_type":2,"component_enabled":false,"hierarchy_active_without_root":false,"position":[1,2,3],"triggers":[{"file_id":0,"path_id":0},{"file_id":0,"path_id":42},{"file_id":0,"path_id":42},{"file_id":3,"path_id":-17}]},
            {"object_id":2,"body_type":-2147483648i64,"component_enabled":true,"hierarchy_active_without_root":true,"position":[4,5,6],"triggers":[]}]);
        let valid: Attributes = serde_json::from_value(data.clone()).unwrap();
        valid.validate().unwrap();
        let water = valid.environment.unwrap().water_placement.unwrap();
        assert_eq!(
            water.bodies.iter().map(|b| b.object_id).collect::<Vec<_>>(),
            [10, 2]
        );
        assert_eq!(water.bodies[0].triggers[2].path_id, 42);
        assert_eq!(water.bodies[0].triggers[3].file_id, 3);
        for (path, value) in [
            ("/environment/water_placement/version", serde_json::json!(2)),
            (
                "/environment/water_placement/bodies/1/object_id",
                serde_json::json!(10),
            ),
            (
                "/environment/water_placement/bodies/0/position/0",
                serde_json::json!(10001),
            ),
            (
                "/environment/water_placement/bodies/0/triggers/0/file_id",
                serde_json::json!(1),
            ),
            (
                "/environment/water_placement/bodies/0/triggers/1/file_id",
                serde_json::json!(-1),
            ),
        ] {
            let mut bad = data.clone();
            *bad.pointer_mut(path).unwrap() = value;
            let bad: Attributes = serde_json::from_value(bad).unwrap();
            assert!(bad.validate().is_err(), "{path}");
        }
    }
    use super::*;
    #[test]
    fn environment_coverage_distinguishes_legacy_missing_from_captured_empty() {
        let mut doc = serde_json::json!({"version":2,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[]});
        let legacy: Attributes = serde_json::from_value(doc.clone()).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.environment.is_none());
        doc["version"] = 3.into();
        assert!(
            serde_json::from_value::<Attributes>(doc.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        doc["environment"] = serde_json::json!({"root_transform":{"rotation":[0,0,0,1],"scale":[1,1,1]},"volumes":[],"checks":[],"issues":[]});
        let captured: Attributes = serde_json::from_value(doc.clone()).unwrap();
        captured.validate().unwrap();
        assert_eq!(captured.audit()["environment"]["volumes"], 0);
        assert_eq!(captured.audit()["environment"]["executed"], false);
        doc["version"] = 2.into();
        assert!(
            serde_json::from_value::<Attributes>(doc)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    #[test]
    fn environment_geometry_preserves_full_transform_masks_and_collider_inventory() {
        let record = serde_json::json!({"object_id":42,"hierarchy_active":false,"component_enabled":true,"type_mask":17,"center":[1,2,3],"size":[8,10,12],"local_to_root":[[0,0,2,12],[0,3,0,-4],[-1,0,0,5],[0,0,0,1]],"collider_types":[64],"collider_ids":[1001],"rotation_chain":[[0,1,0,0]],"field_names":["Center","Size","Type"]});
        let mut doc = serde_json::json!({"version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],"environment":{"root_transform":{"rotation":[0,0,0,1],"scale":[-1,2,3]},"volumes":[record],"checks":[],"issues":[]}});
        let check = |d: &serde_json::Value| {
            serde_json::from_value::<Attributes>(d.clone())
                .unwrap()
                .validate()
        };
        check(&doc).unwrap();
        let data: Attributes = serde_json::from_value(doc.clone()).unwrap();
        assert_eq!(data.audit()["environment"]["existing_non_box_colliders"], 1);
        assert_eq!(
            data.environment.as_ref().unwrap().volumes[0].local_to_root[2],
            [-1., 0., 0., 5.]
        );
        for (path, value) in [
            ("/environment/volumes/0/size/0", serde_json::json!(-1)),
            (
                "/environment/root_transform/rotation/3",
                serde_json::json!(2),
            ),
            (
                "/environment/volumes/0/local_to_root/3/0",
                serde_json::json!(1),
            ),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(check(&bad).is_err());
        }
        doc["environment"]["checks"] = doc["environment"]["volumes"].clone();
        assert!(
            check(&doc).is_err(),
            "Duplicate component IDs cannot satisfy capture coverage"
        );
        doc["environment"]["checks"][0]["object_id"] = 43.into();
        check(&doc).unwrap();
        doc["environment"]["checks"][0]["field_names"] =
            serde_json::json!(["Center", "NewDepth", "Size", "Type"]);
        assert!(check(&doc).is_err());
        let mut nonfinite = data;
        nonfinite.environment.as_mut().unwrap().volumes[0].local_to_root[0][0] = f32::NAN;
        assert!(nonfinite.validate().is_err());
        assert!(nonfinite.validate_coastal_inputs().is_ok());
    }
    #[test]
    fn collider_mesh_capture_is_optional_but_known_coverage_must_match_references() {
        let mut doc = serde_json::json!({"version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],"environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},"volumes":[{"object_id":42,"hierarchy_active":false,"component_enabled":true,"type_mask":17,"center":[0,0,0],"size":[1,1,1],"local_to_root":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"collider_types":[64],"collider_ids":[1001],"rotation_chain":[],"field_names":["Center","Size","Type"]}],"checks":[],"issues":[]}});
        let read = |d: &serde_json::Value| -> Result<Attributes, String> {
            let attrs: Attributes = serde_json::from_value(d.clone()).map_err(|e| e.to_string())?;
            attrs.validate()?;
            Ok(attrs)
        };
        let legacy = read(&doc).unwrap();
        assert!(legacy.environment.unwrap().collider_meshes.is_none());
        doc["environment"]["collider_meshes"] = serde_json::json!([]);
        assert!(read(&doc).is_err());
        doc["environment"]["issues"] = serde_json::json!(["Mesh decoding unsupported"]);
        read(&doc).unwrap();
        doc["environment"]["issues"] = serde_json::json!([]);
        let mesh = serde_json::json!({"collider_id":1001,"mesh_asset":"CAB-owned","mesh_id":77,"enabled":false,"is_trigger":true,"convex":true,"cooking_options":30,"vertices":[[0,0,0],[1,2,3],[-1,0,2]],"triangles":[[2,1,0]]});
        doc["environment"]["collider_meshes"] = serde_json::json!([mesh]);
        let captured = read(&doc).unwrap();
        let meshes = captured
            .environment
            .as_ref()
            .unwrap()
            .collider_meshes
            .as_ref()
            .unwrap();
        assert_eq!(meshes[0].triangles, [[2, 1, 0]]);
        assert_eq!(meshes[0].vertices[1], [1., 2., 3.]);
        assert!(meshes[0].convex && !meshes[0].enabled && meshes[0].is_trigger);
        assert_eq!(
            captured.audit()["environment"]["collider_meshes_captured"],
            1
        );
        for (path, value) in [
            (
                "/environment/collider_meshes/0/collider_id",
                serde_json::json!(1002),
            ),
            (
                "/environment/collider_meshes/0/mesh_asset",
                serde_json::json!("../mesh"),
            ),
            (
                "/environment/collider_meshes/0/mesh_id",
                serde_json::json!(0),
            ),
            (
                "/environment/collider_meshes/0/triangles/0/2",
                serde_json::json!(3),
            ),
            (
                "/environment/collider_meshes/0/cooking_options",
                serde_json::json!(-1),
            ),
        ] {
            let mut invalid = doc.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(read(&invalid).is_err(), "{path}");
        }
        let duplicate = doc["environment"]["collider_meshes"][0].clone();
        doc["environment"]["collider_meshes"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(read(&doc).is_err());
    }
    #[test]
    fn environment_object_links_must_match_components_layers_and_shared_facts() {
        let base = serde_json::json!({"version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],"environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},"volumes":[{"object_id":50,"hierarchy_active":true,"component_enabled":true,"type_mask":17,"center":[0,0,0],"size":[1,1,1],"local_to_root":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"collider_types":[64],"collider_ids":[52],"rotation_chain":[],"field_names":["Center","Size","Type"],"game_object":{"object_id":90,"layer":18,"volume_components":[50],"collider_enabled":[false]}}],"checks":[],"issues":[]}});
        let read = |d: &serde_json::Value| -> Result<Attributes, String> {
            let data: Attributes = serde_json::from_value(d.clone()).map_err(|e| e.to_string())?;
            data.validate()?;
            Ok(data)
        };
        read(&base).unwrap();
        for (path, value) in [
            (
                "/environment/volumes/0/game_object/layer",
                serde_json::json!(32),
            ),
            (
                "/environment/volumes/0/game_object/object_id",
                serde_json::json!(50),
            ),
            (
                "/environment/volumes/0/game_object/object_id",
                serde_json::json!(52),
            ),
            (
                "/environment/volumes/0/game_object/volume_components",
                serde_json::json!([51]),
            ),
            (
                "/environment/volumes/0/game_object/collider_enabled",
                serde_json::json!([]),
            ),
        ] {
            let mut bad = base.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(read(&bad).is_err(), "{path}");
        }
        let mut shared = base.clone();
        let mut second = shared["environment"]["volumes"][0].clone();
        second["object_id"] = 51.into();
        shared["environment"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(second);
        for record in shared["environment"]["volumes"].as_array_mut().unwrap() {
            record["game_object"]["volume_components"] = serde_json::json!([50, 51]);
        }
        read(&shared).unwrap();
        shared["environment"]["volumes"][1]["game_object"]["layer"] = 16.into();
        assert!(read(&shared).is_err());
    }
    #[test]
    fn blocker_coverage_distinguishes_legacy_missing_and_verified_empty() {
        let mut doc = serde_json::json!({"version":1,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"issues":[]});
        let check = |d: &serde_json::Value| {
            serde_json::from_value::<Attributes>(d.clone())
                .unwrap()
                .validate()
        };
        check(&doc).unwrap();
        doc["version"] = 2.into();
        assert!(check(&doc).is_err());
        doc["placement_blockers"] = serde_json::json!([]);
        check(&doc).unwrap();
        doc["placement_blockers"] = serde_json::json!([{"object_id":1,"active":false,"position":[10,0,20],"radius":30,"fade":5,"opacity":0}]);
        check(&doc).unwrap();
        let attributes: Attributes = serde_json::from_value(doc.clone()).unwrap();
        assert_eq!(attributes.audit()["placement_blockers"], 1);
        doc["placement_blockers"][0]["radius"] = (-1).into();
        assert!(check(&doc).is_err());
        doc["placement_blockers"] = serde_json::json!([]);
        doc["version"] = 1.into();
        assert!(check(&doc).is_err());
    }
    #[test]
    fn validates_source_order_parameters_identity_and_unknown_coverage() {
        let doc = serde_json::json!({"version":1,"root_decor":[{"object_id":1,"active":true,"kind":"DecorSwim","values":[]},{"object_id":2,"active":true,"kind":"DecorAlign","values":[0,1,0,0,0,1,1,1]},{"object_id":3,"active":true,"kind":"DecorTransform","values":[0,0,0,0,-90,0,1,1,1]}],"terrain_checks":[{"object_id":4,"active":true,"position":[-180,-1,120],"rotate":true,"extents":40}],"water_checks":[],"path_connectors":[{"object_id":5,"active":false,"position":[0,0,200],"infrastructure_type":5}],"issues":[]});
        let check = |d: &serde_json::Value| {
            serde_json::from_value::<Attributes>(d.clone())
                .unwrap()
                .validate()
        };
        check(&doc).unwrap();
        for (pointer, value) in [
            ("/version", serde_json::json!(2)),
            ("/terrain_checks/0/extents", serde_json::json!(-1)),
            ("/root_decor/1/values", serde_json::json!([0, 1])),
            ("/path_connectors/0/object_id", serde_json::json!(4)),
            (
                "/path_connectors/0/infrastructure_type",
                serde_json::json!(100),
            ),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(check(&bad).is_err(), "{pointer}");
        }
        let mut future = doc;
        future["root_decor"][0]["kind"] = "DecorFuture".into();
        assert!(check(&future).is_err());
        future["issues"] = serde_json::json!(["Unsupported root decorator: DecorFuture"]);
        check(&future).unwrap();
        let parsed: Attributes = serde_json::from_value(future).unwrap();
        assert_eq!(parsed.audit()["executed"], false);
        assert_eq!(parsed.audit()["inactive_records"], 1);
    }
}
