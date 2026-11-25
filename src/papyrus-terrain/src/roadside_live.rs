//! Live roadside fitting and terrain admission under shared world-pass budgets.
//! The installation adapter supplies its fitted-location validity policy.
use crate::{
    decor::{self, Kind, Stage},
    decor_fit::{self, OrientedOutcome, OrientedRejection, StaticRequest},
    decor_live::{self, Context, Instance},
    roadside_decor::{self, Road, Settings},
    spawn,
};
use serde::Serialize;
use std::{cell::Cell, collections::BTreeMap};

pub const VERSION: &str = "papyrus-live-roadside-decoration-1";

pub struct Request<'a> {
    pub stage: &'a Stage,
    pub sampling: Settings,
    pub roads: &'a [Road<'a>],
    pub seed: u32,
    pub realm_mask: u32,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub skipped: Option<&'static str>,
    pub sampling: roadside_decor::Report,
    pub fitting_attempts: usize,
    pub rejected_bounds: usize,
    pub rejected_fits: BTreeMap<String, usize>,
    pub rejected_checks: usize,
    pub rejected_admissions: usize,
    pub admitted: Vec<Instance>,
    pub assembled_instances: usize,
    pub physics_validated: bool,
}

/// Callback checks run after fitting and before any terrain admission. They own
/// source-specific slope, monument eligibility and clearance checks. Any error
/// invalidates the shared private transaction. Cancellation prevents publication.
pub fn emit(
    request: Request<'_>,
    mut context: Context<'_, '_>,
    mut valid_location: impl FnMut(
        &decor_fit::OrientedPose,
        crate::native_layers::View<'_>,
    ) -> Result<bool, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let result = emit_inner(request, &mut context, &mut valid_location, &cancelled);
    if cancelled() {
        context.transaction.cancel();
        return Ok(None);
    }
    match &result {
        Ok(None) => context.transaction.cancel(),
        Err(_) => context.transaction.invalidate(),
        _ => {}
    }
    result
}

fn emit_inner(
    request: Request<'_>,
    context: &mut Context<'_, '_>,
    valid_location: &mut impl FnMut(
        &decor_fit::OrientedPose,
        crate::native_layers::View<'_>,
    ) -> Result<bool, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if cancelled() {
        return Ok(None);
    }
    let stage = request.stage;
    stage.validate_path_attached()?;
    request
        .sampling
        .validate()
        .map_err(|error| format!("Invalid roadside sampling policy {error:?}"))?;
    if stage.kind != Kind::Uniform
        || stage.cluster.is_some()
        || stage.density != 0.
        || request.realm_mask == 0
        || stage.distance != request.sampling.distance
        || stage.dithering != request.sampling.dithering
    {
        return Err("Roadside fitting descriptor differs from its sampling stage".into());
    }
    let skipped = if !stage.enabled {
        Some("Disabled in source")
    } else if stage.mode as u32 & request.realm_mask == 0 {
        Some("Source realm excludes the selected generation realm")
    } else if stage.prefabs.is_empty() {
        Some("Missing source prefab roster")
    } else {
        None
    };
    let mut report = Report {
        engine: VERSION,
        skipped,
        sampling: roadside_decor::Report {
            final_seed: request.seed,
            ..Default::default()
        },
        fitting_attempts: 0,
        rejected_bounds: 0,
        rejected_fits: BTreeMap::new(),
        rejected_checks: 0,
        rejected_admissions: 0,
        admitted: Vec::new(),
        assembled_instances: 0,
        physics_validated: false,
    };
    if skipped.is_some() {
        return Ok(Some(report));
    }
    for path in &stage.prefabs {
        if !context.prefabs.contains_key(path) {
            return Err("Missing live roadside fitting metadata".into());
        }
    }
    let stopped = Cell::new(false);
    let sampled = roadside_decor::sample(
        request.roads,
        request.sampling,
        stage.prefabs.len(),
        request.seed,
        context.attempts,
        |candidate| {
            let view = context.transaction.view();
            let [x, z] = candidate.position_xz;
            let size = view.size as f32;
            if !x.is_finite() || !z.is_finite() || x.abs() > size * 0.5 || z.abs() > size * 0.5 {
                report.rejected_bounds += 1;
                return Ok(false);
            }
            let y = spawn::sample_height_view(view, x / size + 0.5, z / size + 0.5)?;
            let source = decor::Candidate {
                stage: stage.order,
                prefab: candidate.prefab,
                position: [x, y, z],
            };
            let path = &stage.prefabs[candidate.prefab];
            let prefab = &context.prefabs[path];
            report.fitting_attempts += 1;
            if decor_live::origin_precheck_ready(prefab, context.water.is_some())
                && context
                    .transaction
                    .origin_blocked(stage, path, source.position)?
            {
                OrientedRejection::Reservation.record(&mut report.rejected_fits);
                return Ok(false);
            }
            let transform = match context.fitting.try_fit_path_static(
                context.transaction.view(),
                StaticRequest {
                    candidate: &source,
                    stage,
                    prefab,
                    seed: request.seed,
                    water: context.water,
                },
            )? {
                OrientedOutcome::Fitted(pose) => pose,
                OrientedOutcome::Rejected(OrientedRejection::Budget) => {
                    return Err("Live roadside stage exhausted the shared fitting allowance".into());
                }
                OrientedOutcome::Rejected(reason) => {
                    reason.record(&mut report.rejected_fits);
                    return Ok(false);
                }
            };
            if !valid_location(&transform, context.transaction.view())? {
                report.rejected_checks += 1;
                return Ok(false);
            }
            let id = *context.next_pose;
            *context.next_pose = id
                .checked_add(1)
                .ok_or("Live roadside admission ID overflow")?;
            let pose = decor_fit::Pose {
                candidate: id,
                position: transform.position,
                rotation: transform.rotation,
                scale: transform.scale,
            };
            match context
                .transaction
                .try_apply(id, stage, path, &pose, cancelled)?
            {
                Some(true) => {
                    report.admitted.push(Instance {
                        transaction_pose: id,
                        prefab: path.clone(),
                        transform,
                    });
                    Ok(true)
                }
                Some(false) => {
                    report.rejected_admissions += 1;
                    Ok(false)
                }
                None => {
                    stopped.set(true);
                    Ok(false)
                }
            }
        },
        || stopped.get() || cancelled(),
    )
    .map_err(|error| format!("Live roadside sampling failed {error:?}"))?;
    let Some(sampled) = sampled else {
        return Ok(None);
    };
    if stopped.get() || cancelled() {
        return Ok(None);
    }
    report.sampling = sampled;
    Ok(Some(report))
}

#[cfg(test)]
mod tests;
