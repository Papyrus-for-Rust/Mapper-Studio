use super::*;
use papyrus_terrain::environment::{Verdict, rotate};
use std::cell::Cell;
fn attributes() -> Attributes {
    serde_json::from_value(serde_json::json!({
        "version":3,"root_decor":[
            {"object_id":1,"active":false,"kind":"DecorRotate","values":[0,-180,0,0,180,0]},
            {"object_id":2,"active":true,"kind":"DecorSwim","values":[]}],
        "terrain_checks":[],"water_checks":[],"path_connectors":[],"placement_blockers":[],"issues":[],
        "environment":{"root_transform":{"position":[123,456,789],"rotation":[0,0,0,1],"scale":[1,1,1]},
            "volumes":[],"checks":[],"collider_meshes":[],"issues":[]}
    })).unwrap()
}
fn close(a: [f32; 3], b: [f32; 3]) {
    assert!(
        a.into_iter().zip(b).all(|(a, b)| (a - b).abs() < 0.00001),
        "{a:?} != {b:?}"
    );
}
#[test]
fn installed_oilrig_program_uses_water_and_retains_inactive_root_decorators() {
    let program = Program::new(&attributes()).unwrap();
    let draws = Cell::new(0);
    let pose = program
        .apply(
            [2400., -80., 33.],
            -500.,
            program.required_work(),
            || false,
            |point, index| {
                assert_eq!(point, [2400., -80., 33.]);
                assert_eq!(index, 0);
                draws.set(draws.get() + 1);
                Ok(0.75)
            },
            |point| {
                assert_eq!(point, [2400., -80., 33.]);
                Ok(7.)
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(pose.position, [2400., 7., 33.]);
    assert_eq!(pose.scale, [1.; 3]);
    close(rotate(pose.rotation, [0., 0., 1.]), [1., 0., 0.]);
    assert_eq!(draws.get(), 1);
}
#[test]
fn rotation_pre_multiplies_authored_frame_and_preserves_scale_and_program_order() {
    let mut data = attributes();
    data.root_decor.swap(0, 1);
    let root = data
        .environment
        .as_mut()
        .unwrap()
        .root_transform
        .as_mut()
        .unwrap();
    let (s, c) = std::f32::consts::FRAC_PI_4.sin_cos();
    root.rotation = [s, 0., 0., c];
    root.scale = [2., 3., 4.];
    let program = Program::new(&data).unwrap();
    let pose = program
        .apply(
            [2., -100., 4.],
            -500.,
            100,
            || false,
            |point, index| {
                assert_eq!(point, [2., 12., 4.]);
                assert_eq!(index, 1);
                Ok(0.75)
            },
            |_| Ok(12.),
        )
        .unwrap()
        .unwrap();
    close(rotate(pose.rotation, [0., 1., 0.]), [1., 0., 0.]);
    assert_eq!(pose.scale, [2., 3., 4.]);
}
#[test]
fn swim_fallback_uses_inclusive_terrain_origin_threshold() {
    let mut data = attributes();
    data.root_decor.remove(0);
    let program = Program::new(&data).unwrap();
    for (water, origin, expected) in [
        (3., 3., 0.),
        (4., 3., 0.),
        (4.001, 3., 4.001),
        (0., -500., 0.),
        (5., -500., 5.),
    ] {
        let pose = program
            .apply(
                [0., -90., 0.],
                origin,
                100,
                || false,
                |_, _| panic!(),
                |_| Ok(water),
            )
            .unwrap()
            .unwrap();
        assert_eq!(pose.position[1], expected);
    }
}
#[test]
fn work_cancellation_and_callback_errors_cannot_return_a_partial_pose() {
    let program = Program::new(&attributes()).unwrap();
    assert!(matches!(
        program
            .apply(
                [0.; 3],
                -500.,
                program.required_work() - 1,
                || false,
                |_, _| panic!(),
                |_| panic!()
            )
            .unwrap(),
        Err(Verdict::Budget)
    ));
    assert!(matches!(
        program
            .apply([0.; 3], -500., 100, || true, |_, _| panic!(), |_| panic!())
            .unwrap(),
        Err(Verdict::Cancelled)
    ));
    let stop = Cell::new(false);
    assert!(matches!(
        program
            .apply(
                [0.; 3],
                -500.,
                100,
                || stop.get(),
                |_, _| {
                    stop.set(true);
                    Ok(0.5)
                },
                |_| panic!()
            )
            .unwrap(),
        Err(Verdict::Cancelled)
    ));
    assert_eq!(
        program
            .apply(
                [0.; 3],
                -500.,
                100,
                || false,
                |_, _| Ok(0.5),
                |_| Err("missing water source".into())
            )
            .unwrap_err(),
        "missing water source"
    );
    for value in [f32::NAN, -0.01, 1.01] {
        assert!(
            program
                .apply(
                    [0.; 3],
                    -500.,
                    100,
                    || false,
                    |_, _| Ok(value),
                    |_| panic!()
                )
                .is_err()
        );
    }
    assert!(
        program
            .apply(
                [0.; 3],
                -500.,
                100,
                || false,
                |_, _| Ok(0.5),
                |_| Ok(f32::INFINITY)
            )
            .is_err()
    );
}
#[test]
fn unsupported_programs_and_invalid_context_are_errors_instead_of_guesses() {
    let mut data = attributes();
    data.root_decor[0].values[0] = 10.;
    assert!(Program::new(&data).is_err());
    data = attributes();
    data.root_decor[0].kind = "DecorScale".into();
    data.root_decor[0].values = vec![1.; 6];
    assert!(Program::new(&data).is_err());
    data = attributes();
    data.environment = None;
    assert!(Program::new(&data).is_err());
    let program = Program::new(&attributes()).unwrap();
    assert!(
        program
            .apply(
                [0.; 3],
                f32::NAN,
                100,
                || false,
                |_, _| panic!(),
                |_| panic!()
            )
            .is_err()
    );
    assert!(
        program
            .apply(
                [f32::NAN; 3],
                0.,
                100,
                || false,
                |_, _| panic!(),
                |_| panic!()
            )
            .is_err()
    );
}
