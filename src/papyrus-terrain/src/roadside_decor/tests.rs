use super::*;
use std::cell::Cell;

fn settings() -> Settings {
    Settings {
        road_mode: 5,
        distance: 800.,
        spacing_fraction: 0.25,
        side_offset: 7.,
        dithering: 10.,
        placement_attempts: 8,
    }
}

fn path() -> Path {
    Path::from_controls(&[[0., 0., 0.], [0., 0., 1000.]], 1000, || false)
        .unwrap()
        .unwrap()
}

fn road(path: &Path) -> Road<'_> {
    Road {
        hierarchy: 0,
        width: 12.,
        spline: true,
        path,
    }
}

#[test]
fn spacing_width_jitter_and_one_success_per_station_follow_the_supplied_policy() {
    let path = path();
    let mut candidates = Vec::new();
    let report = sample(
        &[road(&path)],
        settings(),
        3,
        42,
        &mut Budget::new(1000).unwrap(),
        |candidate| {
            candidates.push(candidate);
            Ok(true)
        },
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!((report.roads, report.stations, report.attempts), (1, 4, 64));
    assert_eq!((report.evaluated, report.accepted, report.work), (4, 4, 69));
    assert_eq!(
        candidates.iter().map(|c| c.distance).collect::<Vec<_>>(),
        [200., 400., 600., 800.]
    );
    for candidate in candidates {
        assert!(candidate.prefab < 3);
        assert!((13. ..=23.).contains(&candidate.position_xz[0].abs()));
        let base = path.point(candidate.distance).unwrap();
        assert!((candidate.position_xz[1] - base[2]).abs() <= 10.);
        assert_eq!(candidate.attempt, 0);
    }
}

#[test]
fn acceptance_suppresses_callbacks_without_changing_later_station_randomness() {
    let path = path();
    let run = |accepted| {
        let mut candidates = Vec::new();
        let report = sample(
            &[road(&path)],
            settings(),
            7,
            12345,
            &mut Budget::new(1000).unwrap(),
            |candidate| {
                candidates.push(candidate);
                Ok(accepted)
            },
            || false,
        )
        .unwrap()
        .unwrap();
        (candidates, report)
    };
    let (accepted, a) = run(true);
    let (rejected, b) = run(false);
    assert_eq!(a.final_seed, b.final_seed);
    assert_eq!(a.attempts, b.attempts);
    assert_eq!(accepted.len(), 4);
    assert_eq!(rejected.len(), 64);
    for candidate in accepted {
        assert_eq!(candidate, rejected[candidate.station * 16]);
    }
}

#[test]
fn opposite_side_is_tried_after_failed_first_side_and_prefab_is_station_scoped() {
    let path = path();
    let mut candidates = Vec::new();
    let report = sample(
        &[road(&path)],
        settings(),
        5,
        27,
        &mut Budget::new(1000).unwrap(),
        |candidate| {
            let position = candidates.len() % 9;
            candidates.push(candidate);
            Ok(position == 8)
        },
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!((report.accepted, report.evaluated), (4, 36));
    for station in candidates.chunks(9) {
        assert!(
            station
                .iter()
                .all(|candidate| candidate.prefab == station[0].prefab)
        );
        assert!(
            station[..8]
                .iter()
                .all(|candidate| candidate.side == station[0].side)
        );
        assert_eq!(station[8].side, -station[0].side);
        assert_eq!(station[8].attempt, 0);
    }
}

#[test]
fn all_six_source_road_modes_reuse_the_existing_hierarchy_rules() {
    let path = path();
    let expected = [
        vec![0, 1],
        vec![1],
        vec![0],
        vec![1, 2],
        vec![2],
        vec![0, 1, 2],
    ];
    for (mode, expected) in expected.iter().enumerate() {
        let roads: Vec<_> = (0..4)
            .map(|hierarchy| Road {
                hierarchy,
                ..road(&path)
            })
            .collect();
        let mut selected = Vec::new();
        sample(
            &roads,
            Settings {
                road_mode: mode as u8,
                ..settings()
            },
            1,
            42,
            &mut Budget::new(1000).unwrap(),
            |candidate| {
                selected.push(candidate.road);
                Ok(true)
            },
            || false,
        )
        .unwrap()
        .unwrap();
        selected.dedup();
        assert_eq!(&selected, expected);
    }
}

#[test]
fn trim_ranges_and_both_position_interpolators_are_retained() {
    let mut path = Path::from_controls(
        &[
            [0., 0., 0.],
            [100., 20., 200.],
            [-100., 0., 800.],
            [0., 0., 1000.],
        ],
        1000,
        || false,
    )
    .unwrap()
    .unwrap();
    path.set_range(1, 2).unwrap();
    assert_eq!(path.point_linear(-100.).unwrap(), path.points()[1]);
    assert_eq!(path.point_linear(100000.).unwrap(), path.points()[2]);
    assert!(path.point_linear(f32::NAN).is_err());
    for spline in [false, true] {
        let sampling = Settings {
            distance: 100.,
            dithering: 0.,
            side_offset: 0.,
            ..settings()
        };
        let report = sample(
            &[Road {
                spline,
                width: 0.,
                ..road(&path)
            }],
            sampling,
            1,
            0,
            &mut Budget::new(1000).unwrap(),
            |candidate| {
                let point = if spline {
                    path.point(candidate.distance)
                } else {
                    path.point_linear(candidate.distance)
                }
                .unwrap();
                assert_eq!(candidate.position_xz, [point[0], point[2]]);
                assert!(candidate.distance >= path.offsets().0 + 25.);
                assert!(candidate.distance <= path.length() - path.offsets().1 - 25.);
                Ok(true)
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(report.stations > 0);
    }
    let distance = path.length() * 0.4;
    assert_ne!(
        path.point(distance).unwrap(),
        path.point_linear(distance).unwrap()
    );
}

#[test]
fn zero_attempts_and_tiny_spacing_remain_bounded() {
    let path = path();
    let report = sample(
        &[road(&path)],
        Settings {
            placement_attempts: 0,
            ..settings()
        },
        1,
        42,
        &mut Budget::new(10).unwrap(),
        |_| panic!("No attempts expected"),
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!((report.stations, report.attempts, report.work), (4, 0, 5));
    assert_ne!(report.final_seed, 42);
    let result = sample(
        &[road(&path)],
        Settings {
            distance: 0.001,
            ..settings()
        },
        1,
        42,
        &mut Budget::new(10).unwrap(),
        |_| Ok(false),
        || false,
    );
    assert!(matches!(result, Err(Failure::Budget)));
}

#[test]
fn cancellation_and_callback_errors_cannot_publish_a_completed_report() {
    let path = path();
    let cancelled = Cell::new(false);
    let result = sample(
        &[road(&path)],
        settings(),
        1,
        42,
        &mut Budget::new(1000).unwrap(),
        |_| {
            cancelled.set(true);
            Ok(true)
        },
        || cancelled.get(),
    )
    .unwrap();
    assert!(result.is_none());
    let result = sample(
        &[road(&path)],
        settings(),
        1,
        42,
        &mut Budget::new(1000).unwrap(),
        |_| Err("Fit failed".into()),
        || false,
    );
    assert!(matches!(result, Err(Failure::Evaluation(message)) if message == "Fit failed"));
    assert!(
        sample(
            &[],
            settings(),
            1,
            42,
            &mut Budget::new(1).unwrap(),
            |_| Ok(false),
            || true
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn invalid_late_road_or_settings_are_rejected_before_any_callback() {
    let path = path();
    let roads = [
        road(&path),
        Road {
            width: f32::NAN,
            ..road(&path)
        },
    ];
    assert!(matches!(
        sample(
            &roads,
            settings(),
            1,
            42,
            &mut Budget::new(1000).unwrap(),
            |_| panic!("Input validation must precede fitting"),
            || false
        ),
        Err(Failure::Invalid(_))
    ));
    for bad in [
        Settings {
            road_mode: 6,
            ..settings()
        },
        Settings {
            distance: 0.,
            ..settings()
        },
        Settings {
            spacing_fraction: 0.,
            ..settings()
        },
        Settings {
            dithering: f32::NAN,
            ..settings()
        },
        Settings {
            placement_attempts: 1025,
            ..settings()
        },
    ] {
        assert!(bad.validate().is_err());
    }
    assert!(matches!(
        sample(
            &[],
            settings(),
            0,
            42,
            &mut Budget::new(1000).unwrap(),
            |_| Ok(false),
            || false
        ),
        Err(Failure::Invalid(_))
    ));
}
