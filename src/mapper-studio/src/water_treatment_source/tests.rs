use super::*;
use serde_json::{Value, json};

const PREFAB: &str = "assets/bundled/prefabs/autospawn/monument/maintainables/pipe.prefab";

#[path = "stage_tests.rs"]
mod stage;

fn receipt(path: &str) -> Value {
    json!({"relative_path":path,"sha256":"a".repeat(64),"bytes":10})
}

fn fixture(current: bool) -> Value {
    let disabled = if current { Value::Null } else { json!(false) };
    let stage = json!({
        "object_id":2,"order":1,"name":"Pipes","hierarchy_active":true,
        "mode":-1,"disabled":disabled,"component_enabled":true,
        "folder":"monument/maintainables",
        "filter":{"SplatType":-1,"BiomeType":31,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},
        "anchor_mode":0,"road_type":5,"placement_attempts":8,"prefabs":[PREFAB],
        "distance":800.,"side_offset":7.,"maximum_slope":20.,"dithering":10.,
        "prevent_building_padding":7.
    });
    json!({
        "format":"mapper-water-treatment-reference","schema":2,"inspection_only":true,
        "game_code_executed":false,"scene":"BuildPlayer-Procedural Map",
        "stage_layout":if current {"mode-description"} else {"legacy"},
        "order_basis":"relative hierarchy procedural component order","collider_backfaces":false,
        "sources":[receipt("Bundles/maps/maps.bundle")],"extra_sources":[],
        "extractor_sha256":metadata_runtime::water_treatment_extractor_sha256(),
        "stages":[stage],
        "procedural_order":{"format":"papyrus-procedural-order","schema":if current {2} else {1},
            "stages":[
                {"object_id":1,"order":0,"kind":"PlaceRoadObjects","description":"Objects",
                    "mode":-1,"disabled":disabled,"component_enabled":true,"hierarchy_active":true},
                {"object_id":2,"order":1,"kind":"PlaceWaterTreatmentPipes","description":"Pipes",
                    "mode":-1,"disabled":disabled,"component_enabled":true,"hierarchy_active":true}
            ]},
        "prefabs":{PREFAB:{"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],
            "attributes":[],"components":{},"issues":[]}}
    })
}

fn read(value: &Value) -> Result<Source, String> {
    Source::read(
        &serde_json::to_vec(value).unwrap(),
        &BTreeMap::from([("Field".into(), 1)]),
        0,
    )
}

fn road(value: &Value) -> road_object_source::Source {
    let mut doc = crate::road_object_placement::tests::source_value();
    doc["schema"] = if value["stage_layout"] == "legacy" {
        2
    } else {
        3
    }
    .into();
    if value["stage_layout"] != "legacy" {
        doc["stage_layout"] = value["stage_layout"].clone();
        doc["adapter"] = road_object_source::HIERARCHY_VERSION.into();
    }
    doc["stages"][0]["scene_order"] = 0.into();
    doc["stages"][0]["disabled"] = value["stages"][0]["disabled"].clone();
    doc["workflow"] = value["procedural_order"].clone();
    doc["sources"] = value["sources"].clone();
    road_object_source::Source::parse(
        &serde_json::to_vec(&doc).unwrap(),
        &"a".repeat(64),
        &serde_json::from_value::<Vec<SourceFile>>(value["sources"].clone()).unwrap(),
        || false,
    )
    .unwrap()
}

#[test]
fn both_activation_layouts_bind_to_prepared_road_workflows() {
    for current in [false, true] {
        let value = fixture(current);
        let source = read(&value).unwrap();
        source.verify_road(&road(&value)).unwrap();
        assert_eq!(
            source.sha256(),
            digest(&serde_json::to_vec(&value).unwrap())
        );
        assert_eq!(source.prefabs().len(), 1);
        assert!(!source.collider_backfaces());
        let program = &source.programs()[0];
        assert!(program.descriptor(0, 2).unwrap().enabled);
        assert_eq!(program.sampling(0.25).unwrap().spacing_fraction, 0.25);
        assert!(program.sampling(f32::NAN).is_err());
    }
}

#[test]
fn source_headers_receipts_and_prefab_coverage_are_required() {
    for (pointer, bad) in [
        ("/schema", json!(1)),
        ("/game_code_executed", json!(true)),
        ("/inspection_only", json!(false)),
        ("/stage_layout", json!("unknown")),
        ("/extractor_sha256", json!("0".repeat(64))),
        ("/sources", json!([])),
        ("/sources/0/bytes", json!(0)),
        ("/sources/0/relative_path", json!("../maps.bundle")),
        ("/sources/0/sha256", json!("A".repeat(64))),
        ("/prefabs", json!({})),
        ("/stages/0/maximum_slope", json!(91)),
        ("/stages/0/prevent_building_padding", json!(-1)),
        ("/stages/0/road_type", json!(6)),
        ("/stages/0/anchor_mode", json!(3)),
        ("/stages/0/placement_attempts", json!(1025)),
        ("/stages/0/distance", json!(0)),
        ("/stages/0/prefabs", json!([PREFAB, PREFAB])),
    ] {
        let mut value = fixture(true);
        *value.pointer_mut(pointer).unwrap() = bad;
        assert!(read(&value).is_err(), "{pointer}");
    }
    let mut value = fixture(true);
    value["unknown"] = true.into();
    assert!(read(&value).is_err());
    let mut value = fixture(true);
    value["stages"][0]
        .as_object_mut()
        .unwrap()
        .remove("disabled");
    assert!(read(&value).is_err());
    assert!(Source::read(&vec![b' '; LIMIT as usize + 1], &BTreeMap::new(), 0).is_err());
}

#[test]
fn conflicting_receipts_and_duplicate_programs_fail() {
    let mut value = fixture(true);
    let mut conflicting = value["sources"][0].clone();
    conflicting["sha256"] = "b".repeat(64).into();
    value["extra_sources"] = json!([conflicting]);
    assert!(read(&value).is_err());
    let mut value = fixture(true);
    let stage = value["stages"][0].clone();
    value["stages"].as_array_mut().unwrap().push(stage);
    assert!(read(&value).is_err());
}

#[test]
fn workflow_and_program_identity_cannot_drift() {
    let original = fixture(true);
    let prepared_road = road(&original);
    for (pointer, bad) in [
        ("/stages/0/name", json!("Different")),
        ("/stages/0/order", json!(2)),
        ("/stages/0/object_id", json!(3)),
        ("/stages/0/mode", json!(1)),
        ("/stages/0/disabled", json!(true)),
        ("/stages/0/component_enabled", json!(false)),
        ("/stages/0/hierarchy_active", json!(false)),
        ("/procedural_order/stages/1/description", json!("Different")),
        ("/stage_layout", json!("legacy")),
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = bad;
        assert!(
            read(&value).unwrap().verify_road(&prepared_road).is_err(),
            "{pointer}"
        );
    }
    let mut mutated_road = road(&original);
    mutated_road.workflow.as_mut().unwrap().stages[1].mode = 1;
    assert!(read(&original).unwrap().verify_road(&mutated_road).is_err());
}

#[test]
fn installation_gameplay_roster_detects_added_and_removed_files() {
    let root = tempfile::tempdir().unwrap();
    let value = fixture(true);
    let source = read(&value).unwrap();
    source.verify_gameplay_roster(root.path()).unwrap();
    for name in GAMEPLAY_FILES {
        let path = root.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fixture").unwrap();
        assert!(source.verify_gameplay_roster(root.path()).is_err());
        let mut captured = value.clone();
        captured["sources"]
            .as_array_mut()
            .unwrap()
            .push(receipt(name));
        let captured = read(&captured).unwrap();
        captured.verify_gameplay_roster(root.path()).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(captured.verify_gameplay_roster(root.path()).is_err());
    }
}

#[test]
fn slope_uses_live_diagonal_corners_and_inclusive_threshold() {
    let source = read(&fixture(true)).unwrap();
    let mut program: Program = serde_json::from_value(fixture(true)["stages"][0].clone()).unwrap();
    let mut height = [16000; 25];
    fn view(height: &[i16]) -> View<'_> {
        View {
            size: 1000,
            height_resolution: 5,
            surface_resolution: 0,
            height,
            biome: &[],
            splat: &[],
            topology: &[],
            placement_blockers: None,
            topology_tiles: None,
        }
    }
    assert!(
        source.programs()[0]
            .accepts_slope(view(&height), [0.; 3])
            .unwrap()
    );
    height[8] = 26000;
    height[16] = 24000;
    let slope = terrain_normal::slope(1000, 5, &height, [0.5; 2]).unwrap();
    assert!(slope > 20.);
    assert!(!program.accepts_slope(view(&height), [0.; 3]).unwrap());
    program.maximum_slope = slope;
    assert!(program.accepts_slope(view(&height), [0.; 3]).unwrap());
    program.maximum_slope = slope - 0.001;
    assert!(!program.accepts_slope(view(&height), [0.; 3]).unwrap());
    assert!(
        program
            .accepts_slope(view(&height), [501., 0., 0.])
            .is_err()
    );
    assert!(
        program
            .accepts_slope(view(&height), [f32::NAN, 0., 0.])
            .is_err()
    );
    height[8] = -1;
    assert!(program.accepts_slope(view(&height), [0.; 3]).is_err());
    assert!(program.accepts_slope(view(&[]), [0.; 3]).is_err());
}

#[test]
fn verified_pipe_captures_remain_readable_and_resettable() {
    assert!(VERIFIED_PRODUCERS.contains(&metadata_runtime::water_treatment_extractor_sha256().as_str()));
    for current_layout in [false, true] {
        let mut value = fixture(current_layout);
        let baseline = read(&value).unwrap();
        for producer in VERIFIED_PRODUCERS {
            value["extractor_sha256"] = json!(producer);
            let source = read(&value).unwrap();
            source.verify_road(&road(&value)).unwrap();
            assert_eq!(source.document.stages.len(), baseline.document.stages.len());
            assert_eq!(source.prefabs().keys().collect::<Vec<_>>(), baseline.prefabs().keys().collect::<Vec<_>>());
            assert_eq!(source.sha256(), digest(&serde_json::to_vec(&value).unwrap()));
        }
    }
    assert!(crate::client_cache::DIRECTORIES.contains(&"papyrus-water-treatment-cache"));
}

#[test]
fn cancelled_load_and_preparation_return_before_accessing_files() {
    let value = fixture(true);
    let road = road(&value);
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let catalog = Catalog {
        sha256: "a".repeat(64),
        installation: installation::Installation {
            kind: installation::InstallationKind::Client,
            native: serde_json::from_value(receipt(
                "RustClient_Data/Plugins/x86_64/RustNative.dll",
            ))
            .unwrap(),
            scene_manifest: None,
        },
        entries: Default::default(),
        sources: vec![],
    };
    let flags = BTreeMap::new();
    for result in [
        Source::load(&missing, &missing, &catalog, &road, &flags, 0, || true),
        prepare(&missing, &catalog, &road, &flags, 0, || true),
    ] {
        assert_eq!(result.err().unwrap(), "Pipe preparation cancelled");
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn unkeyed_capture_publishes_to_reusable_immutable_hash_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("capture");
    let cache = temporary.path().join("cache");
    std::fs::create_dir(&output).unwrap();
    let path = output.join("source.json");
    let value = fixture(true);
    let raw = serde_json::to_vec(&value).unwrap();
    let source = read(&value).unwrap();
    let published = cache.join(source.sha256()).join("source.json");
    for iteration in 0..2 {
        let input = output.join(iteration.to_string());
        std::fs::create_dir(&input).unwrap();
        let path = input.join(path.file_name().unwrap());
        std::fs::write(&path, &raw).unwrap();
        publish_capture(&path, &cache, &source).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&published).unwrap(), raw);
        assert_eq!(
            crate::object_extract::documents(&cache, "source.json")
                .unwrap()
                .as_slice(),
            std::slice::from_ref(&published)
        );
        let loaded =
            read(&serde_json::from_slice(&std::fs::read(&published).unwrap()).unwrap()).unwrap();
        assert_eq!(loaded.sha256(), source.sha256());
    }
}
