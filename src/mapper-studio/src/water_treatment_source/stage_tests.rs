use super::{fixture, read};
use crate::{
    monument_tag_geometry::{Placement, Prepared, Registry},
    water_treatment_stage as stage,
};
use papyrus_terrain::{
    environment::Transform,
    native_layers::{Layers, dimensions},
    path_geometry::Path,
    roadside_decor,
};
use serde_json::json;
use std::collections::BTreeMap;

fn layers() -> Layers {
    let (hr, n) = dimensions(1000).unwrap();
    let cells = n * n;
    let mut splat = vec![0; cells * 8];
    splat[..cells].fill(255);
    Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 2,
        height_resolution: hr,
        surface_resolution: n,
        height: vec![17038; hr * hr],
        splat,
        biome: vec![51; cells * 5],
        topology: vec![1; cells],
        normalized_splat_cells: cells,
        normalized_biome_cells: cells,
    }
}

fn registry(plant: bool, blocked: bool) -> Registry {
    let mut value = json!({"space":"prefab-root-local-unity","monuments":[],"tags":[],
        "issues":[],"complete":true,"root_rotation":[0.,0.,0.,1.],"root_scale":[1.,1.,1.],"nodes_visited":1});
    if plant {
        value["monuments"] = json!([{"object_id":20,"root":true,"hierarchy_active":true,
            "component_enabled":true,"local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
            "rotation_chain":[],"display_phrase":{"token":"water_treatment_plant","english":"Water Treatment"}}]);
    }
    if blocked {
        value["tags"] = json!([{"object_id":1,"root":true,"hierarchy_active":true,"component_enabled":true,
            "local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
            "rotation_chain":[],"auto_find_monument":false,"attached_monument":{"file_id":0,"path_id":0},
            "collider_order":"serialized_component_order","colliders":[{"object_id":11,"class_id":65,
                "enabled":true,"trigger":false,"center":[0.,0.,0.],"size":[4000.,4000.,4000.]}]}]);
    }
    let prepared = Prepared::read(&serde_json::to_vec(&value).unwrap()).unwrap();
    Registry::assemble(
        &BTreeMap::from([("plant".into(), prepared)]),
        &[Placement {
            instance: 7,
            prefab: "plant",
            pose: Transform {
                position: [0., 20., 0.],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
            },
        }],
        stage::MONUMENT_TOKEN,
        || false,
    )
    .unwrap()
    .unwrap()
}

fn settings() -> stage::Settings {
    stage::Settings {
        seed: 42,
        realm_mask: 2,
        maximum_terrain_work: 32_000_000,
        maximum_sampling_attempts: 10000,
        maximum_fitting_samples: 1_000_000,
        maximum_clearance_work: 10000,
    }
}

#[test]
fn gate_clearance_and_activation_control_real_shared_admissions() {
    let layers = layers();
    let alpha = vec![255; layers.surface_resolution * layers.surface_resolution];
    let path = Path::from_controls(&[[0., 20., -500.], [0., 20., 500.]], 10000, || false)
        .unwrap()
        .unwrap();
    let roads = [roadside_decor::Road {
        hierarchy: 0,
        path: &path,
        spline: true,
        width: 10.,
    }];
    for current in [true, false] {
        for (plant, blocked, enabled) in [
            (false, false, true),
            (true, false, true),
            (true, true, true),
            (true, false, false),
        ] {
            let mut value = fixture(current);
            if !enabled {
                value["stages"][0]["component_enabled"] = false.into();
            }
            let source = read(&value).unwrap();
            let registry = registry(plant, blocked);
            let output = stage::generate(
                stage::Input {
                    layers: &layers,
                    alpha: &alpha,
                    roads: &roads,
                    source: &source,
                    registry: &registry,
                    known_topology: 3,
                    protected_topology: 2,
                    water: None,
                    height_reservations: None,
                },
                settings(),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(output.report.eligible_monuments, usize::from(plant));
            assert_eq!(output.terrain.is_some(), plant);
            let exported = stage::prefab_records(&output.report, |_| Some(7), || false)
                .unwrap()
                .unwrap();
            assert_eq!(exported.len(), output.report.admitted_instances);
            assert_eq!(
                output.report.admitted_instances > 0,
                plant && !blocked && enabled
            );
            if let Some(terrain) = output.terrain {
                assert_eq!(terrain.height, layers.height);
            }
            if blocked {
                assert!(output.report.programs[0].rejected_checks > 0);
                assert!(output.report.clearance_work > 0);
            }
            assert!(!output.report.alpha_modified);
            assert!(!output.report.physics_validated);
            assert!(alpha.iter().all(|v| *v == 255));
        }
    }
}

#[test]
fn exhaustion_and_cancellation_do_not_publish_partial_terrain() {
    let layers = layers();
    let before = (
        layers.height.clone(),
        layers.topology.clone(),
        layers.splat.clone(),
    );
    let alpha = vec![255; layers.surface_resolution * layers.surface_resolution];
    let path = Path::from_controls(&[[0., 20., -500.], [0., 20., 500.]], 10000, || false)
        .unwrap()
        .unwrap();
    let roads = [roadside_decor::Road {
        hierarchy: 0,
        path: &path,
        spline: true,
        width: 10.,
    }];
    let source = read(&fixture(true)).unwrap();
    let registry = registry(true, true);
    let input = || stage::Input {
        layers: &layers,
        alpha: &alpha,
        roads: &roads,
        source: &source,
        registry: &registry,
        known_topology: 3,
        protected_topology: 2,
        water: None,
        height_reservations: None,
    };
    let mut limited = settings();
    limited.maximum_clearance_work = 1;
    assert!(
        stage::generate(input(), limited, || false)
            .err()
            .unwrap()
            .contains("clearance")
    );
    let stopped = std::cell::Cell::new(0);
    assert!(
        stage::generate(input(), settings(), || {
            stopped.set(stopped.get() + 1);
            stopped.get() > 20
        })
        .unwrap()
        .is_none()
    );
    assert_eq!(
        (&layers.height, &layers.topology, &layers.splat),
        (&before.0, &before.1, &before.2)
    );
}
