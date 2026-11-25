//! Typed handoff from ordered source layout to retained planning. This is not
//! an entrance, terrain, road-surface or gameplay acceptance certificate.
use crate::source_road_inputs::{self, Prepared};
use papyrus_terrain::{
    path_geometry::Path,
    road_costmap::CellLattice,
    road_network::{Destination, Road},
    source_road_junctions::{self as junctions, Settings},
    source_road_layout::Layout,
    source_road_ring::RawNode,
};
use serde::Serialize;
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-source-road-handoff-2";
pub const STRIP_CHECKED_VERSION: &str = "papyrus-source-road-handoff-3";
pub const SOURCE_CHECKED_VERSION: &str = "papyrus-source-road-handoff-4";
pub const RAW_VERSION: &str = "papyrus-source-road-handoff-5";
pub const JUNCTION_CHECKED_VERSION: &str = "papyrus-source-road-handoff-6";
/// Bounded numerical endpoint drift from source f32 resampling; not a socket
/// relocation allowance. The immutable shaped terminal itself is retained.
pub const MAXIMUM_AUTHORED_ROUNDING_METRES: f64 = 0.001;
pub struct Ring<'a> {
    pub lattice: CellLattice,
    pub nodes: &'a [RawNode],
    pub path: &'a Path,
}
pub struct Family<'a> {
    pub inputs: &'a Prepared,
    pub layout: &'a Layout,
    pub shaped: &'a [Path],
}
#[derive(Debug, Serialize)]
pub struct MonumentPassage {
    pub monument: u64,
    pub prefab: String,
    pub hierarchy: u8,
    pub first_road: usize,
    pub first_connector: i64,
    pub second_road: usize,
    pub second_connector: i64,
}
#[derive(Debug, Serialize)]
pub struct PrimaryRouting {
    pub lattice: CellLattice,
    pub cells: Vec<[usize; 2]>,
}
#[derive(Debug, Serialize)]
pub struct Receipt {
    pub engine: &'static str,
    pub junction_engine: &'static str,
    pub raw_endpoints_preserved: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw_connections: Vec<junctions::RawConnection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_attachment_cost_margin: Option<f32>,
    pub source_sha256: String,
    pub assets_sha256: String,
    pub roles_sha256: String,
    pub attachments: Vec<junctions::Attachment>,
    pub monument_passages: Vec<MonumentPassage>,
    pub connectivity_model: &'static str,
    pub authored_monument_access_validated: bool,
    pub authored_terminals: usize,
    pub maximum_authored_offset_metres: f64,
    pub maximum_allowed_authored_offset_metres: f64,
    pub maximum_shared_socket_gap_metres: f64,
    pub road_count: usize,
    pub has_ring: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_routing: Option<PrimaryRouting>,
    pub prepared_families: [bool; 2],
    pub work: usize,
    pub every_road_connected: bool,
    pub terrain_and_mesh_validated: bool,
}
#[derive(Debug)]
pub struct Network {
    roads: Vec<Road>,
    paths: Vec<Path>,
    hierarchy: Vec<u8>,
    receipt: Receipt,
}
impl Network {
    /// Build family metadata from the exact selected infrastructure snapshot.
    /// Ring and ordinary paved roads have separate width/hierarchy policies.
    pub fn into_retained_plan(
        self,
        source: &crate::infrastructure::Source,
        road_seed: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<crate::road_planning::Plan>, String> {
        if cancelled() {
            return Ok(None);
        }
        if source.sha256 != self.receipt.source_sha256 {
            return Err(
                "Ordered road network differs from the selected infrastructure snapshot".into(),
            );
        }
        let mut metadata = Vec::new();
        let mut first = 0;
        for h in 0..3u8 {
            let enabled = if h == 0 {
                self.receipt.has_ring
            } else {
                self.receipt.prepared_families[(h - 1) as usize]
            };
            if !enabled {
                continue;
            }
            let (kind, hierarchy, width, channel, seed) = if h == 0 {
                let r = source
                    .profile
                    .ring
                    .as_ref()
                    .ok_or("Missing source ring metadata policy")?;
                (
                    source.profile.road_type,
                    r.hierarchy,
                    r.width,
                    source.profile.splat_channel,
                    road_seed,
                )
            } else if h == 1 {
                (
                    source.profile.road_type,
                    source
                        .profile
                        .road_hierarchy
                        .ok_or("Missing source paved hierarchy")?,
                    source.profile.width,
                    source.profile.splat_channel,
                    road_seed,
                )
            } else {
                let t = source
                    .profile
                    .trails
                    .as_ref()
                    .ok_or("Missing source dirt metadata policy")?;
                (
                    t.infrastructure_type,
                    t.hierarchy,
                    t.width,
                    t.splat_channel,
                    crate::infrastructure::trail_seed(road_seed, &t.seed_domain),
                )
            };
            if hierarchy != u32::from(h) {
                return Err("Source road family hierarchy differs from the retained layout".into());
            }
            let count = self.hierarchy.iter().filter(|&&v| v == h).count();
            let sockets = self.roads[first..first + count]
                .iter()
                .flat_map(|r| [r.from, r.to])
                .filter_map(|d| match d {
                    Destination::Monument {
                        monument,
                        connector,
                    } => Some((monument, connector)),
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            metadata.push(papyrus_terrain::road_network::Family {
                infrastructure_type: kind,
                hierarchy,
                seed,
                first_road: first,
                road_count: count,
                socket_count: sockets,
                unresolved: vec![],
                width,
                splat_channel: channel,
            });
            first += count;
        }
        crate::road_planning::Plan::from_source_network(self, metadata, cancelled)
    }
    pub fn roads(&self) -> &[Road] {
        &self.roads
    }
    pub fn paths(&self) -> &[Path] {
        &self.paths
    }
    pub fn hierarchy(&self) -> &[u8] {
        &self.hierarchy
    }
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }
    pub(crate) fn into_parts(self) -> (Vec<Road>, Vec<Path>, Vec<u8>, Receipt) {
        (self.roads, self.paths, self.hierarchy, self.receipt)
    }
}
fn fingerprint(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Validate a serialized raw descriptor for readers of a receipt-verified road
/// report. This never constructs the trusted core token from untrusted JSON.
pub(crate) fn raw_json_endpoint(
    report: &serde_json::Value,
    road: usize,
    side: usize,
    endpoint: &serde_json::Value,
) -> Result<bool, String> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Descriptor {
        road: usize,
        endpoint: usize,
        parent_road: usize,
        raw_cell: [usize; 2],
        anchor_xz_bits: [u32; 2],
        reference_parent_point: usize,
    }
    let Some(raw) = endpoint.get("source_network") else {
        return Ok(false);
    };
    if endpoint.as_object().is_none_or(|o| o.len() != 1) {
        return Err("Mixed raw road destination".into());
    }
    let d: Descriptor = serde_json::from_value(raw.clone())
        .map_err(|e| format!("Invalid raw road descriptor: {e}"))?;
    let rows = report["roads"]
        .as_array()
        .filter(|v| v.len() <= 4096)
        .ok_or("Missing bounded raw road roster")?;
    let receipt = &report["source_layout"];
    let records = receipt["raw_connections"]
        .as_array()
        .filter(|v| v.len() <= 8192)
        .ok_or("Missing raw road records")?;
    if side > 1
        || d.road != road
        || d.endpoint != side
        || road >= rows.len()
        || d.parent_road >= road
        || d.raw_cell.iter().any(|v| *v >= 1024)
        || receipt["engine"] != RAW_VERSION
        || receipt["raw_endpoints_preserved"] != true
        || records.iter().filter(|r| *r == raw).count() != 1
        || rows[d.parent_road]["points"]
            .as_array()
            .is_none_or(|p| d.reference_parent_point >= p.len())
    {
        return Err("Raw road descriptor or handoff identity differs".into());
    }
    let points = rows[road]["points"]
        .as_array()
        .filter(|v| v.len() >= 2)
        .ok_or("Missing raw road controls")?;
    let p: [f32; 3] =
        serde_json::from_value(points[if side == 0 { 0 } else { points.len() - 1 }].clone())
            .map_err(|e| e.to_string())?;
    if p.iter().any(|v| !v.is_finite()) || [p[0].to_bits(), p[2].to_bits()] != d.anchor_xz_bits {
        return Err("Raw road JSON anchor changed".into());
    }
    Ok(true)
}
/// Preserve actual source connector IDs and immutable shaped parent indices.
/// Families must be in source order (paved, then optional dirt). A layout with
/// unresolved connectors cannot be relabeled as a complete retained network.
pub fn prepare(
    ring: Option<Ring<'_>>,
    families: &[Family<'_>],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, String> {
    prepare_impl(
        ring,
        families,
        None,
        None,
        Mode::Indexed,
        settings,
        cancelled,
    )
}

pub fn prepare_raw(
    ring: Option<Ring<'_>>,
    families: &[Family<'_>],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, String> {
    prepare_impl(ring, families, None, None, Mode::Raw, settings, cancelled)
}

pub fn prepare_strip_checked(
    ring: Option<Ring<'_>>,
    families: &[Family<'_>],
    widths: [f32; 3],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, String> {
    prepare_impl(
        ring,
        families,
        Some(widths),
        None,
        Mode::Indexed,
        settings,
        cancelled,
    )
}

pub fn prepare_junction_checked(
    ring: Option<Ring<'_>>,
    families: &[Family<'_>],
    widths: [f32; 3],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, String> {
    prepare_impl(
        ring,
        families,
        Some(widths),
        None,
        Mode::JunctionChecked,
        settings,
        cancelled,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Indexed,
    Raw,
    JunctionChecked,
}

pub fn prepare_source_checked(
    ring: Option<Ring<'_>>,
    families: &[Family<'_>],
    widths: [f32; 3],
    routing: junctions::Routing<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, String> {
    prepare_impl(
        ring,
        families,
        Some(widths),
        Some(routing),
        Mode::Indexed,
        settings,
        cancelled,
    )
}

fn prepare_impl(
    ring: Option<Ring<'_>>,
    families: &[Family<'_>],
    widths: Option<[f32; 3]>,
    routing: Option<junctions::Routing<'_>>,
    mode: Mode,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, String> {
    let raw = mode == Mode::Raw;
    if cancelled() {
        return Ok(None);
    }
    if families.is_empty() || families.len() > 2 {
        return Err("Source handoff requires bounded ordered connector families".into());
    }
    let source = families[0].inputs;
    if ![
        &source.source_sha256,
        &source.assets_sha256,
        &source.roles_sha256,
    ]
    .into_iter()
    .all(|s| fingerprint(s))
    {
        return Err("Source road handoff fingerprints are invalid".into());
    }
    let mut geometry = Vec::new();
    let mut terminals = Vec::new();
    let mut authored = BTreeMap::new();
    let mut work = 0usize;
    let mut maximum_authored_offset_metres = 0_f64;
    macro_rules! spend {
        ($n:expr) => {{
            if cancelled() {
                return Ok(None);
            }
            work = work
                .checked_add($n)
                .filter(|&w| w <= settings.maximum_work)
                .ok_or("Source road handoff work budget reached")?;
        }};
    }
    spend!(0);
    let has_ring = ring.is_some();
    let mut primary_routing = None;
    if let Some(ring) = ring {
        if ring.lattice != source.lattice || !ring.path.is_circular() {
            return Err("Source handoff ring lattice or closure differs".into());
        }
        if ring.nodes.len() > 1_000_000 {
            return Err("Source primary-road routing exceeds its node bound".into());
        }
        let mut cells = Vec::with_capacity(ring.nodes.len());
        for chunk in ring.nodes.chunks(1024) {
            spend!(chunk.len());
            cells.extend(chunk.iter().map(|node| node.cell));
        }
        primary_routing = Some(PrimaryRouting { lattice: ring.lattice, cells });
        geometry.push(junctions::Road {
            lattice: ring.lattice,
            nodes: ring.nodes,
            path: ring.path,
            terminals: [false; 2],
            hierarchy: 0,
        });
        terminals.push([None, None]);
    }
    let mut previous = None;
    for family in families {
        spend!(1);
        let input = family.inputs;
        let hierarchy = if input.trail { 2 } else { 1 };
        if previous.is_some_and(|p| p >= hierarchy)
            || input.engine != source_road_inputs::VERSION
            || input.lattice != source.lattice
            || input.source_sha256 != source.source_sha256
            || input.assets_sha256 != source.assets_sha256
            || input.roles_sha256 != source.roles_sha256
            || input.connectors.len() != input.endpoints.len()
            || input.endpoints.len() > 32768
            || family.layout.lattice != input.lattice
            || family.layout.segments.len() != family.shaped.len()
            || !family.layout.unrecovered_connectors.is_empty()
            || !family.layout.unattached_connectors.is_empty()
        {
            return Err(
                "Source road handoff family order, receipts or complete layout coverage differs"
                    .into(),
            );
        }
        previous = Some(hierarchy);
        spend!(input.endpoints.len());
        let mut used = vec![false; input.endpoints.len()];
        for (segment, path) in family.layout.segments.iter().zip(family.shaped) {
            spend!(1);
            if geometry.len() >= 4096 || segment.lattice != input.lattice || path.is_circular() {
                return Err("Invalid source handoff open segment or path allowance".into());
            }
            let mut ends = [None, None];
            for (side, index) in [segment.origin, segment.target].into_iter().enumerate() {
                let Some(index) = index else {
                    continue;
                };
                spend!(authored.len().max(1).ilog2() as usize * 3 + 4);
                let endpoint = input
                    .endpoints
                    .get(index)
                    .ok_or("Source road connector index is missing")?;
                used[index] = true;
                let cell = segment
                    .nodes
                    .get(if side == 0 {
                        0
                    } else {
                        segment.nodes.len().saturating_sub(1)
                    })
                    .ok_or("Source road terminal has no raw node")?
                    .cell;
                if cell != input.connectors[index].cell
                    || cell != input.lattice.cell([endpoint.world[0], endpoint.world[2]])?
                {
                    return Err(
                        "Source road terminal differs from its authored connector cell".into(),
                    );
                }
                if endpoint.connector == 0
                    || endpoint
                        .world
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                {
                    return Err("Invalid source road authored endpoint".into());
                }
                let p = path.points()[if side == 0 {
                    0
                } else {
                    path.points().len() - 1
                }];
                // Segment::shape samples terrain Y, and round-trips authored X/Z
                // through source UVs. Never invent a vertical socket certificate.
                let canonical = [endpoint.world[0], endpoint.world[2]].map(|v| {
                    -input.lattice.size() * 0.5
                        + ((v + input.lattice.size() * 0.5) * (1. / input.lattice.size()))
                            * input.lattice.size()
                });
                let offset = ((f64::from(p[0]) - f64::from(canonical[0])).powi(2)
                    + (f64::from(p[2]) - f64::from(canonical[1])).powi(2))
                .sqrt();
                if offset > MAXIMUM_AUTHORED_ROUNDING_METRES {
                    return Err(format!(
                        "Source road authored endpoint moved before handoff: monument {}, connector {}, shaped [{}, {}], expected {canonical:?}",
                        endpoint.monument, endpoint.connector, p[0], p[2]
                    ));
                }
                maximum_authored_offset_metres = maximum_authored_offset_metres.max(offset);
                let key = (endpoint.monument, endpoint.connector);
                let pose = endpoint.world.map(f32::to_bits);
                if authored.get(&key).is_some_and(|p| *p != pose) {
                    return Err("Source connector identity has conflicting authored poses".into());
                }
                authored.insert(key, pose);
                ends[side] = Some(Destination::Monument {
                    monument: endpoint.monument,
                    connector: endpoint.connector,
                });
            }
            geometry.push(junctions::Road {
                lattice: segment.lattice,
                nodes: &segment.nodes,
                path,
                terminals: ends.map(|e| e.is_some()),
                hierarchy,
            });
            terminals.push(ends);
        }
        if used.iter().any(|&v| !v) {
            return Err(
                "Source road handoff omits an authored connector despite complete layout flags"
                    .into(),
            );
        }
    }
    let junction_settings = Settings {
        maximum_work: settings.maximum_work.saturating_sub(work),
        ..settings
    };
    let adapted = if raw {
        junctions::attach_raw(&geometry, junction_settings, &cancelled)
    } else if let Some(widths) = widths {
        spend!(geometry.len());
        let widths = geometry
            .iter()
            .map(|r| widths[r.hierarchy as usize])
            .collect::<Vec<_>>();
        let settings = Settings {
            maximum_work: settings.maximum_work.saturating_sub(work),
            ..settings
        };
        if mode == Mode::JunctionChecked {
            junctions::attach_junction_checked(&geometry, &widths, settings, &cancelled)
        } else if let Some(routing) = routing {
            junctions::attach_source_checked(&geometry, &widths, routing, settings, &cancelled)
        } else {
            junctions::attach_strip_checked(&geometry, &widths, settings, &cancelled)
        }
    } else {
        junctions::attach(&geometry, junction_settings, &cancelled)
    };
    let Some(adapted) = adapted.map_err(|e| format!("Source road handoff junctions: {e:?}"))?
    else {
        return Ok(None);
    };
    spend!(adapted.work);
    spend!(geometry.len() * 4);
    let mut destinations = BTreeMap::new();
    let mut links = vec![Vec::new(); geometry.len()];
    for a in &adapted.attachments {
        spend!(destinations.len().max(1).ilog2() as usize + 2);
        let destination = if raw {
            let proof = adapted
                .raw_connections
                .iter()
                .find(|p| p.road() == a.road && p.endpoint() == a.endpoint)
                .ok_or("Missing raw source connection token")?;
            Destination::RawNetwork(*proof)
        } else {
            Destination::Network {
                road: a.parent_road,
                point: a.parent_point,
            }
        };
        if destinations
            .insert((a.road, a.endpoint), destination)
            .is_some()
        {
            return Err("Duplicate source road parent reference".into());
        }
        links[a.road].push(a.parent_road);
        links[a.parent_road].push(a.road);
    }
    let mut shared = BTreeMap::new();
    let mut monument_prefabs = BTreeMap::new();
    for family in families {
        for endpoint in &family.inputs.endpoints {
            spend!(1);
            if monument_prefabs
                .insert(endpoint.monument, endpoint.prefab.as_str())
                .is_some_and(|old| old != endpoint.prefab)
            {
                return Err("Source monument identity has conflicting prefabs".into());
            }
        }
    }
    let mut monument_groups = BTreeMap::new();
    let mut monument_passages = Vec::new();
    let mut roads = Vec::with_capacity(geometry.len());
    let mut authored_terminals = 0;
    let mut maximum_shared_socket_gap_metres = 0_f64;
    for (id, path) in adapted.paths.iter().enumerate() {
        spend!(path.points().len());
        let mut ends = [Destination::Loop; 2];
        for side in 0..2 {
            spend!(
                shared.len().max(1).ilog2() as usize
                    + destinations.len().max(1).ilog2() as usize
                    + 3
            );
            if path.is_circular() {
                continue;
            }
            ends[side] = if let Some(terminal) = terminals[id][side] {
                authored_terminals += 1;
                if let Destination::Monument {
                    monument,
                    connector,
                } = terminal
                {
                    let point = path.points()[if side == 0 {
                        0
                    } else {
                        path.points().len() - 1
                    }];
                    if let Some((parent, before)) =
                        shared.insert((monument, connector), (id, point.map(f32::to_bits)))
                    {
                        let gap = before
                            .into_iter()
                            .zip(point)
                            .map(|(a, b)| (f64::from(f32::from_bits(a)) - f64::from(b)).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        if gap > MAXIMUM_AUTHORED_ROUNDING_METRES {
                            return Err(
                                "Shared source socket has different shaped terrain heights".into(),
                            );
                        }
                        maximum_shared_socket_gap_metres =
                            maximum_shared_socket_gap_metres.max(gap);
                        links[id].push(parent);
                        links[parent].push(id);
                    }
                    // GenerateRoadLayout promotes/defer groups by MonumentInfo,
                    // then permits other exits of that monument as starts. This
                    // is a logical authored-prefab passage, not a snapped road
                    // junction or a certificate for its internal collision mesh.
                    spend!(1);
                    let (parent, first_connector) = *monument_groups
                        .entry((monument, geometry[id].hierarchy))
                        .or_insert((id, connector));
                    if parent != id && first_connector != connector {
                        links[id].push(parent);
                        links[parent].push(id);
                        monument_passages.push(MonumentPassage {
                            monument,
                            prefab: monument_prefabs[&monument].into(),
                            hierarchy: geometry[id].hierarchy,
                            first_road: parent,
                            first_connector,
                            second_road: id,
                            second_connector: connector,
                        });
                    }
                }
                terminal
            } else {
                *destinations
                    .get(&(id, side))
                    .ok_or("Missing source road network destination")?
            };
        }
        roads.push(Road {
            from: ends[0],
            to: ends[1],
            points: path.points().to_vec(),
        });
    }
    let mut reached = vec![false; roads.len()];
    let mut pending = if roads.is_empty() { vec![] } else { vec![0] };
    while let Some(id) = pending.pop() {
        spend!(1);
        if !reached[id] {
            reached[id] = true;
            pending.extend(links[id].iter().copied());
        }
    }
    if reached.iter().any(|&v| !v) {
        return Err("Source road handoff contains disconnected components".into());
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Network {
        roads,
        paths: adapted.paths,
        hierarchy: geometry.iter().map(|r| r.hierarchy).collect(),
        receipt: Receipt {
            engine: if raw {
                RAW_VERSION
            } else if mode == Mode::JunctionChecked {
                JUNCTION_CHECKED_VERSION
            } else if routing.is_some() {
                SOURCE_CHECKED_VERSION
            } else if widths.is_some() {
                STRIP_CHECKED_VERSION
            } else {
                VERSION
            },
            junction_engine: adapted.engine,
            raw_endpoints_preserved: raw,
            raw_connections: adapted.raw_connections,
            source_attachment_cost_margin: adapted.source_cost_margin,
            source_sha256: source.source_sha256.clone(),
            assets_sha256: source.assets_sha256.clone(),
            roles_sha256: source.roles_sha256.clone(),
            attachments: adapted.attachments,
            monument_passages,
            connectivity_model: if raw {
                "source-raw-cell-graph-and-monument-groups; physical support unvalidated"
            } else {
                "source-monument-groups-and-exact-network-junctions"
            },
            authored_monument_access_validated: false,
            authored_terminals,
            maximum_authored_offset_metres,
            maximum_allowed_authored_offset_metres: MAXIMUM_AUTHORED_ROUNDING_METRES,
            maximum_shared_socket_gap_metres,
            road_count: geometry.len(),
            has_ring,
            primary_routing,
            prepared_families: [
                families.iter().any(|f| !f.inputs.trail),
                families.iter().any(|f| f.inputs.trail),
            ],
            work,
            every_road_connected: true,
            terrain_and_mesh_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_road_inputs::Endpoint;
    use papyrus_terrain::{
        source_road_layout::{self, Connector, Segment},
        source_road_route,
    };
    fn path(p: &[[f32; 3]]) -> Path {
        Path::from_controls(p, 100000, || false).unwrap().unwrap()
    }
    fn raw(lattice: CellLattice, points: &[[f32; 2]]) -> Vec<RawNode> {
        points
            .iter()
            .enumerate()
            .map(|(i, &p)| RawNode {
                cell: lattice.cell(p).unwrap(),
                cost: i as u64,
            })
            .collect()
    }
    fn fixture(trail: bool) -> (Prepared, Layout, Vec<Path>) {
        let lattice = CellLattice::new(1000., 10.).unwrap();
        let world = if trail {
            [80., 5., -50.]
        } else {
            [20., 5., -80.]
        };
        let canonical = |v: f32| -500. + ((v + 500.) * (1. / 1000.)) * 1000.;
        let end = [canonical(world[0]), 5., canonical(world[2])];
        let points = if trail {
            vec![[25., 5., -50.], [50., 5., -50.], end]
        } else {
            vec![[20., 5., -11.], [20., 5., -50.], end]
        };
        let cells = if trail {
            vec![[20., -50.], [50., -50.], [world[0], world[2]]]
        } else {
            vec![[20., -20.], [20., -50.], [world[0], world[2]]]
        };
        let input = Prepared {
            engine: source_road_inputs::VERSION,
            assets_sha256: "a".repeat(64),
            roles_sha256: "b".repeat(64),
            source_sha256: "c".repeat(64),
            lattice,
            trail,
            connectors: vec![Connector {
                monument: 1,
                cell: lattice.cell([world[0], world[2]]).unwrap(),
                centre: lattice.cell([world[0], world[2]]).unwrap(),
                radius: 0,
                roadside: false,
            }],
            endpoints: vec![Endpoint {
                monument: if trail { 43 } else { 42 },
                connector: if trail { 903 } else { 902 },
                prefab: "assets/test.prefab".into(),
                world,
                hierarchy_active_without_root: true,
            }],
            skipped_roadside: vec![],
            skipped_nonmonuments: vec![],
        };
        let layout = Layout {
            lattice,
            segments: vec![Segment {
                lattice,
                nodes: raw(lattice, &cells),
                origin: None,
                target: Some(0),
            }],
            network_targets: vec![],
            recovered_entrances: vec![],
            unrecovered_connectors: vec![],
            unattached_connectors: vec![],
            audit: source_road_layout::Audit {
                engine: source_road_layout::CONNECTED_VERSION,
                policy: source_road_layout::Policy {
                    network_sample_stride: 8,
                    push_multiplier: 50000,
                    maximum_queries: 100,
                    maximum_node_storage: 1000,
                },
                search: source_road_route::Settings {
                    maximum_expansions: 1000,
                    maximum_work: 100000,
                    directional_penalty: 0,
                    directional_radius: 0,
                },
                maximum_work: 100000,
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
                bootstrap_recovery_targets: 0,
                layout_queries: 0,
                depth_rejections: 0,
                failed_connections: 0,
                skipped_roadside: 0,
                promoted_groups: vec![],
                accepted_connectors: vec![0],
                discarded_singletons: 0,
            },
        };
        (input, layout, vec![path(&points)])
    }
    fn settings() -> Settings {
        Settings {
            maximum_snap_distance: 20.,
            maximum_work: 100000,
        }
    }
    #[test]
    fn raw_source_handoff_preserves_paved_and_dirt_endpoints_and_records_tokens() {
        let (paved, layout, mut shaped) = fixture(false);
        let (dirt, trails, mut trail_shapes) = fixture(true);
        let lattice = paved.lattice;
        for (layout, shapes) in [(&layout, &mut shaped), (&trails, &mut trail_shapes)] {
            let mut points = shapes[0].points().to_vec();
            let p = lattice.point(layout.segments[0].nodes[0].cell).unwrap();
            points[0] = [p[0], 5., p[1]];
            shapes[0] = path(&points);
        }
        let ring = path(&[
            [-20., 5., -20.],
            [20., 5., -20.],
            [20., 5., 20.],
            [-20., 5., 20.],
            [-20., 5., -20.],
        ]);
        let nodes = raw(
            lattice,
            &[
                [-20., -20.],
                [20., -20.],
                [20., 20.],
                [-20., 20.],
                [-20., -20.],
            ],
        );
        let network = prepare_raw(
            Some(Ring {
                lattice,
                nodes: &nodes,
                path: &ring,
            }),
            &[
                Family {
                    inputs: &paved,
                    layout: &layout,
                    shaped: &shaped,
                },
                Family {
                    inputs: &dirt,
                    layout: &trails,
                    shaped: &trail_shapes,
                },
            ],
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(network.paths()[1].points(), shaped[0].points());
        assert_eq!(network.paths()[2].points(), trail_shapes[0].points());
        assert_eq!(network.receipt().engine, RAW_VERSION);
        let retained = network.receipt().primary_routing.as_ref().unwrap();
        assert_eq!(retained.lattice, lattice);
        assert_eq!(retained.cells, nodes.iter().map(|node| node.cell).collect::<Vec<_>>());
        let saved = serde_json::to_value(network.receipt()).unwrap();
        assert_eq!(saved["primary_routing"]["cells"], serde_json::json!(retained.cells));
        assert_eq!(saved["primary_routing"]["lattice"], serde_json::to_value(lattice).unwrap());
        assert!(network.receipt().raw_endpoints_preserved);
        assert_eq!(network.receipt().raw_connections.len(), 2);
        for i in 1..=2 {
            let Destination::RawNetwork(proof) = network.roads()[i].from else {
                panic!("Expected raw source destination")
            };
            assert!(proof.matches(i, 0, 3, network.paths()[i].points()[0]));
            assert_eq!(proof.parent_road(), i - 1);
        }
        assert!(!network.receipt().terrain_and_mesh_validated);
        assert!(network.receipt().every_road_connected);
        let mut report = papyrus_terrain::road_network::Report {
            roads: network
                .roads()
                .iter()
                .map(|r| papyrus_terrain::road_network::Road {
                    from: r.from,
                    to: r.to,
                    points: r.points.clone(),
                })
                .collect(),
            source_layout: Some(serde_json::to_value(network.receipt()).unwrap()),
            ..Default::default()
        };
        crate::road_generation::check_connection_contract(&report, true).unwrap();
        assert!(crate::road_generation::check_connection_contract(&report, false).is_err());
        report.source_layout.as_mut().unwrap()["raw_connections"][0]["raw_cell"][0] = 999.into();
        assert!(crate::road_generation::check_connection_contract(&report, true).is_err());
        report.source_layout = Some(serde_json::to_value(network.receipt()).unwrap());
        report.roads[1].points[0][0] += 0.01;
        assert!(crate::road_generation::check_connection_contract(&report, true).is_err());
        let families = vec![
            papyrus_terrain::road_network::Family {
                hierarchy: 0,
                width: 12.,
                first_road: 0,
                road_count: 1,
                infrastructure_type: 0,
                seed: 7,
                socket_count: 0,
                unresolved: vec![],
                splat_channel: 7,
            },
            papyrus_terrain::road_network::Family {
                hierarchy: 1,
                width: 10.,
                first_road: 1,
                road_count: 1,
                infrastructure_type: 0,
                seed: 7,
                socket_count: 1,
                unresolved: vec![],
                splat_channel: 7,
            },
            papyrus_terrain::road_network::Family {
                hierarchy: 2,
                width: 4.,
                first_road: 2,
                road_count: 1,
                infrastructure_type: 0,
                seed: 7,
                socket_count: 1,
                unresolved: vec![],
                splat_channel: 0,
            },
        ];
        let plan = crate::road_planning::Plan::from_source_network(network, families, || false)
            .unwrap()
            .unwrap();
        assert!(plan.selection_binding(&[], || false).unwrap().is_some());
        let json = plan.planning_report().json();
        for i in 1..=2 {
            assert!(raw_json_endpoint(&json, i, 0, &json["roads"][i]["from"]).unwrap());
        }
        let decoded = crate::road_object_placement::roads_from_report(&json, 1, 10.).unwrap();
        assert!(!decoded[1].start && decoded[1].end);
        assert!(!decoded[2].start && decoded[2].end);
        let mut stale = json.clone();
        stale["roads"][1]["from"]["source_network"]["raw_cell"][0] = 999.into();
        assert!(raw_json_endpoint(&stale, 1, 0, &stale["roads"][1]["from"]).is_err());
        let mut moved = json;
        moved["roads"][1]["points"][0][0] = 999.0.into();
        assert!(raw_json_endpoint(&moved, 1, 0, &moved["roads"][1]["from"]).is_err());
        let straightened = plan
            .straighten_selected(&[], 100000, || false)
            .unwrap()
            .unwrap();
        for (old, new) in plan.roads().iter().zip(straightened.roads()) {
            assert_eq!(old.path.points(), new.path.points());
        }
    }
    #[test]
    fn deferred_exits_share_a_source_monument_group_without_fabricating_a_junction() {
        let (mut input, mut layout, mut shaped) = fixture(false);
        let lattice = input.lattice;
        for (monument, connector, world, ordinal) in [
            (42, 904, [-100., 5., -100.], 1),
            (77, 905, [-150., 5., -150.], 2),
        ] {
            input.connectors.push(Connector {
                monument: ordinal,
                cell: lattice.cell([world[0], world[2]]).unwrap(),
                centre: lattice.cell([world[0], world[2]]).unwrap(),
                radius: 0,
                roadside: false,
            });
            input.endpoints.push(Endpoint {
                monument,
                connector,
                prefab: "assets/test.prefab".into(),
                world,
                hierarchy_active_without_root: true,
            });
        }
        let coords = [[-100., -100.], [-125., -125.], [-150., -150.]];
        layout.segments.push(Segment {
            lattice,
            nodes: raw(lattice, &coords),
            origin: Some(1),
            target: Some(2),
        });
        let canonical = |v: f32| -500. + ((v + 500.) * (1. / 1000.)) * 1000.;
        shaped.push(path(
            &coords.map(|p| [canonical(p[0]), 5., canonical(p[1])]),
        ));
        let ring = path(&[
            [-20., 5., -20.],
            [20., 5., -20.],
            [20., 5., 20.],
            [-20., 5., 20.],
            [-20., 5., -20.],
        ]);
        let nodes = raw(
            lattice,
            &[
                [-20., -20.],
                [20., -20.],
                [20., 20.],
                [-20., 20.],
                [-20., -20.],
            ],
        );
        let run = |input: &Prepared| {
            prepare(
                Some(Ring {
                    lattice,
                    nodes: &nodes,
                    path: &ring,
                }),
                &[Family {
                    inputs: input,
                    layout: &layout,
                    shaped: &shaped,
                }],
                settings(),
                || false,
            )
        };
        let result = run(&input).unwrap().unwrap();
        assert_eq!(result.paths()[2].points(), shaped[1].points());
        assert_eq!(result.receipt().monument_passages.len(), 1);
        let passage = &result.receipt().monument_passages[0];
        assert_eq!(
            (
                passage.monument,
                passage.first_connector,
                passage.second_connector
            ),
            (42, 902, 904)
        );
        assert!(result.receipt().every_road_connected);
        assert!(!result.receipt().authored_monument_access_validated);
        assert!(!result.receipt().terrain_and_mesh_validated);
        // A nearby different instance is not a source group or an inferred join.
        input.endpoints[1].monument = 99;
        assert!(run(&input).unwrap_err().contains("disconnected"));
        input.endpoints[1].monument = 42;
        input.endpoints[1].prefab = "assets/different.prefab".into();
        assert!(run(&input).unwrap_err().contains("prefabs"));
        input.endpoints[1].prefab = "assets/test.prefab".into();
        input.endpoints[1].connector = 902;
        assert!(run(&input).is_err());
    }
    #[test]
    fn paved_and_dirt_handoff_retains_real_ids_parent_indices_and_source_curves() {
        let (paved, layout, shaped) = fixture(false);
        let (dirt, trails, trail_shapes) = fixture(true);
        let lattice = paved.lattice;
        let ring = path(&[
            [-20., 5., -20.],
            [20., 5., -20.],
            [20., 5., 20.],
            [-20., 5., 20.],
            [-20., 5., -20.],
        ]);
        let nodes = raw(
            lattice,
            &[
                [-20., -20.],
                [20., -20.],
                [20., 20.],
                [-20., 20.],
                [-20., -20.],
            ],
        );
        let build = || {
            prepare(
                Some(Ring {
                    lattice,
                    nodes: &nodes,
                    path: &ring,
                }),
                &[
                    Family {
                        inputs: &paved,
                        layout: &layout,
                        shaped: &shaped,
                    },
                    Family {
                        inputs: &dirt,
                        layout: &trails,
                        shaped: &trail_shapes,
                    },
                ],
                settings(),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let (_, mut doc) = crate::infrastructure::tests::fixture();
        let ring_profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-ring.json"
        ))
        .unwrap();
        let trail_profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-trails.json"
        ))
        .unwrap();
        doc["profile"]["ring"] = ring_profile["ring"].clone();
        doc["profile"]["trails"] = trail_profile["trails"].clone();
        doc["profile"]["road_hierarchy"] = 1.into();
        let mut source: crate::infrastructure::Source = serde_json::from_value(doc).unwrap();
        source.sha256 = "c".repeat(64);
        let source_plan = build()
            .into_retained_plan(&source, 1136074333, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            source_plan
                .planning_report()
                .families
                .iter()
                .map(|f| (f.width, f.splat_channel))
                .collect::<Vec<_>>(),
            vec![(12., 7), (10., 7), (4., 0)]
        );
        assert_eq!(source_plan.planning_report().families[2].seed, 1274088034);
        source.sha256 = "d".repeat(64);
        assert!(build().into_retained_plan(&source, 1, || false).is_err());
        let network = build();
        assert_eq!(
            network.roads()[1].to,
            Destination::Monument {
                monument: 42,
                connector: 902
            }
        );
        assert_eq!(
            network.roads()[2].to,
            Destination::Monument {
                monument: 43,
                connector: 903
            }
        );
        assert!(matches!(
            network.roads()[1].from,
            Destination::Network { road: 0, .. }
        ));
        assert!(matches!(
            network.roads()[2].from,
            Destination::Network { road: 1, .. }
        ));
        let original = serde_json::to_value(network.paths()).unwrap();
        let families = (0..3)
            .map(|h| papyrus_terrain::road_network::Family {
                infrastructure_type: if h == 2 { 2 } else { 0 },
                hierarchy: h,
                seed: 1,
                first_road: h as usize,
                road_count: 1,
                socket_count: usize::from(h != 0),
                unresolved: vec![],
                width: if h == 2 { 4. } else { 10. },
                splat_channel: 7,
            })
            .collect();
        let plan = crate::road_planning::Plan::from_source_network(network, families, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(plan.roads().iter().map(|r| r.path).collect::<Vec<_>>()).unwrap(),
            original
        );
        assert!(plan.planning_report().entrance_joins.is_empty());
        assert!(plan.planning_report().entrance_policy.is_none());
        assert_eq!(plan.planning_report().socket_count, 2);
        assert_eq!(plan.planning_report().trails_enabled, Some(true));
        assert_eq!(
            plan.planning_report().json()["source_layout"]["terrain_and_mesh_validated"],
            false
        );
        assert!(plan.selection_binding(&[], || false).unwrap().is_some());
        // An unbound geometric handoff cannot authorize map/roadside changes.
        assert!(
            plan.quality_feedback_binding("c", || false)
                .unwrap_err()
                .contains("bound")
        );
        assert!(
            crate::road_planning::Plan::from_source_network(build(), vec![], || false).is_err()
        );
    }
    #[test]
    fn wrong_id_pose_cell_order_or_incomplete_layout_rejects_atomically() {
        let (mut input, mut layout, mut shaped) = fixture(false);
        // A no-ring bootstrap can start at two real authored terminals.
        let start = [20., 5., -11.];
        input.connectors.push(Connector {
            cell: input.lattice.cell([start[0], start[2]]).unwrap(),
            ..input.connectors[0]
        });
        input.endpoints.push(Endpoint {
            monument: 40,
            connector: 901,
            prefab: "assets/test.prefab".into(),
            world: start,
            hierarchy_active_without_root: true,
        });
        layout.segments[0].origin = Some(1);
        layout.segments[0].nodes[0].cell = input.connectors[1].cell;
        let p = shaped[0].points();
        let mut points = p.to_vec();
        points[0][0] = -500. + ((start[0] + 500.) * (1. / 1000.)) * 1000.;
        points[0][2] = -500. + ((start[2] + 500.) * (1. / 1000.)) * 1000.;
        shaped[0] = path(&points);
        let run = |i: &Prepared, l: &Layout| {
            prepare(
                None,
                &[Family {
                    inputs: i,
                    layout: l,
                    shaped: &shaped,
                }],
                settings(),
                || false,
            )
        };
        assert!(run(&input, &layout).unwrap().is_some());
        let original_world = input.endpoints[0].world;
        input.endpoints[0].world[0] += 0.0001;
        let rounded = run(&input, &layout).unwrap().unwrap();
        assert!(rounded.receipt().maximum_authored_offset_metres > 0.);
        assert!(
            rounded.receipt().maximum_authored_offset_metres <= MAXIMUM_AUTHORED_ROUNDING_METRES
        );
        assert_eq!(rounded.paths()[0].points(), shaped[0].points());
        input.endpoints[0].world = original_world;
        input.endpoints[0].world[0] += 1.;
        assert!(run(&input, &layout).unwrap_err().contains("endpoint moved"));
        input.endpoints[0].world[0] -= 1.;
        layout.segments[0].target = Some(99);
        assert!(run(&input, &layout).is_err());
        layout.segments[0].target = Some(0);
        layout.unattached_connectors.push(0);
        assert!(run(&input, &layout).is_err());
        layout.unattached_connectors.clear();
        assert!(
            prepare(
                None,
                &[Family {
                    inputs: &input,
                    layout: &layout,
                    shaped: &shaped
                }],
                settings(),
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(
            prepare(
                None,
                &[Family {
                    inputs: &input,
                    layout: &layout,
                    shaped: &shaped
                }],
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn complete_flags_cannot_hide_omitted_sockets_or_changed_family_receipts() {
        let (mut input, layout, shaped) = fixture(false);
        let (mut dirt, trails, trail_shapes) = fixture(true);
        dirt.roles_sha256 = "f".repeat(64);
        assert!(
            prepare(
                None,
                &[
                    Family {
                        inputs: &input,
                        layout: &layout,
                        shaped: &shaped
                    },
                    Family {
                        inputs: &dirt,
                        layout: &trails,
                        shaped: &trail_shapes
                    }
                ],
                settings(),
                || false
            )
            .unwrap_err()
            .contains("receipts")
        );
        input.connectors.push(input.connectors[0]);
        input.endpoints.push(Endpoint {
            monument: 99,
            connector: 999,
            prefab: "assets/test.prefab".into(),
            world: [20., 5., -80.],
            hierarchy_active_without_root: true,
        });
        assert!(
            prepare(
                None,
                &[Family {
                    inputs: &input,
                    layout: &layout,
                    shaped: &shaped
                }],
                settings(),
                || false
            )
            .unwrap_err()
            .contains("omits")
        );
    }
}
