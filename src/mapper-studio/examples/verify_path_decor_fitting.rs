//! Exercise captured roadside prefabs on a controlled flat terrain fixture.
use mapper_studio::decor_source::{prepare_path_fitting, prepare_path_prefab};
use papyrus_terrain::{
    decor, decor_fit, decor_live,
    decor_stream::Budget,
    environment::{Box3, Coverage, Key, Verdict},
    erosion, footprints, native_layers,
    path_geometry::Path,
    roadside_decor, roadside_live, spawn,
    water_treatment_clearance::Clearance,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=4).contains(&args.len())
        || args
            .get(3)
            .is_some_and(|arg| arg != "--clearance-fixture" && arg != "--terrain-fixture")
    {
        return Err(
            "SOURCE_REFERENCE EXPECTED_SHA256 NEW_OUTPUT_JSON [--clearance-fixture|--terrain-fixture]".into(),
        );
    }
    let with_clearance = args.get(3).is_some_and(|arg| arg == "--clearance-fixture");
    let with_terrain = args.get(3).is_some_and(|arg| arg == "--terrain-fixture");
    let output = PathBuf::from(&args[2]);
    if output.exists() {
        return Err("Existing fitting verification was retained".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(&args[0])?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Path fitting reference exceeds 16 MiB".into());
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if args[1].to_str() != Some(hash.as_str()) {
        return Err("Path fitting reference hash differs".into());
    }
    let reference: Value = serde_json::from_slice(&bytes)?;
    if reference["format"] != "mapper-water-treatment-reference" || reference["schema"] != 2 {
        return Err("Unsupported path fitting reference".into());
    }
    let programs = reference["stages"]
        .as_array()
        .ok_or("Missing path programs")?;
    if programs.is_empty() || programs.len() > 16 {
        return Err("Invalid path fitting program roster".into());
    }
    let (hr, n) = native_layers::dimensions(1000)?;
    let cells = n * n;
    let mut layers = native_layers::Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 2,
        height_resolution: hr,
        surface_resolution: n,
        height: vec![17038; hr * hr],
        biome: vec![51; cells * 5],
        splat: vec![0; cells * 8],
        topology: vec![1; cells],
        normalized_biome_cells: cells,
        normalized_splat_cells: cells,
    };
    layers.splat[..cells].fill(255);
    let alpha = vec![255; cells];
    let level = erosion::decode(layers.height[0]);
    let path = Path::from_controls(&[[0., level, -500.], [0., level, 500.]], 1000, || false)
        .map_err(|error| format!("Fixture path failed {error:?}"))?
        .ok_or("Fixture cancelled")?;
    let mut results = Vec::new();
    for (index, program) in programs.iter().enumerate() {
        let paths: Vec<String> = serde_json::from_value(program["prefabs"].clone())?;
        if paths.is_empty() || paths.len() > 256 {
            return Err("Invalid path fitting prefab roster".into());
        }
        let flags = BTreeMap::from([
            ("Field".into(), 1),
            ("Decor".into(), 512),
            ("Monument".into(), 1024),
        ]);
        let mut terrain_prefabs = BTreeMap::new();
        let mut runtime_holes = 0;
        let prefabs = paths
            .iter()
            .map(|path| {
                let raw = reference["prefabs"]
                    .get(path)
                    .ok_or("Missing captured path prefab")?;
                let bytes = serde_json::to_vec(raw)?;
                if with_terrain {
                    let prepared = prepare_path_prefab(&bytes, &flags, 1024)?;
                    terrain_prefabs.insert(path.clone(), prepared.terrain);
                    runtime_holes += prepared.runtime_holes.len();
                    Ok((path.clone(), prepared.fitting))
                } else {
                    prepare_path_fitting(&bytes)
                        .map(|fitting| (path.clone(), fitting))
                        .map_err(Into::into)
                }
            })
            .collect::<Result<BTreeMap<_, _>, Box<dyn std::error::Error>>>()?;
        // Stage supplies the shared fitting descriptor. The roadside sampler owns positions.
        let stage = decor::Stage {
            id: program["object_id"]
                .as_u64()
                .ok_or("Missing path program ID")?
                .to_string(),
            order: index,
            name: program["name"]
                .as_str()
                .ok_or("Missing program name")?
                .into(),
            kind: decor::Kind::Uniform,
            folder: program["folder"]
                .as_str()
                .ok_or("Missing path folder")?
                .into(),
            enabled: true,
            mode: -1,
            filter: serde_json::from_value(program["filter"].clone())?,
            filter_mode: 2,
            anchor_mode: serde_json::from_value(program["anchor_mode"].clone())?,
            distance: serde_json::from_value(program["distance"].clone())?,
            dithering: serde_json::from_value(program["dithering"].clone())?,
            density: 0.,
            cluster: None,
            prefabs: paths,
        };
        let sampling = roadside_decor::Settings {
            road_mode: serde_json::from_value(program["road_type"].clone())?,
            distance: stage.distance,
            spacing_fraction: 0.25,
            side_offset: serde_json::from_value(program["side_offset"].clone())?,
            dithering: stage.dithering,
            placement_attempts: serde_json::from_value(program["placement_attempts"].clone())?,
        };
        let mut fitting = decor_fit::OrientedBudget::new(&layers, &alpha)?;
        let mut transaction = if with_terrain {
            Some(
                footprints::RoadTransaction::new_objects(
                    &layers,
                    &alpha,
                    terrain_prefabs,
                    1537,
                    1024,
                    || false,
                )?
                .ok_or("Fixture terrain preparation cancelled")?,
            )
        } else {
            None
        };
        let mut next_pose = 0;
        let mut attempts = Budget::new(10000)?;
        let exclusion = Box3 {
            center: [0., level, -355.],
            rotation: [0., 0., 0., 1.],
            extents: [25., 10., 25.],
        };
        let clearance = with_clearance
            .then(|| {
                Clearance::new(
                    &[(
                        Key {
                            instance: 1,
                            component: 1,
                        },
                        exclusion,
                    )],
                    serde_json::from_value(program["prevent_building_padding"].clone())
                        .map_err(|error| error.to_string())?,
                    Coverage::Complete,
                )
            })
            .transpose()?;
        let mut clearance_budget = Budget::new(10000)?;
        let mut blocked = 0;
        let mut poses = Vec::new();
        let mut rejected = Vec::new();
        let roads = [roadside_decor::Road {
            hierarchy: 0,
            width: 12.,
            spline: true,
            path: &path,
        }];
        let mut emission = None;
        let report = if with_terrain {
            let emitted = roadside_live::emit(
                roadside_live::Request {
                    stage: &stage,
                    sampling,
                    roads: &roads,
                    seed: 42,
                    realm_mask: 2,
                },
                decor_live::Context {
                    transaction: transaction
                        .as_mut()
                        .ok_or("Missing fixture terrain transaction")?,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next_pose,
                    water: None,
                },
                |_, _| Ok(true),
                || false,
            )?
            .ok_or("Fixture live emission cancelled")?;
            for instance in &emitted.admitted {
                poses.push(
                    json!({"prefab":instance.prefab,"position":instance.transform.position,
                    "rotation":instance.transform.rotation,"scale":instance.transform.scale}),
                );
            }
            emission = Some(serde_json::to_value(&emitted)?);
            emitted.sampling
        } else {
            roadside_decor::sample(
            &roads,
            sampling, prefabs.len(), 42, &mut attempts,
            |candidate| {
                let [x, z] = candidate.position_xz;
                let view = layers.view();
                let y = spawn::sample_height_view(view, x / 1000. + 0.5, z / 1000. + 0.5)?;
                let source = decor::Candidate { stage: index, prefab: candidate.prefab, position: [x, y, z] };
                match fitting.try_fit_path_static(view, decor_fit::StaticRequest {
                    candidate: &source, stage: &stage, prefab: &prefabs[&stage.prefabs[candidate.prefab]], seed: 42, water: None,
                })? {
                    decor_fit::OrientedOutcome::Fitted(pose) => {
                        if let Some(clearance) = &clearance {
                            match clearance.check(pose.position, &mut clearance_budget, || false)?.verdict {
                                Verdict::Clear => {}
                                Verdict::Blocked(_) => {
                                    blocked += 1;
                                    return Ok(false);
                                }
                                verdict => return Err(format!("Fixture clearance failed {verdict:?}")),
                            }
                        }
                        poses.push(json!({"prefab":stage.prefabs[candidate.prefab], "position":pose.position,
                                          "rotation":pose.rotation,"scale":pose.scale}));
                        Ok(true)
                    }
                    decor_fit::OrientedOutcome::Rejected(reason) => {
                        rejected.push(format!("{reason:?}"));
                        Ok(false)
                    }
                }
            }, || false,
        ).map_err(|error| format!("Fixture sampling failed {error:?}"))?.ok_or("Fixture cancelled")?
        };
        if poses.is_empty() {
            return Err(format!("Captured prefab did not fit the fixture {rejected:?}").into());
        }
        let terrain_report = if let Some(mut transaction) = transaction {
            let first = &poses[0];
            let retry = decor_fit::Pose {
                candidate: next_pose,
                position: serde_json::from_value(first["position"].clone())?,
                rotation: serde_json::from_value(first["rotation"].clone())?,
                scale: serde_json::from_value(first["scale"].clone())?,
            };
            let height_before = transaction.view().height.to_vec();
            let topology_before = transaction.view().topology.to_vec();
            let splat_before = transaction.view().splat.to_vec();
            if transaction.try_apply(
                next_pose,
                &stage,
                first["prefab"].as_str().ok_or("Missing fitted prefab")?,
                &retry,
                || false,
            )? != Some(false)
                || transaction.view().height != height_before
                || transaction.view().topology != topology_before
                || transaction.view().splat != splat_before
            {
                return Err("Rejected fixture pose changed committed terrain".into());
            }
            let output = transaction
                .finish(|| false)?
                .ok_or("Fixture terrain publication cancelled")?;
            let topology = output.topology.as_ref().ok_or("Missing fixture topology")?;
            let mut height_hash = Sha256::new();
            for height in &output.height {
                height_hash.update(height.to_le_bytes());
            }
            let mut topology_hash = Sha256::new();
            for cell in topology {
                topology_hash.update(cell.to_le_bytes());
            }
            Some(
                json!({"report":output.report,"runtime_hole_shapes":runtime_holes,
                "height_changed_samples":output.height.iter().zip(&layers.height).filter(|(a,b)|a!=b).count(),
                "topology_changed_cells":topology.iter().zip(&layers.topology).filter(|(a,b)|a!=b).count(),
                "splat_unchanged":output.splat.as_deref()==Some(layers.splat.as_slice()),
                "height_sha256":format!("{:x}",height_hash.finalize()),
                "topology_sha256":format!("{:x}",topology_hash.finalize()),
                "alpha_unchanged":true,"runtime_holes_applied":false,
                "rejected_pose_preserved_grids":true}),
            )
        } else {
            None
        };
        results.push(json!({"scene_order":program["order"],"sampling":report,"fitting_samples":fitting.samples(),
                            "poses":poses,"rejected":rejected,
                            "terrain":terrain_report,"emission":emission,
                            "clearance":with_clearance.then(|| json!({"synthetic_box":exclusion,
                                "padding":program["prevent_building_padding"],"blocked":blocked,
                                "work":clearance_budget.used()})),
                            "pending":prefabs.values().map(|prefab| &prefab.metadata().pending).collect::<Vec<_>>()}));
    }
    let result = json!({"format":"mapper-path-fitting-verification","schema":1,"source_sha256":hash,
                        "terrain_fixture":"flat 1000 metre terrain, field topology, full alpha",
                        "sampling_policy":{"spacing_fraction":0.25,"anchor_filter_mode":2,"seed":42},
                        "programs":results,"terrain_effects_applied":with_terrain,"game_code_executed":false});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    serde_json::to_writer(&mut file, &result)?;
    file.sync_all()?;
    println!("{}", json!({"programs":results.len(),"source_sha256":hash}));
    Ok(())
}
