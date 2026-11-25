//! Source-ordered multiscale hill-cliff fitting and atomic terrain admission.
use crate::{
    decor::{self, Candidate, Kind, Stage, StageReport},
    decor_fit::{self, OrientedOutcome, OrientedRejection, RejectionCounts, StaticRequest},
    decor_live::{Context, Instance, Report},
    footprints::{CliffScaleBounds, OriginCheck, OriginProbe, RoadTransaction},
    spawn,
};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod replay;
pub use replay::{while_composing as with_forecast_replay, without_forecast_replay};

pub const VERSION: &str = "papyrus-uniform-cliffs-1";

#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct ReservationCounts {
    pub before_fitting: usize,
    pub after_transforms: usize,
}

struct CliffPrefab<'a> {
    index: usize,
    priority: i32,
    placed: usize,
    origin_check: Result<Option<(OriginCheck, Option<f64>)>, String>,
    prefab: &'a decor_fit::PreparedPrefab,
}

// Only speculative reads for stationary, normalized transforms are supported.
// Bound rotation of the scaled vector by twice its length. Authored and
// generated quaternions stay near unit length through the tilt rotation.
// The extra cells cover interpolation, normal taps,
// support-window rounding, and the height-to-surface projection at grid edges.
fn prediction_radius(prefab: &decor_fit::PreparedPrefab, size: u32, n: usize) -> Option<usize> {
    use decor_fit::Transform;
    let bounds = cliff_scale_bounds(prefab)?;
    let prefab = prefab.metadata();
    if prefab.transforms.iter().any(|t| match t {
        Transform::Offset { .. } | Transform::Swim => true,
        Transform::AlignSlope { offset, .. } => *offset != [0.; 3],
        _ => false,
    }) {
        return None;
    }
    let reach = |position: [f32; 3], radius: f32| {
        let [x, y, z] = position.map(f64::from);
        2. * f64::from(bounds.largest_axis) * x.hypot(y).hypot(z) + f64::from(radius)
    };
    let maximum = prefab
        .anchors
        .iter()
        .map(|a| reach(a.position, a.radius))
        .chain(
            prefab
                .filters
                .iter()
                .map(|f| reach(f.position, f.topology_radius)),
        )
        .fold(0., f64::max);
    let cells = (maximum / f64::from(size) * n as f64).ceil() + 4.;
    (cells <= 128.).then_some(cells as usize)
}

#[derive(Clone, Copy)]
struct Position {
    xz: [f32; 2],
    uv: [f32; 2],
    threshold: f32,
}

impl Position {
    fn cell(self, n: usize) -> Option<[usize; 2]> {
        let [u, v] = self.uv;
        ((0. ..=1.).contains(&u) && (0. ..=1.).contains(&v)).then(|| surface_coordinates(n, u, v))
    }

    fn passes(self, cell: usize, stage: &Stage, transaction: &RoadTransaction<'_>) -> bool {
        if stage.filter_mode & 1 == 0 {
            return true;
        }
        if stage.filter.splat_type == -1 && stage.filter.biome_type == -1 {
            // The unweighted filter returns 0 or 1. A zero draw still passes.
            return self.threshold == 0.
                || transaction.topology_sample_matches(cell, &stage.filter);
        }
        let view = transaction.view();
        let [u, v] = self.uv;
        let factor = if view.placement_blocked_at(cell) {
            0.
        } else {
            spawn::factor_view_at(view, &stage.filter, cell, u, v)
        };
        (factor * factor).partial_cmp(&self.threshold) != Some(std::cmp::Ordering::Less)
    }
}

fn reservation_scale(
    entry: &CliffPrefab<'_>,
    transaction: &RoadTransaction<'_>,
    position: Position,
    cell: [usize; 2],
    scan_neighbors: bool,
    origin: &mut Option<(OriginCheck, OriginProbe)>,
) -> Result<Option<Option<f64>>, String> {
    let Some((check, limit)) = *entry.origin_check.as_ref().map_err(Clone::clone)? else {
        return Ok(Some(None));
    };
    let probe = match *origin {
        Some((rule, probe)) if check.same_policy(rule) => probe,
        _ => {
            let probe = transaction.probe_origin_cell(position.xz, cell, check, scan_neighbors);
            *origin = Some((check, probe));
            probe
        }
    };
    if check.conflicts(probe) {
        Ok(limit.map(Some))
    } else {
        Ok(Some(None))
    }
}

struct Prediction {
    prefab: u8,
    reserved_scale: Option<f64>,
    work: [usize; 2],
    outcome: OrientedOutcome,
}

const PREDICTION_CHUNK: usize = 16;
const MAX_PREDICTED_PREFABS: usize = 32;

#[derive(Default)]
struct PredictionChunk {
    masks: [u32; PREDICTION_CHUNK],
    offsets: [u16; PREDICTION_CHUNK],
    entries: Vec<Prediction>,
}

impl PredictionChunk {
    fn get(&self, position: usize, prefab: usize) -> Option<&Prediction> {
        let mask = self.masks[position];
        let bit = 1 << prefab;
        (mask & bit != 0).then(|| {
            let index = usize::from(self.offsets[position]) + (mask & (bit - 1)).count_ones() as usize;
            &self.entries[index]
        })
    }
}

struct Batch<'a, 'terrain> {
    transaction: &'a RoadTransaction<'terrain>,
    alpha: &'terrain [u8],
    stage: &'a decor_fit::ValidatedStage<'a>,
    prefabs: &'a [CliffPrefab<'a>],
    radii: &'a [Option<usize>],
    seed: u32,
    scan_neighbors: bool,
    water: Option<decor_fit::ValidatedWater<'terrain>>,
}

impl Batch<'_, '_> {
    fn predict(
        &self,
        position: Position,
        fitter: &mut decor_fit::OrientedBudget<'_>,
        stop: &AtomicBool,
        chunk: &mut PredictionChunk,
        index: usize,
    ) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let view = self.transaction.view();
        let n = view.surface_resolution;
        let Some(cell) = position.cell(n) else {
            return;
        };
        let i = cell[1] * n + cell[0];
        let stage = self.stage.stage();
        if self.alpha[i] < 128 || !position.passes(i, stage, self.transaction) {
            return;
        }
        let [u, v] = position.uv;
        let [x, z] = position.xz;
        let mut height = None;
        let mut origin = None;
        let start = chunk.entries.len();
        for entry in self.prefabs {
            let Ok(reserved) = reservation_scale(
                entry,
                self.transaction,
                position,
                cell,
                self.scan_neighbors,
                &mut origin,
            ) else {
                break;
            };
            let Some(reserved_scale) = reserved else {
                continue;
            };
            if self.radii[entry.index].is_none() {
                continue;
            }
            let candidate = Candidate {
                stage: stage.order,
                prefab: entry.index,
                position: [
                    x,
                    *height.get_or_insert_with(|| spawn::height_view(view, u, v)),
                    z,
                ],
            };
            fitter.reset_cliff_batch_worker();
            let Ok(Some(outcome)) = fitter.try_fit_uniform_cliff_prepared(
                view,
                StaticRequest {
                    candidate: &candidate,
                    stage,
                    prefab: entry.prefab,
                    seed: self.seed,
                    water: self.water,
                },
                self.stage,
                reserved_scale,
                || stop.load(Ordering::Relaxed),
            ) else {
                break;
            };
            if matches!(
                outcome,
                OrientedOutcome::Rejected(OrientedRejection::Budget)
            ) {
                break;
            }
            chunk.masks[index] |= 1 << entry.index;
            chunk.entries.push(Prediction {
                prefab: entry.index as u8,
                reserved_scale,
                work: [fitter.samples(), fitter.height_samples()],
                outcome,
            });
        }
        chunk.offsets[index] = start as u16;
        chunk.entries[start..].sort_unstable_by_key(|entry| entry.prefab);
    }

    fn predict_positions(
        &self,
        pool: &rayon::ThreadPool,
        positions: &[Position],
        fitting: &decor_fit::OrientedBudget<'_>,
        retained: &[Option<Arc<PredictionChunk>>],
        cancelled: &impl Fn() -> bool,
    ) -> Option<Vec<Arc<PredictionChunk>>> {
        let stop = AtomicBool::new(false);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        pool.in_place_scope(|scope| {
            let stop = &stop;
            scope.spawn(move |_| {
                let forecasts = positions
                    .par_chunks(PREDICTION_CHUNK)
                    .enumerate()
                    .map_init(
                        || fitting.cliff_batch_worker(),
                        |fitter, (slot, points)| {
                            if let Some(Some(found)) = retained.get(slot) {
                                return found.clone();
                            }
                            let mut chunk = PredictionChunk::default();
                            for (index, &point) in points.iter().enumerate() {
                                self.predict(point, fitter, stop, &mut chunk, index);
                            }
                            Arc::new(chunk)
                        },
                    )
                    .collect();
                let _ = sender.send(forecasts);
            });
            loop {
                if cancelled() {
                    stop.store(true, Ordering::Relaxed);
                    return None;
                }
                match receiver.recv_timeout(std::time::Duration::from_millis(5)) {
                    Ok(forecasts) => return Some(forecasts),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
                }
            }
        })
    }
}

fn cliff_scale_bounds(prefab: &decor_fit::PreparedPrefab) -> Option<CliffScaleBounds> {
    use decor_fit::Transform;

    let prefab = prefab.metadata();
    let mut lower = prefab.scale;
    let mut upper = prefab.scale;
    for transform in &prefab.transforms {
        match *transform {
            Transform::Scale { min, max } => {
                for axis in 0..3 {
                    lower[axis] *= min[axis];
                    // Use the fitting interpolation evaluated at its upper
                    // endpoint, including its f32 rounding.
                    upper[axis] *= min[axis] + (max[axis] - min[axis]);
                }
            }
            Transform::AlignSlope { scale, .. } if scale != [1.; 3] => return None,
            _ => {}
        }
    }
    let smallest_axis = lower.into_iter().fold(f32::INFINITY, f32::min);
    let largest_axis = upper.into_iter().fold(0., f32::max);
    (smallest_axis > 0. && largest_axis.is_finite()).then_some(CliffScaleBounds {
        smallest_axis,
        largest_axis,
    })
}

fn record_admission(prefabs: &mut [CliffPrefab<'_>], slot: usize) {
    let selected = &mut prefabs[slot];
    selected.placed += 1;
    let priority = selected.priority;
    let placed = selected.placed;
    // Only the admitted prefab's count changed. It can move past peers with
    // fewer placements, and must stay before peers it just tied.
    let passed = prefabs[slot + 1..]
        .iter()
        .take_while(|other| other.priority == priority && other.placed < placed)
        .count();
    if passed != 0 {
        prefabs[slot..=slot + passed].rotate_left(1);
    }
}

fn surface_coordinates(n: usize, u: f32, v: f32) -> [usize; 2] {
    [u, v].map(|coordinate| ((coordinate * n as f32) as usize).min(n - 1))
}

fn scan_level(
    size: f32,
    spacing: f32,
    dithering: f32,
    rng: &mut u64,
) -> impl Iterator<Item = Position> + '_ {
    let half = size * 0.5;
    let normalize = move |value: f32| value / size + 0.5;
    // Build the regular axis with the original repeated additions. Using
    // index * spacing can move points across cells for fractional spacing.
    let columns: Option<Vec<_>> = (dithering == 0.).then(|| {
        std::iter::successors(Some(-half), move |&x| Some(x + spacing))
            .take_while(|&x| x < half)
            .map(|x| [x, normalize(x)])
            .collect()
    });
    let mut column = 0;
    let mut x = -half;
    let mut z = -half;
    let mut v = normalize(z);
    std::iter::from_fn(move || {
        if let Some(columns) = &columns {
            if column == columns.len() {
                column = 0;
                z += spacing;
                v = normalize(z);
            }
            if z >= half {
                return None;
            }
            let [x, u] = columns[column];
            column += 1;
            // The skipped jitter values still consume two draws.
            decor::advance_random(rng, 2);
            return Some(Position {
                xz: [x, z],
                uv: [u, v],
                threshold: decor::unit(rng),
            });
        }
        if x >= half {
            x = -half;
            z += spacing;
        }
        if z >= half {
            return None;
        }
        let px = x + (decor::unit(rng) * 2. - 1.) * dithering;
        let pz = z + (decor::unit(rng) * 2. - 1.) * dithering;
        x += spacing;
        Some(Position {
            xz: [px, pz],
            uv: [normalize(px), normalize(pz)],
            threshold: decor::unit(rng),
        })
    })
}

pub fn emit(
    stage: &Stage,
    priorities: &BTreeMap<String, i32>,
    seed: u32,
    realm: u32,
    context: Context<'_, '_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    emit_with_cpu_pool(stage, priorities, seed, realm, context, None, cancelled)
}

pub fn emit_with_cpu_pool(
    stage: &Stage,
    priorities: &BTreeMap<String, i32>,
    seed: u32,
    realm: u32,
    mut context: Context<'_, '_>,
    pool: Option<&rayon::ThreadPool>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let result = emit_inner::<true>(
        stage,
        priorities,
        seed,
        realm,
        &mut context,
        pool,
        &cancelled,
    );
    context.transaction.finish_cliff_batches();
    match &result {
        Ok(None) => context.transaction.cancel(),
        Err(_) => context.transaction.invalidate(),
        _ => {}
    }
    result
}

fn emit_inner<const PRECHECK: bool>(
    stage: &Stage,
    priorities: &BTreeMap<String, i32>,
    seed: u32,
    realm: u32,
    context: &mut Context<'_, '_>,
    pool: Option<&rayon::ThreadPool>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if cancelled() {
        return Ok(None);
    }
    let validated_stage = decor_fit::ValidatedStage::new(stage)?.with_seed(seed);
    let size = context.transaction.view().size;
    let n = context.transaction.view().surface_resolution;
    if stage.kind != Kind::Uniform || realm == 0 || context.alpha.len() != n * n {
        return Err("Invalid uniform cliff stage or terrain".into());
    }
    // The source uses stable descending priority, then ascending SpawnedCount.
    // A successful admission is the only event that can change this ordering.
    let mut prefabs = Vec::with_capacity(stage.prefabs.len());
    for (index, path) in stage.prefabs.iter().enumerate() {
        let priority = *priorities
            .get(path)
            .ok_or("Missing uniform cliff priority")?;
        let prefab = context
            .prefabs
            .get(path)
            .ok_or("Missing or unsupported uniform cliff fitting metadata")?;
        if !(0..=4).contains(&priority) {
            return Err("Missing or unsupported uniform cliff fitting metadata".into());
        }
        let origin_check = if PRECHECK
            && crate::decor_live::origin_precheck_ready(prefab, context.water.is_some())
        {
            context.transaction.origin_check(stage, path, cliff_scale_bounds(prefab))
        } else {
            Ok(None)
        };
        prefabs.push(CliffPrefab {
            index,
            priority,
            placed: 0,
            origin_check,
            prefab,
        });
    }
    let initial_seed = decor::stage_seed(seed, &stage.id);
    let mut rng = initial_seed;
    let skipped = if !stage.enabled {
        Some("Disabled in source".into())
    } else if stage.mode as u32 & realm == 0 {
        Some("Source realm excludes the selected generation realm".into())
    } else if prefabs.is_empty() {
        Some("Missing source prefab roster".into())
    } else {
        None
    };
    let mut report = Report {
        coastal: None,
        cliff_reservations: None,
        engine: VERSION,
        sampling: StageReport {
            id: stage.id.clone(),
            name: stage.name.clone(),
            seed: initial_seed,
            requested_attempts: (0..8)
                .map(|level| {
                    let side = (f64::from(size) / f64::from(stage.distance * (128 >> level) as f32))
                        .ceil() as usize;
                    side * side
                })
                .sum(),
            attempted: 0,
            candidates: 0,
            passing_candidates: 0,
            rejected_filter: 0,
            rejected_bounds: 0,
            rejected_holes: 0,
            skipped,
            budget_exhausted: false,
            prefabs: stage.prefabs.clone(),
        },
        fitting_attempts: 0,
        rejected_fits: BTreeMap::new(),
        rejected_admissions: 0,
        admitted: Vec::new(),
        assembled_instances: 0,
        physics_validated: false,
    };
    if report.sampling.skipped.is_some() {
        return Ok(Some(report));
    }
    prefabs.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.placed.cmp(&b.placed)));
    let scan_neighbors = prefabs.iter().any(|entry| {
        entry.origin_check.as_ref().ok().and_then(|check| *check)
            .is_some_and(|(check, _)| check.scans_neighbors())
    });
    let mut reservations = ReservationCounts::default();
    let mut rejected_fits = RejectionCounts::default();
    let pool = pool
        .filter(|pool| pool.current_num_threads() > 1 && prefabs.len() <= MAX_PREDICTED_PREFABS);
    let mut radii = vec![None; prefabs.len()];
    if pool.is_some() {
        for entry in &prefabs {
            radii[entry.index] = prediction_radius(entry.prefab, size, n);
        }
    }
    let pool = pool.filter(|_| radii.iter().any(Option::is_some));
    let mut replay = pool.and_then(|_| replay::Tiles::new(stage, seed, context.transaction.view(), &prefabs, &radii));
    let mut predicted = 0_u64;
    let mut reused = 0_u64;
    let mut reused_samples = 0_u64;
    for level in 0..8 {
        let spacing = stage.distance * (128 >> level) as f32;
        // Fill bounded prediction batches across row boundaries without
        // changing the row-major coordinates or the order of random draws.
        let mut scan = scan_level(size as f32, spacing, stage.dithering, &mut rng);
        let mut positions = Vec::new();
        let mut forecasts = Vec::new();
        let mut next = 0;
        loop {
            if cancelled() {
                return Ok(None);
            }
            let (position, predictions, prediction_position) = if let Some(pool) = pool {
                if next == positions.len() {
                    positions.clear();
                    // Each point costs at least one attempt. Read one point
                    // at zero remaining to distinguish exhaustion from EOF.
                    let batch_size = context.attempts.remaining().clamp(1, 512);
                    positions.extend(scan.by_ref().take(batch_size));
                    if positions.is_empty() {
                        break;
                    }
                    let snapshot = Batch {
                        transaction: context.transaction,
                        alpha: context.alpha,
                        stage: &validated_stage,
                        prefabs: &prefabs,
                        radii: &radii,
                        seed,
                        scan_neighbors,
                        water: context.water,
                    };
                    // Skip waking the pool for sparse or disallowed batches.
                    let has_work = context.fitting.remaining_samples() != 0
                        && positions
                            .iter()
                            .filter(|&&point| {
                                point.cell(n).is_some_and(|[col, row]| {
                                    let i = row * n + col;
                                    context.alpha[i] >= 128
                                        && point.passes(i, stage, context.transaction)
                                })
                            })
                            .take(8)
                            .count()
                            == 8;
                    forecasts = if has_work {
                        let keys = replay.as_mut().map_or_else(Vec::new, |tiles| tiles.keys(&snapshot, &positions));
                        let retained: Vec<_> = keys.iter().map(|key| key.and_then(replay::find)).collect();
                        let Some(plans) = snapshot.predict_positions(pool, &positions, context.fitting, &retained, cancelled) else {
                            return Ok(None);
                        };
                        for (key, plan) in keys.into_iter().zip(&plans) {
                            if let (Some(tiles), Some(key)) = (&replay, key)
                                && !plan.entries.is_empty()
                            {
                                tiles.retain(key, plan.clone());
                            }
                        }
                        plans
                    } else {
                        Vec::new()
                    };
                    predicted += forecasts
                        .iter()
                        .map(|chunk| chunk.entries.len() as u64)
                        .sum::<u64>();
                    if has_work {
                        context.transaction.start_cliff_batch();
                    }
                    next = 0;
                }
                let index = next;
                next += 1;
                (
                    positions[index],
                    forecasts.get(index / PREDICTION_CHUNK),
                    index % PREDICTION_CHUNK,
                )
            } else {
                let Some(point) = scan.next() else { break };
                (point, None, 0)
            };
            context.attempts.take()?;
            report.sampling.attempted += 1;
            let [px, pz] = position.xz;
            let [u, v] = position.uv;
            let Some([column, row]) = position.cell(n) else {
                report.sampling.rejected_bounds += 1;
                continue;
            };
            let cell = row * n + column;
            if context.alpha[cell] < 128 {
                report.sampling.rejected_holes += 1;
                continue;
            }
            if !position.passes(cell, stage, context.transaction) {
                report.sampling.rejected_filter += 1;
                continue;
            }
            report.sampling.candidates += 1;
            report.sampling.passing_candidates += 1;
            let mut origin_result = None;
            let mut height = None;
            let mut admitted_slot = None;
            for (slot, entry) in prefabs.iter().enumerate() {
                if cancelled() {
                    return Ok(None);
                }
                context.attempts.take()?;
                report.fitting_attempts += 1;
                let path = &stage.prefabs[entry.index];
                let Some(reserved_scale) = reservation_scale(
                    entry,
                    context.transaction,
                    position,
                    [column, row],
                    scan_neighbors,
                    &mut origin_result,
                )?
                else {
                    reservations.before_fitting += 1;
                    continue;
                };
                let prediction =
                    predictions.and_then(|chunk| chunk.get(prediction_position, entry.index));
                let outcome = if let Some(prediction) = prediction
                    && let Some(radius) = radii[entry.index]
                    && prediction.reserved_scale == reserved_scale
                    && !context
                        .transaction
                        .changed_during_cliff_batch([column, row], radius)
                    && context
                        .fitting
                        .charge_cliff_prediction(prediction.work[0], prediction.work[1])
                {
                    reused += 1;
                    reused_samples += prediction.work[0] as u64;
                    prediction.outcome
                } else {
                    let candidate = Candidate {
                        stage: stage.order,
                        prefab: entry.index,
                        position: [
                            px,
                            *height.get_or_insert_with(|| {
                                spawn::height_view(context.transaction.view(), u, v)
                            }),
                            pz,
                        ],
                    };
                    let Some(outcome) = context.fitting.try_fit_uniform_cliff_prepared(
                        context.transaction.view(),
                        StaticRequest {
                            candidate: &candidate,
                            stage,
                            prefab: entry.prefab,
                            seed,
                            water: context.water,
                        },
                        &validated_stage,
                        reserved_scale,
                        cancelled,
                    )?
                    else {
                        return Ok(None);
                    };
                    outcome
                };
                let transform = match outcome {
                    OrientedOutcome::Fitted(pose) => pose,
                    OrientedOutcome::Rejected(OrientedRejection::Budget) => {
                        return Err(format!(
                            "Hill-cliff fitting exhausted its allowance in {} after {} samples. {}",
                            stage.name, context.fitting.samples(),
                            serde_json::json!({
                                "positions": report.sampling.attempted,
                                "requested_positions": report.sampling.requested_attempts,
                                "prefab_trials": report.fitting_attempts,
                                "placed": report.admitted.len(),
                                "height_samples": context.fitting.height_samples(),
                            })
                        ));
                    }
                    OrientedOutcome::Rejected(OrientedRejection::Reservation) => {
                        reservations.after_transforms += 1;
                        continue;
                    }
                    OrientedOutcome::Rejected(reason) => {
                        rejected_fits.record(reason);
                        continue;
                    }
                };
                let id = *context.next_pose;
                *context.next_pose = id
                    .checked_add(1)
                    .ok_or("Uniform cliff admission ID overflow")?;
                let pose = decor_fit::Pose {
                    candidate: id,
                    position: transform.position,
                    rotation: transform.rotation,
                    scale: transform.scale,
                };
                match context
                    .transaction
                    .try_apply_validated(id, &validated_stage, entry.index, &pose, cancelled)?
                {
                    Some(true) => {
                        if report.admitted.len() == 100_000 {
                            return Err("Uniform cliff instance allowance exhausted".into());
                        }
                        report.admitted.push(Instance {
                            transaction_pose: id,
                            prefab: path.clone(),
                            transform,
                        });
                        admitted_slot = Some(slot);
                        break;
                    }
                    Some(false) => report.rejected_admissions += 1,
                    None => return Ok(None),
                }
            }
            if let Some(slot) = admitted_slot {
                record_admission(&mut prefabs, slot);
            }
        }
    }
    report.rejected_fits = rejected_fits.finish();
    let reservation_rejections = reservations.before_fitting + reservations.after_transforms;
    if reservation_rejections != 0 {
        report
            .rejected_fits
            .insert("reservation".into(), reservation_rejections);
    }
    if pool.is_some() && std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some() {
        eprintln!(
            "MAPPER_CLIFF_PREDICTIONS {}",
            serde_json::json!({
                "stage_id": stage.id, "predicted": predicted, "reused": reused,
                "reused_samples": reused_samples,
            })
        );
    }
    report.cliff_reservations = Some(reservations);
    Ok((!cancelled()).then_some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        decor_fit::{OrientedBudget, PreparedPrefab},
        decor_stream::Budget,
        decor_topology,
        footprints::{self, RoadTransaction},
        native_layers::{Layers, dimensions},
    };
    const A: &str = "assets/bundled/prefabs/autospawn/decor/cliffs/a.prefab";
    const B: &str = "assets/bundled/prefabs/autospawn/decor/cliffs/b.prefab";
    const C: &str = "assets/bundled/prefabs/autospawn/decor/cliffs/c.prefab";
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let mut biome = vec![0; n * n * 5];
        biome[..n * n].fill(255);
        let mut splat = vec![0; n * n * 8];
        splat[..n * n].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![crate::erosion::encode(20.); hr * hr],
            biome,
            splat,
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn stage() -> Stage {
        serde_json::from_value(serde_json::json!({"id":"622","order":0,"name":"Cliffs","kind":"uniform","folder":"decor/cliffs","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":1,"TopologyAll":0,"TopologyNot":2},"filter_mode":1,"anchor_mode":2,"distance":250.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[A,B,C]})).unwrap()
    }
    fn prefabs() -> BTreeMap<String, PreparedPrefab> {
        [A, B, C]
            .into_iter()
            .map(|path| {
                (
                    path.into(),
                    PreparedPrefab::new(decor_fit::Prefab {
                        rotation: [0., 0., 0., 1.],
                        scale: [1.; 3],
                        transforms: vec![],
                        anchors: vec![decor_fit::Anchor {
                            position: [0.; 3],
                            extents: 0.2,
                            offset: 0.,
                            radius: 0.,
                            slope_scale: 0.,
                        }],
                        filters: vec![],
                        unsupported: vec![],
                        pending: vec![],
                    })
                    .unwrap(),
                )
            })
            .collect()
    }
    fn transaction<'a>(layers: &'a Layers, alpha: &'a [u8]) -> RoadTransaction<'a> {
        transaction_with_root_radius(layers, alpha, 0.)
    }

    #[test]
    fn cliff_scan_preserves_coordinates_and_draws_with_fractional_spacing() {
        let size = 1001_f32;
        let half = size * 0.5;
        for (spacing, dithering) in [(7.13, 0.), (9.79, -0.), (7.13, 1.75), (1024., 0.)] {
            let mut expected_rng = u64::MAX - 27;
            let mut rng = expected_rng;
            let mut scan = scan_level(size, spacing, dithering, &mut rng);
            let mut z = -half;
            while z < half {
                let mut x = -half;
                while x < half {
                    let (px, pz) = if dithering != 0. {
                        (
                            x + (decor::unit(&mut expected_rng) * 2. - 1.) * dithering,
                            z + (decor::unit(&mut expected_rng) * 2. - 1.) * dithering,
                        )
                    } else {
                        decor::random(&mut expected_rng);
                        decor::random(&mut expected_rng);
                        (x, z)
                    };
                    let point = scan.next().unwrap();
                    assert_eq!(point.xz.map(f32::to_bits), [px, pz].map(f32::to_bits));
                    assert_eq!(
                        point.uv.map(f32::to_bits),
                        [px / size + 0.5, pz / size + 0.5].map(f32::to_bits)
                    );
                    assert_eq!(point.threshold.to_bits(), decor::unit(&mut expected_rng).to_bits());
                    x += spacing;
                }
                z += spacing;
            }
            assert!(scan.next().is_none());
            assert!(scan.next().is_none());
            drop(scan);
            assert_eq!(rng, expected_rng);
        }
    }

    #[test]
    fn shared_surface_cell_preserves_placement_and_filter_at_boundaries() {
        let mut layers = layers();
        let n = layers.surface_resolution;
        let cells = n * n;
        for (i, topology) in layers.topology.iter_mut().enumerate() {
            *topology = if (i ^ (i / n)).is_multiple_of(2) { 1 } else { 2 };
            layers.splat[i] = (i % 251) as u8;
        }
        let blockers: Vec<u8> = (0..cells).map(|i| (i % 3 == 0) as u8).collect();
        let view = crate::native_layers::View {
            placement_blockers: Some(&blockers),
            ..layers.view()
        };
        let alpha = vec![255; cells];
        let mut tx = transaction(&layers, &alpha);
        tx.initialize_blocking(&blockers, || false).unwrap().unwrap();
        let topology_filter = stage().filter;
        let mut filter = topology_filter.clone();
        filter.splat_type = 1;
        let coordinates = |edge: usize| {
            let edge = edge as f32 / n as f32;
            [edge.next_down().max(0.), edge, edge.next_up().min(1.)]
        };
        for x in 0..=n {
            for z in [0, 1, n / 2, n - 1, n] {
                for u in coordinates(x) {
                    for v in coordinates(z) {
                        let [column, row] = surface_coordinates(n, u, v);
                        let cell = row * n + column;
                        assert_eq!(
                            view.placement_blocked_at(cell),
                            view.placement_blocked(u, v)
                        );
                        assert_eq!(
                            spawn::factor_view_at(view, &filter, cell, u, v).to_bits(),
                            spawn::factor_view(view, &filter, u, v).to_bits()
                        );
                        assert_eq!(
                            tx.topology_sample_matches(cell, &topology_filter),
                            !view.placement_blocked_at(cell)
                                && spawn::factor_view(view, &topology_filter, u, v) == 1.
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn advancing_the_admitted_prefab_matches_stable_source_order() {
        let prefabs = prefabs();
        let prefab = &prefabs[A];
        for count in [1, 4, 33, 128] {
            let mut expected: Vec<_> = (0..count)
                .map(|index| (index, (index * 7 % 5) as i32, 0))
                .collect();
            let sort = |entries: &mut [(usize, i32, usize)]| {
                entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
            };
            sort(&mut expected);
            let mut ordered: Vec<_> = expected
                .iter()
                .map(|&(index, priority, placed)| CliffPrefab {
                    index,
                    priority,
                    placed,
                    origin_check: Ok(None),
                    prefab,
                })
                .collect();
            for step in 0..1024 {
                let slot = (step * 29 + step / 3) % count;
                expected[slot].2 += 1;
                sort(&mut expected);
                record_admission(&mut ordered, slot);
                assert!(
                    ordered
                        .iter()
                        .map(|p| (p.index, p.priority, p.placed))
                        .eq(expected.iter().copied())
                );
            }
        }
    }

    #[test]
    fn validated_cliff_admission_matches_checked_paths_and_work() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stage = stage();
        let validated = decor_fit::ValidatedStage::new(&stage).unwrap();
        let run = |prepared| {
            let mut tx = transaction(&layers, &alpha);
            let mut accepted = Vec::new();
            for (id, x) in [0., 0., 100.].into_iter().enumerate() {
                let y = spawn::height_view(tx.view(), x / layers.size as f32 + 0.5, 0.5);
                let pose = decor_fit::Pose {
                    candidate: id,
                    position: [x, y, 0.],
                    rotation: [0., 0., 0., 1.],
                    scale: [1.; 3],
                };
                let result = if prepared {
                    tx.try_apply_validated(id, &validated, 0, &pose, || false)
                } else {
                    tx.try_apply(id, &stage, &stage.prefabs[0], &pose, || false)
                };
                accepted.push(result.unwrap().unwrap());
            }
            (accepted, tx.finish(|| false).unwrap().unwrap())
        };
        let (reference, before) = run(false);
        let (checked, after) = run(true);
        assert_eq!(reference, [true, false, true]);
        assert_eq!(checked, reference);
        assert_eq!(before.height, after.height);
        assert_eq!(before.topology, after.topology);
        assert_eq!(before.splat, after.splat);
        assert_eq!(
            serde_json::to_value(before.report).unwrap(),
            serde_json::to_value(after.report).unwrap()
        );
    }

    fn transaction_with_root_radius<'a>(
        layers: &'a Layers,
        alpha: &'a [u8],
        root_radius: f32,
    ) -> RoadTransaction<'a> {
        transaction_with_support(layers, alpha, [0.; 3], root_radius)
    }

    fn transaction_with_support<'a>(
        layers: &'a Layers,
        alpha: &'a [u8],
        position: [f32; 3],
        radius: f32,
    ) -> RoadTransaction<'a> {
        let spec = footprints::Prefab {
            support: vec![footprints::Support { position, radius }],
            blockers: vec![],
            raises: vec![],
            topology: Some(decor_topology::Prefab {
                modifiers: vec![decor_topology::Modifier {
                    position: [0.; 3],
                    radius: 4.,
                    fade: 0.,
                    mask: 2,
                    operation: decor_topology::Operation::Add,
                }],
                queries: vec![],
                anchors: vec![],
            }),
            materials: Some(vec![]),
            height_modifiers: Some(vec![crate::decor_height::Modifier {
                position: [0.; 3],
                radius: 4.,
                fade: 0.,
                opacity: 1.,
                mode: crate::decor_height::Mode::Add { delta: 0.5 },
            }]),
            contributors: Some(vec![]),
            footprint: None,
            unsupported: vec![],
            pending: vec![],
        };
        RoadTransaction::new_objects(
            layers,
            alpha,
            [A, B, C]
                .into_iter()
                .map(|p| (p.into(), spec.clone()))
                .collect(),
            11,
            8,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn multiscale_order_uses_priority_then_balanced_counts_and_live_terrain() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stage = stage();
        let prefabs = prefabs();
        let priorities = BTreeMap::from([(A.into(), 3), (B.into(), 3), (C.into(), 2)]);
        let run = || {
            let mut tx = transaction(&layers, &alpha);
            let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
            let mut attempts = Budget::new(10000).unwrap();
            let mut next = 0;
            let report = emit(
                &stage,
                &priorities,
                42,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next,
                    water: None,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(report.sampling.requested_attempts, 26);
            assert_eq!(report.sampling.attempted, 26);
            assert_eq!(report.admitted.len(), 9);
            assert_eq!(report.admitted[0].transform.position[0], 0.);
            assert_eq!(report.admitted[0].transform.position[2], 0.);
            // Equal counts preserve the preceding source sort's order.
            let expected = [A, B, B, A, A, B, B, A, A];
            for (instance, expected) in report.admitted.iter().zip(expected) {
                assert_eq!(instance.prefab, expected);
            }
            assert!(report.sampling.rejected_filter > 0);
            let result = tx.finish(|| false).unwrap().unwrap();
            assert_eq!(result.report.counts.accepted, report.admitted.len());
            assert_eq!(result.report.counts.attempted, next);
            assert_ne!(result.height, layers.height);
            (report, result)
        };
        let (a, first) = run();
        let (b, second) = run();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        assert_eq!(first.height, second.height);
        assert_eq!(first.topology, second.topology);
        assert!(
            layers
                .height
                .iter()
                .all(|v| *v == crate::erosion::encode(20.))
        );
    }
    #[test]
    fn protected_roots_skip_cliff_searches_only_for_stationary_transforms() {
        use crate::decor_fit::Transform;
        let mut layers = layers();
        layers.topology.fill(8);
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let mut stage = stage();
        stage.filter.topology_any = 8;
        let priorities = BTreeMap::from([(A.into(), 3), (B.into(), 3), (C.into(), 2)]);
        for (moving, transform) in [
            (false, None),
            (
                false,
                Some(Transform::Offset {
                    min: [0., -1., 0.],
                    max: [0., -1., 0.],
                }),
            ),
            (
                true,
                Some(Transform::Offset {
                    min: [1., 0., 0.],
                    max: [1., 0., 0.],
                }),
            ),
            (
                true,
                Some(Transform::AlignSlope {
                    normal: 0.,
                    gradient: 0.,
                    offset: [1., 0., 0.],
                    scale: [1.; 3],
                }),
            ),
        ] {
            let prefabs = prefabs()
                .into_iter()
                .map(|(path, prefab)| {
                    let mut metadata = prefab.metadata().clone();
                    if let Some(transform) = &transform {
                        metadata.transforms.push(transform.clone());
                    }
                    (path, PreparedPrefab::new(metadata).unwrap())
                })
                .collect();
            let mut tx = transaction(&layers, &alpha);
            let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
            let mut attempts = Budget::new(10000).unwrap();
            let mut next = 0;
            let report = emit(
                &stage,
                &priorities,
                42,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next,
                    water: None,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(report.admitted.is_empty());
            assert_eq!(report.sampling.attempted, 26);
            let reservations = report.cliff_reservations.unwrap();
            assert_eq!(reservations.after_transforms, 0);
            if moving {
                assert!(!report.rejected_fits.contains_key("reservation"));
                assert_eq!(reservations.before_fitting, 0);
                assert!(fitting.samples() > 0 && next > 0 && tx.sample_visits() > 0);
                assert!(report.rejected_admissions > 0);
            } else {
                assert_eq!(report.rejected_fits["reservation"], report.fitting_attempts);
                assert_eq!(reservations.before_fitting, report.fitting_attempts);
                assert_eq!(fitting.samples(), 0);
                assert_eq!(next, 0);
                assert_eq!(tx.sample_visits(), 0);
            }
            let output = tx.finish(|| false).unwrap().unwrap();
            assert_eq!(output.height, layers.height);
            assert_eq!(
                output.topology.as_deref().unwrap_or(&layers.topology),
                layers.topology.as_slice()
            );
        }
    }

    #[test]
    fn root_margin_precheck_matches_admission_for_adjacent_protection_and_holes() {
        for (dx, dz) in [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ] {
            for hole in [false, true] {
                let mut layers = layers();
                let n = layers.surface_resolution;
                let mut alpha = vec![255; n * n];
                let neighbor = (n / 2).checked_add_signed(dz).unwrap() * n
                    + (n / 2).checked_add_signed(dx).unwrap();
                if hole {
                    alpha[neighbor] = 0;
                } else {
                    layers.topology[neighbor] = 8;
                }
                // The owned cliff metadata uses positive-radius root anchors.
                let mut tx = transaction_with_root_radius(&layers, &alpha, 1.);
                let visits = tx.sample_visits();
                assert!(tx.origin_blocked(&stage(), A, [0., 20., 0.]).unwrap());
                assert_eq!(tx.sample_visits(), visits);
                let pose = decor_fit::Pose {
                    candidate: 0,
                    position: [0., 20., 0.],
                    rotation: [0., 0.38268343, 0., 0.9238795],
                    scale: [0.75, 1.25, 1.5],
                };
                assert_eq!(
                    tx.try_apply(0, &stage(), A, &pose, || false).unwrap(),
                    Some(false)
                );
                let output = tx.finish(|| false).unwrap().unwrap();
                assert_eq!(output.height, layers.height);
                assert_eq!(output.report.counts.accepted, 0);
            }
        }
    }

    #[test]
    fn rooted_and_displaced_support_preserve_source_order_and_admitted_geometry() {
        let mut layers = layers();
        let n = layers.surface_resolution;
        for (i, topology) in layers.topology.iter_mut().enumerate() {
            if i % n < n / 2 {
                *topology = 8;
            }
        }
        let alpha = vec![255; n * n];
        let mut stage = stage();
        stage.filter.topology_any = 9;
        let prefabs = prefabs()
            .into_iter()
            .map(|(path, prepared)| {
                let mut prefab = prepared.metadata().clone();
                prefab.transforms.push(decor_fit::Transform::Scale {
                    min: [0.75, 0.5, 1.],
                    max: [1.15, 1.3, 1.5],
                });
                (path, PreparedPrefab::new(prefab).unwrap())
            })
            .collect();
        let priorities = BTreeMap::from([(A.into(), 3), (B.into(), 3), (C.into(), 2)]);
        let run = |precheck, support| {
            let mut tx = transaction_with_support(&layers, &alpha, support, 0.25);
            let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
            let mut attempts = Budget::new(10000).unwrap();
            let mut next = 0;
            let mut context = Context {
                transaction: &mut tx,
                alpha: &alpha,
                fitting: &mut fitting,
                prefabs: &prefabs,
                attempts: &mut attempts,
                next_pose: &mut next,
                water: None,
            };
            let report = if precheck {
                emit_inner::<true>(&stage, &priorities, 42, 2, &mut context, None, &|| false)
            } else {
                emit_inner::<false>(&stage, &priorities, 42, 2, &mut context, None, &|| false)
            }
            .unwrap()
            .unwrap();
            let geometry = report
                .admitted
                .iter()
                .map(|p| {
                    (
                        p.prefab.clone(),
                        p.transform.position.map(f32::to_bits),
                        p.transform.rotation.map(f32::to_bits),
                        p.transform.scale.map(f32::to_bits),
                    )
                })
                .collect::<Vec<_>>();
            let output = tx.finish(|| false).unwrap().unwrap();
            (report, geometry, output, fitting.samples(), attempts.used())
        };
        let conditional = [2.25, -0.25, 0.];
        let tx = transaction_with_support(&layers, &alpha, conditional, 0.25);
        let scale = CliffScaleBounds { smallest_axis: 1., largest_axis: 1.5 };
        let (_, limit) = tx.origin_check(&stage, A, Some(scale)).unwrap().unwrap();
        assert!((1. ..1.5).contains(&limit.unwrap()));
        drop(tx);
        for support in [[0.; 3], [0.8, -0.35, 1.1], conditional] {
            let (reference, reference_geometry, original, reference_samples, reference_attempts) =
                run(false, support);
            let (report, geometry, output, samples, attempts) = run(true, support);
            assert!(report.rejected_fits["reservation"] > 0);
            let reservations = report.cliff_reservations.unwrap();
            assert_eq!(
                report.rejected_fits["reservation"],
                reservations.before_fitting + reservations.after_transforms
            );
            if support == conditional {
                assert!(reservations.after_transforms > 0);
                assert_eq!(reservations.before_fitting, 0);
            }
            assert!(serde_json::to_value(&report).unwrap().get("cliff_reservations").is_none());
            assert!(!reference.admitted.is_empty());
            assert_eq!(reference_geometry, geometry);
            assert_eq!(reference.sampling.attempted, report.sampling.attempted);
            assert_eq!(reference.fitting_attempts, report.fitting_attempts);
            assert_eq!(reference_attempts, attempts);
            assert!(samples < reference_samples);
            assert_eq!(original.height, output.height);
            assert_eq!(original.topology, output.topology);
            assert_eq!(original.splat, output.splat);
        }
    }

    #[test]
    fn displaced_support_uses_actual_scale_when_the_upper_bound_is_too_large() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stage = stage();
        let tx = transaction_with_support(&layers, &alpha, [2., -0.25, 0.], 0.25);
        let scale = |largest_axis| Some(CliffScaleBounds { smallest_axis: 1., largest_axis });
        assert!(tx.origin_check(&stage, A, scale(1.)).unwrap().is_some());
        let (_, limit) = tx.origin_check(&stage, A, scale(2.)).unwrap().unwrap();
        assert!((1. ..2.).contains(&limit.unwrap()));
        assert!(tx.origin_check(&stage, A, None).unwrap().is_none());

        let mut prefab = prefabs()[A].metadata().clone();
        prefab.transforms.push(decor_fit::Transform::AlignSlope {
            normal: 1.,
            gradient: 1.,
            offset: [0.; 3],
            scale: [1., 1., 2.],
        });
        let prepared = PreparedPrefab::new(prefab).unwrap();
        assert!(cliff_scale_bounds(&prepared).is_none());
    }

    #[test]
    fn cliff_batches_preserve_admission_order_and_work() {
        let mut layers = layers();
        let hr = layers.height_resolution;
        for (i, height) in layers.height.iter_mut().enumerate() {
            *height = crate::erosion::encode(20. + (i % hr) as f32 * 0.01);
        }
        let n = layers.surface_resolution;
        let alpha = vec![255; n * n];
        let mut stage = stage();
        stage.distance = 50.;
        let prefabs = prefabs();
        let priorities = BTreeMap::from([(A.into(), 3), (B.into(), 3), (C.into(), 2)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let run = |pool| {
            let mut tx = transaction(&layers, &alpha);
            let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
            let mut attempts = Budget::new(10_000).unwrap();
            let mut next_pose = 0;
            let report = emit_with_cpu_pool(
                &stage,
                &priorities,
                42,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next_pose,
                    water: None,
                },
                pool,
                || false,
            )
            .unwrap()
            .unwrap();
            let output = tx.finish(|| false).unwrap().unwrap();
            (
                report,
                output,
                fitting.samples(),
                fitting.height_samples(),
                attempts.used(),
            )
        };
        let (serial_report, serial, serial_work, serial_heights, serial_attempts) = run(None);
        let (parallel_report, parallel, parallel_work, parallel_heights, parallel_attempts) =
            run(Some(&pool));
        let run_replay = || {
            with_forecast_replay(1024 * 1024, || Ok(Some(run(Some(&pool)))))
                .unwrap()
                .unwrap()
        };
        let _ = run_replay();
        let (replayed_report, replayed, replayed_work, replayed_heights, replayed_attempts) =
            run_replay();
        assert!(!serial_report.admitted.is_empty());
        let serial_report = serde_json::to_value(serial_report).unwrap();
        assert_eq!(
            serial_report,
            serde_json::to_value(parallel_report).unwrap()
        );
        assert_eq!(serial.height, parallel.height);
        assert_eq!(serial.topology, parallel.topology);
        assert_eq!(serial.splat, parallel.splat);
        assert_eq!(serde_json::to_value(replayed_report).unwrap(), serial_report);
        assert_eq!(replayed.height, serial.height);
        assert_eq!(replayed.topology, serial.topology);
        assert_eq!(replayed.splat, serial.splat);
        assert_eq!(
            (serial_work, serial_heights, serial_attempts),
            (parallel_work, parallel_heights, parallel_attempts)
        );
        assert_eq!(
            (serial_work, serial_heights, serial_attempts),
            (replayed_work, replayed_heights, replayed_attempts)
        );
    }

    #[test]
    fn cliff_batches_allow_cancellation_from_the_ordered_thread() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let tx = transaction(&layers, &alpha);
        let fitting = OrientedBudget::new(&layers, &alpha).unwrap();
        let stage = stage();
        let validated = decor_fit::ValidatedStage::new(&stage).unwrap();
        let batch = Batch {
            transaction: &tx,
            alpha: &alpha,
            stage: &validated,
            prefabs: &[],
            radii: &[],
            seed: 42,
            scan_neighbors: false,
            water: None,
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let cancelled = std::cell::Cell::new(false);
        let points = vec![
            Position {
                xz: [0.; 2],
                uv: [0.5; 2],
                threshold: 0.
            };
            64
        ];
        assert!(
            batch
                .predict_positions(&pool, &points, &fitting, &[], &|| {
                    cancelled.set(true);
                    true
                })
                .is_none()
        );
        assert!(cancelled.get());
        assert_eq!(fitting.samples(), 0);
    }

    #[test]
    fn cliff_batch_limits_preserve_ordered_budget_failures() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let mut stage = stage();
        stage.distance = 1.;
        let prefabs = prefabs();
        let priorities = BTreeMap::from([(A.into(), 3), (B.into(), 3), (C.into(), 2)]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();

        let run = |tight_attempts: bool, pool| {
            let mut tx = transaction(&layers, &alpha);
            let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
            if !tight_attempts {
                fitting = fitting.with_sample_limit(4).unwrap();
                assert!(fitting.charge_cliff_prediction(3, 0));
            }
            let mut attempts = Budget::new(if tight_attempts { 1 } else { 1024 }).unwrap();
            let mut next_pose = 0;
            let polls = std::cell::Cell::new(0);
            let error = emit_with_cpu_pool(
                &stage,
                &priorities,
                42,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next_pose,
                    water: None,
                },
                pool,
                || {
                    polls.set(polls.get() + 1);
                    false
                },
            )
            .err()
            .unwrap();
            assert!(tx.finish(|| false).is_err());
            assert_eq!(next_pose, 0);
            ((error, attempts.used(), fitting.samples()), polls.get())
        };

        let serial = run(true, None);
        let parallel = run(true, Some(&pool));
        assert_eq!(serial, parallel);
        let ((error, _, _), _) = parallel;
        assert!(error.contains("shared attempt allowance"));

        let (serial, _) = run(false, None);
        let (parallel, _) = run(false, Some(&pool));
        assert_eq!(serial, parallel);
        assert!(parallel.0.contains("Hill-cliff fitting exhausted"));
    }

    #[test]
    fn cancellation_budget_and_missing_metadata_prevent_partial_publication() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stage = stage();
        let prefabs = prefabs();
        for mode in 0..3 {
            let mut tx = transaction(&layers, &alpha);
            let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
            let mut attempts = Budget::new(if mode == 1 { 1 } else { 1000 }).unwrap();
            let mut next = 0;
            let priorities = if mode == 2 {
                BTreeMap::new()
            } else {
                BTreeMap::from([(A.into(), 3), (B.into(), 3), (C.into(), 2)])
            };
            let polls = std::cell::Cell::new(0);
            let result = emit(
                &stage,
                &priorities,
                42,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next,
                    water: None,
                },
                || {
                    polls.set(polls.get() + 1);
                    mode == 0 && polls.get() == 20
                },
            );
            if mode == 0 {
                assert!(result.unwrap().is_none());
                assert!(tx.finish(|| false).unwrap().is_none());
            } else {
                assert!(result.is_err());
                assert!(tx.finish(|| false).is_err());
            }
        }
    }
}
