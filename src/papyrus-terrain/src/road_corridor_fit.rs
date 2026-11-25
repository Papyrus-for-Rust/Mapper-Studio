//! Bounded coupled control-height proposals. Acceptance still requires the
//! caller's f32 terrain, curve and native-mesh checks.
use crate::rail_grade_fit::{Edge, Failure, Node};
use std::collections::VecDeque;

const EPSILON: f64 = 1e-7;
// Working headroom only. Every original sample, grade link and hard node bound
// must still pass its unchanged check before a candidate can be returned.
const HEADROOM: f64 = 0.001;
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub a: usize,
    pub b: usize,
    pub along: f64,
    pub minimum: f64,
    pub maximum: f64,
}
pub struct Solution {
    pub heights: Vec<f64>,
    pub work: usize,
    pub rows: usize,
}
#[derive(Clone, Copy)]
struct Row {
    indices: [usize; 2],
    coefficients: [f64; 2],
    constant: f64,
    norm: f64,
    minimum: f64,
    maximum: f64,
    correction: f64,
}
struct Meter<'a, F> {
    work: usize,
    maximum: usize,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, amount: usize) -> Result<(), Failure> {
        if (self.cancelled)() {
            return Err(Failure::Cancelled);
        }
        self.work = self.work.checked_add(amount).ok_or(Failure::Budget)?;
        if self.work > self.maximum {
            return Err(Failure::Budget);
        }
        Ok(())
    }
}
fn hull(points: &[(f64, f64)], lower: bool) -> Vec<(f64, f64)> {
    let mut result: Vec<(f64, f64)> = Vec::new();
    for &p in points {
        while result.len() >= 2 {
            let a = result[result.len() - 2];
            let b = result[result.len() - 1];
            let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
            if (lower && cross >= 0.) || (!lower && cross <= 0.) {
                result.pop();
            } else {
                break;
            }
        }
        result.push(p);
    }
    result
}
fn clip<F: Fn() -> bool>(
    polygon: &mut Vec<[f64; 2]>,
    coefficients: [f64; 2],
    bound: f64,
    meter: &mut Meter<'_, F>,
) -> Result<(), Failure> {
    meter.spend(polygon.len() + 1)?;
    let mut result = Vec::new();
    let mut previous = *polygon.last().ok_or(Failure::Infeasible)?;
    let value = |p: [f64; 2]| coefficients[0] * p[0] + coefficients[1] * p[1];
    // Expand for proof only: an empty relaxed polygon also excludes the
    // original pair. Projection and final checks retain their original limits.
    let bound = bound + EPSILON;
    let mut pv = value(previous);
    for &current in polygon.iter() {
        let cv = value(current);
        if (pv <= bound) != (cv <= bound) {
            let t = ((bound - pv) / (cv - pv)).clamp(0., 1.);
            result.push(std::array::from_fn(|k| {
                previous[k] + t * (current[k] - previous[k])
            }));
        }
        if cv <= bound {
            result.push(current);
        }
        previous = current;
        pv = cv;
    }
    if result.is_empty() {
        return Err(Failure::Infeasible);
    }
    *polygon = result;
    Ok(())
}
fn row(
    nodes: &[Node],
    indices: [usize; 2],
    mut coefficients: [f64; 2],
    minimum: f64,
    maximum: f64,
) -> Result<Row, Failure> {
    let mut constant = 0.;
    let mut lo = 0.;
    let mut hi = 0.;
    for k in 0..2 {
        let node = nodes[indices[k]];
        let c = coefficients[k];
        lo += c * if c >= 0. { node.minimum } else { node.maximum };
        hi += c * if c >= 0. { node.maximum } else { node.minimum };
        if node.minimum == node.maximum {
            constant += c * node.minimum;
            coefficients[k] = 0.;
        }
    }
    if lo > maximum + EPSILON || hi < minimum - EPSILON {
        return Err(Failure::Infeasible);
    }
    let margin = ((maximum - minimum) * 0.25).min(HEADROOM);
    let working_minimum = if minimum > lo {
        (minimum + margin).min(hi).min(maximum)
    } else {
        minimum
    };
    let working_maximum = if maximum < hi {
        (maximum - margin).max(lo).max(working_minimum)
    } else {
        maximum
    };
    Ok(Row {
        indices,
        coefficients,
        constant,
        norm: coefficients.iter().map(|c| c * c).sum(),
        minimum: working_minimum,
        maximum: working_maximum,
        correction: 0.,
    })
}
/// Dykstra projections onto two-control slabs, grade links and node bounds.
/// Convex hull reduction removes implied sample rows; original rows are checked
/// before returning. A failure to converge is Budget, never proof of infeasibility.
pub fn solve(
    nodes: &[Node],
    edges: &[Edge],
    samples: &[Sample],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Solution, Failure> {
    let mut meter = Meter {
        work: 0,
        maximum: maximum_work,
        cancelled: &cancelled,
    };
    meter.spend(0)?;
    if nodes.is_empty()
        || nodes.len() > 65536
        || edges.len() > 131072
        || samples.len() > 1_000_000
        || !(1..=32_000_000).contains(&maximum_work)
    {
        return Err(Failure::Invalid);
    }
    meter.spend(nodes.len() + edges.len() + samples.len())?;
    if nodes.iter().any(|n| {
        !n.desired.is_finite()
            || !n.minimum.is_finite()
            || !n.maximum.is_finite()
            || n.minimum > n.maximum
            || n.minimum < -10000.
            || n.maximum > 10000.
    }) || edges.iter().any(|e| {
        e.a >= nodes.len()
            || e.b >= nodes.len()
            || e.a == e.b
            || !e.maximum_difference.is_finite()
            || !(0. ..=10000.).contains(&e.maximum_difference)
    }) || samples.iter().any(|s| {
        s.a >= nodes.len()
            || s.b >= nodes.len()
            || s.a == s.b
            || !s.along.is_finite()
            || !(0. ..=1.).contains(&s.along)
            || !s.minimum.is_finite()
            || !s.maximum.is_finite()
            || s.minimum > s.maximum
            || s.minimum < -10000.
            || s.maximum > 10000.
    }) {
        return Err(Failure::Invalid);
    }
    // Reserve a conservative comparison-sort plus linear hull-construction cost.
    let logarithm = usize::BITS as usize - samples.len().max(1).leading_zeros() as usize;
    meter.spend(samples.len().saturating_mul(logarithm + 8))?;
    let mut ordered = samples.to_vec();
    ordered.sort_unstable_by(|a, b| {
        (a.a, a.b)
            .cmp(&(b.a, b.b))
            .then_with(|| a.along.total_cmp(&b.along))
            .then_with(|| a.minimum.total_cmp(&b.minimum))
            .then_with(|| a.maximum.total_cmp(&b.maximum))
    });
    let mut rows = Vec::new();
    let mut start = 0;
    while start < ordered.len() {
        meter.spend(1)?;
        let first = ordered[start];
        let mut end = start;
        let mut lower: Vec<(f64, f64)> = Vec::new();
        let mut upper: Vec<(f64, f64)> = Vec::new();
        while end < ordered.len() && (ordered[end].a, ordered[end].b) == (first.a, first.b) {
            let s = ordered[end];
            if let Some(last) = lower.last_mut().filter(|p| p.0 == s.along) {
                last.1 = last.1.max(s.minimum);
                let last = upper.last_mut().unwrap();
                last.1 = last.1.min(s.maximum);
            } else {
                lower.push((s.along, s.minimum));
                upper.push((s.along, s.maximum));
            }
            end += 1;
        }
        let lower = hull(&lower, true);
        let upper = hull(&upper, false);
        let (a, b) = (nodes[first.a], nodes[first.b]);
        let mut polygon = vec![
            [a.minimum, b.minimum],
            [a.maximum, b.minimum],
            [a.maximum, b.maximum],
            [a.minimum, b.maximum],
        ];
        for &(t, minimum) in &lower {
            clip(&mut polygon, [t - 1., -t], -minimum, &mut meter)?;
        }
        for &(t, maximum) in &upper {
            clip(&mut polygon, [1. - t, t], maximum, &mut meter)?;
        }
        for (t, minimum) in lower {
            rows.push(row(
                nodes,
                [first.a, first.b],
                [1. - t, t],
                minimum,
                10000.,
            )?);
        }
        for (t, maximum) in upper {
            rows.push(row(
                nodes,
                [first.a, first.b],
                [1. - t, t],
                -10000.,
                maximum,
            )?);
        }
        start = end;
    }
    for e in edges {
        rows.push(row(
            nodes,
            [e.a, e.b],
            [1., -1.],
            -e.maximum_difference,
            e.maximum_difference,
        )?);
    }
    for (i, n) in nodes.iter().enumerate() {
        if n.minimum != n.maximum {
            rows.push(Row {
                indices: [i, i],
                coefficients: [1., 0.],
                constant: 0.,
                norm: 1.,
                minimum: n.minimum,
                maximum: n.maximum,
                correction: 0.,
            });
        }
    }
    let mut heights: Vec<_> = nodes
        .iter()
        .map(|n| n.desired.clamp(n.minimum, n.maximum))
        .collect();
    if rows.is_empty() {
        return Ok(Solution {
            heights,
            work: meter.work,
            rows: 0,
        });
    }
    meter.spend(rows.len() * 2 + nodes.len())?;
    let mut adjacency = vec![Vec::new(); nodes.len()];
    for (i, r) in rows.iter().enumerate() {
        for k in 0..2 {
            if r.coefficients[k] != 0. {
                adjacency[r.indices[k]].push(i);
            }
        }
    }
    let mut queue: VecDeque<_> = (0..rows.len()).collect();
    let mut queued = vec![true; rows.len()];
    let mut visits = 0;
    loop {
        let Some(index) = queue.pop_front() else {
            return Err(Failure::Budget);
        };
        queued[index] = false;
        meter.spend(8).inspect_err(|_| {
            #[cfg(feature = "road-diagnostics")]
            if std::env::var_os("MAPPER_ROAD_RING_COST_DEBUG").is_some() {
                let excess = rows
                    .iter()
                    .map(|r| {
                        let v = r.constant
                            + r.coefficients[0] * heights[r.indices[0]]
                            + r.coefficients[1] * heights[r.indices[1]];
                        (r.minimum - v).max(v - r.maximum).max(0.)
                    })
                    .fold(0., f64::max);
                eprintln!(
                    "Coupled ring fit stopped: visits={visits}, rows={}, samples={}, grant={maximum_work}, work={}, excess={excess}",
                    rows.len(),
                    samples.len(),
                    meter.work
                );
            }
        })?;
        {
            let mut r = rows[index];
            let before = [heights[r.indices[0]], heights[r.indices[1]]];
            let mut value = r.constant;
            for k in 0..2 {
                heights[r.indices[k]] += r.correction * r.coefficients[k];
                value += r.coefficients[k] * heights[r.indices[k]];
            }
            if !value.is_finite() {
                return Err(Failure::Invalid);
            }
            if r.norm != 0. {
                r.correction = (value - value.clamp(r.minimum, r.maximum)) / r.norm;
                for k in 0..2 {
                    heights[r.indices[k]] -= r.correction * r.coefficients[k];
                }
            }
            rows[index] = r;
            // A satisfied row with unchanged inputs is already at its own
            // projection fixed point, even when its dual correction is nonzero.
            // Only changed neighboring controls require another visit.
            for (k, old) in before.into_iter().enumerate() {
                let variable = r.indices[k];
                if heights[variable] != old && r.coefficients[k] != 0. {
                    meter.spend(adjacency[variable].len()).inspect_err(|_| {
                        #[cfg(feature = "road-diagnostics")]
                        if std::env::var_os("MAPPER_ROAD_RING_COST_DEBUG").is_some() {
                            let excess = rows
                                .iter()
                                .map(|r| {
                                    let v = r.constant
                                        + r.coefficients[0] * heights[r.indices[0]]
                                        + r.coefficients[1] * heights[r.indices[1]];
                                    (r.minimum - v).max(v - r.maximum).max(0.)
                                })
                                .fold(0., f64::max);
                            eprintln!(
                                "Coupled ring adjacency stopped: visits={visits}, rows={}, grant={maximum_work}, work={}, excess={excess}",
                                rows.len(),
                                meter.work
                            );
                        }
                    })?;
                    for &neighbor in &adjacency[variable] {
                        if neighbor != index && !queued[neighbor] {
                            queue.push_back(neighbor);
                            queued[neighbor] = true;
                        }
                    }
                }
            }
        }
        visits += 1;
        if visits % rows.len().max(1) != 0 && !queue.is_empty() {
            continue;
        }
        meter.spend(rows.len() * 3)?;
        let excess = rows
            .iter()
            .map(|r| {
                let value = r.constant
                    + r.coefficients[0] * heights[r.indices[0]]
                    + r.coefficients[1] * heights[r.indices[1]];
                (r.minimum - value).max(value - r.maximum).max(0.)
            })
            .fold(0., f64::max);
        if excess <= HEADROOM + EPSILON {
            meter.spend(samples.len() * 3 + edges.len() * 2 + nodes.len())?;
            let candidate: Vec<_> = heights
                .iter()
                .zip(nodes)
                .map(|(&h, n)| h.clamp(n.minimum, n.maximum))
                .collect();
            let valid = samples.iter().all(|s| {
                let v = (1. - s.along) * candidate[s.a] + s.along * candidate[s.b];
                v >= s.minimum - EPSILON && v <= s.maximum + EPSILON
            }) && edges
                .iter()
                .all(|e| (candidate[e.a] - candidate[e.b]).abs() <= e.maximum_difference + EPSILON)
                && nodes
                    .iter()
                    .zip(&candidate)
                    .all(|(n, &h)| h.is_finite() && h >= n.minimum && h <= n.maximum);
            if valid {
                return Ok(Solution {
                    heights: candidate,
                    work: meter.work,
                    rows: rows.len(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn nodes() -> Vec<Node> {
        vec![
            Node {
                desired: 5.,
                minimum: 0.,
                maximum: 10.
            };
            2
        ]
    }
    fn samples() -> Vec<Sample> {
        vec![
            Sample {
                a: 0,
                b: 1,
                along: 0.25,
                minimum: 2.4,
                maximum: 2.6,
            },
            Sample {
                a: 0,
                b: 1,
                along: 0.75,
                minimum: 7.4,
                maximum: 7.6,
            },
        ]
    }
    #[test]
    fn interpolated_constraints_fit_when_equal_endpoint_corrections_cannot() {
        let result = solve(
            &nodes(),
            &[Edge {
                a: 0,
                b: 1,
                maximum_difference: 10.,
            }],
            &samples(),
            100000,
            || false,
        )
        .unwrap();
        assert!(result.heights[0] < 0.3 && result.heights[1] > 9.7);
        assert!(result.work <= 100000);
    }
    #[test]
    fn reduction_is_order_independent_and_checks_original_samples() {
        let mut samples = samples();
        for i in 0..101 {
            let t = i as f64 / 100.;
            samples.push(Sample {
                a: 0,
                b: 1,
                along: t,
                minimum: -1.,
                maximum: 11.,
            });
        }
        let a = solve(&nodes(), &[], &samples, 100000, || false).unwrap();
        samples.reverse();
        let b = solve(&nodes(), &[], &samples, 100000, || false).unwrap();
        assert_eq!(a.heights, b.heights);
        assert!(a.rows < samples.len());
    }
    #[test]
    fn exact_rows_and_fixed_boundary_poses_do_not_inherit_working_headroom() {
        let nodes = [
            Node {
                desired: 0.,
                minimum: 0.,
                maximum: 0.,
            },
            Node {
                desired: 5.,
                minimum: 0.,
                maximum: 10.,
            },
        ];
        let samples = [
            Sample {
                a: 0,
                b: 1,
                along: 0.,
                minimum: 0.,
                maximum: 0.,
            },
            Sample {
                a: 0,
                b: 1,
                along: 1.,
                minimum: 10.,
                maximum: 10.,
            },
        ];
        let result = solve(
            &nodes,
            &[Edge {
                a: 0,
                b: 1,
                maximum_difference: 10.,
            }],
            &samples,
            100000,
            || false,
        )
        .unwrap();
        assert_eq!(result.heights, vec![0., 10.]);
        let fixed = solve(&nodes[..1], &[], &[], 100000, || false).unwrap();
        assert_eq!(fixed.heights, vec![0.]);
    }
    #[test]
    fn fixed_poses_invalid_input_cancellation_and_budget_are_explicit() {
        let mut nodes = nodes();
        nodes[0] = Node {
            desired: 0.,
            minimum: 0.,
            maximum: 0.,
        };
        let result = solve(&nodes, &[], &samples(), 100000, || false).unwrap();
        assert_eq!(result.heights[0], 0.);
        assert!(matches!(
            solve(&nodes, &[], &samples(), 100000, || true),
            Err(Failure::Cancelled)
        ));
        assert!(matches!(
            solve(&nodes, &[], &samples(), 1, || false),
            Err(Failure::Budget)
        ));
        let mut bad = samples();
        bad[0].along = f64::NAN;
        assert!(matches!(
            solve(&nodes, &[], &bad, 100000, || false),
            Err(Failure::Invalid)
        ));
        nodes[1] = Node {
            desired: 0.,
            minimum: 0.,
            maximum: 0.,
        };
        assert!(matches!(
            solve(&nodes, &[], &samples(), 100000, || false),
            Err(Failure::Infeasible)
        ));
    }
}
