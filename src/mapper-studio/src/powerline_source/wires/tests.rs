use super::*;
use serde_json::json;

const PREFAB: &str = "assets/bundled/prefabs/autospawn/decor/test/a.prefab";
const WIRE: &str = "assets/bundled/prefabs/wire_prefab.prefab";
fn runtime_rows() -> Vec<RuntimeComponent> {
    let fields = json!({"m_GameObject":{"m_FileID":0,"m_PathID":1},
        "m_Script":{"m_FileID":1,"m_PathID":100},"m_Enabled":1,"m_Name":""});
    let row = json!({"game_object":1,"object_id":2,"name":PREFAB,"layer":0,
        "hierarchy_active_without_root":true,
        "local_to_root":[[0.,0.,2.,3.],[0.,3.,0.,4.],[-2.,0.,0.,5.],[0.,0.,0.,1.]],
        "component":["Assembly-CSharp","","PowerlineNode"],"fields":fields});
    let mut node = row.clone();
    node["fields"]["WirePrefab"] = json!({"guid":"a".repeat(32)});
    node["fields"]["MaxDistance"] = 50.into();
    let mut helper = row;
    helper["object_id"] = 3.into();
    helper["component"] = json!(["Assembly-CSharp", "", "PowerLineWireConnectionHelper"]);
    helper["fields"]["showgizmo"] = true.into();
    helper["fields"]["connections"] = json!([{"inOffset":{"x":-1.,"y":0.,"z":2.},
        "outOffset":{"x":1.,"y":2.,"z":3.},"radius":0.04,"hidden":0}]);
    serde_json::from_value(json!([node, helper])).unwrap()
}
fn resources() -> BTreeMap<String, String> {
    BTreeMap::from([("a".repeat(32), WIRE.into())])
}
fn loaded_source(rows: Vec<RuntimeComponent>, node_count: usize, helper_count: usize) -> Source {
    let (mut doc, road) = crate::powerline_source::tests::fixture(false);
    // Raw runtime fields are deliberately retained by the capture format.
    doc["prefabs"][PREFAB]["runtime"]=serde_json::to_value(rows.iter().map(|row|json!({
        "game_object":row.game_object,"object_id":row.object_id,"name":row.name,"layer":row.layer,
        "hierarchy_active_without_root":row.hierarchy_active_without_root,"local_to_root":row.local_to_root,
        "component":row.component,"fields":row.fields.as_ref().map(|raw|serde_json::from_str::<Value>(raw.get()).unwrap())
    })).collect::<Vec<_>>()).unwrap();
    doc["prefabs"][PREFAB]["placement"]["components"] = json!({"PowerlineNode":node_count,
        "PowerLineWireConnectionHelper":helper_count});
    doc["wire_resources"] = json!({"a".repeat(32):WIRE});
    doc["wire_prefabs"] = json!({WIRE:[]});
    Source::read(
        &serde_json::to_vec(&doc).unwrap(),
        &road,
        &BTreeMap::from([("Field".into(), 1)]),
        0,
        &|| false,
    )
    .unwrap()
}
fn change_fields(row: &mut RuntimeComponent, change: impl FnOnce(&mut Value)) {
    let mut fields: Value = serde_json::from_str(row.fields.as_ref().unwrap().get()).unwrap();
    change(&mut fields);
    row.fields = Some(serde_json::value::to_raw_value(&fields).unwrap());
}
#[test]
fn local_affine_frames_follow_full_rotated_scaled_and_mirrored_root_poses() {
    let template = templates(&runtime_rows(), &resources()).unwrap().remove(0);
    let half = std::f32::consts::FRAC_1_SQRT_2;
    let pose = environment::Transform {
        position: [10., 20., 30.],
        rotation: [0., half, 0., half],
        scale: [-2., 3., 4.],
    };
    let placed = template.place(12, "scene-root", &pose).unwrap();
    let node = placed.input();
    assert_eq!(node.instance, 12);
    assert_eq!(node.component, 2);
    assert_eq!(node.root, "scene-root");
    for (actual, expected) in [
        (node.position, [30., 32., 36.]),
        (node.connections.unwrap()[0].incoming, [38., 32., 44.]),
        (node.connections.unwrap()[0].outgoing, [22., 50., 48.]),
    ] {
        for axis in 0..3 {
            assert!((actual[axis] - expected[axis]).abs() < 0.0001);
        }
    }
    assert_eq!(node.maximum_distance, 50.);
}
#[test]
fn disabled_components_keep_awake_inputs_and_inactive_hierarchies_stay_excluded() {
    let mut rows = runtime_rows();
    for row in &mut rows {
        change_fields(row, |fields| fields["m_Enabled"] = false.into());
    }
    assert_eq!(templates(&rows, &resources()).unwrap().len(), 1);
    let source = loaded_source(rows, 1, 1);
    assert_eq!(
        source.wire_templates(|| false).unwrap().unwrap()[PREFAB].len(),
        1
    );
    let mut inactive = runtime_rows();
    for row in &mut inactive {
        row.hierarchy_active_without_root = false;
    }
    let source = loaded_source(inactive, 0, 0);
    assert!(source.wire_templates(|| false).unwrap().unwrap()[PREFAB].is_empty());
}
#[test]
fn first_same_object_helper_is_selected_and_missing_helpers_remain_explicit() {
    let mut rows = runtime_rows();
    let mut second:RuntimeComponent=serde_json::from_value(json!({
        "game_object":1,"object_id":4,"name":PREFAB,"layer":0,"hierarchy_active_without_root":true,
        "local_to_root":rows[1].local_to_root,"component":["Assembly-CSharp","","PowerLineWireConnectionHelper"],
        "fields":serde_json::from_str::<Value>(rows[1].fields.as_ref().unwrap().get()).unwrap()})).unwrap();
    change_fields(&mut second, |fields| fields["connections"] = json!([]));
    rows.push(second);
    assert_eq!(
        templates(&rows, &resources()).unwrap()[0]
            .connections
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    rows.truncate(1);
    assert!(
        templates(&rows, &resources()).unwrap()[0]
            .connections
            .is_none()
    );
}
#[test]
fn incomplete_unknown_nonfinite_or_misaligned_component_inputs_fail() {
    for (target, pointer, value) in [
        (0, "/MaxDistance", Value::Null),
        (0, "/MaxDistance", json!(10001)),
        (0, "/WirePrefab", json!({"guid":"b".repeat(32)})),
        (0, "/m_GameObject/m_PathID", json!(999)),
        (0, "/m_Enabled", json!(2)),
        (1, "/connections", json!(null)),
        (1, "/connections/0/hidden", json!("yes")),
        (1, "/connections/0/inOffset/x", json!(10001)),
    ] {
        let mut rows = runtime_rows();
        change_fields(&mut rows[target], |fields| {
            *fields.pointer_mut(pointer).unwrap() = value
        });
        assert!(
            templates(&rows, &resources()).is_err(),
            "{target} {pointer}"
        );
    }
    let mut extended = runtime_rows();
    change_fields(&mut extended[0], |fields| fields["NewWireRule"] = 1.into());
    assert!(templates(&extended, &resources()).is_err());
    let mut moved = runtime_rows();
    moved[1].local_to_root[0][3] += 1.;
    assert!(templates(&moved, &resources()).is_err());
}
#[test]
fn roster_mismatches_and_cancellation_do_not_publish_partial_templates() {
    let source = loaded_source(runtime_rows(), 2, 1);
    assert!(source.wire_templates(|| false).is_err());
    let source = loaded_source(runtime_rows(), 1, 1);
    assert!(source.wire_templates(|| true).unwrap().is_none());
    let polls = std::cell::Cell::new(0);
    assert!(
        source
            .wire_templates(|| {
                polls.set(polls.get() + 1);
                polls.get() == 2
            })
            .unwrap()
            .is_none()
    );
    let template = source
        .wire_templates(|| false)
        .unwrap()
        .unwrap()
        .remove(PREFAB)
        .unwrap()
        .remove(0);
    let mut pose = environment::Transform {
        position: [0.; 3],
        rotation: [0., 0., 0., 1.],
        scale: [1.; 3],
    };
    pose.scale[0] = 0.;
    assert!(template.place(1, "root", &pose).is_err());
}
