//! Ordered horizontal/vertical clutter dilation on a private topology copy.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-clutter-topology-1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub decor_flag: u32,
    pub clutter_flag: u32,
    pub radius_cells: usize,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if self.decor_flag.count_ones() != 1
            || self.clutter_flag.count_ones() != 1
            || self.decor_flag == self.clutter_flag
            || !(1..=64).contains(&self.radius_cells)
        {
            return Err("Invalid clutter topology policy".into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub added_cells: usize,
    pub work: usize,
}

/// The vertical pass observes horizontal writes. Call once after world placement,
/// before manual topology overrides. Repeated application expands existing clutter.
pub fn apply(
    source: &[i32],
    resolution: usize,
    policy: Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    let n = resolution;
    if !(2..=2048).contains(&n) || policy.radius_cells >= n || source.len() != n * n {
        return Err("Invalid clutter topology grid".into());
    }
    let work = 6 * n * n + 2 * n;
    if work > maximum_work {
        return Err("Clutter topology exceeds its work allowance".into());
    }
    let mut topology = source.to_vec();
    Ok(dilate(&mut topology, n, policy, maximum_work, cancelled)?.map(|report| (topology, report)))
}

/// Reuses the caller's private grid. Discard that revision on cancellation.
pub(crate) fn dilate(
    topology: &mut [i32],
    resolution: usize,
    policy: Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    let n = resolution;
    if !(2..=2048).contains(&n) || policy.radius_cells >= n || topology.len() != n * n {
        return Err("Invalid clutter topology grid".into());
    }
    let work = 6 * n * n + 2 * n;
    if work > maximum_work {
        return Err("Clutter topology exceeds its work allowance".into());
    }
    let mut prefix = vec![0_u32; n + 1];
    let mask = policy.decor_flag | policy.clutter_flag;
    let mut added_cells = 0;
    for row in topology.chunks_exact_mut(n) {
        if cancelled() {
            return Ok(None);
        }
        for (i, &flags) in row.iter().enumerate() {
            prefix[i + 1] = prefix[i] + u32::from(flags as u32 & mask != 0);
        }
        for (i, flags) in row.iter_mut().enumerate() {
            let lo = i.saturating_sub(policy.radius_cells);
            let hi = (i + policy.radius_cells + 1).min(n);
            if prefix[hi] != prefix[lo] && *flags as u32 & policy.decor_flag == 0 {
                added_cells += usize::from(*flags as u32 & policy.clutter_flag == 0);
                *flags |= policy.clutter_flag as i32;
            }
        }
    }
    // Read each source row before painting the destination one radius behind.
    // Keeping the last source row per column avoids column-strided reads and
    // prevents new clutter from contributing to its own vertical pass.
    let radius = policy.radius_cells;
    let mut last_seen = vec![0_u32; n];
    for source in 0..n + radius {
        if cancelled() {
            return Ok(None);
        }
        if source < n {
            let row = &topology[source * n..(source + 1) * n];
            for (last, &flags) in last_seen.iter_mut().zip(row) {
                if flags as u32 & mask != 0 {
                    *last = source as u32 + 1;
                }
            }
        }
        if source >= radius {
            let target = source - radius;
            let first = target.saturating_sub(radius) as u32;
            let row = &mut topology[target * n..(target + 1) * n];
            for (flags, &last) in row.iter_mut().zip(&last_seen) {
                if last > first && *flags as u32 & policy.decor_flag == 0 {
                    added_cells += usize::from(*flags as u32 & policy.clutter_flag == 0);
                    *flags |= policy.clutter_flag as i32;
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Report {
        engine: VERSION,
        policy,
        added_cells,
        work,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(radius_cells: usize) -> Policy {
        Policy {
            decor_flag: 512,
            clutter_flag: 16777216,
            radius_cells,
        }
    }
    // Independent literal window traversal, preserving ordered pass visibility.
    fn reference(source: &[i32], n: usize, p: Policy) -> Vec<i32> {
        let mut result = source.to_vec();
        for vertical in [false, true] {
            let frozen = result.clone();
            for row in 0..n {
                for col in 0..n {
                    let index = row * n + col;
                    let center = if vertical { row } else { col };
                    let affected = (center.saturating_sub(p.radius_cells)
                        ..=(center + p.radius_cells).min(n - 1))
                        .any(|k| {
                            let i = if vertical { k * n + col } else { row * n + k };
                            frozen[i] as u32 & (p.decor_flag | p.clutter_flag) != 0
                        });
                    if affected && result[index] as u32 & p.decor_flag == 0 {
                        result[index] |= p.clutter_flag as i32;
                    }
                }
            }
        }
        result
    }
    #[test]
    fn matches_ordered_windows_and_preserves_every_other_bit() {
        for n in [4, 8, 17] {
            for radius in 1..n {
                for offset in 0..n {
                    let mut source = vec![i32::MIN | 1; n * n];
                    source[offset * n + (offset * 3) % n] |= 512;
                    source[(n - 1) * n] |= 16777216;
                    let p = policy(radius);
                    let (out, report) =
                        apply(&source, n, p, usize::MAX, || false).unwrap().unwrap();
                    assert_eq!(out, reference(&source, n, p));
                    assert_eq!(
                        report.added_cells,
                        source
                            .iter()
                            .zip(&out)
                            .filter(|(a, b)| {
                                **a as u32 & p.clutter_flag == 0 && **b as u32 & p.clutter_flag != 0
                            })
                            .count()
                    );
                    for (&a, &b) in source.iter().zip(&out) {
                        assert_eq!(a & !(p.clutter_flag as i32), b & !(p.clutter_flag as i32));
                        if a & 512 != 0 {
                            assert_eq!(a, b);
                        }
                    }
                    assert!(report.added_cells > 0);
                }
            }
        }
    }
    #[test]
    fn failures_and_cancellation_preserve_input() {
        let source = vec![512; 64];
        assert!(apply(&source, 8, policy(3), 1, || false).is_err());
        assert!(apply(&source, 7, policy(3), usize::MAX, || false).is_err());
        let calls = std::cell::Cell::new(0);
        let out = apply(&source, 8, policy(3), usize::MAX, || {
            calls.set(calls.get() + 1);
            calls.get() > 4
        })
        .unwrap();
        assert!(out.is_none());
        calls.set(0);
        let out = apply(&source, 8, policy(3), usize::MAX, || {
            calls.set(calls.get() + 1);
            calls.get() > 14
        })
        .unwrap();
        assert!(out.is_none());
        assert_eq!(source, vec![512; 64]);
    }
}
