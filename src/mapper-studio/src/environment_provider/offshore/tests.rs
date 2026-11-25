use super::*;
use crate::monument_attributes::{WaterBody, WaterPlacement};
use papyrus_terrain::environment::{Box3, Coverage, Key, Shape, Volume};
fn context() -> scene::Context {
    scene::Context {
        request_sha256: "a".repeat(64),
        sources_sha256: "b".repeat(64),
    }
}
fn request_for(stage: i64) -> Request {
    request(&context(), stage, &[], |_| true).unwrap()
}
fn attributes() -> Attributes {
    serde_json::from_value(serde_json::json!({"version":3,
        "root_decor":[{"object_id":1,"active":true,"kind":"DecorRotate","values":[0,-180,0,0,180,0]},
            {"object_id":2,"active":true,"kind":"DecorSwim","values":[]}],
        "terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],
        "environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},
            "volumes":[{"object_id":50,"hierarchy_active":true,"component_enabled":true,"type_mask":1,"center":[0,0,0],"size":[2,2,2],
                "local_to_root":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"collider_types":[],"collider_ids":[],"rotation_chain":[],"field_names":["Center","Size","Type"]}],
            "checks":[],"collider_meshes":[],"issues":[],"water_placement":{"version":1,"bodies":[],"issues":[]}}})).unwrap()
}
fn stage(data: &Attributes) -> Stage {
    let request = request_for(10);
    Stage {
        context: context(),
        stage: 10,
        inputs_sha256: request.inputs_sha256,
        stage_source_sha256: "c".repeat(64),
        size: 1000,
        seed: 7,
        policy: search::Policy {
            minimum_world_size: 0,
            target_count: 0,
            minimum_distance: 100.,
            maximum_distance: 150.,
            spacing: 0.,
            passes: 2,
            attempts_per_prefab: 4,
        },
        entries: vec![Entry {
            prefab: search::Prefab { id: 1, score: 16 },
            path: "rig.prefab".into(),
            decor: offshore_decor::Program::new(data).unwrap(),
            bounds: tiers::Bounds {
                center: [3., 0., 0.],
                extent: [5., 10., 5.],
            },
            tier_mask: 4,
            minimum_corners: 3,
            environment: Candidate::prepare(
                &context().sources_sha256,
                "rig.prefab",
                data,
                100000,
                || false,
            )
            .unwrap()
            .unwrap(),
            water: WaterCandidate {
                sources_sha256: context().sources_sha256,
                model: crate::placement_water::Model::client_reference().unwrap(),
                facts: data.environment.as_ref().unwrap().water_placement.clone(),
            },
        }],
        preparation_work: 1000,
    }
}
fn scene(coverage: Coverage, shape: Option<Shape>) -> scene::Scene {
    let upstream = request_for(7);
    let offshore = request_for(10);
    let mut scene = scene::Scene::new_ordered(
        context(),
        vec![upstream.requirement(vec![]), offshore.requirement(vec![])],
        vec![7, 10],
        coverage,
    )
    .unwrap();
    let instances = shape
        .map(|shape| {
            vec![scene::Instance {
                id: 1,
                volumes: vec![Volume {
                    key: Key {
                        instance: 1,
                        component: 42,
                    },
                    mask: 1,
                    shape,
                }],
            }]
        })
        .unwrap_or_default();
    let snapshot = scene.snapshot();
    let update = snapshot
        .prepare(
            &context(),
            &[scene::Replacement {
                stage: 7,
                inputs_sha256: upstream.inputs_sha256,
                instances,
            }],
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
    assert!(scene.publish(update, || false).unwrap());
    scene
}
fn large_box() -> Shape {
    Shape::Box(Box3 {
        center: [0.; 3],
        rotation: [0., 0., 0., 1.],
        extents: [10000.; 3],
    })
}
fn terrain(topology: &[i32]) -> Terrain<'_> {
    Terrain {
        rect: search::TerrainRect {
            size: 1000,
            minimum: [-500.; 2],
        },
        topology,
        resolution: 2,
        minimum_y: -500.,
    }
}
fn limits() -> Limits {
    Limits {
        prefix: 100000,
        search: 10000,
        checks: 100000,
    }
}
struct Sampler {
    lookup: LookupState,
    selection: Selection,
    height_calls: usize,
    water_calls: usize,
    body_calls: usize,
    body_height: Option<f32>,
    fail_water: bool,
}
impl Default for Sampler {
    fn default() -> Self {
        Self {
            lookup: LookupState::AuthoredUnchanged,
            selection: Selection::Unknown,
            height_calls: 0,
            water_calls: 0,
            body_calls: 0,
            body_height: None,
            fail_water: false,
        }
    }
}
impl Sampling for Sampler {
    fn height(&mut self, _: [f32; 2]) -> Result<f32, String> {
        self.height_calls += 1;
        Ok(-50.)
    }
    fn surface_water(&mut self, _: &str, _: [f32; 3]) -> Result<f32, String> {
        self.water_calls += 1;
        if self.fail_water {
            Err("water source unavailable".into())
        } else {
            Ok(0.)
        }
    }
    fn lookup_state(&mut self, _: &str) -> LookupState {
        self.lookup
    }
    fn selection(&mut self, _: &str) -> Selection {
        self.selection
    }
    fn body_y(&mut self, _: &str, _: &WaterBody) -> Result<Option<f32>, String> {
        self.body_calls += 1;
        Ok(self.body_height)
    }
}
#[test]
fn integrated_stage_replays_outside_terrain_poses_and_keeps_scene_unchanged() {
    let stage = stage(&attributes());
    let scene = scene(Coverage::Complete, None);
    let snapshot = scene.snapshot();
    let request = request_for(10);
    let first = stage
        .generate(
            &snapshot,
            &request,
            terrain(&[4; 4]),
            limits(),
            &mut Sampler::default(),
            || false,
        )
        .unwrap();
    let second = stage
        .generate(
            &snapshot,
            &request,
            terrain(&[4; 4]),
            limits(),
            &mut Sampler::default(),
            || false,
        )
        .unwrap();
    let search::Outcome::Found { placements, audit } = first.outcome else {
        panic!()
    };
    let search::Outcome::Found {
        placements: again, ..
    } = second.outcome
    else {
        panic!()
    };
    assert_eq!(placements, again);
    assert_eq!(placements.len(), 1);
    assert_eq!(stage.prefab_path(placements[0].id), Some("rig.prefab"));
    assert_eq!(placements[0].pose.position[1], 0.);
    assert!(
        placements[0].pose.position[0].abs() >= 600.
            || placements[0].pose.position[2].abs() >= 600.
    );
    assert_eq!(audit.completed_passes, 2);
    assert_eq!(scene.snapshot().revision(), snapshot.revision());
    assert_eq!(scene.snapshot().summary().pending_stages, vec![10]);
    let mut exact = limits();
    exact.checks = first.checks_work;
    assert!(matches!(
        stage
            .generate(
                &snapshot,
                &request,
                terrain(&[4; 4]),
                exact,
                &mut Sampler::default(),
                || false
            )
            .unwrap()
            .outcome,
        search::Outcome::Found { .. }
    ));
    exact.checks -= 1;
    assert!(matches!(
        stage.generate(
            &snapshot,
            &request,
            terrain(&[4; 4]),
            exact,
            &mut Sampler::default(),
            || false
        ),
        Err(Failure::Environment(Verdict::Budget))
    ));
}
#[test]
fn incomplete_prefix_stops_before_sampling_and_retries_exclude_old_self_geometry() {
    let stage = stage(&attributes());
    let incomplete = scene(Coverage::Incomplete, None);
    let mut sampler = Sampler::default();
    assert!(matches!(
        stage.generate(
            &incomplete.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || false
        ),
        Err(Failure::Environment(Verdict::Incomplete))
    ));
    assert_eq!(sampler.height_calls, 0);
    let mut scene = scene(Coverage::Complete, None);
    let snapshot = scene.snapshot();
    let request = request_for(10);
    let update = snapshot
        .prepare(
            &context(),
            &[scene::Replacement {
                stage: 10,
                inputs_sha256: request.inputs_sha256.clone(),
                instances: vec![scene::Instance {
                    id: 2,
                    volumes: vec![Volume {
                        key: Key {
                            instance: 2,
                            component: 42,
                        },
                        mask: 1,
                        shape: large_box(),
                    }],
                }],
            }],
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
    scene.publish(update, || false).unwrap();
    let snapshot = scene.snapshot();
    let result = stage
        .generate(
            &snapshot,
            &request,
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || false,
        )
        .unwrap();
    let search::Outcome::Found { placements, .. } = result.outcome else {
        panic!()
    };
    assert_eq!(placements.len(), 1);
    assert_eq!(scene.snapshot().revision(), snapshot.revision());
}
#[test]
fn blocked_geometry_rejects_sites_and_unresolved_geometry_stops_the_stage() {
    let stage = stage(&attributes());
    let blocked = scene(Coverage::Complete, Some(large_box()));
    let result = stage
        .generate(
            &blocked.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut Sampler::default(),
            || false,
        )
        .unwrap();
    let search::Outcome::Found { placements, .. } = result.outcome else {
        panic!()
    };
    assert!(placements.is_empty());
    assert_eq!(result.environment_rejections, 8);
    let unknown = scene(Coverage::Complete, Some(Shape::Unresolved { bounds: None }));
    assert!(matches!(
        stage.generate(
            &unknown.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut Sampler::default(),
            || false
        ),
        Err(Failure::Environment(Verdict::Unresolved(_)))
    ));
}
#[test]
fn tier_rejection_short_circuits_missing_water_but_unknown_water_cannot_finish_empty() {
    let stage = stage(&attributes());
    let scene = scene(Coverage::Complete, None);
    let mut sampler = Sampler {
        lookup: LookupState::Unmodeled,
        ..Default::default()
    };
    let rejected = stage
        .generate(
            &scene.snapshot(),
            &request_for(10),
            terrain(&[0; 4]),
            limits(),
            &mut sampler,
            || false,
        )
        .unwrap();
    assert_eq!(rejected.tier_rejections, 8);
    assert_eq!(sampler.body_calls, 0);
    assert!(matches!(
        stage.generate(
            &scene.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || false
        ),
        Err(Failure::Missing(_))
    ));
    let mut sampler = Sampler {
        fail_water: true,
        ..Default::default()
    };
    assert!(matches!(
        stage.generate(
            &scene.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || false
        ),
        Err(Failure::Sampling(_))
    ));
}
#[test]
fn lake_height_gate_and_bad_requests_propagate_through_the_stage() {
    let mut data = attributes();
    data.environment.as_mut().unwrap().water_placement = Some(WaterPlacement {
        version: 1,
        bodies: vec![WaterBody {
            object_id: 10,
            body_type: 4,
            component_enabled: false,
            hierarchy_active_without_root: true,
            position: [0.; 3],
            triggers: vec![],
        }],
        issues: vec![],
    });
    let stage = stage(&data);
    let scene = scene(Coverage::Complete, None);
    let mut sampler = Sampler {
        selection: Selection::Body(10),
        body_height: Some(-1.),
        ..Default::default()
    };
    let result = stage
        .generate(
            &scene.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || false,
        )
        .unwrap();
    assert_eq!(result.lake_rejections, 8);
    let mut different = context();
    different.request_sha256 = "f".repeat(64);
    let different = request(&different, 10, &[], |_| true).unwrap();
    assert!(matches!(
        stage.generate(
            &scene.snapshot(),
            &different,
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || false
        ),
        Err(Failure::Invalid(_))
    ));
    assert!(matches!(
        stage.generate(
            &scene.snapshot(),
            &request_for(10),
            terrain(&[4; 4]),
            limits(),
            &mut sampler,
            || true
        ),
        Err(Failure::Environment(Verdict::Cancelled))
    ));
}
