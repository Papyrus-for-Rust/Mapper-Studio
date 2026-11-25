use super::*;
use crate::road_paint_stage::{
    self as stage,
    tests::{layers, paths, profiles},
};
use papyrus_terrain::{path_surface, road_paint};
use std::cell::Cell;

#[test]
fn roadside_earthwork_rebinds_control_heights_without_moving_xz_or_shared_joins() {
    let layers = layers();
    let (paint, terrain) = profiles();
    let paths = paths(&layers, &paint);
    let roads = [road_paint::Road { path: &paths[0], hierarchy: 1 }, road_paint::Road { path: &paths[1], hierarchy: 1 }];
    let joins = [papyrus_terrain::road_grade::Junction { a: (0,1), b: (1,0) }];
    let output = stage::paint_with_roadside(stage::Input {
        layers: &layers, roads: &roads, junctions: &joins, paint: &paint, terrain: &terrain,
    }, None, 32_000_000, |p| Ok(Some(vec![0.;p.len()])), |_| Ok(Some(terrain.water_no_hit_level)), |frame| {
        Ok(Some(RoadsideChanges { height: Some(frame.layers.height.iter().map(|height| height + 1).collect()), ..Default::default() }))
    }, || false).unwrap().unwrap();
    let grid = papyrus_terrain::path_terrain::Grid {
        size: layers.size as f32, resolution: layers.height_resolution,
        origin_y: paint.height_origin, vertical_span: paint.vertical_span, height: &output.height,
    };
    for (before, after) in paths.iter().zip(&output.roads) {
        for (before, after) in before.points().iter().zip(after.points()) {
            assert_eq!([before[0], before[2]], [after[0], after[2]]);
            assert_eq!(after[1].to_bits(), papyrus_terrain::path_terrain::sample_height(grid, *after).unwrap().to_bits());
        }
    }
    assert_eq!(output.roads[0].points()[1], output.roads[1].points()[0]);
    assert!(output.report.roadside_height_work.unwrap() > 0);
}

#[test]
fn composition_defers_materials_without_changing_road_geometry_or_final_paint() {
    let layers = layers();
    let (paint, terrain) = profiles();
    let paths = paths(&layers, &paint);
    let roads = [
        road_paint::Road { path: &paths[0], hierarchy: 1 },
        road_paint::Road { path: &paths[1], hierarchy: 1 },
    ];
    let joins = [papyrus_terrain::road_grade::Junction { a: (0, 1), b: (1, 0) }];
    let alpha = vec![255; layers.topology.len()];
    let base = papyrus_terrain::monument_composition::compose(&layers, &alpha, &[], || false)
        .unwrap()
        .unwrap();
    let run = |preview_texture| {
        let calls = Cell::new(0);
        let initial = stage::paint_for_composition(
            stage::Input {
                layers: &layers, roads: &roads, junctions: &joins, paint: &paint, terrain: &terrain,
            },
            None,
            32_000_000,
            |p| {
                calls.set(calls.get() + 1);
                Ok(Some(vec![0.; p.len()]))
            },
            |_| Ok(Some(terrain.water_no_hit_level)),
            |frame| Ok(Some(RoadsideChanges {
                height: Some(frame.layers.height.iter().map(|&height| height + 1).collect()),
                ..Default::default()
            })),
            preview_texture,
            || false,
        ).unwrap().unwrap();
        let corrected: Vec<_> = initial.roads.iter()
            .map(|path| road_paint::Road { path, hierarchy: 1 })
            .collect();
        let final_surfaces = stage::finish_surfaces_with_context(
            stage::FinalSurfaceInput {
                base: &base, height: &initial.height, roads: &corrected,
                profile: &paint, retained: None,
            },
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            || false,
        ).unwrap().unwrap();
        assert_eq!(initial.splat.is_some(), preview_texture);
        assert_eq!(initial.report.texture.is_some(), preview_texture);
        (initial, final_surfaces, calls.get())
    };
    let (before, before_final, before_calls) = run(true);
    let (after, after_final, after_calls) = run(false);
    assert_eq!(before_calls, after_calls + 1);
    assert_eq!(before.height, after.height);
    assert_eq!(before.topology, after.topology);
    for (before, after) in before.roads.iter().zip(&after.roads) {
        assert_eq!(before.points(), after.points());
    }
    assert_eq!(before_final.splat, after_final.splat);
    assert_eq!(before_final.topology, after_final.topology);
}

#[test]
fn source_authored_splat_cells_keep_exact_weights_without_normalization() {
    let mut layers = layers();
    layers.splat.fill(0);
    let cells = layers.topology.len();
    // Source material-set operations can also rewrite an empty pixel to a
    // partial total. Both untouched and rewritten bytes must remain exact.
    layers.splat[1] = 254;
    let mut splat = layers.splat.clone();
    splat[2] = 255;
    let mut changes = RoadsideChanges {
        splat: Some(splat),
        ..Default::default()
    };
    assert!(
        changes
            .validate(layers.view(), cells * 8, || false)
            .unwrap()
    );
    changes.splat.as_mut().unwrap()[3] = 254;
    assert!(
        changes
            .validate(layers.view(), cells * 8, || false)
            .unwrap()
    );
    assert_eq!(changes.splat.as_ref().unwrap()[1], 254);
    assert_eq!(changes.splat.as_ref().unwrap()[3], 254);
}

#[test]
fn final_repaint_keeps_roadside_channels_and_empty_callbacks_preserve_existing_outputs() {
    for raw in [
        include_str!("../../../../../compatibility/infrastructure/client-0f703d43-road-paint.json"),
        include_str!("../../../../../compatibility/infrastructure/client-8dd4e235-road-paint.json"),
    ] {
        let paint: crate::road_paint_source::Profile = serde_json::from_str(raw).unwrap();
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution * layers.surface_resolution];
        let base = papyrus_terrain::monument_composition::compose(&layers, &alpha, &[], || false)
            .unwrap()
            .unwrap();
        let input = || stage::FinalSurfaceInput {
            base: &base,
            height: &layers.height,
            roads: &[],
            profile: &paint,
            retained: None,
        };
        let baseline = stage::finish_surfaces_with_context(
            input(),
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            || false,
        )
        .unwrap()
        .unwrap();
        let empty = stage::finish_surfaces_with_roadside(
            input(),
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(None),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(baseline.topology, empty.topology);
        assert_eq!(baseline.splat, empty.splat);
        assert!(empty.height.is_none());
        let output = stage::finish_surfaces_with_roadside(
            input(),
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |frame| {
                let mut height = frame.layers.height.to_vec();
                height[0] += 1;
                let mut topology = frame.layers.topology.to_vec();
                topology[0] |= 512;
                let cells = topology.len();
                let mut splat = vec![0; cells * 8];
                splat[cells * 2..cells * 3].fill(255);
                Ok(Some(RoadsideChanges {
                    height: Some(height),
                    topology: Some(topology),
                    splat: Some(splat),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.height.as_ref().unwrap()[0], layers.height[0] + 1);
        assert_eq!(output.topology[0], baseline.topology[0] | 512);
        let cells = layers.surface_resolution * layers.surface_resolution;
        assert!(
            output.splat[cells * 2..cells * 3]
                .iter()
                .all(|value| *value == 255)
        );
        assert_eq!(base.layers.height, layers.height);
    }
}

#[test]
fn final_ocean_reuses_audited_height_until_roadside_changes_it() {
    use papyrus_terrain::ocean;

    let mut layers = layers();
    layers.height.fill(16383);
    let (hr, n) = (layers.height_resolution, layers.surface_resolution);
    let alpha = vec![255; n * n];
    let (paint, _) = profiles();
    let mut base = papyrus_terrain::monument_composition::compose(&layers, &alpha, &[], || false)
        .unwrap()
        .unwrap();
    let mut local_water = vec![0; hr * hr];
    local_water[0] = 123;
    base.ocean = ocean::reconstruct_with_water(
        &layers.height, hr, &layers.topology, n, Some(&local_water), || false,
    )
    .unwrap();

    for change_height in [false, true] {
        let render = |use_audit: bool| {
            let mask = use_audit
                .then(|| ocean::classify_height(&layers.height, hr, n, || false).unwrap().unwrap());
            stage::finish_surfaces_with_ocean_mask(
                stage::FinalSurfaceInput {
                    base: &base,
                    height: &layers.height,
                    roads: &[],
                    profile: &paint,
                    retained: None,
                },
                mask,
                32_000_000,
                |p| Ok(Some(vec![0.; p.len()])),
                |frame| {
                    let mut topology = frame.layers.topology.to_vec();
                    topology[0] |= 512;
                    Ok(Some(RoadsideChanges {
                        height: change_height.then(|| vec![16384; hr * hr]),
                        topology: Some(topology),
                        ..Default::default()
                    }))
                },
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let baseline = render(false);
        let reused = render(true);
        assert_eq!(reused.topology, baseline.topology);
        assert_eq!(reused.splat, baseline.splat);
        assert_eq!(reused.ocean, baseline.ocean);
        let ocean = reused.ocean.unwrap();
        assert_eq!(ocean.ocean_cells, if change_height { 0 } else { n * n });
        assert_eq!(ocean.local_water, local_water);
        assert_eq!(ocean.topology[0] & 512, 512);
    }
}

#[test]
fn final_roadside_failure_and_cancellation_never_publish_channels() {
    let layers = layers();
    let (paint, _) = profiles();
    let alpha = vec![255; layers.surface_resolution * layers.surface_resolution];
    let base = papyrus_terrain::monument_composition::compose(&layers, &alpha, &[], || false)
        .unwrap()
        .unwrap();
    for cancel in [false, true] {
        let stopped = Cell::new(false);
        let result = stage::finish_surfaces_with_roadside(
            stage::FinalSurfaceInput {
                base: &base,
                height: &layers.height,
                roads: &[],
                profile: &paint,
                retained: None,
            },
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| {
                stopped.set(cancel);
                Err("private roadside failure".into())
            },
            || stopped.get(),
        );
        if cancel {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err());
        }
        assert_eq!(base.layers.height, layers.height);
    }
}

#[test]
fn retained_rail_topology_and_earthwork_finish_before_the_roadside_callback() {
    let layers = layers();
    let (paint, terrain) = profiles();
    let paths = paths(&layers, &paint);
    let roads = [road_paint::Road {
        path: &paths[0],
        hierarchy: 1,
    }];
    let (_root, parent, source) = crate::retained_rail_height::tests::source_fixture();
    let rail_paths = [papyrus_terrain::path_geometry::Path::from_controls(
        &[[-200., 21., 300.], [0., 21., 300.], [200., 21., 300.]],
        10000,
        || false,
    )
    .unwrap()
    .unwrap()];
    let rails = crate::rail_topology_stage::prepare(
        &source,
        &parent,
        &rail_paths,
        1000,
        32_000_000,
        || false,
    )
    .unwrap()
    .unwrap();
    let input = || stage::Input {
        layers: &layers,
        roads: &roads,
        junctions: &[],
        paint: &paint,
        terrain: &terrain,
    };
    let baseline = stage::paint_with_rail_stages(
        input(),
        &rails,
        32_000_000,
        |p| Ok(Some(vec![0.; p.len()])),
        |_| Ok(Some(terrain.water_no_hit_level)),
        || false,
    )
    .unwrap()
    .unwrap();
    assert!(baseline.report.rail_topology.is_some());
    let called = Cell::new(false);
    let output = stage::paint_with_roadside(
        input(),
        Some(&rails),
        32_000_000,
        |p| Ok(Some(vec![0.; p.len()])),
        |_| Ok(Some(terrain.water_no_hit_level)),
        |frame| {
            called.set(true);
            assert_eq!(frame.layers.height, baseline.height);
            assert_eq!(frame.layers.topology, baseline.topology);
            Ok(None)
        },
        || false,
    )
    .unwrap()
    .unwrap();
    assert!(called.get());
    assert_eq!(output.height, baseline.height);
    assert_eq!(output.topology, baseline.topology);
    assert_eq!(output.splat, baseline.splat);
    assert_eq!(
        serde_json::to_value(&output.report).unwrap(),
        serde_json::to_value(&baseline.report).unwrap()
    );
}

#[test]
fn phase_observes_finished_earthwork_and_changes_feed_the_texture_pass() {
    for raw in [
        include_str!("../../../../../compatibility/infrastructure/client-0f703d43-road-paint.json"),
        include_str!("../../../../../compatibility/infrastructure/client-8dd4e235-road-paint.json"),
    ] {
        let layers = layers();
        let original = (
            layers.height.clone(),
            layers.topology.clone(),
            layers.splat.clone(),
        );
        let paint = serde_json::from_str(raw).unwrap();
        let terrain = crate::road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        let paths = paths(&layers, &paint);
        let roads = [road_paint::Road {
            path: &paths[0],
            hierarchy: 1,
        }];
        let input = || stage::Input {
            layers: &layers,
            roads: &roads,
            junctions: &[],
            paint: &paint,
            terrain: &terrain,
        };
        let baseline = stage::paint(
            input(),
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(Some(terrain.water_no_hit_level)),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_ne!(baseline.height, layers.height);
        let calls = Cell::new(0);
        let mut expected_materials = Vec::new();
        let output = stage::paint_with_roadside(
            input(),
            None,
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(Some(terrain.water_no_hit_level)),
            |frame| {
                calls.set(calls.get() + 1);
                assert_eq!(frame.layers.height, baseline.height);
                assert_eq!(frame.layers.topology, baseline.topology);
                assert_eq!(frame.layers.splat, layers.splat);
                assert_eq!(frame.roads[0].path.points(), baseline.roads[0].points());
                let mut height = frame.layers.height.to_vec();
                height[0] += 1;
                let mut topology = frame.layers.topology.to_vec();
                topology[0] |= 512;
                let cells = topology.len();
                let mut splat = vec![0; cells * 8];
                splat[cells * 2..cells * 3].fill(255);
                let prepared =
                    road_paint::prepare(frame.roads, &paint.policy, 32_000_000, || false)?.unwrap();
                expected_materials = prepared
                    .apply(
                        road_paint::Grid {
                            geometry: path_surface::Grid {
                                size: layers.size as f32,
                                resolution: layers.surface_resolution,
                                origin_y: paint.height_origin,
                                vertical_span: paint.vertical_span,
                            },
                            splat: &splat,
                            topology: &topology,
                        },
                        &vec![0.; prepared.noise_coordinates().len()],
                        road_paint::Pass::Texture,
                        || false,
                    )?
                    .unwrap()
                    .splat
                    .unwrap();
                Ok(Some(RoadsideChanges {
                    height: Some(height),
                    topology: Some(topology),
                    splat: Some(splat),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(output.height[0], baseline.height[0] + 1);
        assert_eq!(&output.height[1..], &baseline.height[1..]);
        assert_eq!(output.topology[0], baseline.topology[0] | 512);
        assert_eq!(&output.topology[1..], &baseline.topology[1..]);
        assert_eq!(output.splat, expected_materials);
        assert_ne!(output.splat, baseline.splat);
        assert_eq!(
            (&layers.height, &layers.topology, &layers.splat),
            (&original.0, &original.1, &original.2)
        );
    }
}

#[test]
fn omitted_and_empty_revisions_match_the_existing_entry_point_exactly() {
    let layers = layers();
    let (paint, terrain) = profiles();
    let paths = paths(&layers, &paint);
    let roads = [road_paint::Road {
        path: &paths[0],
        hierarchy: 1,
    }];
    let input = || stage::Input {
        layers: &layers,
        roads: &roads,
        junctions: &[],
        paint: &paint,
        terrain: &terrain,
    };
    let baseline = stage::paint(
        input(),
        32_000_000,
        |p| Ok(Some(vec![0.; p.len()])),
        |_| Ok(Some(terrain.water_no_hit_level)),
        || false,
    )
    .unwrap()
    .unwrap();
    for revision in [None, Some(RoadsideChanges::default())] {
        let output = stage::paint_with_roadside(
            input(),
            None,
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(Some(terrain.water_no_hit_level)),
            |_| Ok(revision),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.height, baseline.height);
        assert_eq!(output.topology, baseline.topology);
        assert_eq!(output.splat, baseline.splat);
        assert_eq!(output.roads[0].points(), baseline.roads[0].points());
        assert_eq!(
            serde_json::to_value(&output.report).unwrap(),
            serde_json::to_value(&baseline.report).unwrap()
        );
    }
}

#[test]
fn cancelled_or_failed_roadside_and_texture_work_never_publishes() {
    let layers = layers();
    let original = (
        layers.height.clone(),
        layers.topology.clone(),
        layers.splat.clone(),
    );
    let (paint, terrain) = profiles();
    let paths = paths(&layers, &paint);
    let roads = [road_paint::Road {
        path: &paths[0],
        hierarchy: 1,
    }];
    let input = || stage::Input {
        layers: &layers,
        roads: &roads,
        junctions: &[],
        paint: &paint,
        terrain: &terrain,
    };
    for mode in 0..5 {
        let entered = Cell::new(false);
        let cancelled = Cell::new(mode == 0);
        let result = stage::paint_with_roadside(
            input(),
            None,
            32_000_000,
            |p| {
                if mode == 3 && entered.get() {
                    Err("texture failure".into())
                } else {
                    Ok(Some(vec![0.; p.len()]))
                }
            },
            |_| Ok(Some(terrain.water_no_hit_level)),
            |frame| {
                entered.set(true);
                if mode == 4 {
                    cancelled.set(true);
                    return Err("cancelled roadside callback".into());
                }
                if mode == 1 {
                    return Err("roadside failure".into());
                }
                let mut height = frame.layers.height.to_vec();
                height[0] += 1;
                if mode == 2 {
                    cancelled.set(true);
                }
                Ok(Some(RoadsideChanges {
                    height: Some(height),
                    ..Default::default()
                }))
            },
            || cancelled.get(),
        );
        match mode {
            0 => {
                assert!(result.unwrap().is_none());
                assert!(!entered.get());
            }
            1 => assert_eq!(result.err().unwrap(), "roadside failure"),
            2 | 4 => assert!(result.unwrap().is_none()),
            _ => assert_eq!(result.err().unwrap(), "texture failure"),
        }
        assert_eq!(
            (&layers.height, &layers.topology, &layers.splat),
            (&original.0, &original.1, &original.2)
        );
    }
}

#[test]
fn revisions_require_compatible_encoded_and_bounded_rasters() {
    let layers = layers();
    for changes in [
        RoadsideChanges {
            height: Some(vec![]),
            ..Default::default()
        },
        RoadsideChanges {
            topology: Some(vec![]),
            ..Default::default()
        },
        RoadsideChanges {
            splat: Some(vec![]),
            ..Default::default()
        },
        RoadsideChanges {
            height: Some(vec![-1; layers.height.len()]),
            ..Default::default()
        },
        RoadsideChanges {
            height: Some(vec![32767; layers.height.len()]),
            ..Default::default()
        },
    ] {
        assert!(
            changes
                .validate(layers.view(), 32_000_000, || false)
                .is_err()
        );
    }
    let changes = RoadsideChanges {
        height: Some(layers.height.clone()),
        splat: Some(layers.splat.clone()),
        ..Default::default()
    };
    assert!(
        changes
            .validate(layers.view(), 32_000_000, || false)
            .unwrap()
    );
    assert!(changes.validate(layers.view(), 1, || false).is_err());
    assert!(
        !changes
            .validate(layers.view(), 32_000_000, || true)
            .unwrap()
    );
    let calls = Cell::new(0);
    assert!(
        !changes
            .validate(layers.view(), 32_000_000, || {
                calls.set(calls.get() + 1);
                calls.get() > 1
            })
            .unwrap()
    );
}
