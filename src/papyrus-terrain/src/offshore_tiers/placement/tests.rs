use super::*;
use std::cell::Cell;
const RECT: TerrainRect = TerrainRect {
    size: 4000,
    minimum: [-2000.; 2],
};
const BOUNDS: Bounds = Bounds {
    center: [0.; 3],
    extent: [10., 20., 10.],
};
fn pose(y: f32) -> Transform {
    Transform {
        position: [0., y, 0.],
        rotation: [0., 0., 0., 1.],
        scale: [1.; 3],
    }
}
#[test]
fn corners_match_owned_obb_constructor_and_midplane_point_queries() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Input {
        center: [f32; 3],
        extent: [f32; 3],
        position: [f32; 3],
        rotation: [f32; 4],
        scale: [f32; 3],
    }
    #[derive(serde::Deserialize)]
    struct Row {
        input: Input,
        corners: [[f32; 3]; 4],
    }
    let document: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/offshore-bounds-reference.json"
    ))
    .unwrap();
    assert_eq!(
        document["format"],
        "papyrus-owned-offshore-bounds-reference"
    );
    assert_eq!(document["gameProcessStarted"], false);
    assert_eq!(document["nativeLibraryLoaded"], false);
    assert_eq!(document["physicsExecuted"], false);
    assert_eq!(
        document["sources"]["Assembly-CSharp.dll"],
        "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
    );
    let rows: Vec<Row> = serde_json::from_value(document["rows"].clone()).unwrap();
    assert_eq!(rows.len(), 14);
    let mut maximum = 0_f32;
    for row in rows {
        let input = row.input;
        let actual = midplane_corners(
            Bounds {
                center: input.center,
                extent: input.extent,
            },
            Transform {
                position: input.position,
                rotation: input.rotation,
                scale: input.scale,
            },
        )
        .unwrap();
        for (a, b) in actual.iter().flatten().zip(row.corners.iter().flatten()) {
            maximum = maximum.max((a - b).abs());
        }
        assert_eq!(actual, row.corners);
        for resolution in [1, 2, 256, 1024] {
            let topology = vec![4; resolution * resolution];
            let actual = assess(RECT, &topology, resolution, actual, 4, 3, || false)
                .unwrap()
                .unwrap();
            let expected = assess(RECT, &topology, resolution, row.corners, 4, 3, || false)
                .unwrap()
                .unwrap();
            assert_eq!(actual.cells, expected.cells);
        }
    }
    eprintln!("Owned OBB maximum corner difference: {maximum}");
}
#[test]
fn tier_failure_short_circuits_water_and_unknown_water_cannot_pass() {
    let rejected = check_placement(
        RECT,
        &[0; 4],
        2,
        BOUNDS,
        pose(10.),
        4,
        3,
        || false,
        || panic!("tier failure resolved water"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(rejected.verdict, PlacementVerdict::TierRejected);
    assert!(rejected.water.is_none());
    let unknown = check_placement(
        RECT,
        &[4; 4],
        2,
        BOUNDS,
        pose(10.),
        4,
        3,
        || false,
        || Ok(WaterGate::Unknown),
    )
    .unwrap()
    .unwrap();
    assert_eq!(unknown.verdict, PlacementVerdict::WaterUnknown);
    let known = check_placement(
        RECT,
        &[4; 4],
        2,
        BOUNDS,
        pose(-80.),
        4,
        3,
        || false,
        || Ok(WaterGate::NotLake),
    )
    .unwrap()
    .unwrap();
    assert_eq!(known.verdict, PlacementVerdict::Passed);
}
#[test]
fn lake_gate_adds_source_minimum_level_and_accepts_exact_threshold() {
    for (height, minimum, verdict) in [
        (10., -9.001, PlacementVerdict::LakeRejected),
        (10., -9., PlacementVerdict::Passed),
        (-10., 11., PlacementVerdict::Passed),
        (-10., 10.999, PlacementVerdict::LakeRejected),
    ] {
        let result = check_placement(
            RECT,
            &[4; 4],
            2,
            BOUNDS,
            pose(height),
            4,
            3,
            || false,
            || {
                Ok(WaterGate::Lake {
                    minimum_level: minimum,
                })
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.verdict, verdict);
    }
}
#[test]
fn cancellation_and_missing_water_propagate_without_a_placement() {
    assert!(matches!(
        check_placement(
            RECT,
            &[4; 4],
            2,
            BOUNDS,
            pose(10.),
            4,
            3,
            || true,
            || panic!()
        )
        .unwrap(),
        Err(Verdict::Cancelled)
    ));
    let cancelled = Cell::new(false);
    assert!(matches!(
        check_placement(
            RECT,
            &[4; 4],
            2,
            BOUNDS,
            pose(10.),
            4,
            3,
            || cancelled.get(),
            || {
                cancelled.set(true);
                Ok(WaterGate::NotLake)
            }
        )
        .unwrap(),
        Err(Verdict::Cancelled)
    ));
    assert_eq!(
        check_placement(
            RECT,
            &[4; 4],
            2,
            BOUNDS,
            pose(10.),
            4,
            3,
            || false,
            || Err("water facts unavailable".into())
        )
        .unwrap_err(),
        "water facts unavailable"
    );
    assert!(
        check_placement(
            RECT,
            &[4; 4],
            2,
            BOUNDS,
            pose(10.),
            4,
            3,
            || false,
            || Ok(WaterGate::Lake {
                minimum_level: f32::NAN
            })
        )
        .is_err()
    );
}
#[test]
fn malformed_geometry_is_an_error_and_degenerate_bounds_keep_four_corners() {
    let mut bounds = BOUNDS;
    bounds.extent[1] = -1.;
    assert!(midplane_corners(bounds, pose(0.)).is_err());
    bounds = BOUNDS;
    bounds.center[0] = f32::NAN;
    assert!(midplane_corners(bounds, pose(0.)).is_err());
    let mut invalid = pose(0.);
    invalid.scale[0] = -1.;
    assert!(midplane_corners(BOUNDS, invalid).is_err());
    let points = midplane_corners(
        Bounds {
            center: [2., 3., 4.],
            extent: [0.; 3],
        },
        pose(10.),
    )
    .unwrap();
    assert_eq!(points, [[2., 13., 4.]; 4]);
}
