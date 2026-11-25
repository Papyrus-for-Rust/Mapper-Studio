//! Installation-configured powerline terrain costs on a cell-centred lattice.
//! Routing, prefab placement and runtime wires are separate dependent stages.
use crate::{
    rail_costmap::{Rows, Terrain},
    road_costmap::CellLattice,
};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-powerline-costmap-1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub spacing: f32,
    pub radius: f32,
    pub blocked_topology: u32,
    pub expensive_topology: u32,
    pub moderate_topology: u32,
    pub expensive_cost: u32,
    pub moderate_cost: u32,
    pub normal_base: u32,
    pub slope_scale: f32,
}

impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if !self.spacing.is_finite()
            || !(0.1..=128.).contains(&self.spacing)
            || !self.radius.is_finite()
            || !(0. ..=128.).contains(&self.radius)
            || [self.expensive_cost, self.moderate_cost, self.normal_base]
                .iter()
                .any(|&v| !(1..=1_000_000).contains(&v))
            || !self.slope_scale.is_finite()
            || !(0. ..=100.).contains(&self.slope_scale)
        {
            return Err("Invalid source powerline cost policy".into());
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Counts {
    pub blocked_topology: usize,
    pub expensive_topology: usize,
    pub placement: usize,
    pub moderate_topology: usize,
    pub normal: usize,
}

#[derive(Debug, Serialize)]
pub struct Grid {
    pub engine: &'static str,
    pub lattice: CellLattice,
    /// Zero denotes blocked terrain. Every walkable cell has a positive cost.
    pub costs: Vec<u32>,
    pub counts: Counts,
    pub work: usize,
}

/// Uses the shared native-frame slope sampler and radius-query implementation.
/// Rust's normal interpolation remains an independent numeric implementation.
/// The input rasters remain immutable on failure, exhaustion and cancellation.
pub fn build(
    terrain: Terrain<'_>,
    policy: &Policy,
    maximum_work: usize,
    cancel: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    policy.validate()?;
    if !terrain.size.is_finite()
        || terrain.size.fract() != 0.
        || !(1000. ..=6000.).contains(&terrain.size)
        || !(2..=4097).contains(&terrain.height_resolution)
        || terrain.height.len() != terrain.height_resolution.pow(2)
        || !(2..=2048).contains(&terrain.topology_resolution)
        || terrain.topology.len() != terrain.topology_resolution.pow(2)
        || !(2..=2048).contains(&terrain.placement_resolution)
        || terrain.placement.len() != terrain.placement_resolution.pow(2)
        || !(1..=128_000_000).contains(&maximum_work)
    {
        return Err("Invalid powerline terrain or work allowance".into());
    }
    let lattice = CellLattice::new(terrain.size, policy.spacing)?;
    let mut work = 0usize;
    for chunk in terrain.height.chunks(4096) {
        if cancel() {
            return Ok(None);
        }
        charge(&mut work, chunk.len(), maximum_work)?;
        if chunk.iter().any(|v| !(0..=32766).contains(v)) {
            return Err("Invalid powerline height sample".into());
        }
    }
    let Some(topology) = Rows::new(
        terrain.topology_resolution,
        |i| {
            let mask = terrain.topology[i] as u32;
            [
                policy.blocked_topology,
                policy.expensive_topology,
                policy.moderate_topology,
            ]
            .map(|role| mask & role != 0)
        },
        &mut work,
        maximum_work,
        &cancel,
    )?
    else {
        return Ok(None);
    };
    let mut invalid = false;
    let Some(placement) = Rows::new(
        terrain.placement_resolution,
        |i| {
            invalid |= terrain.placement[i] > 1;
            [terrain.placement[i] != 0]
        },
        &mut work,
        maximum_work,
        &cancel,
    )?
    else {
        return Ok(None);
    };
    if invalid {
        return Err("Invalid powerline placement cell".into());
    }
    let topology = topology.with_reciprocal_radius(true);
    let placement = placement.with_reciprocal_radius(true);
    let resolution = lattice.resolution();
    let mut grid = Grid {
        engine: VERSION,
        lattice,
        costs: Vec::with_capacity(resolution * resolution),
        counts: Counts::default(),
        work: 0,
    };
    for z in 0..resolution {
        for x in 0..resolution {
            if cancel() {
                return Ok(None);
            }
            charge(&mut work, 1, maximum_work)?;
            let uv = lattice
                .normalized([x, z])
                .ok_or("Invalid powerline lattice cell")?;
            let nearby = topology.query(
                uv,
                policy.radius,
                terrain.size,
                true,
                &mut work,
                maximum_work,
            )?;
            // Earlier branches determine the result without slope interpolation.
            // The complete height input has already passed encoded-value checks.
            let cost = if nearby[0] {
                grid.counts.blocked_topology += 1;
                0
            } else if nearby[1] {
                grid.counts.expensive_topology += 1;
                policy.expensive_cost
            } else if placement.query(
                uv,
                policy.radius,
                terrain.size,
                false,
                &mut work,
                maximum_work,
            )?[0]
            {
                grid.counts.placement += 1;
                policy.expensive_cost
            } else if nearby[2] {
                grid.counts.moderate_topology += 1;
                policy.moderate_cost
            } else {
                charge(&mut work, 12, maximum_work)?;
                let slope = crate::terrain_normal::slope(
                    terrain.size as u32,
                    terrain.height_resolution,
                    terrain.height,
                    uv,
                )?;
                grid.counts.normal += 1;
                normal_cost(policy, slope)
            };
            grid.costs.push(cost);
        }
    }
    grid.work = work;
    Ok(Some(grid))
}

fn normal_cost(policy: &Policy, slope: f32) -> u32 {
    policy.normal_base + (slope * slope * policy.slope_scale) as u32
}

fn charge(work: &mut usize, amount: usize, maximum: usize) -> Result<(), String> {
    *work = work
        .checked_add(amount)
        .filter(|&v| v <= maximum)
        .ok_or("Powerline cost work allowance exhausted")?;
    Ok(())
}

#[cfg(test)]
mod tests;
