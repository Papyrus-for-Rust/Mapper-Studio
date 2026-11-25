use super::tests::{layers, road, source, source_value};
use super::*;
use papyrus_terrain::footprints::{ProtectionScope, RoadTransaction};

const PREFAB: &str = "assets/bundled/prefabs/autospawn/decor/test/a.prefab";

fn sources() -> (Source, Source) {
    let mut document = source_value();
    document["schema"] = 2.into();
    document["stages"][0]["scene_order"] = 0.into();
    let mut second = document["stages"][0].clone();
    second["order"] = 1.into();
    second["object_id"] = 2.into();
    second["scene_order"] = 2.into();
    second["side"][0]["offset"] = 30.into();
    document["stages"].as_array_mut().unwrap().push(second);
    document["prefabs"][PREFAB]["components"] = serde_json::json!({"MonumentInfo":1});
    document["prefabs"][PREFAB]["attributes"] = serde_json::json!([
        {"kind":"TerrainAnchor","root":true,"position":[0.,0.,0.],"fields":{"Extents":1.,"Offset":0.,"Radius":0.,"SlopeScale":0.}},
        {"kind":"TerrainHeightAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":3.,"Fade":0.,"Delta":2.}},
        {"kind":"TerrainPathConnect","root":true,"position":[2.,0.,0.],"fields":{"Type":1}}
    ]);
    let entries = [
        (1, "PlaceRoadObjects"),
        (3, "PlacePowerlineObjects"),
        (2, "PlaceRoadObjects"),
        (4, "PlacePowerlineObjects"),
    ];
    document["workflow"] = serde_json::json!({"format":"papyrus-procedural-order","schema":1,
        "stages":entries.iter().enumerate().map(|(order,(id,kind))|serde_json::json!({
            "object_id":id,"order":order,"kind":kind,"description":"Objects","mode":-1,
            "disabled":false,"component_enabled":true,"hierarchy_active":true})).collect::<Vec<_>>()});
    let road_source = source(document.clone());
    document["schema"] = 6.into();
    document["adapter"] = source::PATH_VERSION.into();
    document["path_family"] = "powerline".into();
    for (index, stage) in document["stages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        stage["object_id"] = (index as i64 + 3).into();
        stage["scene_order"] = (index * 2 + 1).into();
        stage["side"][0]["offset"] = (60 + index * 30).into();
    }
    (road_source, source(document))
}
fn settings() -> Settings {
    Settings {
        seed: 42,
        maximum_work: 100_000,
        maximum_poses: 10_000,
    }
}
fn scope(namespace: &str) -> ProtectionScope {
    ProtectionScope {
        namespace: namespace.into(),
        protected_topology: 8,
        role_exceptions: 0,
        material_role_exceptions: 0,
        maximum_height_change: None,
    }
}
fn emitter<'a, F: Fn() -> bool>(
    layers: &'a Layers,
    alpha: &'a [u8],
    roads: &'a [Road],
    source: &'a Source,
    utility: &'a Source,
    cancel: F,
) -> Emitter<'a, F> {
    let flags = BTreeMap::from([
        ("Field".into(), 1),
        ("Decor".into(), 2),
        ("Monument".into(), 8),
    ]);
    let spec = decor_source::compile_road_effects(&source.prefabs[PREFAB], &flags, 8).unwrap();
    let transaction = RoadTransaction::new_objects(
        layers,
        alpha,
        BTreeMap::from([
            (format!("road:{PREFAB}"), spec.clone()),
            (format!("utility:{PREFAB}"), spec),
        ]),
        11,
        8,
        || false,
    )
    .unwrap()
    .unwrap();
    let mut emitter = Emitter::new(
        layers,
        alpha,
        roads,
        source,
        settings(),
        Some(transaction),
        cancel,
    )
    .unwrap()
    .unwrap()
    .with_utilities(utility, settings())
    .unwrap()
    .unwrap();
    emitter.begin_workflow(1, 0, 0, 2).unwrap();
    emitter.use_scope(scope("road")).unwrap();
    emitter
}
fn decoration<F: Fn() -> bool>(emitter: &mut Emitter<'_, F>) {
    let stage = serde_json::from_value(serde_json::json!({"id":"20","order":0,"name":"Disabled",
        "kind":"uniform","folder":"decor/test","enabled":false,"mode":-1,
        "filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},
        "filter_mode":1,"anchor_mode":0,"distance":250.,"dithering":0.,"density":0.,
        "cluster":null,"prefabs":[]}))
    .unwrap();
    emitter
        .emit_decoration(DecorationStage {
            stage: &stage,
            index: 0,
            seed: 42,
            realm: 2,
            prefabs: &BTreeMap::new(),
            attempts: &mut papyrus_terrain::decor_stream::Budget::new(1000).unwrap(),
            water: None,
        })
        .unwrap()
        .unwrap();
}
fn complete() -> (Report, papyrus_terrain::footprints::Output) {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let roads = [road()];
    let (source, utility) = sources();
    let mut emitter = emitter(&layers, &alpha, &roads, &source, &utility, || false);
    emitter.use_scope(scope("road")).unwrap();
    emitter.emit_stage(0).unwrap().unwrap();
    let first = emitter.progress();
    let sockets = emitter.live_road_connectors(1).unwrap();
    assert!(!sockets.is_empty());
    assert!(sockets.iter().all(|(id, _)| {
        emitter
            .run
            .transaction
            .as_ref()
            .unwrap()
            .applied()
            .iter()
            .any(|row| row.pose == *id)
    }));
    let address = emitter.view().height.as_ptr();
    assert_ne!(emitter.view().height, layers.height);
    let mut path = road();
    path.width = 0.;
    emitter.set_utility_paths(vec![path]).unwrap().unwrap();
    emitter.use_scope(scope("utility")).unwrap();
    emitter.emit_utility_stage(0).unwrap().unwrap();
    assert!(emitter.progress().admissions > first.admissions);
    assert!(emitter.progress().surface_samples > first.surface_samples);
    assert!(emitter.progress().terrain_work > first.terrain_work);
    assert_eq!(address, emitter.view().height.as_ptr());
    emitter.use_scope(scope("road")).unwrap();
    emitter.emit_stage(1).unwrap().unwrap();
    emitter.use_scope(scope("utility")).unwrap();
    emitter.emit_utility_stage(1).unwrap().unwrap();
    decoration(&mut emitter);
    let (report, terrain) = emitter.finish().unwrap().unwrap();
    let utility = report.utilities.as_ref().unwrap();
    let utility_ids = utility.global_admission_ids.as_ref().unwrap();
    let road_ids = report.global_admission_ids.as_ref().unwrap();
    assert!(!utility_ids.is_empty() && !road_ids.is_empty());
    assert!(utility_ids.iter().all(|id| !road_ids.contains(id)));
    assert_eq!(
        report.global_admission_count,
        utility.global_admission_count
    );
    assert_eq!(report.surface_samples, utility.surface_samples);
    assert!(layers.height.iter().all(|value| *value == 18000));
    (report, terrain.unwrap())
}
#[test]
fn utility_stages_share_live_terrain_ids_and_budgets_and_replay_deterministically() {
    let (report, terrain) = complete();
    let (replay, other) = complete();
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    assert_eq!(terrain.height, other.height);
    assert_eq!(terrain.topology, other.topology);
}
#[test]
fn rejected_road_objects_do_not_supply_utility_connectors() {
    let mut layers = layers();
    layers.topology.fill(9);
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let roads = [road()];
    let (source, utility) = sources();
    let mut emitter = emitter(&layers, &alpha, &roads, &source, &utility, || false);
    emitter.use_scope(scope("road")).unwrap();
    emitter.emit_stage(0).unwrap().unwrap();
    assert!(!emitter.run.path.out.provisional_poses.is_empty());
    assert!(emitter.live_road_connectors(1).unwrap().is_empty());
    assert!(
        emitter
            .run
            .transaction
            .as_ref()
            .unwrap()
            .applied()
            .is_empty()
    );
}
#[test]
fn utility_publication_requires_layout_source_order_completion_and_available_budget() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let roads = [road()];
    let (source, utility) = sources();
    let new = || emitter(&layers, &alpha, &roads, &source, &utility, || false);
    let mut missing = new();
    assert!(missing.emit_utility_stage(0).is_err());
    assert!(missing.finish().is_err());
    let mut duplicate = new();
    duplicate.set_utility_paths(vec![]).unwrap().unwrap();
    assert!(duplicate.set_utility_paths(vec![]).is_err());
    assert!(duplicate.finish().is_err());
    let mut unordered = new();
    unordered.set_utility_paths(vec![]).unwrap().unwrap();
    unordered.emit_utility_stage(0).unwrap().unwrap();
    assert!(unordered.emit_stage(0).is_err());
    assert!(unordered.finish().is_err());
    let mut incomplete = new();
    incomplete.emit_stage(0).unwrap().unwrap();
    incomplete.set_utility_paths(vec![]).unwrap().unwrap();
    incomplete.emit_utility_stage(0).unwrap().unwrap();
    assert!(incomplete.finish().is_err());
    let mut exhausted = new();
    exhausted.emit_stage(0).unwrap().unwrap();
    exhausted.set_utility_paths(vec![]).unwrap().unwrap();
    exhausted.emit_utility_stage(0).unwrap().unwrap();
    exhausted.emit_stage(1).unwrap().unwrap();
    exhausted.emit_utility_stage(1).unwrap().unwrap();
    decoration(&mut exhausted);
    exhausted
        .utility
        .as_mut()
        .unwrap()
        .state
        .out
        .budget_exhausted = true;
    assert!(exhausted.finish().is_err());
    let cancel = std::cell::Cell::new(false);
    let mut cancelled = emitter(&layers, &alpha, &roads, &source, &utility, || cancel.get());
    cancelled.emit_stage(0).unwrap().unwrap();
    cancel.set(true);
    assert!(cancelled.set_utility_paths(vec![]).unwrap().is_none());
    cancel.set(false);
    assert!(cancelled.finish().is_err());
    assert!(layers.height.iter().all(|value| *value == 18000));
}
