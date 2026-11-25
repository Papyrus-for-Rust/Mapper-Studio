use super::*;
use serde_json::{Value, json};

const PREFAB: &str = "assets/bundled/prefabs/autospawn/decor/test.prefab";
const EMPTY: &str = "assets/bundled/prefabs/autospawn/decor/empty.prefab";
const WIRE: &str = "assets/bundled/prefabs/wires/test.prefab";

fn fixture() -> Value {
    let mut node = json!({"game_object":1,"object_id":2,"name":"wire node","layer":0,
        "hierarchy_active_without_root":true,"component":["Assembly-CSharp","","PowerlineNode"],
        "local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
        "fields":{"m_GameObject":{"m_FileID":0,"m_PathID":1},
            "m_Script":{"m_FileID":1,"m_PathID":42},"m_Name":"","m_Enabled":1}});
    let mut helper = node.clone();
    node["fields"]["WirePrefab"] = json!({"guid":"a".repeat(32)});
    node["fields"]["MaxDistance"] = 80.into();
    helper["object_id"] = 3.into();
    helper["component"] = json!(["Assembly-CSharp", "", "PowerLineWireConnectionHelper"]);
    helper["fields"]["showgizmo"] = false.into();
    helper["fields"]["connections"] = json!([{"inOffset":{"x":-1.,"y":2.,"z":0.},
        "outOffset":{"x":1.,"y":2.,"z":0.},"radius":0.03,"hidden":0}]);
    let receipts: Vec<_> = [
        "RustClient_Data/Plugins/x86_64/RustNative.dll",
        MANIFEST,
        ASSET_SCENES,
    ]
    .into_iter()
    .map(|relative| json!({"relative_path":relative,"sha256":"b".repeat(64),"bytes":1}))
    .collect();
    json!({"format":"mapper-scene-wire-source-reference","schema":1,
        "inspection_only":true,"game_code_executed":false,"physics_executed":false,
        "sources":receipts,"extra_sources":[{"relative_path":CONTENT,"sha256":"c".repeat(64),"bytes":1}],
        "extractor_sha256":metadata_runtime::scene_wire_extractor_sha256(),
        "prefabs":{PREFAB:[node,helper],EMPTY:[]},"wire_resources":{"a".repeat(32):WIRE},"wire_prefabs":{WIRE:[]}})
}

fn read(value: &Value) -> Result<Source, String> {
    Source::read(
        &serde_json::to_vec(value).unwrap(),
        &BTreeSet::from([PREFAB.into(), EMPTY.into()]),
        &|| false,
    )
}

#[test]
fn scene_capture_decodes_nodes_keeps_empty_coverage_and_places_full_frames() {
    let value = fixture();
    let source = read(&value).unwrap();
    assert_eq!(source.templates().len(), 2);
    assert!(source.templates()[EMPTY].is_empty());
    assert_eq!(source.templates()[PREFAB].len(), 1);
    assert_eq!(source.wire_resources().len(), 1);
    let placed = source.templates()[PREFAB][0]
        .place(
            12,
            PREFAB,
            &papyrus_terrain::environment::Transform {
                position: [5., 10., 15.],
                rotation: [0., 0., 0., 1.],
                scale: [2., 3., 4.],
            },
        )
        .unwrap();
    assert_eq!(placed.input().position, [5., 10., 15.]);
    assert_eq!(
        placed.input().connections.unwrap()[0].incoming,
        [3., 16., 15.]
    );
    assert_eq!(
        source.sha256(),
        format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
    );
}

#[test]
fn changed_headers_source_rosters_and_unreviewed_components_are_rejected() {
    let original = fixture();
    for (pointer, value) in [
        ("/schema", json!(2)),
        ("/inspection_only", json!(false)),
        ("/game_code_executed", json!(true)),
        ("/physics_executed", json!(true)),
        ("/extractor_sha256", json!("c".repeat(64))),
        ("/sources", json!([])),
        ("/sources/0/relative_path", json!("../invalid")),
        (
            "/extra_sources/0/relative_path",
            json!("Bundles/maps/maps.bundle"),
        ),
        ("/prefabs", json!({})),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(read(&changed).is_err(), "{pointer}");
    }
    let mut changed = original.clone();
    changed["prefabs"][PREFAB][0]["component"] = json!(["OtherAssembly", "", "PowerlineNode"]);
    assert!(read(&changed).is_err());
    changed = original;
    changed["prefabs"][PREFAB][0]["local_to_root"][3][0] = 1.into();
    assert!(read(&changed).err().unwrap().contains("affine"));
}

#[test]
fn resource_coverage_unknown_fields_and_unresolved_guids_are_rejected() {
    for change in 0..5 {
        let mut changed = fixture();
        match change {
            0 => changed["wire_prefabs"] = json!({}),
            1 => changed["wire_resources"] = json!({}),
            2 => {
                changed["prefabs"][PREFAB][0]["fields"]["NewPolicy"] = 1.into();
            }
            3 => {
                changed["prefabs"][PREFAB][0]["fields"]["WirePrefab"]["guid"] =
                    "d".repeat(32).into();
            }
            _ => {
                changed["wire_resources"]["d".repeat(32)] = WIRE.into();
            }
        }
        assert!(read(&changed).is_err(), "mutation {change}");
    }
}

#[test]
fn inactive_nodes_are_retained_in_capture_and_excluded_from_registration_inputs() {
    let mut value = fixture();
    for row in value["prefabs"][PREFAB].as_array_mut().unwrap() {
        row["hierarchy_active_without_root"] = false.into();
    }
    value["wire_resources"] = json!({});
    value["wire_prefabs"] = json!({});
    value["extra_sources"] = json!([]);
    let source = read(&value).unwrap();
    assert!(source.templates()[PREFAB].is_empty());
    assert_eq!(source.document.prefabs[PREFAB].len(), 2);
}

#[test]
fn cancellation_and_empty_or_incomplete_requests_fail_before_publication() {
    let bytes = serde_json::to_vec(&fixture()).unwrap();
    assert!(Source::read(&bytes, &BTreeSet::new(), &|| false).is_err());
    assert!(Source::read(&bytes, &BTreeSet::from([PREFAB.into()]), &|| true).is_err());
    assert!(
        Source::read(
            &bytes,
            &BTreeSet::from(["assets/missing.prefab".into()]),
            &|| false
        )
        .is_err()
    );
}

fn scene_settings() -> papyrus_terrain::powerline_wires::Settings {
    papyrus_terrain::powerline_wires::Settings {
        minimum_chain_nodes: 3,
        maximum_nodes: 200_000,
        maximum_groups: 4096,
        maximum_chains: 200_000,
        maximum_spans: 1_000_000,
        maximum_work: 128_000_000,
    }
}
fn identities() -> crate::prefab_identities::Source {
    crate::prefab_identities::Source {
        sha256: "d".repeat(64),
        source: SourceFile {
            relative_path: CONTENT.into(),
            sha256: "e".repeat(64),
            bytes: 1,
        },
        prefabs: BTreeMap::from([(PREFAB.into(), 1), (EMPTY.into(), 2)]),
    }
}
fn records() -> Vec<papyrus_terrain::prefab_records::Record> {
    let mut rows = vec![papyrus_terrain::prefab_records::Record {
        category: "Decor".into(),
        path: EMPTY.into(),
        id: 2,
        position: [0.; 3],
        rotation: [0.; 3],
        scale: [1.; 3],
    }];
    rows.extend((0..3).map(|i| papyrus_terrain::prefab_records::Record {
        category: format!("Road {i}"),
        path: PREFAB.into(),
        id: 1,
        position: [i as f32 * 10., 0., 0.],
        rotation: [0., 90., 0.],
        scale: [-2., 3., 4.],
    }));
    rows
}
#[test]
fn record_collection_preserves_prefab_order_and_full_mirrored_map_poses() {
    let source = read(&fixture()).unwrap();
    let records = records();
    let scene = crate::scene_wires::from_records(
        &records,
        &source,
        &identities(),
        scene_settings(),
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(scene.prefab_roots, 4);
    assert_eq!(scene.nodes.len(), 3);
    assert_eq!(scene.nodes[0].input().instance, 1);
    assert_eq!(scene.nodes[2].input().instance, 3);
    assert_eq!(scene.report.groups.len(), 1);
    assert_eq!(scene.report.spans.len(), 2);
    for (actual, expected) in [
        (scene.report.spans[0].start, [0., 6., 2.]),
        (scene.report.spans[0].end, [10., 6., -2.]),
    ] {
        for axis in 0..3 {
            assert!((actual[axis] - expected[axis]).abs() < 0.0001);
        }
    }
    let replay = crate::scene_wires::from_records(
        &records,
        &source,
        &identities(),
        scene_settings(),
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::to_value(&scene).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    let mut reordered = records;
    reordered.swap(1, 2);
    let reordered = crate::scene_wires::from_records(
        &reordered,
        &source,
        &identities(),
        scene_settings(),
        || false,
    )
    .unwrap()
    .unwrap();
    assert_ne!(scene.roots_sha256, reordered.roots_sha256);
}
#[test]
fn record_collection_hashes_signed_zero_as_the_map_codec_serializes_it() {
    let source = read(&fixture()).unwrap();
    let mut roots = records();
    let positive = crate::scene_wires::from_records(
        &roots, &source, &identities(), scene_settings(), || false,
    ).unwrap().unwrap();
    for root in &mut roots {
        for value in root.position.iter_mut().chain(&mut root.rotation) {
            if *value == 0. { *value = -0.; }
        }
    }
    let negative = crate::scene_wires::from_records(
        &roots, &source, &identities(), scene_settings(), || false,
    ).unwrap().unwrap();
    assert_eq!(positive.roots_sha256, negative.roots_sha256);
    assert_eq!(positive.nodes.len(), negative.nodes.len());
    assert_eq!(positive.report.spans.len(), negative.report.spans.len());
}
#[test]
fn record_collection_requires_complete_metadata_matching_ids_and_valid_poses() {
    let source = read(&fixture()).unwrap();
    for change in 0..6 {
        let mut records = records();
        match change {
            0 => records[1].id = 2,
            1 => records[1].position[0] = f32::NAN,
            2 => records[1].scale[0] = 0.,
            3 => records[1].rotation[0] = f32::INFINITY,
            4 => records[1].scale[0] = 100.01,
            _ => records[1].position[0] = 10000.1,
        }
        assert!(
            crate::scene_wires::from_records(
                &records,
                &source,
                &identities(),
                scene_settings(),
                || false
            )
            .is_err()
        );
    }
    let mut records = records();
    records[1].path = "assets/missing.prefab".into();
    let mut identities = identities();
    identities.prefabs.insert(records[1].path.clone(), 1);
    assert!(
        crate::scene_wires::from_records(&records, &source, &identities, scene_settings(), || {
            false
        })
        .err()
        .unwrap()
        .contains("metadata")
    );
}
#[test]
fn record_collection_preserves_supported_tiny_nonzero_scales() {
    let source = read(&fixture()).unwrap();
    let mut records = records();
    records[0].scale = [1e-8; 3];
    records[1].scale[0] = -1e-8;
    let scene = crate::scene_wires::from_records(
        &records,
        &source,
        &identities(),
        scene_settings(),
        || false,
    )
    .unwrap()
    .unwrap();
    assert!((scene.nodes[0].input().connections.unwrap()[0].outgoing[2] - 1e-8).abs() < 1e-12);
}
#[test]
fn record_collection_cancels_and_honors_node_and_assembly_bounds() {
    let source = read(&fixture()).unwrap();
    let records = records();
    let mut settings = scene_settings();
    settings.maximum_nodes = 2;
    assert!(
        crate::scene_wires::from_records(&records, &source, &identities(), settings, || false)
            .is_err()
    );
    settings = scene_settings();
    settings.maximum_work = 1;
    assert!(
        crate::scene_wires::from_records(&records, &source, &identities(), settings, || false)
            .is_err()
    );
    let calls = std::cell::Cell::new(0);
    assert!(
        crate::scene_wires::from_records(
            &records,
            &source,
            &identities(),
            scene_settings(),
            || {
                calls.set(calls.get() + 1);
                calls.get() > 4
            }
        )
        .unwrap()
        .is_none()
    );
}

#[test]
#[ignore = "Requires owned exported map decodes and prepared client metadata"]
fn owned_exported_roots_assemble_complete_scene_cables() {
    let cases: Vec<Value> = serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("MAPPER_SCENE_WIRE_TEST_CASES").expect("MAPPER_SCENE_WIRE_TEST_CASES"),
        )
        .unwrap(),
    )
    .unwrap();
    for case in cases {
        let path = |key: &str| Path::new(case[key].as_str().unwrap());
        let root = path("root");
        let installation = installation::inspect(root, || false).unwrap();
        let catalog =
            crate::prefab_assets::load(root, path("catalog"), &installation, || false).unwrap();
        let source = Source::load(
            root,
            path("capture"),
            &catalog,
            &roster(root, &catalog, &|| false).unwrap(),
            || false,
        )
        .unwrap();
        let identities =
            crate::prefab_identities::Source::load(root, path("identities"), &catalog, || false)
                .unwrap();
        let decoded: Value =
            serde_json::from_slice(&std::fs::read(path("decoded")).unwrap()).unwrap();
        let by_id: BTreeMap<_, _> = identities
            .prefabs
            .iter()
            .map(|(path, id)| (*id, path))
            .collect();
        let records: Vec<_> = decoded["prefabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let id = u32::try_from(row["Id"].as_u64().unwrap()).unwrap();
                papyrus_terrain::prefab_records::Record {
                    category: row["Category"].as_str().unwrap().into(),
                    path: by_id[&id].clone(),
                    id,
                    position: serde_json::from_value(row["position"].clone()).unwrap(),
                    rotation: serde_json::from_value(row["rotation"].clone()).unwrap(),
                    scale: serde_json::from_value(row["scale"].clone()).unwrap(),
                }
            })
            .collect();
        let scene = crate::scene_wires::from_records(
            &records,
            &source,
            &identities,
            scene_settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(scene.nodes.len() > 21);
        assert!(!scene.report.spans.is_empty());
        println!(
            "{} roots, {} nodes, {} spans, root digest {} for {}",
            scene.prefab_roots,
            scene.nodes.len(),
            scene.report.spans.len(),
            scene.roots_sha256,
            root.display()
        );
        let output = path("wire_output");
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(output)
            .and_then(|file| serde_json::to_writer(file, &scene).map_err(std::io::Error::other))
            .unwrap();
    }
}

#[test]
#[ignore = "Requires owned client installations and scene-wire captures"]
fn owned_captures_reuse_persistent_cache_with_extraction_unavailable() {
    let cases: Vec<Value> = serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("MAPPER_SCENE_WIRE_TEST_CASES").expect("MAPPER_SCENE_WIRE_TEST_CASES"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let path = |key: &str| Path::new(case[key].as_str().unwrap());
        let root = path("root");
        let installation = installation::inspect(root, || false).unwrap();
        let catalog =
            crate::prefab_assets::load(root, path("catalog"), &installation, || false).unwrap();
        let required = roster(root, &catalog, &|| false).unwrap();
        let source = Source::load(root, path("capture"), &catalog, &required, || false).unwrap();
        assert_eq!(source.templates().len(), required.len());
        assert!(source.templates().values().any(|nodes| !nodes.is_empty()));
        let local = tempfile::tempdir().unwrap();
        if case["cold"].as_bool() == Some(true) {
            let started = std::time::Instant::now();
            let captured = prepare_in(root, &catalog, local.path(), || false).unwrap();
            assert_eq!(source.sha256(), captured.sha256());
            println!("Cold cable preparation took {:?}", started.elapsed());
        }
        let staging = tempfile::tempdir().unwrap();
        let copy = staging.path().join("source.json");
        std::fs::copy(path("capture"), &copy).unwrap();
        metadata_runtime::publish_unkeyed(&copy, &local.path().join(CACHE), source.sha256(), LIMIT)
            .unwrap();
        let started = std::time::Instant::now();
        let prepared = prepare_in(root, &catalog, local.path(), || false).unwrap();
        println!("Warm cable preparation took {:?}", started.elapsed());
        let reused = find_cached(root, &catalog, &required, local.path(), &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(source.sha256(), prepared.sha256());
        assert_eq!(source.sha256(), reused.sha256());
        assert_eq!(
            source.sha256(),
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(path("capture")).unwrap())
            )
        );
        println!(
            "Verified {} prefab roots and {} active node templates for {}",
            required.len(),
            source.templates().values().map(Vec::len).sum::<usize>(),
            root.display()
        );
    }
}
