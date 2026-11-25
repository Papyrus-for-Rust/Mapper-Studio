//! Configurable ordered river-bank dilation with explicit callback coordinates.
use crate::rail_costmap::sample_height_slope;
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-bank-topology-1";
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlopeCoordinates {
    World,
    CallbackTransposed,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub source_mask: i32,
    pub eligible_mask: i32,
    pub bank_flag: i32,
    pub cliff_flag: i32,
    pub radius_cells: usize,
    pub cliff_slope_degrees: f32,
    pub slope_coordinates: SlopeCoordinates,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if self.source_mask <= 0
            || self.eligible_mask <= 0
            || (self.bank_flag as u32).count_ones() != 1
            || (self.cliff_flag as u32).count_ones() != 1
            || self.bank_flag == self.cliff_flag
            || self.source_mask & self.bank_flag == 0
            || self.source_mask & self.cliff_flag != 0
            || !(1..=64).contains(&self.radius_cells)
            || !self.cliff_slope_degrees.is_finite()
            || !(1. ..89.).contains(&self.cliff_slope_degrees)
        {
            return Err("Invalid bank topology policy".into());
        }
        Ok(())
    }
}
pub struct Terrain<'a> {
    pub size: f32,
    pub height_resolution: usize,
    pub surface_resolution: usize,
    pub height: &'a [i16],
    pub topology: &'a [i32],
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub bank_added: usize,
    pub cliff_added: usize,
    pub affected_cells: usize,
    pub work: usize,
}
pub fn apply(
    t: Terrain<'_>,
    p: Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    if cancelled() {
        return Ok(None);
    }
    p.validate()?;
    let n = t.surface_resolution;
    let hr = t.height_resolution;
    if !(2..=2048).contains(&n)
        || p.radius_cells >= n
        || !(2..=4097).contains(&hr)
        || hr.checked_mul(hr) != Some(t.height.len())
        || n.checked_mul(n) != Some(t.topology.len())
        || !t.size.is_finite()
        || !(1. ..=6000.).contains(&t.size)
        || !(1..=512_000_000).contains(&maximum_work)
    {
        return Err("Invalid bank topology terrain or work allowance".into());
    }
    let mut work = 0;
    for chunk in t.height.chunks(4096) {
        if cancelled() {
            return Ok(None);
        }
        if chunk.len() > maximum_work - work {
            return Err("Bank topology work budget exceeded".into());
        }
        work += chunk.len();
        if chunk.iter().any(|h| !(0..=32766).contains(h)) {
            return Err("Invalid bank topology heights".into());
        }
    }
    let result = dilate(
        t.topology,
        n,
        p,
        &mut work,
        maximum_work,
        &cancelled,
        |row, col| {
            let uv = match p.slope_coordinates {
                SlopeCoordinates::World => [col, row],
                SlopeCoordinates::CallbackTransposed => [row, col],
            }
            .map(|v| (v as f32 + 0.5) / n as f32);
            sample_height_slope(t.size, hr, t.height, uv).1 > p.cliff_slope_degrees
        },
    )?;
    Ok(result.map(|(topology, mut report)| {
        report.work = work;
        (topology, report)
    }))
}
fn dilate(
    source: &[i32],
    n: usize,
    p: Policy,
    work: &mut usize,
    limit: usize,
    cancelled: &impl Fn() -> bool,
    steep: impl Fn(usize, usize) -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    let mut topology = source.to_vec();
    let mut affected = vec![false; n * n];
    let mut prefix = vec![0u32; n + 1];
    macro_rules! visit {
        () => {
            if cancelled() {
                return Ok(None);
            }
            if *work >= limit {
                return Err("Bank topology work budget exceeded".into());
            }
            *work += 1;
        };
    }
    for vertical in [false, true] {
        for line in 0..n {
            visit!();
            let index = |k: usize| if vertical { k * n + line } else { line * n + k };
            prefix[0] = 0;
            for k in 0..n {
                visit!();
                prefix[k + 1] = prefix[k] + u32::from(topology[index(k)] & p.source_mask != 0);
            }
            for k in 0..n {
                visit!();
                let lo = k.saturating_sub(p.radius_cells);
                let hi = (k + p.radius_cells + 1).min(n);
                if prefix[hi] == prefix[lo] {
                    continue;
                }
                let i = index(k);
                if topology[i] & p.eligible_mask != 0 {
                    topology[i] |= p.bank_flag;
                }
                if !affected[i] {
                    if steep(i / n, i % n) {
                        topology[i] |= p.cliff_flag;
                    }
                    affected[i] = true;
                }
            }
        }
    }
    let mut report = Report {
        engine: VERSION,
        policy: p,
        bank_added: 0,
        cliff_added: 0,
        affected_cells: 0,
        work: *work,
    };
    for (i, (&a, &b)) in source.iter().zip(&topology).enumerate() {
        visit!();
        report.bank_added += usize::from(a & p.bank_flag == 0 && b & p.bank_flag != 0);
        report.cliff_added += usize::from(a & p.cliff_flag == 0 && b & p.cliff_flag != 0);
        report.affected_cells += usize::from(affected[i]);
    }
    Ok(Some((topology, report)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            source_mask: 49152,
            eligible_mask: 49,
            bank_flag: 32768,
            cliff_flag: 2,
            radius_cells: 6,
            cliff_slope_degrees: 40.,
            slope_coordinates: SlopeCoordinates::CallbackTransposed,
        }
    }
    #[test]
    fn ordered_mutating_dilation_matches_actual_owned_int_array_overload() {
        let r: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/river-surface-primitives-bd19c4ac.json"
        ))
        .unwrap();
        let cases = r["dilations"].as_array().unwrap();
        assert_eq!(cases.len(), 3);
        for c in cases {
            let source: Vec<i32> = serde_json::from_value(c["input"].clone()).unwrap();
            let expected: Vec<i32> = serde_json::from_value(c["expected"].clone()).unwrap();
            let mut p = policy();
            p.radius_cells = c["radius"].as_u64().unwrap() as usize;
            let n = c["size"].as_u64().unwrap() as usize;
            let (actual, report) = dilate(&source, n, p, &mut 0, 100000, &|| false, |row, col| {
                (row * 3 + col * 5) % 7 == 0
            })
            .unwrap()
            .unwrap();
            assert_eq!(actual, expected);
            assert!(report.bank_added > 0);
            assert!(report.cliff_added > 0);
        }
    }
    #[test]
    fn callback_slope_coordinates_are_explicit_and_world_mode_stays_distinct() {
        let n = 32;
        let hr = 65;
        let mut h = vec![0; hr * hr];
        let mut topo = vec![1; n * n];
        topo[24 * n + 16] = 16384;
        for z in 0..hr {
            for x in 0..hr {
                h[z * hr + x] = if x <= 32 { 10000 } else { 24000 };
            }
        }
        let terrain = || Terrain {
            size: 64.,
            height_resolution: hr,
            surface_resolution: n,
            height: &h,
            topology: &topo,
        };
        let mut p = policy();
        p.radius_cells = 2;
        let (a, ar) = apply(terrain(), p, 100000, || false).unwrap().unwrap();
        p.slope_coordinates = SlopeCoordinates::World;
        let (b, br) = apply(terrain(), p, 100000, || false).unwrap().unwrap();
        assert_eq!(ar.cliff_added, 0);
        assert!(br.cliff_added > 0);
        assert_ne!(a, b);
        assert_eq!(ar.bank_added, br.bank_added);
        let calls = std::cell::Cell::new(0);
        assert!(
            apply(terrain(), p, 100000, || {
                calls.set(calls.get() + 1);
                calls.get() > 100
            })
            .unwrap()
            .is_none()
        );
        assert!(apply(terrain(), p, 10, || false).is_err());
    }
}
