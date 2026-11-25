//! Assemble authored tag geometry at an instantiated prefab-root pose.
//! Authored registration uses an explicit independent geometry policy.
use papyrus_terrain::{
    environment::{self, Box3, Key, Transform},
    water_treatment_clearance::{capsule_bounds, sphere_bounds},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod mesh;
mod monument;
mod registry;
use mesh::MeshBounds;
use monument::Monument;
pub use registry::{PendingTag, Placement, Registry};

pub const AUTHORED_VERSION: &str = "mapper-authored-monument-tags-1";
#[cfg(test)]
mod authored_tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    space: String,
    monuments: Vec<Monument>,
    tags: Vec<Tag>,
    issues: Vec<serde_json::Value>,
    complete: bool,
    root_rotation: [f32; 4],
    root_scale: [f32; 3],
    nodes_visited: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    file_id: u32,
    #[serde(rename = "path_id")]
    _path_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tag {
    object_id: i64,
    root: bool,
    hierarchy_active: bool,
    #[serde(rename = "component_enabled")]
    _component_enabled: bool,
    local_to_root: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
    #[serde(rename = "auto_find_monument")]
    _auto_find_monument: bool,
    attached_monument: Pointer,
    colliders: Vec<serde_json::Value>,
    collider_order: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoxCollider {
    #[serde(rename = "object_id")]
    _object_id: i64,
    #[serde(rename = "class_id")]
    _class_id: u32,
    #[serde(rename = "enabled")]
    _enabled: bool,
    #[serde(rename = "trigger")]
    _trigger: bool,
    center: [f32; 3],
    size: [f32; 3],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SphereCollider {
    #[serde(rename = "object_id")]
    _object_id: i64,
    #[serde(rename = "class_id")]
    _class_id: u32,
    #[serde(rename = "enabled")]
    _enabled: bool,
    #[serde(rename = "trigger")]
    _trigger: bool,
    center: [f32; 3],
    radius: f32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapsuleCollider {
    #[serde(rename = "object_id")]
    _object_id: i64,
    #[serde(rename = "class_id")]
    _class_id: u32,
    #[serde(rename = "enabled")]
    _enabled: bool,
    #[serde(rename = "trigger")]
    _trigger: bool,
    center: [f32; 3],
    radius: f32,
    height: f32,
    direction: u8,
}

/// Geometry from a single captured prefab, ready for repeated root poses.
pub struct Prepared {
    monuments: Vec<Monument>,
    tags: Vec<PreparedTag>,
    indices: BTreeMap<i64, usize>,
}
struct PreparedTag {
    object_id: i64,
    hierarchy_active: bool,
    local_to_root: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
    colliders: Vec<(i64, Collider)>,
}
enum Collider {
    Box(BoxCollider),
    Sphere(SphereCollider),
    Capsule(CapsuleCollider),
    RuntimeMesh(Option<MeshBounds>),
}
#[derive(Debug, Serialize)]
pub struct Pending {
    pub tag: i64,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct Assembly {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<&'static str>,
    pub boxes: Vec<(Key, Box3)>,
    pub pending: Vec<Pending>,
    pub inactive_tags: usize,
    pub tags_without_colliders: usize,
}

fn vector(values: &[f32]) -> bool {
    values
        .iter()
        .all(|value| value.is_finite() && value.abs() <= 1_000_000.)
}
fn rotation(value: [f32; 4]) -> Result<(), String> {
    Transform {
        position: [0.; 3],
        rotation: value,
        scale: [1.; 3],
    }
    .validate()
}

impl Prepared {
    /// The installation-bound loader supplies one captured prefab body.
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("Monument tag geometry exceeds 16 MiB".into());
        }
        let capture: Capture = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if capture.space != "prefab-root-local-unity"
            || !capture.complete
            || !capture.issues.is_empty()
            || capture.nodes_visited > 65536
            || capture.tags.len() > 4096
            || capture.monuments.len() > 1024
        {
            return Err("Incomplete or unsupported monument tag capture".into());
        }
        rotation(capture.root_rotation)?;
        if !vector(&capture.root_scale) || capture.root_scale.contains(&0.) {
            return Err("Invalid monument tag root scale".into());
        }
        let mut monuments = Vec::with_capacity(capture.monuments.len());
        let mut monument_ids = BTreeSet::new();
        for monument in capture.monuments {
            if !monument_ids.insert(monument.object_id) {
                return Err("Duplicate monument phrase component".into());
            }
            monuments.push(monument.prepare()?);
        }
        let mut indices = BTreeMap::new();
        let mut tags = Vec::with_capacity(capture.tags.len());
        for tag in capture.tags {
            if tag.object_id == 0
                || indices.insert(tag.object_id, tags.len()).is_some()
                || tag.colliders.len() > 64
                || tag.rotation_chain.len() > 128
                || tag.collider_order != "serialized_component_order"
                || !vector(tag.local_to_root.as_flattened())
                || tag.local_to_root[3] != [0., 0., 0., 1.]
                || tag.attached_monument.file_id > 65536
                || tag.root && !tag.rotation_chain.is_empty()
            {
                return Err("Invalid monument tag identity or transform".into());
            }
            for &quaternion in &tag.rotation_chain {
                rotation(quaternion)?;
            }
            let mut collider_ids = BTreeSet::new();
            let mut colliders = Vec::with_capacity(tag.colliders.len());
            for collider in &tag.colliders {
                let id = collider["object_id"]
                    .as_i64()
                    .ok_or("Missing tagged collider ID")?;
                if id == 0 || !collider_ids.insert(id) {
                    return Err("Duplicate or zero tagged collider ID".into());
                }
                // Primitive field layouts are validated once during preparation.
                // Mesh data stays opaque until a cooked-world-bounds provider exists.
                let prepared = Collider::read(collider)?;
                let _ = prepared.bounds(
                    Transform {
                        position: [0.; 3],
                        rotation: [0., 0., 0., 1.],
                        scale: [1.; 3],
                    },
                    tag.local_to_root,
                )?;
                colliders.push((id, prepared));
            }
            tags.push(PreparedTag {
                object_id: tag.object_id,
                hierarchy_active: tag.hierarchy_active,
                local_to_root: tag.local_to_root,
                rotation_chain: tag.rotation_chain,
                colliders,
            });
        }
        Ok(Self {
            monuments,
            tags,
            indices,
        })
    }

    /// Active authored MonumentInfo records matching a lower-case token substring.
    /// Registration of these records in a generated world belongs to the caller.
    pub fn matching_monuments(&self, needle: &str) -> Result<Vec<i64>, String> {
        monument::validate_query(needle)?;
        let mut matches = Vec::new();
        for monument in &self.monuments {
            if monument.matches(needle)? {
                matches.push(monument.object_id);
            }
        }
        Ok(matches)
    }

    /// `pose` is the actual prefab-root world pose, including its authored basis.
    /// Ambiguous tags require an explicit tag-to-collider selection from the caller.
    /// Authored hierarchy activation is assumed after the caller's spawn lifecycle.
    pub fn assemble(
        &self,
        instance: u64,
        pose: Transform,
        selected: &BTreeMap<i64, i64>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Assembly>, String> {
        self.assemble_inner(instance, pose, selected, false, cancelled)
    }

    /// Mapper's independent policy selects the first serialized collider and uses
    /// transformed authored mesh AABBs. It supplies deterministic static volumes.
    pub fn assemble_authored(
        &self,
        instance: u64,
        pose: Transform,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Assembly>, String> {
        self.assemble_inner(instance, pose, &BTreeMap::new(), true, cancelled)
    }

    fn assemble_inner(
        &self,
        instance: u64,
        pose: Transform,
        selected: &BTreeMap<i64, i64>,
        authored: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Assembly>, String> {
        pose.validate()?;
        if selected.len() > self.tags.len() {
            return Err("Invalid monument tag selections".into());
        }
        for (&tag_id, &collider_id) in selected {
            let &index = self
                .indices
                .get(&tag_id)
                .ok_or("Collider selection references an absent tag")?;
            let tag = &self.tags[index];
            if !tag.colliders.iter().any(|(id, _)| *id == collider_id) {
                return Err("Collider selection belongs to another tag".into());
            }
        }
        let mut result = Assembly {
            engine: authored.then_some(AUTHORED_VERSION),
            boxes: vec![],
            pending: vec![],
            inactive_tags: 0,
            tags_without_colliders: 0,
        };
        let mut rotations = Vec::new();
        for tag in &self.tags {
            if cancelled() {
                return Ok(None);
            }
            if !tag.hierarchy_active {
                result.inactive_tags += 1;
                continue;
            }
            let collider = if let Some(&id) = selected.get(&tag.object_id) {
                &tag.colliders
                    .iter()
                    .find(|(collider_id, _)| *collider_id == id)
                    .unwrap()
                    .1
            } else if tag.colliders.is_empty() {
                result.tags_without_colliders += 1;
                continue;
            } else if authored || tag.colliders.len() == 1 {
                &tag.colliders[0].1
            } else {
                result.pending.push(Pending {
                    tag: tag.object_id,
                    reason: "Runtime collider selection required".into(),
                });
                continue;
            };
            let mut matrix = tag.local_to_root;
            for column in [0, 1, 2, 3] {
                let point = std::array::from_fn(|i| tag.local_to_root[i][column] * pose.scale[i]);
                let rotated = environment::rotate(pose.rotation, point);
                for i in 0..3 {
                    matrix[i][column] =
                        rotated[i] + if column == 3 { pose.position[i] } else { 0. };
                }
            }
            rotations.clear();
            rotations.push(pose.rotation);
            rotations.extend_from_slice(&tag.rotation_chain);
            let bounds = if let Collider::RuntimeMesh(Some(bounds)) = collider
                && authored
            {
                bounds.world_bounds(matrix).map(Some)
            } else {
                crate::environment_checks::local_transform(matrix, &rotations, true)
                    .and_then(|frame| collider.bounds(frame, matrix))
            };
            match bounds {
                Ok(Some(bounds)) => result.boxes.push((
                    Key {
                        instance,
                        component: tag.object_id,
                    },
                    bounds,
                )),
                Ok(None) => result.pending.push(Pending {
                    tag: tag.object_id,
                    reason: "Runtime mesh collider bounds required".into(),
                }),
                Err(reason) => result.pending.push(Pending {
                    tag: tag.object_id,
                    reason,
                }),
            }
        }
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(result))
    }
}

impl Collider {
    fn read(value: &serde_json::Value) -> Result<Self, String> {
        match value["class_id"].as_u64() {
            Some(65) => serde_json::from_value(value.clone())
                .map(Self::Box)
                .map_err(|e| e.to_string()),
            Some(135) => serde_json::from_value(value.clone())
                .map(Self::Sphere)
                .map_err(|e| e.to_string()),
            Some(136) => serde_json::from_value(value.clone())
                .map(Self::Capsule)
                .map_err(|e| e.to_string()),
            Some(64) => MeshBounds::read(value).map(Self::RuntimeMesh),
            _ => Err("Unsupported tagged collider class".into()),
        }
    }
    fn bounds(&self, frame: Transform, matrix: [[f32; 4]; 4]) -> Result<Option<Box3>, String> {
        let bounds = match self {
            Self::Box(collider) => environment::check_box(
                [0.; 3],
                [0., 0., 0., 1.],
                collider.center,
                collider.size,
                frame,
            )?,
            Self::Sphere(collider) => {
                let center = std::array::from_fn(|i| {
                    ((matrix[i][0] * collider.center[0] + matrix[i][1] * collider.center[1])
                        + matrix[i][2] * collider.center[2])
                        + matrix[i][3]
                });
                sphere_bounds(center, collider.radius, frame.scale)?
            }
            Self::Capsule(collider) => capsule_bounds(
                frame,
                collider.center,
                collider.radius,
                collider.height,
                collider.direction,
            )?,
            Self::RuntimeMesh(_) => return Ok(None),
        };
        Ok(Some(bounds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::{
        decor_stream::Budget,
        environment::{Coverage, Verdict},
        water_treatment_clearance::Clearance,
    };
    use serde_json::{Value, json};

    pub(super) fn fixture() -> Value {
        json!({"space":"prefab-root-local-unity","monuments":[],"tags":[{
            "object_id":1,"root":false,"hierarchy_active":true,"component_enabled":true,
            "local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
            "rotation_chain":[],"auto_find_monument":false,"attached_monument":{"file_id":0,"path_id":0},
            "collider_order":"serialized_component_order","colliders":[{
                "object_id":11,"class_id":65,"enabled":true,"trigger":false,"center":[1.,0.,0.],"size":[2.,4.,6.]
            }]}],"issues":[],"complete":true,"root_rotation":[0.,0.,0.,1.],"root_scale":[1.,1.,1.],"nodes_visited":2})
    }
    pub(super) fn read(value: &Value) -> Prepared {
        Prepared::read(&serde_json::to_vec(value).unwrap()).unwrap()
    }
    pub(super) fn pose() -> Transform {
        Transform {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    fn assemble(value: &Value) -> Assembly {
        read(value)
            .assemble(7, pose(), &BTreeMap::new(), || false)
            .unwrap()
            .unwrap()
    }
    fn near(a: [f32; 3], b: [f32; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 0.0001, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn assembly_composes_hierarchy_and_instance_frames_for_all_primitive_kinds() {
        let mut v = fixture();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        v["tags"][0]["local_to_root"] = json!([
            [0., 0., 4., 10.],
            [0., 3., 0., 0.],
            [-2., 0., 0., 5.],
            [0., 0., 0., 1.]
        ]);
        v["tags"][0]["rotation_chain"] = json!([[0., q, 0., q]]);
        let root = Transform {
            position: [100., 20., 200.],
            rotation: [0., q, 0., q],
            scale: [2.; 3],
        };
        let variants = [
            (
                json!({"object_id":11,"class_id":65,"enabled":true,"trigger":false,"center":[1.,0.,0.],"size":[2.,4.,6.]}),
                [106., 20., 180.],
                [4., 12., 24.],
            ),
            (
                json!({"object_id":11,"class_id":135,"enabled":true,"trigger":true,"center":[1.,2.,3.],"radius":1.}),
                [106., 32., 156.],
                [8.; 3],
            ),
            (
                json!({"object_id":11,"class_id":136,"enabled":true,"trigger":false,"center":[1.,0.,0.],"radius":1.,"height":6.,"direction":2}),
                [106., 20., 180.],
                [4., 6., 24.],
            ),
        ];
        for (collider, center, extents) in variants {
            v["tags"][0]["colliders"] = json!([collider]);
            let assembly = read(&v)
                .assemble(7, root, &BTreeMap::new(), || false)
                .unwrap()
                .unwrap();
            assert!(assembly.pending.is_empty());
            assert_eq!(assembly.boxes.len(), 1);
            near(assembly.boxes[0].1.center, center);
            near(assembly.boxes[0].1.extents, extents);
            let clearance = Clearance::new(&assembly.boxes, 7., Coverage::Complete).unwrap();
            assert_eq!(
                clearance
                    .check(center, &mut Budget::new(10).unwrap(), || false)
                    .unwrap()
                    .verdict,
                Verdict::Blocked(Key {
                    instance: 7,
                    component: 1
                })
            );
        }
    }

    #[test]
    fn disabled_behaviours_and_colliders_keep_get_component_geometry_while_inactive_children_skip()
    {
        let mut v = fixture();
        v["tags"][0]["component_enabled"] = false.into();
        v["tags"][0]["colliders"][0]["enabled"] = false.into();
        v["tags"][0]["colliders"][0]["trigger"] = true.into();
        assert_eq!(assemble(&v).boxes.len(), 1);
        v["tags"][0]["hierarchy_active"] = false.into();
        let result = assemble(&v);
        assert!(result.boxes.is_empty());
        assert_eq!(result.inactive_tags, 1);
        v["tags"][0]["hierarchy_active"] = true.into();
        v["tags"][0]["colliders"] = json!([]);
        let result = assemble(&v);
        assert_eq!(result.tags_without_colliders, 1);
        assert!(result.pending.is_empty());
    }

    #[test]
    fn ambiguous_colliders_require_an_explicit_valid_selection_and_meshes_stay_pending() {
        let mut v = fixture();
        let mut second = v["tags"][0]["colliders"][0].clone();
        second["object_id"] = 12.into();
        second["size"] = json!([4., 4., 4.]);
        v["tags"][0]["colliders"]
            .as_array_mut()
            .unwrap()
            .push(second);
        let source = read(&v);
        let result = assemble(&v);
        assert!(result.boxes.is_empty());
        assert_eq!(result.pending.len(), 1);
        let result = source
            .assemble(7, pose(), &BTreeMap::from([(1, 12)]), || false)
            .unwrap()
            .unwrap();
        near(result.boxes[0].1.extents, [2.; 3]);
        for selected in [BTreeMap::from([(2, 11)]), BTreeMap::from([(1, 99)])] {
            assert!(source.assemble(7, pose(), &selected, || false).is_err());
        }
        v["tags"][0]["colliders"] = json!([{"object_id":30,"class_id":64}]);
        let result = assemble(&v);
        assert!(result.boxes.is_empty());
        assert_eq!(
            result.pending[0].reason,
            "Runtime mesh collider bounds required"
        );
    }

    #[test]
    fn shear_reflection_and_runtime_pose_shear_preserve_unresolved_geometry() {
        for entry in [(0, 1, 0.3), (0, 0, -1.)] {
            let mut v = fixture();
            v["tags"][0]["local_to_root"][entry.0][entry.1] = json!(entry.2);
            let result = assemble(&v);
            assert!(result.boxes.is_empty());
            assert_eq!(result.pending.len(), 1);
        }
        let mut v = fixture();
        let q = (0.3_f32).sin();
        let w = (0.3_f32).cos();
        let rotation = [0., q, 0., w];
        let x = environment::rotate(rotation, [1., 0., 0.]);
        let z = environment::rotate(rotation, [0., 0., 1.]);
        v["tags"][0]["local_to_root"] = json!([
            [x[0], 0., z[0], 0.],
            [0., 1., 0., 0.],
            [x[2], 0., z[2], 0.],
            [0., 0., 0., 1.]
        ]);
        v["tags"][0]["rotation_chain"] = json!([rotation]);
        let result = read(&v)
            .assemble(
                7,
                Transform {
                    scale: [2., 1., 1.],
                    ..pose()
                },
                &BTreeMap::new(),
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.pending.len(), 1);
        assert!(result.boxes.is_empty());
    }

    #[test]
    fn source_validation_rejects_unknown_fields_duplicates_invalid_frames_and_incomplete_capture() {
        for case in 0..8 {
            let mut v = fixture();
            match case {
                0 => v["future_tag_layout"] = true.into(),
                1 => v["tags"][0]["colliders"][0]["future_rule"] = true.into(),
                2 => {
                    let tag = v["tags"][0].clone();
                    v["tags"].as_array_mut().unwrap().push(tag);
                }
                3 => v["tags"][0]["local_to_root"][3][0] = 1.into(),
                4 => v["tags"][0]["rotation_chain"] = json!([[0., 0., 0., 0.]]),
                5 => v["complete"] = false.into(),
                6 => v["tags"][0]["colliders"][0]["class_id"] = 99.into(),
                _ => v["tags"][0]["colliders"][0]["size"] = json!([-1., 1., 1.]),
            }
            assert!(
                Prepared::read(&serde_json::to_vec(&v).unwrap()).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn assemblies_are_repeatable_instance_scoped_and_cancel_without_a_partial_result() {
        let mut v = fixture();
        let mut tag = v["tags"][0].clone();
        tag["object_id"] = 2.into();
        v["tags"].as_array_mut().unwrap().push(tag);
        let source = read(&v);
        let selectors = BTreeMap::new();
        let first = source
            .assemble(7, pose(), &selectors, || false)
            .unwrap()
            .unwrap();
        let again = source
            .assemble(7, pose(), &selectors, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&again).unwrap()
        );
        let moved = source
            .assemble(
                8,
                Transform {
                    position: [10., 0., 0.],
                    ..pose()
                },
                &selectors,
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(moved.boxes[0].0.instance, 8);
        near(moved.boxes[0].1.center, [11., 0., 0.]);
        let calls = std::cell::Cell::new(0);
        assert!(
            source
                .assemble(7, pose(), &selectors, || {
                    calls.set(calls.get() + 1);
                    calls.get() == 2
                })
                .unwrap()
                .is_none()
        );
        assert_eq!(
            source
                .assemble(0, pose(), &selectors, || false)
                .unwrap()
                .unwrap()
                .boxes[0]
                .0
                .instance,
            0
        );
        assert!(
            source
                .assemble(
                    7,
                    Transform {
                        scale: [f32::NAN; 3],
                        ..pose()
                    },
                    &selectors,
                    || false
                )
                .is_err()
        );
    }
}
