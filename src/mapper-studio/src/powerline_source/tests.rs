use super::*;
use serde_json::{Value, json};

const PREFAB: &str = "assets/bundled/prefabs/autospawn/decor/test/a.prefab";

pub(super) fn fixture(current: bool) -> (Value, road_object_source::Source) {
    let disabled = if current { Value::Null } else { json!(false) };
    let mut workflow = vec![json!({"object_id":1,"order":0,"kind":"PlaceRoadObjects",
        "description":"Objects","mode":-1,"disabled":disabled,
        "component_enabled":true,"hierarchy_active":true})];
    let mut stages = Vec::new();
    for (i, kind) in KINDS.iter().enumerate() {
        let entry = json!({"object_id":i+2,"order":i+1,"kind":kind,
            "description":kind,"mode":-1,"disabled":disabled,
            "component_enabled":true,"hierarchy_active":true});
        let mut program = entry.clone();
        if *kind == "PlacePowerlineObjects" {
            program["start"] = json!([]);
            program["end"] = json!([]);
            program["side"] = json!([]);
            program["path"] = json!([{"folder":"decor/test","filter":{
                "SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},
                "prefabs":[PREFAB],"alignment":1,"density":1.,"distance":75.,"dithering":0.}]);
        }
        stages.push(program);
        workflow.push(entry);
    }
    let workflow = json!({"format":"papyrus-procedural-order","schema":if current {2} else {1},"stages":workflow});
    let sources =
        json!([{"relative_path":"Bundles/maps/maps.bundle","sha256":"a".repeat(64),"bytes":10}]);
    let value = json!({"format":"mapper-powerline-source-reference","schema":1,
        "inspection_only":true,"game_code_executed":false,"sources":sources,"extra_sources":[],
        "extractor_sha256":metadata_runtime::powerline_extractor_sha256(),
        "stage_layout":if current {"mode-description"} else {"legacy"},
        "stages":stages,"procedural_order":workflow,"prefabs":{PREFAB:{"placement":{
            "rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],"attributes":[],"components":{},"issues":[]},
            "runtime":[]}},"wire_resources":{},"wire_prefabs":{}});
    let mut road = crate::road_object_placement::tests::source_value();
    road["schema"] = if current { 3 } else { 2 }.into();
    if current {
        road["adapter"] = road_object_source::HIERARCHY_VERSION.into();
        road["stage_layout"] = "mode-description".into();
    }
    road["stages"][0]["scene_order"] = 0.into();
    road["stages"][0]["disabled"] = disabled;
    road["workflow"] = workflow;
    road["sources"] = sources;
    let source = road_object_source::Source::parse(
        &serde_json::to_vec(&road).unwrap(),
        &"a".repeat(64),
        &serde_json::from_value::<Vec<SourceFile>>(road["sources"].clone()).unwrap(),
        || false,
    )
    .unwrap();
    (value, source)
}
fn read(value: &Value, road: &road_object_source::Source) -> Result<Source, String> {
    Source::read(
        &serde_json::to_vec(value).unwrap(),
        road,
        &BTreeMap::from([("Field".into(), 1)]),
        0,
        &|| false,
    )
}
#[test]
fn both_activation_layouts_reuse_source_rule_and_prefab_preparation() {
    for current in [false, true] {
        let (value, road) = fixture(current);
        let source = read(&value, &road).unwrap();
        assert_eq!(source.programs().len(), 4);
        assert_eq!(source.prefabs().len(), 1);
        assert_eq!(source.runtime(PREFAB).unwrap().len(), 0);
        assert_eq!(
            source.sha256(),
            format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
        );
    }
}
#[test]
fn changed_headers_workflow_rules_and_prefab_coverage_fail_before_publication() {
    let (original, road) = fixture(true);
    for (pointer, bad) in [
        ("/schema", json!(2)),
        ("/inspection_only", json!(false)),
        ("/game_code_executed", json!(true)),
        ("/extractor_sha256", json!("b".repeat(64))),
        ("/sources", json!([])),
        ("/stage_layout", json!("legacy")),
        ("/stages/0/order", json!(2)),
        ("/stages/0/disabled", json!(false)),
        ("/stages/0/description", json!("changed")),
        ("/stages/2/path/0/distance", json!(0)),
        ("/stages/2/path/0/prefabs", json!([])),
        ("/prefabs", json!({})),
        ("/procedural_order/stages/1/component_enabled", json!(false)),
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = bad;
        assert!(read(&value, &road).is_err(), "{pointer}");
    }
    let mut value = original.clone();
    value["stages"][0]["path"] = json!([]);
    assert!(read(&value, &road).is_err());
    let mut value = original.clone();
    value["stages"][2].as_object_mut().unwrap().remove("start");
    assert!(read(&value, &road).is_err());
    let mut value = original;
    value["extra_sources"] =
        json!([{"relative_path":"Bundles/maps/maps.bundle","sha256":"b".repeat(64),"bytes":10}]);
    assert!(read(&value, &road).is_err());
}
#[test]
fn runtime_and_wire_resource_inputs_remain_bounded_and_identity_checked() {
    let (mut value, road) = fixture(false);
    let runtime = json!({"game_object":1,"object_id":2,"name":"tower","layer":0,
        "hierarchy_active_without_root":true,"local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
        "component":["Assembly-CSharp","","PowerlineNode"],"fields":{"MaxDistance":50.,"WirePrefab":{"guid":"a".repeat(32)}}});
    value["prefabs"][PREFAB]["runtime"] = json!([runtime.clone()]);
    let wire = "assets/bundled/prefabs/wire_prefab.prefab";
    value["wire_resources"] = json!({"a".repeat(32):wire});
    value["wire_prefabs"] = json!({wire:[runtime.clone()]});
    assert!(read(&value, &road).is_ok());
    for bad in [
        json!([runtime.clone(), runtime.clone()]),
        json!([{"object_id":2}]),
    ] {
        let mut changed = value.clone();
        changed["prefabs"][PREFAB]["runtime"] = bad;
        assert!(read(&changed, &road).is_err());
    }
    value["wire_prefabs"] = json!({});
    assert!(read(&value, &road).is_err());
    value["wire_resources"] = json!({});
    assert!(read(&value, &road).is_err());
}
#[test]
fn utility_sockets_use_the_existing_path_compiler_and_keep_opaque_source_roles() {
    let (mut value, road) = fixture(true);
    value["prefabs"][PREFAB]["placement"]["components"]["TerrainPathConnect"] = 1.into();
    value["prefabs"][PREFAB]["placement"]["attributes"] = json!([{
        "kind":"TerrainPathConnect","root":true,"position":[0.,0.,0.],"fields":{"Type":12345}
    }]);
    let source = read(&value, &road).unwrap();
    assert!(source.prefabs()[PREFAB].terrain.unsupported.is_empty());
    value["prefabs"][PREFAB]["placement"]["attributes"][0]["fields"]["Unknown"] = true.into();
    assert!(read(&value, &road).is_err());
}
#[test]
fn cancellation_and_input_limits_cannot_return_a_source() {
    let (value, road) = fixture(true);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(Source::read(&bytes, &road, &BTreeMap::new(), 0, &|| true).is_err());
    assert!(
        Source::read(
            &vec![b' '; LIMIT as usize + 1],
            &road,
            &BTreeMap::new(),
            0,
            &|| false
        )
        .is_err()
    );
    let checks = std::cell::Cell::new(0);
    assert!(
        Source::read(
            &bytes,
            &road,
            &BTreeMap::from([("Field".into(), 1)]),
            0,
            &|| {
                checks.set(checks.get() + 1);
                checks.get() > 3
            }
        )
        .is_err()
    );
}

#[test]
#[ignore = "Requires owned installation paths and prepared catalog, road and utility captures"]
fn owned_captures_prepare_and_reuse_cache_without_an_extractor() {
    let cases: Vec<Value> = serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("MAPPER_POWERLINE_TEST_CASES").expect("MAPPER_POWERLINE_TEST_CASES"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let path = |name: &str| Path::new(case[name].as_str().unwrap());
        let root = path("root");
        let installation = installation::inspect(root, || false).unwrap();
        let catalog =
            crate::prefab_assets::load(root, path("catalog"), &installation, || false).unwrap();
        let infrastructure =
            crate::infrastructure::Source::load(path("infrastructure"), &catalog, || false)
                .unwrap();
        let road = road_object_source::Source::load(root, path("road"), &infrastructure, || false)
            .unwrap();
        let flags = infrastructure.profile.topology_flags.as_ref().unwrap();
        let protected = infrastructure.profile.protected_topology as u32;
        let source = Source::load(
            root,
            path("capture"),
            &catalog,
            &road,
            flags,
            protected,
            || false,
        )
        .unwrap();
        assert_eq!(source.programs().len(), 5);
        assert_eq!(source.prefabs().len(), 9);
        assert_eq!(source.wire_resources().len(), 1);
        let cache = crate::storage::local_data().unwrap().join(CACHE);
        let temporary = tempfile::tempdir().unwrap();
        let capture = temporary.path().join("source.json");
        std::fs::copy(path("capture"), &capture).unwrap();
        metadata_runtime::publish_unkeyed(&capture, &cache, source.sha256(), LIMIT).unwrap();
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(path("capture")).unwrap())
            ),
            source.sha256()
        );
        let prepared = prepare(root, &catalog, &road, flags, protected, || false).unwrap();
        let reused = available(root, &catalog, &road, flags, protected, || false).unwrap();
        assert_eq!(prepared.sha256(), source.sha256());
        assert_eq!(reused.sha256(), source.sha256());
        let objects = source
            .bind_objects(&road, &ResourceRules::default(), || false)
            .unwrap();
        assert_eq!(
            objects.path_family,
            road_object_source::PathFamily::Powerline
        );
        assert_eq!(objects.stages.len(), 2);
        assert_eq!(objects.prefabs.len(), 9);
        let templates = source.wire_templates(|| false).unwrap().unwrap();
        assert_eq!(templates.len(), 9);
        assert_eq!(templates.values().map(Vec::len).sum::<usize>(), 7);
        let mut placed = Vec::new();
        let mut instance = 0u64;
        for (path, templates) in &templates {
            for step in 0..3 {
                let pose = papyrus_terrain::environment::Transform {
                    position: [step as f32 * 25., 10., 0.],
                    rotation: [0., 0., 0., 1.],
                    scale: [1.2, 0.9, 1.1],
                };
                for template in templates {
                    placed.push(template.place(instance, path, &pose).unwrap());
                }
                instance += 1;
            }
        }
        let nodes: Vec<_> = placed.iter().map(PlacedWireNode::input).collect();
        let wires = papyrus_terrain::powerline_wires::assemble(
            &nodes,
            papyrus_terrain::powerline_wires::Settings {
                minimum_chain_nodes: 3,
                maximum_nodes: 100,
                maximum_groups: 16,
                maximum_chains: 100,
                maximum_spans: 1000,
                maximum_work: 10000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(wires.groups.len(), 7);
        assert_eq!(wires.chains.len(), 7);
        assert!(!wires.spans.is_empty());
        println!(
            "Utility wire inputs {} assembled {} controlled nodes and {} spans",
            source.sha256(),
            nodes.len(),
            wires.spans.len()
        );
        let layers = crate::road_object_placement::tests::layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let paths = [crate::road_object_placement::Road {
            hierarchy: 16,
            width: 6.,
            start: true,
            end: true,
            points: (-200..=200)
                .step_by(5)
                .map(|x| [x as f32, 50., 0.])
                .collect(),
        }];
        let report = crate::road_object_placement::fit(
            &layers,
            &alpha,
            &paths,
            &objects,
            crate::road_object_placement::Settings {
                seed: 42,
                maximum_work: 200_000,
                maximum_poses: 1000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(!report.provisional_poses.is_empty());
        assert!(!report.budget_exhausted);
        let stage_counts: Vec<_> = (0..objects.stages.len())
            .map(|stage| {
                report
                    .provisional_poses
                    .iter()
                    .filter(|pose| pose.stage == stage)
                    .count()
            })
            .collect();
        assert!(
            stage_counts.iter().all(|count| *count > 0),
            "Missing utility stage output {stage_counts:?}"
        );
        assert!(
            report
                .provisional_poses
                .iter()
                .all(|pose| objects.prefabs.contains_key(&pose.prefab))
        );
        println!(
            "Utility source {} verified with {} prefabs and {} fitted utility instances across stages {:?}",
            source.sha256(),
            source.prefabs().len(),
            report.provisional_poses.len(),
            stage_counts
        );
    }
}
