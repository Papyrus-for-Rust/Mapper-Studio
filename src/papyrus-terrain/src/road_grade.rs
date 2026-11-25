//! Independent road control-height proposals. Applying these to terrain requires
//! a separate corridor/earthwork/access audit; a feasible proposal is not a map.
use crate::{
    path_geometry::Path,
    rail_grade_fit::{self, Edge, Failure, Node},
};
use serde::Serialize;

pub const VERSION: &str = "papyrus-road-grade-proposal-1";
pub const COUPLED_VERSION: &str = "papyrus-road-grade-proposal-2";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JunctionMode {
    Pinned,
    CoupledNetwork,
}
pub struct Road<'a> {
    pub path: &'a Path,
    /// Ground before the road terrain stage, sampled at the same controls.
    pub ground: &'a [f32],
    /// Protected monument/rail controls must retain their current height.
    pub fixed: &'a [bool],
}
#[derive(Clone, Copy)]
pub struct Junction {
    pub a: (usize, usize),
    pub b: (usize, usize),
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub controls: usize,
    pub fixed_controls: usize,
    pub junctions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coupled_network_endpoints: Option<usize>,
    pub changed_controls: usize,
    pub maximum_grade_before: f64,
    pub maximum_grade_after: f64,
    pub maximum_control_earthwork: f64,
    pub height_reserve: f32,
    pub work: usize,
    pub terrain_applied: bool,
    pub runtime_access_validated: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    FixedEarthwork,
    CoincidentControls,
    IntervalEnvelope,
    RoundedEarthwork,
    RoundedGrade,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ControlConflict {
    pub kind: ConflictKind,
    pub road: usize,
    pub point: usize,
    pub position: [f32; 3],
    pub required: f64,
    pub available: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope: Option<rail_grade_fit::EnvelopePrefix>,
}
pub struct Proposal {
    pub roads: Vec<Path>,
    pub report: Report,
}
fn run(a: [f32; 3], b: [f32; 3]) -> f64 {
    (f64::from(a[0]) - f64::from(b[0])).hypot(f64::from(a[2]) - f64::from(b[2]))
}
/// Open endpoints and explicit network junctions remain fixed. Unrelated XZ
/// crossings are never welded. Closed paths retain their duplicate endpoint.
pub fn propose(
    roads: &[Road<'_>],
    junctions: &[Junction],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Proposal, Failure> {
    propose_with_reserve(
        roads,
        junctions,
        maximum_grade,
        maximum_cut_fill,
        0.,
        maximum_work,
        cancelled,
    )
}
/// Reserve vertical headroom on adjustable edges for later terrain quantization.
/// This tightens the requested grade; it never relaxes it. Fixed-to-fixed edges
/// are checked against the original limit and are not needlessly moved.
pub fn propose_with_reserve(
    roads: &[Road<'_>],
    junctions: &[Junction],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    height_reserve: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Proposal, Failure> {
    propose_with_junction_policy(
        roads,
        junctions,
        maximum_grade,
        maximum_cut_fill,
        height_reserve,
        JunctionMode::Pinned,
        maximum_work,
        cancelled,
    )
}
/// Coupled mode may move network-owned branch endpoints (`Junction.a`) and
/// unprotected parent interiors together. Other open endpoints and fixed flags
/// retain their authored heights; the junction coordinates remain equal.
#[allow(clippy::too_many_arguments)]
pub fn propose_with_junction_policy(
    roads: &[Road<'_>],
    junctions: &[Junction],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    height_reserve: f32,
    mode: JunctionMode,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Proposal, Failure> {
    propose_diagnosed(
        roads,
        junctions,
        maximum_grade,
        maximum_cut_fill,
        height_reserve,
        mode,
        maximum_work,
        cancelled,
    )
    .0
}
#[allow(clippy::too_many_arguments)]
pub fn propose_diagnosed(
    roads: &[Road<'_>],
    junctions: &[Junction],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    height_reserve: f32,
    mode: JunctionMode,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> (Result<Proposal, Failure>, Option<ControlConflict>) {
    let mut conflict = None;
    let result = propose_inner(
        roads,
        junctions,
        maximum_grade,
        maximum_cut_fill,
        height_reserve,
        mode,
        maximum_work,
        cancelled,
        &mut conflict,
    );
    (result, conflict)
}
#[allow(clippy::too_many_arguments)]
fn propose_inner(
    roads: &[Road<'_>],
    junctions: &[Junction],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    height_reserve: f32,
    mode: JunctionMode,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
    conflict: &mut Option<ControlConflict>,
) -> Result<Proposal, Failure> {
    let count = roads.iter().map(|r| r.path.points().len()).sum::<usize>();
    if roads.len() > 4096
        || count > 131072
        || junctions.len() > 65536
        || !(1..=32_000_000).contains(&maximum_work)
        || !maximum_grade.is_finite()
        || !(0.001..=1.).contains(&maximum_grade)
        || !maximum_cut_fill.is_finite()
        || !(0. ..=64.).contains(&maximum_cut_fill)
        || !height_reserve.is_finite()
        || !(0. ..=8.).contains(&height_reserve)
    {
        return Err(Failure::Invalid);
    }
    let mut work = 0;
    let mut visit = || {
        if cancelled() {
            return Err(Failure::Cancelled);
        }
        if work >= maximum_work {
            return Err(Failure::Budget);
        }
        work += 1;
        Ok(())
    };
    if roads.is_empty() {
        visit()?;
    }
    let coupled = mode == JunctionMode::CoupledNetwork;
    let mut network_ends = std::collections::BTreeSet::new();
    if coupled {
        for junction in junctions {
            visit()?;
            if roads
                .get(junction.a.0)
                .is_none_or(|r| junction.a.1 >= r.path.points().len())
            {
                return Err(Failure::Invalid);
            }
            network_ends.insert(junction.a);
        }
    }
    let mut nodes = Vec::with_capacity(count);
    let mut edges = Vec::with_capacity(count + junctions.len());
    let mut offsets = Vec::with_capacity(roads.len());
    let mut maximum_grade_before = 0_f64;
    let allowance = f64::from(maximum_cut_fill);
    for (road_index, road) in roads.iter().enumerate() {
        let points = road.path.points();
        if road.ground.len() != points.len()
            || road.fixed.len() != points.len()
            || road.path.range() != (0, points.len() - 1)
            || road
                .ground
                .iter()
                .any(|h| !h.is_finite() || h.abs() > 10000.)
            || (road.path.is_circular() && points.first() != points.last())
        {
            return Err(Failure::Invalid);
        }
        let start = nodes.len();
        offsets.push(start);
        for (i, p) in points.iter().enumerate() {
            visit()?;
            let desired = f64::from(p[1]);
            let ground = f64::from(road.ground[i]);
            let fixed = road.fixed[i]
                || (!road.path.is_circular()
                    && (i == 0 || i + 1 == points.len())
                    && !network_ends.contains(&(road_index, i)));
            if fixed && (desired - ground).abs() > allowance {
                *conflict = Some(ControlConflict {
                    kind: ConflictKind::FixedEarthwork,
                    road: road_index,
                    point: i,
                    position: *p,
                    required: (desired - ground).abs(),
                    available: allowance,
                    envelope: None,
                });
                return Err(Failure::Infeasible);
            }
            nodes.push(Node {
                desired,
                minimum: if fixed {
                    desired
                } else {
                    (ground - allowance).max(-10000.)
                },
                maximum: if fixed {
                    desired
                } else {
                    (ground + allowance).min(10000.)
                },
            });
            if i > 0 {
                let length = run(points[i - 1], *p);
                let difference = (desired - f64::from(points[i - 1][1])).abs();
                // Retained joins can contain almost coincident controls. Keep
                // both indices and constrain their heights to equality.
                if length < 1e-4 && difference != 0. {
                    *conflict = Some(ControlConflict {
                        kind: ConflictKind::CoincidentControls,
                        road: road_index,
                        point: i,
                        position: *p,
                        required: difference,
                        available: 0.,
                        envelope: None,
                    });
                    return Err(Failure::Infeasible);
                }
                if length >= 1e-4 {
                    maximum_grade_before = maximum_grade_before.max(difference / length);
                }
                edges.push(Edge {
                    a: start + i - 1,
                    b: start + i,
                    maximum_difference: if length < 1e-4 {
                        0.
                    } else {
                        length * f64::from(maximum_grade) * 0.999
                    },
                });
            }
        }
        if road.path.is_circular() {
            edges.push(Edge {
                a: start,
                b: nodes.len() - 1,
                maximum_difference: 0.,
            });
        }
    }
    for junction in junctions {
        visit()?;
        let node = |(r, p): (usize, usize)| -> Result<usize, Failure> {
            if r >= roads.len() || p >= roads[r].path.points().len() {
                return Err(Failure::Invalid);
            }
            Ok(offsets[r] + p)
        };
        let a = node(junction.a)?;
        let b = node(junction.b)?;
        let pa = roads[junction.a.0].path.points()[junction.a.1];
        let pb = roads[junction.b.0].path.points()[junction.b.1];
        if pa.iter().zip(pb).any(|(x, y)| x.to_bits() != y.to_bits()) {
            return Err(Failure::Invalid);
        }
        for i in [a, b].into_iter().filter(|_| !coupled) {
            if nodes[i].desired < nodes[i].minimum || nodes[i].desired > nodes[i].maximum {
                return Err(Failure::Infeasible);
            }
            nodes[i].minimum = nodes[i].desired;
            nodes[i].maximum = nodes[i].desired;
        }
        edges.push(Edge {
            a,
            b,
            maximum_difference: 0.,
        });
    }
    let fixed_controls = nodes.iter().filter(|n| n.minimum == n.maximum).count();
    for edge in &mut edges {
        if edge.maximum_difference > 0.
            && (nodes[edge.a].minimum != nodes[edge.a].maximum
                || nodes[edge.b].minimum != nodes[edge.b].maximum)
        {
            edge.maximum_difference = (edge.maximum_difference - f64::from(height_reserve)).max(0.);
        }
    }
    let remaining = maximum_work
        .checked_sub(work)
        .filter(|v| *v > 0)
        .ok_or(Failure::Budget)?;
    let (heights, used) = if nodes.is_empty() {
        (Vec::new(), 0)
    } else {
        let (result, detail) =
            rail_grade_fit::solve_diagnosed(&nodes, &edges, remaining, &cancelled);
        if let Some(detail) = detail {
            let road = offsets.partition_point(|&start| start <= detail.node) - 1;
            let point = detail.node - offsets[road];
            *conflict = Some(ControlConflict {
                kind: ConflictKind::IntervalEnvelope,
                road,
                point,
                position: roads[road].path.points()[point],
                required: detail.minimum,
                available: detail.maximum,
                envelope: Some(detail.prefix),
            });
        }
        result?
    };
    work += used;
    let mut output = Vec::with_capacity(roads.len());
    let mut changed_controls = 0;
    let mut maximum_grade_after = 0_f64;
    let mut maximum_control_earthwork = 0_f64;
    for (r, road) in roads.iter().enumerate() {
        if cancelled() {
            return Err(Failure::Cancelled);
        }
        let h: Vec<_> = heights[offsets[r]..offsets[r] + road.ground.len()]
            .iter()
            .map(|v| *v as f32)
            .collect();
        for (i, &height) in h.iter().enumerate() {
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            if work >= maximum_work {
                return Err(Failure::Budget);
            }
            work += 1;
            changed_controls += usize::from(height.to_bits() != road.path.points()[i][1].to_bits());
            let earthwork = (f64::from(height) - f64::from(road.ground[i])).abs();
            // The published f32, rather than the solver's f64, is authoritative.
            if earthwork > allowance {
                *conflict = Some(ControlConflict {
                    kind: ConflictKind::RoundedEarthwork,
                    road: r,
                    point: i,
                    position: road.path.points()[i],
                    required: earthwork,
                    available: allowance,
                    envelope: None,
                });
                return Err(Failure::Infeasible);
            }
            maximum_control_earthwork = maximum_control_earthwork.max(earthwork);
            if i > 0 {
                let length = run(road.path.points()[i - 1], road.path.points()[i]);
                let difference = (f64::from(height) - f64::from(h[i - 1])).abs();
                let grade = if length < 1e-4 {
                    if difference != 0. {
                        return Err(Failure::Infeasible);
                    }
                    0.
                } else {
                    difference / length
                };
                if grade > f64::from(maximum_grade) {
                    *conflict = Some(ControlConflict {
                        kind: ConflictKind::RoundedGrade,
                        road: r,
                        point: i,
                        position: road.path.points()[i],
                        required: difference,
                        available: length * f64::from(maximum_grade),
                        envelope: None,
                    });
                    return Err(Failure::Infeasible);
                }
                maximum_grade_after = maximum_grade_after.max(grade);
            }
        }
        let remaining = maximum_work
            .checked_sub(work)
            .filter(|v| *v > 0)
            .ok_or(Failure::Budget)?;
        let path = road
            .path
            .refit_heights(&h, 0, &vec![1.; h.len()], remaining, &cancelled)
            .map_err(|e| match e {
                crate::path_geometry::Failure::Invalid => Failure::Invalid,
                crate::path_geometry::Failure::Budget => Failure::Budget,
            })?
            .ok_or(Failure::Cancelled)?;
        work += path.work();
        output.push(path);
    }
    Ok(Proposal {
        roads: output,
        report: Report {
            engine: if coupled { COUPLED_VERSION } else { VERSION },
            coupled_network_endpoints: coupled.then_some(network_ends.len()),
            controls: count,
            fixed_controls,
            junctions: junctions.len(),
            changed_controls,
            maximum_grade_before,
            maximum_grade_after,
            maximum_control_earthwork,
            height_reserve,
            work,
            terrain_applied: false,
            runtime_access_validated: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_network_is_cancellable_and_cannot_hide_dangling_junctions_or_invalid_limits() {
        let result = propose(&[], &[], 0.2, 8., 100, || false).unwrap();
        assert!(result.roads.is_empty());
        assert_eq!(result.report.controls, 0);
        assert_eq!(result.report.changed_controls, 0);
        assert!(!result.report.terrain_applied && !result.report.runtime_access_validated);
        assert!(matches!(
            propose(&[], &[], 0.2, 8., 100, || true),
            Err(Failure::Cancelled)
        ));
        assert!(matches!(
            propose(
                &[],
                &[Junction {
                    a: (0, 0),
                    b: (0, 0)
                }],
                0.2,
                8.,
                100,
                || false
            ),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            propose(&[], &[], f32::NAN, 8., 100, || false),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            propose(&[], &[], 0.2, 8., 0, || false),
            Err(Failure::Invalid)
        ));
    }
    fn path(points: &[[f32; 3]]) -> Path {
        Path::from_controls(points, 10000, || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn diagnosed_proposals_keep_results_and_locate_the_adjustable_conflicting_control() {
        let steep = path(&[[0., 0., 0.], [10., 50., 0.], [20., 0., 0.]]);
        let roads = [Road {
            path: &steep,
            ground: &[0., 50., 0.],
            fixed: &[true, false, true],
        }];
        let (result, conflict) = propose_diagnosed(
            &roads,
            &[],
            0.2,
            8.,
            0.,
            JunctionMode::CoupledNetwork,
            10000,
            || false,
        );
        assert!(matches!(result, Err(Failure::Infeasible)));
        let conflict = conflict.unwrap();
        assert_eq!((conflict.road, conflict.point), (0, 1));
        assert_eq!(conflict.kind, ConflictKind::IntervalEnvelope);
        assert_eq!(conflict.position, [10., 50., 0.]);
        assert!(conflict.required > conflict.available);
        assert!(matches!(
            propose(&roads, &[], 0.2, 8., 10000, || false),
            Err(Failure::Infeasible)
        ));
        let flat = path(&[[0., 0., 0.], [10., 0., 0.], [20., 0., 0.]]);
        let roads = [Road {
            path: &flat,
            ground: &[0.; 3],
            fixed: &[true, false, true],
        }];
        let original = propose_with_junction_policy(
            &roads,
            &[],
            0.2,
            8.,
            0.,
            JunctionMode::CoupledNetwork,
            10000,
            || false,
        )
        .unwrap();
        let (result, conflict) = propose_diagnosed(
            &roads,
            &[],
            0.2,
            8.,
            0.,
            JunctionMode::CoupledNetwork,
            10000,
            || false,
        );
        let diagnosed = result.unwrap();
        assert!(conflict.is_none());
        assert_eq!(original.roads[0].points(), diagnosed.roads[0].points());
        assert_eq!(original.report.work, diagnosed.report.work);
        let (result, conflict) = propose_diagnosed(
            &roads,
            &[],
            0.2,
            8.,
            0.,
            JunctionMode::CoupledNetwork,
            10000,
            || true,
        );
        assert!(matches!(result, Err(Failure::Cancelled)));
        assert!(conflict.is_none());
    }
    #[test]
    fn coupled_network_height_repair_preserves_authored_and_protected_endpoints() {
        let main = path(&[[-20., 10., 0.], [0., 18., 0.], [20., 10., 0.]]);
        let branch = path(&[[0., 18., 0.], [0., 10., 60.]]);
        let links = [Junction {
            a: (1, 0),
            b: (0, 1),
        }];
        let roads = [
            Road {
                path: &main,
                ground: &[10.; 3],
                fixed: &[true, false, true],
            },
            Road {
                path: &branch,
                ground: &[10.; 2],
                fixed: &[false, false],
            },
        ];
        assert!(matches!(
            propose_with_reserve(&roads, &links, 0.2, 8., 0.12, 10000, || false),
            Err(Failure::Infeasible)
        ));
        let out = propose_with_junction_policy(
            &roads,
            &links,
            0.2,
            8.,
            0.12,
            JunctionMode::CoupledNetwork,
            10000,
            || false,
        )
        .unwrap();
        assert_eq!(out.report.engine, COUPLED_VERSION);
        assert_eq!(out.report.coupled_network_endpoints, Some(1));
        assert_eq!(out.roads[0].points()[1], out.roads[1].points()[0]);
        assert!(out.roads[0].points()[1][1] < 14.);
        assert_eq!(out.roads[0].points()[0], main.points()[0]);
        assert_eq!(out.roads[0].points()[2], main.points()[2]);
        assert_eq!(out.roads[1].points()[1], branch.points()[1]);
        assert!(out.report.maximum_grade_after <= f64::from(0.2f32));
        assert!(out.report.maximum_control_earthwork <= 8.);
        let fixed = [
            Road {
                path: &main,
                ground: &[10.; 3],
                fixed: &[true; 3],
            },
            Road {
                path: &branch,
                ground: &[10.; 2],
                fixed: &[false, false],
            },
        ];
        assert!(matches!(
            propose_with_junction_policy(
                &fixed,
                &links,
                0.2,
                8.,
                0.12,
                JunctionMode::CoupledNetwork,
                10000,
                || false
            ),
            Err(Failure::Infeasible)
        ));
        let steep = path(&[[0., 18., 0.], [0., 0., 10.]]);
        let impossible = [
            Road {
                path: &main,
                ground: &[10.; 3],
                fixed: &[true, false, true],
            },
            Road {
                path: &steep,
                ground: &[10., 0.],
                fixed: &[false, false],
            },
        ];
        assert!(matches!(
            propose_with_junction_policy(
                &impossible,
                &links,
                0.2,
                8.,
                0.12,
                JunctionMode::CoupledNetwork,
                10000,
                || false
            ),
            Err(Failure::Infeasible)
        ));
        assert!(matches!(
            propose_with_junction_policy(
                &roads,
                &links,
                0.2,
                8.,
                0.12,
                JunctionMode::CoupledNetwork,
                10000,
                || true
            ),
            Err(Failure::Cancelled)
        ));
        assert!(matches!(
            propose_with_junction_policy(
                &roads,
                &links,
                0.2,
                8.,
                0.12,
                JunctionMode::CoupledNetwork,
                1,
                || false
            ),
            Err(Failure::Budget)
        ));
    }
    #[test]
    fn repairs_slopes_preserving_endpoints_junctions_and_protected_controls() {
        let a = path(&[
            [0., 0., 0.],
            [10., 0., 0.],
            [20., 5., 0.],
            [30., 0., 0.],
            [40., 0., 0.],
        ]);
        let b = path(&[[10., 0., 0.], [10., 5., 10.], [10., 0., 20.]]);
        let inputs = [
            Road {
                path: &a,
                ground: &[0.; 5],
                fixed: &[false, false, false, true, false],
            },
            Road {
                path: &b,
                ground: &[0.; 3],
                fixed: &[false; 3],
            },
        ];
        let joins = [Junction {
            a: (0, 1),
            b: (1, 0),
        }];
        let p = propose(&inputs, &joins, 0.2, 8., 100000, || false).unwrap();
        assert!(p.report.maximum_grade_before > 0.2 && p.report.maximum_grade_after <= 0.2);
        assert_eq!(p.roads[0].points()[1], p.roads[1].points()[0]);
        assert_eq!(p.roads[0].points()[3], a.points()[3]);
        assert_eq!(a.points()[2][1], 5.);
        let again = propose(&inputs, &joins, 0.2, 8., 100000, || false).unwrap();
        assert_eq!(p.roads[0].points(), again.roads[0].points());
        assert!(!p.report.terrain_applied);
    }
    #[test]
    fn closed_seams_and_infeasible_protection_are_explicit() {
        let a = path(&[
            [0., 0., 0.],
            [10., 5., 0.],
            [10., 0., 10.],
            [0., 0., 10.],
            [0., 0., 0.],
        ]);
        let road = Road {
            path: &a,
            ground: &[0.; 5],
            fixed: &[false; 5],
        };
        let p = propose(&[road], &[], 0.2, 8., 100000, || false).unwrap();
        assert_eq!(p.roads[0].points().first(), p.roads[0].points().last());
        let road = Road {
            path: &a,
            ground: &[0.; 5],
            fixed: &[true; 5],
        };
        assert!(matches!(
            propose(&[road], &[], 0.2, 8., 100000, || false),
            Err(Failure::Infeasible)
        ));
    }
    #[test]
    fn budgets_cancellation_and_invalid_links_publish_nothing() {
        let a = path(&[[0., 0., 0.], [10., 5., 0.], [20., 0., 0.]]);
        let roads = [Road {
            path: &a,
            ground: &[0.; 3],
            fixed: &[false; 3],
        }];
        assert!(matches!(
            propose(&roads, &[], 0.2, 8., 1, || false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            propose(&roads, &[], 0.2, 8., 100000, || true),
            Err(Failure::Cancelled)
        ));
        assert!(matches!(
            propose(
                &roads,
                &[Junction {
                    a: (0, 0),
                    b: (0, 2)
                }],
                0.2,
                8.,
                100000,
                || false
            ),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            propose(&roads, &[], f32::NAN, 8., 100000, || false),
            Err(Failure::Invalid)
        ));
    }
    #[test]
    fn coincident_join_controls_retain_indices_and_junctions_cannot_bypass_earthwork() {
        let a = path(&[
            [0., 0., 0.],
            [10., 5., 0.],
            [10.00001, 5., 0.],
            [20., 0., 0.],
        ]);
        let roads = [Road {
            path: &a,
            ground: &[0.; 4],
            fixed: &[false; 4],
        }];
        let p = propose(&roads, &[], 0.2, 8., 100000, || false).unwrap();
        assert_eq!(p.roads[0].points().len(), 4);
        assert_eq!(p.roads[0].points()[1][1], p.roads[0].points()[2][1]);
        let bad = [Road {
            path: &a,
            ground: &[0., -10., -10., 0.],
            fixed: &[false; 4],
        }];
        assert!(matches!(
            propose(
                &bad,
                &[Junction {
                    a: (0, 1),
                    b: (0, 1)
                }],
                0.2,
                8.,
                100000,
                || false
            ),
            Err(Failure::Infeasible)
        ));
    }
}
