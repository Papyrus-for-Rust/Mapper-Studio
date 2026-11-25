//! Exercise normal tunnel preparation and retained reuse against an owned installation.
use mapper_studio::{Project, asset_discovery, installation, monument_source, tunnel_source};
use std::{io::Write, path::Path, sync::Arc, time::Instant};

fn connector_fixture(source: &tunnel_source::Source) -> Result<serde_json::Value, String> {
    use papyrus_terrain::dungeon_base::Pose;
    let half = 35_f32.to_radians() * 0.5;
    let pose = Pose {
        position: [125., -36., -75.],
        rotation: [0., half.sin(), 0., half.cos()],
    };
    let mut rows = Vec::new();
    let mut count = 0;
    for (index, path) in source.selectors["UpwardsFolder"].prefabs.iter().enumerate() {
        let kinds = source.prefabs[path]
            .connectors
            .iter()
            .map(|c| c.infrastructure_type)
            .collect::<std::collections::BTreeSet<_>>();
        let mut groups = Vec::new();
        for kind in kinds {
            let sockets = source.path_sockets(path, index as u64 + 1, pose, kind)?;
            count += sockets.len();
            groups.push(serde_json::json!({"kind":kind,"sockets":sockets}));
        }
        rows.push(serde_json::json!({"prefab":path,"instance":index+1,"groups":groups}));
    }
    Ok(
        serde_json::json!({"pose":{"position":pose.position,"rotation":pose.rotation},
        "prefabs":rows,"connectors":count,"synthetic_instances":true}),
    )
}

fn registration_fixture(
    source: &tunnel_source::Source,
    runtime: &mapper_studio::environment_runtime::Verified,
) -> Result<serde_json::Value, String> {
    use mapper_studio::environment_runtime::Lifecycle;
    use papyrus_terrain::environment::{Shape, Transform};
    // Synthetic occurrence lifecycles using the selected installation's verified
    // runtime. The normal generation stage still owns its world coverage.
    let half = 35_f32.to_radians() * 0.5;
    let pose = Transform {
        position: [125., -36., -75.],
        rotation: [0., half.sin(), 0., half.cos()],
        scale: [1.; 3],
    };
    let mut rows = Vec::new();
    let mut boxes = 0;
    let mut unresolved = 0;
    for (index, (path, prefab)) in source.prefabs.iter().enumerate() {
        let prepared = prefab
            .registration
            .as_ref()
            .ok_or("Missing tunnel registration facts")?;
        let targets = |lifecycle| {
            prepared.targets(
                index as u64 + 1,
                pose,
                |record| runtime.registration(record, lifecycle).registration,
                || false,
            )
        };
        for lifecycle in [
            Lifecycle::SerializedOnly,
            Lifecycle::RootInactive,
            Lifecycle::Removed,
        ] {
            if !targets(lifecycle)?
                .ok_or("Registration fixture cancelled")?
                .is_empty()
            {
                return Err("Inactive tunnel actor published environment volumes".into());
            }
        }
        let active =
            targets(Lifecycle::ActivatedAuthored)?.ok_or("Registration fixture cancelled")?;
        let mut records = Vec::new();
        for target in active {
            let geometry = match target.shape {
                Shape::Box(geometry) => {
                    boxes += 1;
                    serde_json::json!({"box":geometry})
                }
                Shape::Unresolved { bounds } => {
                    unresolved += 1;
                    serde_json::json!({"unresolved":bounds})
                }
                Shape::Convex(_) => return Err("Unexpected uncaptured tunnel collider mesh".into()),
            };
            records
                .push(serde_json::json!({"key":target.key,"mask":target.mask,"geometry":geometry}));
        }
        if prepared
            .targets(index as u64 + 1, pose, |_| unreachable!(), || true)?
            .is_some()
        {
            return Err("Cancelled registration fixture was accepted".into());
        }
        rows.push(serde_json::json!({"prefab":path,"instance":index+1,"targets":records}));
    }
    Ok(
        serde_json::json!({"runtime_profile":runtime.id(),"runtime_verified":true,"synthetic_lifecycle":true,
        "pose":pose,"prefabs":rows,"boxes":boxes,"unresolved":unresolved,
        "inactive_states_empty":true,"cancellation_rejected":true}),
    )
}

fn registry_fixture(
    provider: &mapper_studio::environment_provider::Source,
    source: &tunnel_source::Source,
) -> Result<serde_json::Value, String> {
    use mapper_studio::{environment_provider::Occurrence, environment_runtime::Lifecycle};
    use papyrus_terrain::environment::{Coverage, Transform, Verdict, scene};
    let half = 35_f32.to_radians() * 0.5;
    let actors = source
        .prefabs
        .keys()
        .enumerate()
        .map(|(index, path)| Occurrence {
            id: index as u64 + 1,
            prefab: path.clone(),
            pose: Transform {
                position: [125., -36., -75.],
                rotation: [0., half.sin(), 0., half.cos()],
                scale: [1.; 3],
            },
            lifecycle: Lifecycle::ActivatedAuthored,
        })
        .collect::<Vec<_>>();
    let context = provider.context("a".repeat(64))?;
    let first = provider.request(&context, 25, &actors)?;
    let next = provider.request(&context, 26, &[])?;
    let mut scene = scene::Scene::new_ordered(
        context,
        vec![first.requirement(vec![]), next.requirement(vec![25])],
        vec![25, 26],
        Coverage::Complete,
    )?;
    let snapshot = scene.snapshot();
    let prefix = first
        .query_prefix(&snapshot, 10_000_000, || false)?
        .map_err(|v| format!("Registry prefix unavailable: {v:?}"))?;
    let update = provider
        .prepare_update_from_prefix(&snapshot, &first, &prefix, 10_000_000, || false)?
        .map_err(|v| format!("Registry update unavailable: {v:?}"))?;
    let exact = update.report.preparation_work_reserved + update.report.index_work;
    if provider
        .prepare_update_from_prefix(&snapshot, &first, &prefix, exact, || false)?
        .is_err()
        || !matches!(
            provider.prepare_update_from_prefix(&snapshot, &first, &prefix, exact - 1, || false)?,
            Err(Verdict::Budget)
        )
        || !matches!(
            provider.prepare_update_from_prefix(&snapshot, &first, &prefix, exact, || true)?,
            Err(Verdict::Cancelled)
        )
        || scene.snapshot().revision() != 0
    {
        return Err("Registry budget/cancellation transaction failed".into());
    }
    let (update, report) = update.into_parts();
    scene.publish(update, || false)?;
    let snapshot = scene.snapshot();
    let next_prefix = next
        .query_prefix(&snapshot, 10_000_000, || false)?
        .map_err(|v| format!("Next registry prefix unavailable: {v:?}"))?;
    if !next_prefix.summary().declared_prefix_complete {
        return Err("Published tunnel registry did not satisfy its dependent prefix".into());
    }
    Ok(serde_json::json!({
        "synthetic_lifecycle":true, "synthetic_stage_roster":true,
        "update":report, "summary":snapshot.summary(),
        "dependent_prefix":next_prefix.summary(), "exact_budget_verified":true,
        "cancellation_atomic":true,
    }))
}

fn joint_publication_fixture(
    provider: &mapper_studio::environment_provider::Source,
    source: &tunnel_source::Source,
) -> Result<serde_json::Value, String> {
    use mapper_studio::{
        environment_provider::{Occurrence, TunnelLimits},
        environment_runtime::Lifecycle,
    };
    use papyrus_terrain::{
        dungeon_base::Pose,
        environment::{Coverage, Transform, scene},
        native_layers::{Layers, dimensions},
        prefab_terrain_effects::{self, Instance},
    };
    use sha2::{Digest, Sha256};
    let (hr, n) = dimensions(1000)?;
    let mut layers = Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 1,
        height_resolution: hr,
        surface_resolution: n,
        height: vec![16383; hr * hr],
        topology: vec![0; n * n],
        biome: vec![0; n * n * 5],
        splat: vec![0; n * n * 8],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let original = layers.clone();
    let half = 35_f32.to_radians() * 0.5;
    let mut actors = source
        .prefabs
        .iter()
        .filter(|(_, p)| p.effects.is_some())
        .enumerate()
        .map(|(i, (path, _))| Occurrence {
            id: i as u64 + 1,
            prefab: path.clone(),
            pose: Transform {
                position: [125., (i % 9) as f32 * 5. - 30., -75.],
                rotation: [0., half.sin(), 0., half.cos()],
                scale: [1.; 3],
            },
            lifecycle: Lifecycle::ActivatedAuthored,
        })
        .collect::<Vec<_>>();
    actors.reverse();
    let context = provider.context("f".repeat(64))?;
    let request = provider.request(&context, 25, &actors)?;
    let mut scene = scene::Scene::new_ordered(
        context,
        vec![request.requirement(vec![])],
        vec![25],
        Coverage::Complete,
    )?;
    let prefix = request
        .query_prefix(&scene.snapshot(), 10_000_000, || false)?
        .map_err(|v| format!("Missing joint publication prefix: {v:?}"))?;
    for limits in [
        TunnelLimits {
            environment_work: 1,
            terrain_work: 128_000_000,
        },
        TunnelLimits {
            environment_work: 10_000_000,
            terrain_work: 1,
        },
    ] {
        if provider
            .publish_tunnels(&mut scene, &request, &prefix, &mut layers, limits, || false)
            .is_ok()
            || scene.snapshot().revision() != 0
            || layers.height != original.height
            || layers.topology != original.topology
        {
            return Err("Failed tunnel publication changed the scene or terrain".into());
        }
    }
    let instances = actors
        .iter()
        .map(|a| Instance {
            prefab_id: source.prefabs[&a.prefab].id,
            pose: Pose {
                position: a.pose.position,
                rotation: a.pose.rotation,
            },
            effects: source.prefabs[&a.prefab].effects.as_ref().unwrap(),
        })
        .collect::<Vec<_>>();
    let mut expected = original.clone();
    let expected_report =
        prefab_terrain_effects::apply(&mut expected, &instances, 128_000_000, || false)
            .map_err(|e| format!("Reference tunnel effects failed: {e:?}"))?;
    let report = provider
        .publish_tunnels(
            &mut scene,
            &request,
            &prefix,
            &mut layers,
            TunnelLimits {
                environment_work: 10_000_000,
                terrain_work: 128_000_000,
            },
            || false,
        )
        .map_err(|e| format!("Joint tunnel publication failed: {e:?}"))?;
    if layers.height != expected.height
        || layers.topology != expected.topology
        || serde_json::to_value(&report.terrain).map_err(|e| e.to_string())?
            != serde_json::to_value(expected_report).map_err(|e| e.to_string())?
        || scene.snapshot().revision() != 1
        || scene.snapshot().summary().instances != actors.len()
    {
        return Err("Joint tunnel publication differs from source-ordered terrain effects".into());
    }
    if provider
        .publish_tunnels(
            &mut scene,
            &request,
            &prefix,
            &mut layers,
            TunnelLimits {
                environment_work: 10_000_000,
                terrain_work: 128_000_000,
            },
            || false,
        )
        .is_ok()
        || layers.height != expected.height
        || layers.topology != expected.topology
        || scene.snapshot().revision() != 1
    {
        return Err("Repeated tunnel publication reapplied terrain effects".into());
    }
    let mut height = Sha256::new();
    for value in &layers.height {
        height.update(value.to_le_bytes());
    }
    let mut topology = Sha256::new();
    for value in &layers.topology {
        topology.update(value.to_le_bytes());
    }
    Ok(
        serde_json::json!({"synthetic_lifecycle":true,"synthetic_stage_roster":true,
        "synthetic_terrain":true,"actors":actors.len(),"source_order_retained":true,
        "budget_failures_atomic":true,"duplicate_publication_rejected":true,
        "height_sha256":format!("{:x}",height.finalize()),"topology_sha256":format!("{:x}",topology.finalize()),
        "report":report,"summary":scene.snapshot().summary()}),
    )
}

fn portal_fixture(source: &tunnel_source::Source) -> Result<serde_json::Value, String> {
    use papyrus_terrain::{
        environment::{Box3, Coverage, Key, Shape, Volume, World},
        native_layers::{Layers, dimensions},
        tunnel_depth::Policy,
    };
    let (hr, n) = dimensions(1000)?;
    let mut biome = vec![0; n * n * 5];
    biome[..n * n].fill(255);
    let mut splat = vec![0; n * n * 8];
    splat[..n * n].fill(255);
    let mut layers = Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 1,
        height_resolution: hr,
        surface_resolution: n,
        height: vec![16383; hr * hr],
        biome,
        splat,
        topology: vec![0; n * n],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let mut blocked = vec![0; n * n];
    let empty =
        World::build(vec![], Coverage::Complete, 1000, || false)?.map_err(|v| format!("{v:?}"))?;
    let occupied = World::build(
        vec![Volume {
            key: Key {
                instance: 1,
                component: 1,
            },
            mask: 1,
            shape: Shape::Box(Box3 {
                center: [0.; 3],
                rotation: [0., 0., 0., 1.],
                extents: [1000.; 3],
            }),
        }],
        Coverage::Complete,
        1000,
        || false,
    )?
    .map_err(|v| format!("{v:?}"))?;
    let mut fixtures = Vec::new();
    for (name, encoded, placement, topology, world) in [
        ("flat_0", 16383_i16, 0, 0, &empty),
        ("flat_20", 17038, 0, 0, &empty),
        ("flat_80", 19004, 0, 0, &empty),
        ("placement_blocked", 17038, 1, 0, &empty),
        ("forbidden_topology", 17038, 0, i32::MAX, &empty),
        ("occupied_environment", 17038, 0, 0, &occupied),
    ] {
        layers.height.fill(encoded);
        layers.topology.fill(topology);
        blocked.fill(placement);
        let mut view = layers.view();
        view.placement_blockers = Some(&blocked);
        let checks = tunnel_source::PortalChecks::new(
            source,
            view,
            world,
            Policy {
                underground: 1,
                train_tunnels: 32,
                entrance: 4096,
                forbidden: 1,
                altitude: 0.,
                terrain_raise: 9.,
                collision_raise: source.fields.link_transition + 1.,
                step: 3.,
                maximum_drop: 1000.,
            },
        )?;
        let mut cases = Vec::new();
        for path in &source.selectors["UpwardsFolder"].prefabs {
            let result = checks
                .check(source.prefabs[path].id, [0., -36., 0.], 8_000_000, || false)
                .map_err(|error| format!("Portal fixture {path}: {error:?}"))?;
            cases.push(
                serde_json::json!({"path":path,"rejection":format!("{:?}", result.rejection),
                "work":result.work,"terrain_samples":result.terrain_samples}),
            );
        }
        fixtures.push(
            serde_json::json!({"name":name,"height":papyrus_terrain::erosion::decode(encoded),
            "placement_blocked":placement,"topology":topology,"cases":cases}),
        );
    }
    Ok(
        serde_json::json!({"input":"synthetic flat native-channel terrain and explicit empty/occupied world fixtures",
        "root":[0.,-36.,0.],"policy_origin":"inspected owned managed server reference",
        "normal_generation_integrated":false,"fixtures":fixtures}),
    )
}
fn terrain_effect_fixture(
    source: &tunnel_source::Source,
    directory: &Path,
) -> Result<serde_json::Value, String> {
    use papyrus_terrain::{
        dungeon_base::Pose,
        native_layers::{Layers, dimensions},
        prefab_terrain_effects::{self, Instance},
    };
    use sha2::{Digest, Sha256};
    let (hr, n) = dimensions(1000)?;
    let mut layers = Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 1,
        height_resolution: hr,
        surface_resolution: n,
        height: vec![14745; hr * hr],
        biome: vec![0; n * n * 5],
        splat: vec![0; n * n * 8],
        topology: vec![i32::MIN; n * n],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let paths: std::collections::BTreeSet<_> = source
        .selectors
        .values()
        .flat_map(|selector| selector.prefabs.iter())
        .collect();
    let instances: Vec<_> = paths
        .iter()
        .map(|path| {
            let prefab = &source.prefabs[*path];
            Ok(Instance {
                prefab_id: prefab.id,
                pose: Pose {
                    position: [0., -36., 0.],
                    rotation: [0., 0., 0., 1.],
                },
                effects: prefab
                    .effects
                    .as_ref()
                    .ok_or("Missing generated prefab effects")?,
            })
        })
        .collect::<Result<_, String>>()?;
    let report = prefab_terrain_effects::apply(&mut layers, &instances, 128_000_000, || false)
        .map_err(|e| format!("{e:?}"))?;
    let height: Vec<_> = layers.height.iter().flat_map(|v| v.to_le_bytes()).collect();
    let topology: Vec<_> = layers
        .topology
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let manifest = serde_json::json!({"format":"mapper-prefab-effects-fixture","schema":1,
        "capture_sha256":source.sha256,"size":1000,"height_resolution":hr,"surface_resolution":n,
        "initial_height":14745,"initial_topology":i32::MIN,"pose":{"position":[0.,-36.,0.],"rotation":[0.,0.,0.,1.]},
        "ordered_prefabs":paths,"report":report,
        "height_sha256":format!("{:x}",Sha256::digest(&height)),"topology_sha256":format!("{:x}",Sha256::digest(&topology)),
        "normal_generation_integrated":false,"game_runtime_validated":false});
    std::fs::create_dir(directory).map_err(|e| e.to_string())?;
    for (name, data) in [
        ("height.i16le", height),
        ("topology.i32le", topology),
        (
            "manifest.json",
            serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        ),
    ] {
        std::fs::File::create_new(directory.join(name))
            .and_then(|mut file| file.write_all(&data))
            .map_err(|e| e.to_string())?;
    }
    Ok(manifest)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err("Expected RUST_ROOT PROJECT NEW_REPORT [NEW_EFFECT_FIXTURE]".into());
    }
    let root = Path::new(&args[0]);
    let project = Project::load(Path::new(&args[1]))?;
    let _verification = installation::VerificationScope::default();
    let (catalog, bindings) = asset_discovery::prepare(
        root,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        project.monument_assets.as_ref(),
        || false,
    )?;
    let monuments = monument_source::Source::cached(
        project
            .monument_rules
            .as_ref()
            .ok_or("Missing monument rules")?,
        &catalog,
        &bindings,
        || false,
    )?;
    let start = Instant::now();
    let prepared = tunnel_source::Source::prepare(root, &catalog, &monuments, &|| false)?
        .ok_or("Tunnel adapter unavailable")?;
    let initial_seconds = start.elapsed().as_secs_f64();
    let start = Instant::now();
    let warm = tunnel_source::Source::prepare(root, &catalog, &monuments, &|| false)?
        .ok_or("Warm tunnel preparation cancelled")?;
    let warm_seconds = start.elapsed().as_secs_f64();
    if !Arc::ptr_eq(&prepared, &warm) {
        return Err("Warm tunnel preparation rebuilt the prepared model".into());
    }
    if tunnel_source::Source::prepare(root, &catalog, &monuments, &|| true).is_ok() {
        return Err("Cancelled tunnel preparation was accepted".into());
    }
    let upward = &prepared.selectors["UpwardsFolder"].prefabs;
    let surface_markers = upward
        .iter()
        .filter(|path| prepared.prefabs[*path].surface_offset.is_some())
        .count();
    let portal_fixture = portal_fixture(&prepared)?;
    let runtime =
        mapper_studio::environment_runtime::Verified::available(root, &catalog.installation, || {
            false
        })?
        .ok_or("Selected installation environment runtime is unavailable")?;
    let registration_fixture = registration_fixture(&prepared, &runtime)?;
    let rules = project
        .monument_rules
        .as_ref()
        .ok_or("Missing monument rules")?;
    let snapshot = mapper_studio::storage::local_data()?
        .join("papyrus-monument-cache")
        .join(&rules.source_sha256)
        .join("monuments.json");
    let provider = mapper_studio::environment_provider::Source::load(
        runtime,
        &catalog,
        &bindings,
        &snapshot,
        &rules.source_sha256,
        || false,
    )?
    .ok_or("Environment provider cancelled")?;
    let unbound_context = provider.context("a".repeat(64))?;
    let provider = provider
        .with_tunnels(prepared.clone(), || false)?
        .ok_or("Tunnel environment binding cancelled")?;
    if provider.request(&unbound_context, 25, &[]).is_ok() {
        return Err("Tunnel source binding accepted an earlier world context".into());
    }
    let registry_fixture = registry_fixture(&provider, &prepared)?;
    let joint_publication_fixture = joint_publication_fixture(&provider, &prepared)?;
    if registry_fixture["update"]["volumes"].as_u64()
        != Some(
            registration_fixture["boxes"]
                .as_u64()
                .ok_or("Missing box count")?
                + registration_fixture["unresolved"]
                    .as_u64()
                    .ok_or("Missing unresolved count")?,
        )
        || registry_fixture["summary"]["volumes"] != registry_fixture["update"]["volumes"]
        || provider.with_tunnels(prepared.clone(), || false).is_ok()
    {
        return Err("Tunnel registry differs from direct registration or accepts rebinding".into());
    }
    let connector_fixture = connector_fixture(&prepared)?;
    let terrain_effect_fixture = args
        .get(3)
        .map(|path| terrain_effect_fixture(&prepared, Path::new(path)))
        .transpose()?;
    let report = serde_json::json!({"format":"mapper-tunnel-preparation-verification","schema":1,
        "source":prepared.audit(),"initial_seconds":initial_seconds,"warm_seconds":warm_seconds,
        "upward_portal_definitions":upward.len(),"upward_surface_markers":surface_markers,
        "portal_fixture":portal_fixture,
        "registration_fixture":registration_fixture,
        "registry_fixture":registry_fixture,
        "joint_publication_fixture":joint_publication_fixture,
        "connector_fixture":connector_fixture,
        "terrain_effect_fixture":terrain_effect_fixture,
        "warm_reuses_same_allocation":true,"cancellation_rejected":true,"normal_generation_integrated":false});
    std::fs::File::create_new(Path::new(&args[2]))
        .and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(&report).unwrap()))
        .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
