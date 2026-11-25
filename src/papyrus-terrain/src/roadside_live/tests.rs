use super::*;
use crate::{
    decor_fit::{OrientedBudget, PreparedPrefab},
    decor_height,
    decor_stream::Budget,
    footprints::{self, RoadTransaction},
    native_layers::{Layers, dimensions},
    path_geometry::Path,
};

#[test]
fn path_descriptors_accept_sampler_ranges_and_preserve_static_stage_bounds() {
    let mut descriptor = stage();
    descriptor.folder = "decor/fixtures".into();
    descriptor.prefabs = vec!["assets/bundled/prefabs/autospawn/decor/fixtures/test.prefab".into()];
    assert!(descriptor.validate().is_ok());
    for (distance, dithering) in [(0.001, 0.), (100000., 10000.)] {
        descriptor.distance = distance;
        descriptor.dithering = dithering;
        assert!(descriptor.validate_path_attached().is_ok());
        assert!(descriptor.validate().is_err());
    }
    descriptor.dithering = 0.;
    for distance in [0., -1., f32::NAN, 100001.] {
        descriptor.distance = distance;
        assert!(descriptor.validate_path_attached().is_err());
    }
}

const PREFAB: &str = "assets/bundled/prefabs/autospawn/monument/maintainables/fixture.prefab";

#[test]
fn shared_fitting_exhaustion_fails_the_stage_instead_of_publishing_an_incomplete_pass() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let stage = stage();
    let path = path(0.);
    let roads = [Road {
        hierarchy: 0,
        width: 0.,
        spline: false,
        path: &path,
    }];
    let prefabs = prefabs();
    let mut transaction = transaction(&layers, &alpha, false);
    let mut fitting = OrientedBudget::new(&layers, &alpha)
        .unwrap()
        .with_sample_limit(4)
        .unwrap();
    let mut attempts = Budget::new(10000).unwrap();
    let mut next = 0;
    let result = emit(
        Request {
            stage: &stage,
            sampling: settings(),
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |_, _| Ok(true),
        || false,
    );
    assert!(result.is_err());
    assert_eq!(fitting.samples(), 4);
    assert!(transaction.finish(|| false).is_err());
}
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
        normalized_biome_cells: n * n,
        normalized_splat_cells: n * n,
    }
}
fn stage() -> Stage {
    serde_json::from_value(serde_json::json!({"id":"1","order":0,"name":"Pipes","kind":"uniform",
        "folder":"monument/maintainables","enabled":true,"mode":-1,
        "filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":1,"TopologyAll":0,"TopologyNot":0},
        "filter_mode":2,"anchor_mode":0,"distance":200.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[PREFAB]})).unwrap()
}
fn settings() -> Settings {
    Settings {
        road_mode: 5,
        distance: 200.,
        spacing_fraction: 0.25,
        side_offset: 20.,
        dithering: 0.,
        placement_attempts: 1,
    }
}
fn path(x: f32) -> Path {
    Path::from_controls(&[[x, 20., -350.], [x, 20., 350.]], 1000, || false)
        .unwrap()
        .unwrap()
}
fn prefabs() -> BTreeMap<String, PreparedPrefab> {
    BTreeMap::from([(
        PREFAB.into(),
        PreparedPrefab::new(decor_fit::Prefab {
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
            transforms: vec![decor_fit::Transform::Align {
                normal: 1.,
                gradient: 0.,
            }],
            anchors: vec![],
            filters: vec![],
            unsupported: vec![],
            pending: vec![],
        })
        .unwrap(),
    )])
}
fn transaction<'a>(layers: &'a Layers, alpha: &'a [u8], raises: bool) -> RoadTransaction<'a> {
    let prefab = footprints::Prefab {
        support: vec![footprints::Support {
            position: [0.; 3],
            radius: 0.,
        }],
        blockers: vec![],
        raises: vec![],
        topology: Some(Default::default()),
        materials: Some(vec![]),
        height_modifiers: Some(if raises {
            vec![decor_height::Modifier {
                position: [0.; 3],
                radius: 80.,
                fade: 0.,
                opacity: 1.,
                mode: decor_height::Mode::Add { delta: 1. },
            }]
        } else {
            vec![]
        }),
        contributors: Some(vec![]),
        footprint: None,
        unsupported: vec![],
        pending: vec![],
    };
    RoadTransaction::new_objects(
        layers,
        alpha,
        BTreeMap::from([(PREFAB.into(), prefab)]),
        3,
        2,
        || false,
    )
    .unwrap()
    .unwrap()
}
fn context<'a, 'terrain>(
    transaction: &'a mut RoadTransaction<'terrain>,
    alpha: &'terrain [u8],
    fitting: &'a mut OrientedBudget<'terrain>,
    prefabs: &'a BTreeMap<String, PreparedPrefab>,
    attempts: &'a mut Budget,
    next_pose: &'a mut usize,
) -> Context<'a, 'terrain> {
    Context {
        transaction,
        alpha,
        fitting,
        prefabs,
        attempts,
        next_pose,
        water: None,
    }
}

#[test]
fn later_candidates_fit_against_the_terrain_written_by_earlier_admissions() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let stage = stage();
    let path = path(0.);
    let roads = [Road {
        hierarchy: 0,
        width: 0.,
        spline: false,
        path: &path,
    }];
    let prefabs = prefabs();
    let mut prefabs = prefabs;
    let mut moving = prefabs.remove(PREFAB).unwrap().metadata().clone();
    moving.transforms.push(decor_fit::Transform::Offset {
        min: [1., 0., 0.],
        max: [1., 0., 0.],
    });
    prefabs.insert(PREFAB.into(), PreparedPrefab::new(moving).unwrap());
    let mut transaction = transaction(&layers, &alpha, true);
    let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
    let mut attempts = Budget::new(10000).unwrap();
    let mut next = 0;
    let mut fitted_heights = Vec::new();
    let report = emit(
        Request {
            stage: &stage,
            sampling: settings(),
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |pose, view| {
            let height = spawn::sample_height_view(
                view,
                pose.position[0] / 1000. + 0.5,
                pose.position[2] / 1000. + 0.5,
            )?;
            assert!((height - pose.position[1]).abs() < 0.1);
            fitted_heights.push(pose.position[1]);
            Ok(true)
        },
        || false,
    )
    .unwrap()
    .unwrap();
    assert!(!report.admitted.is_empty());
    assert!(fitted_heights.len() > 1);
    assert!(
        fitted_heights[1] > fitted_heights[0] + 0.5,
        "heights={fitted_heights:?}, admitted={}",
        serde_json::to_string(&report.admitted).unwrap()
    );
    assert_eq!(report.sampling.accepted, report.admitted.len());
    let output = transaction.finish(|| false).unwrap().unwrap();
    assert!(output.height.iter().zip(&layers.height).any(|(a, b)| a > b));
}

#[test]
fn consecutive_stages_share_reservations_fitting_sampling_and_admission_ids() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let first = stage();
    let mut second = stage();
    second.id = "2".into();
    second.order = 1;
    let path = path(0.);
    let roads = [Road {
        hierarchy: 0,
        width: 0.,
        spline: false,
        path: &path,
    }];
    let prefabs = prefabs();
    let mut transaction = transaction(&layers, &alpha, false);
    let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
    let mut attempts = Budget::new(10000).unwrap();
    let mut next = 0;
    let first = emit(
        Request {
            stage: &first,
            sampling: settings(),
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |_, _| Ok(true),
        || false,
    )
    .unwrap()
    .unwrap();
    let before = next;
    let samples = fitting.samples();
    let work = attempts.used();
    let mut later = settings();
    later.side_offset = 40.;
    let second = emit(
        Request {
            stage: &second,
            sampling: later,
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |_, _| Ok(true),
        || false,
    )
    .unwrap()
    .unwrap();
    assert!(before > 0 && next > before);
    assert!(fitting.samples() > samples);
    assert!(attempts.used() > work);
    assert_eq!(second.admitted[0].transaction_pose, before);
    assert_eq!(next, first.admitted.len() + second.admitted.len());
    assert_eq!(
        transaction
            .finish(|| false)
            .unwrap()
            .unwrap()
            .report
            .counts
            .accepted,
        next
    );
}

#[test]
fn rejected_location_checks_preserve_sampler_rng_and_leave_the_terrain_unmodified() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let stage = stage();
    let path = path(0.);
    let roads = [Road {
        hierarchy: 0,
        width: 0.,
        spline: false,
        path: &path,
    }];
    let prefabs = prefabs();
    let mut transaction = transaction(&layers, &alpha, false);
    let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
    let mut attempts = Budget::new(10000).unwrap();
    let mut next = 0;
    let report = emit(
        Request {
            stage: &stage,
            sampling: settings(),
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |_, _| Ok(false),
        || false,
    )
    .unwrap()
    .unwrap();
    let reference = roadside_decor::sample(
        &roads,
        settings(),
        1,
        42,
        &mut Budget::new(10000).unwrap(),
        |_| Ok(true),
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(report.sampling.final_seed, reference.final_seed);
    assert!(report.rejected_checks > 0);
    assert!(report.admitted.is_empty());
    assert_eq!(next, 0);
    let output = transaction.finish(|| false).unwrap().unwrap();
    assert_eq!(output.height, layers.height);
    assert_eq!(output.topology.as_ref(), Some(&layers.topology));
}

#[test]
fn checker_errors_and_sampling_exhaustion_invalidate_private_partial_admissions() {
    for exhaust in [false, true] {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stage = stage();
        let path = path(0.);
        let roads = [Road {
            hierarchy: 0,
            width: 0.,
            spline: false,
            path: &path,
        }];
        let prefabs = prefabs();
        let mut transaction = transaction(&layers, &alpha, false);
        let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
        let mut attempts = Budget::new(if exhaust { 8 } else { 10000 }).unwrap();
        let mut next = 0;
        let mut checks = 0;
        let result = emit(
            Request {
                stage: &stage,
                sampling: settings(),
                roads: &roads,
                seed: 42,
                realm_mask: 2,
            },
            context(
                &mut transaction,
                &alpha,
                &mut fitting,
                &prefabs,
                &mut attempts,
                &mut next,
            ),
            |_, _| {
                checks += 1;
                if !exhaust && checks == 2 {
                    Err("fixture checker failure".into())
                } else {
                    Ok(true)
                }
            },
            || false,
        );
        assert!(result.is_err());
        assert!(next > 0);
        assert!(transaction.finish(|| false).is_err());
    }
}

#[test]
fn cancellation_after_partial_admission_prevents_transaction_publication() {
    for checker_error in [false, true] {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stage = stage();
        let path = path(0.);
        let roads = [Road {
            hierarchy: 0,
            width: 0.,
            spline: false,
            path: &path,
        }];
        let prefabs = prefabs();
        let mut transaction = transaction(&layers, &alpha, false);
        let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
        let mut attempts = Budget::new(10000).unwrap();
        let mut next = 0;
        let mut checks = 0;
        let cancelled = Cell::new(false);
        let result = emit(
            Request {
                stage: &stage,
                sampling: settings(),
                roads: &roads,
                seed: 42,
                realm_mask: 2,
            },
            context(
                &mut transaction,
                &alpha,
                &mut fitting,
                &prefabs,
                &mut attempts,
                &mut next,
            ),
            |_, _| {
                checks += 1;
                if checks == 2 {
                    cancelled.set(true);
                    if checker_error {
                        return Err("fixture cancelled query".into());
                    }
                }
                Ok(true)
            },
            || cancelled.get(),
        )
        .unwrap();
        assert!(result.is_none());
        assert!(next > 0);
        assert!(transaction.finish(|| false).unwrap().is_none());
    }
}

#[test]
fn disabled_stages_skip_work_and_mismatched_descriptors_fail_before_sampling() {
    for disabled in [true, false] {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let mut stage = stage();
        stage.enabled = !disabled;
        let path = path(0.);
        let roads = [Road {
            hierarchy: 0,
            width: 0.,
            spline: false,
            path: &path,
        }];
        let prefabs = prefabs();
        let mut transaction = transaction(&layers, &alpha, false);
        let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
        let mut attempts = Budget::new(10000).unwrap();
        let mut next = 0;
        let mut sampling = settings();
        if !disabled {
            sampling.distance += 1.;
        }
        let result = emit(
            Request {
                stage: &stage,
                sampling,
                roads: &roads,
                seed: 42,
                realm_mask: 2,
            },
            context(
                &mut transaction,
                &alpha,
                &mut fitting,
                &prefabs,
                &mut attempts,
                &mut next,
            ),
            |_, _| panic!("Unexpected fitting callback"),
            || false,
        );
        assert_eq!(attempts.used(), 0);
        assert_eq!(next, 0);
        if disabled {
            assert!(result.unwrap().unwrap().skipped.is_some());
            assert!(transaction.finish(|| false).unwrap().is_some());
        } else {
            assert!(result.is_err());
            assert!(transaction.finish(|| false).is_err());
        }
    }
}

#[test]
fn candidates_outside_the_world_are_rejected_without_aborting_other_placements() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let stage = stage();
    let path = path(490.);
    let roads = [Road {
        hierarchy: 0,
        width: 40.,
        spline: false,
        path: &path,
    }];
    let prefabs = prefabs();
    let mut transaction = transaction(&layers, &alpha, false);
    let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
    let mut attempts = Budget::new(10000).unwrap();
    let mut next = 0;
    let report = emit(
        Request {
            stage: &stage,
            sampling: settings(),
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |_, _| Ok(true),
        || false,
    )
    .unwrap()
    .unwrap();
    assert!(report.rejected_bounds > 0);
    assert!(!report.admitted.is_empty());
    assert!(
        report
            .admitted
            .iter()
            .all(|instance| instance.transform.position[0].abs() <= 500.)
    );
    assert!(transaction.finish(|| false).unwrap().is_some());
}

#[test]
fn horizontal_offsets_fit_before_the_occupied_origin_shortcut() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let stage = stage();
    let path = path(0.);
    let roads = [Road {
        hierarchy: 0,
        width: 0.,
        spline: false,
        path: &path,
    }];
    let mut first = None;
    roadside_decor::sample(
        &roads,
        settings(),
        1,
        42,
        &mut Budget::new(10000).unwrap(),
        |candidate| {
            first.get_or_insert(candidate.position_xz);
            Ok(true)
        },
        || false,
    )
    .unwrap()
    .unwrap();
    let [x, z] = first.unwrap();
    let mut transaction = transaction(&layers, &alpha, false);
    assert_eq!(
        transaction
            .try_apply(
                0,
                &stage,
                PREFAB,
                &decor_fit::Pose {
                    candidate: 0,
                    position: [x, 20., z],
                    rotation: [0., 0., 0., 1.],
                    scale: [1.; 3]
                },
                || false
            )
            .unwrap(),
        Some(true)
    );
    let mut fitting = OrientedBudget::new(&layers, &alpha).unwrap();
    let mut prefabs = prefabs();
    let mut prefab = prefabs.remove(PREFAB).unwrap().metadata().clone();
    prefab.transforms.push(decor_fit::Transform::Offset {
        min: [30., 0., 0.],
        max: [30., 0., 0.],
    });
    prefabs.insert(PREFAB.into(), PreparedPrefab::new(prefab).unwrap());
    let mut attempts = Budget::new(10000).unwrap();
    let mut next = 1;
    let report = emit(
        Request {
            stage: &stage,
            sampling: settings(),
            roads: &roads,
            seed: 42,
            realm_mask: 2,
        },
        context(
            &mut transaction,
            &alpha,
            &mut fitting,
            &prefabs,
            &mut attempts,
            &mut next,
        ),
        |_, _| Ok(true),
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(report.admitted[0].transform.position[0], x + 30.);
    assert!(transaction.finish(|| false).unwrap().is_some());
}
