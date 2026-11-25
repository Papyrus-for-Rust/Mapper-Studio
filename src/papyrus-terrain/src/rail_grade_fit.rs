//! Bounded graph-height projection for an explicitly requested Mapper rail fit.
//! This is independent network fitting, not an official game generation stage.
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BinaryHeap};
pub const VERSION: &str = "papyrus-rail-grade-fit-1";
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_grade: f32,
    pub maximum_work: usize,
    pub maximum_attempts: u8,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.maximum_grade.is_finite()
            || !(0.001..=1.).contains(&self.maximum_grade)
            || !(1..=32000000).contains(&self.maximum_work)
            || !(1..=8).contains(&self.maximum_attempts)
        {
            return Err("Invalid connected rail grade fitting limits".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub desired: f64,
    pub minimum: f64,
    pub maximum: f64,
}
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub a: usize,
    pub b: usize,
    pub maximum_difference: f64,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
    Infeasible,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct IntervalConflict {
    pub node: usize,
    pub minimum: f64,
    pub maximum: f64,
    pub prefix: EnvelopePrefix,
}
/// Earliest violated envelope in stable graph-node order, before diagnosis
/// chooses a more useful adjustable witness. All earlier nodes passed the
/// unchanged solver envelope comparison. This is not a feasibility certificate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EnvelopePrefix {
    pub first_node: usize,
    pub required: f64,
    pub available: f64,
    pub nodes: usize,
    pub edges: usize,
}
pub type Projection = Result<(Vec<f64>, usize), Failure>;
#[derive(Clone, Copy)]
struct Item {
    cost: f64,
    node: usize,
}
impl PartialEq for Item {
    fn eq(&self, b: &Self) -> bool {
        self.cost == b.cost && self.node == b.node
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        Some(self.cmp(b))
    }
}
impl Ord for Item {
    fn cmp(&self, b: &Self) -> Ordering {
        b.cost
            .total_cmp(&self.cost)
            .then_with(|| b.node.cmp(&self.node))
    }
}
fn visit(work: &mut usize, maximum: usize, cancelled: &impl Fn() -> bool) -> Result<(), Failure> {
    if cancelled() {
        return Err(Failure::Cancelled);
    }
    if *work >= maximum {
        return Err(Failure::Budget);
    }
    *work += 1;
    Ok(())
}
fn envelope(
    initial: Vec<f64>,
    edges: &[Vec<(usize, f64)>],
    work: &mut usize,
    maximum: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<f64>, Failure> {
    let mut result = initial;
    let mut heap = BinaryHeap::new();
    for (node, &cost) in result.iter().enumerate() {
        visit(work, maximum, cancelled)?;
        heap.push(Item { cost, node });
    }
    while let Some(Item { cost, node }) = heap.pop() {
        visit(work, maximum, cancelled)?;
        if cost > result[node] {
            continue;
        }
        for &(other, difference) in &edges[node] {
            visit(work, maximum, cancelled)?;
            let next = cost + difference;
            if next < result[other] {
                result[other] = next;
                heap.push(Item {
                    cost: next,
                    node: other,
                });
            }
        }
    }
    Ok(result)
}
/// Computes lower/upper Lipschitz envelopes and projects the desired surface
/// between them. Midpoint, max and min preserve the edge difference constraints.
/// Exact fixed endpoints are represented by equal minimum and maximum bounds.
pub fn solve(
    nodes: &[Node],
    edges: &[Edge],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<(Vec<f64>, usize), Failure> {
    solve_diagnosed(nodes, edges, maximum_work, cancelled).0
}
/// Same projection and limits, retaining the first conflicting envelope index
/// on infeasibility. Successful heights and work accounting are unchanged.
pub fn solve_diagnosed(
    nodes: &[Node],
    edges: &[Edge],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> (Projection, Option<IntervalConflict>) {
    let (result, conflict, _) = solve_measured(nodes, edges, maximum_work, cancelled);
    (result, conflict)
}

pub(crate) fn solve_measured(
    nodes: &[Node],
    edges: &[Edge],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> (Projection, Option<IntervalConflict>, usize) {
    let mut conflict = None;
    let mut work = 0;
    let result = solve_inner(
        nodes,
        edges,
        maximum_work,
        cancelled,
        &mut conflict,
        &mut work,
    );
    (result, conflict, work)
}

fn solve_inner(
    nodes: &[Node],
    edges: &[Edge],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
    conflict: &mut Option<IntervalConflict>,
    work: &mut usize,
) -> Result<(Vec<f64>, usize), Failure> {
    if nodes.is_empty()
        || nodes.len() > 131072
        || edges.len() > 262144
        || !(1..=32000000).contains(&maximum_work)
        || nodes.iter().any(|v| {
            !v.desired.is_finite()
                || !v.minimum.is_finite()
                || !v.maximum.is_finite()
                || v.minimum > v.maximum
                || v.minimum < -10000.
                || v.maximum > 10000.
        })
        || edges.iter().any(|v| {
            v.a >= nodes.len()
                || v.b >= nodes.len()
                || !v.maximum_difference.is_finite()
                || !(0. ..=10000.).contains(&v.maximum_difference)
        })
    {
        return Err(Failure::Invalid);
    }
    let mut adjacency = vec![Vec::new(); nodes.len()];
    for e in edges {
        visit(work, maximum_work, &cancelled)?;
        adjacency[e.a].push((e.b, e.maximum_difference));
        adjacency[e.b].push((e.a, e.maximum_difference));
    }
    let upper = envelope(
        nodes.iter().map(|n| n.maximum).collect(),
        &adjacency,
        work,
        maximum_work,
        &cancelled,
    )?;
    let lower = envelope(
        nodes.iter().map(|n| -n.minimum).collect(),
        &adjacency,
        work,
        maximum_work,
        &cancelled,
    )?;
    if let Some(first) = upper.iter().zip(&lower).position(|(&u, &l)| u < -l - 1e-9) {
        let mut node = first;
        let mut minimum = -lower[first];
        // Prefer the local lower bound that cannot reach an upper-bound
        // anchor, rather than blaming the immutable anchor itself. Diagnosis
        // shares the remaining grant and cannot hide infeasibility.
        let available = maximum_work.saturating_sub(*work);
        for (i, bound) in nodes.iter().enumerate().take(available) {
            visit(work, maximum_work, &cancelled)?;
            if bound.minimum < bound.maximum && upper[i] < bound.minimum - 1e-9 {
                node = i;
                minimum = bound.minimum;
                break;
            }
        }
        *conflict = Some(IntervalConflict {
            node,
            minimum,
            maximum: upper[node],
            prefix: EnvelopePrefix {
                first_node: first,
                required: -lower[first],
                available: upper[first],
                nodes: nodes.len(),
                edges: edges.len(),
            },
        });
        return Err(Failure::Infeasible);
    }
    let down = envelope(
        nodes
            .iter()
            .map(|n| n.desired.clamp(n.minimum, n.maximum))
            .collect(),
        &adjacency,
        work,
        maximum_work,
        &cancelled,
    )?;
    let up = envelope(
        nodes
            .iter()
            .map(|n| -n.desired.clamp(n.minimum, n.maximum))
            .collect(),
        &adjacency,
        work,
        maximum_work,
        &cancelled,
    )?;
    let mut fitted = Vec::with_capacity(nodes.len());
    for i in 0..nodes.len() {
        visit(work, maximum_work, &cancelled)?;
        fitted.push(((down[i] - up[i]) * 0.5).max(-lower[i]).min(upper[i]));
    }
    for e in edges {
        visit(work, maximum_work, &cancelled)?;
        if (fitted[e.a] - fitted[e.b]).abs() > e.maximum_difference + 1e-8 {
            return Err(Failure::Infeasible);
        }
    }
    Ok((fitted, *work))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_connected_loop_and_branch_keep_fixed_heights_and_all_edges_feasible() {
        let nodes = [
            Node {
                desired: 0.,
                minimum: 0.,
                maximum: 0.,
            },
            Node {
                desired: 10.,
                minimum: -20.,
                maximum: 20.,
            },
            Node {
                desired: 0.,
                minimum: -20.,
                maximum: 20.,
            },
            Node {
                desired: -10.,
                minimum: -20.,
                maximum: 20.,
            },
            Node {
                desired: 2.,
                minimum: 2.,
                maximum: 2.,
            },
        ];
        let edges = [
            Edge {
                a: 0,
                b: 1,
                maximum_difference: 2.,
            },
            Edge {
                a: 1,
                b: 2,
                maximum_difference: 2.,
            },
            Edge {
                a: 2,
                b: 3,
                maximum_difference: 2.,
            },
            Edge {
                a: 3,
                b: 0,
                maximum_difference: 2.,
            },
            Edge {
                a: 2,
                b: 4,
                maximum_difference: 2.,
            },
        ];
        let (fitted, _) = solve(&nodes, &edges, 10000, || false).unwrap();
        assert_eq!(fitted[0], 0.);
        assert_eq!(fitted[4], 2.);
        for e in &edges {
            assert!((fitted[e.a] - fitted[e.b]).abs() <= e.maximum_difference + 1e-9);
        }
        assert_eq!(fitted, solve(&nodes, &edges, 10000, || false).unwrap().0);
        let mut reversed = edges;
        reversed.reverse();
        assert_eq!(fitted, solve(&nodes, &reversed, 10000, || false).unwrap().0);
    }
    #[test]
    fn feasible_generated_graphs_preserve_witness_bounds_and_edge_limits() {
        let mut seed = 73u32;
        let mut random = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            f64::from(seed % 10000) / 1000.
        };
        for _ in 0..100 {
            let witness: Vec<f64> = (0..8).map(|_| random()).collect();
            let nodes: Vec<Node> = witness
                .iter()
                .enumerate()
                .map(|(i, &v)| Node {
                    desired: random() * 3. - 10.,
                    minimum: if i == 0 { v } else { v - 2. },
                    maximum: if i == 0 { v } else { v + 2. },
                })
                .collect();
            let mut edges = Vec::new();
            for a in 0..8 {
                for b in a + 1..8 {
                    if b == a + 1 || random() > 6. {
                        edges.push(Edge {
                            a,
                            b,
                            maximum_difference: (witness[a] - witness[b]).abs() + 0.1,
                        });
                    }
                }
            }
            let (fitted, _) = solve(&nodes, &edges, 100000, || false).unwrap();
            for (i, &v) in fitted.iter().enumerate() {
                assert!(v >= nodes[i].minimum - 1e-9 && v <= nodes[i].maximum + 1e-9);
            }
            assert!((fitted[0] - witness[0]).abs() < 1e-9);
            for e in edges {
                assert!((fitted[e.a] - fitted[e.b]).abs() <= e.maximum_difference + 1e-8);
            }
        }
    }
    #[test]
    fn diagnosed_conflict_prefers_adjustable_ground_over_an_earlier_fixed_anchor() {
        let nodes = [
            Node {
                desired: 20.,
                minimum: 20.,
                maximum: 20.,
            },
            Node {
                desired: 0.,
                minimum: 0.,
                maximum: 0.,
            },
            Node {
                desired: 45.,
                minimum: 40.,
                maximum: 50.,
            },
        ];
        let edges = [
            Edge {
                a: 0,
                b: 1,
                maximum_difference: 10.,
            },
            Edge {
                a: 1,
                b: 2,
                maximum_difference: 10.,
            },
        ];
        let (result, conflict) = solve_diagnosed(&nodes, &edges, 1000, || false);
        assert_eq!(result, Err(Failure::Infeasible));
        assert_eq!(
            conflict,
            Some(IntervalConflict {
                node: 2,
                minimum: 40.,
                maximum: 10.,
                prefix: EnvelopePrefix {
                    first_node: 0,
                    required: 20.,
                    available: 10.,
                    nodes: 3,
                    edges: 2
                },
            })
        );
    }
    #[test]
    fn envelope_prefix_advances_only_after_earlier_graph_nodes_are_feasible() {
        let mut nodes = [
            Node {
                desired: 20.,
                minimum: 20.,
                maximum: 20.,
            },
            Node {
                desired: 0.,
                minimum: 0.,
                maximum: 0.,
            },
            Node {
                desired: 45.,
                minimum: 40.,
                maximum: 50.,
            },
        ];
        let edges = [
            Edge {
                a: 0,
                b: 1,
                maximum_difference: 10.,
            },
            Edge {
                a: 1,
                b: 2,
                maximum_difference: 10.,
            },
        ];
        let (result, before) = solve_diagnosed(&nodes, &edges, 1000, || false);
        assert_eq!(result, Err(Failure::Infeasible));
        let before = before.unwrap();
        assert_eq!(before.prefix.first_node, 0);
        assert_eq!(before.node, 2);
        nodes[0] = Node {
            desired: 0.,
            minimum: 0.,
            maximum: 0.,
        };
        nodes[2] = Node {
            desired: 25.,
            minimum: 25.,
            maximum: 30.,
        };
        let (result, after) = solve_diagnosed(&nodes, &edges, 1000, || false);
        assert_eq!(result, Err(Failure::Infeasible));
        let after = after.unwrap();
        assert_eq!(after.prefix.first_node, 0); // The remaining conflict still propagates to node 0.
        assert_eq!(after.prefix.required, 5.);
        // Separate components prove that advancing first_node actually clears
        // the preceding component rather than just selecting a different witness.
        let disconnected = [Edge {
            a: 1,
            b: 2,
            maximum_difference: 10.,
        }];
        let (result, after) = solve_diagnosed(&nodes, &disconnected, 1000, || false);
        assert_eq!(result, Err(Failure::Infeasible));
        assert_eq!(after.unwrap().prefix.first_node, 1);
    }
    #[test]
    fn contradictions_work_bounds_and_cancel_never_return_partial_heights() {
        let nodes = [
            Node {
                desired: 0.,
                minimum: 0.,
                maximum: 0.,
            },
            Node {
                desired: 10.,
                minimum: 10.,
                maximum: 10.,
            },
        ];
        let edges = [Edge {
            a: 0,
            b: 1,
            maximum_difference: 1.,
        }];
        assert_eq!(
            solve(&nodes, &edges, 1000, || false),
            Err(Failure::Infeasible)
        );
        assert_eq!(solve(&nodes, &edges, 1, || false), Err(Failure::Budget));
        assert_eq!(
            solve(&nodes, &edges, 1000, || true),
            Err(Failure::Cancelled)
        );
    }
}
