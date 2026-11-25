//! Re-run an installation-verified search against its immutable native terrain cache.
use mapper_studio::{
    Presence, Project, composition, native_preview, native_processing, placement_preview,
    prefab_assets, prefab_textures,
};
use papyrus_terrain::placement::CandidateCheck;
use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(5..=7).contains(&args.len()) {
        return Err("inspect_monument_search PROJECT RUST_ROOT CATALOG BINDINGS NEW_REPORT [WORKER [MONUMENT_SEEDS_JSON]]".into());
    }
    let project = Project::load(Path::new(&args[0]))?;
    let root = Path::new(&args[1]);
    let destination = Path::new(&args[4]);
    if destination.exists() {
        return Err("Report must be a new file".into());
    }
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[2]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[3]), &catalog, || false)?;
    let stage_source = project
        .monument_rules
        .as_ref()
        .filter(|r| r.stage_rules.is_some())
        .map(|r| mapper_studio::monument_source::Source::cached(r, &catalog, &bindings, || false))
        .transpose()?;
    let default_worker;
    let worker = if let Some(path) = args.get(5) {
        Path::new(path)
    } else {
        default_worker = native_preview::worker_path()?;
        &default_worker
    };
    let processed = native_processing::build(
        root,
        worker,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Terrain processing cancelled")?;
    let seeds: Vec<u32> = if let Some(path) = args.get(6) {
        let raw = std::fs::read(path).map_err(|e| e.to_string())?;
        if raw.len() > 4096 {
            return Err("Seed list exceeds 4 KiB".into());
        }
        serde_json::from_slice(&raw).map_err(|e| e.to_string())?
    } else {
        vec![project.seeds[4].value]
    };
    if seeds.is_empty() || seeds.len() > 16 {
        return Err("Choose between one and sixteen monument seeds".into());
    }
    let mut cases = Vec::new();
    for seed in seeds {
        let mut project = project.clone();
        project.seeds[4].value = seed;
        project.seeds[4].inherited = false;
        project.validate()?;
        if native_preview::processing_key(&project.terrain_key()) != processed.source.key {
            return Err("Monument seed changed the processed terrain identity".into());
        }
        eprintln!("Inspecting monument seed {seed}");
        let mut counts = BTreeMap::<u64, BTreeMap<String, usize>>::new();
        let solution = placement_preview::solve_native_observed(
            &project,
            &processed.source.layers,
            &catalog,
            &bindings,
            || false,
            |id, check| {
                let reason = match check {
                    CandidateCheck::Accepted(..) => "Accepted candidate".into(),
                    CandidateCheck::AcceptedRelaxed(_, _, r) => {
                        format!("Accepted candidate at relaxation {r}")
                    }
                    CandidateCheck::Rejected(s) => s.clone(),
                    CandidateCheck::Exhausted(s) | CandidateCheck::TargetExhausted(s) => {
                        format!("Budget: {s}")
                    }
                    CandidateCheck::Cancelled => "Cancelled".into(),
                };
                *counts.entry(id).or_default().entry(reason).or_default() += 1;
            },
        )?
        .ok_or("Search cancelled")?;
        let again = placement_preview::solve_native(
            &project,
            &processed.source.layers,
            &catalog,
            &bindings,
            || false,
        )?
        .ok_or("Replay cancelled")?;
        if again != solution {
            return Err("Placement search was not reproducible".into());
        }
        // Independently sample every accepted root and rotated anchor on the
        // processed pre-monument layers. Final topology includes stamp painting
        // and is not the terrain on which source eligibility was decided.
        let mut stage_audit = Vec::new();
        if let Some(source) = &stage_source {
            let stages = source.stage_source.as_ref().ok_or("Missing stage source")?;
            for outcome in &solution.outcomes {
                let Some(p) = &outcome.position else {
                    continue;
                };
                let target = project
                    .targets
                    .iter()
                    .find(|t| t.instance_id == outcome.id)
                    .ok_or("Missing target")?;
                let path = target
                    .prefab_path
                    .as_ref()
                    .ok_or("Missing accepted prefab")?;
                let root_check = stages.evaluate(
                    &processed.source.layers,
                    path,
                    seed,
                    outcome.id,
                    [p.x, p.z],
                )?;
                if !matches!(
                    root_check,
                    papyrus_terrain::monument_stage::Check::Accepted { .. }
                ) {
                    return Err(format!(
                        "Accepted target {} failed its stage root audit",
                        outcome.id
                    ));
                }
                let filter = stages
                    .anchor_filter(path)
                    .ok_or("Missing accepted stage filter")?;
                let prefab = source
                    .prefabs
                    .get(path)
                    .ok_or("Missing accepted source rules")?;
                let (sin, cos) = p.yaw.to_radians().sin_cos();
                let mut factors = Vec::new();
                for anchor in &prefab.rules.anchors {
                    let x = p.x + cos * anchor.position[0] - sin * anchor.position[2];
                    let z = p.z + sin * anchor.position[0] + cos * anchor.position[2];
                    let u = x / project.size as f32 + 0.5;
                    let v = z / project.size as f32 + 0.5;
                    if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
                        return Err("Accepted stage anchor lies outside terrain".into());
                    }
                    let factor =
                        papyrus_terrain::spawn::sample_factor(&processed.source.layers, filter, u, v)?;
                    if factor <= 0. {
                        return Err(format!(
                            "Accepted target {} failed stage anchor audit",
                            outcome.id
                        ));
                    }
                    factors.push(factor);
                }
                stage_audit.push(serde_json::json!({"id":outcome.id,"root":format!("{root_check:?}"),"anchor_factors":factors}));
            }
        }
        let mut pinned = project.clone();
        pinned.targets.retain_mut(|target| {
            if let Some(p) = solution
                .outcomes
                .iter()
                .find(|o| o.id == target.instance_id)
                .and_then(|o| o.position.as_ref())
            {
                target.x = p.x;
                target.z = p.z;
                target.yaw = Some(p.yaw);
                target.exact = true;
                target.presence = Presence::Required;
                true
            } else {
                false
            }
        });
        let exact = placement_preview::solve_native(
            &pinned,
            &processed.source.layers,
            &catalog,
            &bindings,
            || false,
        )?
        .ok_or("Exact replay cancelled")?;
        let exact_matches = exact.required_missing == 0
            && exact.outcomes.iter().all(|o| {
                o.position
                    == solution
                        .outcomes
                        .iter()
                        .find(|p| p.id == o.id)
                        .unwrap()
                        .position
            });
        // Check final monument terrain and boat access without claiming downstream
        // roads, rails, objects or runtime physics have been validated by this study.
        let mut monuments_only = project.clone();
        monuments_only.roads = None;
        monuments_only.rails = None;
        monuments_only.objects = None;
        monuments_only.decoration = None;
        let composition_error = match composition::compose_layout_with_worker(
            &processed.source,
            &monuments_only,
            &solution,
            &catalog,
            &bindings,
            Some(worker),
            || false,
        ) {
            Ok(Some(_)) => None,
            Ok(None) => Some("Composition cancelled".into()),
            Err(e) => Some(e),
        };
        let report = serde_json::json!({
            "engine":project.placement_engine, "monument_rules":project.monument_rules, "terrain_key":project.terrain_key(),"monument_seed":seed,
            "rivers_processed":processed.source.rivers.is_some(),"repeatable":true,
            "accepted_stage_filter_audit":stage_audit,
        "exact_accepted_poses_reproduced":exact_matches,"exact_outcomes":exact.outcomes.iter().map(|o|serde_json::json!({"id":o.id,"position":o.position.as_ref().map(|p|[p.x,p.y,p.z,p.yaw]),"reason":o.reason})).collect::<Vec<_>>(),
            "monument_composition_error":composition_error,"full_world_validated":false,
            "required_missing":solution.required_missing,
            "targets":project.targets.iter().map(|t| {
                let outcome = solution.outcomes.iter().find(|o| o.id == t.instance_id);
                serde_json::json!({"id":t.instance_id,"name":t.name,
                    "placed":outcome.is_some_and(|o|o.position.is_some()),
                    "position":outcome.and_then(|o|o.position.as_ref()).map(|p|serde_json::json!({"x":p.x,"y":p.y,"z":p.z,"yaw":p.yaw})),
                    "reason":outcome.map(|o|&o.reason), "assessments":counts.get(&t.instance_id)})
            }).collect::<Vec<_>>()
        });
        cases.push(report);
    }
    let report = serde_json::json!({"format":"mapper-native-monument-search-study","schema":2,
        "terrain_cache":processed.cache_status,"cases":cases,"desktop_windows_opened":0});
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &report).map_err(|e| e.to_string())?;
    println!(
        "{} targets inspected across {} monument seed cases",
        project.targets.len(),
        cases.len()
    );
    Ok(())
}
