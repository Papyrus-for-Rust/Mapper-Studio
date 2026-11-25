//! Opt-in quality preparation for source rings with overlapping joined legs.
//! Selects an existing closed subwalk; never adds an edge or changes a price.
use crate::{road_costmap::CellLattice, source_road_ring::RawNode};
use serde::Serialize;
use std::collections::BTreeMap;

pub const VERSION: &str = "papyrus-source-ring-cycle-1";
#[derive(Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub input_nodes: usize,
    pub output_nodes: usize,
    pub removed_nodes: usize,
    pub signed_area_twice: i64,
    pub work: usize,
}
pub struct Selection {
    pub nodes: Vec<RawNode>,
    pub audit: Audit,
}
struct Meter<'a, F> {
    work: usize,
    limit: usize,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, amount: usize) -> Result<bool, String> {
        if (self.cancelled)() {
            return Ok(false);
        }
        if amount > self.limit.saturating_sub(self.work) {
            return Err("Ring cycle work exhausted".into());
        }
        self.work += amount;
        Ok(true)
    }
}
fn cross(a: [usize; 2], b: [usize; 2]) -> i64 {
    // CellLattice bounds each coordinate below 1024, validated before use.
    a[0] as i64 * b[1] as i64 - b[0] as i64 * a[1] as i64
}
fn orientation(a: [usize; 2], b: [usize; 2], c: [usize; 2]) -> i64 {
    cross(a, b) + cross(b, c) + cross(c, a)
}
fn on_segment(a: [usize; 2], b: [usize; 2], p: [usize; 2]) -> bool {
    (0..2).all(|axis| (a[axis].min(b[axis])..=a[axis].max(b[axis])).contains(&p[axis]))
}
fn intersect(a: [usize; 2], b: [usize; 2], c: [usize; 2], d: [usize; 2]) -> bool {
    let [ab_c, ab_d, cd_a, cd_b] = [
        orientation(a, b, c),
        orientation(a, b, d),
        orientation(c, d, a),
        orientation(c, d, b),
    ];
    (ab_c.signum() * ab_d.signum() < 0 && cd_a.signum() * cd_b.signum() < 0)
        || (ab_c == 0 && on_segment(a, b, c))
        || (ab_d == 0 && on_segment(a, b, d))
        || (cd_a == 0 && on_segment(c, d, a))
        || (cd_b == 0 && on_segment(c, d, b))
}

/// Chronological loop erasure discovers closed cycles in traversal order.
/// Keep the greatest absolute area (first on ties), then reject segment
/// self-crossings. Vertex uniqueness alone does not establish a simple cycle.
/// The caller supplies the *remaining* assembly allowance and commits only a
/// successful selection, before any child junction is indexed into the ring.
pub fn select(
    nodes: &[RawNode],
    lattice: CellLattice,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Selection>, String> {
    if !(4..=32768).contains(&nodes.len())
        || nodes.first().map(|n| n.cell) != nodes.last().map(|n| n.cell)
        || maximum_work > 128_000_000
    {
        return Err("Invalid ring cycle request".into());
    }
    let mut meter = Meter {
        work: 0,
        limit: maximum_work,
        cancelled: &cancelled,
    };
    macro_rules! spend {
        ($n:expr) => {
            if !meter.spend($n)? {
                return Ok(None);
            }
        };
    }
    // Charge ordered-map comparisons at their conservative depth, not O(1).
    let lookup = 2 * (nodes.len().ilog2() as usize + 2);
    for node in nodes {
        spend!(1);
        if lattice.normalized(node.cell).is_none() {
            return Err("Ring cycle contains an invalid lattice cell".into());
        }
    }
    let mut locations: BTreeMap<[usize; 2], usize> = BTreeMap::new();
    let mut stack: Vec<RawNode> = Vec::new();
    let mut prefix: Vec<i64> = Vec::new();
    let mut best = Vec::new();
    let mut best_area = 0i64;
    for node in nodes {
        spend!(lookup);
        if let Some(&start) = locations.get(&node.cell) {
            let area = prefix.last().copied().unwrap() - prefix[start]
                + cross(stack.last().unwrap().cell, node.cell);
            if stack.len() - start >= 3 && area.abs() > best_area.abs() {
                spend!(stack.len() - start + 1);
                best.clear();
                best.extend_from_slice(&stack[start..]);
                best.push(stack[start]);
                best_area = area;
            }
            while stack.len() > start + 1 {
                spend!(lookup);
                locations.remove(&stack.pop().unwrap().cell);
                prefix.pop();
            }
        } else {
            let area = stack.last().map_or(0, |previous| {
                prefix.last().copied().unwrap() + cross(previous.cell, node.cell)
            });
            locations.insert(node.cell, stack.len());
            stack.push(*node);
            prefix.push(area);
        }
    }
    if best.is_empty() {
        return Err("Source ring has no nonzero closed cycle".into());
    }
    // Joins can leave edges longer than one cell. Test full segments, including
    // diagonal crossings/touches and partial retracing of adjacent segments.
    for i in 0..best.len() - 1 {
        let [a, b] = [best[i].cell, best[i + 1].cell];
        let c = best[(i + 2) % (best.len() - 1)].cell;
        spend!(12);
        if orientation(a, b, c) == 0 && (on_segment(a, b, c) || on_segment(b, c, a)) {
            return Err("Source ring cycle retraces an adjacent edge".into());
        }
        for j in i + 2..best.len() - 1 {
            if i == 0 && j == best.len() - 2 {
                continue;
            }
            spend!(32);
            if intersect(a, b, best[j].cell, best[j + 1].cell) {
                return Err("Source ring cycle crosses an existing edge".into());
            }
        }
    }
    spend!(0);
    Ok(Some(Selection {
        audit: Audit {
            engine: VERSION,
            input_nodes: nodes.len(),
            output_nodes: best.len(),
            removed_nodes: nodes.len() - best.len(),
            signed_area_twice: best_area,
            work: meter.work,
        },
        nodes: best,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lattice() -> CellLattice {
        CellLattice::new(100., 10.).unwrap()
    }
    fn walk(cells: &[[usize; 2]]) -> Vec<RawNode> {
        cells
            .iter()
            .enumerate()
            .map(|(i, &cell)| RawNode {
                cell,
                cost: 100 + i as u64,
            })
            .collect()
    }
    fn choose(cells: &[[usize; 2]]) -> Selection {
        select(&walk(cells), lattice(), 10000, || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn removes_retraced_tail_and_preserves_existing_node_prices() {
        let raw = walk(&[
            [0, 0],
            [1, 0],
            [2, 0],
            [2, 1],
            [3, 1],
            [3, 2],
            [2, 2],
            [2, 1],
            [2, 0],
            [1, 0],
            [0, 0],
        ]);
        let selected = select(&raw, lattice(), 10000, || false).unwrap().unwrap();
        assert_eq!(&selected.nodes[..4], &raw[3..7]);
        assert_eq!(selected.nodes.first(), selected.nodes.last());
        assert_eq!(selected.audit.signed_area_twice, 2);
        assert_eq!(selected.audit.removed_nodes, 6);
    }
    #[test]
    fn selects_larger_cycle_and_first_on_equal_area() {
        let selected = choose(&[
            [1, 1],
            [0, 1],
            [0, 0],
            [1, 0],
            [1, 1],
            [2, 1],
            [3, 1],
            [3, 2],
            [3, 3],
            [2, 3],
            [1, 3],
            [1, 2],
            [1, 1],
        ]);
        assert_eq!(selected.audit.signed_area_twice, 8);
        assert_eq!(selected.nodes.len(), 9);
        let tied = choose(&[
            [1, 1],
            [0, 1],
            [0, 0],
            [1, 0],
            [1, 1],
            [2, 1],
            [2, 2],
            [1, 2],
            [1, 1],
        ]);
        assert_eq!(tied.nodes[1].cell, [0, 1]);
    }
    #[test]
    fn clean_ring_retains_order_and_winding() {
        let cells = [[1, 1], [2, 1], [2, 2], [1, 2], [1, 1]];
        let forward = choose(&cells);
        let reverse = choose(&cells.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(
            forward.nodes.iter().map(|n| n.cell).collect::<Vec<_>>(),
            cells
        );
        assert_eq!(
            forward.audit.signed_area_twice,
            -reverse.audit.signed_area_twice
        );
    }
    #[test]
    fn joined_long_edges_are_retained_and_checked_as_whole_segments() {
        let selected = choose(&[[1, 1], [3, 1], [3, 3], [1, 3], [1, 1]]);
        assert_eq!(selected.audit.signed_area_twice, 8);
        assert_eq!(selected.audit.removed_nodes, 0);
        let crossed = walk(&[[0, 0], [2, 2], [3, 2], [3, 0], [2, 0], [0, 2], [0, 0]]);
        assert!(
            select(&crossed, lattice(), 10000, || false)
                .err()
                .unwrap()
                .contains("crosses")
        );
        let overlapped = walk(&[[0, 0], [3, 0], [1, 0], [1, 1], [0, 1], [0, 0]]);
        assert!(
            select(&overlapped, lattice(), 10000, || false)
                .err()
                .unwrap()
                .contains("retraces")
        );
    }
    #[test]
    fn cancellation_during_selection_leaves_input_untouched() {
        let raw = walk(&[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]);
        let original = raw.clone();
        let calls = std::cell::Cell::new(0);
        let result = select(&raw, lattice(), 10000, || {
            calls.set(calls.get() + 1);
            calls.get() > raw.len() + 2
        })
        .unwrap();
        assert!(result.is_none());
        assert_eq!(raw, original);
    }
    #[test]
    fn rejects_crossed_diagonals_even_without_repeated_vertices() {
        let raw = walk(&[[0, 0], [1, 1], [2, 1], [2, 0], [1, 0], [0, 1], [0, 0]]);
        assert!(
            select(&raw, lattice(), 10000, || false)
                .err()
                .unwrap()
                .contains("crosses")
        );
    }
    #[test]
    fn invalid_degenerate_budget_and_cancellation_never_publish() {
        let raw = walk(&[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]);
        let exact = select(&raw, lattice(), 10000, || false)
            .unwrap()
            .unwrap()
            .audit
            .work;
        assert!(select(&raw, lattice(), exact - 1, || false).is_err());
        assert!(select(&raw, lattice(), exact, || false).unwrap().is_some());
        assert!(select(&raw, lattice(), 10000, || true).unwrap().is_none());
        let degenerate = walk(&[[0, 0], [1, 0], [0, 0], [1, 0], [0, 0]]);
        assert!(select(&degenerate, lattice(), 10000, || false).is_err());
        let outside = walk(&[[9, 9], [10, 9], [10, 8], [9, 9]]);
        assert!(select(&outside, lattice(), 10000, || false).is_err());
        assert!(select(&raw[..4], lattice(), 10000, || false).is_err());
    }
}
