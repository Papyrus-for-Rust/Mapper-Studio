//! Compact equivalent of the source's repeated prefab-name resource arrays.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-path-resource-weights-1";
const MAX_TICKETS: usize = 1_000_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub maximum_tickets: usize,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION || !(1..=MAX_TICKETS).contains(&self.maximum_tickets) {
            return Err("Invalid path resource weight policy".into());
        }
        Ok(())
    }
}

/// The source's `(float)index < Count` loop rounds positive finite values up.
/// Finite zero and negative values omit the prefab from the resource array.
pub fn count_tickets(count: f32) -> Result<usize, String> {
    if !count.is_finite() || count > MAX_TICKETS as f32 {
        return Err("Invalid or excessive prefab resource count".into());
    }
    Ok(count.max(0.).ceil() as usize)
}

#[derive(Debug)]
pub struct Pool {
    /// Exclusive cumulative bounds, paired with original roster indices.
    entries: Vec<(usize, usize)>,
    total: usize,
}
impl Pool {
    pub fn new(weights: &[usize], policy: &Policy) -> Result<Self, String> {
        policy.validate()?;
        if weights.len() > 4096 {
            return Err("Path resource roster exceeds its bound".into());
        }
        let mut entries = Vec::new();
        let mut total = 0usize;
        for (index, &weight) in weights.iter().enumerate() {
            total = total
                .checked_add(weight)
                .filter(|&v| v <= policy.maximum_tickets)
                .ok_or("Path resource ticket allowance exhausted")?;
            if weight > 0 {
                entries.push((index, total));
            }
        }
        Ok(Self { entries, total })
    }
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
    pub fn total_tickets(&self) -> usize {
        self.total
    }
    /// Reuses the caller's unit sample, preserving its random-stream ownership.
    pub fn select(&self, unit: f32) -> Result<Option<usize>, String> {
        if !unit.is_finite() || !(0. ..=1.).contains(&unit) {
            return Err("Invalid path resource random sample".into());
        }
        if self.is_empty() {
            return Ok(None);
        }
        let ticket = ((unit * self.total as f32) as usize).min(self.total - 1);
        let entry = self.entries.partition_point(|&(_, end)| end <= ticket);
        Ok(Some(self.entries[entry].0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(maximum_tickets: usize) -> Policy {
        Policy {
            engine: VERSION.into(),
            maximum_tickets,
        }
    }
    #[test]
    fn compact_selection_matches_materialized_source_arrays_for_every_ticket() {
        for weights in [vec![2, 0, 4, 2, 1, 8, 8, 8], vec![0, 0], vec![1], vec![]] {
            let pool = Pool::new(&weights, &policy(1000)).unwrap();
            let expanded: Vec<_> = weights
                .iter()
                .enumerate()
                .flat_map(|(index, &count)| std::iter::repeat_n(index, count))
                .collect();
            assert_eq!(pool.total_tickets(), expanded.len());
            for sample in 0..=4096 {
                let unit = sample as f32 / 4096.;
                let expected = if expanded.is_empty() {
                    None
                } else {
                    Some(
                        expanded[((unit * expanded.len() as f32) as usize).min(expanded.len() - 1)],
                    )
                };
                assert_eq!(pool.select(unit).unwrap(), expected);
            }
        }
    }
    #[test]
    fn counts_match_source_fractional_and_disabled_cases_and_limits_are_exact() {
        assert_eq!(count_tickets(-10.).unwrap(), 0);
        assert_eq!(count_tickets(0.).unwrap(), 0);
        assert_eq!(count_tickets(0.1).unwrap(), 1);
        assert_eq!(count_tickets(2.).unwrap(), 2);
        assert_eq!(count_tickets(2.1).unwrap(), 3);
        assert!(count_tickets(f32::NAN).is_err());
        assert!(count_tickets(f32::INFINITY).is_err());
        assert!(Pool::new(&[2, 3], &policy(5)).is_ok());
        assert!(Pool::new(&[2, 3], &policy(4)).is_err());
        assert!(Pool::new(&[usize::MAX, 1], &policy(5)).is_err());
        let pool = Pool::new(&[1], &policy(1)).unwrap();
        assert!(pool.select(f32::NAN).is_err());
        assert!(pool.select(-0.1).is_err());
    }
}
