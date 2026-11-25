use super::*;
use crate::environment_runtime::Model;
use papyrus_terrain::environment::{Box3, Coverage};

fn candidate(data: &Attributes) -> Candidate {
    Candidate::prepare(
        &context().sources_sha256,
        "box.prefab",
        data,
        100000,
        || false,
    )
    .unwrap()
    .unwrap()
}
#[test]
fn candidate_checks_upstream_geometry_and_retains_unknown_activation() {
    for (lifecycle, unresolved) in [
        (Lifecycle::ActivatedAuthored, false),
        (Lifecycle::ActivationUnknown, true),
    ] {
        let first = req(20, &[actor(1, 0., lifecycle)]);
        let next = req(10, &[]);
        let mut scene = scene::Scene::new_ordered(
            context(),
            vec![first.requirement(vec![]), next.requirement(vec![])],
            vec![20, 10],
            Coverage::Complete,
        )
        .unwrap();
        let snapshot = scene.snapshot();
        let (update, _) = prepare(&snapshot, &first, &attributes(), 100000)
            .unwrap()
            .unwrap()
            .into_parts();
        scene.publish(update, || false).unwrap();
        let snapshot = scene.snapshot();
        let prefix = next
            .query_prefix(&snapshot, 100000, || false)
            .unwrap()
            .unwrap();
        let candidate = candidate(&attributes());
        let assess = |x, mask| {
            candidate
                .assess(&snapshot, &next, &prefix, pose(x), mask, 0., 100000, || {
                    false
                })
                .unwrap()
        };
        let hit = assess(0., 1);
        assert!(if unresolved {
            matches!(hit.verdict, Verdict::Unresolved(_))
        } else {
            matches!(hit.verdict, Verdict::Blocked(_))
        });
        assert_eq!(assess(50., 1).verdict, Verdict::Clear);
        assert_eq!(assess(0., 2).verdict, Verdict::Clear);
        assert!(!snapshot.summary().declared_coverage_complete);
        let exact = hit.work;
        assert_eq!(
            candidate
                .assess(&snapshot, &next, &prefix, pose(0.), 1, 0., exact, || false)
                .unwrap()
                .verdict,
            hit.verdict
        );
        assert_eq!(
            candidate
                .assess(
                    &snapshot,
                    &next,
                    &prefix,
                    pose(0.),
                    1,
                    0.,
                    exact - 1,
                    || false
                )
                .unwrap()
                .verdict,
            Verdict::Budget
        );
        let (update, _) = prepare(&snapshot, &first, &attributes(), 100000)
            .unwrap()
            .unwrap()
            .into_parts();
        scene.publish(update, || false).unwrap();
        assert!(
            candidate
                .assess(
                    &scene.snapshot(),
                    &next,
                    &prefix,
                    pose(0.),
                    1,
                    0.,
                    100000,
                    || false
                )
                .is_err()
        );
    }
}
#[test]
fn empty_candidate_does_not_clear_missing_prefix_and_checks_source_binding() {
    let mut empty = attributes();
    empty.environment.as_mut().unwrap().volumes.clear();
    let candidate = candidate(&empty);
    let request = req(10, &[]);
    for coverage in [Coverage::Complete, Coverage::Incomplete] {
        let scene = scene::Scene::new_ordered(
            context(),
            vec![request.requirement(vec![])],
            vec![10],
            coverage,
        )
        .unwrap();
        let snapshot = scene.snapshot();
        let prefix = request
            .query_prefix(&snapshot, 100000, || false)
            .unwrap()
            .unwrap();
        let result = candidate
            .assess(
                &snapshot,
                &request,
                &prefix,
                pose(0.),
                1,
                0.,
                100000,
                || false,
            )
            .unwrap();
        assert_eq!(
            result.verdict,
            if coverage == Coverage::Complete {
                Verdict::Clear
            } else {
                Verdict::Incomplete
            }
        );
        assert_eq!(
            candidate
                .assess(
                    &snapshot,
                    &request,
                    &prefix,
                    pose(0.),
                    1,
                    0.,
                    100000,
                    || true
                )
                .unwrap()
                .verdict,
            Verdict::Cancelled
        );
        assert!(
            candidate
                .assess(
                    &snapshot,
                    &request,
                    &prefix,
                    pose(0.),
                    1,
                    f64::NAN,
                    100000,
                    || false
                )
                .is_err()
        );
        assert!(
            candidate
                .assess(&snapshot, &request, &prefix, pose(0.), 1, 0., 0, || false)
                .is_err()
        );
        let other = Candidate::prepare(&"c".repeat(64), "box.prefab", &empty, 100000, || false)
            .unwrap()
            .unwrap();
        assert!(
            other
                .assess(
                    &snapshot,
                    &request,
                    &prefix,
                    pose(0.),
                    1,
                    0.,
                    100000,
                    || false
                )
                .is_err()
        );
    }
}
#[test]
fn candidate_preparation_is_bounded_cancellable_and_reusable() {
    let data = attributes();
    let prepared = candidate(&data);
    assert_eq!(prepared.prefab(), "box.prefab");
    let exact = prepared.preparation_work_reserved();
    assert!(
        Candidate::prepare(
            &context().sources_sha256,
            "box.prefab",
            &data,
            exact,
            || false
        )
        .unwrap()
        .is_ok()
    );
    assert!(matches!(
        Candidate::prepare(
            &context().sources_sha256,
            "box.prefab",
            &data,
            exact - 1,
            || false
        )
        .unwrap(),
        Err(Verdict::Budget)
    ));
    assert!(matches!(
        Candidate::prepare(
            &context().sources_sha256,
            "box.prefab",
            &data,
            exact,
            || true
        )
        .unwrap(),
        Err(Verdict::Cancelled)
    ));
    assert!(
        Candidate::prepare(&context().sources_sha256, "box.prefab", &data, 0, || false).is_err()
    );
}

#[test]
fn source_request_prefixes_bind_winning_outputs_and_reject_stale_or_foreign_bases() {
    let first = req(20, &[]);
    let next = req(10, &[]);
    let plan = || vec![first.requirement(vec![]), next.requirement(vec![])];
    let mut scene =
        scene::Scene::new_ordered(context(), plan(), vec![20, 10], Coverage::Complete).unwrap();
    let snapshot = scene.snapshot();
    let missing = next
        .query_prefix(&snapshot, 100000, || false)
        .unwrap()
        .unwrap();
    assert!(!validate_prefix(&snapshot, &next, &missing).unwrap());
    let prefix = first
        .query_prefix(&snapshot, 100000, || false)
        .unwrap()
        .unwrap();
    let winning = req(20, &[actor(2, 20., Lifecycle::ActivatedAuthored)]);
    assert_eq!(winning.inputs_sha256(), first.inputs_sha256());
    assert_ne!(winning.occurrences_sha256(), first.occurrences_sha256());
    assert!(validate_prefix(&snapshot, &winning, &prefix).unwrap());
    let (update, _) = prepare(&snapshot, &winning, &attributes(), 100000)
        .unwrap()
        .unwrap()
        .into_parts();
    scene.publish(update, || false).unwrap();
    let current = scene.snapshot();
    assert!(validate_prefix(&current, &winning, &prefix).is_err());
    let prefix = next
        .query_prefix(&current, 100000, || false)
        .unwrap()
        .unwrap();
    assert!(validate_prefix(&current, &next, &prefix).unwrap());
    assert!(!current.summary().declared_coverage_complete);
    assert!(matches!(query(&current, 0., 1), Verdict::Incomplete));
    assert_eq!(
        prefix
            .world()
            .query(
                Box3 {
                    center: [0.; 3],
                    rotation: [0., 0., 0., 1.],
                    extents: [0.1; 3]
                },
                1,
                0.,
                10000,
                || false
            )
            .unwrap()
            .verdict,
        Verdict::Clear
    );
    let foreign =
        scene::Scene::new_ordered(context(), plan(), vec![20, 10], Coverage::Complete).unwrap();
    assert!(validate_prefix(&foreign.snapshot(), &next, &prefix).is_err());
    assert!(validate_prefix(&current, &winning, &prefix).is_err());
}
#[test]
fn source_prefix_validation_preserves_inherited_unknown_coverage_even_for_empty_outputs() {
    let request = req(10, &[]);
    let scene = scene::Scene::new_ordered(
        context(),
        vec![request.requirement(vec![])],
        vec![10],
        Coverage::Incomplete,
    )
    .unwrap();
    let snapshot = scene.snapshot();
    let prefix = request
        .query_prefix(&snapshot, 10000, || false)
        .unwrap()
        .unwrap();
    assert!(prefix.summary().pending_stages.is_empty());
    assert!(!validate_prefix(&snapshot, &request, &prefix).unwrap());
    assert_eq!(scene.snapshot().revision(), 0);
}

fn context() -> scene::Context {
    scene::Context {
        request_sha256: "a".repeat(64),
        sources_sha256: "b".repeat(64),
    }
}
fn pose(x: f32) -> Transform {
    Transform {
        position: [x, 0., 0.],
        rotation: [0., 0., 0., 1.],
        scale: [1.; 3],
    }
}
fn actor(id: u64, x: f32, lifecycle: Lifecycle) -> Occurrence {
    Occurrence {
        id,
        prefab: "box.prefab".into(),
        pose: pose(x),
        lifecycle,
    }
}
fn attributes() -> Attributes {
    serde_json::from_value(serde_json::json!({
        "version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],
        "environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},"issues":[],"checks":[],"collider_meshes":[],
        "volumes":[{"object_id":50,"hierarchy_active":true,"component_enabled":true,"type_mask":1,"center":[0,0,0],"size":[2,2,2],"local_to_root":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"collider_types":[65],"collider_ids":[52],"rotation_chain":[],"field_names":["Center","Size","Type"],"game_object":{"object_id":90,"layer":18,"volume_components":[50],"collider_enabled":[false]}}]}
    })).unwrap()
}
fn req(stage: i64, actors: &[Occurrence]) -> Request {
    request(&context(), stage, actors, |p| p == "box.prefab").unwrap()
}
fn prepare(
    snapshot: &scene::Snapshot,
    request: &Request,
    data: &Attributes,
    maximum: usize,
) -> Result<Result<Update, Verdict>, String> {
    let model = Model::client_reference().unwrap();
    prepare_update(
        snapshot,
        request,
        maximum,
        |_| Some(data),
        |r, s| model.modeled_registration(r, s).registration,
        || false,
    )
}
fn query(snapshot: &scene::Snapshot, x: f32, mask: u32) -> Verdict {
    snapshot
        .world()
        .query(
            Box3 {
                center: [x, 0., 0.],
                rotation: [0., 0., 0., 1.],
                extents: [0.1; 3],
            },
            mask,
            0.,
            10000,
            || false,
        )
        .unwrap()
        .verdict
}
#[test]
fn source_lifecycles_map_to_occurrences_without_promoting_incomplete_coverage() {
    let actors = [
        actor(1, 0., Lifecycle::ActivatedAuthored),
        actor(2, 10., Lifecycle::SerializedOnly),
        actor(3, 20., Lifecycle::RootInactive),
        actor(4, 30., Lifecycle::ActivationUnknown),
        actor(5, 40., Lifecycle::Removed),
    ];
    let request = req(10, &actors);
    let mut scene = scene::Scene::new(
        context(),
        vec![request.requirement(vec![])],
        Coverage::Incomplete,
    )
    .unwrap();
    let old = scene.snapshot();
    let update = prepare(&old, &request, &attributes(), 100000)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            update.report.actors,
            update.report.volumes,
            update.report.unresolved_geometry
        ),
        (5, 2, 1)
    );
    let (update, _) = update.into_parts();
    scene.publish(update, || false).unwrap();
    let current = scene.snapshot();
    assert!(matches!(query(&current, 0., 1), Verdict::Blocked(_)));
    assert!(matches!(query(&current, 30., 1), Verdict::Unresolved(_)));
    for x in [10., 20., 40., 100.] {
        assert_eq!(query(&current, x, 1), Verdict::Incomplete);
    }
    assert_eq!(old.summary().revision, 0);
    assert_eq!(current.summary().instances, 5);
    assert!(!current.summary().declared_coverage_complete);
}
#[test]
fn replacing_upstream_occurrences_retires_dependents_and_preserves_old_frames() {
    let a = req(10, &[actor(1, 0., Lifecycle::ActivatedAuthored)]);
    let b = req(20, &[actor(2, 20., Lifecycle::ActivatedAuthored)]);
    let mut scene = scene::Scene::new(
        context(),
        vec![a.requirement(vec![]), b.requirement(vec![10])],
        Coverage::Complete,
    )
    .unwrap();
    assert!(prepare(&scene.snapshot(), &b, &attributes(), 100000).is_err());
    for request in [&a, &b] {
        let (u, _) = prepare(&scene.snapshot(), request, &attributes(), 100000)
            .unwrap()
            .unwrap()
            .into_parts();
        scene.publish(u, || false).unwrap();
    }
    let old = scene.snapshot();
    assert!(old.summary().declared_coverage_complete);
    let moved = req(10, &[actor(1, 100., Lifecycle::ActivatedAuthored)]);
    assert_eq!(a.inputs_sha256(), moved.inputs_sha256());
    assert_ne!(a.occurrences_sha256(), moved.occurrences_sha256());
    let (u, _) = prepare(&old, &moved, &attributes(), 100000)
        .unwrap()
        .unwrap()
        .into_parts();
    scene.publish(u, || false).unwrap();
    let new = scene.snapshot();
    assert_eq!(new.summary().pending_stages, vec![20]);
    assert!(matches!(query(&new, 100., 1), Verdict::Blocked(_)));
    assert_eq!(query(&new, 0., 1), Verdict::Incomplete);
    assert!(matches!(query(&old, 0., 1), Verdict::Blocked(_)));
    assert!(matches!(query(&old, 20., 1), Verdict::Blocked(_)));
}
#[test]
fn requests_bind_exact_occurrences_and_cancel_budget_or_staleness_publishes_nothing() {
    let actors = [
        actor(1, 0., Lifecycle::ActivatedAuthored),
        actor(2, 20., Lifecycle::ActivatedAuthored),
    ];
    let a = req(10, &actors);
    let reversed = req(10, &[actors[1].clone(), actors[0].clone()]);
    assert_eq!(a.occurrences_sha256(), reversed.occurrences_sha256());
    let mut modified = actors.clone();
    modified[0].lifecycle = Lifecycle::SerializedOnly;
    assert_ne!(
        a.occurrences_sha256(),
        req(10, &modified).occurrences_sha256()
    );
    let mut scene =
        scene::Scene::new(context(), vec![a.requirement(vec![])], Coverage::Incomplete).unwrap();
    let snapshot = scene.snapshot();
    let data = attributes();
    assert!(matches!(
        prepare(&snapshot, &a, &data, 1).unwrap(),
        Err(Verdict::Budget)
    ));
    let cancelled = prepare_update(
        &snapshot,
        &a,
        100000,
        |_| Some(&data),
        |_, _| Registration::Enabled,
        || true,
    )
    .unwrap();
    assert!(matches!(cancelled, Err(Verdict::Cancelled)));
    let (first, _) = prepare(&snapshot, &a, &data, 100000)
        .unwrap()
        .unwrap()
        .into_parts();
    let (stale, _) = prepare(&snapshot, &a, &data, 100000)
        .unwrap()
        .unwrap()
        .into_parts();
    scene.publish(first, || false).unwrap();
    let revision = scene.snapshot().revision();
    assert!(scene.publish(stale, || false).is_err());
    assert_eq!(scene.snapshot().revision(), revision);
    let mut changed_context = context();
    changed_context.request_sha256 = "c".repeat(64);
    let foreign = request(&changed_context, 10, &actors, |_| true).unwrap();
    assert!(prepare(&scene.snapshot(), &foreign, &data, 100000).is_err());
    assert!(
        request(
            &context(),
            10,
            &[actors[0].clone(), actors[0].clone()],
            |_| true
        )
        .is_err()
    );
    assert!(request(&context(), 10, &actors, |_| false).is_err());
}
#[test]
fn post_spawn_pose_is_used_once_and_unknown_frames_remain_unresolved() {
    let mut data = attributes();
    let root = data
        .environment
        .as_mut()
        .unwrap()
        .root_transform
        .as_mut()
        .unwrap();
    root.position = Some([30., 2., 10.]);
    root.rotation = [0., 0.6, 0., 0.8];
    root.scale = [3.; 3];
    data.environment.as_mut().unwrap().volumes[0].center = [1., 0., 0.];
    let mut a = actor(1, 10., Lifecycle::ActivatedAuthored);
    a.pose.rotation = [
        0.,
        std::f32::consts::FRAC_1_SQRT_2,
        0.,
        std::f32::consts::FRAC_1_SQRT_2,
    ];
    a.pose.scale = [2., 1., 1.];
    let request = req(10, &[a]);
    let mut scene = scene::Scene::new(
        context(),
        vec![request.requirement(vec![])],
        Coverage::Complete,
    )
    .unwrap();
    let (u, _) = prepare(&scene.snapshot(), &request, &data, 100000)
        .unwrap()
        .unwrap()
        .into_parts();
    scene.publish(u, || false).unwrap();
    let q = Box3 {
        center: [10., 0., -2.],
        rotation: [0., 0., 0., 1.],
        extents: [0.1; 3],
    };
    assert!(matches!(
        scene
            .snapshot()
            .world()
            .query(q, 1, 0., 10000, || false)
            .unwrap()
            .verdict,
        Verdict::Blocked(_)
    ));
    let unknown = req(10, &[actor(1, 10., Lifecycle::Unmodeled)]);
    let (u, _) = prepare(&scene.snapshot(), &unknown, &data, 100000)
        .unwrap()
        .unwrap()
        .into_parts();
    scene.publish(u, || false).unwrap();
    assert!(matches!(
        query(&scene.snapshot(), 10000., 1),
        Verdict::Unresolved(_)
    ));
    assert!(matches!(
        query(&scene.snapshot(), 10000., 2),
        Verdict::Unresolved(_)
    ));
    // Arbitrary runtime changes can add a volume to an originally empty prefab.
    let no_metadata = req(10, &[actor(1, 10., Lifecycle::Unmodeled)]);
    let (u, _) = prepare_update(
        &scene.snapshot(),
        &no_metadata,
        100000,
        |_| None,
        |_, _| panic!("unmodeled actor used captured geometry"),
        || false,
    )
    .unwrap()
    .unwrap()
    .into_parts();
    scene.publish(u, || false).unwrap();
    assert!(matches!(
        query(&scene.snapshot(), 10000., 1 << 31),
        Verdict::Unresolved(_)
    ));
}
#[test]
fn missing_active_metadata_cannot_publish_a_partial_provider() {
    let data = attributes();
    let actors = [
        actor(1, 0., Lifecycle::ActivatedAuthored),
        Occurrence {
            prefab: "missing.prefab".into(),
            ..actor(2, 10., Lifecycle::ActivatedAuthored)
        },
    ];
    let request = request(&context(), 10, &actors, |_| true).unwrap();
    let scene = scene::Scene::new(
        context(),
        vec![request.requirement(vec![])],
        Coverage::Complete,
    )
    .unwrap();
    assert!(
        prepare_update(
            &scene.snapshot(),
            &request,
            100000,
            |p| if p == "box.prefab" { Some(&data) } else { None },
            |_, _| Registration::Enabled,
            || false
        )
        .is_err()
    );
    assert_eq!(scene.snapshot().revision(), 0);
    assert_eq!(scene.snapshot().summary().instances, 0);
    let inactive = req(10, &[actor(1, 0., Lifecycle::SerializedOnly)]);
    assert!(
        prepare_update(
            &scene.snapshot(),
            &inactive,
            100000,
            |_| None,
            |_, _| panic!("inactive actor registered"),
            || false
        )
        .unwrap()
        .is_ok()
    );
}

#[test]
fn source_folder_binding_compares_resolved_paths_and_rejects_another_installation() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let verified = root.path().canonicalize().unwrap();
    assert!(source_root_matches(&child.join(".."), &verified).unwrap());
    let other = tempfile::tempdir().unwrap();
    assert!(!source_root_matches(other.path(), &verified).unwrap());
    assert!(source_root_matches(&root.path().join("missing"), &verified).is_err());
}

#[test]
fn repeated_convex_instances_reserve_transforms_without_rebuilding_hulls() {
    let mut data = attributes();
    let env = data.environment.as_mut().unwrap();
    env.volumes[0].collider_types = vec![64];
    env.collider_meshes = Some(vec![crate::monument_attributes::EnvironmentColliderMesh {
        collider_id: 52,
        mesh_asset: "CAB-owned".into(),
        mesh_id: 77,
        enabled: false,
        is_trigger: true,
        convex: true,
        cooking_options: 30,
        vertices: [-1., 1.]
            .into_iter()
            .flat_map(|x| {
                [-1., 1.]
                    .into_iter()
                    .flat_map(move |y| [-1., 1.].map(move |z| [x, y, z]))
            })
            .collect(),
        triangles: vec![[0, 1, 2]],
    }]);
    let actors = (1..=64)
        .map(|id| actor(id, id as f32 * 10., Lifecycle::ActivatedAuthored))
        .collect::<Vec<_>>();
    let request = req(10, &actors);
    let scene = scene::Scene::new(
        context(),
        vec![request.requirement(vec![])],
        Coverage::Complete,
    )
    .unwrap();
    let snapshot = scene.snapshot();
    let update = prepare(&snapshot, &request, &data, 4_000_000)
        .unwrap()
        .unwrap();
    assert_eq!(update.report.volumes, 64);
    assert_eq!(update.report.unresolved_geometry, 0);
    let exact = update.report.preparation_work_reserved + update.report.index_work;
    assert!(exact < 4_000_000);
    // The shared cap still covers both transformation and final index work.
    assert!(prepare(&snapshot, &request, &data, exact).unwrap().is_ok());
    assert!(matches!(
        prepare(&snapshot, &request, &data, exact - 1).unwrap(),
        Err(Verdict::Budget)
    ));
    // A repeated request gets the same reservation, regardless of prior work.
    let again = prepare(&snapshot, &request, &data, 4_000_000)
        .unwrap()
        .unwrap();
    assert_eq!(
        again.report.preparation_work_reserved,
        update.report.preparation_work_reserved
    );
    assert_eq!(again.report.index_work, update.report.index_work);
    assert_eq!(scene.snapshot().revision(), 0);
}

#[test]
fn cached_geometry_uses_shared_atomic_registry_and_exact_budget() {
    let data = attributes();
    let geometry = Prepared::new(&data, || false).unwrap().unwrap();
    let model = Model::client_reference().unwrap();
    let actors = [
        actor(1, 0., Lifecycle::ActivatedAuthored),
        actor(2, 10., Lifecycle::ActivationUnknown),
        actor(3, 20., Lifecycle::SerializedOnly),
        actor(4, 30., Lifecycle::Unmodeled),
    ];
    let first = req(25, &actors);
    let next = req(26, &[]);
    let mut scene = scene::Scene::new_ordered(
        context(),
        vec![first.requirement(vec![]), next.requirement(vec![25])],
        vec![25, 26],
        Coverage::Complete,
    )
    .unwrap();
    let snapshot = scene.snapshot();
    let prepare = |maximum, cancelled| {
        prepare_resolved(
            &snapshot,
            &first,
            maximum,
            |_| Some(Geometry::Cached(&geometry)),
            |r, s| model.modeled_registration(r, s).registration,
            move || cancelled,
        )
        .unwrap()
    };
    let update = prepare(100000, false).unwrap();
    assert_eq!(update.report.volumes, 3);
    assert_eq!(update.report.unresolved_geometry, 2);
    assert_eq!(
        update.report.preparation_work_reserved,
        actors.len() + 2 * geometry.target_work_reservation()
    );
    let exact = update.report.preparation_work_reserved + update.report.index_work;
    assert!(prepare(exact, false).is_ok());
    assert!(matches!(prepare(exact - 1, false), Err(Verdict::Budget)));
    assert!(matches!(prepare(exact, true), Err(Verdict::Cancelled)));
    assert_eq!(scene.snapshot().revision(), 0);
    let (update, _) = update.into_parts();
    scene.publish(update, || false).unwrap();
    let snapshot = scene.snapshot();
    let prefix = next
        .query_prefix(&snapshot, 100000, || false)
        .unwrap()
        .unwrap();
    assert!(prefix.summary().declared_prefix_complete);
    assert!(matches!(query(&snapshot, 0., 1), Verdict::Blocked(_)));
    assert!(matches!(query(&snapshot, 10., 1), Verdict::Unresolved(_)));
    // The unmodeled occurrence conservatively covers uncaptured geometry.
    assert!(matches!(query(&snapshot, 20., 1), Verdict::Unresolved(_)));
    assert_eq!(scene.snapshot().summary().instances, 4);
}
#[test]
fn source_spawn_order_survives_canonical_environment_receipts() {
    let actors = [
        actor(2, 0., Lifecycle::ActivatedAuthored),
        actor(1, 10., Lifecycle::ActivatedAuthored),
    ];
    let first = req(25, &actors);
    let reversed = req(25, &[actors[1].clone(), actors[0].clone()]);
    assert_eq!(first.occurrences_sha256, reversed.occurrences_sha256);
    assert_eq!(
        first.actors.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(first.publication_order, vec![2, 1]);
    assert_eq!(reversed.publication_order, vec![1, 2]);
}
