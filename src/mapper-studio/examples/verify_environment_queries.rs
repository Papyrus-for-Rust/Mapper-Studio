//! Replay a frozen, previously verified monument snapshot. This does not load
//! a game, reverify installed assets, register real colliders or run placement.
//! SNAPSHOT NEW_REPORT NEW_OWNED_ORACLE_INPUT
use mapper_studio::{
    environment_checks::{Prepared, Registration},
    monument_attributes::Attributes,
};
use papyrus_terrain::environment::{self, Coverage, Shape, Transform, Verdict, World};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn write(path: &Path, value: &Value) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn replay_stage_scene(
    instances: Vec<environment::scene::Instance>,
    sources_sha256: String,
) -> Result<Value, String> {
    use environment::scene::{Context, Replacement, Requirement, Scene};
    // Synthetic grouping only. It is not the installed procedural stage order.
    let mut scene = Scene::new(
        Context {
            request_sha256: "e".repeat(64),
            sources_sha256,
        },
        vec![
            Requirement {
                stage: 1,
                inputs_sha256: "1".repeat(64),
                dependencies: vec![],
            },
            Requirement {
                stage: 2,
                inputs_sha256: "2".repeat(64),
                dependencies: vec![1],
            },
        ],
        Coverage::Incomplete,
    )?;
    let actors = instances.len();
    let mut first = vec![];
    let mut second = vec![];
    for (i, instance) in instances.into_iter().enumerate() {
        if i % 2 == 0 {
            first.push(instance)
        } else {
            second.push(instance)
        }
    }
    let first = Replacement {
        stage: 1,
        inputs_sha256: "1".repeat(64),
        instances: first,
    };
    let second = Replacement {
        stage: 2,
        inputs_sha256: "2".repeat(64),
        instances: second,
    };
    let initial = scene.snapshot();
    let prepare = |base: &environment::scene::Snapshot,
                   changes: &[Replacement]|
     -> Result<environment::scene::PreparedUpdate, String> {
        base.prepare(base.context(), changes, 8_000_000, || false)?
            .map_err(|v| format!("Synthetic stage preparation: {v:?}"))
    };
    let stale = prepare(&initial, std::slice::from_ref(&first))?;
    let update = prepare(&initial, std::slice::from_ref(&first))?;
    let first_work = update.work();
    if !scene.publish(update, || false)? {
        return Err("Unexpected cancellation".into());
    }
    if scene.publish(stale, || false).is_ok() {
        return Err("Stale stage publication was accepted".into());
    }
    let mid = scene.snapshot();
    let revision = mid.revision();
    if scene.publish(prepare(&mid, std::slice::from_ref(&second))?, || true)?
        || scene.snapshot().revision() != revision
    {
        return Err("Cancelled stage changed the accepted scene".into());
    }
    let update = prepare(&mid, std::slice::from_ref(&second))?;
    let second_work = update.work();
    scene.publish(update, || false)?;
    let accepted = scene.snapshot();
    let completed = accepted.summary();
    if completed.instances != actors
        || !completed.pending_stages.is_empty()
        || completed.declared_coverage_complete
    {
        return Err("Synthetic scene lost actors or invented upstream coverage".into());
    }
    let far = environment::Box3 {
        center: [900000.; 3],
        rotation: [0., 0., 0., 1.],
        extents: [1.; 3],
    };
    let clearance = accepted.world().query(far, u32::MAX, 0., 10000, || false)?;
    if clearance.verdict == Verdict::Clear {
        return Err("Incomplete synthetic scene claimed clearance".into());
    }
    let removal = Replacement {
        stage: 1,
        inputs_sha256: "1".repeat(64),
        instances: vec![],
    };
    scene.publish(prepare(&accepted, &[removal])?, || false)?;
    let retired = scene.snapshot().summary();
    if retired.instances != 0
        || retired.pending_stages != [2]
        || accepted.summary().instances != actors
        || initial.revision() != 0
    {
        return Err(
            "Stage retirement altered a retained snapshot or left stale dependent actors".into(),
        );
    }
    Ok(
        json!({"synthetic_grouping":true,"runtime_compatibility_certified":false,"first_work":first_work,"second_work":second_work,
        "initial":initial.summary(),"completed":completed,"after_upstream_replacement":retired,"far_clearance":clearance,
        "stale_update_rejected":true,"cancelled_publication_preserved_revision":true,"retained_snapshot_preserved":true}),
    )
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("SNAPSHOT NEW_REPORT NEW_OWNED_ORACLE_INPUT".into());
    }
    let report = Path::new(&args[1]);
    let oracle = Path::new(&args[2]);
    if report.exists() || oracle.exists() {
        return Err("Outputs must be new".into());
    }
    let mut raw = Vec::new();
    fs::File::open(&args[0])
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 32 * 1024 * 1024 {
        return Err("Snapshot exceeds allowance".into());
    }
    let snapshot: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if snapshot["format"] != "papyrus-monument-source" || snapshot["schema"] != 1 {
        return Err("Unsupported frozen snapshot".into());
    }
    let prefabs = snapshot["prefabs"]
        .as_object()
        .filter(|p| !p.is_empty() && p.len() <= 512)
        .ok_or("Invalid snapshot roster")?;
    let mut names = prefabs.keys().collect::<Vec<_>>();
    names.sort_by_key(|name| (!name.contains("/offshore/"), *name));
    let pose = Transform {
        position: [100., 7., 200.],
        rotation: [0., 0.6, 0., 0.8],
        scale: [1.; 3],
    };
    let mut entries = Vec::new();
    let mut inputs = Vec::new();
    let mut expected = Vec::new();
    let mut hull_checks = Vec::new();
    let runtime_model = mapper_studio::environment_runtime::Model::reference()?;
    let mut scene_instances = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        let attributes: Attributes =
            serde_json::from_value(prefabs[name]["placement_attributes"].clone())
                .map_err(|e| e.to_string())?;
        attributes.validate()?;
        let prepared = match Prepared::new(&attributes, || false) {
            Ok(Some(p)) => p,
            Ok(None) => return Err("Unexpected cancellation".into()),
            Err(error) => {
                entries.push(json!({"prefab":name,"unsupported":error}));
                continue;
            }
        };
        let queries = prepared
            .query_boxes(pose, u32::MAX, || false)?
            .ok_or("Cancelled")?;
        let targets = prepared
            .targets(index as u64 + 1, pose, |_| Registration::Unknown, || false)?
            .ok_or("Cancelled")?;
        let global_unknown = targets
            .iter()
            .filter(|v| matches!(v.shape, Shape::Unresolved { bounds: None }))
            .count();
        let bounded_unknown = targets
            .iter()
            .filter(|v| matches!(v.shape, Shape::Unresolved { bounds: Some(_) }))
            .count();
        let world = World::build(targets, Coverage::Incomplete, 4_000_000, || false)?
            .map_err(|v| format!("Index: {v:?}"))?;
        let result = prepared.assess(&world, pose, u32::MAX, 0., 4_000_000, || false)?;
        if matches!(
            result.verdict,
            Verdict::Blocked(_) | Verdict::Budget | Verdict::Cancelled
        ) {
            return Err(
                "Frozen replay unexpectedly claims registered geometry or exhausts its budget"
                    .into(),
            );
        }
        let source = attributes.environment.as_ref().unwrap();
        let modeled = prepared
            .targets(
                index as u64 + 1,
                pose,
                |record| {
                    runtime_model
                        .modeled_registration(
                            record,
                            mapper_studio::environment_runtime::Lifecycle::ActivatedAuthored,
                        )
                        .registration
                },
                || false,
            )?
            .ok_or("Cancelled")?;
        scene_instances.push(environment::scene::Instance {
            id: index as u64 + 1,
            volumes: modeled,
        });
        let mut registration_reasons = std::collections::BTreeMap::<String, usize>::new();
        for record in &source.volumes {
            let decision = runtime_model.modeled_registration(
                record,
                mapper_studio::environment_runtime::Lifecycle::ActivatedAuthored,
            );
            *registration_reasons
                .entry(format!("{:?}", decision.reason))
                .or_default() += 1;
        }
        if source
            .collider_meshes
            .as_ref()
            .is_some_and(|meshes| !meshes.is_empty())
        {
            // Synthetic registration, isolated coverage and raw hull model only.
            // This does not establish the generated world's physics coverage.
            let targets = prepared
                .targets(index as u64 + 1, pose, |_| Registration::Enabled, || false)?
                .ok_or("Cancelled")?;
            for mesh in source.collider_meshes.iter().flatten().filter(|m| m.convex) {
                let record = source
                    .volumes
                    .iter()
                    .find(|v| v.collider_ids.first() == Some(&mesh.collider_id))
                    .ok_or("Missing first-collider environment association")?;
                let target = targets
                    .iter()
                    .find(|t| t.key.component == record.object_id)
                    .ok_or("Missing modeled target")?;
                let Shape::Convex(hull) = &target.shape else {
                    return Err("Captured convex mesh was not modeled".into());
                };
                let mean: [f32; 3] = std::array::from_fn(|i| {
                    mesh.vertices.iter().map(|p| f64::from(p[i])).sum::<f64>() as f32
                        / mesh.vertices.len() as f32
                });
                let local: [f32; 3] = std::array::from_fn(|i| {
                    record.local_to_root[i][3]
                        + (0..3)
                            .map(|j| record.local_to_root[i][j] * mean[j])
                            .sum::<f32>()
                });
                let offset = environment::rotate(
                    pose.rotation,
                    std::array::from_fn(|i| local[i] * pose.scale[i]),
                );
                let center = std::array::from_fn(|i| pose.position[i] + offset[i]);
                let query = environment::Box3 {
                    center,
                    rotation: [0., 0., 0., 1.],
                    extents: [0.01; 3],
                };
                let mut target = target.clone();
                target.mask = 1;
                let world = World::build(vec![target], Coverage::Complete, 1000, || false)?
                    .map_err(|v| format!("Synthetic index: {v:?}"))?;
                let inside = world.query(query, 1, 0., 4_000_000, || false)?;
                if !matches!(inside.verdict, Verdict::Blocked(_)) {
                    return Err(
                        "Convex model excluded a convex combination of its source vertices".into(),
                    );
                }
                let outside = environment::Box3 {
                    center: hull.bounds().maximum.map(|v| v as f32 + 10.),
                    ..query
                };
                let outside = world.query(outside, 1, 0., 4_000_000, || false)?;
                if outside.verdict != Verdict::Clear {
                    return Err("Convex model did not clear a distant isolated query".into());
                }
                hull_checks.push(json!({"prefab":name,"collider_id":mesh.collider_id,"hull_counts":hull.counts(),"inside":inside,"outside":outside}));
            }
        }
        for (check, records) in [(true, &source.checks), (false, &source.volumes)] {
            for (ordinal, record) in records.iter().enumerate() {
                if inputs.len() >= 128 || (!check && ordinal != 0 && !name.contains("/offshore/")) {
                    continue;
                }
                let query = queries
                    .iter()
                    .find(|q| q.component == record.object_id)
                    .ok_or("Missing query")?;
                let frame = query.source_transform;
                inputs.push(json!({"kind":if check {"check"}else{"volume"},"localPosition":frame.position,
                    "localRotation":frame.rotation,"localScale":frame.scale,"center":record.center,"size":record.size,
                    "position":pose.position,"rotation":pose.rotation,"scale":pose.scale}));
                expected.push(json!({"prefab":name,"component":record.object_id,
                    "geometry":query.geometry}));
            }
        }
        entries.push(json!({"prefab":name,"query_counts":prepared.counts(),"query_boxes":queries.len(),
            "unresolved_collider_shapes":global_unknown,"unresolved_registration_bounds":bounded_unknown,
            "raw_collision_meshes":source.collider_meshes.as_ref().map(|meshes|meshes.iter().map(|m|json!({
                "collider_id":m.collider_id,"mesh_asset":m.mesh_asset,"mesh_id":m.mesh_id,
                "vertices":m.vertices.len(),"triangles":m.triangles.len(),"convex":m.convex,
                "enabled":m.enabled,"is_trigger":m.is_trigger,"cooking_options":m.cooking_options
            })).collect::<Vec<_>>()),
            "assessment":result,"reference_lifecycle_model_reasons":registration_reasons}));
    }
    let sources_sha256=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"snapshot":format!("{:x}",Sha256::digest(&raw)),"runtime_model":runtime_model.fingerprint()?,"geometry":environment::VERSION})).map_err(|e|e.to_string())?));
    let scene_replay = replay_stage_scene(scene_instances, sources_sha256)?;
    write(oracle, &json!(inputs))?;
    write(
        report,
        &json!({"format":"papyrus-environment-query-replay-1","schema":1,
        "snapshot_sha256":format!("{:x}",Sha256::digest(raw)),"engine":environment::VERSION,
        "snapshot_sources_revalidated":false,"physics_executed":false,"offshore_generation_enabled":false,
        "reference_runtime_model":runtime_model.id(),"reference_runtime_code_verified":false,
        "reference_runtime_native_matches_snapshot":snapshot["sources"][0]==serde_json::to_value(runtime_model.native()).map_err(|e|e.to_string())?,
        "synthetic_pose":pose,"synthetic_forbidden_mask":u32::MAX,"entries":entries,"owned_oracle_expected":expected,
        "isolated_raw_convex_model_checks":hull_checks,"isolated_stage_scene_replay":scene_replay}),
    )?;
    println!(
        "Replayed {} frozen prefab programs; {} owned-oracle queries",
        entries.len(),
        inputs.len()
    );
    Ok(())
}
