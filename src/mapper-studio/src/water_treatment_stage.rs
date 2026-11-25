//! Atomic pipe placement over an accepted road-earthwork revision.
use crate::{
    Project,
    monument_tag_geometry::{Placement, Registry},
    monument_tag_source, prefab_export, roadside_stamps, water_treatment_source,
};
use papyrus_terrain::{
    decor_fit::{self, ValidatedWater},
    decor_live,
    decor_stream::Budget,
    environment::{Coverage, Verdict},
    footprints,
    height_reservations::HeightReservations,
    native_layers::Layers,
    placement::Solution,
    roadside_decor, roadside_live,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod commit;
pub use commit::{Boundary, Commit, PreparedSources, receipt_records, reserve_committed, reserve_heights, validate_committed};

pub const VERSION: &str = "mapper-authored-water-treatment-1";
pub const MONUMENT_TOKEN: &str = "water_treatment_plant";

/// Independent Mapper fitting policy, retained in every stage receipt.
/// Source programs continue to supply distances, filters, slope and padding.
pub const SPACING_FRACTION: f32 = 0.25;
pub const ANCHOR_FILTER_MODE: i32 = 2;

pub struct Input<'a> {
    pub layers: &'a Layers,
    pub alpha: &'a [u8],
    pub roads: &'a [roadside_decor::Road<'a>],
    pub source: &'a water_treatment_source::Source,
    pub registry: &'a Registry,
    pub known_topology: u32,
    pub protected_topology: u32,
    pub water: Option<ValidatedWater<'a>>,
    pub height_reservations: Option<&'a HeightReservations>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Settings {
    pub seed: u32,
    pub realm_mask: u32,
    pub maximum_terrain_work: usize,
    pub maximum_sampling_attempts: usize,
    pub maximum_fitting_samples: usize,
    pub maximum_clearance_work: usize,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        footprints::ObjectPolicy {
            maximum_work: self.maximum_terrain_work, role_exceptions: 0, material_role_exceptions: 0,
        }.validate()?;
        Budget::new(self.maximum_sampling_attempts)?;
        Budget::new(self.maximum_clearance_work)?;
        if self.realm_mask == 0 || !(4..=decor_fit::MAXIMUM_FITTING_SAMPLES)
            .contains(&(self.maximum_fitting_samples as u64)) {
            return Err("Invalid pipe realm or fitting allowance".into());
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub source_sha256: String,
    pub settings: Settings,
    pub spacing_fraction: f32,
    pub anchor_filter_mode: i32,
    pub monument_token: &'static str,
    pub eligible_monuments: usize,
    pub tagged_volumes: usize,
    pub geometry_engine: &'static str,
    pub skipped: Option<&'static str>,
    pub programs: Vec<roadside_live::Report>,
    pub sampling_work: usize,
    pub fitting_samples: usize,
    pub clearance_work: usize,
    pub admitted_instances: usize,
    pub retained_runtime_holes: usize,
    pub alpha_modified: bool,
    pub physics_validated: bool,
}

pub struct Output {
    /// Absent when no water-treatment monument enables the stage.
    pub terrain: Option<footprints::Output>,
    pub report: Report,
}

/// Export the complete prefab roots so their authored runtime holes travel with
/// the map. The container exporter supplies the verified installation identities.
pub fn prefab_records(
    report: &Report,
    resolve: impl Fn(&str) -> Option<u32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<papyrus_terrain::prefab_records::Record>>, String> {
    if cancelled() {
        return Ok(None);
    }
    if report.engine != VERSION
        || report.admitted_instances > papyrus_terrain::content::MAX_PREFABS
        || report.programs.iter().try_fold(0usize, |count, program| {
            count.checked_add(program.admitted.len())
        }) != Some(report.admitted_instances)
        || (report.skipped.is_some() && report.admitted_instances != 0)
    {
        return Err("Pipe export differs from its completed admission receipt".into());
    }
    let instances = report
        .programs
        .iter()
        .flat_map(|program| &program.admitted)
        .map(|instance| papyrus_terrain::prefab_records::Instance {
            path: &instance.prefab,
            category: "Monument",
            position: instance.transform.position,
            rotation: instance.transform.rotation,
            scale: instance.transform.scale,
        })
        .collect::<Vec<_>>();
    papyrus_terrain::prefab_records::assemble(&instances, resolve, cancelled)
}

/// Register every accepted primary and roadside prefab root at its export pose.
pub fn registry(
    project: &Project,
    solution: &Solution,
    roadside: &[roadside_stamps::Placement],
    source: &monument_tag_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Registry>, String> {
    if cancelled() {
        return Ok(None);
    }
    let placements = placements(project, solution, roadside)?;
    Registry::assemble(source.prefabs(), &placements, MONUMENT_TOKEN, cancelled)
}

pub(crate) fn placements<'a>(
    project: &'a Project,
    solution: &Solution,
    roadside: &'a [roadside_stamps::Placement],
) -> Result<Vec<Placement<'a>>, String> {
    if solution.outcomes.len() != project.targets.len()
        || project.targets.len() > papyrus_terrain::placement::MAX_TARGETS
        || roadside.len() > papyrus_terrain::placement::MAX_TARGETS - project.targets.len()
    {
        return Err("Pipe monument roster differs from the accepted layout".into());
    }
    let targets = project
        .targets
        .iter()
        .map(|target| (target.instance_id, target))
        .collect::<BTreeMap<_, _>>();
    if targets.len() != project.targets.len() {
        return Err("Duplicate pipe monument target".into());
    }
    let mut seen = BTreeSet::new();
    let mut placements = Vec::with_capacity(solution.outcomes.len() + roadside.len());
    for outcome in &solution.outcomes {
        if !seen.insert(outcome.id) {
            return Err("Duplicate pipe monument outcome".into());
        }
        let target = targets
            .get(&outcome.id)
            .ok_or("Unknown pipe monument outcome")?;
        if let Some(position) = &outcome.position {
            placements.push(Placement {
                instance: outcome.id,
                prefab: target
                    .prefab_path
                    .as_deref()
                    .ok_or("Placed monument lacks a prefab")?,
                pose: prefab_export::monument_pose(
                    [position.x, position.y, position.z],
                    position.yaw,
                )?,
            });
        }
    }
    for placement in roadside {
        if !seen.insert(placement.request) {
            return Err("Duplicate primary or roadside pipe monument identity".into());
        }
        placements.push(Placement {
            instance: placement.request,
            prefab: &placement.prefab,
            pose: prefab_export::monument_pose(placement.position, placement.yaw)?,
        });
    }
    Ok(placements)
}

pub fn generate(
    input: Input<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    let policy = footprints::ObjectPolicy {
        maximum_work: settings.maximum_terrain_work,
        role_exceptions: 0,
        material_role_exceptions: 0,
    };
    settings.validate()?;
    let mut attempts = Budget::new(settings.maximum_sampling_attempts)?;
    let mut clearance_work = Budget::new(settings.maximum_clearance_work)?;
    let eligible = input.registry.matched_monuments().len();
    let mut report = Report {
        engine: VERSION,
        source_sha256: input.source.sha256().into(),
        settings,
        spacing_fraction: SPACING_FRACTION,
        anchor_filter_mode: ANCHOR_FILTER_MODE,
        monument_token: MONUMENT_TOKEN,
        eligible_monuments: eligible,
        tagged_volumes: input.registry.boxes().len(),
        geometry_engine: crate::monument_tag_geometry::AUTHORED_VERSION,
        skipped: (eligible == 0).then_some("No water-treatment monument is placed"),
        programs: Vec::new(),
        sampling_work: 0,
        fitting_samples: 0,
        clearance_work: 0,
        admitted_instances: 0,
        retained_runtime_holes: 0,
        alpha_modified: false,
        physics_validated: false,
    };
    if eligible == 0 {
        return Ok(Some(Output {
            terrain: None,
            report,
        }));
    }
    if input.registry.coverage() != Coverage::Complete {
        return Err("Pipe placement requires complete monument tag coverage".into());
    }
    let fitting = input
        .source
        .prefabs()
        .iter()
        .map(|(path, prefab)| (path.clone(), prefab.fitting.clone()))
        .collect();
    let terrain = input
        .source
        .prefabs()
        .iter()
        .map(|(path, prefab)| (path.clone(), prefab.terrain.clone()))
        .collect();
    let Some(mut transaction) = footprints::RoadTransaction::new_objects_with_policy(
        input.layers,
        input.alpha,
        terrain,
        input.known_topology,
        input.protected_topology,
        policy,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    if let Some(reservations) = input.height_reservations
        && !transaction.set_height_reservations(reservations, &cancelled)?
    {
        return Ok(None);
    }
    let mut fitter = decor_fit::OrientedBudget::new(input.layers, input.alpha)?
        .with_sample_limit(settings.maximum_fitting_samples)?;
    let mut next_pose = 0;
    for (index, program) in input.source.programs().iter().enumerate() {
        let descriptor = program.descriptor(index, ANCHOR_FILTER_MODE)?;
        let clearance = input.registry.clearance(program.prevent_building_padding)?;
        let Some(emission) = roadside_live::emit(
            roadside_live::Request {
                stage: &descriptor,
                sampling: program.sampling(SPACING_FRACTION)?,
                roads: input.roads,
                seed: settings.seed,
                realm_mask: settings.realm_mask,
            },
            decor_live::Context {
                transaction: &mut transaction,
                alpha: input.alpha,
                fitting: &mut fitter,
                prefabs: &fitting,
                attempts: &mut attempts,
                next_pose: &mut next_pose,
                water: input.water,
            },
            |pose, view| {
                if !program.accepts_slope(view, pose.position)? {
                    return Ok(false);
                }
                match clearance
                    .check(pose.position, &mut clearance_work, &cancelled)?
                    .verdict
                {
                    Verdict::Clear => Ok(true),
                    Verdict::Blocked(_) => Ok(false),
                    verdict => Err(format!("Pipe clearance did not complete: {verdict:?}")),
                }
            },
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        report.admitted_instances += emission.admitted.len();
        if report.admitted_instances > papyrus_terrain::content::MAX_PREFABS {
            return Err("Pipe stage exceeds the map prefab allowance".into());
        }
        for instance in &emission.admitted {
            report.retained_runtime_holes += input
                .source
                .prefabs()
                .get(&instance.prefab)
                .ok_or("Unknown admitted pipe prefab")?
                .runtime_holes
                .len();
        }
        report.programs.push(emission);
    }
    report.sampling_work = attempts.used();
    report.fitting_samples = fitter.samples();
    report.clearance_work = clearance_work.used();
    let Some(terrain) = transaction.finish(&cancelled)? else {
        return Ok(None);
    };
    Ok((!cancelled()).then_some(Output {
        terrain: Some(terrain),
        report,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::placement::{Outcome, Position};

    fn layout() -> (Project, Solution, Vec<roadside_stamps::Placement>) {
        let mut project = Project::default();
        project.targets.truncate(2);
        for (index, target) in project.targets.iter_mut().enumerate() {
            target.prefab_path = Some(format!("assets/test/monument-{index}.prefab"));
        }
        let solution = Solution {
            outcomes: project
                .targets
                .iter()
                .enumerate()
                .map(|(index, target)| Outcome {
                    id: target.instance_id,
                    position: (index == 0).then_some(Position {
                        x: 100.,
                        y: 20.,
                        z: -40.,
                        yaw: 37.,
                        clearance: 10.,
                    }),
                    distance: None,
                    within_radius: false,
                    reason: String::new(),
                })
                .collect(),
            required_missing: 0,
            attempts: 0,
        };
        let roadside = vec![roadside_stamps::Placement {
            request: u64::MAX,
            prefab: "assets/test/roadside.prefab".into(),
            position: [-90., 14., 55.],
            yaw: 123.,
            road: 0,
            start_index: 0,
            end_index: 1,
        }];
        (project, solution, roadside)
    }

    #[test]
    fn complete_roster_uses_primary_and_roadside_export_poses_and_skips_unplaced_targets() {
        let (project, solution, roadside) = layout();
        let roots = placements(&project, &solution, &roadside).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].instance, project.targets[0].instance_id);
        assert_eq!(
            roots[0].prefab,
            project.targets[0].prefab_path.as_deref().unwrap()
        );
        assert_eq!(
            roots[0].pose,
            prefab_export::monument_pose([100., 20., -40.], 37.).unwrap()
        );
        assert_eq!(roots[1].instance, roadside[0].request);
        assert_eq!(roots[1].prefab, roadside[0].prefab);
        assert_eq!(
            roots[1].pose,
            prefab_export::monument_pose(roadside[0].position, 123.).unwrap()
        );
    }

    #[test]
    fn missing_unknown_duplicate_and_colliding_rosters_are_rejected() {
        let (mut project, mut solution, mut roadside) = layout();
        roadside[0].request = solution.outcomes[1].id;
        assert!(placements(&project, &solution, &roadside).is_err());
        roadside[0].request = u64::MAX;
        solution.outcomes[1].id = solution.outcomes[0].id;
        assert!(placements(&project, &solution, &roadside).is_err());
        solution.outcomes[1].id = u64::MAX - 1;
        assert!(placements(&project, &solution, &roadside).is_err());
        solution.outcomes.pop();
        assert!(placements(&project, &solution, &roadside).is_err());
        project.targets.truncate(1);
        project.targets[0].prefab_path = None;
        assert!(placements(&project, &solution, &roadside).is_err());
    }
}
