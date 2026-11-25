//! MonumentInfo.CheckPlacement subset with explicit water-body knowledge.
use super::*;
use crate::environment::{Transform, Verdict, rotate};

pub const VERSION: &str = "papyrus-offshore-placement-gate-1";
#[derive(Clone, Copy, Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub center: [f32; 3],
    pub extent: [f32; 3],
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum WaterGate {
    /// Includes confirmed absence of a child WaterBody. Not inferred from names.
    NotLake,
    /// Source MinWaterLevel on the selected first child body, before spawning.
    Lake {
        minimum_level: f32,
    },
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum PlacementVerdict {
    /// This method only; decorators/environment/whole-world acceptance separate.
    Passed,
    TierRejected,
    LakeRejected,
    WaterUnknown,
}
#[derive(Debug, Serialize)]
pub struct PlacementAssessment {
    pub engine: &'static str,
    pub corners: [[f32; 3]; 4],
    pub tiers: Assessment,
    pub water: Option<WaterGate>,
    pub verdict: PlacementVerdict,
}
/// Water branch shared by the complete gate and a staged evaluator that has
/// already checked tiers. The supplied minimum is in the pre-spawn prefab frame.
pub fn water_verdict(position_y: f32, water: WaterGate) -> Result<PlacementVerdict, String> {
    if !position_y.is_finite() || position_y.abs() > 1_000_000. {
        return Err("Invalid offshore placement height".into());
    }
    Ok(match water {
        WaterGate::NotLake => PlacementVerdict::Passed,
        WaterGate::Unknown => PlacementVerdict::WaterUnknown,
        WaterGate::Lake { minimum_level } => {
            if !minimum_level.is_finite() || minimum_level.abs() > 1_000_000. {
                return Err("Invalid offshore lake minimum water level".into());
            }
            if position_y + minimum_level < 1. {
                PlacementVerdict::LakeRejected
            } else {
                PlacementVerdict::Passed
            }
        }
    })
}

/// Source OBB(position, scale, rotation, bounds).GetPoint(±1,0,±1), in
/// (-,-),(-,+),(+,-),(+,+) order. The authored center and positive nonuniform
/// scale are retained; these are neither root points nor projected AABB corners.
pub fn midplane_corners(bounds: Bounds, pose: Transform) -> Result<[[f32; 3]; 4], String> {
    pose.validate()?;
    if bounds
        .center
        .iter()
        .chain(&bounds.extent)
        .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
        || bounds.extent.iter().any(|v| *v < 0.)
    {
        return Err("Invalid offshore monument bounds".into());
    }
    let offset = rotate(
        pose.rotation,
        std::array::from_fn(|i| pose.scale[i] * bounds.center[i]),
    );
    let center: [f32; 3] = std::array::from_fn(|i| pose.position[i] + offset[i]);
    let extent: [f32; 3] = std::array::from_fn(|i| pose.scale[i] * bounds.extent[i]);
    let right = rotate(pose.rotation, [1., 0., 0.]);
    let up = rotate(pose.rotation, [0., 1., 0.]);
    let forward = rotate(pose.rotation, [0., 0., 1.]);
    let points: [[f32; 3]; 4] = std::array::from_fn(|index| {
        let x = if index < 2 { -1. } else { 1. };
        let z = if index % 2 == 0 { -1. } else { 1. };
        std::array::from_fn(|i| {
            ((center[i] + x * extent[0] * right[i]) + 0. * extent[1] * up[i])
                + z * extent[2] * forward[i]
        })
    });
    if points
        .iter()
        .flatten()
        .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
    {
        return Err("Unbounded offshore monument corners".into());
    }
    Ok(points)
}

/// Source-shaped placement gate. Resolve water facts only after the tier check,
/// matching the inspected method. Missing water knowledge cannot pass. The
/// resolver's own work must be bounded by its owning installation adapter.
#[allow(clippy::too_many_arguments)]
pub fn check_placement(
    rect: TerrainRect,
    topology: &[i32],
    resolution: usize,
    bounds: Bounds,
    pose: Transform,
    tier_mask: u32,
    minimum_corners: usize,
    cancelled: impl Fn() -> bool,
    water: impl FnOnce() -> Result<WaterGate, String>,
) -> Result<Result<PlacementAssessment, Verdict>, String> {
    let corners = midplane_corners(bounds, pose)?;
    let Some(tiers) = assess(
        rect,
        topology,
        resolution,
        corners,
        tier_mask,
        minimum_corners,
        &cancelled,
    )?
    else {
        return Ok(Err(Verdict::Cancelled));
    };
    if cancelled() {
        return Ok(Err(Verdict::Cancelled));
    }
    let (water, verdict) = if !tiers.accepted() {
        (None, PlacementVerdict::TierRejected)
    } else {
        let water = water()?;
        if cancelled() {
            return Ok(Err(Verdict::Cancelled));
        }
        let verdict = water_verdict(pose.position[1], water)?;
        (Some(water), verdict)
    };
    Ok(Ok(PlacementAssessment {
        engine: VERSION,
        corners,
        tiers,
        water,
        verdict,
    }))
}

#[cfg(test)]
mod tests;
