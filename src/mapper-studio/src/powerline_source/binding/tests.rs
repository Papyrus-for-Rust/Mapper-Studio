use super::*;
use crate::road_object_placement::{self, Road};
use serde_json::{Value, json};

fn bound_fixture(current: bool) -> (Value, road_object_source::Source) {
    let (mut value, road) = crate::powerline_source::tests::fixture(current);
    value["sources"].as_array_mut().unwrap().push(json!({
        "relative_path":"RustClient_Data/Plugins/x86_64/RustNative.dll",
        "sha256":if current {"b".repeat(64)} else {"c".repeat(64)},"bytes":1,
    }));
    value["sources"].as_array_mut().unwrap().push(json!({
        "relative_path":"GameAssembly.dll","sha256":"d".repeat(64),"bytes":10,
    }));
    let mut document = serde_json::to_value(road).unwrap();
    document["sources"] = value["sources"].clone();
    (
        value.clone(),
        road_object_source::Source::parse(
            &serde_json::to_vec(&document).unwrap(),
            &"a".repeat(64),
            &serde_json::from_value::<Vec<SourceFile>>(value["sources"].clone()).unwrap(),
            || false,
        )
        .unwrap(),
    )
}
fn source(value: &Value, road: &road_object_source::Source) -> Source {
    Source::read(
        &serde_json::to_vec(value).unwrap(),
        road,
        &BTreeMap::from([("Field".into(), 1)]),
        0,
        &|| false,
    )
    .unwrap()
}
fn paths() -> Vec<Road> {
    vec![Road {
        hierarchy: 16,
        width: 6.,
        start: false,
        end: false,
        points: (-200..=200)
            .step_by(5)
            .map(|x| [x as f32, 50., 0.])
            .collect(),
    }]
}
fn sequenced_fixture() -> (Value, road_object_source::Source) {
    let (mut value, road) = bound_fixture(true);
    let path = value["prefabs"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    value["prefabs"][&path]["placement"]["components"]["PathSequencePowerline"] = 1.into();
    value["prefabs"][&path]["runtime"] = json!([{
        "game_object":1,"object_id":2,"name":path,"hierarchy_active_without_root":true,"layer":16,
        "local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
        "component":["Assembly-CSharp","","PathSequencePowerline"],"fields":{"Rule":1}
    }]);
    (value, road)
}
#[test]
fn captured_sequence_runtime_becomes_a_rooted_fitting_attribute() {
    let (value, road) = sequenced_fixture();
    let source = source(&value, &road);
    let objects = source
        .bind_objects(&road, &ResourceRules::default(), || false)
        .unwrap();
    let prefab = objects.prefabs.values().next().unwrap();
    let attributes: Vec<_> = prefab
        .attributes
        .iter()
        .filter(|attribute| attribute["kind"] == "PathSequencePowerline")
        .collect();
    assert_eq!(attributes.len(), 1);
    assert_eq!(attributes[0]["root"], true);
    assert_eq!(attributes[0]["fields"], json!({"Rule":1}));
}
#[test]
fn missing_sequence_records_unknown_rules_and_ambiguous_roots_fail_binding() {
    let (original, road) = sequenced_fixture();
    let path = original["prefabs"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let mut missing = original.clone();
    missing["prefabs"][&path]["runtime"] = json!([]);
    assert!(
        source(&missing, &road)
            .bind_objects(&road, &ResourceRules::default(), || false)
            .is_err()
    );
    let mut unknown = original.clone();
    unknown["prefabs"][&path]["runtime"][0]["fields"]["Rule"] = 2.into();
    assert!(
        source(&unknown, &road)
            .bind_objects(&road, &ResourceRules::default(), || false)
            .is_err()
    );
    let mut ambiguous = original;
    let mut other = ambiguous["prefabs"][&path]["runtime"][0].clone();
    other["game_object"] = 3.into();
    other["object_id"] = 4.into();
    other["component"][2] = "OtherScript".into();
    ambiguous["prefabs"][&path]["runtime"]
        .as_array_mut()
        .unwrap()
        .push(other);
    assert!(
        source(&ambiguous, &road)
            .bind_objects(&road, &ResourceRules::default(), || false)
            .is_err()
    );
}
#[test]
fn utility_policy_binds_both_source_layouts_and_shares_the_actual_fitter() {
    for current in [false, true] {
        let (value, road) = bound_fixture(current);
        let source = source(&value, &road);
        let utility = source
            .bind_objects(&road, &ResourceRules::default(), || false)
            .unwrap();
        assert_eq!(
            utility.path_family,
            road_object_source::PathFamily::Powerline
        );
        assert_eq!(utility.stages.len(), 1);
        assert_eq!(utility.stages[0].scene_order, Some(3));
        let policy = utility.path_policy.as_ref().unwrap();
        assert!(policy.resource_weights.is_some());
        assert!(policy.powerline_sequence.is_some());
        let layers = road_object_placement::tests::layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let run = || {
            road_object_placement::fit(
                &layers,
                &alpha,
                &paths(),
                &utility,
                road_object_placement::Settings {
                    seed: 42,
                    maximum_work: 100_000,
                    maximum_poses: 1000,
                },
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let report = run();
        assert!(report.provisional_poses.len() >= 3);
        assert!(!report.counts.contains_key("skipped_road_hierarchy"));
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::to_value(run()).unwrap()
        );
    }
}
#[test]
fn installation_receipts_are_retained_with_supported_data_from_unlisted_builds() {
    let (value, road) = bound_fixture(true);
    let source = source(&value, &road);
    let objects = source
        .bind_objects(&road, &ResourceRules::default(), || false)
        .unwrap();
    let document = serde_json::to_value(objects).unwrap();
    assert_eq!(document["sources"], value["sources"]);
    assert_eq!(document["workflow"], value["procedural_order"]);
    assert_eq!(
        document["placement_profile_sha256"],
        format!("{:x}", Sha256::digest(POLICY.as_bytes()))
    );
}
#[test]
fn exclusions_precede_inclusions_and_empty_filtered_rosters_remain_valid() {
    let rules = ResourceRules {
        blacklist: vec!["platform".into()],
        whitelist: vec!["powerline".into()],
    };
    assert!(rules.allows("decor/powerline/powerline_a.prefab"));
    assert!(!rules.allows("decor/powerline/powerlineplatform_a.prefab"));
    assert!(!rules.allows("power substations/big/a.prefab"));
    let case_sensitive = ResourceRules {
        blacklist: vec!["Powerline".into()],
        whitelist: vec![],
    };
    assert!(case_sensitive.allows("powerline_a.prefab"));
    assert!(
        !ResourceRules {
            blacklist: vec![String::new()],
            whitelist: vec![]
        }
        .allows("anything")
    );
    assert!(
        ResourceRules {
            blacklist: vec![],
            whitelist: vec![String::new()]
        }
        .allows("anything")
    );
    let (value, road) = bound_fixture(true);
    let source = source(&value, &road);
    let empty = source
        .bind_objects(
            &road,
            &ResourceRules {
                blacklist: vec![String::new()],
                whitelist: vec![],
            },
            || false,
        )
        .unwrap();
    assert!(empty.prefabs.is_empty());
    let layers = road_object_placement::tests::layers();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let report = road_object_placement::fit(
        &layers,
        &alpha,
        &paths(),
        &empty,
        road_object_placement::Settings {
            seed: 42,
            maximum_work: 100_000,
            maximum_poses: 1000,
        },
        || false,
    )
    .unwrap()
    .unwrap();
    assert!(report.provisional_poses.is_empty());
}
#[test]
fn utility_headers_cannot_be_relabelled_as_roads_or_rivers() {
    let (value, road) = bound_fixture(true);
    let source = source(&value, &road);
    let utility = source
        .bind_objects(&road, &ResourceRules::default(), || false)
        .unwrap();
    let original = serde_json::to_value(utility).unwrap();
    let receipts: Vec<SourceFile> = serde_json::from_value(value["sources"].clone()).unwrap();
    for (pointer, bad) in [
        ("/path_family", json!("road")),
        ("/path_family", json!("river")),
        ("/schema", json!(5)),
        ("/schema", json!(6)),
    ] {
        let mut document = original.clone();
        *document.pointer_mut(pointer).unwrap() = bad;
        assert!(
            road_object_source::Source::parse(
                &serde_json::to_vec(&document).unwrap(),
                &road.infrastructure_sha256,
                &receipts,
                || false
            )
            .is_err()
        );
    }
}
#[test]
fn binding_is_cancelled_privately_and_rejects_excessive_filters() {
    let (value, road) = bound_fixture(true);
    let source = source(&value, &road);
    assert!(
        source
            .bind_objects(&road, &ResourceRules::default(), || true)
            .is_err()
    );
    let checks = std::cell::Cell::new(0);
    assert!(
        source
            .bind_objects(&road, &ResourceRules::default(), || {
                checks.set(checks.get() + 1);
                checks.get() > 2
            })
            .is_err()
    );
    assert!(
        source
            .bind_objects(
                &road,
                &ResourceRules {
                    blacklist: vec!["a".repeat(513)],
                    whitelist: vec![]
                },
                || false
            )
            .is_err()
    );
}
