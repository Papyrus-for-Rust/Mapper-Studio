//! Bounded utility-network layout over installation-configured terrain costs.
//! Retains source grouping and sampling order through the shared PathFinder.
use crate::{
    powerline_costmap,
    source_road_route::{self as routing, CostGrid, Outcome},
};
use serde::Serialize;
use std::collections::BTreeSet;

pub const VERSION: &str = "papyrus-powerline-layout-1";
pub const MAXIMUM_WORK: usize = 1_000_000_000;
pub const MAXIMUM_SEARCH_WORK: usize = 128_000_000;

#[derive(Clone, Copy, Debug)]
pub struct Connector {
    pub monument: u64,
    pub object_id: i64,
    pub position: [f32; 3],
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Settings {
    pub search: routing::Settings,
    pub sample_stride: usize,
    pub minimum_path_cells: usize,
    pub maximum_path_cells: usize,
    pub maximum_network_samples: usize,
    pub maximum_routes: usize,
    pub maximum_work: usize,
}
impl Settings {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=1024).contains(&self.sample_stride)
            || !(2..=262144).contains(&self.maximum_path_cells)
            || !(2..=self.maximum_path_cells).contains(&self.minimum_path_cells)
            || !(1..=4096).contains(&self.maximum_network_samples)
            || !(1..=4096).contains(&self.maximum_routes)
            || !(1..=MAXIMUM_WORK).contains(&self.maximum_work)
            || !(1..=MAXIMUM_SEARCH_WORK).contains(&self.search.maximum_work)
            || !(1..=1_000_000).contains(&self.search.maximum_expansions)
            || self.search.directional_penalty > 1_000_000
            || !(1..=4).contains(&self.search.directional_radius)
        {
            return Err("Invalid powerline layout limits".into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    Recovered { cell: [usize; 2] },
    Unreachable,
    DepthLimit,
}
#[derive(Debug, Serialize)]
pub struct Recovery {
    pub monument: u64,
    pub object_id: i64,
    pub requested: [usize; 2],
    pub state: RecoveryState,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootReason {
    NoNetwork,
    Unreachable,
    DepthLimit,
}
#[derive(Debug, Serialize)]
pub struct Root {
    pub monument: u64,
    pub reason: RootReason,
}
#[derive(Debug, Serialize)]
pub struct Link {
    pub monument: u64,
    pub route: routing::Route,
    /// The source promotes short connections but emits only sufficiently long paths.
    pub retained: bool,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub settings: Settings,
    pub recoveries: Vec<Recovery>,
    pub roots: Vec<Root>,
    pub links: Vec<Link>,
    pub sampled_network_cells: usize,
    pub work: usize,
}
#[derive(Clone, Copy)]
struct Endpoint {
    monument: u64,
    cell: [usize; 2],
}

/// Connectors retain placed-monument order and authored component order.
/// Primary road chains must contain the original routing nodes on this lattice,
/// with both procedural endpoints retained. Smoothed visual controls differ.
pub fn generate(
    grid: &powerline_costmap::Grid,
    connectors: &[Connector],
    primary_roads: &[&[[usize; 2]]],
    settings: Settings,
    cancel: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    settings.validate()?;
    let n = grid.lattice.resolution();
    if grid.engine != powerline_costmap::VERSION
        || grid.costs.len() != n * n
        || connectors.len() > 4096
        || primary_roads.len() > 256
    {
        return Err("Invalid powerline layout inputs".into());
    }
    let prices = CostGrid {
        resolution: n,
        costs: &grid.costs,
    };
    let mut report = Report {
        engine: VERSION,
        settings,
        recoveries: Vec::new(),
        roots: Vec::new(),
        links: Vec::new(),
        sampled_network_cells: 0,
        work: 0,
    };
    for chunk in grid.costs.chunks(1024) {
        if cancel() {
            return Ok(None);
        }
        charge(&mut report.work, chunk.len(), settings.maximum_work)?;
        if chunk.iter().any(|&v| v > 1_000_000) {
            return Err("Invalid powerline routing price".into());
        }
    }
    let mut identities = BTreeSet::new();
    for connector in connectors {
        if cancel() {
            return Ok(None);
        }
        charge(&mut report.work, 1, settings.maximum_work)?;
        if connector.object_id == 0
            || !identities.insert((connector.monument, connector.object_id))
            || connector
                .position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err("Invalid or duplicate powerline connector".into());
        }
    }
    let mut samples = Vec::new();
    for road in primary_roads {
        if road.len() > 1_000_000 {
            return Err("Powerline primary road exceeds its node bound".into());
        }
        for (index, &cell) in road.iter().enumerate() {
            if cancel() {
                return Ok(None);
            }
            charge(&mut report.work, 1, settings.maximum_work)?;
            if cell.iter().any(|&v| v >= n) {
                return Err("Powerline primary road uses another lattice".into());
            }
            if (index + 1) % settings.sample_stride == 0 {
                append_sample(&mut samples, cell, settings.maximum_network_samples)?;
            }
        }
    }
    let mut pending = Vec::new();
    for connector in connectors {
        if cancel() {
            return Ok(None);
        }
        let requested = grid
            .lattice
            .cell([connector.position[0], connector.position[2]])?;
        let search = search_settings(settings, report.work)?;
        let outcome = routing::recover_costs(prices, requested, None, None, search, &cancel)?;
        charge(
            &mut report.work,
            outcome_work(&outcome),
            settings.maximum_work,
        )?;
        let state = match outcome {
            Outcome::Found(route) => {
                let cell = *route
                    .cells
                    .first()
                    .ok_or("Empty powerline connector recovery")?;
                pending.push(Endpoint {
                    monument: connector.monument,
                    cell,
                });
                RecoveryState::Recovered { cell }
            }
            Outcome::Unreachable { .. } => RecoveryState::Unreachable,
            Outcome::DepthLimit { .. } => RecoveryState::DepthLimit,
            Outcome::Cancelled { .. } => return Ok(None),
            Outcome::Budget { .. } => {
                return Err("Powerline connector recovery work exhausted".into());
            }
        };
        report.recoveries.push(Recovery {
            monument: connector.monument,
            object_id: connector.object_id,
            requested,
            state,
        });
    }
    let mut connected = Vec::new();
    while !pending.is_empty() {
        if cancel() {
            return Ok(None);
        }
        let count = connected.len() + samples.len();
        if count > settings.maximum_network_samples {
            return Err("Powerline network endpoint allowance exhausted".into());
        }
        charge(
            &mut report.work,
            count + pending.len(),
            settings.maximum_work,
        )?;
        let starts: Vec<_> = connected
            .iter()
            .copied()
            .chain(samples.iter().copied())
            .collect();
        let ends: Vec<_> = pending.iter().map(|endpoint| endpoint.cell).collect();
        let (seeds, goals) = if starts.len() > ends.len() {
            (&ends, &starts)
        } else {
            (&starts, &ends)
        };
        let outcome = routing::search_costs(
            prices,
            seeds,
            goals,
            search_settings(settings, report.work)?,
            None,
            &cancel,
        )?;
        charge(
            &mut report.work,
            outcome_work(&outcome),
            settings.maximum_work,
        )?;
        let monument = match outcome {
            Outcome::Found(route) => {
                if report.links.len() == settings.maximum_routes
                    || route.cells.len() > settings.maximum_path_cells
                {
                    return Err("Powerline route output allowance exhausted".into());
                }
                let endpoint = pending
                    .iter()
                    .find(|endpoint| {
                        route.cells.first() == Some(&endpoint.cell)
                            || route.cells.last() == Some(&endpoint.cell)
                    })
                    .ok_or("Powerline route lost its monument endpoint")?;
                let monument = endpoint.monument;
                for &cell in route
                    .cells
                    .iter()
                    .skip(settings.sample_stride - 1)
                    .step_by(settings.sample_stride)
                {
                    if cancel() {
                        return Ok(None);
                    }
                    charge(&mut report.work, 1, settings.maximum_work)?;
                    append_sample(&mut samples, cell, settings.maximum_network_samples)?;
                }
                let retained = route.cells.len() >= settings.minimum_path_cells;
                report.links.push(Link {
                    monument,
                    route,
                    retained,
                });
                monument
            }
            Outcome::Unreachable { .. } | Outcome::DepthLimit { .. } => {
                let monument = pending[0].monument;
                let reason = if starts.is_empty() {
                    RootReason::NoNetwork
                } else if matches!(outcome, Outcome::DepthLimit { .. }) {
                    RootReason::DepthLimit
                } else {
                    RootReason::Unreachable
                };
                report.roots.push(Root { monument, reason });
                monument
            }
            Outcome::Cancelled { .. } => return Ok(None),
            Outcome::Budget { .. } => return Err("Powerline layout search work exhausted".into()),
        };
        charge(&mut report.work, pending.len() * 2, settings.maximum_work)?;
        connected.extend(
            pending
                .iter()
                .filter(|endpoint| endpoint.monument == monument)
                .map(|endpoint| endpoint.cell),
        );
        if connected.len() + samples.len() > settings.maximum_network_samples {
            return Err("Powerline network endpoint allowance exhausted".into());
        }
        pending.retain(|endpoint| endpoint.monument != monument);
    }
    if cancel() {
        return Ok(None);
    }
    report.sampled_network_cells = samples.len();
    Ok(Some(report))
}

fn append_sample(
    samples: &mut Vec<[usize; 2]>,
    cell: [usize; 2],
    maximum: usize,
) -> Result<(), String> {
    if samples.len() == maximum {
        return Err("Powerline network sample allowance exhausted".into());
    }
    samples.push(cell);
    Ok(())
}
fn outcome_work(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Found(route) => route.work,
        Outcome::Unreachable { work }
        | Outcome::Budget { work }
        | Outcome::DepthLimit { work }
        | Outcome::Cancelled { work } => *work,
    }
}
fn search_settings(settings: Settings, used: usize) -> Result<routing::Settings, String> {
    let remaining = settings.maximum_work.saturating_sub(used);
    if remaining == 0 {
        return Err("Powerline layout work allowance exhausted".into());
    }
    Ok(routing::Settings {
        maximum_work: settings.search.maximum_work.min(remaining),
        ..settings.search
    })
}
fn charge(work: &mut usize, amount: usize, maximum: usize) -> Result<(), String> {
    *work = work
        .checked_add(amount)
        .filter(|&v| v <= maximum)
        .ok_or("Powerline layout work allowance exhausted")?;
    Ok(())
}

#[cfg(test)]
mod tests;
