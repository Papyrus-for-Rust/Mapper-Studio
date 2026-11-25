use mapper_studio::{Project, recipe_options::Options, rustmaps_profile};
use serde_json::json;

#[test]
fn legacy_booleans_survive_and_automatic_rules_keep_existing_defaults() {
    let old = json!({
        "oasis": true,
        "lakes": false,
        "underground_tunnels": false,
        "center_outpost": false,
        "block_car_wrecks": true,
    });
    let options: Options = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(options.oasis, Some(true));
    assert_eq!(options.lakes, Some(false));
    assert_eq!(options.underground_tunnels, Some(false));
    assert_eq!(options.center_outpost, Some(false));
    assert!(options.export_blocked("assets/vehicles/carwreck_a.prefab"));

    let saved = serde_json::to_value(&options).unwrap();
    for (field, value) in old.as_object().unwrap() {
        assert_eq!(&saved[field], value);
    }
    assert!(!saved.as_object().unwrap().contains_key("canyons"));
    assert_eq!(serde_json::from_value::<Options>(saved).unwrap(), options);

    let automatic = Options::default();
    assert!(!automatic.export_blocked("assets/vehicles/carwreck_a.prefab"));
    let explicit_defaults = Options {
        oasis: Some(true),
        lakes: Some(true),
        center_outpost: Some(false),
        ..Options::default()
    };
    assert_eq!(automatic.early_placement_key(), explicit_defaults.early_placement_key());
}

#[test]
fn rustmaps_import_can_restore_a_landmark_to_automatic() {
    let mut base = Project::default();
    base.options.oasis = Some(false);
    base.options.canyons = Some(false);
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("profile.json");
    let profile = json!({
        "version": 5,
        "riverCount": null,
        "oasesConfiguration": {"generate": "NoPreference"},
        "lakesConfiguration": {"generate": "TryGenerate"},
        "removeUndergroundTunnels": true,
    });
    std::fs::write(&path, profile.to_string()).unwrap();

    let imported = rustmaps_profile::load(&path, &base, None).unwrap().recipe;
    assert_eq!(imported.options.oasis, None);
    assert_eq!(imported.options.lakes, Some(true));
    assert_eq!(imported.options.canyons, Some(false));
    assert_eq!(imported.options.underground_tunnels, Some(false));
}
