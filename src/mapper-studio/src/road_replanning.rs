//! Independent soft routing preferences derived from rejected geometry. Original
//! installation cost grids remain separate, and every final audit still applies.
use crate::road_refinement::AlignmentWitness;
use papyrus_terrain::{road_costmap::Grid, road_network::Report, source_road_route::EntranceFootprint};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub const VERSION: &str = "papyrus-road-layout-avoidance-10";
// A full installation roster can exceed 32 accepted monuments. Retain one
// preventive footprint per supported target while charging the same work budget.
pub const MAX_MARKERS: usize = papyrus_terrain::placement::MAX_TARGETS;
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Marker {
    pub witness: AlignmentWitness,
    pub radius: f32,
    pub exclude: bool,
    pub hierarchy: u32,
    /// Source connector identity in the rejected layout, never a road index in
    /// a subsequent layout. This is feedback, not permission to move a site.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<papyrus_terrain::road_network::socket_recovery::Key>,
    /// The complete rejected foreign footprint, rather than a single collision
    /// sample. Radius becomes its extra routing margin; final gates still apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obstacle: Option<papyrus_terrain::road_network::socket_recovery::BlockedObstacle>,
    /// Confirmed protected bits that no monument entrance can waive.
    #[serde(skip_serializing_if = "is_zero")]
    pub protected_topology: i32,
    /// The completed road failed dry-ground validation at this sample.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub low_ground: bool,
}
fn is_zero(value: &i32) -> bool {
    *value == 0
}
impl Marker {
    pub fn harden(&mut self) {
        self.exclude = true;
        // A sampled failure already has a road-width clearance radius. Expanding
        // that point can seal a nearby required entrance on otherwise sound
        // ground. Only a known foreign footprint has a separate extra margin.
        if self.obstacle.is_some() {
            self.radius = (self.radius * 4.).min(256.);
        }
    }
}
/// Attribute local paved failures during ordinary socket recovery. Dirt-curve
/// attribution is deferred until coupled local alignment has been exhausted.
pub fn terminal(
    report: &Report,
    road_index: usize,
    position: [f64; 2],
) -> Option<papyrus_terrain::road_network::socket_recovery::Key> {
    terminal_for_hierarchy(report, road_index, position, 1, None)
}
fn terminal_for_hierarchy(
    report: &Report,
    road_index: usize,
    position: [f64; 2],
    maximum_hierarchy: u32,
    lattice: Option<papyrus_terrain::road_costmap::CellLattice>,
) -> Option<papyrus_terrain::road_network::socket_recovery::Key> {
    use papyrus_terrain::road_network::{Destination, LaunchEngine, socket_recovery::Key};
    if report.entrance_launch?.engine != LaunchEngine::GlobalEarthwork
        || position.iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let road = report.roads.get(road_index)?;
    let family = report.families.iter().find(|f| {
        road_index >= f.first_road
            && road_index < f.first_road.checked_add(f.road_count).unwrap_or(0)
    })?;
    if family.hierarchy > maximum_hierarchy || !family.width.is_finite() || family.width <= 0. {
        return None;
    }
    [road.from, road.to]
        .into_iter()
        .zip([road.points.first()?, road.points.last()?])
        .filter_map(|(destination, p)| match destination {
            Destination::Monument {
                monument,
                connector,
            } if p.iter().all(|v| v.is_finite()) => {
                let endpoint = if let Some(lattice) = lattice {
                    lattice.point(lattice.cell([p[0], p[2]]).ok()?)?
                } else {
                    [p[0], p[2]]
                };
                Some((
                    (f64::from(endpoint[0]) - position[0])
                        .hypot(f64::from(endpoint[1]) - position[1]),
                    Key {
                        monument,
                        connector,
                    },
                ))
            }
            _ => None,
        })
        .filter(|(distance, _)| *distance <= f64::from(family.width) * 2.)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, key)| key)
}
/// After local curve recovery fails, preserve a nearby dirt entrance's owner
/// so coupled generation can assess a fresh source placement and restamp it.
/// Match the source cell centre used by routing exclusions, including failures
/// whose radius covers that cell but stops short of the authored world point.
pub(crate) fn attribute_curvature_terminals(
    report: &Report,
    lattice: papyrus_terrain::road_costmap::CellLattice,
    markers: &mut [Marker],
) {
    for marker in markers {
        if marker.hierarchy == 2
            && marker.witness.kind == crate::road_refinement::AlignmentKind::Curvature
            && marker.terminal.is_none()
        {
            marker.terminal = terminal_for_hierarchy(
                report,
                marker.witness.road,
                marker.witness.position,
                2,
                Some(lattice),
            );
        }
    }
}
pub fn marker(report: &Report, witness: AlignmentWitness) -> Result<Marker, String> {
    let family = report
        .families
        .iter()
        .find(|f| {
            witness.road >= f.first_road && witness.road < f.first_road.saturating_add(f.road_count)
        })
        .ok_or("Missing replan road family")?;
    let radius = family.width * 2.;
    if family.hierarchy > 2
        || !radius.is_finite()
        || !(0.2..=256.).contains(&radius)
        || witness.position.iter().any(|v| !v.is_finite())
    {
        return Err("Invalid road avoidance witness".into());
    }
    let terminal = matches!(
        witness.kind,
        crate::road_refinement::AlignmentKind::Clearance
            | crate::road_refinement::AlignmentKind::Curvature
    )
    .then(|| terminal(report, witness.road, witness.position))
    .flatten();
    Ok(Marker {
        witness,
        radius,
        exclude: false,
        hierarchy: family.hierarchy,
        terminal,
        obstacle: None,
        protected_topology: 0,
        low_ground: false,
    })
}
pub fn collect(markers: &mut Vec<Marker>, next: Marker) {
    // Ring-wide preventive footprints must leave observed failures a way into
    // a full queue. Keep the existing budget and protect witnessed collisions.
    let witnessed = |marker: &Marker| {
        let Some(obstacle) = &marker.obstacle else {
            return true;
        };
        let (s, c) = f64::from(obstacle.yaw).to_radians().sin_cos();
        let dx = marker.witness.position[0] - f64::from(obstacle.center[0]);
        let dz = marker.witness.position[1] - f64::from(obstacle.center[1]);
        (c * dx + s * dz).abs() <= f64::from(obstacle.extent[0] + marker.radius)
            && (-s * dx + c * dz).abs() <= f64::from(obstacle.extent[1] + marker.radius)
    };
    if let Some(index) = markers.iter().position(|m| {
        if m.hierarchy != next.hierarchy
            || m.terminal != next.terminal
            || m.obstacle != next.obstacle
            || m.protected_topology != next.protected_topology
            || m.low_ground != next.low_ground
            || m.witness.kind != next.witness.kind
        {
            return false;
        }
        if m.obstacle.is_some() {
            return true;
        }
        let distance = (m.witness.position[0] - next.witness.position[0])
            .hypot(m.witness.position[1] - next.witness.position[1]);
        if m.exclude && next.exclude {
            // Confirmed terrain patches are redundant only when one covers the other.
            distance <= (f64::from(m.radius) - f64::from(next.radius)).abs()
        } else {
            distance <= f64::from(m.radius.min(next.radius)) * 0.5
        }
    }) {
        let current = &markers[index];
        let next_priority = (witnessed(&next), next.exclude);
        let current_priority = (witnessed(current), current.exclude);
        if next_priority > current_priority
            || (next_priority == current_priority && next.exclude && next.radius > current.radius)
        {
            markers[index] = next;
        }
        return;
    }
    if markers.len() < MAX_MARKERS {
        markers.push(next);
        return;
    }
    if witnessed(&next) {
        let replace = markers.iter().rposition(|marker| !witnessed(marker));
        let replace = replace.or_else(|| {
            next.exclude
                .then(|| markers.iter().rposition(|marker| !marker.exclude))
                .flatten()
        });
        if let Some(index) = replace {
            markers[index] = next;
        }
    }
}
/// Batch foreign footprints across the rejected network. A rebuilt ring can
/// detour into previously distant monuments, so include its known obstacles.
/// Retained controls provide feedback for the other road families.
pub fn network_obstacles(
    report: &Report,
    paths: &[papyrus_terrain::roadside_monuments::Road<'_>],
    marker: Marker,
    obstacles: &[papyrus_terrain::road_network::Obstacle],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<Marker>, usize)>, String> {
    network_obstacles_impl(
        report,
        paths,
        marker,
        obstacles,
        maximum_work,
        false,
        cancelled,
    )
}
/// Source bootstrap layouts can take new routes far from every rejected path.
/// Include distant footprints for paved and dirt families as well. The shared
/// marker allowance and witnessed-footprint retention still apply.
pub fn network_obstacles_all_families(
    report: &Report,
    paths: &[papyrus_terrain::roadside_monuments::Road<'_>],
    marker: Marker,
    obstacles: &[papyrus_terrain::road_network::Obstacle],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<Marker>, usize)>, String> {
    network_obstacles_impl(
        report,
        paths,
        marker,
        obstacles,
        maximum_work,
        true,
        cancelled,
    )
}
fn network_obstacles_impl(
    report: &Report,
    paths: &[papyrus_terrain::roadside_monuments::Road<'_>],
    mut marker: Marker,
    obstacles: &[papyrus_terrain::road_network::Obstacle],
    maximum_work: usize,
    all_families: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<Marker>, usize)>, String> {
    use papyrus_terrain::road_network::{Destination, socket_recovery::BlockedObstacle};
    if cancelled() {
        return Ok(None);
    }
    if paths.len() != report.roads.len() || paths.len() > 256 {
        return Err("Invalid network obstacle feedback".into());
    }
    let controls = paths
        .iter()
        .try_fold(0usize, |n, p| n.checked_add(p.path.points().len() + 1))
        .ok_or("Network feedback controls overflow")?;
    let work = obstacles
        .len()
        .checked_mul(controls)
        .filter(|&w| w <= maximum_work)
        .ok_or("Network obstacle feedback work exhausted")?;
    // A witnessed foreign footprint is a forbidden region, not merely costly
    // terrain. Keep exclusions scoped to that road family; the original source
    // grid and the final geometric checks remain independent.
    marker.exclude |= marker.obstacle.is_some();
    let mut markers = vec![marker];
    for (road, (source, path)) in report.roads.iter().zip(paths).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let points = path.path.points();
        let mut template = self::marker(
            report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Clearance,
                road,
                position: [f64::from(points[0][0]), f64::from(points[0][2])],
            },
        )?;
        if template.hierarchy != u32::from(path.hierarchy) {
            return Err("Network feedback family differs".into());
        }
        // A foreign-footprint prediction must never request movement of this
        // road's own entrance or treat its own monument as a foreign blocker.
        template.terminal = None;
        let own = [source.from, source.to].map(|d| match d {
            Destination::Monument { monument, .. } => Some(monument),
            _ => None,
        });
        for o in obstacles {
            if cancelled() {
                return Ok(None);
            }
            if !o.yaw.is_finite()
                || o.center.iter().any(|v| !v.is_finite())
                || o.extent.iter().any(|v| !v.is_finite() || *v < 0.)
            {
                return Err("Invalid network obstacle footprint".into());
            }
            if own.contains(&Some(o.monument)) {
                continue;
            }
            if all_families || template.hierarchy == 0 {
                let mut next = template.clone();
                next.obstacle = Some(BlockedObstacle::from(o));
                next.exclude = true;
                collect(&mut markers, next);
                continue;
            }
            let (s, c) = o.yaw.to_radians().sin_cos();
            for (i, p) in points.iter().enumerate() {
                if i % 256 == 0 && cancelled() {
                    return Ok(None);
                }
                let dx = p[0] - o.center[0];
                let dz = p[2] - o.center[1];
                if (c * dx + s * dz).abs() <= o.extent[0] + template.radius
                    && (-s * dx + c * dz).abs() <= o.extent[1] + template.radius
                {
                    let mut next = template.clone();
                    next.witness.position = [f64::from(p[0]), f64::from(p[2])];
                    next.obstacle = Some(BlockedObstacle::from(o));
                    next.exclude = true;
                    collect(&mut markers, next);
                    break;
                }
            }
        }
    }
    Ok(Some((markers, work)))
}
pub struct Applied {
    pub original: Grid,
    pub report: Value,
    /// Owner-independent terrain exclusions, including source-blocked cells.
    pub entrance_exclusions: Vec<u8>,
    /// Authored exits may enter their own footprint but must avoid foreign ones.
    pub entrance_footprints: Option<Vec<EntranceFootprint>>,
}
#[cfg(test)]
fn apply(
    grid: &mut Grid,
    size: u32,
    markers: &[Marker],
    penalty: u32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Applied>, String> {
    apply_for(grid, size, markers, penalty, maximum_work, None, cancelled)
}
pub fn apply_for(
    grid: &mut Grid,
    size: u32,
    markers: &[Marker],
    penalty: u32,
    maximum_work: usize,
    hierarchy: Option<u32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Applied>, String> {
    apply_registered(
        grid,
        size,
        markers,
        penalty,
        maximum_work,
        hierarchy,
        None,
        None,
        cancelled,
    )
}
/// Source cost maps sample cell centres, not the endpoints of a terrain grid.
#[allow(clippy::too_many_arguments)]
pub fn apply_cell_for(
    grid: &mut papyrus_terrain::road_costmap::CellGrid,
    markers: &[Marker],
    penalty: u32,
    maximum_work: usize,
    hierarchy: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Applied>, String> {
    apply_cell_with_entrances(grid, markers, penalty, maximum_work, hierarchy, None, cancelled)
}

pub fn apply_cell_with_entrances(
    grid: &mut papyrus_terrain::road_costmap::CellGrid,
    markers: &[Marker],
    penalty: u32,
    maximum_work: usize,
    hierarchy: u32,
    entrance_margin: Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Applied>, String> {
    let lattice = grid.lattice;
    if grid.grid.engine != papyrus_terrain::road_costmap::CELL_VERSION
        || grid.grid.resolution != lattice.resolution()
        || lattice.size().fract() != 0.
    {
        return Err("Invalid source routing preference lattice".into());
    }
    apply_registered(
        &mut grid.grid,
        lattice.size() as u32,
        markers,
        penalty,
        maximum_work,
        Some(hierarchy),
        Some(lattice),
        entrance_margin,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn apply_registered(
    grid: &mut Grid,
    size: u32,
    markers: &[Marker],
    penalty: u32,
    maximum_work: usize,
    hierarchy: Option<u32>,
    lattice: Option<papyrus_terrain::road_costmap::CellLattice>,
    entrance_margin: Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Applied>, String> {
    if cancelled() {
        return Ok(None);
    }
    let n = grid.resolution;
    if !(2..=if lattice.is_some() { 1024 } else { 513 }).contains(&n)
        || grid.costs.len() != n * n
        || !(1000..=6000).contains(&size)
        || markers.is_empty()
        || markers.len() > MAX_MARKERS
        || entrance_margin.is_some_and(|margin| !(0. ..=256.).contains(&margin))
        || !(1..=1_000_000).contains(&penalty)
        || grid.costs.iter().any(|&v| v > 1_000_000)
    {
        return Err("Invalid road routing preference input".into());
    }
    for marker in markers {
        if marker.hierarchy > 2
            || !marker.radius.is_finite()
            || !(0.2..=256.).contains(&marker.radius)
            || marker
                .witness
                .position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > f64::from(size) * 0.5)
            || marker.obstacle.as_ref().is_some_and(|o| {
                !o.yaw.is_finite()
                    || o.center
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > size as f32)
                    || o.extent
                        .iter()
                        .any(|v| !v.is_finite() || *v < 0. || *v > size as f32)
            })
        {
            return Err("Invalid road routing preference bounds".into());
        }
    }
    let raster_entrances = entrance_margin.is_some() && markers.iter().any(|marker| {
        marker.exclude && Some(marker.hierarchy) == hierarchy && marker.obstacle.is_some()
    });
    let work = grid
        .costs
        .len()
        .checked_mul(markers.len() + 6 + usize::from(raster_entrances))
        .filter(|&v| v <= maximum_work)
        .ok_or("Road routing preference work exhausted")?;
    let original = grid.clone();
    let axis = (0..n)
        .map(|i| {
            if let Some(lattice) = lattice {
                lattice
                    .point([i, 0])
                    .map(|p| f64::from(p[0]))
                    .ok_or("Invalid source routing cell")
            } else {
                Ok(i as f64 / (n - 1) as f64 * f64::from(size) - f64::from(size) * 0.5)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let axis_range = |center: f64, extent: f64| {
        // Round outward before the exact circle or rotated rectangle test.
        let extent = extent + 1e-8;
        axis.partition_point(|&v| v < center - extent)
            ..axis.partition_point(|&v| v <= center + extent)
    };
    let mut costs = grid.costs.clone();
    let mut exclusions = vec![0; costs.len()];
    let mut footprints = raster_entrances.then(|| vec![EntranceFootprint::Clear; costs.len()]);
    for marker in markers {
        let radius = f64::from(marker.radius);
        let region = marker.obstacle.as_ref().map(|obstacle| {
            let (s, c) = f64::from(obstacle.yaw).to_radians().sin_cos();
            let extent = obstacle.extent.map(|v| f64::from(v) + radius);
            (obstacle.center.map(f64::from), extent, s, c)
        });
        let (center, bounds) = if let Some((center, [x, z], s, c)) = region {
            (
                center,
                [x * c.abs() + z * s.abs(), x * s.abs() + z * c.abs()],
            )
        } else {
            (marker.witness.position, [radius; 2])
        };
        let columns = axis_range(center[0], bounds[0]);
        let exclude = marker.exclude && hierarchy.is_none_or(|h| h == marker.hierarchy);
        for z in axis_range(center[1], bounds[1]) {
            if cancelled() {
                return Ok(None);
            }
            let dz = axis[z] - center[1];
            for x in columns.clone() {
                let dx = axis[x] - center[0];
                let covered = if let Some((_, extent, s, c)) = region {
                    (c * dx + s * dz).abs() <= extent[0] && (-s * dx + c * dz).abs() <= extent[1]
                } else {
                    dx.hypot(dz) <= radius
                };
                if !covered {
                    continue;
                }
                let i = z * n + x;
                if exclude {
                    // An exact terrain exclusion also blocks entrance recovery.
                    exclusions[i] |= u8::from(region.is_none());
                    if let (Some(footprints), Some(margin), Some(obstacle), Some((_, _, s, c))) =
                        (&mut footprints, entrance_margin, &marker.obstacle, region)
                    {
                        let margin = f64::from(margin);
                        if (c * dx + s * dz).abs() <= f64::from(obstacle.extent[0]) + margin
                            && (-s * dx + c * dz).abs() <= f64::from(obstacle.extent[1]) + margin
                        {
                            let cell = &mut footprints[i];
                            let owner = EntranceFootprint::Monument(obstacle.monument);
                            *cell = if *cell == EntranceFootprint::Clear || *cell == owner {
                                owner
                            } else {
                                EntranceFootprint::Overlap
                            };
                        }
                    }
                    costs[i] = 0;
                } else if costs[i] != 0 {
                    costs[i] = costs[i].saturating_add(penalty).min(1_000_000);
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let changed = costs
        .iter()
        .zip(&original.costs)
        .filter(|(cost, before)| cost != before)
        .count();
    let hash = |costs: &[u32]| {
        let mut h = Sha256::new();
        for c in costs {
            h.update(c.to_le_bytes());
        }
        format!("{:x}", h.finalize())
    };
    let report = json!({"engine":VERSION,"hierarchy":hierarchy,"markers":markers,"penalty":penalty,"changed_cells":changed,"work":work,"source_costs_sha256":hash(&original.costs),"routing_costs_sha256":hash(&costs),"original_zero_cells_preserved":true,"planning_only":true,"road_ids_refer_to_rejected_layout":true});
    let mut report = report;
    report["entrance_exclusion_cells"] = json!(exclusions.iter().filter(|&&v| v != 0).count());
    if let Some(lattice) = lattice {
        report["cell_lattice"] = json!(lattice);
    }
    grid.costs = costs;
    Ok(Some(Applied {
        original,
        report,
        entrance_exclusions: exclusions,
        entrance_footprints: footprints,
    }))
}
pub fn attach(report: &mut Value, applied: Option<Applied>) {
    if let Some(applied) = applied {
        report["routing_grid"] = report["grid"].take();
        report["grid"] = json!(applied.original);
        report["routing_preferences"] = applied.report;
    }
}
#[derive(Debug, Serialize)]
pub struct Connections {
    sockets: std::collections::BTreeSet<(i32, u32, u64, i64)>,
    main_loops: usize,
}
#[derive(Debug, Serialize)]
pub struct MissingConnections {
    pub sockets: Vec<(i32, u32, u64, i64)>,
    pub main_loops: usize,
}
impl MissingConnections {
    pub fn is_empty(&self) -> bool {
        self.sockets.is_empty() && self.main_loops == 0
    }
}
impl Connections {
    pub fn capture(report: &Report) -> Result<Self, String> {
        use papyrus_terrain::road_network::Destination;
        let mut sockets = std::collections::BTreeSet::new();
        let mut main_loops = 0;
        for family in &report.families {
            let end = family
                .first_road
                .checked_add(family.road_count)
                .ok_or("Invalid required road family")?;
            let roads = report
                .roads
                .get(family.first_road..end)
                .ok_or("Missing required road family")?;
            for road in roads {
                if family.hierarchy == 0
                    && road.from == Destination::Loop
                    && road.to == Destination::Loop
                {
                    main_loops += 1;
                }
                for d in [road.from, road.to] {
                    if let Destination::Monument {
                        monument,
                        connector,
                    } = d
                    {
                        sockets.insert((
                            family.infrastructure_type,
                            family.hierarchy,
                            monument,
                            connector,
                        ));
                    }
                }
            }
        }
        Ok(Self {
            sockets,
            main_loops,
        })
    }
    #[cfg(test)]
    fn preserved(&self, report: &Report) -> Result<bool, String> {
        Ok(self.missing(report)?.is_empty())
    }
    pub fn missing(&self, report: &Report) -> Result<MissingConnections, String> {
        let other = Self::capture(report)?;
        Ok(MissingConnections {
            main_loops: self.main_loops.saturating_sub(other.main_loops),
            sockets: self.sockets.difference(&other.sockets).copied().collect(),
        })
    }
    /// Keep the actual planning failure without copying geometry or cost rasters
    /// into an error. Losing a ring can also disconnect otherwise valid sockets.
    pub fn rejection(&self, report: &Report) -> Result<Option<Value>, String> {
        let missing = self.missing(report)?;
        if missing.is_empty() {
            return Ok(None);
        }
        Ok(Some(json!({
            "missing": missing,
            "ring": report.ring,
            "families": report.families,
            "budget_exhausted": report.budget_exhausted,
            "rejection_reasons": report.rejection_reasons,
            "missing_metadata": report.missing_metadata,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road_refinement::AlignmentKind;
    fn entrance_report() -> Report {
        use papyrus_terrain::road_network::{
            Destination, Family, LaunchEngine, LaunchPolicy, Road,
        };
        let mut report = Report {
            entrance_launch: Some(LaunchPolicy {
                engine: LaunchEngine::GlobalEarthwork,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 1_000_000,
            }),
            ..Default::default()
        };
        report.roads.push(Road {
            from: Destination::Network { road: 1, point: 0 },
            to: Destination::Monument {
                monument: 14,
                connector: 75944,
            },
            points: vec![[100., 10., 100.], [-163.6217, 4.14859, -1606.0309]],
        });
        report.families.push(Family {
            infrastructure_type: 0,
            hierarchy: 1,
            seed: 1,
            first_road: 0,
            road_count: 1,
            socket_count: 1,
            unresolved: vec![],
            width: 12.,
            splat_channel: 7,
        });
        report
    }
    #[test]
    fn harbor_clearance_feedback_retains_connector_but_grade_and_distant_failures_do_not() {
        use papyrus_terrain::road_network::socket_recovery::Key;
        let mut report = entrance_report();
        // The captured harbor witness is close to the actual authored terminal.
        let witness = AlignmentWitness {
            kind: AlignmentKind::Clearance,
            road: 0,
            position: [-165.4161090787145, -1607.9873401286495],
        };
        let feedback = marker(&report, witness).unwrap();
        let curvature = marker(
            &report,
            AlignmentWitness {
                kind: AlignmentKind::Curvature,
                ..witness
            },
        )
        .unwrap();
        assert_eq!(curvature.terminal, feedback.terminal);
        assert_eq!(
            feedback.terminal,
            Some(Key {
                monument: 14,
                connector: 75944
            })
        );
        let serialized = serde_json::to_value(&feedback).unwrap();
        assert_eq!(serialized["terminal"]["connector"], 75944);
        assert!(
            marker(
                &report,
                AlignmentWitness {
                    kind: AlignmentKind::Grade,
                    ..witness
                }
            )
            .unwrap()
            .terminal
            .is_none()
        );
        assert!(terminal(&report, 0, [0.; 2]).is_none());
        assert!(terminal(&report, 0, [f64::NAN, 0.]).is_none());
        assert!(terminal(&report, usize::MAX, witness.position).is_none());
        report.families[0].hierarchy = 2;
        assert!(marker(&report, witness).unwrap().terminal.is_none());
        assert!(
            marker(
                &report,
                AlignmentWitness {
                    kind: AlignmentKind::Curvature,
                    ..witness
                }
            )
            .unwrap()
            .terminal
            .is_none()
        );
        report.families[0].hierarchy = 1;
        report.families[0].width = f32::NAN;
        assert!(terminal(&report, 0, witness.position).is_none());
        report.families[0].width = 12.;
        report.entrance_launch = None;
        assert!(terminal(&report, 0, witness.position).is_none());
    }
    #[test]
    fn exhausted_dirt_curvature_preserves_satellite_entrance_identity_only_near_its_endpoint() {
        use papyrus_terrain::road_network::{Destination, socket_recovery::Key};
        let mut report = entrance_report();
        report.families[0].hierarchy = 2;
        report.families[0].width = 4.;
        report.roads[0].to = Destination::Monument {
            monument: 42,
            connector: 45533,
        };
        *report.roads[0].points.last_mut().unwrap() = [1150.1284, 41.85389, 354.08014];
        let witness = AlignmentWitness {
            kind: AlignmentKind::Curvature,
            road: 0,
            position: [1148.5133056640625, 359.9344482421875],
        };
        let mut markers = vec![
            marker(&report, witness).unwrap(),
            marker(
                &report,
                AlignmentWitness {
                    position: [1168.6019, 376.0613],
                    ..witness
                },
            )
            .unwrap(),
            marker(
                &report,
                AlignmentWitness {
                    kind: AlignmentKind::Clearance,
                    ..witness
                },
            )
            .unwrap(),
            marker(
                &report,
                AlignmentWitness {
                    kind: AlignmentKind::Grade,
                    ..witness
                },
            )
            .unwrap(),
        ];
        markers.insert(
            1,
            marker(
                &report,
                AlignmentWitness {
                    position: [1153.427490234375, 363.15106201171875],
                    ..witness
                },
            )
            .unwrap(),
        );
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(5000., 7.5).unwrap();
        assert!(markers.iter().all(|m| m.terminal.is_none()));
        let before = report.json();
        attribute_curvature_terminals(&report, lattice, &mut markers);
        assert_eq!(
            markers[0].terminal,
            Some(Key {
                monument: 42,
                connector: 45533
            })
        );
        assert_eq!(markers[1].terminal, markers[0].terminal);
        assert!(markers[2..].iter().all(|m| m.terminal.is_none()));
        assert!(markers.iter().all(|m| !m.exclude));
        assert!(terminal(&report, 0, witness.position).is_none());
        assert_eq!(report.json(), before);
        report.entrance_launch = None;
        markers[0].terminal = None;
        attribute_curvature_terminals(&report, lattice, &mut markers);
        assert!(markers[0].terminal.is_none());
    }
    #[test]
    fn nearby_terminal_failures_are_not_merged_across_different_connectors() {
        use papyrus_terrain::road_network::socket_recovery::Key;
        let report = entrance_report();
        let first = marker(
            &report,
            AlignmentWitness {
                kind: AlignmentKind::Clearance,
                road: 0,
                position: [-165., -1608.],
            },
        )
        .unwrap();
        let mut other = first.clone();
        other.terminal = Some(Key {
            monument: 14,
            connector: 75950,
        });
        let mut markers = Vec::new();
        collect(&mut markers, first.clone());
        collect(&mut markers, first);
        collect(&mut markers, other);
        assert_eq!(markers.len(), 2);
    }
    fn fixture() -> Grid {
        let mut costs = vec![101; 81];
        costs[0] = 0;
        Grid {
            engine: papyrus_terrain::road_costmap::VERSION,
            resolution: 9,
            seed: 123,
            trail: false,
            counts: papyrus_terrain::road_costmap::Counts {
                blocked_topology: 1,
                normal: 80,
                ..Default::default()
            },
            work: 0,
            costs,
        }
    }
    fn preference() -> Marker {
        Marker {
            witness: AlignmentWitness {
                kind: AlignmentKind::Grade,
                road: 0,
                position: [0.; 2],
            },
            radius: 200.,
            exclude: false,
            hierarchy: 0,
            terminal: None,
            obstacle: None,
            protected_topology: 0,
            low_ground: false,
        }
    }
    #[test]
    fn confirmed_ground_feedback_promotes_penalties_and_keeps_new_coverage() {
        let mut soft = preference();
        soft.hierarchy = 2;
        soft.witness.kind = AlignmentKind::Clearance;
        soft.radius = 185.;
        let mut confirmed = soft.clone();
        confirmed.exclude = true;
        let mut shifted = confirmed.clone();
        shifted.witness.position = [75., 0.];

        let mut markers = vec![soft.clone()];
        collect(&mut markers, confirmed.clone());
        collect(&mut markers, soft);
        assert_eq!(markers, [confirmed.clone()]);
        collect(&mut markers, shifted.clone());
        collect(&mut markers, shifted);
        assert_eq!(markers.len(), 2);

        let mut grid = fixture();
        let applied = apply_for(&mut grid, 1000, &markers, 5000, 1000, Some(2), || false)
            .unwrap()
            .unwrap();
        for index in [4 * 9 + 3, 4 * 9 + 6] {
            assert_eq!(grid.costs[index], 0);
            assert_eq!(applied.entrance_exclusions[index], 1);
            assert_eq!(applied.original.costs[index], 101);
        }

        let mut covered = confirmed.clone();
        covered.radius = 190.;
        collect(&mut markers, covered.clone());
        collect(&mut markers, confirmed);
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0], covered);
    }

    #[test]
    fn confirmed_ground_can_replace_a_penalty_in_a_full_feedback_buffer() {
        let mut soft = preference();
        soft.radius = 20.;
        let mut markers = (0..MAX_MARKERS)
            .map(|index| {
                let mut marker = soft.clone();
                marker.witness.position[0] = index as f64;
                marker
            })
            .collect::<Vec<_>>();
        let mut confirmed = soft;
        confirmed.hierarchy = 2;
        confirmed.witness.kind = AlignmentKind::Clearance;
        confirmed.exclude = true;
        collect(&mut markers, confirmed.clone());
        assert_eq!(markers.len(), MAX_MARKERS);
        assert_eq!(markers.last(), Some(&confirmed));
    }

    #[test]
    fn clipped_preferences_match_full_grid_scan() {
        use papyrus_terrain::{
            road_costmap::CellLattice, road_network::socket_recovery::BlockedObstacle,
        };
        let mut markers = vec![
            preference(),
            Marker {
                exclude: true,
                hierarchy: 2,
                ..preference()
            },
            Marker {
                exclude: true,
                hierarchy: 1,
                obstacle: Some(BlockedObstacle {
                    monument: 15,
                    center: [500., -410.],
                    extent: [7., 83.],
                    yaw: 47.,
                }),
                ..preference()
            },
        ];
        markers[1].witness.position = [75., -90.];
        markers[1].radius = 90.;
        markers[2].radius = 11.;
        let size = 1000;
        let resolution = 100;
        for lattice in [None, Some(CellLattice::new(size as f32, 10.).unwrap())] {
            let mut grid = Grid {
                resolution,
                costs: (0..resolution * resolution)
                    .map(|i| [0, 250, 999_998][i % 3])
                    .collect(),
                ..fixture()
            };
            for hierarchy in [None, Some(1), Some(2)] {
                let (mut expected, mut exclusions) =
                    (grid.costs.clone(), vec![0; grid.costs.len()]);
                for (i, cost) in expected.iter_mut().enumerate() {
                    let cell = [i % resolution, i / resolution];
                    let point = if let Some(lattice) = lattice {
                        lattice.point(cell).unwrap().map(f64::from)
                    } else {
                        cell.map(|v| v as f64 / (resolution - 1) as f64 * 1000. - 500.)
                    };
                    for marker in &markers {
                        let radius = f64::from(marker.radius);
                        let covered = if let Some(obstacle) = &marker.obstacle {
                            let [dx, dz] = std::array::from_fn(|axis| {
                                point[axis] - f64::from(obstacle.center[axis])
                            });
                            let (s, c) = f64::from(obstacle.yaw).to_radians().sin_cos();
                            (c * dx + s * dz).abs() <= f64::from(obstacle.extent[0]) + radius
                                && (-s * dx + c * dz).abs()
                                    <= f64::from(obstacle.extent[1]) + radius
                        } else {
                            (point[0] - marker.witness.position[0])
                                .hypot(point[1] - marker.witness.position[1])
                                <= radius
                        };
                        if covered {
                            if marker.exclude && hierarchy.is_none_or(|h| h == marker.hierarchy) {
                                *cost = 0;
                                if marker.obstacle.is_none() {
                                    exclusions[i] = 1;
                                    break;
                                }
                            } else if *cost != 0 {
                                *cost = (*cost + 10).min(1_000_000);
                            }
                        }
                    }
                }
                let mut actual = grid.clone();
                let applied = apply_registered(
                    &mut actual,
                    size,
                    &markers,
                    10,
                    100_000,
                    hierarchy,
                    lattice,
                    None,
                    || false,
                )
                .unwrap()
                .unwrap();
                assert_eq!(actual.costs, expected);
                assert_eq!(applied.entrance_exclusions, exclusions);
                assert_eq!(applied.original.costs, grid.costs);
            }
            assert!(
                apply_registered(
                    &mut grid,
                    size,
                    &markers,
                    10,
                    100_000,
                    None,
                    lattice,
                    None,
                    || true,
                )
                .unwrap()
                .is_none()
            );
        }
    }
    #[test]
    fn final_point_exclusion_keeps_the_silo_dirt_entrance_recoverable() {
        use papyrus_terrain::road_costmap::{self, CellGrid, CellLattice};
        let lattice = CellLattice::new(5000., 7.5).unwrap();
        let n = lattice.resolution();
        let entrance = 328 * n + 169;
        let mut grid = CellGrid {
            lattice,
            grid: Grid {
                engine: road_costmap::CELL_VERSION,
                resolution: n,
                seed: 13,
                trail: true,
                counts: Default::default(),
                work: 0,
                costs: vec![101; n * n],
            },
        };
        grid.grid.costs[entrance] = 0;
        let mut point = preference();
        point.witness = AlignmentWitness {
            kind: AlignmentKind::Clearance,
            road: 61,
            position: [-1226.94873046875, -5.042612552642822],
        };
        point.hierarchy = 2;
        point.radius = 8.;
        let mut expanded = point.clone();
        expanded.exclude = true;
        expanded.radius *= 4.;
        let old = apply_cell_for(&mut grid.clone(), &[expanded], 5000, 8_000_000, 2, || false)
            .unwrap()
            .unwrap();
        assert_eq!(old.entrance_exclusions[entrance], 1);
        point.harden();
        assert!(point.exclude);
        assert_eq!(point.radius, 8.);
        let actual = apply_cell_for(&mut grid, &[point.clone()], 5000, 8_000_000, 2, || false)
            .unwrap()
            .unwrap();
        assert_eq!(actual.entrance_exclusions[entrance], 0);
        assert_eq!(grid.grid.costs[entrance], 0);
        let witness_cell = lattice
            .cell(point.witness.position.map(|v| v as f32))
            .unwrap();
        assert_eq!(
            actual.entrance_exclusions[witness_cell[1] * n + witness_cell[0]],
            1
        );
        assert_eq!(grid.grid.costs[witness_cell[1] * n + witness_cell[0]], 0);
    }
    #[test]
    fn footprint_raster_tracks_overlap_and_keeps_routing_margin_out_of_entrances() {
        use papyrus_terrain::{
            road_costmap::{CELL_VERSION, CellGrid, CellLattice},
            road_network::socket_recovery::BlockedObstacle,
        };
        let lattice = CellLattice::new(1000., 7.5).unwrap();
        let n = lattice.resolution();
        let mut grid = CellGrid { lattice, grid: fixture() };
        grid.grid.engine = CELL_VERSION;
        grid.grid.resolution = n;
        grid.grid.costs = vec![101; n * n];
        let mut first = preference();
        first.hierarchy = 2;
        first.exclude = true;
        first.radius = 30.;
        first.obstacle = Some(BlockedObstacle {
            monument: 71,
            center: lattice.point([50, 50]).unwrap(),
            extent: [5.; 2],
            yaw: 0.,
        });
        let mut second = first.clone();
        let obstacle = second.obstacle.as_mut().unwrap();
        obstacle.monument = 72;
        obstacle.center = lattice.point([52, 50]).unwrap();
        let applied = apply_cell_with_entrances(
            &mut grid, &[first, second], 5000, 2_000_000, 2, Some(3.), || false,
        ).unwrap().unwrap();
        let footprints = applied.entrance_footprints.unwrap();
        assert_eq!(footprints[50 * n + 50], EntranceFootprint::Monument(71));
        assert_eq!(footprints[50 * n + 51], EntranceFootprint::Overlap);
        assert_eq!(footprints[50 * n + 52], EntranceFootprint::Monument(72));
        let margin = 50 * n + 48;
        assert_eq!(footprints[margin], EntranceFootprint::Clear);
        assert_eq!(applied.entrance_exclusions[margin], 0);
        assert_eq!(grid.grid.costs[margin], 0);
    }

    #[test]
    fn final_foreign_footprint_exclusion_retains_its_bounded_margin() {
        use papyrus_terrain::road_network::socket_recovery::BlockedObstacle;
        let mut marker = preference();
        marker.radius = 24.;
        marker.obstacle = Some(BlockedObstacle {
            monument: 37,
            center: [0.; 2],
            extent: [50.; 2],
            yaw: 31.,
        });
        marker.harden();
        assert!(marker.exclude);
        assert_eq!(marker.radius, 96.);
        marker.harden();
        assert_eq!(marker.radius, 256.);
    }
    #[test]
    fn saturated_feedback_keeps_new_witnesses_and_protects_observed_collisions() {
        use papyrus_terrain::road_network::socket_recovery::BlockedObstacle;
        let mut markers = Vec::new();
        for i in 0..MAX_MARKERS {
            let mut marker = preference();
            marker.radius = 20.;
            marker.obstacle = Some(BlockedObstacle {
                monument: i as u64,
                center: [300., 300.],
                extent: [10., 10.],
                yaw: 37.,
            });
            collect(&mut markers, marker);
        }
        let mut collision = markers[0].clone();
        collision.witness.position = [300., 300.];
        markers[0] = collision.clone();
        let original = markers.clone();
        let mut shoreline = preference();
        shoreline.hierarchy = 2;
        shoreline.radius = 8.;
        shoreline.exclude = true;
        shoreline.witness.kind = crate::road_refinement::AlignmentKind::Clearance;
        collect(&mut markers, shoreline.clone());
        assert_eq!(markers.len(), MAX_MARKERS);
        assert_ne!(markers, original);
        assert_eq!(markers[0], collision);
        assert_eq!(markers[MAX_MARKERS - 1], shoreline);
        let stable = markers.clone();
        let duplicate = markers[0].clone();
        collect(&mut markers, duplicate);
        assert_eq!(markers, stable);
        let mut promoted = original[1].clone();
        promoted.witness.position = [300., 300.];
        collect(&mut markers, promoted.clone());
        assert_eq!(markers[1], promoted);
        let mut foreign = collision;
        foreign.obstacle.as_mut().unwrap().monument = 999;
        collect(&mut markers, foreign.clone());
        assert!(markers.contains(&foreign));
        let mut grid = fixture();
        apply_for(&mut grid, 1000, &markers, 5000, 20000, Some(2), || false)
            .unwrap()
            .unwrap();
        assert_eq!(grid.costs[4 * 9 + 4], 0);
        let resolution = papyrus_terrain::road_costmap::CellLattice::new(6000., 7.5)
            .unwrap()
            .resolution();
        assert!(resolution * resolution * (MAX_MARKERS + 6) <= 128_000_000);
    }
    #[test]
    fn rejected_network_batches_families_without_excluding_own_entrances_or_exceeding_limits() {
        use papyrus_terrain::{
            path_geometry::Path,
            road_network::{Destination, Family, Obstacle, Road, socket_recovery::BlockedObstacle},
        };
        let mut report = Report::default();
        let controls = [
            vec![[0., 20., 0.], [150., 20., 100.], [300., 20., 200.]],
            vec![[300., 20., 200.], [0., 20., 0.], [-400., 20., 400.]],
        ];
        report.roads = vec![
            Road {
                from: Destination::Monument {
                    monument: 13,
                    connector: 1,
                },
                to: Destination::Network { road: 1, point: 0 },
                points: controls[0].clone(),
            },
            Road {
                from: Destination::Network { road: 0, point: 2 },
                to: Destination::Monument {
                    monument: 14,
                    connector: 2,
                },
                points: controls[1].clone(),
            },
        ];
        report.families = (0..2)
            .map(|i| Family {
                infrastructure_type: if i == 0 { 0 } else { 2 },
                hierarchy: i as u32 + 1,
                seed: 1,
                first_road: i,
                road_count: 1,
                socket_count: 1,
                unresolved: vec![],
                width: if i == 0 { 12. } else { 4. },
                splat_channel: 7,
            })
            .collect();
        let paths = controls
            .iter()
            .map(|p| Path::from_controls(p, 10000, || false).unwrap().unwrap())
            .collect::<Vec<_>>();
        let roads = paths
            .iter()
            .enumerate()
            .map(|(i, p)| papyrus_terrain::roadside_monuments::Road {
                path: p,
                hierarchy: i as u8 + 1,
            })
            .collect::<Vec<_>>();
        let obstacle = |monument, center| Obstacle {
            monument,
            center,
            extent: [20., 20.],
            yaw: 99.462326,
        };
        let obstacles = [
            obstacle(13, [0., 0.]),
            obstacle(21, [300., 200.]),
            obstacle(14, [-400., 400.]),
        ];
        let mut primary = marker(
            &report,
            AlignmentWitness {
                kind: AlignmentKind::Clearance,
                road: 0,
                position: [300., 200.],
            },
        )
        .unwrap();
        primary.obstacle = Some(BlockedObstacle::from(&obstacles[1]));
        let (batch, work) =
            network_obstacles(&report, &roads, primary.clone(), &obstacles, 100, || false)
                .unwrap()
                .unwrap();
        assert_eq!(work, 24);
        let keys = batch
            .iter()
            .map(|m| (m.hierarchy, m.obstacle.as_ref().unwrap().monument))
            .collect::<Vec<_>>();
        assert_eq!(keys, vec![(1, 21), (2, 13), (2, 21)]);
        assert!(batch.iter().all(|m| m.terminal.is_none()));
        assert!(batch.iter().all(|m| m.exclude));
        let (all, all_work) = network_obstacles_all_families(
            &report,
            &roads,
            primary.clone(),
            &obstacles,
            100,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(all_work, work);
        let keys = all
            .iter()
            .map(|m| (m.hierarchy, m.obstacle.as_ref().unwrap().monument))
            .collect::<Vec<_>>();
        assert_eq!(keys, vec![(1, 21), (1, 14), (2, 13), (2, 21)]);
        assert!(all.iter().all(|m| m.terminal.is_none() && m.exclude));
        let distant = [obstacle(99, [1000., -1000.])];
        let (all, _) =
            network_obstacles_all_families(&report, &roads, primary.clone(), &distant, 100, || {
                false
            })
            .unwrap()
            .unwrap();
        for hierarchy in [1, 2] {
            assert!(all.iter().any(|m| m.hierarchy == hierarchy
                && m.obstacle.as_ref().is_some_and(|o| o.monument == 99)));
        }
        assert!(
            network_obstacles_all_families(
                &report,
                &roads,
                primary.clone(),
                &obstacles,
                23,
                || false
            )
            .is_err()
        );
        assert!(
            network_obstacles_all_families(
                &report,
                &roads,
                primary.clone(),
                &obstacles,
                100,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(
            network_obstacles(&report, &roads, primary.clone(), &obstacles, 23, || false).is_err()
        );
        assert!(
            network_obstacles(&report, &roads, primary.clone(), &obstacles, 100, || true)
                .unwrap()
                .is_none()
        );
        let mut invalid = obstacles.clone();
        invalid[1].yaw = f32::NAN;
        assert!(
            network_obstacles(&report, &roads, primary.clone(), &invalid, 100, || false).is_err()
        );
        // Authored point and line bounds still acquire the road's existing
        // clearance margin when feedback is rasterized.
        for extent in [[0., 0.], [0., 20.]] {
            let point = Obstacle {
                monument: 99,
                center: [0., 0.],
                extent,
                yaw: 37.,
            };
            let (markers, _) = network_obstacles(
                &report,
                &roads,
                primary.clone(),
                std::slice::from_ref(&point),
                100,
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(markers.iter().any(|m| {
                m.obstacle
                    .as_ref()
                    .is_some_and(|o| o.monument == 99 && o.extent == extent)
            }));
            let mut grid = fixture();
            apply(&mut grid, 1000, &markers, 5000, 10000, || false)
                .unwrap()
                .unwrap();
            assert_eq!(grid.costs[4 * 9 + 4], 0);
            for invalid in [-1., f32::NAN, f32::INFINITY] {
                let bad = Obstacle {
                    extent: [invalid, 0.],
                    ..point.clone()
                };
                assert!(
                    network_obstacles(&report, &roads, primary.clone(), &[bad], 100, || false)
                        .is_err()
                );
            }
        }
        let many = (0..MAX_MARKERS * 2)
            .map(|i| obstacle(i as u64, [300., 200.]))
            .collect::<Vec<_>>();
        let (bounded, _) = network_obstacles(
            &report,
            &roads,
            primary.clone(),
            &many,
            MAX_MARKERS * 32,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(bounded.len(), MAX_MARKERS);
        let (bounded, _) = network_obstacles_all_families(
            &report,
            &roads,
            primary,
            &many,
            MAX_MARKERS * 32,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(bounded.len(), MAX_MARKERS);
        // A ring detour must account for a monument that the rejected ring
        // never approached. Access paths still retain their own-site exemption.
        report.families[0].hierarchy = 0;
        report.roads[0].from = Destination::Loop;
        report.roads[0].to = Destination::Loop;
        let ring_roads = [
            papyrus_terrain::roadside_monuments::Road {
                path: &paths[0],
                hierarchy: 0,
            },
            papyrus_terrain::roadside_monuments::Road {
                path: &paths[1],
                hierarchy: 2,
            },
        ];
        let distant = [obstacle(99, [1000., -1000.]), obstacles[0].clone()];
        let primary = marker(
            &report,
            AlignmentWitness {
                kind: AlignmentKind::Clearance,
                road: 0,
                position: [300., 200.],
            },
        )
        .unwrap();
        let (batch, _) = network_obstacles(&report, &ring_roads, primary, &distant, 100, || false)
            .unwrap()
            .unwrap();
        assert!(batch.iter().any(|m| m.hierarchy == 0
            && m.exclude
            && m.obstacle.as_ref().is_some_and(|o| o.monument == 99)));
        assert!(
            batch
                .iter()
                .any(|m| m.hierarchy == 0 && m.obstacle.as_ref().is_some_and(|o| o.monument == 13))
        );
    }
    #[test]
    fn foreign_footprint_feedback_covers_rotated_bounds_and_keeps_source_costs() {
        use papyrus_terrain::road_network::socket_recovery::BlockedObstacle;
        let mut feedback = preference();
        feedback.radius = 24.;
        feedback.obstacle = Some(BlockedObstacle {
            monument: 13,
            center: [0., 0.],
            extent: [200., 20.],
            yaw: 90.,
        });
        let mut grid = fixture();
        let original = grid.costs.clone();
        let applied = apply(&mut grid, 1000, &[feedback.clone()], 5000, 10000, || false)
            .unwrap()
            .unwrap();
        // The long axis rotates onto Z; the old point-sized patch misses it.
        assert_eq!(grid.costs[5 * 9 + 4], 5101);
        assert_eq!(grid.costs[4 * 9 + 5], 101);
        assert_eq!(grid.costs[0], 0);
        assert_eq!(applied.original.costs, original);
        feedback.exclude = true;
        let mut excluded = fixture();
        let excluded_feedback = apply_for(
            &mut excluded,
            1000,
            &[feedback.clone()],
            5000,
            10000,
            Some(0),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            excluded_feedback
                .entrance_exclusions
                .iter()
                .all(|&v| v == 0)
        );
        assert_eq!(excluded.costs[5 * 9 + 4], 0);
        assert_eq!(excluded.costs[4 * 9 + 5], 101);
        let mut wet = preference();
        wet.witness.position = [0., 125.];
        wet.radius = 24.;
        wet.exclude = true;
        let mut overlap = fixture();
        let overlap_feedback = apply_for(
            &mut overlap,
            1000,
            &[feedback.clone(), wet],
            5000,
            10000,
            Some(0),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(overlap_feedback.entrance_exclusions[5 * 9 + 4], 1);
        let mut other_family = fixture();
        apply_for(
            &mut other_family,
            1000,
            &[feedback.clone()],
            5000,
            10000,
            Some(1),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(other_family.costs[5 * 9 + 4], 5101);
        let mut markers = vec![];
        collect(&mut markers, feedback.clone());
        feedback.witness.position = [0., 200.];
        collect(&mut markers, feedback.clone());
        assert_eq!(
            markers.len(),
            1,
            "Same obstacle must not consume repeated retries"
        );
        feedback.obstacle.as_mut().unwrap().monument = 14;
        collect(&mut markers, feedback.clone());
        assert_eq!(markers.len(), 2);
        feedback.obstacle.as_mut().unwrap().yaw = f32::NAN;
        let mut invalid = fixture();
        assert!(apply(&mut invalid, 1000, &[feedback], 5000, 10000, || false).is_err());
        assert_eq!(invalid.costs, original);
    }
    #[test]
    fn source_preferences_use_native_cell_centres_and_preserve_original_acceptance_costs() {
        use papyrus_terrain::road_costmap::{CELL_VERSION, CellGrid, CellLattice};
        let lattice = CellLattice::new(4000., 7.5).unwrap();
        assert_eq!(lattice.resolution(), 533);
        let mut grid = CellGrid {
            lattice,
            grid: fixture(),
        };
        grid.grid.engine = CELL_VERSION;
        grid.grid.resolution = 533;
        grid.grid.costs = vec![101; 533 * 533];
        grid.grid.costs[0] = 0;
        let original = grid.grid.costs.clone();
        let mut m = preference();
        m.witness.position = lattice.point([1, 1]).unwrap().map(f64::from);
        m.radius = 0.2;
        m.hierarchy = 2;
        m.exclude = true;
        let receipt = apply_cell_for(
            &mut grid,
            std::slice::from_ref(&m),
            5000,
            2_000_000,
            2,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt.original.costs, original);
        assert_eq!(grid.grid.costs[534], 0);
        assert_eq!(grid.grid.costs.iter().filter(|&&c| c == 0).count(), 2);
        assert_eq!(receipt.report["changed_cells"], 1);
        assert_eq!(receipt.report["cell_lattice"]["resolution"], 533);
        assert_eq!(receipt.entrance_exclusions[534], 1);
        assert_eq!(receipt.entrance_exclusions[0], 0);
        // A known forbidden region remains forbidden to entrance recovery even
        // when it overlaps an already-zero cell in the original source grid.
        let mut blocked = grid.clone();
        let mut first = m.clone();
        first.witness.position = lattice.point([0, 0]).unwrap().map(f64::from);
        let receipt = apply_cell_for(&mut blocked, &[first], 5000, 2_000_000, 2, || false)
            .unwrap()
            .unwrap();
        assert_eq!(blocked.grid.costs[0], 0);
        assert_eq!(receipt.entrance_exclusions[0], 1);
        // An exclusion for dirt roads stays a soft preference on paved roads.
        grid.grid.costs.clone_from(&original);
        apply_cell_for(
            &mut grid,
            std::slice::from_ref(&m),
            5000,
            2_000_000,
            1,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(grid.grid.costs[534], 5101);
        assert_eq!(grid.grid.costs[0], 0);
        let before = grid.grid.costs.clone();
        let calls = std::cell::Cell::new(0);
        assert!(
            apply_cell_for(
                &mut grid,
                std::slice::from_ref(&m),
                5000,
                2_000_000,
                2,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 2
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(grid.grid.costs, before);
        assert!(apply_cell_for(&mut grid, &[m], 5000, 1, 2, || false).is_err());
        assert_eq!(grid.grid.costs, before);
    }
    #[test]
    fn a_complete_route_uses_alternatives_but_can_still_cross_and_audits_keep_original_costs() {
        use papyrus_terrain::routing::{self, Outcome};
        let original = fixture();
        let mut grid = original.clone();
        let applied = apply(&mut grid, 1000, &[preference()], 5000, 10000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(applied.original.costs, original.costs);
        assert_eq!(grid.costs[0], 0);
        assert_eq!(grid.seed, original.seed);
        let heights = vec![10.; 81];
        let route = |costs: &[u32], corridor: bool| {
            let blocked = costs
                .iter()
                .enumerate()
                .map(|(i, &c)| c == 0 || (corridor && i / 9 != 4))
                .collect::<Vec<_>>();
            let traversal = costs.iter().map(|&c| c.max(1)).collect::<Vec<_>>();
            routing::route_with_costs(
                routing::Grid {
                    resolution: 9,
                    size: 1000.,
                    heights: &heights,
                    blocked: &blocked,
                },
                [0, 4],
                [8, 4],
                routing::Settings {
                    seed: 123,
                    maximum_grade: 0.2,
                    slope_penalty: 0.,
                    minimum_height: 1.,
                    maximum_expansions: 81,
                },
                Some(&traversal),
                || false,
            )
            .unwrap()
        };
        let Outcome::Found(before) = route(&original.costs, false) else {
            panic!("Base route missing")
        };
        let Outcome::Found(after) = route(&grid.costs, false) else {
            panic!("Alternative route missing")
        };
        assert!(before.iter().any(|p| p[0] == 0. && p[2] == 0.));
        assert!(after.iter().all(|p| p[0].hypot(p[2]) > 200.));
        let Outcome::Found(crossing) = route(&grid.costs, true) else {
            panic!("Soft preference incorrectly blocks the only route")
        };
        assert!(crossing.iter().any(|p| p[0] == 0. && p[2] == 0.));
        let mut report = json!({"grid":grid});
        attach(&mut report, Some(applied));
        assert_eq!(report["grid"]["costs"], json!(original.costs));
        assert_ne!(report["routing_grid"]["costs"], report["grid"]["costs"]);
    }
    #[test]
    fn private_preferences_reject_bad_input_or_work_and_cancel_without_mutation() {
        let mut grid = fixture();
        let before = grid.costs.clone();
        assert!(apply(&mut grid, 1000, &[preference()], 5000, 1, || false).is_err());
        let mut invalid = preference();
        invalid.witness.position[0] = f64::NAN;
        assert!(apply(&mut grid, 1000, &[invalid], 5000, 10000, || false).is_err());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            apply(&mut grid, 1000, &[preference()], 5000, 10000, || calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                > 0)
            .unwrap()
            .is_none()
        );
        assert_eq!(grid.costs, before);
        let mut markers = vec![];
        for _ in 0..30 {
            collect(&mut markers, preference());
        }
        assert_eq!(markers.len(), 1);
    }
    #[test]
    fn final_planning_exclusion_keeps_source_grid_and_does_not_hide_an_unreachable_route() {
        use papyrus_terrain::routing::{self, Outcome};
        let source = fixture();
        let mut routed = source.clone();
        let mut excluded = preference();
        excluded.exclude = true;
        let applied = apply(&mut routed, 1000, &[excluded], 5000, 10000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(applied.original.costs, source.costs);
        assert_eq!(routed.costs[4 * 9 + 4], 0);
        assert!(source.costs[4 * 9 + 4] > 0);
        let heights = vec![10.; 81];
        let blocked = routed
            .costs
            .iter()
            .enumerate()
            .map(|(i, &c)| c == 0 || i / 9 != 4)
            .collect::<Vec<_>>();
        let costs = routed.costs.iter().map(|&c| c.max(1)).collect::<Vec<_>>();
        assert_eq!(
            routing::route_with_costs(
                routing::Grid {
                    resolution: 9,
                    size: 1000.,
                    heights: &heights,
                    blocked: &blocked
                },
                [0, 4],
                [8, 4],
                routing::Settings {
                    seed: 123,
                    maximum_grade: 0.2,
                    slope_penalty: 0.,
                    minimum_height: 1.,
                    maximum_expansions: 81
                },
                Some(&costs),
                || false
            )
            .unwrap(),
            Outcome::Unreachable
        );
    }
    #[test]
    fn replanning_cannot_pass_by_removing_a_loop_socket_or_road_family() {
        use papyrus_terrain::road_network::{Destination, Family, Road};
        let mut report = Report::default();
        report.roads.push(Road {
            from: Destination::Loop,
            to: Destination::Loop,
            points: vec![
                [-20., 0., -20.],
                [20., 0., -20.],
                [20., 0., 20.],
                [-20., 0., 20.],
                [-20., 0., -20.],
            ],
        });
        report.roads.push(Road {
            from: Destination::Monument {
                monument: 7,
                connector: 11,
            },
            to: Destination::Network { road: 0, point: 0 },
            points: vec![[0.; 3], [-20., 0., -20.]],
        });
        report.families.push(Family {
            infrastructure_type: 0,
            hierarchy: 0,
            seed: 1,
            first_road: 0,
            road_count: 1,
            socket_count: 0,
            unresolved: vec![],
            width: 12.,
            splat_channel: 7,
        });
        report.families.push(Family {
            hierarchy: 1,
            first_road: 1,
            socket_count: 1,
            ..report.families[0].clone()
        });
        let required = Connections::capture(&report).unwrap();
        assert!(required.preserved(&report).unwrap());
        report.families[1].hierarchy = 2;
        assert!(!required.preserved(&report).unwrap());
        assert_eq!(
            required.missing(&report).unwrap().sockets,
            vec![(0, 1, 7, 11)]
        );
        report.families[1].hierarchy = 1;
        report.roads[1].from = Destination::Network { road: 0, point: 0 };
        assert!(!required.preserved(&report).unwrap());
        report.roads[1].from = Destination::Monument {
            monument: 7,
            connector: 11,
        };
        report.roads[0].from = Destination::Network { road: 0, point: 0 };
        assert!(!required.preserved(&report).unwrap());
        assert_eq!(required.missing(&report).unwrap().main_loops, 1);
        assert!(required.missing(&report).unwrap().sockets.is_empty());
        report.roads[0].from = Destination::Loop;
        assert!(required.preserved(&report).unwrap());
        assert!(required.rejection(&report).unwrap().is_none());
        assert!(!required.preserved(&Report::default()).unwrap());
        let diagnostic = required.rejection(&Report::default()).unwrap().unwrap();
        assert_eq!(diagnostic["missing"]["main_loops"], 1);
        assert_eq!(diagnostic["missing"]["sockets"], json!([[0, 1, 7, 11]]));
        assert!(diagnostic.get("roads").is_none());
        assert!(diagnostic.get("ring_costmap").is_none());
    }
    #[test]
    fn planning_exclusion_moves_a_virtual_ring_corner_instead_of_reusing_unsafe_waypoint() {
        use papyrus_terrain::{road_ring, routing};
        let n = 17;
        let mut grid = fixture();
        grid.resolution = n;
        grid.costs = vec![101; n * n];
        let marker = Marker {
            witness: AlignmentWitness {
                kind: AlignmentKind::Clearance,
                road: 0,
                position: [312.5, 312.5],
            },
            radius: 100.,
            exclude: true,
            hierarchy: 0,
            terminal: None,
            obstacle: None,
            protected_topology: 0,
            low_ground: false,
        };
        apply(&mut grid, 1000, &[marker], 5000, 100000, || false)
            .unwrap()
            .unwrap();
        let blocked = grid.costs.iter().map(|&v| v == 0).collect::<Vec<_>>();
        let costs = grid.costs.iter().map(|&v| v.max(1)).collect::<Vec<_>>();
        let heights = vec![20.; n * n];
        let result = road_ring::propose_recovering_with_costs(
            routing::Grid {
                resolution: n,
                size: 1000.,
                heights: &heights,
                blocked: &blocked,
            },
            road_ring::Policy {
                minimum_world_size: 1000,
                large_world_size: 5000,
                inset_divisor: 4,
                inward_step_cells: 1,
                maximum_attempts: 128,
                width: 12.,
                hierarchy: 0,
                network_sample_stride: 4,
            },
            routing::Settings {
                seed: 123,
                maximum_grade: 0.2,
                slope_penalty: 0.,
                minimum_height: 1.,
                maximum_expansions: n * n,
            },
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.audit.status, "found");
        assert!(result.audit.waypoint_moves > 0);
        assert!(!result.audit.waypoints.contains(&[13, 13]));
        assert_eq!(result.points.first(), result.points.last());
    }
    #[test]
    fn paved_failure_exclusions_do_not_cut_ring_or_dirt_connections() {
        let mut failure = preference();
        failure.hierarchy = 1;
        failure.exclude = true;
        for hierarchy in [0, 1, 2] {
            let mut grid = fixture();
            let applied = apply_for(
                &mut grid,
                1000,
                &[failure.clone()],
                5000,
                10000,
                Some(hierarchy),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(applied.original.costs[4 * 9 + 4], 101);
            if hierarchy == 1 {
                assert_eq!(grid.costs[4 * 9 + 4], 0);
            } else {
                assert_eq!(grid.costs[4 * 9 + 4], 5101);
            }
            assert_eq!(grid.costs[0], 0);
        }
        let mut markers = vec![];
        collect(&mut markers, preference());
        collect(&mut markers, failure);
        assert_eq!(markers.len(), 2);
    }
}
