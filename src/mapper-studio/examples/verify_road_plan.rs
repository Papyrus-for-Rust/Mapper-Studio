//! Verify the intermediate stage against an already verified full native bundle.
#[path = "support/road_paint_study.rs"]
mod road_paint_study;
#[path = "support/stamp_artifacts.rs"]
mod stamp_artifacts;
use mapper_studio::{
    Project, composition, native_preview, native_processing, prefab_assets, prefab_textures,
};
use papyrus_terrain::monument_composition::Composed;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, sync::atomic::AtomicBool};
fn revision(maps: &Composed) -> String {
    let mut h = Sha256::new();
    for &v in &maps.layers.height {
        h.update(v.to_le_bytes());
    }
    h.update(&maps.layers.biome);
    h.update(&maps.layers.splat);
    for &v in &maps.layers.topology {
        h.update(v.to_le_bytes());
    }
    h.update(&maps.alpha);
    if let Some(o) = &maps.ocean {
        h.update(&o.classification);
        for &v in &o.topology {
            h.update(v.to_le_bytes());
        }
        for &v in &o.local_water {
            h.update(v.to_le_bytes());
        }
    }
    format!("{:x}", h.finalize())
}
fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > maximum {
        return Err("Reference input exceeds bound".into());
    }
    Ok(raw)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(7..=10).contains(&args.len()) {
        return Err("verify_road_plan PROJECT RUST_ROOT CATALOG BINDINGS ROADSIDE_SOURCE REFERENCE_BUNDLE NEW_REPORT [WORKER [NEW_STAMP_ARTIFACTS [QUALITY_FEEDBACK]]]".into());
    }
    let project = Project::load(Path::new(&args[0]))?;
    let feedback: Option<mapper_studio::roadside_placement::QualityFeedback> = args
        .get(9)
        .map(|p| {
            bounded(Path::new(p), 65536)
                .and_then(|raw| serde_json::from_slice(&raw).map_err(|e| e.to_string()))
        })
        .transpose()?;
    let root = Path::new(&args[1]);
    let reference = Path::new(&args[5]);
    let output = Path::new(&args[6]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    if args.get(8).is_some_and(|p| Path::new(p).exists()) {
        return Err("Stamp artifacts must be new".into());
    }
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    let catalog = prefab_assets::load(root, Path::new(&args[2]), installation, || false)?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[3]), &catalog, || false)?;
    let roadside =
        mapper_studio::roadside_source::Source::load(Path::new(&args[4]), root, &catalog, || false)?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&bounded(&reference.join("manifest.json"), 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    let read_receipt = |name: &str, limit: u64| -> Result<Vec<u8>, String> {
        let raw = bounded(&reference.join(name), limit)?;
        if manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
            || manifest["files"][name]["sha256"].as_str()
                != Some(format!("{:x}", Sha256::digest(&raw)).as_str())
        {
            return Err(format!("Reference receipt mismatch: {name}"));
        }
        Ok(raw)
    };
    let reference_project: Project =
        serde_json::from_slice(&read_receipt("project.mapper", 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    let launch_revision = project
        .roads
        .as_ref()
        .is_some_and(|r| r.entrance_launch.is_some());
    let mut terrain_project = project.clone();
    if launch_revision {
        terrain_project
            .roads
            .as_mut()
            .ok_or("Missing road recipe")?
            .entrance_launch = None;
    }
    if reference_project != terrain_project {
        return Err("Reference project differs from planned project".into());
    }
    let roads_raw = read_receipt("roads.json", 128 * 1024 * 1024)?;
    let roads: serde_json::Value = serde_json::from_slice(&roads_raw).map_err(|e| e.to_string())?;
    let default_worker;
    let worker = if let Some(p) = args.get(7) {
        Path::new(p)
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
    .ok_or("Processing cancelled")?;
    if let Some(rivers) = &processed.source.rivers
        && serde_json::to_vec_pretty(&rivers.report).map_err(|e| e.to_string())?
            != read_receipt("river-generation.json", 4 * 1024 * 1024)?
    {
        return Err(
            "Shared path-surface refactor changed the saved river generation report".into(),
        );
    }
    let solution = mapper_studio::placement_preview::solve_native(
        &project,
        &processed.source.layers,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Placement cancelled")?;
    let mut maps = composition::compose_monuments(
        &processed.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Stamp composition cancelled")?;
    if maps.roads.is_some() || maps.spawn_audit.is_some() || maps.topology_overrides.is_some() {
        return Err("Intermediate stage already ran later passes".into());
    }
    let before = revision(&maps);
    let plan = mapper_studio::road_planning::prepare(
        &project,
        &solution,
        &catalog,
        &bindings,
        &maps,
        || false,
    )?
    .ok_or("Planning cancelled")?;
    if revision(&maps) != before {
        return Err("Planning changed the monument revision".into());
    }
    let report = plan.planning_report();
    let expected = roads["roads"]
        .as_array()
        .ok_or("Missing reference routes")?;
    if report.roads.len() != expected.len() {
        return Err("Planned road count differs".into());
    }
    let planned_json = report.json();
    let mut control_count = 0;
    let mut equal_controls = 0;
    let mut controls_equal = true;
    let mut destinations_equal = true;
    for (index, (actual, expected)) in report.roads.iter().zip(expected).enumerate() {
        let points: Vec<[f32; 3]> =
            serde_json::from_value(expected["points"].clone()).map_err(|e| e.to_string())?;
        let equal = points.len() == actual.points.len()
            && points
                .iter()
                .flatten()
                .zip(actual.points.iter().flatten())
                .all(|(a, b)| a.to_bits() == b.to_bits());
        let destinations = planned_json["roads"][index]["from"] == expected["from"]
            && planned_json["roads"][index]["to"] == expected["to"];
        controls_equal &= equal;
        destinations_equal &= destinations;
        if equal {
            equal_controls += points.len();
        }
        if !launch_revision && (!equal || !destinations) {
            return Err(format!(
                "Planned controls or destination changed at road {index}"
            ));
        }
        control_count += actual.points.len();
    }
    if serde_json::to_value(&report.families).map_err(|e| e.to_string())? != roads["families"] {
        return Err("Planned road families differ".into());
    }
    if report.changed_heights == 0 {
        return Err("Study did not exercise scratch routing terrain writes".into());
    }
    let paths = plan.roads();
    let mut candidates = Vec::new();
    for (path, prefab) in roadside.prefabs() {
        let sockets: Vec<_> = prefab
            .road_connectors(roadside.profile())
            .map(|c| c.position)
            .collect();
        let result = papyrus_terrain::roadside_monuments::generate(
            &sockets,
            &paths,
            roadside.stage().road_mode()?,
            roadside.profile().settings(2_000_000, 100_000),
            || false,
        )
        .map_err(|e| format!("Roadside candidate generation failed: {e:?}"))?
        .ok_or("Candidates cancelled")?;
        candidates.push(serde_json::json!({"path":path,"candidates":result.candidates.len(),"prefab_count":prefab.parameters.count}));
    }
    plan.check_inputs(&project, &solution, &catalog, &bindings, &maps, || false)?
        .ok_or("Context check cancelled")?;
    let mut changed = project.clone();
    changed.master_seed = changed.master_seed.wrapping_add(1);
    if plan
        .check_inputs(&changed, &solution, &catalog, &bindings, &maps, || false)
        .is_ok()
    {
        return Err("Stale project was accepted by road plan".into());
    }
    maps.alpha[0] ^= 1;
    let rejected = plan
        .check_inputs(&project, &solution, &catalog, &bindings, &maps, || false)
        .is_err();
    maps.alpha[0] ^= 1;
    if !rejected || revision(&maps) != before {
        return Err("Stale alpha revision was not rejected or restored".into());
    }
    let requests = mapper_studio::roadside_placement::captured_requests(&roadside, &solution)?;
    let context = || mapper_studio::roadside_placement::Context {
        project: &project,
        initial: &solution,
        catalog: &catalog,
        bindings: &bindings,
        maps: &maps,
        plan: &plan,
        source: &roadside,
    };
    let mut selection = mapper_studio::roadside_placement::select_with_feedback(
        context(),
        &requests,
        feedback.as_ref(),
        128_000_000,
        || false,
    )?
    .ok_or("Roadside selection cancelled")?;
    let repeat = mapper_studio::roadside_placement::select_with_feedback(
        context(),
        &requests,
        feedback.as_ref(),
        128_000_000,
        || false,
    )?
    .ok_or("Roadside replay cancelled")?;
    if selection.selection.selected != repeat.selection.selected
        || selection.selection.missing != repeat.selection.missing
        || selection.selection.work != repeat.selection.work
    {
        return Err("Grouped roadside selection was not repeatable".into());
    }
    if selection.selection.selected.is_empty() {
        return Err("No provisional roadside monuments were selected".into());
    }
    if selection
        .quality_feedback
        .as_ref()
        .is_some_and(|f| f.baseline_selected > selection.selection.selected.len())
    {
        return Err("Quality attempt lost roadside monuments".into());
    }
    let retained = serde_json::to_vec(&plan.roads().iter().map(|r| r.path).collect::<Vec<_>>())
        .map_err(|e| e.to_string())?;
    let adjusted = selection
        .straighten(&plan, 1_000_000, || false)?
        .ok_or("Road straightening cancelled")?;
    let replay = repeat
        .straighten(&plan, 1_000_000, || false)?
        .ok_or("Road straightening replay cancelled")?;
    if serde_json::to_vec(&adjusted).map_err(|e| e.to_string())?
        != serde_json::to_vec(&replay).map_err(|e| e.to_string())?
    {
        return Err("Road straightening did not replay exactly".into());
    }
    if adjusted.changed_controls == 0 {
        return Err("Study did not change any roadside road spans".into());
    }
    let adjusted_roads = adjusted.roads();
    let mut attachments = 0;
    for (index, road) in report.roads.iter().enumerate() {
        for (side, destination) in [road.from, road.to].into_iter().enumerate() {
            if let papyrus_terrain::road_network::Destination::Network {
                road: target,
                point,
            } = destination
            {
                let path = adjusted_roads[index].path;
                let endpoint = if side == 0 {
                    0
                } else {
                    path.points().len() - 1
                };
                if path.points()[endpoint]
                    .iter()
                    .zip(adjusted_roads[target].path.points()[point])
                    .any(|(a, b)| a.to_bits() != b.to_bits())
                {
                    return Err(format!(
                        "Straightened road {index} lost its network attachment"
                    ));
                }
                attachments += 1;
            }
        }
    }
    let original = selection.selection.selected[0].yaw;
    selection.selection.selected[0].yaw = original + 1.;
    let rejected = selection.straighten(&plan, 1_000_000, || false).is_err();
    selection.selection.selected[0].yaw = original;
    if !rejected {
        return Err("Edited roadside selection was accepted without re-selection".into());
    }
    eprintln!("Painting checked roadside monument stamps");
    let stamped = mapper_studio::roadside_stamps::compose(context(), &selection, 1_000_000, || false)?
        .ok_or("Roadside stamping cancelled")?;
    let stamped_repeat =
        mapper_studio::roadside_stamps::compose(context(), &repeat, 1_000_000, || false)?
            .ok_or("Roadside stamping replay cancelled")?;
    if revision(&stamped.maps) != revision(&stamped_repeat.maps)
        || serde_json::to_vec(&stamped.report).map_err(|e| e.to_string())?
            != serde_json::to_vec(&stamped_repeat.report).map_err(|e| e.to_string())?
        || serde_json::to_vec(&stamped.roads).map_err(|e| e.to_string())?
            != serde_json::to_vec(&adjusted).map_err(|e| e.to_string())?
    {
        return Err("Roadside stamp stage did not replay exactly or changed adjusted roads".into());
    }
    drop(stamped_repeat);
    let shared_rivers = match (&maps.rivers, &stamped.maps.rivers) {
        (None, None) => true,
        (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
        _ => false,
    };
    let shared_water = match (&maps.ocean, &stamped.maps.ocean) {
        (None, None) => true,
        (Some(a), Some(b)) => a.local_water == b.local_water,
        _ => false,
    };
    if !shared_rivers
        || !shared_water
        || stamped.report.changes.height_samples == 0
        || stamped.report.changes.splat_cells == 0
    {
        return Err(
            "Roadside stage lost preceding water/river data or painted no height/material changes"
                .into(),
        );
    }
    let reused = mapper_studio::roadside_placement::Context {
        maps: &stamped.maps,
        ..context()
    };
    if mapper_studio::roadside_stamps::compose(reused, &selection, 1_000_000, || false).is_ok() {
        return Err("Roadside stamps could be applied twice to the same stage".into());
    }
    eprintln!("Verifying source-bound paved and dirt surface passes");
    let road_paint = match road_paint_study::run(&project, root, worker, &roadside, &plan, &stamped)
    {
        Ok(study) => study,
        Err(error) => {
            let curves: Vec<_> = stamped
                .roads
                .roads()
                .iter()
                .map(|r| serde_json::json!({"hierarchy":r.hierarchy,"path":r.path}))
                .collect();
            let diagnostic = serde_json::json!({"format":"mapper-failed-road-plan-study","schema":1,"playable":false,
                "failed_stage":"road_paint_study","error":error,"road_curves":curves,"planned_road_report":report.json()});
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .map_err(|e| e.to_string())?;
            serde_json::to_writer_pretty(file, &diagnostic).map_err(|e| e.to_string())?;
            return Err(error);
        }
    };
    let stamp_artifacts = if let Some(path) = args.get(8) {
        Some(stamp_artifacts::publish(
            Path::new(path),
            &processed.source,
            &maps,
            &stamped,
            &road_paint,
        )?)
    } else {
        None
    };
    if retained
        != serde_json::to_vec(&plan.roads().iter().map(|r| r.path).collect::<Vec<_>>())
            .map_err(|e| e.to_string())?
    {
        return Err("Road straightening changed retained input controls or tangents".into());
    }
    if revision(&maps) != before {
        return Err("Roadside selection changed the input terrain".into());
    }
    let result = serde_json::json!({"format":"mapper-retained-road-plan-verification","schema":1,"roadside_source_sha256":roadside.sha256,
        "road_count":report.roads.len(),"controls_bitwise_equal_to_existing_full_generation":equal_controls,
        "control_count":control_count,"controls_all_equal_to_reference":controls_equal,"checked_launch_revision":launch_revision,
        "planned_road_report":report.json(),"road_destinations_equal":destinations_equal,"road_families_equal":true,"monument_revision_sha256":before,
        "all_input_terrain_alpha_and_ocean_arrays_unchanged":true,"scratch_height_writes_discarded":report.changed_heights,
        "no_later_passes_run_on_input":true,"candidates":candidates,"accepted_roadside_monuments":0,
        "grouped_selection":selection,"grouped_selection_replayed_exactly":true,"stale_project_and_alpha_rejected":true,
        "straightened_paths":adjusted,"straightening_replayed_exactly":true,"edited_selection_rejected":true,
        "retained_controls_and_tangents_unchanged":true,
        "network_attachments_checked":attachments,"network_attachments_bitwise_equal":true,
        "stamp_stage":stamped.report,"stamp_stage_replayed_exactly":true,"stamp_stage_revision_sha256":revision(&stamped.maps),
        "prior_rivers_and_local_water_preserved":true,"double_stamping_rejected":true,"stamp_artifact_manifest":stamp_artifacts,
        "road_paint_study":road_paint.report,"prior_river_report_bytewise_equal":true,
        "roadside_placement_integrated":false,"desktop_windows_opened":0});
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &result).map_err(|e| e.to_string())?;
    println!(
        "{} retained roads and {control_count} controls verified",
        report.roads.len()
    );
    Ok(())
}
