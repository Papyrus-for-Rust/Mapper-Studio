//! Source-authored environment queries. Independent boxes/raw convex hulls, not Unity
//! physics or a substitute for complete generated-world registration.
use crate::monument_attributes::{Attributes, EnvironmentRecord, RootTransform};
use papyrus_terrain::environment::{
    self, Assessment, Box3, Key, Shape, Transform, Verdict, Volume, World,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Registration {
    Enabled,
    Disabled,
    Unknown,
    /// Both registration and a conservative geometry bound are unavailable.
    UnknownGeometry,
}
struct Record {
    source: EnvironmentRecord,
    transform: Transform,
    hull: Option<environment::Convex>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Query {
    pub component: i64,
    pub mask: u32,
    pub geometry: Box3,
    /// Authored preprocessing frame, distinct from the instance collider frame.
    pub source_transform: Transform,
}
impl Record {
    fn query(&self, check: bool, pose: Transform, forbidden: u32) -> Result<Query, String> {
        let geometry = if check {
            environment::check_box(
                self.transform.position,
                self.transform.rotation,
                self.source.center,
                self.source.size,
                pose,
            )?
        } else {
            environment::volume_box(self.transform, self.source.center, self.source.size, pose)?
        };
        Ok(Query {
            component: self.source.object_id,
            mask: if check {
                self.source.type_mask as u32
            } else {
                forbidden
            },
            geometry,
            source_transform: self.transform,
        })
    }
}
pub struct Prepared {
    pub root: RootTransform,
    checks: Vec<Record>,
    volumes: Vec<Record>,
    queries_complete: bool,
}
fn local(record: &EnvironmentRecord, needs_scale: bool) -> Result<Transform, String> {
    local_transform(record.local_to_root, &record.rotation_chain, needs_scale)
}
/// Recover a positive-scale hierarchy frame and reject shear or reflection.
pub fn local_transform(
    matrix: [[f32; 4]; 4],
    rotation_chain: &[[f32; 4]],
    needs_scale: bool,
) -> Result<Transform, String> {
    let rotation = rotation_chain
        .iter()
        .fold([0., 0., 0., 1.], |a, &b| environment::multiply(a, b));
    let position = std::array::from_fn(|i| matrix[i][3]);
    let mut scale = [1.; 3];
    if needs_scale {
        for axis in 0..3 {
            let mut unit = [0.; 3];
            unit[axis] = 1.;
            let direction = environment::rotate(rotation, unit);
            let column: [f32; 3] = std::array::from_fn(|i| matrix[i][axis]);
            scale[axis] = column.iter().map(|v| v * v).sum::<f32>().sqrt();
            if scale[axis] < 0.000001
                || (0..3).any(|i| (column[i] / scale[axis] - direction[i]).abs() > 0.0001)
            {
                return Err(
                    "Environment hierarchy has shear, reflection or a degenerate axis".into(),
                );
            }
        }
    }
    let result = Transform {
        position,
        rotation,
        scale,
    };
    result.validate()?;
    Ok(result)
}
fn transformed_record(record: &EnvironmentRecord, root: Transform) -> EnvironmentRecord {
    let mut world = record.clone();
    for column in 0..4 {
        let p = std::array::from_fn(|i| record.local_to_root[i][column] * root.scale[i]);
        for (i, value) in environment::rotate(root.rotation, p)
            .into_iter()
            .enumerate()
        {
            world.local_to_root[i][column] =
                value + if column == 3 { root.position[i] } else { 0. };
        }
    }
    world.rotation_chain.insert(0, root.rotation);
    world
}
impl Prepared {
    fn records(&self) -> impl Iterator<Item = (bool, &Record)> {
        self.checks
            .iter()
            .map(|r| (true, r))
            .chain(self.volumes.iter().map(|r| (false, r)))
    }
    pub fn new(
        attributes: &Attributes,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::prepare(attributes, true, cancelled)
    }
    /// Legacy captures put query-layout issues in the shared issue list. Those
    /// checks do not alter the volumes registered by an already-spawned actor.
    pub(crate) fn for_registration(
        attributes: &Attributes,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::prepare(attributes, false, cancelled)
    }
    fn prepare(
        attributes: &Attributes,
        require_queries: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        attributes.validate()?;
        let source = attributes
            .environment
            .as_ref()
            .ok_or("Authored environment capture is missing; refresh monument data")?;
        let queries_complete = source.issues.is_empty();
        if source
            .issues
            .iter()
            .any(|issue| require_queries || issue != "Unknown environment check layout")
        {
            return Err("Authored environment capture has unresolved issues".into());
        }
        let root = source
            .root_transform
            .clone()
            .ok_or("Environment root transform is missing")?;
        let asset_frame = Transform {
            position: root
                .position
                .ok_or("Authored root position is missing; refresh monument data")?,
            rotation: root.rotation,
            scale: root.scale,
        };
        asset_frame.validate()?;
        let mut hulls = std::collections::BTreeMap::new();
        for mesh in source.collider_meshes.iter().flatten().filter(|m| m.convex) {
            if cancelled() {
                return Ok(None);
            }
            match environment::Convex::build(&mesh.vertices, 2_000_000, &cancelled)? {
                Ok(hull) => {
                    hulls.insert(mesh.collider_id, hull);
                }
                Err(Verdict::Cancelled) => return Ok(None),
                Err(other) => return Err(format!("Environment hull preparation: {other:?}")),
            }
        }
        let collect =
            |records: &[EnvironmentRecord], needs_scale| -> Result<Option<Vec<Record>>, String> {
                let mut output = Vec::with_capacity(records.len());
                for record in records {
                    if cancelled() {
                        return Ok(None);
                    }
                    output.push(Record {
                        source: record.clone(),
                        transform: local(&transformed_record(record, asset_frame), needs_scale)?,
                        hull: record
                            .collider_ids
                            .first()
                            .filter(|_| record.collider_types.first() == Some(&64))
                            .and_then(|id| hulls.get(id))
                            .cloned(),
                    });
                }
                Ok(Some(output))
            };
        let Some(checks) = collect(&source.checks, false)? else {
            return Ok(None);
        };
        let Some(volumes) = collect(&source.volumes, true)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            root,
            checks,
            volumes,
            queries_complete,
        }))
    }
    pub fn counts(&self) -> [usize; 2] {
        [self.checks.len(), self.volumes.len()]
    }
    /// Conservative logical reservation for targets(), including record copies,
    /// affine transforms and hull assembly. Transforming a prepared hull does
    /// not run hull construction again. Charge every record regardless of the
    /// registration callback so lifecycle/cache state cannot bypass the bound.
    pub(crate) fn target_work_reservation(&self) -> usize {
        self.volumes.iter().fold(128, |work, record| {
            let geometry = record.hull.as_ref().map_or(0, |hull| {
                let [points, faces] = hull.counts();
                // assemble visits vertices and faces, creates up to three edge
                // IDs per face, then normalizes unique edges. Include margin
                // for bounded tree insertion and numeric operations.
                (points + faces * 8 + 1) * 128
            });
            work + geometry
                + (record.source.rotation_chain.len()
                    + record.source.collider_ids.len()
                    + record.source.field_names.len()
                    + 1)
                    * 128
        })
    }
    /// Poses are post-decor instance transforms. Root transforms and decorators
    /// are not applied a second time. Inactive child query volumes are retained.
    pub fn query_boxes(
        &self,
        pose: Transform,
        forbidden: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<Query>>, String> {
        pose.validate()?;
        if cancelled() {
            return Ok(None);
        }
        if !self.queries_complete {
            return Err("Authored environment queries are incomplete".into());
        }
        let mut output = Vec::with_capacity(self.checks.len() + self.volumes.len());
        for (check, record) in self.records() {
            if cancelled() {
                return Ok(None);
            }
            output.push(record.query(check, pose, forbidden)?);
        }
        Ok(Some(output))
    }
    /// Registration comes from the world's lifecycle adapter, never guessed
    /// from prefab capture activity flags. Captured convex meshes use the raw
    /// point-cloud hull model; this is not reproduction of PhysX mesh cooking.
    pub fn targets(
        &self,
        instance: u64,
        pose: Transform,
        registration: impl Fn(&EnvironmentRecord) -> Registration,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<Volume>>, String> {
        pose.validate()?;
        if instance == 0 {
            return Err("Missing environment instance identity".into());
        }
        if cancelled() {
            return Ok(None);
        }
        let mut targets = Vec::with_capacity(self.volumes.len());
        for record in &self.volumes {
            if cancelled() {
                return Ok(None);
            }
            let state = registration(&record.source);
            if state == Registration::Disabled {
                continue;
            }
            if state == Registration::UnknownGeometry
                || record
                    .source
                    .game_object
                    .as_ref()
                    .is_some_and(|o| o.volume_components.len() > 1)
            {
                targets.push(Volume {
                    key: Key {
                        instance,
                        component: record.source.object_id,
                    },
                    mask: record.source.type_mask as u32,
                    shape: Shape::Unresolved { bounds: None },
                });
                continue;
            }
            let known_box = record
                .source
                .collider_types
                .first()
                .is_none_or(|&kind| kind == 65);
            let shape = if known_box {
                let world = transformed_record(&record.source, pose);
                let frame = local(&world, true)?;
                let identity = Transform {
                    position: [0.; 3],
                    rotation: [0., 0., 0., 1.],
                    scale: [1.; 3],
                };
                let geometry = environment::volume_box(
                    frame,
                    record.source.center,
                    record.source.size,
                    identity,
                )?;
                if state == Registration::Enabled {
                    Shape::Box(geometry)
                } else {
                    Shape::Unresolved {
                        bounds: Some(geometry.bounds()),
                    }
                }
            } else if let Some(hull) = &record.hull {
                let frame = transformed_record(&record.source, pose);
                let Some(hull) = hull.transformed(frame.local_to_root, &cancelled)? else {
                    return Ok(None);
                };
                if state == Registration::Enabled {
                    Shape::Convex(hull)
                } else {
                    Shape::Unresolved {
                        bounds: Some(hull.bounds()),
                    }
                }
            } else {
                Shape::Unresolved { bounds: None }
            };
            targets.push(Volume {
                key: Key {
                    instance,
                    component: record.source.object_id,
                },
                mask: record.source.type_mask as u32,
                shape,
            });
        }
        Ok(Some(targets))
    }
    pub fn assess(
        &self,
        world: &World,
        pose: Transform,
        forbidden: u32,
        tolerance: f64,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Assessment, String> {
        if !(1..=128_000_000).contains(&maximum_work) {
            return Err("Invalid environment assessment work allowance".into());
        }
        pose.validate()?;
        if !tolerance.is_finite() || !(0. ..=1.).contains(&tolerance) {
            return Err("Invalid environment separation tolerance".into());
        }
        let mut result = Assessment {
            verdict: Verdict::Clear,
            work: 0,
            candidates: 0,
        };
        if cancelled() {
            result.verdict = Verdict::Cancelled;
            return Ok(result);
        }
        if !self.queries_complete {
            result.verdict = Verdict::Incomplete;
            return Ok(result);
        }
        for (check, record) in self.records() {
            if cancelled() {
                result.verdict = Verdict::Cancelled;
                return Ok(result);
            }
            if result.work >= maximum_work {
                result.verdict = Verdict::Budget;
                return Ok(result);
            }
            result.work += 1;
            let query = record.query(check, pose, forbidden)?;
            if result.work >= maximum_work {
                result.verdict = Verdict::Budget;
                return Ok(result);
            }
            let next = world.query(
                query.geometry,
                query.mask,
                tolerance,
                maximum_work - result.work,
                &cancelled,
            )?;
            result.work += next.work;
            result.candidates += next.candidates;
            match next.verdict {
                Verdict::Blocked(_) | Verdict::Budget | Verdict::Cancelled => {
                    result.verdict = next.verdict;
                    return Ok(result);
                }
                Verdict::Clear => {}
                other @ Verdict::Unresolved(_) => {
                    if !matches!(result.verdict, Verdict::Unresolved(_)) {
                        result.verdict = other;
                    }
                }
                other => {
                    if result.verdict == Verdict::Clear {
                        result.verdict = other;
                    }
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::environment::Coverage;
    fn attributes() -> Attributes {
        serde_json::from_value(serde_json::json!({"version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],"environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},"volumes":[{"object_id":10,"hierarchy_active":false,"component_enabled":false,"type_mask":2,"center":[0,0,0],"size":[2,2,2],"local_to_root":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"collider_types":[],"collider_ids":[],"rotation_chain":[],"field_names":["Center","Size","Type"]}],"checks":[],"issues":[]}})).unwrap()
    }
    fn pose() -> Transform {
        Transform {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    #[test]
    fn registration_retains_volumes_when_only_query_layout_is_unknown() {
        let mut data = attributes();
        data.environment
            .as_mut()
            .unwrap()
            .issues
            .push("Unknown environment check layout".into());
        assert!(Prepared::new(&data, || false).is_err());
        let prepared = Prepared::for_registration(&data, || false)
            .unwrap()
            .unwrap();
        let targets = prepared
            .targets(7, pose(), |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].key.component, 10);
        assert!(prepared.query_boxes(pose(), 17, || false).is_err());
        let world = World::build(vec![], Coverage::Complete, 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            prepared
                .assess(&world, pose(), 17, 0., 1000, || false)
                .unwrap()
                .verdict,
            Verdict::Incomplete
        );
        data.environment
            .as_mut()
            .unwrap()
            .issues
            .push("Missing environment hierarchy transform".into());
        assert!(Prepared::for_registration(&data, || false).is_err());
    }
    #[test]
    fn inactive_queries_use_external_masks_and_registration_is_explicit() {
        let data = attributes();
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        assert_eq!(prepared.counts(), [0, 1]);
        let boxes = prepared.query_boxes(pose(), 17, || false).unwrap().unwrap();
        assert_eq!(boxes[0].mask, 17);
        let targets = prepared
            .targets(1, pose(), |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        let world = World::build(targets, Coverage::Complete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            prepared
                .assess(&world, pose(), 2, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Blocked(_)
        ));
        assert_eq!(
            prepared
                .assess(&world, pose(), 17, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Clear
        );
        let pending = prepared
            .targets(2, pose(), |_| Registration::Unknown, || false)
            .unwrap()
            .unwrap();
        let world = World::build(pending, Coverage::Complete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            prepared
                .assess(&world, pose(), 2, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Unresolved(_)
        ));
    }
    #[test]
    fn captured_convex_buffers_use_instance_geometry_and_explicit_registration() {
        let mut data = attributes();
        let env = data.environment.as_mut().unwrap();
        env.volumes[0].collider_types = vec![64];
        env.volumes[0].collider_ids = vec![11];
        env.volumes[0].local_to_root[0][3] = 3.;
        env.volumes[0].center = [100.; 3];
        let vertices = [-1., 1.]
            .into_iter()
            .flat_map(|x| {
                [-1., 1.]
                    .into_iter()
                    .flat_map(move |y| [-1., 1.].map(move |z| [x, y, z]))
            })
            .collect();
        env.collider_meshes = Some(vec![crate::monument_attributes::EnvironmentColliderMesh {
            collider_id: 11,
            mesh_asset: "CAB-owned".into(),
            mesh_id: 77,
            enabled: false,
            is_trigger: true,
            convex: true,
            cooking_options: 30,
            vertices,
            triangles: vec![[0, 1, 2]],
        }]);
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let pose = Transform {
            position: [10., 2., 30.],
            rotation: [0., 0.6, 0., 0.8],
            scale: [1.; 3],
        };
        let center = environment::rotate(pose.rotation, [3., 0., 0.]);
        let center = std::array::from_fn(|i| center[i] + pose.position[i]);
        let query = Box3 {
            center,
            rotation: [0., 0., 0., 1.],
            extents: [0.01; 3],
        };
        let targets = prepared
            .targets(1, pose, |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        assert!(matches!(targets[0].shape, Shape::Convex(_)));
        let world = World::build(targets, Coverage::Complete, 1000, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            world.query(query, 2, 0., 10000, || false).unwrap().verdict,
            Verdict::Blocked(_)
        ));
        let targets = prepared
            .targets(1, pose, |_| Registration::Unknown, || false)
            .unwrap()
            .unwrap();
        assert!(matches!(
            targets[0].shape,
            Shape::Unresolved { bounds: Some(_) }
        ));
        let world = World::build(targets, Coverage::Complete, 1000, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            world.query(query, 2, 0., 10000, || false).unwrap().verdict,
            Verdict::Unresolved(_)
        ));
        assert!(
            prepared
                .targets(1, pose, |_| Registration::Disabled, || false)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        data.environment
            .as_mut()
            .unwrap()
            .collider_meshes
            .as_mut()
            .unwrap()[0]
            .convex = false;
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let targets = prepared
            .targets(1, pose, |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        assert!(matches!(
            targets[0].shape,
            Shape::Unresolved { bounds: None }
        ));
    }
    #[test]
    fn unknown_geometry_and_shared_trigger_writers_never_reuse_a_narrow_bound() {
        let mut data = attributes();
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let targets = prepared
            .targets(1, pose(), |_| Registration::UnknownGeometry, || false)
            .unwrap()
            .unwrap();
        assert!(matches!(
            targets[0].shape,
            Shape::Unresolved { bounds: None }
        ));
        let env = data.environment.as_mut().unwrap();
        env.volumes[0].game_object = Some(crate::monument_attributes::EnvironmentObject {
            object_id: 90,
            layer: 18,
            volume_components: vec![10, 20],
            collider_enabled: vec![],
        });
        let mut second = env.volumes[0].clone();
        second.object_id = 20;
        env.volumes.push(second);
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let targets = prepared
            .targets(1, pose(), |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        assert!(
            targets
                .iter()
                .all(|v| matches!(v.shape, Shape::Unresolved { bounds: None }))
        );
        let world = World::build(targets, Coverage::Complete, 1000, || false)
            .unwrap()
            .ok()
            .unwrap();
        let far = Box3 {
            center: [10000.; 3],
            rotation: [0., 0., 0., 1.],
            extents: [1.; 3],
        };
        assert!(matches!(
            world.query(far, 2, 0., 1000, || false).unwrap().verdict,
            Verdict::Unresolved(_)
        ));
        data.environment
            .as_mut()
            .unwrap()
            .issues
            .push("Incomplete environment hierarchy".into());
        assert!(Prepared::new(&data, || false).is_err());
    }
    #[test]
    fn meshes_and_legacy_captures_cannot_become_guessed_boxes() {
        let mut data = attributes();
        let env = data.environment.as_mut().unwrap();
        env.volumes[0].collider_types = vec![64];
        env.volumes[0].collider_ids = vec![11];
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let targets = prepared
            .targets(1, pose(), |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        assert!(matches!(
            targets[0].shape,
            Shape::Unresolved { bounds: None }
        ));
        data.version = 2;
        data.environment = None;
        data.validate().unwrap();
        assert!(Prepared::new(&data, || false).is_err());
        assert!(Prepared::new(&data, || true).unwrap().is_none());
    }
    #[test]
    fn missing_authored_root_position_is_unknown_and_not_assumed_zero() {
        let mut data = attributes();
        data.environment
            .as_mut()
            .unwrap()
            .root_transform
            .as_mut()
            .unwrap()
            .position = None;
        data.validate().unwrap();
        assert!(Prepared::new(&data, || false).is_err());
    }
    #[test]
    fn authored_asset_frame_affects_queries_but_not_instance_colliders() {
        let mut data = attributes();
        let env = data.environment.as_mut().unwrap();
        env.root_transform.as_mut().unwrap().position = Some([10., 0., 0.]);
        env.volumes[0].local_to_root[0][3] = 3.;
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let query = prepared.query_boxes(pose(), 17, || false).unwrap().unwrap();
        assert_eq!(query[0].geometry.center, [13., 0., 0.]);
        let targets = prepared
            .targets(1, pose(), |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        let Shape::Box(geometry) = targets[0].shape else {
            panic!("Expected box");
        };
        assert_eq!(geometry.center, [3., 0., 0.]);
    }
    #[test]
    fn reflected_or_sheared_volume_frames_are_reported_instead_of_flattened() {
        let mut data = attributes();
        data.environment.as_mut().unwrap().volumes[0].local_to_root[0][0] = -1.;
        assert!(Prepared::new(&data, || false).is_err());
        data.environment.as_mut().unwrap().volumes[0].local_to_root[0][0] = 1.;
        data.environment.as_mut().unwrap().volumes[0].local_to_root[0][1] = 0.2;
        assert!(Prepared::new(&data, || false).is_err());
    }
    #[test]
    fn cancellation_and_work_limits_do_not_clear_empty_or_partial_evaluations() {
        let mut data = attributes();
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let world = World::build(vec![], Coverage::Incomplete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert_eq!(
            prepared
                .assess(&world, pose(), 17, 0., 1, || false)
                .unwrap()
                .verdict,
            Verdict::Budget
        );
        data.environment.as_mut().unwrap().volumes.clear();
        let empty = Prepared::new(&data, || false).unwrap().unwrap();
        assert!(empty.query_boxes(pose(), 17, || true).unwrap().is_none());
        assert!(
            empty
                .targets(1, pose(), |_| Registration::Enabled, || true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            empty
                .assess(&world, pose(), 17, 0., 100, || true)
                .unwrap()
                .verdict,
            Verdict::Cancelled
        );
        assert!(
            empty
                .assess(&world, pose(), 17, f64::NAN, 100, || false)
                .is_err()
        );
    }
    #[test]
    fn registered_colliders_do_not_reuse_the_sources_postmultiplied_query_rotation() {
        let mut data = attributes();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let record = &mut data.environment.as_mut().unwrap().volumes[0];
        record.rotation_chain = vec![[q, 0., 0., q]];
        record.local_to_root = [
            [1., 0., 0., 0.],
            [0., 0., -1., 0.],
            [0., 1., 0., 0.],
            [0., 0., 0., 1.],
        ];
        record.size = [2., 4., 8.];
        let prepared = Prepared::new(&data, || false).unwrap().unwrap();
        let pose = Transform {
            rotation: [0., q, 0., q],
            ..pose()
        };
        let query = prepared.query_boxes(pose, 17, || false).unwrap().unwrap()[0].geometry;
        let target = prepared
            .targets(1, pose, |_| Registration::Enabled, || false)
            .unwrap()
            .unwrap();
        let Shape::Box(collider) = target[0].shape else {
            panic!("Expected registered box");
        };
        assert_ne!(query.rotation, collider.rotation);
        assert_eq!(
            query.rotation,
            environment::multiply([q, 0., 0., q], pose.rotation)
        );
        assert_eq!(
            collider.rotation,
            environment::multiply(pose.rotation, [q, 0., 0., q])
        );
    }
}
