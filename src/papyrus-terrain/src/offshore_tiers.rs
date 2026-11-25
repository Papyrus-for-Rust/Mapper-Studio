//! The offshore CheckPlacement tier subset. This does not certify environment
//! volumes, decor, water bodies, occupancy, or an assembled monument.
use crate::offshore_search::TerrainRect;
use serde::Serialize;
mod placement;
pub use placement::{
    Bounds, PlacementAssessment, PlacementVerdict, VERSION as PLACEMENT_VERSION, WaterGate,
    check_placement, midplane_corners, water_verdict,
};

#[derive(Debug, PartialEq, Serialize)]
pub struct Assessment {
    pub cells: [[usize; 2]; 4],
    pub matching: usize,
    pub required: usize,
}
impl Assessment {
    pub fn accepted(&self) -> bool {
        self.matching >= self.required
    }
}
fn index(normalized: f32, resolution: usize) -> usize {
    (normalized * resolution as f32).clamp(0., (resolution - 1) as f32) as usize
}

/// Caller supplies the four source OBB midplane corners after decor. Unlike the
/// land search, the source offshore stage samples outside coordinates by clamping
/// each topology texel. Normalization retains the source subtraction/multiply
/// order. Existing land proposal bounds are deliberately unaffected.
pub fn assess(
    rect: TerrainRect,
    topology: &[i32],
    resolution: usize,
    corners: [[f32; 3]; 4],
    tier_mask: u32,
    minimum_corners: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Assessment>, String> {
    if !(1000..=6000).contains(&rect.size)
        || rect
            .minimum
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 100_000.)
        || !(1..=4096).contains(&resolution)
        || resolution.checked_mul(resolution) != Some(topology.len())
        || !(1..=4).contains(&minimum_corners)
        || corners
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
    {
        return Err("Invalid offshore tier query".into());
    }
    let inverse = 1. / rect.size as f32;
    let mut result = Assessment {
        cells: [[0; 2]; 4],
        matching: 0,
        required: minimum_corners,
    };
    for (i, point) in corners.into_iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let x = index((point[0] - rect.minimum[0]) * inverse, resolution);
        let z = index((point[2] - rect.minimum[1]) * inverse, resolution);
        result.cells[i] = [x, z];
        result.matching += usize::from(topology[z * resolution + x] as u32 & tier_mask != 0);
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outside_and_boundary_indices_match_owned_terrain_map() {
        let document: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/terrain-cell-reference.json"
        ))
        .unwrap();
        assert_eq!(document["format"], "papyrus-owned-terrain-cell-reference");
        let rows = document["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 55);
        for row in rows {
            assert_eq!(
                index(
                    row["normalized"].as_f64().unwrap() as f32,
                    row["resolution"].as_u64().unwrap() as usize
                ),
                row["cell"].as_u64().unwrap() as usize
            );
        }
    }
    #[test]
    fn offshore_corners_clamp_to_each_edge_and_use_captured_masks() {
        let rect = TerrainRect {
            size: 1000,
            minimum: [-500.; 2],
        };
        let corners = [
            [-800., 0., -800.],
            [800., 0., -800.],
            [-800., 0., 800.],
            [800., 0., 800.],
        ];
        let result = assess(rect, &[4, 4, 0, 4], 2, corners, 4, 3, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.cells, [[0, 0], [1, 0], [0, 1], [1, 1]]);
        assert_eq!(result.matching, 3);
        assert!(result.accepted());
        assert!(
            !assess(rect, &[4, 4, 0, 4], 2, corners, 8, 3, || false)
                .unwrap()
                .unwrap()
                .accepted()
        );
        assert!(
            assess(rect, &[4; 4], 2, corners, 4, 3, || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn malformed_grids_and_nonfinite_corners_cannot_pass() {
        let rect = TerrainRect {
            size: 1000,
            minimum: [-500.; 2],
        };
        assert!(assess(rect, &[], 0, [[0.; 3]; 4], 4, 3, || false).is_err());
        assert!(assess(rect, &[4; 3], 2, [[0.; 3]; 4], 4, 3, || false).is_err());
        assert!(assess(rect, &[4; 4], 2, [[f32::NAN; 3]; 4], 4, 3, || false).is_err());
    }
}
