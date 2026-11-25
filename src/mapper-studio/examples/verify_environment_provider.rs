//! Verify owned source inputs and exercise the real provider bridge. Occurrences,
//! activation states and provider grouping are synthetic, not generated gameplay.
use mapper_studio::{
    environment_provider::{Occurrence, Source},
    environment_runtime::{Lifecycle, Verified},
    installation, prefab_assets,
    prefab_textures::Bindings,
};
use papyrus_terrain::environment::{Box3, Coverage, Transform, Verdict, scene::Scene};
use serde_json::json;
use std::{fs::OpenOptions, io::Write, path::Path};
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !matches!(args.len(), 6 | 8) {
        return Err("ROOT CATALOG BINDINGS SNAPSHOT SNAPSHOT_SHA256 NEW_REPORT [OFFSHORE_CAPTURE NATIVE_BUNDLE]".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[5]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let _scope = installation::VerificationScope::default();
    let installation = installation::inspect(root, || false)?;
    let runtime = Verified::available(root, &installation, || false)?
        .ok_or("No matching environment runtime profile")?;
    let runtime_id = runtime.id().to_owned();
    let runtime_sha = runtime.fingerprint()?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let bindings = Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    let expected = args[4].to_str().ok_or("Invalid snapshot receipt")?;
    let source = Source::load(
        runtime,
        &catalog,
        &bindings,
        Path::new(&args[3]),
        expected,
        || false,
    )?
    .ok_or("Cancelled")?;
    let context = source.context("a".repeat(64))?;
    let actors = source
        .prefab_paths()
        .enumerate()
        .map(|(i, path)| Occurrence {
            id: i as u64 + 1,
            prefab: path.into(),
            pose: Transform {
                position: [i as f32 * 250., 7., 200.],
                rotation: [0., 0.6, 0., 0.8],
                scale: [1.; 3],
            },
            lifecycle: Lifecycle::ActivatedAuthored,
        })
        .collect::<Vec<_>>();
    let request = source.request(&context, 1, &actors)?;
    let pending = source.request(&context, 2, &[])?;
    let mut scene = Scene::new_ordered(
        context.clone(),
        vec![request.requirement(vec![]), pending.requirement(vec![1])],
        vec![1, 2],
        Coverage::Incomplete,
    )?;
    let before = scene.snapshot();
    let unknown_prefix = request
        .query_prefix(&before, 1_000_000, || false)?
        .map_err(|v| format!("Prefix: {v:?}"))?;
    assert!(matches!(
        source.prepare_update_from_prefix(
            &before,
            &request,
            &unknown_prefix,
            128_000_000,
            || false
        )?,
        Err(Verdict::Incomplete)
    ));
    // The low-level actor import can retain known geometry in an incomplete
    // index. It is distinct from accepting a checked placement stage above.
    let update = source
        .prepare_update(&before, &request, 128_000_000, || false)?
        .map_err(|v| format!("Provider: {v:?}"))?;
    let (update, report) = update.into_parts();
    assert!(report.preparation_work_reserved + report.index_work <= 128_000_000);
    assert!(scene.publish(update, || false)?);
    let current = scene.snapshot();
    let distant = current.world().query(
        Box3 {
            center: [100000., 100000., 100000.],
            rotation: [0., 0., 0., 1.],
            extents: [1.; 3],
        },
        u32::MAX,
        0.,
        1_000_000,
        || false,
    )?;
    assert!(matches!(
        distant.verdict,
        Verdict::Incomplete | Verdict::Unresolved(_)
    ));
    assert!(!current.summary().declared_coverage_complete);
    assert_eq!(current.summary().pending_stages, vec![2]);
    assert_eq!(before.revision(), 0);
    let cancelled = source.prepare_update(&current, &request, 128_000_000, || true)?;
    assert!(matches!(cancelled, Err(Verdict::Cancelled)));
    let mut moved = actors.clone();
    moved[0].pose.position[0] += 25.;
    let moved = source.request(current.context(), 1, &moved)?;
    assert_eq!(request.inputs_sha256(), moved.inputs_sha256());
    assert_ne!(request.occurrences_sha256(), moved.occurrences_sha256());
    // Explicitly synthetic complete baseline: not a claim about the game's
    // actual scene/provider roster. Exercise the public checked-update path.
    let mut modeled = Scene::new_ordered(
        context,
        vec![request.requirement(vec![]), pending.requirement(vec![])],
        vec![1, 2],
        Coverage::Complete,
    )?;
    let modeled_before = modeled.snapshot();
    let modeled_prefix = request
        .query_prefix(&modeled_before, 1_000_000, || false)?
        .map_err(|v| format!("Prefix: {v:?}"))?;
    let (update, modeled_report) = source
        .prepare_update_from_prefix(
            &modeled_before,
            &request,
            &modeled_prefix,
            128_000_000,
            || false,
        )?
        .map_err(|v| format!("Checked provider: {v:?}"))?
        .into_parts();
    assert!(modeled.publish(update, || false)?);
    let modeled_after = modeled.snapshot();
    assert!(
        source
            .prepare_update_from_prefix(
                &modeled_after,
                &request,
                &modeled_prefix,
                128_000_000,
                || false
            )
            .is_err()
    );
    let next_prefix = pending
        .query_prefix(&modeled_after, 1_000_000, || false)?
        .map_err(|v| format!("Prefix: {v:?}"))?;
    assert!(next_prefix.summary().declared_prefix_complete);
    assert!(!modeled_after.summary().declared_coverage_complete);
    let self_prefix = request
        .query_prefix(&modeled_after, 1_000_000, || false)?
        .map_err(|v| format!("Prefix: {v:?}"))?;
    assert_eq!(self_prefix.summary().instances, 0);
    assert_eq!(next_prefix.summary().volumes, modeled_report.volumes);
    let mut candidate_counts = [0_usize; 3]; // clear, blocked, unresolved
    let mut candidate_preparation_work = 0;
    let mut candidate_assessment_work = 0;
    let mut offshore_decorator_checks = Vec::new();
    let mut authored_water_captures = Vec::new();
    let mut water_gate_assessments = Vec::new();
    for actor in &actors {
        authored_water_captures.push(json!({"prefab":actor.prefab,
            "capture":source.attributes(&actor.prefab)?.environment.as_ref().and_then(|e|e.water_placement.as_ref())}));
        if let Some(water) = source.prepare_water(&actor.prefab)? {
            use mapper_studio::placement_water::{LookupState, Selection};
            let assessment = water
                .assess(
                    &request,
                    LookupState::AuthoredUnchanged,
                    Selection::Unknown,
                    100000,
                    || false,
                    |_| Err("Diagnostic lacks runtime body position".into()),
                    |_| Err("Diagnostic lacks runtime collider bounds".into()),
                )?
                .map_err(|v| format!("Water gate: {v:?}"))?;
            let unmodeled = water
                .assess(
                    &request,
                    LookupState::Unmodeled,
                    Selection::Unknown,
                    100000,
                    || false,
                    |_| panic!(),
                    |_| panic!(),
                )?
                .map_err(|v| format!("Unmodeled water gate: {v:?}"))?;
            assert_eq!(
                unmodeled.water,
                papyrus_terrain::offshore_tiers::WaterGate::Unknown
            );
            water_gate_assessments.push(json!({"prefab":actor.prefab,
                "synthetic_lookup_state":"AuthoredUnchanged","assessment":assessment,
                "unmodeled_lookup_stays_unknown":true}));
        }
        let candidate = source
            .prepare_candidate(&actor.prefab, 128_000_000, || false)?
            .map_err(|v| format!("Candidate preparation: {v:?}"))?;
        candidate_preparation_work += candidate.preparation_work_reserved();
        if actor.prefab.contains("/monument/offshore/") {
            // These are explicit reference-behavior trials using independent
            // draws/water fixtures, not execution of the selected client's code.
            let program =
                mapper_studio::offshore_decor::Program::new(source.attributes(&actor.prefab)?)?;
            for value in [0., 0.125, 0.25, 0.5, 0.75, 1.] {
                for water in [-2., 0., 8.] {
                    let pose = program
                        .apply(
                            [2700., -80., 100.],
                            -500.,
                            program.required_work(),
                            || false,
                            |_, _| Ok(value),
                            |_| Ok(water),
                        )?
                        .map_err(|v| format!("Offshore decorators: {v:?}"))?;
                    assert_eq!(pose.position, [2700., water, 100.]);
                    let assessment = candidate.assess(
                        &modeled_after,
                        &request,
                        &self_prefix,
                        Transform {
                            position: pose.position,
                            rotation: pose.rotation,
                            scale: pose.scale,
                        },
                        33,
                        0.,
                        128_000_000,
                        || false,
                    )?;
                    assert_eq!(assessment.verdict, Verdict::Clear);
                    offshore_decorator_checks.push(json!({"prefab":actor.prefab,
                        "draw":value,"water_surface":water,"pose":pose,"assessment":assessment}));
                }
            }
        }
        // This diagnostic uses the inspected profile's forbidden mask, but
        // synthetic occurrence positions and baseline. It is not a site solve.
        for (request, prefix, expect_clear) in [
            (&request, &self_prefix, true),
            (&pending, &next_prefix, false),
        ] {
            let assessment = candidate.assess(
                &modeled_after,
                request,
                prefix,
                actor.pose,
                33,
                0.,
                128_000_000,
                || false,
            )?;
            candidate_assessment_work += assessment.work;
            if expect_clear {
                assert_eq!(assessment.verdict, Verdict::Clear);
            } else {
                let index = match assessment.verdict {
                    Verdict::Clear => 0,
                    Verdict::Blocked(_) => 1,
                    Verdict::Unresolved(_) => 2,
                    other => return Err(format!("Candidate assessment: {other:?}")),
                };
                candidate_counts[index] += 1;
            }
        }
    }
    assert_eq!(candidate_counts.iter().sum::<usize>(), actors.len());
    assert_eq!(modeled.snapshot().revision(), modeled_after.revision());
    let prefix_result = json!({"synthetic_complete_baseline":true,"before":modeled_before.summary(),"after":modeled_after.summary(),
        "first_prefix":modeled_prefix.summary(),"next_prefix":next_prefix.summary(),"retrial_prefix":self_prefix.summary(),
        "stale_prefix_rejected":true,"incomplete_baseline_checked_update_rejected":true,"provider":modeled_report,
        "candidate_checks":{"count":actors.len(),"synthetic_forbidden_mask":33,
            "self_excluded_checks_all_clear":true,"next_prefix_clear":candidate_counts[0],
            "next_prefix_blocked":candidate_counts[1],"next_prefix_unresolved":candidate_counts[2],
            "preparation_work_reserved_sum":candidate_preparation_work,
            "assessment_work_sum":candidate_assessment_work,"scene_revision_unchanged":true}});
    let integrated_offshore = if args.len() == 8 {
        Some(verify_offshore_stage(
            root,
            &catalog,
            &bindings,
            &source,
            Path::new(&args[6]),
            Path::new(&args[7]),
        )?)
    } else {
        None
    };
    let result = json!({"format":"papyrus-owned-environment-provider-check","schema":1,
        "snapshot_sha256":expected,"runtime_profile":runtime_id,"runtime_profile_sha256":runtime_sha,
        "source_context_sha256":source.sha256(),"source_data_verified":true,
        "synthetic_occurrences_and_activation":true,"provider_order_is_official_scene_order":false,
        "before":before.summary(),"after":current.summary(),"provider":report,"distant_query":distant,
        "cancelled_update_preserved_revision":scene.snapshot().revision()==current.revision(),
        "generation_prefix_check":prefix_result,
        "authored_water_captures":authored_water_captures,"runtime_water_bounds_captured":false,
        "water_gate_assessments":water_gate_assessments,
        "integrated_offshore_stage":integrated_offshore,
        "offshore_decorator_checks":{"engine":mapper_studio::offshore_decor::VERSION,
            "independent_draws_not_game_seed_parity":true,"reference_behavior_not_client_gameplay_validation":true,
            "trials":offshore_decorator_checks},
        "world_generated":false,"physics_executed":false,"game_code_executed":false,"offshore_generation_enabled":false});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Verified source-bound provider bridge with {} synthetic occurrences",
        actors.len()
    );
    Ok(())
}

fn verify_offshore_stage(
    root: &Path,
    catalog: &prefab_assets::Catalog,
    bindings: &Bindings,
    source: &Source,
    capture: &Path,
    bundle: &Path,
) -> Result<serde_json::Value, String> {
    use mapper_studio::environment_provider::offshore::{
        self, Execution, Limits, Preparation, Sampling, Terrain,
    };
    use mapper_studio::placement_water::LookupState;
    use papyrus_terrain::{offshore_search, path_terrain};
    use sha2::{Digest, Sha256};
    use std::{io::Read, sync::atomic::AtomicBool};
    let project = mapper_studio::Project::load(&bundle.join("project.mapper"))?;
    let key = project.terrain_key();
    assert!(mapper_studio::native_preview::verify_source(
        root,
        &key,
        &AtomicBool::new(false)
    )?);
    let native = mapper_studio::native_profile::resolve(&key)?;
    let manifest_raw = std::fs::read(bundle.join("manifest.json")).map_err(|e| e.to_string())?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_raw).map_err(|e| e.to_string())?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["size"] != project.size
        || manifest["height_resolution"] != hr
        || manifest["surface_resolution"] != sr
        || manifest["assets"]["catalog_sha256"] != catalog.sha256
        || manifest["assets"]["bindings_sha256"] != bindings.sha256
        || manifest["native_profile"]
            != json!({"id":native.id,"abi":native.abi,"sha256":native.sha256()?})
    {
        return Err("Native offshore diagnostic bundle has different source bindings".into());
    }
    let checked = |name: &str, length: usize| -> Result<Vec<u8>, String> {
        let mut bytes = vec![];
        std::fs::File::open(bundle.join(name))
            .map_err(|e| e.to_string())?
            .take(length as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() != length
            || manifest["files"][name]
                != json!({"bytes":length,"sha256":format!("{:x}",Sha256::digest(&bytes))})
        {
            return Err(format!("Offshore terrain receipt differs: {name}"));
        }
        Ok(bytes)
    };
    let height = checked("height.i16le", hr * hr * 2)?
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect::<Vec<_>>();
    let topology = checked("topology.i32le", sr * sr * 4)?
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect::<Vec<_>>();
    let stages = mapper_studio::offshore_source::Source::load(capture, root, catalog, || false)?;
    if stages.stages.len() != 1 {
        return Err("Owned diagnostic expects its single captured offshore stage".into());
    }
    let stage_id = stages.stages[0].object_id;
    let sea = mapper_studio::coastal_placement::Profile::available(root, catalog, || false)?
        .ok_or("Missing ocean-plane source profile")?
        .sea_level;
    let request_hash = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({"engine":offshore::VERSION,
        "source":source.sha256(),"stage_source":stages.sha256,"project":project,
        "manifest_sha256":format!("{:x}",Sha256::digest(&manifest_raw)),"ocean_plane":sea}))
            .map_err(|e| e.to_string())?
        )
    );
    let context = source.context(request_hash)?;
    let request = source.request(&context, stage_id, &[])?;
    let prepared = match source.prepare_offshore(
        catalog,
        &stages,
        &request,
        Execution {
            size: project.size,
            realm_mask: 2,
            world_cached: false,
            seed: project.seeds[4].value,
            passes: 10,
            attempts_per_prefab: 10000,
        },
        128_000_000,
        || false,
    )? {
        Preparation::Ready(stage) => stage,
        Preparation::Skipped => return Err("Owned offshore stage was skipped".into()),
        Preparation::Stopped(reason) => return Err(format!("Offshore preparation: {reason:?}")),
    };
    struct Sampler<'a> {
        height: path_terrain::Grid<'a>,
        minimum: [f32; 2],
        sea: f32,
    }
    impl Sampling for Sampler<'_> {
        fn height(&mut self, p: [f32; 2]) -> Result<f32, String> {
            let inverse = 1. / self.height.size;
            path_terrain::sample_height_uv(
                self.height,
                [
                    (p[0] - self.minimum[0]) * inverse,
                    (p[1] - self.minimum[1]) * inverse,
                ],
            )
        }
        fn surface_water(&mut self, _: &str, _: [f32; 3]) -> Result<f32, String> {
            Ok(self.sea)
        }
        fn lookup_state(&mut self, _: &str) -> LookupState {
            LookupState::AuthoredUnchanged
        }
    }
    let minimum = [-(project.size as f32) * 0.5; 2];
    let mut sampler = Sampler {
        height: path_terrain::Grid {
            size: project.size as f32,
            resolution: hr,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        },
        minimum,
        sea,
    };
    let terrain = Terrain {
        rect: offshore_search::TerrainRect {
            size: project.size,
            minimum,
        },
        topology: &topology,
        resolution: sr,
        minimum_y: -500.,
    };
    let make_scene = |coverage| {
        Scene::new_ordered(
            context.clone(),
            vec![request.requirement(vec![])],
            vec![stage_id],
            coverage,
        )
    };
    let incomplete = make_scene(Coverage::Incomplete)?;
    assert!(matches!(
        prepared.generate(
            &incomplete.snapshot(),
            &request,
            terrain,
            Limits {
                prefix: 1000000,
                search: 2000000,
                checks: 64000000
            },
            &mut sampler,
            || false
        ),
        Err(offshore::Failure::Environment(Verdict::Incomplete))
    ));
    // Explicit synthetic baseline and unchanged-roster/ocean-plane assertions.
    // The retained height/topology are real data, but this is not a complete world.
    let scene = make_scene(Coverage::Complete)?;
    let before = scene.snapshot();
    let run = prepared
        .generate(
            &before,
            &request,
            terrain,
            Limits {
                prefix: 1000000,
                search: 2000000,
                checks: 64000000,
            },
            &mut sampler,
            || false,
        )
        .map_err(|e| format!("Integrated offshore stage: {e:?}"))?;
    let offshore_search::Outcome::Found { placements, audit } = run.outcome else {
        return Err("Owned offshore search did not finish".into());
    };
    assert_eq!(placements.len(), 2);
    let placed = placements
        .iter()
        .map(|p| {
            assert_eq!(p.pose.position[1], sea);
            assert!(
                p.pose.position[0].abs() > project.size as f32 * 0.5
                    || p.pose.position[2].abs() > project.size as f32 * 0.5
            );
            json!({"prefab":prepared.prefab_path(p.id).unwrap(),"pose":p.pose})
        })
        .collect::<Vec<_>>();
    assert_eq!(scene.snapshot().revision(), before.revision());
    assert!(!scene.snapshot().summary().declared_coverage_complete);
    Ok(
        json!({"engine":run.engine,"stage":run.stage,"stage_source_sha256":run.stage_source_sha256,
        "preparation_work":run.preparation_work,"prefix_work":run.prefix_work,"checks_work":run.checks_work,
        "tier_rejections":run.tier_rejections,"lake_rejections":run.lake_rejections,"environment_rejections":run.environment_rejections,
        "search":audit,"placements":placed,"synthetic_complete_baseline":true,"synthetic_unchanged_roster_and_ocean_plane":true,
        "incomplete_baseline_rejected":true,"world_cached":false,"terrain_is_retained_final_composition":true,
        "environment_not_published":true,"world_generated":false,"game_code_executed":false}),
    )
}
