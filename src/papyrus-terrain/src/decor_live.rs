//! Live sampling, static fitting and atomic admission for one decoration stage.
//! The caller keeps this same transaction and budgets for later workflow phases.
use crate::{
    decor::{Stage, StageReport},
    decor_fit::{
        self, OrientedBudget, OrientedOutcome, PreparedPrefab, RejectionCounts, StaticRequest,
        ValidatedWater,
    },
    decor_stream::{Advance, Budget, Stream},
    footprints::{OriginCheck, RoadTransaction},
};
use serde::Serialize;
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-live-decoration-4";
pub struct Context<'a, 'terrain> {
    pub transaction: &'a mut RoadTransaction<'terrain>,
    pub alpha: &'terrain [u8],
    pub fitting: &'a mut OrientedBudget<'terrain>,
    pub prefabs: &'a BTreeMap<String, PreparedPrefab>,
    pub attempts: &'a mut Budget,
    /// A shared ID allocator; every fitted admission attempt consumes one ID.
    pub next_pose: &'a mut usize,
    pub water: Option<ValidatedWater<'terrain>>,
}

/// Shared root-reservation shortcut for static and roadside prefab fitting.
pub(crate) fn origin_precheck_ready(prefab: &PreparedPrefab, has_water: bool) -> bool {
    let transforms = &prefab.metadata().transforms;
    let stationary = transforms.iter().all(|transform| match transform {
        decor_fit::Transform::AlignSlope { offset, .. } => *offset == [0.; 3],
        decor_fit::Transform::Offset { min, max } => {
            min[0] == 0. && min[2] == 0. && max[0] == 0. && max[2] == 0.
        }
        _ => true,
    });
    stationary
        && (has_water
            || !transforms.iter().any(|transform| matches!(transform, decor_fit::Transform::Swim)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        decor_height, decor_topology, footprints,
        native_layers::{Layers, dimensions},
    };
    const PATH: &str = "assets/bundled/prefabs/autospawn/decor/rocks/a.prefab";
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let mut splat = vec![0; n * n * 8];
        splat[..n * n].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![crate::erosion::encode(20.); hr * hr],
            biome: vec![51; n * n * 5],
            splat,
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: n * n,
        }
    }
    fn stage() -> Stage {
        serde_json::from_value(serde_json::json!({"id":"20","order":0,"name":"Rocks","kind":"uniform","folder":"decor/rocks","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":1,"TopologyAll":0,"TopologyNot":2},"filter_mode":1,"anchor_mode":0,"distance":100.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[PATH]})).unwrap()
    }
    fn prepared() -> BTreeMap<String, PreparedPrefab> {
        BTreeMap::from([(
            PATH.into(),
            PreparedPrefab::new(decor_fit::Prefab {
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
                transforms: vec![],
                anchors: vec![decor_fit::Anchor {
                    position: [0.; 3],
                    extents: 4.,
                    offset: 0.,
                    radius: 0.,
                    slope_scale: 0.,
                }],
                filters: vec![],
                unsupported: vec![],
                pending: vec![],
            })
            .unwrap(),
        )])
    }
    fn transaction<'a>(l: &'a Layers, alpha: &'a [u8]) -> RoadTransaction<'a> {
        let spec = footprints::Prefab {
            support: vec![footprints::Support {
                position: [0.; 3],
                radius: 0.,
            }],
            blockers: vec![],
            raises: vec![],
            topology: Some(decor_topology::Prefab {
                modifiers: vec![decor_topology::Modifier {
                    position: [0.; 3],
                    radius: 120.,
                    fade: 0.,
                    mask: 2,
                    operation: decor_topology::Operation::Add,
                }],
                queries: vec![],
                anchors: vec![],
            }),
            materials: Some(vec![]),
            height_modifiers: Some(vec![decor_height::Modifier {
                position: [0.; 3],
                radius: 60.,
                fade: 0.,
                opacity: 1.,
                mode: decor_height::Mode::Add { delta: 1. },
            }]),
            contributors: Some(vec![]),
            footprint: None,
            unsupported: vec![],
            pending: vec![],
        };
        RoadTransaction::new_objects(
            l,
            alpha,
            BTreeMap::from([(PATH.into(), spec)]),
            11,
            8,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn occupied_root_precheck_skips_fitting_unless_a_transform_moves_xz() {
        let mut l = layers();
        l.topology.fill(8);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage();
        s.filter.topology_any = 8;
        for (shifted, transform) in [
            (
                false,
                decor_fit::Transform::AlignSlope {
                    normal: 0.,
                    gradient: 0.,
                    offset: [0.; 3],
                    scale: [1.; 3],
                },
            ),
            (
                true,
                decor_fit::Transform::AlignSlope {
                    normal: 0.,
                    gradient: 0.,
                    offset: [1., 0., 0.],
                    scale: [1.; 3],
                },
            ),
            (
                false,
                decor_fit::Transform::Offset {
                    min: [0.; 3],
                    max: [0.; 3],
                },
            ),
            (
                false,
                decor_fit::Transform::Offset {
                    min: [0., -0.4, 0.],
                    max: [0., -0.7, 0.],
                },
            ),
            (
                true,
                decor_fit::Transform::Offset {
                    min: [1., 0., 0.],
                    max: [1., 0., 0.],
                },
            ),
            (
                true,
                decor_fit::Transform::Offset {
                    min: [0.; 3],
                    max: [1., 0., 0.],
                },
            ),
            (
                true,
                decor_fit::Transform::Offset {
                    min: [0., 0., 1.],
                    max: [0.; 3],
                },
            ),
        ] {
            let mut p = prepared()[PATH].metadata().clone();
            p.transforms.push(transform);
            let prefabs = BTreeMap::from([(PATH.into(), PreparedPrefab::new(p).unwrap())]);
            let mut tx = transaction(&l, &alpha);
            let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
            let mut attempts = Budget::new(1000).unwrap();
            let mut next = 0;
            let report = emit(
                &s,
                0,
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
            if shifted {
                assert!(!report.rejected_fits.contains_key("reservation"));
                assert!(fitting.samples() > 0);
                assert!(report.rejected_admissions > 0);
                assert!(tx.sample_visits() > 0);
            } else {
                assert_eq!(report.rejected_fits["reservation"], 100);
                assert_eq!(fitting.samples(), 0);
                assert_eq!(next, 0);
                assert_eq!(tx.sample_visits(), 0);
            }
            assert_eq!(tx.finish(|| false).unwrap().unwrap().height, l.height);
        }
    }
    #[test]
    fn live_emission_commits_before_later_sampling_and_retains_state_for_following_stages() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let prefabs = prepared();
        let run = || {
            let mut transaction = transaction(&l, &alpha);
            let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
            let mut attempts = Budget::new(1000).unwrap();
            let mut id = 0;
            let first = emit(
                &s,
                0,
                42,
                2,
                Context {
                    transaction: &mut transaction,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut id,
                    water: None,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(!first.admitted.is_empty());
            assert!(first.sampling.rejected_filter > 0);
            assert!(first.rejected_admissions > 0);
            let mut second = s.clone();
            second.id = "21".into();
            second.order = 1;
            let next = emit(
                &second,
                1,
                42,
                2,
                Context {
                    transaction: &mut transaction,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut id,
                    water: None,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(attempts.used(), 200);
            assert!(fitting.samples() > 0);
            let out = transaction.finish(|| false).unwrap().unwrap();
            assert_eq!(out.report.engine, footprints::OBJECT_VERSION);
            assert_eq!(
                out.report.counts.accepted,
                first.admitted.len() + next.admitted.len()
            );
            assert_ne!(out.height, l.height);
            assert_ne!(out.topology.as_ref().unwrap(), &l.topology);
            (first, next, out)
        };
        let (a, b, out) = run();
        let (c, d, replay) = run();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(c).unwrap()
        );
        assert_eq!(
            serde_json::to_value(b).unwrap(),
            serde_json::to_value(d).unwrap()
        );
        assert_eq!(out.height, replay.height);
        assert!(l.height.iter().all(|v| *v == crate::erosion::encode(20.)));
    }
    #[test]
    fn live_phase_cancellation_missing_metadata_and_work_exhaustion_poison_publication() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let prefabs = prepared();
        for mode in 0..3 {
            let mut transaction = transaction(&l, &alpha);
            let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
            let mut attempts = Budget::new(if mode == 2 { 1 } else { 1000 }).unwrap();
            let mut id = 0;
            let empty = BTreeMap::new();
            let counter = std::cell::Cell::new(0);
            let result = emit(
                &s,
                0,
                42,
                2,
                Context {
                    transaction: &mut transaction,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: if mode == 1 { &empty } else { &prefabs },
                    attempts: &mut attempts,
                    next_pose: &mut id,
                    water: None,
                },
                || {
                    counter.set(counter.get() + 1);
                    mode == 0 && counter.get() > 80
                },
            );
            if mode == 0 {
                assert!(result.unwrap().is_none());
                assert!(transaction.finish(|| false).unwrap().is_none());
            } else {
                assert!(result.is_err());
                assert!(transaction.finish(|| false).is_err());
            }
        }
        assert!(l.topology.iter().all(|v| *v == 1));
    }
}
#[derive(Serialize)]
pub struct Instance {
    pub transaction_pose: usize,
    pub prefab: String,
    pub transform: decor_fit::OrientedPose,
}
#[derive(Serialize)]
pub struct Report {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coastal: Option<crate::coastal_cliffs::PlacementReport>,
    #[serde(skip)]
    pub cliff_reservations: Option<crate::uniform_cliffs::ReservationCounts>,
    pub engine: &'static str,
    pub sampling: StageReport,
    pub fitting_attempts: usize,
    pub rejected_fits: BTreeMap<String, usize>,
    pub rejected_admissions: usize,
    pub admitted: Vec<Instance>,
    pub assembled_instances: usize,
    pub physics_validated: bool,
}
pub fn emit(
    stage: &Stage,
    index: usize,
    seed: u32,
    realm_mask: u32,
    mut context: Context<'_, '_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let result = emit_inner(stage, index, seed, realm_mask, &mut context, &cancelled);
    match &result {
        Ok(None) => context.transaction.cancel(),
        Err(_) => context.transaction.invalidate(),
        _ => {}
    }
    result
}
fn emit_inner(
    stage: &Stage,
    index: usize,
    seed: u32,
    realm_mask: u32,
    context: &mut Context<'_, '_>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if cancelled() {
        return Ok(None);
    }
    if index != stage.order {
        return Err("Live decoration stage index differs from its source roster".into());
    }
    let mut stream = Stream::new(
        stage,
        index,
        context.transaction.view().size,
        seed,
        realm_mask,
    )?;
    let validated_stage = decor_fit::ValidatedStage::new(stage)?.with_seed(seed);
    // Resolve each prefab on its first visit, preserving when missing metadata
    // would fail. Only the support policy is cached, never its live result.
    let mut resolved = vec![None::<(&PreparedPrefab, Option<OriginCheck>)>; stage.prefabs.len()];
    let mut fitting_attempts = 0;
    let mut rejected_fits = RejectionCounts::default();
    let mut rejected_admissions = 0;
    let mut admitted = Vec::new();
    loop {
        let candidate = match stream.next(
            context.transaction.view(),
            context.alpha,
            context.attempts,
            cancelled,
        )? {
            Advance::Candidate(candidate) => candidate,
            Advance::Complete => break,
            Advance::Cancelled => return Ok(None),
        };
        let path = &stage.prefabs[candidate.prefab];
        let entry = &mut resolved[candidate.prefab];
        let (prefab, origin) = match *entry {
            Some(resolved) => resolved,
            None => {
                let prefab = context
                    .prefabs
                    .get(path)
                    .ok_or("Missing live decoration fitting metadata")?;
                let origin = if origin_precheck_ready(prefab, context.water.is_some()) {
                    context
                        .transaction
                        .origin_check(stage, path, None)?
                        .map(|(check, _)| check)
                } else {
                    None
                };
                *entry.insert((prefab, origin))
            }
        };
        fitting_attempts += 1;
        // Root reservations depend on X/Z. Slope offsets and horizontal world
        // offsets can move that support, so those prefabs must fit first.
        if origin.is_some_and(|check| context.transaction.origin_conflicts(candidate.position, check))
        {
            rejected_fits.record(decor_fit::OrientedRejection::Reservation);
            continue;
        }
        let transform = match context.fitting.try_fit_static_prepared(
            context.transaction.view(),
            StaticRequest {
                candidate: &candidate,
                stage,
                prefab,
                seed,
                water: context.water,
            },
            &validated_stage,
        )? {
            OrientedOutcome::Fitted(pose) => pose,
            OrientedOutcome::Rejected(decor_fit::OrientedRejection::Budget) => {
                return Err(format!(
                    "Live decoration stage {} exhausted the shared fitting allowance after {} samples",
                    stage.name,
                    context.fitting.samples()
                ));
            }
            OrientedOutcome::Rejected(reason) => {
                rejected_fits.record(reason);
                continue;
            }
        };
        let id = *context.next_pose;
        *context.next_pose = id
            .checked_add(1)
            .ok_or("Live object admission ID overflow")?;
        let pose = decor_fit::Pose {
            candidate: id,
            position: transform.position,
            rotation: transform.rotation,
            scale: transform.scale,
        };
        match context
            .transaction
            .try_apply_validated(id, &validated_stage, candidate.prefab, &pose, cancelled)?
        {
            Some(true) => admitted.push(Instance {
                transaction_pose: id,
                prefab: path.clone(),
                transform,
            }),
            Some(false) => rejected_admissions += 1,
            None => return Ok(None),
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Report {
        coastal: None,
        cliff_reservations: None,
        engine: VERSION,
        sampling: stream.finish()?,
        fitting_attempts,
        rejected_fits: rejected_fits.finish(),
        rejected_admissions,
        admitted,
        assembled_instances: 0,
        physics_validated: false,
    }))
}
