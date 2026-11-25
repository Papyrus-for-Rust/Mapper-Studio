//! Bounded roadside decoration sampling with live acceptance feedback.
//! The installation adapter supplies spacing policy and fully prepared paths.
//! Callers perform live terrain reads, fitting and private transactional writes.
use crate::{decor_stream::Budget, path_geometry::Path, roadside_monuments::RoadMode, water};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-roadside-decoration-sampling-1";

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub road_mode: u8,
    pub distance: f32,
    /// Reviewed source-code policy, separate from the scene's Distance field.
    pub spacing_fraction: f32,
    pub side_offset: f32,
    pub dithering: f32,
    pub placement_attempts: u16,
}

impl Settings {
    pub fn validate(self) -> Result<(), Failure> {
        if self.road_mode > 5
            || !self.distance.is_finite()
            || !(0.001..=100_000.).contains(&self.distance)
            || !self.spacing_fraction.is_finite()
            || self.spacing_fraction <= 0.
            || self.spacing_fraction > 1.
            || !self.side_offset.is_finite()
            || self.side_offset.abs() > 10_000.
            || !self.dithering.is_finite()
            || !(0. ..=10_000.).contains(&self.dithering)
            || self.placement_attempts > 1024
            || self.distance * self.spacing_fraction == 0.
        {
            return Err(Failure::Invalid(
                "Invalid roadside decoration sampling settings",
            ));
        }
        Ok(())
    }

    fn matches(self, hierarchy: u8) -> bool {
        const MODES: [RoadMode; 5] = [
            RoadMode::SideRoadOrRingRoad,
            RoadMode::SideRoad,
            RoadMode::RingRoad,
            RoadMode::SideRoadOrDesireTrail,
            RoadMode::DesireTrail,
        ];
        if self.road_mode == 5 {
            hierarchy <= 2
        } else {
            MODES[usize::from(self.road_mode)].matches(hierarchy)
        }
    }
}

pub struct Road<'a> {
    pub hierarchy: u8,
    pub width: f32,
    pub spline: bool,
    pub path: &'a Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Candidate {
    pub road: usize,
    pub station: usize,
    pub distance: f32,
    pub prefab: usize,
    pub side: i8,
    pub attempt: u16,
    pub position_xz: [f32; 2],
    pub random_state: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub roads: usize,
    pub stations: usize,
    pub attempts: usize,
    pub evaluated: usize,
    pub accepted: usize,
    pub work: usize,
    pub final_seed: u32,
}

#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid(&'static str),
    Budget,
    Path,
    Evaluation(String),
}

fn visit(budget: &mut Budget) -> Result<(), Failure> {
    budget.take().map_err(|_| Failure::Budget)
}

fn jitter(bits: u32, extent: f32) -> f32 {
    let minimum = -extent;
    let unit = bits as f32 * (1. / 4_294_967_296.);
    minimum + unit * (extent - minimum)
}

/// Each returned candidate needs a live height query before fitting. A true
/// callback result suppresses further evaluations at that station, while all
/// remaining retry slots still advance the random stream. Errors/cancellation
/// return no completed report. The caller must discard private callback writes.
pub fn sample(
    roads: &[Road<'_>],
    settings: Settings,
    prefab_count: usize,
    seed: u32,
    budget: &mut Budget,
    mut accept: impl FnMut(Candidate) -> Result<bool, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    settings.validate()?;
    if roads.len() > 65536 || !(1..=4096).contains(&prefab_count) {
        return Err(Failure::Invalid(
            "Invalid roadside decoration road or prefab roster",
        ));
    }
    let before = budget.used();
    // Validate the complete roster before any callback can change private terrain.
    for road in roads {
        if cancelled() {
            return Ok(None);
        }
        visit(budget)?;
        if !road.width.is_finite() || !(0. ..=10_000.).contains(&road.width) {
            return Err(Failure::Invalid("Invalid roadside decoration road width"));
        }
    }
    let mut report = Report::default();
    let mut random = seed;
    let spacing = settings.distance * settings.spacing_fraction;
    for (index, road) in roads.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        if !settings.matches(road.hierarchy) {
            continue;
        }
        report.roads += 1;
        let (start, end) = road.path.offsets();
        let mut distance = start + spacing;
        let limit = road.path.length() - end - spacing;
        let mut station = 0;
        while distance <= limit {
            if cancelled() {
                return Ok(None);
            }
            visit(budget)?;
            let position = if road.spline {
                road.path.point(distance)
            } else {
                road.path.point_linear(distance)
            }
            .map_err(|_| Failure::Path)?;
            let tangent = road.path.tangent(distance).map_err(|_| Failure::Path)?;
            let length = (tangent[0] * tangent[0] + tangent[2] * tangent[2]).sqrt();
            let forward = if length > 0.00001 {
                [tangent[0] / length, tangent[2] / length]
            } else {
                [0.; 2]
            };
            // Unity's positive Y quarter-turn maps +Z onto +X.
            let right = [forward[1], -forward[0]];
            let side_seed = water::next(&mut random);
            let prefab = water::next(&mut random) as usize % prefab_count;
            let mut accepted = false;
            for side_index in 0..2_u32 {
                let side = if side_seed.wrapping_add(side_index) & 1 == 0 {
                    -1
                } else {
                    1
                };
                for attempt in 0..settings.placement_attempts {
                    if cancelled() {
                        return Ok(None);
                    }
                    visit(budget)?;
                    let longitudinal_bits = water::next(&mut random);
                    let lateral_bits = water::next(&mut random);
                    report.attempts += 1;
                    if accepted {
                        continue;
                    }
                    let along = jitter(longitudinal_bits, settings.dithering);
                    let outward = (road.width * 0.5
                        + settings.side_offset
                        + jitter(lateral_bits, settings.dithering).abs())
                        * f32::from(side);
                    let candidate = Candidate {
                        road: index,
                        station,
                        distance,
                        prefab,
                        side,
                        attempt,
                        position_xz: [
                            position[0] + along * forward[0] + outward * right[0],
                            position[2] + along * forward[1] + outward * right[1],
                        ],
                        random_state: random,
                    };
                    report.evaluated += 1;
                    accepted = accept(candidate).map_err(Failure::Evaluation)?;
                    report.accepted += usize::from(accepted);
                }
            }
            report.stations += 1;
            station += 1;
            let next = distance + spacing;
            if next <= distance {
                return Err(Failure::Invalid(
                    "Roadside decoration spacing cannot advance",
                ));
            }
            distance = next;
        }
    }
    if cancelled() {
        return Ok(None);
    }
    report.work = budget.used() - before;
    report.final_seed = random;
    Ok(Some(report))
}
