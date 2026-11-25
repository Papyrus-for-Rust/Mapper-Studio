//! Source-configured erosion floors over an immutable topology snapshot.
use crate::topology_query::{Index, Query, Tiles};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-erosion-bounds-1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub protected_topology: u32,
    pub nearby_topology: u32,
    pub maximum_radius: f32,
    pub radius_step: f32,
    pub maximum_cut: f32,
}

#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub height_resolution: usize,
    pub heights: &'a [f32],
    pub topology_resolution: usize,
    pub topology: &'a [i32],
    pub terrain_one_over_size: f32,
    pub ocean_level: f32,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub maximum_bytes: usize,
    pub maximum_work: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub cells: usize,
    pub ocean_cells: usize,
    pub protected_cells: usize,
    pub tapered_cells: usize,
    pub work: usize,
    pub peak_buffer_bytes: usize,
    pub retained_buffer_bytes: usize,
}

pub struct Output {
    pub minimum_heights: Vec<f32>,
    pub report: Report,
}

impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if !self.maximum_radius.is_finite()
            || !(0. ..=65536.).contains(&self.maximum_radius)
            || self.maximum_radius == 0.
            || !self.radius_step.is_finite()
            || self.radius_step <= 0.
            || self.radius_step > self.maximum_radius
            || self.maximum_radius - self.radius_step == self.maximum_radius
            || !self.maximum_cut.is_finite()
            || !(0. ..=1024.).contains(&self.maximum_cut)
        {
            return Err("Invalid erosion topology policy".into());
        }
        Ok(())
    }
}

/// Rectangle bounds use normalized world distance. The circle comparison uses
/// the source's raw radius against integer topology-cell offsets.
fn topology(
    input: Input<'_>,
    tiles: Tiles<'_>,
    uv: [f32; 2],
    radius: f32,
    mask: u32,
    work: &mut usize,
    limit: usize,
) -> Result<bool, String> {
    let n = input.topology_resolution;
    let index =
        |coordinate: f32| ((coordinate * n as f32) as isize).clamp(0, n as isize - 1) as usize;
    if radius == 0. {
        let center = [index(uv[0]), index(uv[1])];
        charge(work, limit)?;
        return Ok(input.topology[center[1] * n + center[0]] as u32 & mask != 0);
    }
    let query = Query::normalized(n, uv, radius, input.terrain_one_over_size, mask);
    tiles
        .union(query, work, limit)
        .map(|value| value != 0)
        .map_err(|()| "Erosion topology queries exhausted their work allowance".into())
}

fn charge(work: &mut usize, limit: usize) -> Result<(), String> {
    if *work >= limit {
        return Err("Erosion topology queries exhausted their work allowance".into());
    }
    *work += 1;
    Ok(())
}

pub fn build(
    input: Input<'_>,
    policy: Policy,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    policy.validate()?;
    let hr = input.height_resolution;
    let tr = input.topology_resolution;
    if !(3..=4097).contains(&hr)
        || !(1..=4096).contains(&tr)
        || input.heights.len() != hr * hr
        || input.topology.len() != tr * tr
        || input
            .heights
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 65536.)
        || !input.ocean_level.is_finite()
        || input.ocean_level.abs() > 65536.
        || !input.terrain_one_over_size.is_finite()
        || !(f32::MIN_POSITIVE..=1.).contains(&input.terrain_one_over_size)
    {
        return Err("Invalid erosion topology grids or world frame".into());
    }
    let retained_bytes = std::mem::size_of_val(input.heights);
    let peak_bytes = retained_bytes + Index::allocation_bytes(tr)? as usize;
    if peak_bytes > limits.maximum_bytes
        || input.topology.len() + input.heights.len() > limits.maximum_work
    {
        return Err("Erosion topology preparation exceeds its memory or work allowance".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let Some(index) = Index::new(input.topology, tr, &cancelled)? else {
        return Ok(None);
    };
    let tiles = index.view(input.topology);
    let mut minimum_heights = Vec::new();
    minimum_heights
        .try_reserve_exact(input.heights.len())
        .map_err(|error| format!("Erosion floor allocation failed: {error}"))?;
    let mut report = Report {
        engine: VERSION,
        policy,
        cells: input.heights.len(),
        ocean_cells: 0,
        protected_cells: 0,
        tapered_cells: 0,
        work: input.topology.len(),
        peak_buffer_bytes: peak_bytes,
        retained_buffer_bytes: retained_bytes,
    };
    for (i, &height) in input.heights.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        charge(&mut report.work, limits.maximum_work)?;
        if height < input.ocean_level {
            minimum_heights.push(height);
            report.ocean_cells += 1;
            continue;
        }
        let uv = [
            ((i % hr) as f32 - 0.5) / hr as f32,
            ((i / hr) as f32 - 0.5) / hr as f32,
        ];
        let protected = topology(
            input,
            tiles,
            uv,
            0.,
            policy.protected_topology,
            &mut report.work,
            limits.maximum_work,
        )?;
        let mut last_radius = policy.maximum_radius;
        if protected {
            report.protected_cells += 1;
        } else {
            let mut radius = policy.maximum_radius;
            while radius > 0. {
                // Charge the attempt even when an empty relevant mask lets the
                // indexed union return immediately.
                charge(&mut report.work, limits.maximum_work)?;
                if !topology(
                    input,
                    tiles,
                    uv,
                    radius,
                    policy.nearby_topology,
                    &mut report.work,
                    limits.maximum_work,
                )? {
                    break;
                }
                last_radius = radius;
                radius -= policy.radius_step;
            }
        }
        let factor = if protected {
            0.
        } else {
            last_radius / policy.maximum_radius
        };
        report.tapered_cells += usize::from(!protected && factor < 1.);
        minimum_heights.push((height - policy.maximum_cut * factor).max(input.ocean_level));
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Output {
        minimum_heights,
        report,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            protected_topology: 0x14080,
            nearby_topology: 0x3c198,
            maximum_radius: 8.,
            radius_step: 0.25,
            maximum_cut: 1.,
        }
    }
    fn limits() -> Limits {
        Limits {
            maximum_bytes: 1_000_000,
            maximum_work: 1_000_000,
        }
    }
    fn input<'a>(heights: &'a [f32], topology: &'a [i32]) -> Input<'a> {
        Input {
            height_resolution: 5,
            heights,
            topology_resolution: 4,
            topology,
            terrain_one_over_size: 1. / 64.,
            ocean_level: 0.,
        }
    }
    #[test]
    fn ocean_and_protected_ground_are_fixed_and_nearby_flags_taper_the_floor() {
        let mut heights = vec![10.; 25];
        heights[0] = -2.;
        heights[1] = 0.5;
        let empty = vec![0; 16];
        let bare = build(input(&heights, &empty), policy(), limits(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(bare.minimum_heights[0], -2.);
        assert_eq!(bare.minimum_heights[1], 0.);
        assert!(bare.minimum_heights[2..].iter().all(|v| *v == 9.));
        let protected = vec![0x80; 16];
        let fixed = build(input(&heights, &protected), policy(), limits(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(fixed.minimum_heights, heights);
        assert_eq!(fixed.report.protected_cells, 24);
        let nearby = vec![0x8; 16];
        let tapered = build(input(&heights, &nearby), policy(), limits(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(tapered.minimum_heights[2], 10. - 0.25 / 8.);
        assert_eq!(tapered.report.tapered_cells, 24);
        assert_eq!(tapered.report.ocean_cells, 1);
        assert_eq!(heights[0], -2.);
    }
    #[test]
    fn cancellation_and_exhausted_budgets_do_not_publish_partial_floors() {
        let heights = vec![10.; 25];
        let cells = vec![0x8; 16];
        let mut budget = limits();
        budget.maximum_work = 42;
        assert!(build(input(&heights, &cells), policy(), budget, || false).is_err());
        assert!(
            build(input(&heights, &cells), policy(), limits(), || true)
                .unwrap()
                .is_none()
        );
        budget = limits();
        budget.maximum_bytes = 100;
        assert!(build(input(&heights, &cells), policy(), budget, || false).is_err());
        let mut bad = policy();
        bad.radius_step = f32::MIN_POSITIVE;
        assert!(build(input(&heights, &cells), bad, limits(), || false).is_err());
        assert!(build(input(&heights[..24], &cells), policy(), limits(), || false).is_err());
        assert_eq!(heights, vec![10.; 25]);
    }

    #[test]
    fn indexed_floors_match_literal_radius_scans_across_grids_and_world_scales() {
        fn reference(input: Input<'_>, policy: Policy) -> Vec<f32> {
            let n = input.topology_resolution;
            let index = |v: f32| ((v * n as f32) as isize).clamp(0, n as isize - 1) as usize;
            let query = |uv: [f32; 2], radius: f32, mask: u32| {
                let xm = index(uv[0]);
                let zm = index(uv[1]);
                let d = input.terrain_one_over_size * radius;
                let mut flags = 0u32;
                for z in index(uv[1] - d)..=index(uv[1] + d) {
                    for x in index(uv[0] - d)..=index(uv[0] + d) {
                        let dx = x as i32 - xm as i32;
                        let dz = z as i32 - zm as i32;
                        if (dx * dx + dz * dz) as f32 <= radius * radius {
                            flags |= input.topology[z * n + x] as u32;
                        }
                    }
                }
                flags & mask != 0
            };
            input
                .heights
                .iter()
                .enumerate()
                .map(|(i, &h)| {
                    if h < input.ocean_level {
                        return h;
                    }
                    let hr = input.height_resolution;
                    let uv = [
                        ((i % hr) as f32 - 0.5) / hr as f32,
                        ((i / hr) as f32 - 0.5) / hr as f32,
                    ];
                    let protected = query(uv, 0., policy.protected_topology);
                    let mut last = policy.maximum_radius;
                    let mut r = policy.maximum_radius;
                    while r > 0. && !protected && query(uv, r, policy.nearby_topology) {
                        last = r;
                        r -= policy.radius_step;
                    }
                    let weight = if protected {
                        0.
                    } else {
                        last / policy.maximum_radius
                    };
                    (h - policy.maximum_cut * weight).max(input.ocean_level)
                })
                .collect()
        }
        for hr in [5, 17, 33] {
            for tr in [4, 16] {
                for size in [64., 1000., 5000.] {
                    let heights: Vec<_> = (0..hr * hr)
                        .map(|i| {
                            if i % 11 == 0 {
                                -2.
                            } else {
                                10. + (i % 7) as f32
                            }
                        })
                        .collect();
                    let cells: Vec<_> = (0..tr * tr)
                        .map(|i| 1u32.rotate_left((i * 7 + i / tr) as u32) as i32)
                        .collect();
                    let input = Input {
                        height_resolution: hr,
                        heights: &heights,
                        topology_resolution: tr,
                        topology: &cells,
                        terrain_one_over_size: 1. / size,
                        ocean_level: 0.25,
                    };
                    for p in [
                        policy(),
                        Policy {
                            protected_topology: 1 << 31,
                            nearby_topology: 0x8080_0008,
                            maximum_radius: 3.2,
                            radius_step: 0.3,
                            maximum_cut: 0.75,
                        },
                    ] {
                        let output = build(input, p, limits(), || false).unwrap().unwrap();
                        assert_eq!(output.minimum_heights, reference(input, p));
                        assert!(output.report.work <= limits().maximum_work);
                    }
                }
            }
        }
    }
}
