//! Ordered source-style connector groups on a cell-centred road lattice.
//! Produces raw candidates; terrain shaping, mesh acceptance and publication
//! remain separate. Connector enumeration must include inactive authored exits
//! and already be filtered to the requested road/trail infrastructure family.
use crate::{
    path_geometry,
    road_costmap::{CELL_VERSION, CellGrid, CellLattice},
    source_road_ring::RawNode,
    source_road_route::{self as route, Push, Settings},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-source-road-layout-1";
pub const CONNECTED_VERSION: &str = "papyrus-connected-road-layout-1";
pub const TERRAIN_CONNECTED_VERSION: &str = "papyrus-terrain-connected-road-layout-1";
pub const TERRAIN_ROUTED_VERSION: &str = "papyrus-terrain-connected-road-layout-2";
pub const TERRAIN_BOOTSTRAP_VERSION: &str = "papyrus-terrain-connected-road-layout-3";
fn no(v: &bool) -> bool {
    !*v
}
fn zero(v: &usize) -> bool {
    *v == 0
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Connector {
    pub monument: u32,
    pub cell: [usize; 2],
    pub centre: [usize; 2],
    pub radius: usize,
    pub roadside: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub network_sample_stride: usize,
    pub push_multiplier: u32,
    pub maximum_queries: usize,
    pub maximum_node_storage: usize,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=1024).contains(&self.network_sample_stride)
            || self.push_multiplier > 1_000_000
            || !(1..=8192).contains(&self.maximum_queries)
            || !(2..=262144).contains(&self.maximum_node_storage)
        {
            return Err("Invalid source connector layout policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Segment {
    pub lattice: CellLattice,
    pub nodes: Vec<RawNode>,
    /// Indices into the caller's connector array, not monument IDs.
    pub origin: Option<usize>,
    pub target: Option<usize>,
}
pub struct ExistingPath<'a> {
    pub lattice: CellLattice,
    pub nodes: &'a [RawNode],
}
pub struct ShapedSegment {
    pub path: path_geometry::Path,
    pub total_work: usize,
    pub start: bool,
    pub end: bool,
}
impl Segment {
    /// Height receives source-normalized UVs. Only the first/last authored
    /// connector replaces a cell centre; intermediate recovery cells survive.
    pub fn shape(
        &self,
        settings: path_geometry::Settings,
        mut endpoint: impl FnMut(usize) -> Result<[f32; 2], String>,
        mut height: impl FnMut([f32; 2]) -> Result<f32, String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<ShapedSegment>, String> {
        let lattice = self.lattice;
        if !(2..=32768).contains(&self.nodes.len())
            || !(1..=32_000_000).contains(&settings.maximum_work)
            || self.nodes.len() >= settings.maximum_work
            || settings.horizontal_iterations > 256
            || settings.vertical_iterations > 256
            || !settings.resample_distance.is_finite()
            || !(0.1..=100.).contains(&settings.resample_distance)
        {
            return Err("Invalid source road segment shaping request".into());
        }
        let mut controls = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let mut uv = lattice
                .normalized(node.cell)
                .ok_or("Invalid raw road cell")?;
            let marker = if i == 0 {
                self.origin
            } else if i == self.nodes.len() - 1 {
                self.target
            } else {
                None
            };
            if let Some(id) = marker {
                let world = endpoint(id)?;
                if world.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
                    return Err("Invalid authored road endpoint".into());
                }
                uv = world.map(|v| (v + lattice.size() * 0.5) * (1. / lattice.size()));
            }
            let y = height(uv)?;
            if !y.is_finite() || y.abs() > 10000. {
                return Err("Invalid road terrain height".into());
            }
            let [x, z] = uv.map(|v| -lattice.size() * 0.5 + v * lattice.size());
            controls.push([x, y.max(1.), z]);
        }
        let Some(path) = path_geometry::build(
            &controls,
            path_geometry::Settings {
                maximum_work: settings.maximum_work - controls.len(),
                ..settings
            },
            None,
            cancelled,
        )
        .map_err(|e| format!("Source road segment shaping failed: {e:?}"))?
        else {
            return Ok(None);
        };
        Ok(Some(ShapedSegment {
            total_work: path.work() + controls.len(),
            path,
            start: self.origin.is_some(),
            end: self.target.is_some(),
        }))
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub policy: Policy,
    pub search: Settings,
    pub maximum_work: usize,
    pub work: usize,
    pub entrance_queries: usize,
    #[serde(skip_serializing_if = "zero")]
    pub terrain_entrance_queries: usize,
    #[serde(skip_serializing_if = "zero")]
    pub terrain_route_queries: usize,
    #[serde(skip_serializing_if = "zero")]
    pub shared_entrance_branches: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terrain_route_traces: Vec<route::AccessDiagnostics>,
    #[serde(skip_serializing_if = "no")]
    pub terrain_route_fallback: bool,
    #[serde(skip_serializing_if = "zero")]
    pub terrain_entrance_replacements: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terrain_replaced_connectors: Vec<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub terrain_fallback_connectors: Vec<usize>,
    #[serde(skip_serializing_if = "zero")]
    pub terrain_preparation_work: usize,
    pub connected_goal_preparations: usize,
    pub connected_goal_work: usize,
    #[serde(skip_serializing_if = "zero")]
    pub bootstrap_recovery_targets: usize,
    pub layout_queries: usize,
    pub depth_rejections: usize,
    pub failed_connections: usize,
    pub skipped_roadside: usize,
    pub promoted_groups: Vec<u32>,
    pub accepted_connectors: Vec<usize>,
    pub discarded_singletons: usize,
}
impl Audit {
    /// Retain consumed work, queries and diagnostics across immutable retries.
    /// The caller supplies and checks the enclosing work and query allowances.
    pub fn add_previous(&mut self, previous: &Self) -> Result<(), String> {
        macro_rules! add { ($($field:ident),+) => {$(
            self.$field=self.$field.checked_add(previous.$field).ok_or("Entrance retry accounting overflow")?;
        )+}; }
        add!(
            work,
            entrance_queries,
            terrain_entrance_queries,
            terrain_route_queries,
            shared_entrance_branches,
            terrain_entrance_replacements,
            terrain_preparation_work,
            connected_goal_preparations,
            connected_goal_work,
            layout_queries,
            depth_rejections,
            failed_connections
        );
        let mut traces = previous.terrain_route_traces.clone();
        traces.append(&mut self.terrain_route_traces);
        self.terrain_route_traces = traces;
        self.terrain_route_fallback |= previous.terrain_route_fallback;
        self.bootstrap_recovery_targets = self
            .bootstrap_recovery_targets
            .max(previous.bootstrap_recovery_targets);
        self.terrain_replaced_connectors
            .extend_from_slice(&previous.terrain_replaced_connectors);
        self.terrain_replaced_connectors.sort_unstable();
        self.terrain_replaced_connectors.dedup();
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Layout {
    pub lattice: CellLattice,
    pub segments: Vec<Segment>,
    /// Source sampling order, before entrance chains are attached. Duplicates
    /// are intentional and influence source multi-target queue order.
    pub network_targets: Vec<[usize; 2]>,
    /// Final entrance choices for this pass, before authored chains are joined.
    /// Connector indices refer to the caller's array, including skipped exits.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered_entrances: Vec<RecoveredEntrance>,
    pub unrecovered_connectors: Vec<usize>,
    pub unattached_connectors: Vec<usize>,
    pub audit: Audit,
}

#[derive(Debug)]
pub struct AcceptedFamily {
    pub raw: Layout,
    pub shaped: Vec<crate::path_geometry::Path>,
}

#[derive(Debug)]
pub struct AcceptedNetwork {
    pub ring: Option<std::sync::Arc<crate::source_road_ring::Layout>>,
    pub families: Vec<std::sync::Arc<AcceptedFamily>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecoveredEntrance {
    pub connector: usize,
    pub root: [usize; 2],
    pub nodes: usize,
}
#[derive(Debug)]
pub enum Outcome {
    Found(Layout),
    Budget(Audit),
    Cancelled(Audit),
}
enum Halt {
    Budget,
    Cancelled,
    Invalid(String),
}
struct Entrance {
    connector: usize,
    nodes: Vec<RawNode>,
}
struct Context<'a, F> {
    grid: &'a CellGrid,
    connectors: &'a [Connector],
    cancelled: &'a F,
    audit: Audit,
    node_storage: usize,
    connected_entrances: bool,
    terrain: Option<route::TerrainRecovery<'a>>,
    terrain_exclusions: &'a [usize],
    guided_routes: bool,
    recovery_targets: &'a [[usize; 2]],
}
impl<F: Fn() -> bool> Context<'_, F> {
    fn completed(&mut self, amount: usize) -> Result<(), Halt> {
        if amount > self.audit.maximum_work.saturating_sub(self.audit.work) {
            return Err(Halt::Budget);
        }
        self.audit.work += amount;
        Ok(())
    }
    fn spend(&mut self, amount: usize) -> Result<(), Halt> {
        if (self.cancelled)() {
            return Err(Halt::Cancelled);
        }
        self.completed(amount)
    }
    fn reserve(&mut self, nodes: usize) -> Result<(), Halt> {
        if nodes
            > self
                .audit
                .policy
                .maximum_node_storage
                .saturating_sub(self.node_storage)
        {
            return Err(Halt::Budget);
        }
        self.node_storage += nodes;
        Ok(())
    }
    fn settings(&mut self, entrance: bool) -> Result<Settings, Halt> {
        self.spend(0)?;
        if self.audit.entrance_queries + self.audit.layout_queries
            == self.audit.policy.maximum_queries
        {
            return Err(Halt::Budget);
        }
        if entrance {
            self.audit.entrance_queries += 1;
        } else {
            self.audit.layout_queries += 1;
        }
        let available = self.audit.maximum_work - self.audit.work;
        if available == 0 {
            return Err(Halt::Budget);
        }
        Ok(Settings {
            maximum_work: self.audit.search.maximum_work.min(available),
            ..self.audit.search
        })
    }
    fn receive(&mut self, outcome: route::Outcome) -> Result<Option<Vec<RawNode>>, Halt> {
        match outcome {
            route::Outcome::Found(path) => {
                self.completed(path.work)?;
                self.spend(path.cells.len())?;
                self.reserve(path.cells.len())?;
                Ok(Some(
                    path.cells
                        .into_iter()
                        .zip(path.cumulative_costs)
                        .map(|(cell, cost)| RawNode { cell, cost })
                        .collect(),
                ))
            }
            route::Outcome::Unreachable { work } => {
                self.completed(work)?;
                self.spend(0)?;
                Ok(None)
            }
            route::Outcome::DepthLimit { work } => {
                self.completed(work)?;
                self.spend(0)?;
                self.audit.depth_rejections += 1;
                Ok(None)
            }
            route::Outcome::Budget { work } => {
                self.completed(work)?;
                Err(Halt::Budget)
            }
            route::Outcome::Cancelled { work } => {
                self.completed(work)?;
                Err(Halt::Cancelled)
            }
        }
    }
    fn sample(&mut self, nodes: &[RawNode], targets: &mut Vec<[usize; 2]>) -> Result<(), Halt> {
        for (i, node) in nodes.iter().enumerate() {
            self.spend(1)?;
            if (i + 1) % self.audit.policy.network_sample_stride == 0 {
                if targets.len() == 4096 {
                    return Err(Halt::Budget);
                }
                targets.push(node.cell);
            }
        }
        Ok(())
    }
    fn defer_group(
        &mut self,
        selected: usize,
        entrances: &[Entrance],
        pending: &mut Vec<usize>,
        deferred: &mut Vec<usize>,
        accepted: &mut Vec<usize>,
    ) -> Result<(), Halt> {
        self.spend(pending.len() + deferred.len())?;
        let group = self.connectors[entrances[selected].connector].monument;
        pending.retain(|&id| {
            if self.connectors[entrances[id].connector].monument == group {
                deferred.push(id);
                false
            } else {
                true
            }
        });
        deferred.retain(|&id| id != selected);
        accepted.push(selected);
        Ok(())
    }
    fn generate(&mut self, existing: &[ExistingPath<'_>]) -> Result<Layout, Halt> {
        let mut targets = Vec::new();
        for path in existing {
            self.sample(path.nodes, &mut targets)?;
        }
        self.spend(if self.connected_entrances {
            self.connectors.len()
        } else {
            0
        })?;
        let recovery_targets = if self.recovery_targets.is_empty() {
            targets.as_slice()
        } else {
            self.recovery_targets
        };
        let connected = if self.connected_entrances
            && !recovery_targets.is_empty()
            && self.connectors.iter().any(|c| !c.roadside)
        {
            self.spend(0)?;
            let available = self
                .audit
                .maximum_work
                .saturating_sub(self.audit.work)
                .min(self.audit.search.maximum_work);
            if available == 0 {
                return Err(Halt::Budget);
            }
            match route::prepare_connected(self.grid, recovery_targets, available, self.cancelled)
                .map_err(Halt::Invalid)?
            {
                route::ConnectedPreparation::Ready(goals) => {
                    self.completed(goals.preparation_work())?;
                    self.audit.connected_goal_preparations = 1;
                    self.audit.connected_goal_work = goals.preparation_work();
                    Some(goals)
                }
                route::ConnectedPreparation::Budget { work } => {
                    self.completed(work)?;
                    return Err(Halt::Budget);
                }
                route::ConnectedPreparation::Cancelled { work } => {
                    self.completed(work)?;
                    return Err(Halt::Cancelled);
                }
            }
        } else {
            None
        };
        let mut entrances = Vec::new();
        let mut unrecovered = Vec::new();
        for (id, connector) in self.connectors.iter().enumerate() {
            self.spend(1)?;
            if connector.roadside {
                self.audit.skipped_roadside += 1;
                continue;
            }
            let settings = self.settings(true)?;
            // With no existing network, retain source bootstrapping. Once a
            // network exists, independent recovery must join its walkable
            // components rather than a nearby isolated positive-cost cell.
            let result = if let Some(goals) = &connected {
                goals.find_closest(connector.cell, settings, self.cancelled)
            } else {
                route::find_closest_walkable(self.grid, connector.cell, settings, self.cancelled)
            }
            .map_err(Halt::Invalid)?;
            if let Some(mut nodes) = self.receive(result)? {
                self.spend(self.terrain_exclusions.len())?;
                let slope_guidance = !self.terrain_exclusions.contains(&id);
                if let Some(terrain) = self.terrain.map(|terrain| terrain.for_connector(id))
                    && (slope_guidance || terrain.has_entrance_guidance())
                {
                    let mut steep = false;
                    let mut excluded = false;
                    let mut penalized = false;
                    let mut radius = connector.radius;
                    for node in &nodes {
                        self.spend(1)?;
                        penalized |= terrain.entrance_penalized(node.cell);
                        excluded |= !terrain
                            .entrance_clear(node.cell, node.cell)
                            .map_err(Halt::Invalid)?;
                        radius = radius.max(
                            node.cell[0]
                                .abs_diff(connector.cell[0])
                                .max(node.cell[1].abs_diff(connector.cell[1])),
                        );
                    }
                    for pair in nodes.windows(2) {
                        self.spend(1)?;
                        steep |= slope_guidance
                            && terrain
                                .is_steep(pair[0].cell, pair[1].cell)
                                .map_err(Halt::Invalid)?;
                        excluded |= !terrain
                            .entrance_clear(pair[0].cell, pair[1].cell)
                            .map_err(Halt::Invalid)?;
                    }
                    if steep || excluded || penalized {
                        let settings = self.settings(true)?;
                        self.audit.terrain_entrance_queries += 1;
                        let candidate = terrain
                            .find_closest(
                                self.grid,
                                connector.cell,
                                connected.as_ref(),
                                radius,
                                settings,
                                self.cancelled,
                            )
                            .map_err(Halt::Invalid)?;
                        if let Some(replacement) = self.receive(candidate)? {
                            nodes = replacement;
                            self.audit.terrain_entrance_replacements += 1;
                            self.audit.terrain_replaced_connectors.push(id);
                        } else if excluded {
                            unrecovered.push(id);
                            continue;
                        }
                    }
                }
                entrances.push(Entrance {
                    connector: id,
                    nodes,
                });
            } else {
                unrecovered.push(id);
            }
        }
        let mut pending: Vec<usize> = (0..entrances.len()).collect();
        let mut deferred: Vec<usize> = Vec::new();
        let mut accepted: Vec<usize> = Vec::new();
        let mut segments: Vec<Segment> = Vec::new();
        let mut push = None;
        while !pending.is_empty() || !deferred.is_empty() {
            self.spend(pending.len() + accepted.len() + deferred.len() + targets.len())?;
            if pending.is_empty() {
                let group = self.connectors[entrances[deferred[0]].connector];
                deferred.retain(|&id| {
                    if self.connectors[entrances[id].connector].monument == group.monument {
                        pending.push(id);
                        false
                    } else {
                        true
                    }
                });
                self.audit.promoted_groups.push(group.monument);
                push = Some(Push {
                    point: group.centre,
                    radius: group.radius,
                    distance: group.radius,
                    multiplier: self.audit.policy.push_multiplier,
                });
            }
            let starts: Vec<_> = accepted
                .iter()
                .chain(&deferred)
                .map(|&id| entrances[id].nodes[0].cell)
                .chain(targets.iter().copied())
                .collect();
            let ends: Vec<_> = pending
                .iter()
                .map(|&id| entrances[id].nodes[0].cell)
                .collect();
            if starts.len() > 4096 || ends.len() > 4096 {
                return Err(Halt::Budget);
            }
            let settings = self.settings(false)?;
            let mut result = if self.guided_routes {
                self.audit.terrain_route_queries += 1;
                let (result, trace) = self
                    .terrain
                    .ok_or_else(|| Halt::Invalid("Missing terrain access guidance".into()))?
                    .find_undirected_diagnosed(
                        self.grid,
                        &starts,
                        &ends,
                        Settings {
                            maximum_work: (settings.maximum_work / 2).max(1),
                            ..settings
                        },
                        push,
                        self.audit.policy.maximum_node_storage,
                        self.cancelled,
                    );
                self.audit.terrain_route_traces.push(trace);
                result
            } else {
                route::find_undirected_with_push(
                    self.grid,
                    &starts,
                    &ends,
                    settings,
                    push,
                    self.cancelled,
                )
            }
            .map_err(Halt::Invalid)?;
            if self.guided_routes {
                let fallback = match &result {
                    route::Outcome::Budget { work } | route::Outcome::DepthLimit { work } => {
                        Some((*work, matches!(&result, route::Outcome::DepthLimit { .. })))
                    }
                    _ => None,
                };
                if let Some((work, depth)) = fallback {
                    self.completed(work)?;
                    if depth {
                        self.audit.depth_rejections += 1;
                    }
                    self.audit.terrain_route_fallback = true;
                    let remaining = self.settings(false)?;
                    result = route::find_undirected_with_push(
                        self.grid,
                        &starts,
                        &ends,
                        remaining,
                        push,
                        self.cancelled,
                    )
                    .map_err(Halt::Invalid)?;
                }
            }
            let Some(nodes) = self.receive(result)? else {
                self.audit.failed_connections += 1;
                self.defer_group(
                    pending[0],
                    &entrances,
                    &mut pending,
                    &mut deferred,
                    &mut accepted,
                )?;
                continue;
            };
            let (start, end) = (nodes[0].cell, nodes.last().unwrap().cell);
            let matches = |id: usize| {
                let p = entrances[id].nodes[0].cell;
                p == start || p == end
            };
            let selected = *pending.iter().find(|&&id| matches(id)).ok_or_else(|| {
                Halt::Invalid("Source layout route missed pending endpoints".into())
            })?;
            self.defer_group(
                selected,
                &entrances,
                &mut pending,
                &mut deferred,
                &mut accepted,
            )?;
            if let Some(index) = deferred.iter().position(|&id| matches(id)) {
                accepted.push(deferred.remove(index));
            }
            self.sample(&nodes, &mut targets)?;
            segments.push(Segment {
                lattice: self.grid.lattice,
                nodes,
                origin: None,
                target: None,
            });
        }
        // A ringless bootstrap builds its own network. The grouped pass may
        // promote an entrance without ever routing it to a retained segment.
        // On the bootstrap retry, give those roots a direct search against
        // the actual network, without other promoted roots competing as goals.
        if self.guided_routes && !self.recovery_targets.is_empty() && !segments.is_empty() {
            self.spend(targets.len())?;
            for &id in &accepted {
                let entrance = &entrances[id];
                let cell = entrance.nodes[0].cell;
                let mut present = false;
                for segment in &segments {
                    self.spend(segment.nodes.len())?;
                    if segment.nodes.iter().any(|node| node.cell == cell) {
                        present = true;
                        break;
                    }
                }
                if present || targets.is_empty() {
                    continue;
                }
                let settings = self.settings(false)?;
                let goals = &targets[..targets.len().min(4096)];
                let result = route::find_undirected(self.grid, &[cell], goals, settings, self.cancelled)
                    .map_err(Halt::Invalid)?;
                if let Some(nodes) = self.receive(result)? {
                    self.sample(&nodes, &mut targets)?;
                    segments.push(Segment {
                        lattice: self.grid.lattice,
                        nodes,
                        origin: None,
                        target: None,
                    });
                }
            }
        }
        for &id in &accepted {
            let entrance = &entrances[id];
            self.audit.accepted_connectors.push(entrance.connector);
            self.spend(segments.len())?;
            let cell = entrance.nodes[0].cell;
            if let Some(segment) = segments
                .iter_mut()
                .find(|s| s.nodes[0].cell == cell || s.nodes.last().unwrap().cell == cell)
            {
                self.spend(segment.nodes.len() + entrance.nodes.len())?;
                self.reserve(entrance.nodes.len())?;
                if segment.nodes[0].cell == cell {
                    let mut prefix: Vec<_> = entrance.nodes.iter().rev().copied().collect();
                    prefix.append(&mut segment.nodes);
                    segment.nodes = prefix;
                    segment.origin = Some(entrance.connector);
                } else {
                    segment.nodes.extend_from_slice(&entrance.nodes);
                    segment.target = Some(entrance.connector);
                }
            } else if self.guided_routes && entrance.nodes.len() >= 2 {
                // An earlier authored chain can turn a shared recovered root
                // into an interior node. Preserve this exit as a separate spur
                // only when its root is in the actual retained network.
                let mut parent = false;
                for segment in &segments {
                    self.spend(segment.nodes.len())?;
                    parent |= segment.nodes.iter().any(|n| n.cell == cell);
                }
                for path in existing {
                    self.spend(path.nodes.len())?;
                    parent |= path.nodes.iter().any(|n| n.cell == cell);
                }
                if parent {
                    self.spend(entrance.nodes.len())?;
                    self.reserve(entrance.nodes.len())?;
                    segments.push(Segment {
                        lattice: self.grid.lattice,
                        nodes: entrance.nodes.clone(),
                        origin: None,
                        target: Some(entrance.connector),
                    });
                    self.audit.shared_entrance_branches += 1;
                }
            }
        }
        segments.retain(|s| {
            if s.nodes.len() < 2 {
                self.audit.discarded_singletons += 1;
                false
            } else {
                true
            }
        });
        let mut attached = vec![false; self.connectors.len()];
        for segment in &segments {
            self.spend(segment.nodes.len())?;
            for id in [segment.origin, segment.target].into_iter().flatten() {
                attached[id] = true;
            }
        }
        let unattached = entrances
            .iter()
            .map(|e| e.connector)
            .filter(|&id| !attached[id])
            .collect();
        self.spend(self.connectors.len())?;
        Ok(Layout {
            lattice: self.grid.lattice,
            segments,
            network_targets: targets,
            recovered_entrances: entrances
                .iter()
                .map(|entrance| RecoveredEntrance {
                    connector: entrance.connector,
                    root: entrance.nodes[0].cell,
                    nodes: entrance.nodes.len(),
                })
                .collect(),
            unrecovered_connectors: unrecovered,
            unattached_connectors: unattached,
            audit: self.audit.clone(),
        })
    }
}

pub fn generate(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    generate_impl(
        grid,
        connectors,
        existing,
        policy,
        search,
        maximum_work,
        false,
        None,
        false,
        &[],
        &[],
        cancelled,
    )
}
/// Independent layout using network-connected entrance recovery. Ordinary
/// routing, source connector ordering and prices remain unchanged. This is not
/// an exact replay of the game's entrance choice and is identified separately.
pub fn generate_connected(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    generate_impl(
        grid,
        connectors,
        existing,
        policy,
        search,
        maximum_work,
        true,
        None,
        false,
        &[],
        &[],
        cancelled,
    )
}
/// Optional independent soft terrain guidance; source entrance recovery is
/// retained when no alternative is found. All ordinary routing is unchanged.
#[allow(clippy::too_many_arguments)]
pub fn generate_connected_with_terrain(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    terrain: route::TerrainRecovery<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    generate_terrain_impl(
        grid,
        connectors,
        existing,
        policy,
        search,
        maximum_work,
        terrain,
        false,
        false,
        cancelled,
    )
}
/// Version two retains terrain-guided entrance choices with an oriented,
/// terrain-aware connecting search. Recovery stays in the original work grant.
#[allow(clippy::too_many_arguments)]
pub fn generate_connected_with_terrain_routed(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    terrain: route::TerrainRecovery<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    generate_terrain_impl(
        grid,
        connectors,
        existing,
        policy,
        search,
        maximum_work,
        terrain,
        true,
        false,
        cancelled,
    )
}
/// Version three can retry an isolated entrance against components reached by
/// its first generated layout. Candidate roads are regenerated from scratch,
/// within the same three-pass query and work grants.
#[allow(clippy::too_many_arguments)]
pub fn generate_connected_with_terrain_bootstrap(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    terrain: route::TerrainRecovery<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    generate_terrain_impl(
        grid,
        connectors,
        existing,
        policy,
        search,
        maximum_work,
        terrain,
        true,
        true,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn generate_terrain_impl(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    terrain: route::TerrainRecovery<'_>,
    guided_routes: bool,
    bootstrap: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let requested_routes = guided_routes;
    policy.validate()?;
    let mut history: Option<Audit> = None;
    let mut exclusions = Vec::new();
    let mut recovery_targets = Vec::new();
    for attempt in 0..3 {
        let work = history.as_ref().map_or(0, |a| a.work);
        let queries = history
            .as_ref()
            .map_or(0, |a| a.entrance_queries + a.layout_queries);
        let outcome = generate_impl(
            grid,
            connectors,
            existing,
            Policy {
                maximum_queries: policy.maximum_queries - queries,
                ..policy
            },
            search,
            maximum_work - work,
            true,
            Some(terrain),
            guided_routes,
            &exclusions,
            &recovery_targets,
            &cancelled,
        )?;
        match outcome {
            Outcome::Found(mut layout) => {
                let current_replaced = layout.audit.terrain_replaced_connectors.clone();
                if let Some(previous) = &history {
                    layout.audit.add_previous(previous)?;
                }
                layout.audit.maximum_work = maximum_work;
                layout.audit.policy = policy;
                layout.audit.terrain_fallback_connectors = exclusions.clone();
                if bootstrap {
                    layout.audit.engine = TERRAIN_BOOTSTRAP_VERSION;
                } else if requested_routes {
                    layout.audit.engine = TERRAIN_ROUTED_VERSION;
                }
                let mut fresh = layout
                    .unattached_connectors
                    .iter()
                    .copied()
                    .filter(|id| current_replaced.contains(id) && !exclusions.contains(id))
                    .collect::<Vec<_>>();
                // Prefer a local restoration. If the cause is indirect, or
                // only the final bounded pass remains, restore all remaining
                // replaced entrances rather than silently dropping a socket.
                if !layout.unattached_connectors.is_empty() && (fresh.is_empty() || attempt == 1) {
                    fresh = current_replaced
                        .into_iter()
                        .filter(|id| !exclusions.contains(id))
                        .collect();
                }
                let recover_bootstrap = bootstrap
                    && existing.is_empty()
                    && recovery_targets.is_empty()
                    && !layout.unattached_connectors.is_empty()
                    && !layout.network_targets.is_empty()
                    && layout.network_targets.len() <= 4096;
                if (!recover_bootstrap && fresh.is_empty())
                    || !layout.unrecovered_connectors.is_empty()
                    || attempt == 2
                    || layout.audit.work >= maximum_work
                    || layout.audit.entrance_queries + layout.audit.layout_queries
                        >= policy.maximum_queries
                {
                    return Ok(Outcome::Found(layout));
                }
                if recover_bootstrap {
                    let remaining = maximum_work - layout.audit.work;
                    if layout.network_targets.len() >= remaining {
                        return Ok(Outcome::Found(layout));
                    }
                    layout.audit.work += layout.network_targets.len();
                    recovery_targets = layout.network_targets.clone();
                } else {
                    exclusions.extend(fresh);
                    exclusions.sort_unstable();
                    exclusions.dedup();
                }
                history = Some(layout.audit);
            }
            outcome @ (Outcome::Budget(_) | Outcome::Cancelled(_)) => {
                let cancelled_outcome = matches!(&outcome, Outcome::Cancelled(_));
                let mut audit = match outcome {
                    Outcome::Budget(audit) | Outcome::Cancelled(audit) => audit,
                    Outcome::Found(_) => unreachable!(),
                };
                if let Some(previous) = &history {
                    audit.add_previous(previous)?;
                }
                audit.maximum_work = maximum_work;
                audit.policy = policy;
                if bootstrap {
                    audit.engine = TERRAIN_BOOTSTRAP_VERSION;
                } else if requested_routes {
                    audit.engine = TERRAIN_ROUTED_VERSION;
                }
                audit.terrain_fallback_connectors = exclusions;
                return Ok(if cancelled_outcome {
                    Outcome::Cancelled(audit)
                } else {
                    Outcome::Budget(audit)
                });
            }
        }
    }
    unreachable!("The final entrance-layout attempt returns its outcome")
}
#[allow(clippy::too_many_arguments)]
fn generate_impl(
    grid: &CellGrid,
    connectors: &[Connector],
    existing: &[ExistingPath<'_>],
    policy: Policy,
    search: Settings,
    maximum_work: usize,
    connected_entrances: bool,
    terrain: Option<route::TerrainRecovery<'_>>,
    guided_routes: bool,
    terrain_exclusions: &[usize],
    recovery_targets: &[[usize; 2]],
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    policy.validate()?;
    let n = grid.lattice.resolution();
    if grid.grid.engine != CELL_VERSION
        || grid.grid.resolution != n
        || grid.grid.costs.len() != n * n
        || connectors.len() > 1024
        || existing.len() > 1024
        || recovery_targets.len() > 4096
        || recovery_targets.iter().flatten().any(|&v| v >= n)
        || (guided_routes && terrain.is_none())
        || terrain.is_some_and(|t| t.lattice() != grid.lattice || !t.matches_connectors(connectors.len()))
        || terrain_exclusions.len() > connectors.len()
        || terrain_exclusions.iter().any(|&i| i >= connectors.len())
        || terrain_exclusions.windows(2).any(|w| w[0] >= w[1])
        || !(1..=512_000_000).contains(&maximum_work)
        || !(1..=128_000_000).contains(&search.maximum_work)
        || !(1..=1_000_000).contains(&search.maximum_expansions)
        || search.directional_penalty > 1_000_000
        || !(1..=4).contains(&search.directional_radius)
        || connectors.iter().any(|c| {
            c.cell.iter().chain(&c.centre).any(|&v| v >= n) || !(1..=1024).contains(&c.radius)
        })
    {
        return Err("Invalid source connector layout request".into());
    }
    let mut context = Context {
        grid,
        connectors,
        cancelled: &cancelled,
        node_storage: 0,
        connected_entrances,
        terrain,
        terrain_exclusions,
        guided_routes,
        recovery_targets,
        audit: Audit {
            engine: if guided_routes {
                TERRAIN_ROUTED_VERSION
            } else if terrain.is_some() {
                TERRAIN_CONNECTED_VERSION
            } else if connected_entrances {
                CONNECTED_VERSION
            } else {
                VERSION
            },
            policy,
            search,
            maximum_work,
            work: 0,
            entrance_queries: 0,
            terrain_entrance_queries: 0,
            terrain_route_queries: 0,
            shared_entrance_branches: 0,
            terrain_route_traces: Vec::new(),
            terrain_route_fallback: false,
            terrain_entrance_replacements: 0,
            terrain_replaced_connectors: Vec::new(),
            terrain_fallback_connectors: Vec::new(),
            terrain_preparation_work: 0,
            connected_goal_preparations: 0,
            connected_goal_work: 0,
            bootstrap_recovery_targets: recovery_targets.len(),
            layout_queries: 0,
            depth_rejections: 0,
            failed_connections: 0,
            skipped_roadside: 0,
            promoted_groups: Vec::new(),
            accepted_connectors: Vec::new(),
            discarded_singletons: 0,
        },
    };
    let result = (|| {
        if let Some(terrain) = terrain {
            context.spend(terrain.preparation_work())?;
            context.audit.terrain_preparation_work = terrain.preparation_work();
        }
        let mut groups = BTreeMap::new();
        for c in connectors {
            context.spend(1)?;
            let info = (c.centre, c.radius, c.roadside);
            if groups
                .insert(c.monument, info)
                .is_some_and(|old| old != info)
            {
                return Err(Halt::Invalid("Inconsistent monument group metadata".into()));
            }
        }
        let mut existing_count = 0usize;
        for path in existing {
            if path.lattice != grid.lattice {
                return Err(Halt::Invalid(
                    "Existing road and source layout lattices differ".into(),
                ));
            }
            let nodes = path.nodes;
            if nodes.len() > policy.maximum_node_storage.saturating_sub(existing_count) {
                return Err(Halt::Budget);
            }
            existing_count += nodes.len();
            for node in nodes {
                context.spend(1)?;
                if node.cell.iter().any(|&v| v >= n) {
                    return Err(Halt::Invalid("Invalid existing raw road cell".into()));
                }
            }
        }
        for chunk in grid.grid.costs.chunks(1024) {
            context.spend(chunk.len())?;
            if chunk.iter().any(|&v| v > 1_000_000) {
                return Err(Halt::Invalid("Invalid source road price".into()));
            }
        }
        context.generate(existing)
    })();
    match result {
        Ok(layout) => Ok(Outcome::Found(layout)),
        Err(Halt::Budget) => Ok(Outcome::Budget(context.audit)),
        Err(Halt::Cancelled) => Ok(Outcome::Cancelled(context.audit)),
        Err(Halt::Invalid(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road_costmap::{Counts, Grid};
    #[test]
    fn bootstrap_recovery_joins_an_isolated_exit_without_changing_old_layouts_or_costs() {
        let mut costs = vec![0; 17 * 17];
        for z in 0..17 {
            costs[z * 17 + 2] = 100;
        }
        costs[8 * 17 + 9] = 100;
        let g = grid(17, costs);
        let before = g.grid.costs.clone();
        let connectors = [[2, 2], [2, 14], [10, 8]]
            .into_iter()
            .enumerate()
            .map(|(id, cell)| Connector {
                monument: id as u32 + 1,
                cell,
                centre: cell,
                radius: 1,
                roadside: false,
            })
            .collect::<Vec<_>>();
        let heights = vec![20.; 17 * 17];
        let terrain = route::TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 16).unwrap();
        let p = Policy {
            network_sample_stride: 1,
            ..policy()
        };
        let Outcome::Found(old) = generate_connected_with_terrain_routed(
            &g,
            &connectors,
            &[],
            p,
            settings(),
            128_000_000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(old.unattached_connectors, vec![2]);
        assert_eq!(old.recovered_entrances[2].root, [9, 8]);
        assert_eq!(old.audit.bootstrap_recovery_targets, 0);

        let Outcome::Found(recovered) = generate_connected_with_terrain_bootstrap(
            &g,
            &connectors,
            &[],
            p,
            settings(),
            128_000_000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(recovered.audit.engine, TERRAIN_BOOTSTRAP_VERSION);
        assert!(recovered.unattached_connectors.is_empty());
        assert!(recovered.unrecovered_connectors.is_empty());
        assert!(recovered.audit.bootstrap_recovery_targets > 0);
        assert_eq!(recovered.recovered_entrances[2].root[0], 2);
        assert!(recovered.audit.work <= 128_000_000);
        assert!(
            recovered.audit.entrance_queries + recovered.audit.layout_queries <= p.maximum_queries
        );
        assert_eq!(g.grid.costs, before);

        let retry_limit = old.audit.work + old.network_targets.len() + 1;
        let Outcome::Budget(audit) = generate_connected_with_terrain_bootstrap(
            &g,
            &connectors,
            &[],
            p,
            settings(),
            retry_limit,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!("A retry must stay inside the original work grant")
        };
        assert_eq!(audit.engine, TERRAIN_BOOTSTRAP_VERSION);
        assert!(audit.work <= retry_limit);
        let Outcome::Cancelled(audit) = generate_connected_with_terrain_bootstrap(
            &g,
            &connectors,
            &[],
            p,
            settings(),
            128_000_000,
            terrain,
            || true,
        )
        .unwrap() else {
            panic!("Cancellation must not publish a candidate")
        };
        assert_eq!(audit.engine, TERRAIN_BOOTSTRAP_VERSION);
        assert_eq!(audit.work, 0);

        // A physical hole barrier stays forbidden even when another component
        // is reachable in the ordinary source-cost graph.
        let mut holes = vec![0; 17 * 17];
        for z in 0..17 {
            holes[z * 17 + 6] = 1;
        }
        let blocked = terrain.with_entrance_exclusions(&holes).unwrap();
        let Outcome::Found(rejected) = generate_connected_with_terrain_bootstrap(
            &g,
            &connectors,
            &[],
            p,
            settings(),
            128_000_000,
            blocked,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert!(
            rejected.unattached_connectors.contains(&2)
                || rejected.unrecovered_connectors.contains(&2)
        );
        assert!(
            rejected
                .segments
                .iter()
                .flat_map(|s| &s.nodes)
                .all(|node| holes[node.cell[1] * 17 + node.cell[0]] == 0)
        );
        assert_eq!(g.grid.costs, before);
    }

    #[test]
    fn roadside_only_family_does_not_prepare_unused_connectivity() {
        let g = grid(9, vec![100; 81]);
        let nodes = (0..8)
            .map(|z| RawNode {
                cell: [1, z],
                cost: 1,
            })
            .collect::<Vec<_>>();
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let connectors = [Connector {
            monument: 1,
            cell: [4, 4],
            centre: [4, 4],
            radius: 1,
            roadside: true,
        }];
        let Outcome::Found(layout) = generate_connected(
            &g,
            &connectors,
            &existing,
            Policy {
                network_sample_stride: 8,
                push_multiplier: 1,
                maximum_queries: 8,
                maximum_node_storage: 1000,
            },
            settings(),
            256,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(layout.audit.connected_goal_preparations, 0);
        assert_eq!(layout.audit.entrance_queries, 0);
        assert!(layout.recovered_entrances.is_empty());
        assert!(layout.segments.is_empty());
    }
    #[test]
    fn connected_layout_recovers_isolated_harbor_entrance_to_existing_network() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9 + 1] = 100;
        }
        costs[4 * 9 + 4] = 100;
        let g = grid(9, costs);
        let before = g.grid.costs.clone();
        let connectors = [Connector {
            monument: 1,
            cell: [4, 5],
            centre: [4, 5],
            radius: 1,
            roadside: false,
        }];
        let nodes = [
            RawNode {
                cell: [1, 0],
                cost: 1,
            },
            RawNode {
                cell: [1, 1],
                cost: 2,
            },
        ];
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let p = Policy {
            network_sample_stride: 1,
            ..policy()
        };
        let Outcome::Found(original) =
            generate(&g, &connectors, &existing, p, settings(), 128000000, || {
                false
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(original.unattached_connectors, vec![0]);
        assert_eq!(original.recovered_entrances.len(), 1);
        assert_eq!(original.recovered_entrances[0].connector, 0);
        assert_eq!(original.recovered_entrances[0].root, [4, 4]);
        assert_eq!(original.recovered_entrances[0].nodes, 2);
        let Outcome::Found(connected) =
            generate_connected(&g, &connectors, &existing, p, settings(), 128000000, || {
                false
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(connected.unattached_connectors.is_empty());
        assert_eq!(connected.recovered_entrances.len(), 1);
        assert_eq!(connected.recovered_entrances[0].connector, 0);
        assert_eq!(connected.recovered_entrances[0].root[0], 1);
        assert!(connected.recovered_entrances[0].nodes > 2);
        assert_eq!(connected.audit.engine, CONNECTED_VERSION);
        assert_eq!(connected.segments.len(), 1);
        assert!(connected.segments[0].nodes.iter().any(|n| n.cell == [4, 5]));
        assert!(connected.segments[0].nodes.iter().any(|n| n.cell[0] == 1));
        assert_eq!(g.grid.costs, before);
    }
    #[test]
    fn routed_entrances_detour_around_holes_and_never_fall_back_through_them() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9] = 100;
        }
        let g = grid(9, costs);
        let original = g.grid.costs.clone();
        let connectors = [Connector {
            monument: 1,
            cell: [8, 4],
            centre: [8, 4],
            radius: 8,
            roadside: false,
        }];
        let nodes = (0..9)
            .map(|z| RawNode {
                cell: [0, z],
                cost: 1,
            })
            .collect::<Vec<_>>();
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let heights = vec![0.; 81];
        for sealed in [false, true] {
            let mut mask = vec![0; 81];
            for z in 0..9 {
                if sealed || z != 2 {
                    mask[z * 9 + 2] = 1;
                }
            }
            let terrain = route::TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 4)
                .unwrap()
                .with_entrance_exclusions(&mask)
                .unwrap();
            let Outcome::Found(result) = generate_connected_with_terrain_routed(
                &g,
                &connectors,
                &existing,
                Policy {
                    network_sample_stride: 1,
                    ..policy()
                },
                settings(),
                128000000,
                terrain,
                || false,
            )
            .unwrap() else {
                panic!();
            };
            if sealed {
                assert_eq!(result.unrecovered_connectors, vec![0]);
                assert!(result.segments.is_empty());
            } else {
                assert!(result.unrecovered_connectors.is_empty());
                assert!(result.unattached_connectors.is_empty());
                assert!(result.audit.terrain_entrance_replacements > 0);
                assert!(
                    result
                        .segments
                        .iter()
                        .flat_map(|s| &s.nodes)
                        .all(|v| mask[v.cell[1] * 9 + v.cell[0]] == 0)
                );
            }
            let Outcome::Found(fallback) = generate_impl(
                &g,
                &connectors,
                &existing,
                Policy {
                    network_sample_stride: 1,
                    ..policy()
                },
                settings(),
                128000000,
                true,
                Some(terrain),
                true,
                &[0],
                &[],
                || false,
            )
            .unwrap() else {
                panic!();
            };
            assert_eq!(
                fallback.unrecovered_connectors,
                result.unrecovered_connectors
            );
            assert_eq!(fallback.unattached_connectors, result.unattached_connectors);
            assert!(
                fallback
                    .segments
                    .iter()
                    .flat_map(|s| &s.nodes)
                    .all(|v| mask[v.cell[1] * 9 + v.cell[0]] == 0)
            );
            assert_eq!(g.grid.costs, original);
        }
    }
    #[test]
    fn routed_layout_preserves_distinct_entrances_recovered_to_one_junction() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9] = 100;
        }
        costs[4 * 9 + 1] = 100;
        let g = grid(9, costs);
        let before = g.grid.costs.clone();
        let connectors = [[5, 3], [5, 5]].map(|cell| Connector {
            monument: 1,
            cell,
            centre: [5, 4],
            radius: 2,
            roadside: false,
        });
        let nodes = (0..9)
            .map(|z| RawNode {
                cell: [0, z],
                cost: 1,
            })
            .collect::<Vec<_>>();
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let heights = vec![10.; 81];
        let terrain = route::TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 1).unwrap();
        let p = Policy {
            network_sample_stride: 1,
            ..policy()
        };
        let Outcome::Found(layout) = generate_connected_with_terrain_routed(
            &g,
            &connectors,
            &existing,
            p,
            settings(),
            128000000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert!(
            layout.unattached_connectors.is_empty(),
            "{:?}",
            layout.unattached_connectors
        );
        assert!(layout.unrecovered_connectors.is_empty());
        assert!(layout.audit.terrain_fallback_connectors.is_empty());
        let mut retained = layout
            .segments
            .iter()
            .flat_map(|s| [s.origin, s.target])
            .flatten()
            .collect::<Vec<_>>();
        retained.sort_unstable();
        assert_eq!(retained, vec![0, 1]);
        assert_eq!(g.grid.costs, before);
    }
    #[test]
    fn flat_coastal_entrances_use_soft_dry_corridor_guidance() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9] = 100;
        }
        let g = grid(9, costs);
        let connectors = [Connector {
            monument: 1,
            cell: [8, 4],
            centre: [8, 4],
            radius: 1,
            roadside: false,
        }];
        let heights = vec![10.; 81];
        let mut coast = vec![0; 81];
        for z in 1..9 {
            coast[z * 9 + 4] = 1;
        }
        let terrain = route::TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 4)
            .unwrap()
            .with_entrance_penalties(&coast)
            .unwrap();
        let nodes = [
            RawNode {
                cell: [0, 0],
                cost: 1,
            },
            RawNode {
                cell: [0, 1],
                cost: 2,
            },
        ];
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let Outcome::Found(result) = generate_connected_with_terrain_bootstrap(
            &g,
            &connectors,
            &existing,
            Policy {
                network_sample_stride: 1,
                ..policy()
            },
            settings(),
            128000000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!("Expected a connected coastal entrance")
        };
        assert!(result.audit.terrain_entrance_queries > 0);
        assert!(result.audit.terrain_entrance_replacements > 0);
        assert_eq!(
            result.audit.terrain_preparation_work,
            heights.len() + coast.len()
        );
        assert!(result.unrecovered_connectors.is_empty());
        assert!(result.unattached_connectors.is_empty());
        assert!(
            result
                .segments
                .iter()
                .flat_map(|s| &s.nodes)
                .any(|node| node.cell == connectors[0].cell)
        );
        assert!(
            result
                .segments
                .iter()
                .flat_map(|s| &s.nodes)
                .all(|node| coast[node.cell[1] * 9 + node.cell[0]] == 0)
        );
    }
    #[test]
    fn terrain_guided_layout_retains_the_authored_exit_and_connects_via_a_gentle_detour() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9 + 1] = 100;
        }
        let g = grid(9, costs);
        let original = g.grid.costs.clone();
        let connectors = [Connector {
            monument: 1,
            cell: [5, 4],
            centre: [5, 4],
            radius: 1,
            roadside: false,
        }];
        let nodes = [
            RawNode {
                cell: [1, 0],
                cost: 1,
            },
            RawNode {
                cell: [1, 1],
                cost: 2,
            },
        ];
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let mut heights = vec![6.; 81];
        for z in 0..9 {
            heights[z * 9 + 1] = 0.;
        }
        for [x, z] in [[5, 4], [5, 5], [5, 6], [4, 7], [3, 7], [2, 7]] {
            heights[z * 9 + x] = 0.;
        }
        let terrain = route::TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        let policy = Policy {
            network_sample_stride: 1,
            ..policy()
        };
        let Outcome::Found(result) = generate_connected_with_terrain(
            &g,
            &connectors,
            &existing,
            policy,
            settings(),
            128000000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(result.audit.engine, TERRAIN_CONNECTED_VERSION);
        assert_eq!(result.audit.terrain_entrance_queries, 1);
        assert_eq!(result.audit.terrain_entrance_replacements, 1);
        assert_eq!(result.audit.terrain_preparation_work, 81);
        assert!(result.unattached_connectors.is_empty());
        assert!(result.unrecovered_connectors.is_empty());
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].target, Some(0));
        assert!(result.segments[0].nodes.iter().any(|n| n.cell == [5, 4]));
        assert!(
            result.segments[0]
                .nodes
                .iter()
                .all(|n| heights[n.cell[1] * 9 + n.cell[0]] == 0.)
        );
        assert_eq!(g.grid.costs, original);
        assert!(matches!(
            generate_connected_with_terrain(
                &g,
                &connectors,
                &existing,
                policy,
                settings(),
                128000000,
                terrain,
                || true
            )
            .unwrap(),
            Outcome::Cancelled(_)
        ));
    }
    #[test]
    fn terrain_guidance_restores_the_original_exit_if_its_detour_cannot_attach_in_budget() {
        let n = 31;
        let mut prices = vec![100; n * n];
        let mut heights = vec![6.; n * n];
        for z in 15..n {
            prices[z * n + 16] = 0;
            heights[z * n + 16] = 0.;
        }
        heights[30 * n + 15] = 0.;
        let g = grid(n, prices);
        let original = g.grid.costs.clone();
        let connectors = [Connector {
            monument: 1,
            cell: [16, 15],
            centre: [16, 15],
            radius: 1,
            roadside: false,
        }];
        let nodes = [
            RawNode {
                cell: [15, 15],
                cost: 1,
            },
            RawNode {
                cell: [15, 14],
                cost: 2,
            },
        ];
        let existing = [ExistingPath {
            lattice: g.lattice,
            nodes: &nodes,
        }];
        let terrain = route::TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 16).unwrap();
        let p = Policy {
            network_sample_stride: 1,
            ..policy()
        };
        let settings = Settings {
            maximum_expansions: 100,
            ..settings()
        };
        let Outcome::Found(result) = generate_connected_with_terrain(
            &g,
            &connectors,
            &existing,
            p,
            settings,
            128000000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert!(result.unattached_connectors.is_empty());
        assert!(result.unrecovered_connectors.is_empty());
        assert_eq!(result.audit.terrain_fallback_connectors, vec![0]);
        assert!(result.audit.terrain_entrance_replacements > 0);
        let Outcome::Found(routed) = generate_connected_with_terrain_routed(
            &g,
            &connectors,
            &existing,
            p,
            settings,
            128000000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert!(routed.unattached_connectors.is_empty());
        assert!(routed.unrecovered_connectors.is_empty());
        assert!(routed.audit.terrain_fallback_connectors.is_empty());
        assert!(!routed.audit.terrain_route_fallback);
        assert!(routed.audit.terrain_route_queries > 0);
        assert_eq!(routed.audit.engine, TERRAIN_ROUTED_VERSION);
        assert!(routed.segments[0].nodes.len() > result.segments[0].nodes.len());
        assert!(routed.segments[0].nodes.iter().any(|n| n.cell == [16, 15]));
        let limited = Policy {
            maximum_node_storage: 24,
            ..p
        };
        let Outcome::Found(fallback) = generate_connected_with_terrain_routed(
            &g,
            &connectors,
            &existing,
            limited,
            settings,
            128000000,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!()
        };
        assert!(fallback.unattached_connectors.is_empty());
        assert!(fallback.unrecovered_connectors.is_empty());
        assert!(fallback.audit.terrain_route_fallback);
        assert_eq!(fallback.audit.terrain_fallback_connectors, vec![0]);
        assert!(fallback.audit.work > routed.audit.work && fallback.audit.work <= 128000000);
        assert!(
            fallback.audit.entrance_queries + fallback.audit.layout_queries <= p.maximum_queries
        );
        assert_eq!(result.segments.len(), 1);
        assert!(result.segments[0].target == Some(0) || result.segments[0].origin == Some(0));
        assert!(result.segments[0].nodes.iter().any(|n| n.cell == [16, 15]));
        assert!(result.audit.work < 128000000);
        assert!(result.audit.entrance_queries + result.audit.layout_queries <= p.maximum_queries);
        assert_eq!(g.grid.costs, original);
        let Outcome::Budget(budget) = generate_connected_with_terrain(
            &g,
            &connectors,
            &existing,
            p,
            settings,
            result.audit.work - 1,
            terrain,
            || false,
        )
        .unwrap() else {
            panic!("retry budget was reset")
        };
        assert!(budget.work < result.audit.work);
        assert_eq!(g.grid.costs, original);
    }
    fn grid(n: usize, costs: Vec<u32>) -> CellGrid {
        CellGrid {
            lattice: CellLattice::new(n as f32, 1.).unwrap(),
            grid: Grid {
                engine: CELL_VERSION,
                resolution: n,
                seed: 0,
                trail: false,
                counts: Counts::default(),
                work: 0,
                costs,
            },
        }
    }
    fn policy() -> Policy {
        Policy {
            network_sample_stride: 8,
            push_multiplier: 50000,
            maximum_queries: 8192,
            maximum_node_storage: 262144,
        }
    }
    fn settings() -> Settings {
        Settings {
            maximum_expansions: 100000,
            maximum_work: 128_000_000,
            directional_penalty: 10000,
            directional_radius: 1,
        }
    }
    #[test]
    fn segment_conversion_uses_authored_endpoints_and_shape_failures_are_atomic() {
        use std::cell::{Cell, RefCell};
        let segment = Segment {
            lattice: CellLattice::new(1000., 7.5).unwrap(),
            nodes: vec![
                RawNode {
                    cell: [10, 10],
                    cost: 100,
                },
                RawNode {
                    cell: [50, 50],
                    cost: 1,
                },
            ],
            origin: Some(0),
            target: Some(1),
        };
        let settings = path_geometry::Settings {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let endpoints = [[-422.25, -421.75], [-120.5, -119.5]];
        let calls = Cell::new(0);
        let samples = RefCell::new(Vec::new());
        let shaped = segment
            .shape(
                settings,
                |id| {
                    calls.set(calls.get() + 1);
                    Ok(endpoints[id])
                },
                |uv| {
                    samples.borrow_mut().push(uv);
                    Ok(-2.)
                },
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(calls.get(), 2);
        assert!(shaped.start && shaped.end);
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/source-road-segment-endpoints-bd19c4ac.json"
        ))
        .unwrap();
        let expected: Vec<[f32; 3]> =
            serde_json::from_value(fixture["cases"][0]["expected"]["points"].clone()).unwrap();
        assert_eq!(shaped.path.points().len(), expected.len());
        for (actual, source) in shaped.path.points().iter().zip(expected) {
            // The installed source itself accumulates ~0.16 mm at the final
            // resampled point in this case. Compare its actual output instead
            // of asserting an exact endpoint property the source does not have.
            for axis in 0..3 {
                assert!((actual[axis] - source[axis]).abs() < 0.001);
            }
            assert_eq!(actual[1], 1.);
        }
        for (uv, endpoint) in samples.borrow().iter().zip(endpoints) {
            for axis in 0..2 {
                assert!(((-500. + uv[axis] * 1000.) - endpoint[axis]).abs() < 0.0001);
            }
        }
        assert!(
            segment
                .shape(settings, |_| Ok([f32::NAN, 0.]), |_| Ok(20.), || false)
                .is_err()
        );
        assert!(
            segment
                .shape(settings, |id| Ok(endpoints[id]), |_| Ok(f32::NAN), || false)
                .is_err()
        );
        let polls = Cell::new(0);
        let before = segment.nodes.clone();
        assert!(
            segment
                .shape(
                    settings,
                    |id| Ok(endpoints[id]),
                    |_| Ok(20.),
                    || {
                        polls.set(polls.get() + 1);
                        polls.get() > 1
                    }
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(segment.nodes, before);
        assert!(
            segment
                .shape(
                    path_geometry::Settings {
                        maximum_work: 1,
                        ..settings
                    },
                    |_| panic!("Invalid work grant must be rejected before sampling"),
                    |_| Ok(20.),
                    || false
                )
                .is_err()
        );
    }
    #[test]
    fn owned_group_control_study_matches_raw_nodes_endpoints_and_sampling_order() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/source-road-layout-bd19c4ac.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let input = &case["input"];
            let rows = input["Rows"].as_array().unwrap();
            let mut g = grid(
                rows.len(),
                rows.iter()
                    .flat_map(|r| r.as_array().unwrap())
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect(),
            );
            g.lattice =
                CellLattice::new(case["synthetic_world_size"].as_f64().unwrap() as f32, 7.5)
                    .unwrap();
            let connectors: Vec<_> = input["Connectors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| Connector {
                    monument: c["Monument"].as_u64().unwrap() as u32,
                    cell: serde_json::from_value(c["Cell"].clone()).unwrap(),
                    centre: serde_json::from_value(c["Centre"].clone()).unwrap(),
                    radius: c["Radius"].as_u64().unwrap() as usize,
                    roadside: c["Roadside"].as_bool().unwrap(),
                })
                .collect();
            let cells: Vec<Vec<[usize; 2]>> =
                serde_json::from_value(input["Existing"].clone()).unwrap();
            let existing: Vec<Vec<_>> = cells
                .into_iter()
                .map(|p| {
                    p.into_iter()
                        .map(|cell| RawNode { cell, cost: 100 })
                        .collect()
                })
                .collect();
            let Outcome::Found(layout) = generate(
                &g,
                &connectors,
                &existing
                    .iter()
                    .map(|nodes| ExistingPath {
                        lattice: g.lattice,
                        nodes,
                    })
                    .collect::<Vec<_>>(),
                Policy {
                    network_sample_stride: input["Stride"].as_u64().unwrap() as usize,
                    push_multiplier: input["PushMultiplier"].as_u64().unwrap() as u32,
                    ..policy()
                },
                Settings {
                    maximum_expansions: case["source_depth"].as_u64().unwrap() as usize,
                    ..settings()
                },
                128_000_000,
                || false,
            )
            .unwrap() else {
                panic!("Missing layout {}", input["Name"]);
            };
            let expected = case["segments"].as_array().unwrap();
            assert_eq!(layout.segments.len(), expected.len(), "{}", input["Name"]);
            for (segment, expected) in layout.segments.iter().zip(expected) {
                let cells: Vec<[usize; 2]> =
                    serde_json::from_value(expected["cells"].clone()).unwrap();
                let costs: Vec<u64> = serde_json::from_value(expected["costs"].clone()).unwrap();
                assert_eq!(
                    segment.nodes.iter().map(|n| n.cell).collect::<Vec<_>>(),
                    cells,
                    "{}",
                    input["Name"]
                );
                assert_eq!(
                    segment.nodes.iter().map(|n| n.cost).collect::<Vec<_>>(),
                    costs,
                    "{}",
                    input["Name"]
                );
                assert_eq!(
                    serde_json::to_value(segment.origin).unwrap(),
                    expected["origin"]
                );
                assert_eq!(
                    serde_json::to_value(segment.target).unwrap(),
                    expected["target"]
                );
                let endpoints: Vec<[f32; 2]> =
                    serde_json::from_value(case["synthetic_endpoint_positions"].clone()).unwrap();
                let before = segment.nodes.clone();
                let shaped = segment
                    .shape(
                        path_geometry::Settings {
                            horizontal_iterations: 4,
                            vertical_iterations: 16,
                            resample_distance: 7.5,
                            maximum_work: 2_000_000,
                        },
                        |id| Ok(endpoints[id]),
                        |uv| Ok(20. + uv[0] * 4. + uv[1] * 3.),
                        || false,
                    )
                    .unwrap()
                    .unwrap();
                let points: Vec<[f32; 3]> =
                    serde_json::from_value(expected["shaped"]["points"].clone()).unwrap();
                let tangents: Vec<[f32; 3]> =
                    serde_json::from_value(expected["shaped"]["tangents"].clone()).unwrap();
                assert_eq!(
                    shaped.path.points().len(),
                    points.len(),
                    "{}",
                    input["Name"]
                );
                assert_eq!(shaped.path.tangents().len(), tangents.len());
                for (actual, expected) in shaped
                    .path
                    .points()
                    .iter()
                    .flatten()
                    .zip(points.iter().flatten())
                {
                    assert!(
                        (actual - expected).abs() <= 0.001,
                        "{} point error {}",
                        input["Name"],
                        (actual - expected).abs()
                    );
                }
                for (actual, expected) in shaped
                    .path
                    .tangents()
                    .iter()
                    .flatten()
                    .zip(tangents.iter().flatten())
                {
                    assert!(
                        (actual - expected).abs() <= 0.00001,
                        "{} tangent error {}",
                        input["Name"],
                        (actual - expected).abs()
                    );
                }
                assert!(
                    (shaped.path.length() - expected["shaped"]["length"].as_f64().unwrap() as f32)
                        .abs()
                        <= 0.01
                );
                assert_eq!(
                    shaped.path.is_circular(),
                    expected["shaped"]["circular"].as_bool().unwrap()
                );
                assert_eq!(shaped.start, expected["shaped"]["start"].as_bool().unwrap());
                assert_eq!(shaped.end, expected["shaped"]["end"].as_bool().unwrap());
                assert_eq!(segment.nodes, before);
                assert!(shaped.total_work <= 2_000_000);
            }
            assert_eq!(
                serde_json::to_value(&layout.network_targets).unwrap(),
                case["network_targets"],
                "{}",
                input["Name"]
            );
            assert_eq!(
                serde_json::to_value(&layout.unrecovered_connectors).unwrap(),
                case["unrecovered_connectors"]
            );
            assert_eq!(
                serde_json::to_value(&layout.unattached_connectors).unwrap(),
                case["unattached_connectors"],
                "{}",
                input["Name"]
            );
            let audit = serde_json::to_value(&layout.audit).unwrap();
            for key in [
                "entrance_queries",
                "layout_queries",
                "failed_connections",
                "skipped_roadside",
                "promoted_groups",
                "accepted_connectors",
                "discarded_singletons",
            ] {
                assert_eq!(audit[key], case[key], "{} {key}", input["Name"]);
            }
            assert!(layout.audit.work <= 128_000_000);
        }
    }
    #[test]
    fn cancellation_caps_and_invalid_groups_never_publish_partial_layouts() {
        use std::cell::Cell;
        let g = grid(33, vec![100; 33 * 33]);
        let connectors: Vec<_> = [[4, 4], [26, 4], [16, 26]]
            .into_iter()
            .enumerate()
            .map(|(id, cell)| Connector {
                monument: id as u32,
                cell,
                centre: cell,
                radius: 3,
                roadside: false,
            })
            .collect();
        let before = g.grid.costs.clone();
        let polls = Cell::new(0);
        assert!(
            matches!(generate(&g, &connectors, &[], policy(), settings(), 128_000_000, || {
            polls.set(polls.get()+1); polls.get()>100
        }).unwrap(), Outcome::Cancelled(a) if a.work > 33 * 33)
        );
        assert_eq!(before, g.grid.costs);
        assert!(matches!(
            generate(&g, &connectors, &[], policy(), settings(), 1, || false).unwrap(),
            Outcome::Budget(_)
        ));
        assert!(
            matches!(generate(&g, &connectors, &[], policy(), Settings { maximum_work: 1, ..settings() },
            128_000_000, || false).unwrap(), Outcome::Budget(a) if a.depth_rejections == 0)
        );
        assert!(matches!(
            generate(
                &g,
                &connectors,
                &[],
                Policy {
                    maximum_queries: 1,
                    ..policy()
                },
                settings(),
                128_000_000,
                || false
            )
            .unwrap(),
            Outcome::Budget(_)
        ));
        assert!(matches!(
            generate(
                &g,
                &connectors,
                &[],
                Policy {
                    maximum_node_storage: 2,
                    ..policy()
                },
                settings(),
                128_000_000,
                || false
            )
            .unwrap(),
            Outcome::Budget(_)
        ));
        let mut invalid = connectors.clone();
        invalid[1].monument = invalid[0].monument;
        assert!(
            generate(&g, &invalid, &[], policy(), settings(), 128_000_000, || {
                false
            })
            .is_err()
        );
        invalid[1] = connectors[1];
        invalid[0].cell = [33, 0];
        assert!(
            generate(&g, &invalid, &[], policy(), settings(), 128_000_000, || {
                false
            })
            .is_err()
        );
        let too_many_targets = vec![
            RawNode {
                cell: [2, 2],
                cost: 1
            };
            8 * 4097
        ];
        assert!(matches!(
            generate(
                &g,
                &connectors,
                &[ExistingPath {
                    lattice: g.lattice,
                    nodes: &too_many_targets
                }],
                policy(),
                settings(),
                128_000_000,
                || false
            )
            .unwrap(),
            Outcome::Budget(_)
        ));
        assert!(
            generate(
                &g,
                &connectors,
                &[ExistingPath {
                    lattice: CellLattice::new(33., 1.5).unwrap(),
                    nodes: &too_many_targets[..8],
                }],
                policy(),
                settings(),
                128_000_000,
                || false
            )
            .is_err()
        );
    }
}
