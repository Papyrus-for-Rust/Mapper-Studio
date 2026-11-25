use super::*;
use std::cell::Cell;
fn body(id: i64, kind: i32) -> WaterBody {
    WaterBody {
        object_id: id,
        body_type: kind,
        component_enabled: false,
        hierarchy_active_without_root: true,
        position: [100., -300., 200.],
        triggers: vec![],
    }
}
fn facts(bodies: Vec<WaterBody>) -> WaterPlacement {
    WaterPlacement {
        version: 1,
        bodies,
        issues: vec![],
    }
}
#[test]
fn unknown_capture_runtime_changes_and_issues_never_become_non_lake() {
    let model = Model::client_reference().unwrap();
    let mut data = facts(vec![]);
    for (capture, state) in [
        (None, LookupState::AuthoredUnchanged),
        (Some(&data), LookupState::Unmodeled),
    ] {
        assert_eq!(
            model
                .assess(
                    capture,
                    state,
                    Selection::None,
                    100,
                    || false,
                    |_| panic!(),
                    |_| panic!()
                )
                .unwrap()
                .unwrap()
                .water,
            WaterGate::Unknown
        );
    }
    data.issues.push("Incomplete hierarchy".into());
    assert_eq!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::None,
                100,
                || false,
                |_| panic!(),
                |_| panic!()
            )
            .unwrap()
            .unwrap()
            .water,
        WaterGate::Unknown
    );
}
#[test]
fn inspected_enum_and_confirmed_selection_control_the_branch_not_names_or_enabled_flags() {
    let model = Model::client_reference().unwrap();
    let mut data = facts(vec![body(10, 16)]);
    let assess = |data: &WaterPlacement, selected| {
        model
            .assess(
                Some(data),
                LookupState::AuthoredUnchanged,
                selected,
                100,
                || false,
                |_| panic!(),
                |_| panic!(),
            )
            .unwrap()
            .unwrap()
    };
    assert_eq!(assess(&data, Selection::Unknown).water, WaterGate::NotLake);
    data.bodies.push(body(20, 4));
    assert_eq!(assess(&data, Selection::Unknown).water, WaterGate::Unknown);
    assert_eq!(assess(&data, Selection::Body(10)).water, WaterGate::NotLake);
    assert_eq!(assess(&data, Selection::None).water, WaterGate::NotLake);
    assert!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(99),
                100,
                || false,
                |_| panic!(),
                |_| panic!()
            )
            .is_err()
    );
    data.bodies[0].hierarchy_active_without_root = false;
    assert!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                100,
                || false,
                |_| panic!(),
                |_| panic!()
            )
            .is_err()
    );
}
fn lake() -> WaterPlacement {
    let mut lake = body(10, 4);
    lake.triggers = vec![
        WaterTrigger {
            file_id: 0,
            path_id: 0,
        },
        WaterTrigger {
            file_id: 0,
            path_id: 22,
        },
        WaterTrigger {
            file_id: 0,
            path_id: 22,
        },
        WaterTrigger {
            file_id: 3,
            path_id: -17,
        },
        WaterTrigger {
            file_id: 0,
            path_id: 64,
        },
    ];
    facts(vec![lake])
}
#[test]
fn empty_trigger_list_keeps_runtime_body_height_and_equal_levels_keep_source_order() {
    let model = Model::client_reference().unwrap();
    let mut data = facts(vec![body(10, 4)]);
    let result = model
        .assess(
            Some(&data),
            LookupState::AuthoredUnchanged,
            Selection::Body(10),
            100,
            || false,
            |_| Ok(Some(7.)),
            |_| panic!(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(result.water, WaterGate::Lake { minimum_level: 7. });
    data.bodies[0].triggers = vec![
        WaterTrigger {
            file_id: 0,
            path_id: 1,
        },
        WaterTrigger {
            file_id: 0,
            path_id: 2,
        },
    ];
    let result = model
        .assess(
            Some(&data),
            LookupState::AuthoredUnchanged,
            Selection::Body(10),
            100,
            || false,
            |_| Ok(Some(0.)),
            |r| {
                Ok(Trigger::BoundsMaximumY(if r.path_id == 1 {
                    -0.
                } else {
                    0.
                }))
            },
        )
        .unwrap()
        .unwrap();
    let WaterGate::Lake { minimum_level } = result.water else {
        panic!()
    };
    assert_eq!(minimum_level.to_bits(), 0_f32.to_bits());
}
#[test]
fn minimum_uses_runtime_frame_bounds_and_preserves_null_duplicate_and_external_references() {
    let data = lake();
    let model = Model::client_reference().unwrap();
    let mut calls = vec![];
    let result = model
        .assess(
            Some(&data),
            LookupState::AuthoredUnchanged,
            Selection::Body(10),
            100,
            || false,
            |b| {
                assert_eq!(b.position[1], -300.);
                Ok(Some(10.))
            },
            |r| {
                calls.push((r.file_id, r.path_id));
                Ok(if r.path_id == 64 {
                    Trigger::Null
                } else if r.file_id == 3 {
                    Trigger::BoundsMaximumY(2.)
                } else {
                    Trigger::BoundsMaximumY(5.)
                })
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(calls, [(0, 22), (0, 22), (3, -17), (0, 64)]);
    assert_eq!(result.water, WaterGate::Lake { minimum_level: 2. });
    assert_eq!(result.work, 8);
    let exact = model
        .assess(
            Some(&data),
            LookupState::AuthoredUnchanged,
            Selection::Body(10),
            8,
            || false,
            |_| Ok(Some(10.)),
            |_| Ok(Trigger::BoundsMaximumY(2.)),
        )
        .unwrap()
        .unwrap();
    assert_eq!(exact.water, result.water);
    assert!(matches!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                7,
                || false,
                |_| Ok(Some(10.)),
                |_| Ok(Trigger::BoundsMaximumY(2.))
            )
            .unwrap(),
        Err(Verdict::Budget)
    ));
}
#[test]
fn unresolved_bounds_or_missing_body_height_preserve_unknown_without_partial_minima() {
    let data = lake();
    let model = Model::client_reference().unwrap();
    let unknown = model
        .assess(
            Some(&data),
            LookupState::AuthoredUnchanged,
            Selection::Body(10),
            100,
            || false,
            |_| Ok(Some(0.)),
            |r| {
                Ok(if r.file_id == 3 {
                    Trigger::Unknown
                } else {
                    Trigger::BoundsMaximumY(-5.)
                })
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(unknown.water, WaterGate::Unknown);
    assert_eq!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                100,
                || false,
                |_| Ok(None),
                |_| panic!()
            )
            .unwrap()
            .unwrap()
            .water,
        WaterGate::Unknown
    );
    assert!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                100,
                || false,
                |_| Ok(Some(f32::NAN)),
                |_| panic!()
            )
            .is_err()
    );
    assert_eq!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                100,
                || false,
                |_| Ok(Some(10.)),
                |_| Err("missing collider provider".into())
            )
            .unwrap_err(),
        "missing collider provider"
    );
}
#[test]
fn cancellation_inside_callbacks_cannot_return_a_water_decision() {
    let data = lake();
    let model = Model::client_reference().unwrap();
    let stop = Cell::new(false);
    assert!(matches!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                100,
                || stop.get(),
                |_| {
                    stop.set(true);
                    Ok(None)
                },
                |_| panic!()
            )
            .unwrap(),
        Err(Verdict::Cancelled)
    ));
    stop.set(false);
    assert!(matches!(
        model
            .assess(
                Some(&data),
                LookupState::AuthoredUnchanged,
                Selection::Body(10),
                100,
                || stop.get(),
                |_| Ok(Some(10.)),
                |_| {
                    stop.set(true);
                    Ok(Trigger::BoundsMaximumY(1.))
                }
            )
            .unwrap(),
        Err(Verdict::Cancelled)
    ));
}
