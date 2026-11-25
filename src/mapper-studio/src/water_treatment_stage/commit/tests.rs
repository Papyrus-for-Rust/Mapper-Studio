use super::*;
use serde_json::json;

#[test]
fn pipe_reservations_exclude_the_preexisting_protected_terrain_mask() {
    let occupancy = [0, 2, 1, 1, 2, 0, 1, 2, 0];
    let blocking = [0, 0, 0, 0, 0, 1, 0, 0, 0];
    assert_eq!(
        owned_spans(&occupancy, Some(&blocking), || false)
            .unwrap()
            .unwrap(),
        [[2, 4], [5, 7]]
    );
    assert_eq!(
        owned_spans(&[2, 2, 2], None, || false).unwrap().unwrap(),
        Vec::<[usize; 2]>::new()
    );
    assert!(owned_spans(&occupancy, Some(&blocking[..2]), || false).is_err());
    assert!(owned_spans(&[3], None, || false).is_err());
    assert!(owned_spans(&occupancy, None, || true).unwrap().is_none());
}

fn saved() -> (Value, Project) {
    let project = Project {
        objects: Some(crate::object_generation::Recipe::new("a".repeat(64))),
        monument_assets: Some(crate::AssetIdentity {
            catalog_sha256: "b".repeat(64),
            bindings_sha256: "c".repeat(64),
        }),
        ..Default::default()
    };
    let settings = Settings {
        seed: project.seeds[5].value,
        realm_mask: 2,
        maximum_terrain_work: 32_000_000,
        maximum_sampling_attempts: 1000,
        maximum_fitting_samples: 1000,
        maximum_clearance_work: 1000,
    };
    let resolution = papyrus_terrain::native_layers::dimensions(project.size)
        .unwrap()
        .1;
    let height_resolution = papyrus_terrain::native_layers::dimensions(project.size)
        .unwrap()
        .0;
    (
        json!({"engine":VERSION,"complete":true,"tag_source_sha256":"d".repeat(64),
        "identities_sha256":"e".repeat(64),"road_source_sha256":"a".repeat(64),"catalog_sha256":"b".repeat(64),
        "report":{"engine":super::super::VERSION,"source_sha256":"f".repeat(64),"eligible_monuments":1,
            "admitted_instances":1,"settings":settings},
        "prefabs":[{"path":"assets/test/pipe.prefab","identity":7,"position":[1.,2.,3.],
            "rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.]}],"reserved_spans":[[1,4],[6,8]],"surface_resolution":resolution,
            "height_resolution":height_resolution,"height_spans":[[1,3],[6,8]]}),
        project,
    )
}

#[test]
fn committed_records_keep_exact_roots_and_protect_later_object_placements() {
    let (value, project) = saved();
    validate_committed(&value, &project).unwrap();
    let records = receipt_records(&value, |_| Some(7), || false)
        .unwrap()
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].position, [1., 2., 3.]);
    assert_eq!(records[0].id, 7);
    let resolution = value["surface_resolution"].as_u64().unwrap() as usize;
    let mut cells = vec![0; resolution * resolution];
    assert_eq!(
        reserve_committed(&value, &mut cells, || false).unwrap(),
        Some(true)
    );
    assert_eq!(&cells[..10], [0, 1, 1, 1, 0, 0, 1, 1, 0, 0]);
    assert!(cells[10..].iter().all(|cell| *cell == 0));
    assert!(receipt_records(&value, |_| None, || false).is_err());
    assert!(receipt_records(&value, |_| Some(8), || false).is_err());
    assert!(
        receipt_records(&value, |_| Some(7), || true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reserve_committed(&Value::Null, &mut cells, || false).unwrap(),
        Some(false)
    );
}

#[test]
fn edited_sources_seeds_rosters_and_spans_fail_before_publication() {
    let (value, project) = saved();
    for (pointer, replacement) in [
        ("/complete", json!(false)),
        ("/road_source_sha256", json!("f".repeat(64))),
        ("/catalog_sha256", json!("f".repeat(64))),
        (
            "/report/settings/seed",
            json!(project.seeds[5].value.wrapping_add(1)),
        ),
        ("/report/admitted_instances", json!(2)),
        ("/report/eligible_monuments", json!(0)),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(validate_committed(&changed, &project).is_err(), "{pointer}");
    }
    let resolution = value["surface_resolution"].as_u64().unwrap() as usize;
    for spans in [
        json!([[1, 4], [3, 5]]),
        json!([
            [1, 4],
            [resolution * resolution, resolution * resolution + 1]
        ]),
        json!([[4, 4]]),
    ] {
        let mut changed = value.clone();
        changed["reserved_spans"] = spans;
        let mut cells = vec![0; resolution * resolution];
        assert!(reserve_committed(&changed, &mut cells, || false).is_err());
        assert!(cells.iter().all(|cell| *cell == 0));
    }
    validate_committed(&Value::Null, &project).unwrap();
}

#[test]
fn pipe_height_guards_extend_road_guards_and_preflight_their_work_allowance() {
    let (value, project) = saved();
    let resolution = value["height_resolution"].as_u64().unwrap() as usize;
    let height = vec![17038; resolution * resolution];
    let grid = papyrus_terrain::path_terrain::Grid {
        size: project.size as f32,
        resolution,
        origin_y: -500.,
        vertical_span: 1000.,
        height: &height,
    };
    let mut guard = papyrus_terrain::height_reservations::HeightReservations::new(grid, || false)
        .unwrap()
        .unwrap();
    guard.reserve(0).unwrap();
    assert!(reserve_heights(&value, &mut guard, grid, 3, || false).is_err());
    assert_eq!(guard.samples(), 1);
    assert_eq!(
        reserve_heights(&value, &mut guard, grid, 4, || false).unwrap(),
        Some(4)
    );
    assert_eq!(guard.samples(), 5);
    assert!(
        [0, 1, 2, 6, 7]
            .into_iter()
            .all(|index| guard.contains(index))
    );
    assert!(!guard.contains(3));
    assert!(
        reserve_heights(&value, &mut guard, grid, 4, || true)
            .unwrap()
            .is_none()
    );
}
