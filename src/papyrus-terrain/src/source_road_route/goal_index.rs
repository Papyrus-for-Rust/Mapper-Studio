//! Exact Chebyshev nearest-goal queries for sparse, large terrain grids.
use super::{Meter, Outcome, distance};

const LEAF: usize = 16;
struct Node {
    minimum: [usize; 2],
    maximum: [usize; 2],
    range: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}

impl Node {
    fn lower_bound(&self, p: [usize; 2]) -> usize {
        (0..2)
            .map(|i| {
                self.minimum[i]
                    .saturating_sub(p[i])
                    .max(p[i].saturating_sub(self.maximum[i]))
            })
            .max()
            .unwrap()
    }
}
pub(super) struct GoalIndex {
    points: Vec<[usize; 2]>,
    nodes: Vec<Node>,
}
impl GoalIndex {
    /// Conservative allowance for cloning, recursive bounds and bounded sorts.
    /// Callers already limit goal count to 4096.
    pub(super) fn preparation(goals: usize) -> usize {
        let levels = goals.max(1).ilog2() as usize + 1;
        goals * levels * (levels + 3)
    }
    pub(super) fn new<F: Fn() -> bool>(
        goals: &[[usize; 2]],
        meter: &mut Meter<'_, F>,
    ) -> Result<Self, Outcome> {
        meter.spend(goals.len())?;
        let mut points = goals.to_vec();
        let mut nodes = Vec::new();
        Self::build(&mut points, 0, &mut nodes, meter)?;
        Ok(Self { points, nodes })
    }
    fn build<F: Fn() -> bool>(
        points: &mut [[usize; 2]],
        offset: usize,
        nodes: &mut Vec<Node>,
        meter: &mut Meter<'_, F>,
    ) -> Result<usize, Outcome> {
        meter.spend(points.len() * 2 + 1)?;
        let mut minimum = [usize::MAX; 2];
        let mut maximum = [0; 2];
        for p in points.iter() {
            for i in 0..2 {
                minimum[i] = minimum[i].min(p[i]);
                maximum[i] = maximum[i].max(p[i]);
            }
        }
        let id = nodes.len();
        nodes.push(Node {
            minimum,
            maximum,
            range: offset..offset + points.len(),
            children: None,
        });
        if points.len() > LEAF {
            let axis = usize::from(maximum[1] - minimum[1] > maximum[0] - minimum[0]);
            meter.spend(points.len() * (points.len().ilog2() as usize + 1))?;
            points.sort_unstable_by_key(|p| (p[axis], p[axis ^ 1]));
            let split = points.len() / 2;
            let (left, right) = points.split_at_mut(split);
            let a = Self::build(left, offset, nodes, meter)?;
            let b = Self::build(right, offset + split, nodes, meter)?;
            nodes[id].children = Some([a, b]);
        }
        Ok(id)
    }
    pub(super) fn nearest<F: Fn() -> bool>(
        &self,
        p: [usize; 2],
        comparisons: &mut usize,
        meter: &mut Meter<'_, F>,
    ) -> Result<usize, Outcome> {
        let mut best = usize::MAX;
        self.visit(0, p, &mut best, comparisons, meter)?;
        Ok(best)
    }
    fn visit<F: Fn() -> bool>(
        &self,
        id: usize,
        p: [usize; 2],
        best: &mut usize,
        comparisons: &mut usize,
        meter: &mut Meter<'_, F>,
    ) -> Result<(), Outcome> {
        meter.spend(1)?;
        let node = &self.nodes[id];
        if node.lower_bound(p) >= *best {
            return Ok(());
        }
        if let Some([a, b]) = node.children {
            meter.spend(2)?;
            let order = if self.nodes[a].lower_bound(p) <= self.nodes[b].lower_bound(p) {
                [a, b]
            } else {
                [b, a]
            };
            for child in order {
                self.visit(child, p, best, comparisons, meter)?;
            }
        } else {
            meter.spend(node.range.len())?;
            *comparisons += node.range.len();
            for &goal in &self.points[node.range.clone()] {
                *best = (*best).min(distance(p, goal));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_matches_brute_force_for_clusters_duplicates_edges_and_split_boundaries() {
        let sets = [
            (0..365)
                .map(|i| [(i * 37) % 667, (i * 53) % 667])
                .collect::<Vec<_>>(),
            vec![[0, 0]; 64],
            (0..100).map(|i| [666, i * 6]).collect(),
            (0..4096).map(|i| [i % 64, i / 64]).collect(),
        ];
        for goals in sets {
            let mut meter = Meter {
                work: 0,
                limit: 10_000_000,
                cancelled: &|| false,
            };
            let index = GoalIndex::new(&goals, &mut meter).unwrap();
            assert!(meter.work <= GoalIndex::preparation(goals.len()));
            let mut comparisons = 0;
            for z in (0..=666).step_by(11) {
                for x in (0..=666).step_by(13) {
                    let expected = goals.iter().map(|&g| distance([x, z], g)).min().unwrap();
                    assert_eq!(
                        index.nearest([x, z], &mut comparisons, &mut meter).unwrap(),
                        expected
                    );
                }
            }
        }
    }
    #[test]
    fn build_and_query_obey_work_limits_and_cancellation() {
        let goals = (0..365).map(|i| [i, i]).collect::<Vec<_>>();
        let mut meter = Meter {
            work: 0,
            limit: 1,
            cancelled: &|| false,
        };
        assert!(matches!(
            GoalIndex::new(&goals, &mut meter),
            Err(Outcome::Budget { work: 0 })
        ));
        let mut meter = Meter {
            work: 0,
            limit: 1_000_000,
            cancelled: &|| true,
        };
        assert!(matches!(
            GoalIndex::new(&goals, &mut meter),
            Err(Outcome::Cancelled { work: 0 })
        ));
        let calls = std::cell::Cell::new(0);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() > 4
        };
        let mut meter = Meter {
            work: 0,
            limit: 1_000_000,
            cancelled: &cancel,
        };
        assert!(matches!(
            GoalIndex::new(&goals, &mut meter),
            Err(Outcome::Cancelled { .. })
        ));
        let mut meter = Meter {
            work: 0,
            limit: 1_000_000,
            cancelled: &|| false,
        };
        let index = GoalIndex::new(&goals, &mut meter).unwrap();
        let mut limited = Meter {
            work: 0,
            limit: 0,
            cancelled: &|| false,
        };
        assert!(matches!(
            index.nearest([2, 2], &mut 0, &mut limited),
            Err(Outcome::Budget { work: 0 })
        ));
        let mut cancelled = Meter {
            work: 0,
            limit: 1_000_000,
            cancelled: &|| true,
        };
        assert!(matches!(
            index.nearest([2, 2], &mut 0, &mut cancelled),
            Err(Outcome::Cancelled { work: 0 })
        ));
    }
}
