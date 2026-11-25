//! Road-span proposals from PlaceMonumentsRoadside's authored socket geometry.
//! These are candidates, not accepted placements. Terrain checks, grouped search,
//! spacing, path straightening and staged world generation remain separate.
use crate::path_geometry::Path;
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-roadside-monument-proposals-1";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadMode {
    SideRoadOrRingRoad,
    SideRoad,
    RingRoad,
    SideRoadOrDesireTrail,
    DesireTrail,
}
impl RoadMode {
    pub fn matches(self, hierarchy: u8) -> bool {
        match self {
            Self::SideRoadOrRingRoad => hierarchy <= 1,
            Self::SideRoad => hierarchy == 1,
            Self::RingRoad => hierarchy == 0,
            Self::SideRoadOrDesireTrail => hierarchy == 1 || hierarchy == 2,
            Self::DesireTrail => hierarchy == 2,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Source code constants supplied by the installation adapter (5 m in the
    /// inspected build), rather than inferred from road width or path spacing.
    pub sample_spacing: f32,
    pub end_padding: f32,
    pub maximum_work: usize,
    pub maximum_candidates: usize,
}
pub struct Road<'a> {
    pub hierarchy: u8,
    pub path: &'a Path,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Candidate {
    pub road: usize,
    pub start_index: usize,
    pub end_index: usize,
    pub sample_distance: f32,
    pub direction: i8,
    pub position: [f32; 3],
    /// Mapper's XZ yaw convention, opposite Unity's positive Y rotation.
    pub yaw: f32,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    DegenerateSockets,
    Budget,
}
#[derive(Debug, Serialize)]
pub struct Proposals {
    pub engine: &'static str,
    pub candidates: Vec<Candidate>,
    pub work: usize,
    pub degenerate_spans: usize,
}
fn visit(work: &mut usize, maximum: usize) -> Result<(), Failure> {
    if *work >= maximum {
        return Err(Failure::Budget);
    }
    *work += 1;
    Ok(())
}
/// Connector order is significant: the first two road sockets determine the
/// local axis. All road sockets contribute to the center and mean pivot radius.
/// Only unit-scale prefab-local connectors are supported by this boundary.
/// Cancellation/budget failure never returns a partial candidate collection.
pub fn generate(
    sockets: &[[f32; 3]],
    roads: &[Road<'_>],
    mode: RoadMode,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposals>, Failure> {
    if sockets.is_empty()
        || sockets.len() > 4096
        || roads.len() > 4096
        || sockets
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
        || !settings.sample_spacing.is_finite()
        || !(0.1..=100.).contains(&settings.sample_spacing)
        || !settings.end_padding.is_finite()
        || !(0. ..=1000.).contains(&settings.end_padding)
        || !(1..=32_000_000).contains(&settings.maximum_work)
        || !(1..=1_000_000).contains(&settings.maximum_candidates)
        || roads.iter().any(|r| r.hierarchy > 2)
    {
        return Err(Failure::Invalid);
    }
    if cancelled() {
        return Ok(None);
    }
    let mut work = 0;
    let mut center = [0.; 2];
    let mut radius = 0.;
    for p in sockets {
        if cancelled() {
            return Ok(None);
        }
        visit(&mut work, settings.maximum_work)?;
        center[0] += p[0];
        center[1] += p[2];
        radius += (p[0] * p[0] + p[2] * p[2]).sqrt();
    }
    if sockets.len() > 1 {
        center = center.map(|v| v / sockets.len() as f32);
        radius /= sockets.len() as f32;
    }
    let axis = if sockets.len() > 1 {
        [sockets[0][0] - sockets[1][0], sockets[0][2] - sockets[1][2]]
    } else {
        [sockets[0][0], sockets[0][2]]
    };
    // Unity's zero-forward LookRotation needs an explicit adapter. Do not
    // fabricate an orientation that can later be mistaken for a valid site.
    if (axis[0] * axis[0] + axis[1] * axis[1]).sqrt() <= 0.00001 {
        return Err(Failure::DegenerateSockets);
    }
    let socket_yaw = axis[0].atan2(axis[1]);
    let mut candidates = Vec::new();
    let mut degenerate_spans = 0;
    for (road, input) in roads.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        visit(&mut work, settings.maximum_work)?;
        if !mode.matches(input.hierarchy) {
            continue;
        }
        let path = input.path;
        if path.length() == 0. {
            continue;
        }
        let points = path.points();
        let (min, max) = path.range();
        // Preserve source multiplication/division order, including trimmed paths.
        let start_offset = path.length() * min as f32 / (points.len() - 1) as f32;
        let end_offset =
            path.length() * (points.len() - 1 - max) as f32 / (points.len() - 1) as f32;
        let mut distance = start_offset + settings.end_padding + radius;
        let end = path.length() - end_offset - settings.end_padding - radius;
        while distance <= end {
            if cancelled() {
                return Ok(None);
            }
            visit(&mut work, settings.maximum_work)?;
            // Source GetPrevIndex/GetNextIndex use uniform index distance, not
            // arc length along unequal control segments or Hermite interpolation.
            let start_index =
                ((distance - radius) / path.length() * (points.len() - 1) as f32).floor() as usize;
            let end_index =
                ((distance + radius) / path.length() * (points.len() - 1) as f32).ceil() as usize;
            if start_index >= points.len() || end_index >= points.len() {
                return Err(Failure::Invalid);
            }
            let a = points[start_index.clamp(min, max)];
            let b = points[end_index.clamp(min, max)];
            let dx = b[0] - a[0];
            let dz = b[2] - a[2];
            if (dx * dx + dz * dz).sqrt() <= 0.00001 {
                degenerate_spans += 1;
            } else {
                for direction in [-1_i8, 1] {
                    visit(&mut work, settings.maximum_work)?;
                    if candidates.len() >= settings.maximum_candidates {
                        return Err(Failure::Budget);
                    }
                    let radians =
                        -((direction as f32 * dx).atan2(direction as f32 * dz) + socket_yaw);
                    let (sin, cos) = radians.sin_cos();
                    candidates.push(Candidate {
                        road,
                        start_index,
                        end_index,
                        sample_distance: distance,
                        direction,
                        position: [
                            (a[0] + b[0]) * 0.5 - (cos * center[0] - sin * center[1]),
                            (a[1] + b[1]) * 0.5,
                            (a[2] + b[2]) * 0.5 - (sin * center[0] + cos * center[1]),
                        ],
                        yaw: radians.to_degrees(),
                    });
                }
            }
            let next = distance + settings.sample_spacing;
            if next <= distance {
                return Err(Failure::Invalid);
            }
            distance = next;
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Proposals {
        engine: VERSION,
        candidates,
        work,
        degenerate_spans,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            sample_spacing: 5.,
            end_padding: 5.,
            maximum_work: 100_000,
            maximum_candidates: 10_000,
        }
    }
    fn path(points: &[[f32; 3]]) -> Path {
        Path::from_controls(points, 100_000, || false)
            .unwrap()
            .unwrap()
    }
    #[derive(Deserialize)]
    struct Fixture {
        reference: String,
        assembly_sha256: String,
        cases: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Case {
        input: Input,
        expected: Expected,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Input {
        name: String,
        points: Vec<[f32; 3]>,
        sockets: Vec<[f32; 3]>,
        min: usize,
        max: usize,
        spacing: f32,
        padding: f32,
    }
    #[derive(Deserialize)]
    struct Expected {
        length: f32,
        radius: f32,
        start_offset: f32,
        end_offset: f32,
        spans: Vec<Span>,
    }
    #[derive(Deserialize)]
    struct Span {
        distance: f32,
        start_index: usize,
        end_index: usize,
        a: [f32; 3],
        b: [f32; 3],
    }
    #[test]
    fn installed_server_span_indices_offsets_endpoints_and_sampling_match() {
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../tests/fixtures/roadside-spans-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            fixture.reference,
            "installed-server-PathInterpolator-roadside-spans"
        );
        assert_eq!(
            fixture.assembly_sha256,
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(fixture.cases.len(), 10);
        for case in fixture.cases {
            let input = case.input;
            let mut path = path(&input.points);
            path.set_range(input.min, input.max).unwrap();
            assert_eq!(path.length(), case.expected.length, "{}", input.name);
            let n = (path.points().len() - 1) as f32;
            assert_eq!(
                path.length() * input.min as f32 / n,
                case.expected.start_offset
            );
            assert_eq!(
                path.length() * (path.points().len() - 1 - input.max) as f32 / n,
                case.expected.end_offset
            );
            let radius = input
                .sockets
                .iter()
                .map(|p| (p[0] * p[0] + p[2] * p[2]).sqrt())
                .sum::<f32>()
                / input.sockets.len() as f32;
            assert_eq!(radius, case.expected.radius);
            let report = generate(
                &input.sockets,
                &[Road {
                    hierarchy: 0,
                    path: &path,
                }],
                RoadMode::RingRoad,
                Settings {
                    sample_spacing: input.spacing,
                    end_padding: input.padding,
                    ..settings()
                },
                || false,
            )
            .unwrap()
            .unwrap();
            let spans: Vec<_> = case
                .expected
                .spans
                .iter()
                .filter(|s| {
                    ((s.b[0] - s.a[0]).powi(2) + (s.b[2] - s.a[2]).powi(2)).sqrt() > 0.00001
                })
                .collect();
            assert_eq!(report.candidates.len(), spans.len() * 2, "{}", input.name);
            assert_eq!(
                report.degenerate_spans,
                case.expected.spans.len() - spans.len()
            );
            let center = [0, 2].map(|axis| {
                input.sockets.iter().map(|s| s[axis]).sum::<f32>() / input.sockets.len() as f32
            });
            for (pair, span) in report.candidates.chunks_exact(2).zip(spans) {
                for (candidate, direction) in pair.iter().zip([-1, 1]) {
                    assert_eq!(candidate.direction, direction);
                    assert_eq!(candidate.start_index, span.start_index, "{}", input.name);
                    assert_eq!(candidate.end_index, span.end_index, "{}", input.name);
                    assert_eq!(candidate.sample_distance, span.distance);
                    // The transformed authored socket center lies at the actual
                    // installed method's control midpoint, for both road sides.
                    let (sin, cos) = candidate.yaw.to_radians().sin_cos();
                    let predicted = [
                        candidate.position[0] + cos * center[0] - sin * center[1],
                        candidate.position[1],
                        candidate.position[2] + sin * center[0] + cos * center[1],
                    ];
                    for (axis, value) in predicted.into_iter().enumerate() {
                        assert!(
                            (value - (span.a[axis] + span.b[axis]) * 0.5).abs() < 0.001,
                            "{} axis {axis}",
                            input.name
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn authored_socket_offset_places_opposite_sides_without_height_or_width_guessing() {
        let path = path(&[[0., 10., 0.], [0., 30., 200.]]);
        let sockets = [[-10., 400., 20.], [10., -500., 20.]];
        let report = generate(
            &sockets,
            &[Road {
                hierarchy: 1,
                path: &path,
            }],
            RoadMode::SideRoad,
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        let pair = &report.candidates[..2];
        assert!((pair[0].position[0] + 20.).abs() < 0.0001);
        assert!((pair[1].position[0] - 20.).abs() < 0.0001);
        assert!((pair[0].yaw.rem_euclid(360.) - 270.).abs() < 0.0001);
        assert!((pair[1].yaw.rem_euclid(360.) - 90.).abs() < 0.0001);
        assert_eq!(pair[0].position[1], 20.);
        assert_eq!(pair[1].position[1], 20.);
    }
    #[test]
    fn all_source_road_modes_choose_only_their_authored_hierarchies() {
        let path = path(&[[0., 0., 0.], [0., 0., 100.]]);
        let roads = [0, 1, 2].map(|hierarchy| Road {
            hierarchy,
            path: &path,
        });
        for (mode, expected) in [
            (RoadMode::SideRoadOrRingRoad, vec![0, 1]),
            (RoadMode::SideRoad, vec![1]),
            (RoadMode::RingRoad, vec![0]),
            (RoadMode::SideRoadOrDesireTrail, vec![1, 2]),
            (RoadMode::DesireTrail, vec![2]),
        ] {
            let report = generate(
                &[[-5., 0., 10.], [5., 0., 10.]],
                &roads,
                mode,
                settings(),
                || false,
            )
            .unwrap()
            .unwrap();
            let actual: std::collections::BTreeSet<_> =
                report.candidates.iter().map(|c| c.road).collect();
            assert_eq!(actual, expected.into_iter().collect());
        }
    }
    #[test]
    fn cancellation_limits_bad_sockets_and_degenerate_paths_never_publish_partial_candidates() {
        let path = path(&[[0., 0., 0.], [10., 1., 30.], [0., 0., 100.]]);
        let before = serde_json::to_vec(&path).unwrap();
        let sockets = [[-5., 0., 10.], [5., 0., 10.]];
        let roads = [Road {
            hierarchy: 0,
            path: &path,
        }];
        assert!(
            generate(
                &sockets,
                &roads,
                RoadMode::RingRoad,
                Settings {
                    maximum_work: 2,
                    ..settings()
                },
                || false
            )
            .is_err_and(|e| e == Failure::Budget)
        );
        assert!(
            generate(
                &sockets,
                &roads,
                RoadMode::RingRoad,
                Settings {
                    maximum_candidates: 1,
                    ..settings()
                },
                || false
            )
            .is_err_and(|e| e == Failure::Budget)
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            generate(&sockets, &roads, RoadMode::RingRoad, settings(), || {
                calls.set(calls.get() + 1);
                calls.get() > 5
            })
            .unwrap()
            .is_none()
        );
        assert!(
            generate(&[[0.; 3]], &roads, RoadMode::RingRoad, settings(), || false)
                .is_err_and(|e| e == Failure::DegenerateSockets)
        );
        assert!(
            generate(
                &[[f32::NAN, 0., 0.]],
                &roads,
                RoadMode::RingRoad,
                settings(),
                || false
            )
            .is_err_and(|e| e == Failure::Invalid)
        );
        assert_eq!(before, serde_json::to_vec(&path).unwrap());
    }
}
