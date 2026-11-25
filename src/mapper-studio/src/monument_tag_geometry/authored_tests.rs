use super::tests::{fixture, pose, read};
use super::*;
use papyrus_terrain::{
    decor_stream::Budget,
    environment::{Coverage, Verdict},
};
use serde_json::{Value, json};

fn monument(phrase: Value) -> Value {
    json!({"object_id":20,"root":true,"hierarchy_active":true,"component_enabled":true,
        "local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
        "rotation_chain":[],"display_phrase":phrase})
}
fn with_monument() -> Value {
    let mut value = fixture();
    value["monuments"] = json!([monument(
        json!({"token":"WATER_TREATMENT_PLANT_display_name","legacyEnglish":"Water Treatment Plant"})
    )]);
    value
}

#[test]
fn phrase_layouts_use_active_tokens_and_awake_registration_independently_of_enabled() {
    for english_key in ["english", "legacyEnglish"] {
        let mut value = with_monument();
        let phrase = &mut value["monuments"][0]["display_phrase"];
        *phrase = json!({"token":"prefix_WATER_TREATMENT_PLANT_suffix",english_key:"Other label"});
        assert_eq!(
            read(&value)
                .matching_monuments("water_treatment_plant")
                .unwrap(),
            [20]
        );
        value["monuments"][0]["component_enabled"] = false.into();
        assert_eq!(
            read(&value)
                .matching_monuments("water_treatment_plant")
                .unwrap(),
            [20]
        );
        value["monuments"][0]["hierarchy_active"] = false.into();
        assert!(
            read(&value)
                .matching_monuments("water_treatment_plant")
                .unwrap()
                .is_empty()
        );
    }
    for (token, english, expected) in [
        (json!(""), json!("Water Treatment Plant"), Some(false)),
        (Value::Null, Value::Null, Some(false)),
        (Value::Null, json!("Water Treatment Plant"), None),
        (
            json!("different_token"),
            json!("water_treatment_plant"),
            Some(false),
        ),
    ] {
        let mut value = with_monument();
        value["monuments"][0]["display_phrase"] = json!({"token":token,"english":english});
        let result = read(&value).matching_monuments("water_treatment_plant");
        if let Some(expected) = expected {
            assert_eq!(!result.unwrap().is_empty(), expected);
        } else {
            assert!(result.is_err());
        }
    }
    for needle in ["", "Water_Treatment_Plant", "bad\0query"] {
        assert!(read(&with_monument()).matching_monuments(needle).is_err());
    }
}

#[test]
fn malformed_monument_phrases_and_transforms_fail_preparation() {
    for case in 0..7 {
        let mut value = with_monument();
        match case {
            0 => value["monuments"][0]["display_phrase"]["extra"] = true.into(),
            1 => {
                value["monuments"][0]["display_phrase"]
                    .as_object_mut()
                    .unwrap()
                    .remove("token");
            }
            2 => value["monuments"][0]["display_phrase"]["token"] = "x".repeat(4097).into(),
            3 => value["monuments"][0]["local_to_root"][3][0] = 1.into(),
            4 => value["monuments"][0]["rotation_chain"] = json!([[0., 0., 0., 0.]]),
            5 => value["monuments"][0]["object_id"] = 0.into(),
            _ => {
                let duplicate = value["monuments"][0].clone();
                value["monuments"].as_array_mut().unwrap().push(duplicate);
            }
        }
        assert!(
            Prepared::read(&serde_json::to_vec(&value).unwrap()).is_err(),
            "case {case}"
        );
    }
}

fn mesh() -> Value {
    json!({"object_id":11,"class_id":64,"enabled":true,"trigger":true,
        "mesh_bounds":{"kind":"serialized_mesh_local_aabb","center":[1.,2.,3.],"extent":[2.,3.,4.],
            "asset_file":"CAB-fixture","object_id":100}})
}

#[test]
fn authored_mesh_bounds_enclose_transformed_corners_with_shear_and_reflection() {
    let mut value = fixture();
    value["tags"][0]["colliders"] = json!([mesh()]);
    let local: [[f32; 4]; 4] = [
        [1., 0.2, 0., -5.],
        [0., 1., 0.4, 2.],
        [0., 0., -1., 7.],
        [0., 0., 0., 1.],
    ];
    value["tags"][0]["local_to_root"] = json!(local);
    let root = Transform {
        scale: [2., 3., 4.],
        ..pose()
    };
    let prepared = read(&value);
    assert_eq!(
        prepared
            .assemble(0, root, &BTreeMap::new(), || false)
            .unwrap()
            .unwrap()
            .pending
            .len(),
        1
    );
    let assembled = prepared
        .assemble_authored(0, root, || false)
        .unwrap()
        .unwrap();
    assert_eq!(assembled.engine, Some(AUTHORED_VERSION));
    assert!(assembled.pending.is_empty());
    let bounds = assembled.boxes[0].1;
    assert_eq!(bounds.rotation, [0., 0., 0., 1.]);
    let [x, y, z, w] = root.rotation.map(f64::from);
    let rotation = [
        [
            1. - 2. * (y * y + z * z),
            2. * (x * y - z * w),
            2. * (x * z + y * w),
        ],
        [
            2. * (x * y + z * w),
            1. - 2. * (x * x + z * z),
            2. * (y * z - x * w),
        ],
        [
            2. * (x * z - y * w),
            2. * (y * z + x * w),
            1. - 2. * (x * x + y * y),
        ],
    ];
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for corner in 0..8 {
        let point: [f64; 3] = std::array::from_fn(|axis| {
            [1., 2., 3.][axis]
                + [2., 3., 4.][axis] * if corner & (1 << axis) == 0 { -1. } else { 1. }
        });
        let local_point: [f64; 3] = std::array::from_fn(|row| {
            (0..3)
                .map(|column| f64::from(local[row][column]) * point[column])
                .sum::<f64>()
                + f64::from(local[row][3])
        });
        for axis in 0..3 {
            let world = (0..3)
                .map(|column| {
                    rotation[axis][column] * local_point[column] * f64::from(root.scale[column])
                })
                .sum::<f64>()
                + f64::from(root.position[axis]);
            minimum[axis] = minimum[axis].min(world);
            maximum[axis] = maximum[axis].max(world);
        }
    }
    for axis in 0..3 {
        assert!(
            (f64::from(bounds.center[axis]) - (minimum[axis] + maximum[axis]) * 0.5).abs() < 1e-4
        );
        assert!(
            (f64::from(bounds.extents[axis]) - (maximum[axis] - minimum[axis]) * 0.5).abs() < 1e-4
        );
    }
}

#[test]
fn authored_selection_is_serialized_and_missing_mesh_bounds_stay_incomplete() {
    let mut value = fixture();
    let first = value["tags"][0]["colliders"][0].clone();
    let mut second = first.clone();
    second["object_id"] = 12.into();
    second["center"] = json!([100., 100., 100.]);
    value["tags"][0]["colliders"] = json!([first, second]);
    let prepared = read(&value);
    let explicit = prepared
        .assemble(1, pose(), &BTreeMap::from([(1, 11)]), || false)
        .unwrap()
        .unwrap();
    let authored = prepared
        .assemble_authored(1, pose(), || false)
        .unwrap()
        .unwrap();
    assert_eq!(authored.boxes, explicit.boxes);
    assert!(authored.pending.is_empty());
    let mut value = fixture();
    value["tags"][0]["colliders"] = json!([{"object_id":11,"class_id":64}]);
    let prefabs = BTreeMap::from([("a".into(), read(&value))]);
    let registry = Registry::assemble(
        &prefabs,
        &[Placement {
            instance: 0,
            prefab: "a",
            pose: pose(),
        }],
        "water_treatment_plant",
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(registry.coverage(), Coverage::Incomplete);
    assert_eq!(registry.pending().len(), 1);
    let assessment = registry
        .clearance(7.)
        .unwrap()
        .check([0.; 3], &mut Budget::new(100).unwrap(), || false)
        .unwrap();
    assert_eq!(assessment.verdict, Verdict::Incomplete);
    for field in ["kind", "center", "extent", "asset_file", "object_id"] {
        let mut value = fixture();
        let mut collider = mesh();
        collider["mesh_bounds"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        value["tags"][0]["colliders"] = json!([collider]);
        assert!(Prepared::read(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn world_registry_keys_instances_matches_phrases_and_discards_cancelled_work() {
    let prefabs = BTreeMap::from([("a".into(), read(&with_monument()))]);
    let placements = [
        Placement {
            instance: 0,
            prefab: "a",
            pose: pose(),
        },
        Placement {
            instance: 1,
            prefab: "a",
            pose: Transform {
                position: [200., 10., 200.],
                ..pose()
            },
        },
    ];
    let registry = Registry::assemble(&prefabs, &placements, "water_treatment_plant", || false)
        .unwrap()
        .unwrap();
    assert_eq!(
        registry.matched_monuments(),
        [
            Key {
                instance: 0,
                component: 20
            },
            Key {
                instance: 1,
                component: 20
            }
        ]
    );
    assert_eq!(registry.boxes().len(), 2);
    assert_eq!(registry.coverage(), Coverage::Complete);
    let clearance = registry.clearance(7.).unwrap();
    assert_eq!(
        clearance
            .check(
                registry.boxes()[0].1.center,
                &mut Budget::new(100).unwrap(),
                || false
            )
            .unwrap()
            .verdict,
        Verdict::Blocked(registry.boxes()[0].0)
    );
    let calls = std::cell::Cell::new(0);
    assert!(
        Registry::assemble(&prefabs, &placements, "water_treatment_plant", || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        })
        .unwrap()
        .is_none()
    );
    assert!(
        Registry::assemble(
            &prefabs,
            &[Placement {
                instance: 0,
                prefab: "missing",
                pose: pose()
            }],
            "water_treatment_plant",
            || false
        )
        .is_err()
    );
    assert!(
        Registry::assemble(
            &prefabs,
            &[
                Placement {
                    instance: 0,
                    prefab: "a",
                    pose: pose()
                },
                Placement {
                    instance: 0,
                    prefab: "a",
                    pose: pose()
                }
            ],
            "water_treatment_plant",
            || false
        )
        .is_err()
    );
    assert!(Registry::assemble(&prefabs, &[], "", || false).is_err());
}

#[test]
fn registry_limits_reject_excessive_rosters_and_geometry_without_publication() {
    let prefabs = BTreeMap::from([("a".into(), read(&fixture()))]);
    let placements: Vec<_> = (0..4097)
        .map(|instance| Placement {
            instance,
            prefab: "a",
            pose: pose(),
        })
        .collect();
    assert!(Registry::assemble(&prefabs, &placements, "water_treatment_plant", || false).is_err());
    let mut value = fixture();
    let template = value["tags"][0].clone();
    value["tags"] = Value::Array(
        (1..=4096)
            .map(|id| {
                let mut tag = template.clone();
                tag["object_id"] = id.into();
                tag
            })
            .collect(),
    );
    let prefabs = BTreeMap::from([("a".into(), read(&value))]);
    assert!(
        Registry::assemble(&prefabs, &placements[..33], "water_treatment_plant", || {
            false
        })
        .is_err()
    );
    assert_eq!(
        Registry::assemble(&prefabs, &placements[..1], "water_treatment_plant", || {
            false
        })
        .unwrap()
        .unwrap()
        .boxes()
        .len(),
        4096
    );
}
