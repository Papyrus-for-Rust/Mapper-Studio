//! Opt-in coupled placement/road search. Every world starts from immutable
//! native terrain; placement preferences are never modified in the saved recipe.
use crate::{
    Presence, Project, composition, native_processing::Source, prefab_assets::Catalog,
    prefab_textures::Bindings, road_generation,
};
use papyrus_terrain::placement::Solution;
use std::path::Path;

fn engine(project: &Project) -> Option<&str> {
    project
        .roads
        .as_ref()
        .and_then(|r| r.staged.as_ref())
        .map(|p| p.engine.as_str())
        .filter(|engine| {
            matches!(
                *engine,
                road_generation::LEGACY_COUPLED_VERSION
                    | road_generation::PROPOSAL_COUPLED_VERSION
                    | road_generation::GRID_COUPLED_VERSION
                    | road_generation::COUPLED_VERSION
                    | road_generation::SOURCE_COUPLED_VERSION
                    | road_generation::CONNECTED_COUPLED_VERSION
            )
        })
}
pub fn enabled(project: &Project) -> bool {
    engine(project).is_some()
}

fn supported(project: &Project) -> Result<(), String> {
    project.validate()?;
    let roads = project
        .roads
        .as_ref()
        .ok_or("Missing coupled road recipe")?;
    if !enabled(project)
        || !project.performance.monuments
        || !crate::monument_source::supported(&project.placement_engine)
        || !crate::placement_preview::is_balanced(&project.placement_engine)
        || project
            .monument_rules
            .as_ref()
            .is_none_or(|r| r.canonical_yaw.is_none())
        || roads
            .entrance_launch
            .is_none_or(|p| p.engine != papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)
        || roads
            .staged
            .as_ref()
            .and_then(|s| s.layout.as_ref())
            .is_none_or(|l| l.ring_clearance.is_none() && l.terrain_entrances.is_none())
    {
        return Err("Coupled roads require source-bound native placement, canonical yaw and shared ordered road planning".into());
    }
    if project.rails.is_some()
        && (engine(project).is_none_or(|engine| !road_generation::is_source_coupled_engine(engine))
            || !crate::rail_generation::before_roads(project))
    {
        return Err(
            "Coupled recovery requires the source road contract and rails generated before roads"
                .into(),
        );
    }
    Ok(())
}

/// Numerical/texture preparation reservation for ONE exact placement trial and
/// complete monument restamp. Existing I/O snapshot/hash bounds still apply.
/// Full placement keeps its own initial search caps; retry work shares roads'
/// existing total. No cap is increased and failed trial work is not refunded.
pub(crate) fn rebuild_bound(
    source: &Source,
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    rail_approaches: usize,
) -> Result<u64, String> {
    let mut bytes = 0_u64;
    for outcome in solution.outcomes.iter().filter(|o| o.position.is_some()) {
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Unknown recovery target")?;
        let asset = target
            .prefab_path
            .as_ref()
            .and_then(|p| catalog.entries.get(p))
            .ok_or("Missing recovery modifier asset")?;
        if composition::omit_terrain_modifier(project, asset, outcome.position.as_ref().unwrap())? {
            continue;
        }
        // Aliased enabled texture references are counted separately, conservatively.
        for name in composition::enabled_texture_keys(asset)? {
            let guid = asset
                .texture_guids
                .get(name)
                .ok_or("Missing enabled recovery texture")?;
            let texture = bindings
                .textures()
                .get(guid)
                .ok_or("Missing recovery texture binding")?;
            bytes = bytes
                .checked_add(
                    texture
                        .bytes
                        .checked_add(texture.storage.as_ref().map_or(texture.bytes, |s| s.bytes))
                        .ok_or("Recovery texture work overflow")?,
                )
                .ok_or("Recovery texture work overflow")?;
        }
    }
    // The preparation allowance covers bounded source metadata/packing and boat
    // setup. Explicit existing primitive ceilings cover eligibility, projected
    // boat sampling, earthwork and height/surface stamp iteration respectively.
    let fixed = 2_000_000_000_u64
        + crate::tunnel_generation::rebuild_work_bound()
        + 64_000_000
        + 64_000_000
        + papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES
        + 128_000_000;
    let rail_work = rail_rebuild_bound(project, rail_approaches)?;
    fixed
        .checked_add(source.layers.height.len() as u64 * 8)
        .and_then(|n| n.checked_add(source.layers.topology.len() as u64 * 32))
        // At most two loads of any referenced image: exact trial and restamp.
        // Each load retains its stored/decoded length and hash validation.
        .and_then(|n| bytes.checked_mul(8).and_then(|b| n.checked_add(b)))
        .and_then(|n| n.checked_add(rail_work))
        .ok_or_else(|| "Recovery preparation work overflow".into())
}

/// Reserve the complete pre-road rail preparation and final rail work for every
/// restamped world. Per-primitive grants keep their existing ceilings. The
/// approach count is fixed by the unchanged roster and verified connectors.
/// Branches/sidings meter their own path construction.
fn rail_rebuild_bound(project: &Project, approaches: usize) -> Result<u64, String> {
    if approaches > 256 {
        return Err("Invalid recovery rail approach count".into());
    }
    let Some(recipe) = &project.rails else {
        return Ok(0);
    };
    if !crate::rail_generation::before_roads(project) {
        return Err("Recovery cannot rebuild rails generated after roads".into());
    }
    let settings = recipe.settings()?;
    let bed = settings.bed;
    let branches = bed.branching.ok_or("Missing recovery rail branches")?;
    let sidings = bed.siding.ok_or("Missing recovery rail sidings")?;
    let corridor = bed.corridor.ok_or("Missing recovery rail corridor")?;
    let meshes = bed.meshes.ok_or("Missing recovery rail meshes")?;
    let initial_tracks = approaches as u64 + 1;
    let tracks = initial_tracks + branches.maximum_branches as u64 + sidings.maximum_sidings as u64;
    // Blockers and two source-cost grids have separate 128M grants. Fixed
    // terrain scans, layout serialization and ocean reconstruction are covered
    // by the enclosing monument/restamp reservation's raster allowances.
    let work = 3 * 128_000_000_u64
        + initial_tracks * 32_000_000 // ring plus bounded approach Path::from_controls calls
        + tracks * 1_000_000 // independent continuous grade audit per track
        + settings.ring.maximum_total_expansions as u64
        + settings.approaches.maximum_total_expansions as u64
        + settings.approaches.maximum_geometry_work as u64
        + bed.maximum_work as u64
        + branches.maximum_work as u64
        + branches.maximum_expansions as u64
        + sidings.maximum_work as u64
        + bed.reservation_work.ok_or("Missing recovery rail reservations")? as u64
        + corridor.maximum_work as u64 * 2 // pre-object and final shared audits
        + 512_000_000 // deferred texture stage after roads
        + meshes.maximum_work as u64
        + bed.gameplay_height_work.ok_or("Missing recovery rail height allowance")? as u64;
    Ok(work)
}

fn trial_recipe(
    project: &Project,
    current: &Solution,
    canonical: &Solution,
    monument: u64,
    attempt: usize,
    cell: f32,
    proposal: Option<crate::placement_preview::RawProposal>,
) -> Result<Project, String> {
    let target = project
        .targets
        .iter()
        .find(|t| t.instance_id == monument)
        .ok_or("Unknown recovery monument")?;
    if target.exact || target.presence == Presence::Excluded {
        return Err("Cannot recover an explicitly pinned or excluded monument".into());
    }
    let origin = canonical
        .outcomes
        .iter()
        .find(|o| o.id == monument)
        .and_then(|o| o.position.as_ref())
        .ok_or("Missing canonical recovery pose")?;
    let offset = match attempt {
        0 => [cell * 0.5, 0.],
        1 => [0., cell * 0.5],
        _ => return Err("Coupled placement trial limit reached".into()),
    };
    let mut trial = project.clone();
    for target in &mut trial.targets {
        // Keep every presence/size/priority entry, including currently unplaced
        // ones, so original per-target earthwork/nav quotas are not enlarged.
        target.exact = true;
        if let Some(position) = current
            .outcomes
            .iter()
            .find(|o| o.id == target.instance_id)
            .and_then(|o| o.position.as_ref())
        {
            target.x = position.x;
            target.z = position.z;
            target.yaw = Some(position.yaw);
        }
        if target.instance_id == monument {
            target.x = proposal.map_or(origin.x, |p| p.x) + offset[0];
            target.z = proposal.map_or(origin.z, |p| p.z) + offset[1];
            let yaw = proposal.map_or(origin.yaw, |p| p.yaw).rem_euclid(360.);
            // Source proposal ranges can be negative. The exact-trial recipe
            // uses the same canonical angle the native evaluator would use.
            target.yaw = Some(if yaw == 0. || yaw >= 360. { 0. } else { yaw });
        }
    }
    Ok(trial)
}

fn merge_trial(
    project: &Project,
    current: &Solution,
    assessed: &Solution,
    monument: u64,
    origin: &Solution,
    cell: f32,
) -> Result<Solution, String> {
    if current.outcomes.len() != assessed.outcomes.len()
        || current.required_missing != assessed.required_missing
        || current.outcomes.iter().any(|old| {
            assessed
                .outcomes
                .iter()
                .find(|o| o.id == old.id)
                .is_none_or(|new| old.id != monument && old.position != new.position)
        })
    {
        return Err(
            "Exact recovery failed to preserve the canonical roster and other poses".into(),
        );
    }
    let assessment = assessed
        .outcomes
        .iter()
        .find(|o| o.id == monument)
        .ok_or("Missing assessed replacement")?;
    let accepted = assessment.position.as_ref().ok_or_else(|| {
        format!(
            "Source checks rejected the replacement site: {}",
            assessment.reason
        )
    })?;
    let original = origin
        .outcomes
        .iter()
        .find(|o| o.id == monument)
        .and_then(|o| o.position.as_ref())
        .ok_or("Missing original recovery site")?;
    if [
        accepted.x,
        accepted.y,
        accepted.z,
        accepted.yaw,
        accepted.clearance,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || (accepted.x - original.x).hypot(accepted.z - original.z) > cell
    {
        return Err("Source decorators moved the trial beyond the local recovery radius".into());
    }
    let target = project
        .targets
        .iter()
        .find(|t| t.instance_id == monument)
        .ok_or("Missing recipe target")?;
    let distance = (accepted.x - target.x).hypot(accepted.z - target.z);
    if let Some(limit) = target.max_displacement_m && distance > limit as f32 {
        return Err(format!("Entrance recovery exceeds the {limit} m monument move limit"));
    }
    let mut result = current.clone();
    let outcome = result
        .outcomes
        .iter_mut()
        .find(|o| o.id == monument)
        .ok_or("Missing recovery outcome")?;
    outcome.position = Some(accepted.clone());
    outcome.distance = Some(distance);
    outcome.within_radius = distance <= target.radius;
    outcome.reason = "Bounded entrance recovery; fresh source eligibility, earthwork and applicable coastal checks passed".into();
    result.attempts = result.attempts.saturating_add(assessed.attempts);
    Ok(result)
}

pub(crate) fn compose(
    source: &Source,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    worker: Option<&Path>,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<composition::ResolvedLayout>, String> {
    let mut timing = composition::StageTimer::start("world_recovery");
    supported(project)?;
    if source.key != crate::native_preview::processing_key(&project.terrain_key()) {
        return Err("Coupled recovery source key differs from the recipe".into());
    }
    let default_worker;
    let worker = match worker {
        Some(path) => path,
        None => {
            default_worker = crate::native_preview::worker_path()?;
            &default_worker
        }
    };
    // Canonical placement is derived afresh from recipe/seeds, never from an
    // earlier recovered display solution. It retains all existing placement caps.
    let (canonical, proposals) = if engine(project) != Some(road_generation::LEGACY_COUPLED_VERSION)
    {
        let Some(canonical) = crate::terrain_stages::solve(
            source,
            project,
            catalog,
            bindings,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        (canonical.solution, Some(canonical.proposals))
    } else {
        let Some(canonical) = crate::placement_preview::solve_native(
            project,
            &source.layers,
            catalog,
            bindings,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        (canonical, None)
    };
    composition::record_timing(&mut timing, "canonical_monuments");
    let mut solution = canonical.clone();
    let mut session = road_generation::Session::new(
        road_generation::Context {
            project,
            solution: &solution,
            catalog,
            bindings,
            worker,
        },
        crate::rail_generation::before_roads(project),
    );
    let rail_approaches = crate::rail_generation::rebuild_approach_limit(
        project, &solution, catalog, bindings, &cancelled,
    )?;
    let work = rebuild_bound(
        source,
        project,
        &solution,
        catalog,
        bindings,
        rail_approaches,
    )?;
    composition::record_timing(&mut timing, "road_session");
    let cell = source.layers.size as f32 / (source.layers.height_resolution - 1) as f32;
    let mut attempts = Vec::new();
    let mut rejected_worlds = Vec::new();
    let mut guidance_source = None;
    let mut placement_trials = 0;
    let mut world_attempts = 0;
    loop {
        if cancelled() {
            return Ok(None);
        }
        world_attempts += 1;
        let result = composition::compose_layout_with_session(
            source,
            project,
            &solution,
            catalog,
            bindings,
            Some(worker),
            &cancelled,
            Some(&mut session),
        );
        if let Err(composition::Failure::Roads(error)) = &result {
            rejected_worlds.push(
                serde_json::json!({"world_attempt":world_attempts,"feedback":error.feedback,"unconnected_entrances":error.entrances}),
            );
        }
        match result {
            Ok(Some(mut maps)) => {
                if let Some(surfaces) = maps.roads.as_mut().and_then(|r| r.surfaces.as_mut()) {
                    surfaces.report["coupled_placement"] = serde_json::json!({
                        "engine":engine(project),"canonical_layout":crate::composition::layout_fingerprint(&canonical).iter().map(|b|format!("{b:02x}")).collect::<String>(),
                        "rebuild_reservation_per_attempt":work,"world_attempts":world_attempts,"rejected_worlds":rejected_worlds,"attempts":attempts,
                        "terrain_source_unchanged":true,"full_map_validated":false});
                }
                return Ok(Some(composition::ResolvedLayout { base: None, maps, solution, cables: Ok(None) }));
            }
            Ok(None) => return Ok(None),
            Err(composition::Failure::Roads(error)) if session.can_retry() => {
                let monument = error.movable_monument(project);
                if let Some(monument) = monument.filter(|_| placement_trials < 2) {
                    // Failed source candidates must not consume another complete
                    // road layout or its geometry proposal allowance. Each exact
                    // assessment still reserves its entire preparation bound.
                    while placement_trials < 2 {
                        if !session.reserve_world_rebuild(work)? {
                            return Err(format!(
                                "Coupled recovery has no remaining rebuild allowance: {}",
                                error.message
                            ));
                        }
                        let trial_index = placement_trials;
                        placement_trials += 1;
                        let original_proposal = proposals
                            .as_ref()
                            .map(|p| {
                                p.get(&monument)
                                    .copied()
                                    .ok_or("Missing canonical raw proposal")
                            })
                            .transpose()?;
                        let mut trial = trial_recipe(
                            project,
                            &solution,
                            &canonical,
                            monument,
                            trial_index,
                            cell,
                            original_proposal,
                        )?;
                        let mut guidance = serde_json::Value::Null;
                        if matches!(
                            engine(project),
                            Some(
                                road_generation::GRID_COUPLED_VERSION
                                    | road_generation::COUPLED_VERSION
                                    | road_generation::SOURCE_COUPLED_VERSION
                                    | road_generation::CONNECTED_COUPLED_VERSION
                            )
                        ) {
                            if guidance_source.is_none() {
                                guidance_source = Some(crate::monument_source::Source::cached_shared(
                                    project
                                        .monument_rules
                                        .as_ref()
                                        .ok_or("Missing source rules")?,
                                    catalog,
                                    bindings,
                                    &cancelled,
                                )?);
                            }
                            let guided = (|| {
                                let key = error
                                    .feedback
                                    .iter()
                                    .filter_map(|m| m.terminal)
                                    .chain(error.entrances.iter().copied())
                                    .find(|k| k.monument == monument)
                                    .ok_or("Missing rejected connector")?;
                                let path = project
                                    .targets
                                    .iter()
                                    .find(|t| t.instance_id == monument)
                                    .and_then(|t| t.prefab_path.as_ref())
                                    .ok_or("Missing connector prefab")?;
                                let metadata = guidance_source.as_ref().unwrap();
                                let attributes = metadata
                                    .prefabs
                                    .get(path)
                                    .and_then(|p| p.placement_attributes.as_ref())
                                    .ok_or("Missing connector attributes")?;
                                let canonical_pose = canonical
                                    .outcomes
                                    .iter()
                                    .find(|o| o.id == monument)
                                    .and_then(|o| o.position.as_ref())
                                    .ok_or("Missing canonical connector pose")?;
                                crate::entrance_grid::propose(
                                    crate::entrance_grid::Context {
                                        layers: &source.layers,
                                        attributes,
                                        profile: metadata
                                            .coastal_profile
                                            .as_ref()
                                            .ok_or("Missing coastal profile")?,
                                        recipe: project.monument_rules.as_ref().unwrap(),
                                        canonical: canonical_pose,
                                        raw: original_proposal
                                            .ok_or("Missing original connector proposal")?,
                                        connector: key.connector,
                                    },
                                    trial_index,
                                    matches!(
                                        engine(project),
                                        Some(
                                            road_generation::COUPLED_VERSION
                                                | road_generation::SOURCE_COUPLED_VERSION
                                                | road_generation::CONNECTED_COUPLED_VERSION
                                        )
                                    ),
                                )
                            })();
                            match guided {
                                Ok(proposal) => {
                                    let target = trial
                                        .targets
                                        .iter_mut()
                                        .find(|t| t.instance_id == monument)
                                        .unwrap();
                                    target.x = proposal.raw.x;
                                    target.z = proposal.raw.z;
                                    guidance = proposal.audit;
                                }
                                Err(error) => {
                                    guidance =
                                        serde_json::json!({"fallback":"half-cell","reason":error})
                                }
                            }
                        }
                        let Some(assessed) = crate::placement_preview::solve_native_replacement(
                            &trial,
                            &source.layers,
                            catalog,
                            bindings,
                            &solution,
                            monument,
                            &cancelled,
                        )?
                        else {
                            return Ok(None);
                        };
                        let accepted =
                            merge_trial(project, &solution, &assessed, monument, &canonical, cell)
                                .and_then(|replacement| {
                                    session.placement_changed(monument, &replacement)?;
                                    Ok(replacement)
                                });
                        match accepted {
                            Ok(replacement) => {
                                let pose=replacement.outcomes.iter().find(|o|o.id==monument).and_then(|o|o.position.as_ref()).map(|p|[p.x,p.y,p.z,p.yaw]);
                                solution = replacement;
                                attempts.push(serde_json::json!({"attempt":trial_index,"after_world_attempt":world_attempts,"monument":monument,"accepted_source_trial":true,"position":pose,"original_proposal":original_proposal.map(|p|[p.x,p.z,p.yaw]),"grid_guidance":guidance}));
                                break;
                            }
                            Err(reason) => attempts.push(serde_json::json!({"attempt":trial_index,"after_world_attempt":world_attempts,"monument":monument,"accepted_source_trial":false,"reason":reason,"grid_guidance":guidance})),
                        }
                    }
                } else {
                    if !session.reserve_world_rebuild(work)? {
                        return Err(format!(
                            "Coupled recovery has no remaining rebuild allowance: {}",
                            error.message
                        ));
                    }
                    attempts.push(serde_json::json!({"after_world_attempt":world_attempts,"accepted_source_trial":false,"reason":"No remaining movable source trial; retry routing only"}));
                }
            }
            Err(error) => {
                return Err(format!(
                    "Coupled monument/road recovery rejected: {}",
                    serde_json::json!({"world_attempts":world_attempts,"rebuild_reservation_per_attempt":work,"rejected_worlds":rejected_worlds,"attempts":attempts,"error":error.message()})
                ));
            }
        }
    }
}

/// Owned-data diagnostic: an unchanged proposal must reproduce its accepted
/// pose. The comparison replay deliberately uses the old, already-fitted frame.
/// This runs bounded native placement, not a complete world or physics audit.
#[cfg(feature = "road-diagnostics")]
pub fn verify_proposal_replay(
    source: &Source,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    monument: u64,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<serde_json::Value, String> {
    let canonical = crate::placement_preview::solve_native_canonical(
        project,
        &source.layers,
        catalog,
        bindings,
        &cancelled,
    )?
    .ok_or("Proposal replay cancelled")?;
    let proposal = *canonical
        .proposals
        .get(&monument)
        .ok_or("Missing replay proposal")?;
    let expected = canonical
        .solution
        .outcomes
        .iter()
        .find(|o| o.id == monument)
        .and_then(|o| o.position.as_ref())
        .ok_or("Missing replay pose")?;
    let coordinates = |p: &papyrus_terrain::placement::Position| [p.x, p.y, p.z, p.yaw, p.clearance];
    let mut rows = Vec::new();
    for (label, input) in [
        ("original-proposal", Some(proposal)),
        ("already-fitted", None),
    ] {
        let trial = trial_recipe(
            project,
            &canonical.solution,
            &canonical.solution,
            monument,
            0,
            0.,
            input,
        )?;
        let result = crate::placement_preview::solve_native_replacement(
            &trial,
            &source.layers,
            catalog,
            bindings,
            &canonical.solution,
            monument,
            &cancelled,
        )?
        .ok_or("Proposal replay cancelled")?;
        let outcome = result
            .outcomes
            .iter()
            .find(|o| o.id == monument)
            .ok_or("Missing replay outcome")?;
        let exact = outcome.position.as_ref().is_some_and(|p| {
            coordinates(p).map(f32::to_bits) == coordinates(expected).map(f32::to_bits)
        });
        rows.push(serde_json::json!({"input_frame":label,"pose":outcome.position.as_ref().map(coordinates),"exact":exact,"reason":outcome.reason}));
    }
    Ok(
        serde_json::json!({"format":"mapper-native-proposal-replay-1","monument":monument,
        "original_proposal":[proposal.x,proposal.z,proposal.yaw],"canonical_pose":coordinates(expected),
        "replays":rows,"full_map_validated":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::placement::{Outcome, Position};
    fn layout(project: &Project) -> Solution {
        Solution {
            outcomes: project
                .targets
                .iter()
                .map(|t| Outcome {
                    id: t.instance_id,
                    position: Some(Position {
                        x: t.x,
                        y: 10.,
                        z: t.z,
                        yaw: 20.,
                        clearance: 10.,
                    }),
                    distance: Some(0.),
                    within_radius: true,
                    reason: "Canonical source checks".into(),
                })
                .collect(),
            required_missing: 0,
            attempts: 8,
        }
    }
    #[test]
    fn exact_trials_keep_original_content_quotas_and_anchor_offsets_to_canonical_pose() {
        let project = Project::default();
        let canonical = layout(&project);
        let mut current = canonical.clone();
        current.outcomes[0].position.as_mut().unwrap().x += 0.5;
        current.outcomes[1].position = None;
        let trial = trial_recipe(&project, &current, &canonical, 1, 1, 1., None).unwrap();
        assert!(trial.targets.iter().all(|t| t.exact));
        assert_eq!(trial.targets[0].x, project.targets[0].x);
        assert_eq!(trial.targets[0].z, project.targets[0].z + 0.5);
        for (old, new) in project.targets.iter().zip(&trial.targets) {
            assert_eq!(old.presence, new.presence);
            assert_eq!(old.prefab_path, new.prefab_path);
            assert_eq!(old.radius, new.radius);
            assert_eq!(old.strength, new.strength);
        }
        assert_eq!(trial.targets[1].x, project.targets[1].x);
        assert!(trial_recipe(&project, &current, &canonical, 1, 2, 1., None).is_err());
        let mut pinned = project.clone();
        pinned.targets[0].exact = true;
        assert!(trial_recipe(&pinned, &current, &canonical, 1, 0, 1., None).is_err());
        assert_eq!(canonical, layout(&project));
    }
    #[test]
    fn source_decorators_receive_the_original_proposal_not_their_fitted_output() {
        let project = Project {
            placement_engine: crate::placement_preview::ROTATION_VERSION.into(),
            ..Default::default()
        };
        let mut canonical = layout(&project);
        let fitted = canonical.outcomes[0].position.as_mut().unwrap();
        fitted.x = 100.;
        fitted.z = 200.;
        fitted.yaw = 135.;
        let raw = crate::placement_preview::RawProposal {
            x: 20.,
            z: 30.,
            yaw: 45.,
        };
        let replay = trial_recipe(&project, &canonical, &canonical, 1, 0, 0., Some(raw)).unwrap();
        assert_eq!(
            [
                replay.targets[0].x,
                replay.targets[0].z,
                replay.targets[0].yaw.unwrap()
            ],
            [20., 30., 45.]
        );
        let moved = trial_recipe(&project, &canonical, &canonical, 1, 1, 1., Some(raw)).unwrap();
        assert_eq!(
            [
                moved.targets[0].x,
                moved.targets[0].z,
                moved.targets[0].yaw.unwrap()
            ],
            [20., 30.5, 45.]
        );
        let legacy = trial_recipe(&project, &canonical, &canonical, 1, 0, 0., None).unwrap();
        assert_eq!(
            [
                legacy.targets[0].x,
                legacy.targets[0].z,
                legacy.targets[0].yaw.unwrap()
            ],
            [100., 200., 135.]
        );
        let negative = crate::placement_preview::RawProposal { yaw: -315., ..raw };
        let normalized =
            trial_recipe(&project, &canonical, &canonical, 1, 0, 0., Some(negative)).unwrap();
        assert_eq!(normalized.targets[0].yaw, Some(45.));
        normalized.validate().unwrap();
    }
    #[test]
    fn trial_merge_preserves_other_outcomes_and_reports_distance_from_saved_preferences() {
        let project = Project::default();
        let canonical = layout(&project);
        let mut assessed = canonical.clone();
        assessed.outcomes[0].position.as_mut().unwrap().x += 0.5;
        assessed.outcomes[1].reason = "Internal pin assessment".into();
        let merged = merge_trial(&project, &canonical, &assessed, 1, &canonical, 1.).unwrap();
        assert_eq!(merged.outcomes[0].distance, Some(0.5));
        assert_eq!(merged.outcomes[1], canonical.outcomes[1]);
        assert_eq!(merged.attempts, 16);
        assert_eq!(project, Project::default());
        for case in 0..5 {
            let mut bad = assessed.clone();
            match case {
                0 => bad.outcomes[1].position = None,
                1 => bad.outcomes[1].position.as_mut().unwrap().x += 1.,
                2 => bad.outcomes[0].position.as_mut().unwrap().x += 2.,
                3 => bad.outcomes[0].position.as_mut().unwrap().x = f32::NAN,
                _ => bad.required_missing = 1,
            }
            assert!(
                merge_trial(&project, &canonical, &bad, 1, &canonical, 1.).is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn legacy_recipes_do_not_enable_coupled_recovery() {
        let project = Project::default();
        assert!(!enabled(&project));
        assert!(supported(&project).is_err());
    }
    #[test]
    fn rail_rebuild_reservation_covers_each_independent_grant_and_rejects_late_rails() {
        let mut project = Project::default();
        assert_eq!(rail_rebuild_bound(&project, 0).unwrap(), 0);
        project.rails = Some(crate::rail_generation::Recipe {
            engine: crate::rail_generation::BEFORE_ROADS_VERSION.into(),
            source_sha256: "a".repeat(64),
            mesh_sha256: "b".repeat(64),
            settings: crate::rail_generation::initial_settings(1),
        });
        let baseline = rail_rebuild_bound(&project, 4).unwrap();
        assert!(baseline > 5 * 32_000_000);
        assert!(baseline < 16_000_000_000);
        assert_eq!(
            rail_rebuild_bound(&project, 5).unwrap(),
            baseline + 33_000_000
        );
        assert!(rail_rebuild_bound(&project, 257).is_err());
        let settings = &mut project.rails.as_mut().unwrap().settings;
        settings["approaches"]["maximum_geometry_work"] = 32_000_001.into();
        assert_eq!(rail_rebuild_bound(&project, 4).unwrap(), baseline + 1);
        project.rails.as_mut().unwrap().settings["bed"]["branching"]["maximum_branches"] =
            65.into();
        assert_eq!(
            rail_rebuild_bound(&project, 4).unwrap(),
            baseline + 1_000_001
        );
        project.rails.as_mut().unwrap().settings["bed"]["reservation_work"] = 0.into();
        assert!(rail_rebuild_bound(&project, 4).is_err());
        project.rails.as_mut().unwrap().engine = crate::rail_generation::VERSION.into();
        assert!(rail_rebuild_bound(&project, 4).is_err());
    }
}
