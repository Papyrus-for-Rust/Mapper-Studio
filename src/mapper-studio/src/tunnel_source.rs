//! Installation-bound tunnel metadata prepared once for generation workers.
use crate::{
    installation::{self, SourceFile},
    metadata_runtime, monument_source,
    prefab_assets::Catalog,
};
use papyrus_terrain::{
    dungeon_base::Pose,
    environment::{self, Box3, Transform},
    prefab_terrain, prefab_terrain_effects,
    tunnel_depth::Volume,
    tunnel_grid::Cell,
    tunnel_links::{Module, Socket},
    tunnel_stations::Snap,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

const LIMIT: u64 = 64 * 1024 * 1024;
const NAME: &str = "tunnels.json";
const CACHE: &str = "papyrus-tunnel-cache";
const PREFIX: &str = "assets/bundled/prefabs/autospawn/";
const MISSING: &str = "Prepared underground rail metadata is unavailable. Reload the selected Rust installation.";
mod eligibility;
pub use eligibility::{
    PortalAssessment, PortalChecks, PortalError, PortalRailAccess, PortalRejection,
};
type CacheKey = (PathBuf, String);
type Session = Vec<(CacheKey, Arc<Source>)>;
fn session() -> &'static Mutex<Session> {
    static SESSION: OnceLock<Mutex<Session>> = OnceLock::new();
    SESSION.get_or_init(|| Mutex::new(Vec::new()))
}
pub(crate) fn clear_session() {
    if let Ok(mut entries) = session().lock() {
        entries.clear();
    }
}
fn retain(key: CacheKey, source: Source) -> Arc<Source> {
    let source = Arc::new(source);
    if let Ok(mut entries) = session().lock() {
        if let Some((_, existing)) = entries.iter().find(|(id, _)| id == &key) {
            return existing.clone();
        }
        if entries.len() >= 3 {
            entries.remove(0);
        }
        entries.push((key, source.clone()));
    }
    source
}
fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn path(value: &str) -> bool {
    value.starts_with(PREFIX)
        && value.ends_with(".prefab")
        && value.len() <= 512
        && !value.contains(['\\', '\0'])
        && !value
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > limit {
        return Err("Tunnel metadata exceeds its bound".into());
    }
    Ok(raw)
}
#[derive(Deserialize)]
struct Frame {
    transform_id: i64,
    parent_transform: Option<i64>,
    position: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
    matrix: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
    active_without_root: bool,
}
impl Frame {
    fn hierarchy_position(&self) -> Result<[f32; 3], String> {
        let position: [f32; 3] = std::array::from_fn(|i| self.matrix[i][3]);
        if self.transform_id == 0
            || self.matrix.iter().flatten().any(|v| !v.is_finite())
            || position.iter().any(|v| v.abs() > 1_000_000.)
        {
            return Err("Invalid tunnel attribute position".into());
        }
        Ok(position)
    }
    fn local(&self) -> Result<Transform, String> {
        if self.transform_id == 0
            || self.parent_transform == Some(0)
            || self.rotation_chain.len() > 128
        {
            return Err("Invalid tunnel hierarchy frame".into());
        }
        let result = Transform {
            position: self.position,
            rotation: self.rotation,
            scale: self.scale,
        };
        result.validate()?;
        Ok(result)
    }
    fn hierarchy(&self, scale: bool) -> Result<Transform, String> {
        self.local()?;
        crate::environment_checks::local_transform(self.matrix, &self.rotation_chain, scale)
    }
}
#[derive(Deserialize)]
struct Neighbour {
    guid: String,
}
#[derive(Deserialize)]
struct RawCell {
    #[serde(flatten)]
    frame: Frame,
    connections: BTreeMap<String, u8>,
    variants: BTreeMap<String, u8>,
    avoid_neighbours: Vec<Neighbour>,
    replaceable: bool,
}
#[derive(Deserialize)]
struct RawLink {
    #[serde(flatten)]
    frame: Frame,
    up_socket: Frame,
    down_socket: Frame,
    up_type: u8,
    down_type: u8,
    priority: i32,
    rotation_limit: u16,
}
#[derive(Deserialize)]
struct RawVolume {
    #[serde(flatten)]
    frame: Frame,
    center: [f32; 3],
    size: [f32; 3],
}
#[derive(Deserialize)]
struct RawEnvironment {
    #[serde(flatten)]
    frame: Frame,
    mask: i32,
    center: [f32; 3],
    size: [f32; 3],
}
#[derive(Deserialize)]
struct RawEntrance {
    #[serde(flatten)]
    frame: Frame,
    cell_size: f32,
    link_height: f32,
    link_radius: f32,
}
#[derive(Deserialize)]
struct RawConnector {
    object_id: i64,
    #[serde(flatten)]
    frame: Frame,
    infrastructure_type: i32,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(untagged)]
enum Flag {
    Bool(bool),
    Int(i32),
}
impl Flag {
    fn enabled(self) -> Result<bool, String> {
        match self {
            Self::Bool(value) => Ok(value),
            Self::Int(0) => Ok(false),
            Self::Int(1) => Ok(true),
            Self::Int(_) => Err("Invalid tunnel terrain boolean".into()),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HeightFields {
    rotate: Flag,
    extents: f32,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct FilterFields {
    filter: papyrus_terrain::spawn::Filter,
    check_placement_map: Flag,
    check_terrain_bounds: Flag,
    check_topology_radius: f32,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RaiseFields {
    opacity: f32,
    radius: f32,
    fade: f32,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TopologyFields {
    topology_type: i32,
    radius: f32,
    fade: f32,
}
#[derive(Deserialize)]
#[serde(tag = "kind")]
enum RawTerrainAttribute {
    TerrainHeightRaise {
        #[serde(flatten)]
        frame: Frame,
        fields: RaiseFields,
    },
    TerrainTopologySet {
        #[serde(flatten)]
        frame: Frame,
        fields: TopologyFields,
    },
    TerrainCollisionTrigger,
    TerrainCheck {
        #[serde(flatten)]
        frame: Frame,
        fields: HeightFields,
    },
    TerrainFilter {
        #[serde(flatten)]
        frame: Frame,
        fields: FilterFields,
    },
    #[serde(other)]
    Other,
}
fn terrain_attributes(raw: &[RawTerrainAttribute]) -> Result<prefab_terrain::Prepared, String> {
    if raw.len() > 16384 {
        return Err("Too many tunnel terrain attributes".into());
    }
    let mut rules = prefab_terrain::Rules::default();
    for attribute in raw {
        match attribute {
            RawTerrainAttribute::TerrainCheck { frame, fields } => {
                rules.heights.push(prefab_terrain::HeightCheck {
                    position: frame.hierarchy_position()?,
                    rotate: fields.rotate.enabled()?,
                    extents: fields.extents,
                })
            }
            RawTerrainAttribute::TerrainFilter { frame, fields } => {
                rules
                    .filters
                    .push(papyrus_terrain::monument_eligibility::Filter {
                        position: frame.hierarchy_position()?,
                        filter: fields.filter.clone(),
                        radius: fields.check_topology_radius,
                        check_bounds: fields.check_terrain_bounds.enabled()?,
                        check_placement: fields.check_placement_map.enabled()?,
                    })
            }
            _ => {}
        }
    }
    prefab_terrain::Prepared::new(rules)
}
fn terrain_effects(
    raw: &[RawTerrainAttribute],
) -> Result<prefab_terrain_effects::Prepared, String> {
    let mut effects = Vec::new();
    for attribute in raw {
        match attribute {
            RawTerrainAttribute::TerrainHeightRaise { frame, fields } => {
                effects.push(prefab_terrain_effects::Effect::RaiseHeight {
                    position: frame.hierarchy_position()?,
                    opacity: fields.opacity,
                    radius: fields.radius,
                    fade: fields.fade,
                })
            }
            RawTerrainAttribute::TerrainTopologySet { frame, fields } => {
                effects.push(prefab_terrain_effects::Effect::SetTopology {
                    position: frame.hierarchy_position()?,
                    mask: fields.topology_type as u32,
                    radius: fields.radius,
                    fade: fields.fade,
                })
            }
            RawTerrainAttribute::Other => {
                return Err("Unsupported generated tunnel prefab attribute".into());
            }
            _ => {}
        }
    }
    prefab_terrain_effects::Prepared::new(effects)
}
#[derive(Deserialize)]
struct RawPrefab {
    cells: Vec<RawCell>,
    links: Vec<RawLink>,
    volumes: Vec<RawVolume>,
    environments: Vec<RawEnvironment>,
    entrances: Vec<RawEntrance>,
    connectors: Vec<RawConnector>,
    #[serde(default)]
    monument_infos: Vec<Frame>,
    #[serde(default)]
    terrain_checks: Option<Vec<RawTerrainAttribute>>,
    #[serde(default)]
    environment: Option<crate::monument_attributes::Environment>,
    nodes_visited: usize,
}
#[derive(Clone, Deserialize)]
pub struct Selector {
    pub prefixes: Vec<String>,
    pub prefabs: Vec<String>,
}
#[derive(Deserialize)]
struct Program {
    identity: [String; 3],
    object_id: i64,
    order: usize,
    mode: i32,
    disabled: Option<bool>,
    hierarchy_active: bool,
    component_enabled: Option<bool>,
    #[serde(default)]
    selectors: BTreeMap<String, Selector>,
    fields: serde_json::Value,
}
#[derive(Deserialize)]
struct Identity {
    prefab_id: u32,
}
#[derive(Deserialize)]
struct Identities {
    prefabs: BTreeMap<String, u32>,
    neighbours: BTreeMap<String, Identity>,
}
#[derive(Deserialize)]
struct Document {
    format: String,
    schema: u32,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    code_profile_sha256: String,
    extractor_sha256: String,
    monument_sha256: String,
    programs: Vec<serde_json::Value>,
    prefabs: BTreeMap<String, RawPrefab>,
    identities: Identities,
    game_code_executed: bool,
    full_network_generated: bool,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Fields {
    pub connection_type: i32,
    pub cell_size: f32,
    pub link_height: f32,
    pub link_radius: f32,
    pub link_transition: f32,
}
pub struct Entrance {
    pub frame: Transform,
    pub snap: Snap,
    pub cell_size: f32,
    pub connector: Transform,
}
pub struct Prefab {
    pub id: u32,
    pub cell: Option<Cell>,
    pub replaceable: bool,
    pub link: Option<Module>,
    pub volume: Option<Box3>,
    pub environments: Vec<Volume>,
    pub entrance: Option<Entrance>,
    /// Complete authored hierarchy positions and quaternion-derived forwards,
    /// retaining source component order and inactive children.
    pub connectors: Vec<crate::infrastructure::Connector>,
    /// Surface marker offset used when replacing a buried cell with an upward portal.
    pub surface_offset: Option<[f32; 3]>,
    pub terrain: Option<prefab_terrain::Prepared>,
    pub effects: Option<prefab_terrain_effects::Prepared>,
    /// Captured activation and collider facts prepared through the shared
    /// environment adapter. The owning stage supplies runtime lifecycle.
    pub registration: Option<crate::environment_checks::Prepared>,
}
pub struct Source {
    pub sha256: String,
    pub prefabs: BTreeMap<String, Prefab>,
    pub selectors: BTreeMap<String, Selector>,
    pub fields: Fields,
    pub order: usize,
    pub mode: i32,
    pub active: bool,
    sources: Vec<SourceFile>,
    pub(crate) catalog_sha256: String,
    pub(crate) monument_sha256: String,
}
fn sides(map: &BTreeMap<String, u8>) -> Result<[u8; 4], String> {
    let names = ["north", "south", "west", "east"];
    if map.len() != 4 {
        return Err("Invalid tunnel cell sides".into());
    }
    let mut values = [0; 4];
    for (i, name) in names.iter().enumerate() {
        values[i] = *map.get(*name).ok_or("Missing tunnel cell side")?;
    }
    Ok(values)
}
fn unit() -> Transform {
    Transform {
        position: [0.; 3],
        rotation: [0., 0., 0., 1.],
        scale: [1.; 3],
    }
}
fn socket(frame: &Frame, kind: u8) -> Result<Socket, String> {
    let local = frame.local()?;
    Ok(Socket {
        kind,
        pose: Pose {
            position: local.position,
            rotation: local.rotation,
        },
    })
}
fn box_volume(record: &RawVolume) -> Result<Box3, String> {
    let mut local = record.frame.local()?;
    local.scale = [1.; 3];
    environment::volume_box(local, record.center, record.size, unit())
}
impl Source {
    /// Transform the complete ordered connector roster, including inactive
    /// children as requested by the installed rail layout stage.
    pub fn path_sockets(
        &self,
        path: &str,
        instance: u64,
        pose: Pose,
        kind: i32,
    ) -> Result<Vec<crate::rail_preview::Socket>, String> {
        Transform {
            position: pose.position,
            rotation: pose.rotation,
            scale: [1.; 3],
        }
        .validate()?;
        if instance == 0 || !(0..=64).contains(&kind) {
            return Err("Invalid generated monument connector request".into());
        }
        let prefab = self
            .prefabs
            .get(path)
            .ok_or("Missing generated monument connector source")?;
        Ok(prefab
            .connectors
            .iter()
            .filter(|connector| connector.infrastructure_type == kind)
            .map(|connector| crate::rail_preview::Socket {
                monument: instance,
                connector: connector.object_id,
                prefab: path.into(),
                position: pose.point(connector.position),
                forward: environment::rotate(pose.rotation, connector.forward),
            })
            .collect())
    }
    pub fn entrance_spacing(
        &self,
        path: &str,
    ) -> Option<papyrus_terrain::placement::EntranceSpacing> {
        self.prefabs.get(path)?.entrance.as_ref().map(|entrance| {
            papyrus_terrain::placement::EntranceSpacing {
                offset: entrance.frame.position,
                minimum_distance: entrance.cell_size * 2.5,
            }
        })
    }
    /// Snap the embedded entrance, then translate its owning monument by the
    /// same delta. Terrain anchors have already established the initial height.
    pub fn snap_entrance(
        &self,
        path: &str,
        position: [f32; 3],
        yaw: f32,
        world_size: f32,
    ) -> Result<Option<[f32; 3]>, String> {
        let Some(prefab) = self.prefabs.get(path) else {
            return Ok(Some(position));
        };
        let Some(entrance) = &prefab.entrance else {
            return Ok(Some(position));
        };
        let original = self.entrance_spacing(path).unwrap().world(position, yaw);
        let snapped = entrance.snap.position(original)?;
        let grid = papyrus_terrain::tunnel_grid::Grid::new(world_size, entrance.cell_size)?;
        if !papyrus_terrain::tunnel_stations::valid_spawn(
            grid,
            prefab.volume.ok_or("Missing entrance placement volume")?,
            snapped,
        )? {
            return Ok(None);
        }
        Ok(Some(std::array::from_fn(|i| {
            position[i] + (snapped[i] - original[i])
        })))
    }
    fn decode(
        raw: &[u8],
        catalog: &Catalog,
        monuments: &monument_source::Source,
    ) -> Result<Self, String> {
        Self::decode_for_binding(raw, catalog, &monuments.sha256)
    }
    fn decode_for_binding(
        raw: &[u8],
        catalog: &Catalog,
        monument_sha256: &str,
    ) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Tunnel capture exceeds 64 MiB".into());
        }
        let document: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid tunnel metadata: {e}"))?;
        let profile = crate::monument_stages::capture_code_profile(catalog)?
            .ok_or("Tunnel source profile unavailable")?;
        let policy: serde_json::Value = serde_json::from_str(profile).map_err(|e| e.to_string())?;
        let mut expected = catalog.sources.clone();
        expected
            .push(serde_json::from_value(policy["map_scene"].clone()).map_err(|e| e.to_string())?);
        expected.extend(
            serde_json::from_value::<Vec<SourceFile>>(policy["code_sources"].clone())
                .map_err(|e| e.to_string())?,
        );
        if document.format != "mapper-tunnel-grid-source-inspection"
            || document.schema != 1
            || document.sources != expected
            || document.monument_sha256 != monument_sha256
            || !crate::source_profile::matches(profile, &document.code_profile_sha256)
            || !metadata_runtime::tunnel_extractor_candidates().contains(&document.extractor_sha256)
            || document.game_code_executed
            || document.full_network_generated
            || !(1..=128).contains(&document.programs.len())
            || !(1..=1024).contains(&document.prefabs.len())
            || document.identities.prefabs.len() != document.prefabs.len()
            || document.extra_sources.len() != 1
            || document.extra_sources[0].relative_path != "Bundles/shared/content.bundle"
            || !hash(&document.extra_sources[0].sha256)
            || !(1..=8 * 1024 * 1024 * 1024).contains(&document.extra_sources[0].bytes)
        {
            return Err(
                "Tunnel capture differs from its installation, input rules or adapter".into(),
            );
        }
        let grids: Vec<_> = document
            .programs
            .iter()
            .filter(|p| {
                p["identity"][0] == "Assembly-CSharp"
                    && p["identity"][1] == ""
                    && p["identity"][2] == "GenerateDungeonGrid"
            })
            .collect();
        if grids.len() != 1 {
            return Err("Expected one authored tunnel grid".into());
        }
        let grid: Program = serde_json::from_value(grids[0].clone()).map_err(|e| e.to_string())?;
        if grid.identity != ["Assembly-CSharp", "", "GenerateDungeonGrid"] {
            return Err("Invalid tunnel program identity".into());
        }
        let hierarchy = policy["activation"] == "active-in-hierarchy-client-uncached";
        if grid.object_id <= 0
            || grid.order > 65535
            || !(-1..=3).contains(&grid.mode)
            || grid.disabled.is_none() != hierarchy
            || grid.component_enabled.is_some() != hierarchy
        {
            return Err("Unsupported tunnel stage activation".into());
        }
        let active = if hierarchy {
            grid.hierarchy_active && grid.component_enabled.unwrap_or(false)
        } else {
            !grid.disabled.unwrap_or(true)
        };
        let fields: Fields =
            serde_json::from_value(grid.fields.clone()).map_err(|e| e.to_string())?;
        papyrus_terrain::tunnel_grid::Grid::new(4000., fields.cell_size)?;
        Snap {
            radius: fields.link_radius,
            height: fields.link_height,
        }
        .position([0.; 3])?;
        if !(0..=64).contains(&fields.connection_type)
            || !fields.link_transition.is_finite()
            || !(0.001..=1000.).contains(&fields.link_transition)
        {
            return Err("Invalid tunnel grid constants".into());
        }
        let roles: BTreeSet<_> = [
            "TunnelFolder",
            "StationFolder",
            "UpwardsFolder",
            "TransitionFolder",
            "LinkFolder",
        ]
        .into_iter()
        .collect();
        if grid
            .selectors
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != roles
        {
            return Err("Incomplete tunnel selectors".into());
        }
        for selector in grid.selectors.values() {
            if selector.prefixes.is_empty()
                || selector.prefixes.len() > 32
                || selector.prefabs.len() > 1024
                || !selector.prefabs.windows(2).all(|v| v[0] < v[1])
                || selector.prefixes.iter().any(|p| {
                    !p.starts_with(PREFIX)
                        || !p.ends_with('/')
                        || p.contains(['\\', '\0'])
                        || p.split('/').any(|s| s == "." || s == "..")
                })
                || selector.prefabs.iter().any(|p| {
                    !path(p)
                        || !selector.prefixes.iter().any(|prefix| p.starts_with(prefix))
                        || !document.prefabs.contains_key(p)
                })
            {
                return Err("Invalid tunnel selector roster".into());
            }
        }
        if ["TunnelFolder", "StationFolder", "LinkFolder"]
            .iter()
            .any(|role| grid.selectors[*role].prefabs.is_empty())
        {
            return Err("Required tunnel module pool is empty".into());
        }
        let mut ids = BTreeSet::new();
        let mut prefabs = BTreeMap::new();
        let generated: BTreeSet<_> = grid
            .selectors
            .values()
            .flat_map(|selector| selector.prefabs.iter().map(String::as_str))
            .collect();
        for (path, raw) in document.prefabs {
            let id = *document
                .identities
                .prefabs
                .get(&path)
                .ok_or("Missing tunnel prefab identity")?;
            if !self::path(&path)
                || id == 0
                || !ids.insert(id)
                || !(1..=65536).contains(&raw.nodes_visited)
                || raw.cells.len() > 1
                || raw.links.len() > 1
                || raw.entrances.len() > 1
                || raw.volumes.len() > 256
                || raw.environments.len() > 4096
                || raw.connectors.len() > 256
                || raw.monument_infos.len() > 256
            {
                return Err("Invalid tunnel prefab roster or hierarchy".into());
            }
            let cell = raw
                .cells
                .first()
                .map(|record| -> Result<Cell, String> {
                    record.frame.local()?;
                    if record.avoid_neighbours.len() > 256 {
                        return Err("Excessive tunnel neighbour constraints".into());
                    }
                    let avoid_neighbours = record
                        .avoid_neighbours
                        .iter()
                        .map(|reference| {
                            if reference.guid.len() != 32
                                || !reference
                                    .guid
                                    .bytes()
                                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                            {
                                return Err("Invalid tunnel neighbour GUID".into());
                            }
                            let id = document
                                .identities
                                .neighbours
                                .get(&reference.guid)
                                .ok_or("Unresolved tunnel neighbour GUID")?
                                .prefab_id;
                            if id == 0 {
                                return Err("Invalid neighbour prefab ID".into());
                            }
                            Ok(id)
                        })
                        .collect::<Result<_, String>>()?;
                    Ok(Cell {
                        id,
                        connections: sides(&record.connections)?,
                        variants: sides(&record.variants)?,
                        avoid_neighbours,
                    })
                })
                .transpose()?;
            let link = raw
                .links
                .first()
                .map(|record| -> Result<Module, String> {
                    record.frame.local()?;
                    if record.rotation_limit > 360 || !(-65536..=65536).contains(&record.priority) {
                        return Err("Invalid tunnel link constraints".into());
                    }
                    Ok(Module {
                        id,
                        up: socket(&record.up_socket, record.up_type)?,
                        down: socket(&record.down_socket, record.down_type)?,
                        priority: record.priority,
                        rotation_limit: record.rotation_limit as f32,
                    })
                })
                .transpose()?;
            let volume = raw
                .volumes
                .iter()
                .find(|v| v.frame.active_without_root)
                .map(box_volume)
                .transpose()?;
            let environments = raw
                .environments
                .iter()
                .map(|v| {
                    Ok(Volume {
                        mask: v.mask as u32,
                        geometry: environment::volume_box(
                            v.frame.hierarchy(true)?,
                            v.center,
                            v.size,
                            unit(),
                        )?,
                    })
                })
                .collect::<Result<_, String>>()?;
            let entrance = raw
                .entrances
                .iter()
                .find(|v| v.frame.active_without_root)
                .map(|record| -> Result<Entrance, String> {
                    let connectors: Vec<_> = raw
                        .connectors
                        .iter()
                        .filter(|c| c.infrastructure_type == fields.connection_type)
                        .collect();
                    if connectors.len() != 1 || volume.is_none() || link.is_none() {
                        return Err(
                            "Tunnel entrance needs one source connector, volume and link".into(),
                        );
                    }
                    let snap = Snap {
                        radius: record.link_radius,
                        height: record.link_height,
                    };
                    snap.position([0.; 3])?;
                    papyrus_terrain::tunnel_grid::Grid::new(4000., record.cell_size)?;
                    Ok(Entrance {
                        frame: record.frame.hierarchy(false)?,
                        snap,
                        cell_size: record.cell_size,
                        connector: connectors[0].frame.hierarchy(false)?,
                    })
                })
                .transpose()?;
            let mut connector_ids = BTreeSet::new();
            let connectors = raw
                .connectors
                .iter()
                .map(|connector| {
                    if connector.object_id == 0
                        || connector.object_id == i64::MIN
                        || !connector_ids.insert(connector.object_id)
                        || !(0..=64).contains(&connector.infrastructure_type)
                    {
                        return Err("Invalid or duplicate tunnel path connector".to_string());
                    }
                    let frame = connector.frame.hierarchy(false)?;
                    Ok(crate::infrastructure::Connector {
                        object_id: connector.object_id,
                        infrastructure_type: connector.infrastructure_type,
                        position: frame.position,
                        forward: environment::rotate(frame.rotation, [0., 0., 1.]),
                        hierarchy_active_without_root: connector.frame.active_without_root,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let surface_offset = raw
                .monument_infos
                .iter()
                .find(|frame| frame.active_without_root)
                .map(Frame::hierarchy_position)
                .transpose()?;
            let terrain = raw
                .terrain_checks
                .as_deref()
                .map(terrain_attributes)
                .transpose()?;
            let effects = if generated.contains(path.as_str()) {
                raw.terrain_checks
                    .as_deref()
                    .map(terrain_effects)
                    .transpose()?
            } else {
                None
            };
            let registration = raw
                .environment
                .map(|environment| {
                    let attributes = crate::monument_attributes::Attributes {
                        version: 3,
                        root_decor: Vec::new(),
                        terrain_checks: Vec::new(),
                        water_checks: Vec::new(),
                        path_connectors: Vec::new(),
                        placement_blockers: Some(Vec::new()),
                        environment: Some(environment),
                        issues: Vec::new(),
                    };
                    crate::environment_checks::Prepared::new(&attributes, || false)?
                        .ok_or_else(|| "Tunnel environment preparation cancelled".to_string())
                })
                .transpose()?;
            prefabs.insert(
                path,
                Prefab {
                    id,
                    cell,
                    replaceable: raw.cells.first().is_some_and(|v| v.replaceable),
                    link,
                    volume,
                    environments,
                    entrance,
                    connectors,
                    surface_offset,
                    terrain,
                    effects,
                    registration,
                },
            );
        }
        for (role, selector) in &grid.selectors {
            for path in &selector.prefabs {
                let p = &prefabs[path];
                if (role == "LinkFolder" && p.link.is_none())
                    || (role != "LinkFolder" && p.cell.is_none())
                    || (role == "StationFolder" && (p.link.is_none() || p.volume.is_none()))
                {
                    return Err("Tunnel selector lacks required authored geometry".into());
                }
            }
        }
        let mut sources = document.sources;
        sources.extend(document.extra_sources);
        Ok(Self {
            sha256: digest(raw),
            prefabs,
            selectors: grid.selectors,
            fields,
            order: grid.order,
            mode: grid.mode,
            active,
            sources,
            catalog_sha256: catalog.sha256.clone(),
            monument_sha256: monument_sha256.to_owned(),
        })
    }
    pub fn load(
        file: &Path,
        root: &Path,
        catalog: &Catalog,
        monuments: &monument_source::Source,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Tunnel preparation cancelled".into());
        }
        let source = Self::decode(&read(file, LIMIT)?, catalog, monuments)?;
        source.verify(root, cancelled)?;
        Ok(source)
    }
    pub(crate) fn verify(&self, root: &Path, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        for receipt in &self.sources {
            if installation::hash_file(
                root,
                &receipt.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Tunnel installation changed".into());
            }
        }
        if cancelled() {
            return Err("Tunnel verification cancelled".into());
        }
        Ok(())
    }
    pub fn prepare(
        root: &Path,
        catalog: &Catalog,
        monuments: &monument_source::Source,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Arc<Self>>, String> {
        Self::prepare_for_binding(root, catalog, &monuments.sha256, &crate::storage::local_data()?, true, cancelled)
    }
    pub(crate) fn for_recipe(
        root: &Path,
        catalog: &Catalog,
        rules: &monument_source::Recipe,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Arc<Self>>, String> {
        rules.validate()?;
        Self::prepare_for_binding(root, catalog, &rules.source_sha256, &crate::storage::local_data()?, false, cancelled)
    }
    pub(crate) fn for_export(
        root: &Path,
        catalog: &Catalog,
        rules: &monument_source::Recipe,
        source_sha256: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<Self>, String> {
        Self::export_snapshot_in(root, catalog, rules, source_sha256, &crate::storage::local_data()?, cancelled)
    }

    fn export_snapshot_in(
        root: &Path,
        catalog: &Catalog,
        rules: &monument_source::Recipe,
        source_sha256: &str,
        local: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<Self>, String> {
        rules.validate()?;
        let file = crate::client_cache::require_pinned_document(
            local, CACHE, source_sha256, NAME, "underground rail",
        )?;
        let retained = session().lock().ok().and_then(|entries| {
            entries.iter().find(|(_, source)| {
                source.sha256 == source_sha256
                    && source.catalog_sha256 == catalog.sha256
                    && source.monument_sha256 == rules.source_sha256
                    && source.sources.starts_with(&catalog.sources)
            }).map(|(_, source)| source.clone())
        });
        if let Some(source) = retained {
            // The digest addresses the accepted map's exact snapshot, including
            // when it has moved into a cache archive since generation.
            let directory = file.parent().ok_or("Missing tunnel snapshot directory")?;
            let _verification = installation::VerificationScope::default();
            if installation::hash_file(directory, NAME, LIMIT, &cancelled)?.sha256 != source_sha256 {
                return Err("Prepared tunnel metadata changed".into());
            }
            source.verify(root, cancelled)?;
            return Ok(source);
        }
        let source = Self::load_binding(&file, root, catalog, &rules.source_sha256, cancelled)?;
        if source.sha256 != source_sha256 {
            return Err("Prepared tunnel metadata changed".into());
        }
        Ok(Arc::new(source))
    }
    fn load_binding(
        file: &Path,
        root: &Path,
        catalog: &Catalog,
        monument_sha256: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Tunnel preparation cancelled".into());
        }
        let source = Self::decode_for_binding(&read(file, LIMIT)?, catalog, monument_sha256)?;
        source.verify(root, cancelled)?;
        Ok(source)
    }
    fn prepare_for_binding(
        root: &Path,
        catalog: &Catalog,
        monument_sha256: &str,
        local: &Path,
        allow_extraction: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Arc<Self>>, String> {
        let _verification = installation::VerificationScope::default();
        if cancelled() {
            return Err("Tunnel preparation cancelled".into());
        }
        let Some(profile) = crate::monument_stages::capture_code_profile(catalog)? else {
            return Ok(None);
        };
        let cache = local.join(CACHE);
        if allow_extraction {
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        } else if !cache.try_exists().map_err(|e| e.to_string())? {
            return Err(MISSING.into());
        }
        let content = installation::hash_file(
            root,
            "Bundles/shared/content.bundle",
            8 * 1024 * 1024 * 1024,
            &cancelled,
        )?;
        let extractors = metadata_runtime::tunnel_extractor_candidates();
        let profile_sha256 = digest(profile.as_bytes());
        let keys = extractors
            .iter()
            .map(|extractor| {
                digest(
                    format!(
                        "tunnels-1:{}:{}:{}:{}:{}:{}",
                        catalog.sha256,
                        monument_sha256,
                        profile_sha256,
                        extractor,
                        content.sha256,
                        content.bytes,
                    )
                    .as_bytes(),
                )
            })
            .collect::<Vec<_>>();
        let key = keys[0].clone();
        let receipt = cache.join(format!("{key}.json"));
        let session_key = (
            cache.canonicalize().map_err(|e| e.to_string())?,
            key.clone(),
        );
        let retained = session().lock().ok().and_then(|entries| {
            entries
                .iter()
                .find(|(id, _)| id == &session_key)
                .map(|(_, source)| source.clone())
        });
        if let Some(source) = retained {
            source.verify(root, cancelled)?;
            return Ok(Some(source));
        }
        for lookup_key in &keys {
            let lookup = cache.join(format!("{lookup_key}.json"));
            if lookup.try_exists().map_err(|e| e.to_string())? {
                let sha: String =
                    serde_json::from_slice(&read(&lookup, 128)?).map_err(|e| e.to_string())?;
                if !hash(&sha) {
                    return Err("Invalid tunnel preparation receipt".into());
                }
                let file = cache.join(&sha).join(NAME);
                if file.try_exists().map_err(|e| e.to_string())? {
                    let source =
                        Self::load_binding(&file, root, catalog, monument_sha256, cancelled)?;
                    if source.sha256 != sha {
                        return Err("Prepared tunnel metadata changed".into());
                    }
                    return Ok(Some(retain(session_key, source)));
                }
            }
        }
        if !allow_extraction {
            return Err(MISSING.into());
        }
        let policy: serde_json::Value = serde_json::from_str(profile).map_err(|e| e.to_string())?;
        let mut required = catalog.sources.clone();
        required
            .push(serde_json::from_value(policy["map_scene"].clone()).map_err(|e| e.to_string())?);
        required.extend(
            serde_json::from_value::<Vec<SourceFile>>(policy["code_sources"].clone())
                .map_err(|e| e.to_string())?,
        );
        for receipt in &required {
            if installation::hash_file(
                root,
                &receipt.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Tunnel installation changed before preparation".into());
            }
        }
        let temporary = tempfile::Builder::new()
            .prefix(".tunnels-")
            .tempdir_in(&cache)
            .map_err(|e| e.to_string())?;
        let profile_path = temporary.path().join("profile.json");
        std::fs::write(&profile_path, profile).map_err(|e| e.to_string())?;
        let output = temporary.path().join(NAME);
        let arguments: Vec<std::ffi::OsString> = vec![
            "--rust-root".into(),
            root.into(),
            "--monuments".into(),
            crate::client_cache::pinned_document(
                local,
                "papyrus-monument-cache",
                monument_sha256,
                "monuments.json",
            )?
            .into(),
            "--code-profile".into(),
            profile_path.into(),
            "--capture-prefabs".into(),
            "--output".into(),
            output.as_os_str().into(),
        ];
        if !metadata_runtime::run(metadata_runtime::Script::Tunnels, &arguments, cancelled)? {
            return Ok(None);
        }
        let source = Self::load_binding(&output, root, catalog, monument_sha256, cancelled)?;
        let staged = temporary.path().join(&source.sha256);
        std::fs::create_dir(&staged).map_err(|e| e.to_string())?;
        std::fs::rename(&output, staged.join(NAME)).map_err(|e| e.to_string())?;
        metadata_runtime::publish(&staged.join(NAME), &cache, &source.sha256, LIMIT)?;
        if cancelled() {
            return Ok(None);
        }
        let mut file = tempfile::NamedTempFile::new_in(&cache).map_err(|e| e.to_string())?;
        serde_json::to_writer(file.as_file_mut(), &source.sha256).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        match file.persist_noclobber(&receipt) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: String =
                    serde_json::from_slice(&read(&receipt, 128)?).map_err(|e| e.to_string())?;
                if existing != source.sha256 {
                    return Err("Concurrent tunnel preparation disagrees".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(retain(session_key, source)))
    }
    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({"source_sha256":self.sha256,"prefabs":self.prefabs.len(),"cells":self.prefabs.values().filter(|p|p.cell.is_some()).count(),
            "links":self.prefabs.values().filter(|p|p.link.is_some()).count(),"entrances":self.prefabs.values().filter(|p|p.entrance.is_some()).count(),
            "terrain_checked_prefabs":self.prefabs.values().filter(|p|p.terrain.is_some()).count(),
            "terrain_checks":self.prefabs.values().filter_map(|p|p.terrain.as_ref()).map(|p|p.rules().heights.len()).sum::<usize>(),
            "terrain_filters":self.prefabs.values().filter_map(|p|p.terrain.as_ref()).map(|p|p.rules().filters.len()).sum::<usize>(),
            "effect_prefabs":self.prefabs.values().filter(|p|p.effects.is_some()).count(),
            "terrain_effects":self.prefabs.values().filter_map(|p|p.effects.as_ref()).map(|p|p.effects().len()).sum::<usize>(),
            "registration_prefabs":self.prefabs.values().filter(|p|p.registration.is_some()).count(),
            "path_connectors":self.prefabs.values().map(|p|p.connectors.len()).sum::<usize>(),
            "environment_volumes":self.prefabs.values().map(|p|p.environments.len()).sum::<usize>(),"game_runtime_validated":false})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const PREFAB: &str = "assets/bundled/prefabs/autospawn/tunnel-station/test.prefab";

    #[test]
    #[ignore = "requires an owned Rust installation and prepared tunnel snapshot"]
    fn owned_export_reuses_the_tunnel_snapshot_and_checks_archived_bytes() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_TUNNEL_TEST_ROOT");
        let local = path("MAPPER_TUNNEL_TEST_LOCAL");
        let project = crate::Project::load(&path("MAPPER_TUNNEL_TEST_PROJECT")).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_TUNNEL_TEST_CATALOG"),
            project.installation.as_ref().unwrap(),
            || false,
        ).unwrap();
        let rules = project.monument_rules.unwrap();
        let prepared = Source::prepare_for_binding(
            &root, &catalog, &rules.source_sha256, &local, false, &|| false,
        ).unwrap().unwrap();
        let original = local.join(CACHE).join(&prepared.sha256).join(NAME);
        let sandbox = tempfile::tempdir().unwrap();
        let archived = sandbox.path().join("client-cache-archive/fixture").join(CACHE)
            .join(&prepared.sha256).join(NAME);
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::copy(&original, &archived).unwrap();

        {
            let _verification = installation::VerificationScope::default();
            let elapsed = std::time::Instant::now();
            let fresh = Source::load_binding(&archived, &root, &catalog, &rules.source_sha256, &|| false).unwrap();
            let fresh_elapsed = elapsed.elapsed();
            assert_eq!(fresh.sha256, prepared.sha256);

            let elapsed = std::time::Instant::now();
            let retained = Source::export_snapshot_in(
                &root, &catalog, &rules, &prepared.sha256, sandbox.path(), &|| false,
            ).unwrap();
            assert!(Arc::ptr_eq(&prepared, &retained));
            eprintln!("Tunnel snapshot load: {fresh_elapsed:?}; reuse: {:?}", elapsed.elapsed());
            assert!(Source::export_snapshot_in(
                &root, &catalog, &rules, &prepared.sha256, sandbox.path(), &|| true,
            ).is_err());
        }

        let mut changed = std::fs::read(&archived).unwrap();
        changed[0] ^= 1;
        std::fs::write(&archived, changed).unwrap();
        let error = Source::export_snapshot_in(
            &root, &catalog, &rules, &prepared.sha256, sandbox.path(), &|| false,
        ).err().unwrap();
        assert_eq!(error, "Prepared tunnel metadata changed");
    }

    #[test]
    fn cache_only_lookup_with_missing_data_leaves_the_cache_untouched() {
        let (catalog, monuments, _) = fixture();
        let root = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        let content = root.path().join("Bundles/shared/content.bundle");
        std::fs::create_dir_all(content.parent().unwrap()).unwrap();
        std::fs::write(&content, [1]).unwrap();
        let lookup = || Source::prepare_for_binding(root.path(), &catalog, &monuments.sha256,
            local.path(), false, &||false).err().unwrap();
        let cache = local.path().join("papyrus-tunnel-cache");
        assert_eq!(lookup(), MISSING);
        assert!(!cache.exists());
        std::fs::create_dir(&cache).unwrap();
        assert_eq!(lookup(), MISSING);
        assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
    }
    fn fixture() -> (Catalog, monument_source::Source, Value) {
        let (mut catalog, _) = crate::infrastructure::tests::fixture();
        let profile_raw = metadata_runtime::CURRENT_MONUMENT_STAGE_CODE_PROFILE;
        let profile: Value = serde_json::from_str(profile_raw).unwrap();
        catalog.installation.native.sha256 = profile["native_sha256"].as_str().unwrap().into();
        catalog.sources[0] = catalog.installation.native.clone();
        catalog.sources[2].sha256 = profile["scene_sha256"].as_str().unwrap().into();
        let monuments = monument_source::Source {
            sha256: "e".repeat(64),
            extractor_sha256: "f".repeat(64),
            profile_sha256: "a".repeat(64),
            prefabs: BTreeMap::new(),
            models_requested: false,
            coastal_profile: None,
            stage_source: None,
            dungeon_source: None,
            offshore_source: None,
            code_sources: Vec::new(),
        };
        let frame = json!({"transform_id":1,"parent_transform":null,"position":[0.,0.,0.],"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],
            "matrix":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],"rotation_chain":[[0.,0.,0.,1.]],"active_without_root":true});
        let mut cell = frame.clone();
        cell["connections"] = json!({"north":0,"south":0,"west":1,"east":1});
        cell["variants"] = json!({"north":0,"south":0,"west":0,"east":0});
        cell["avoid_neighbours"] = json!([]);
        cell["replaceable"] = json!(true);
        let mut link = frame.clone();
        link["up_socket"] = frame.clone();
        link["down_socket"] = frame.clone();
        link["up_type"] = json!(0);
        link["down_type"] = json!(0);
        link["priority"] = json!(0);
        link["rotation_limit"] = json!(0);
        let mut volume = frame;
        volume["center"] = json!([0., 0., 0.]);
        volume["size"] = json!([10., 10., 10.]);
        let mut sources = catalog.sources.clone();
        sources.push(serde_json::from_value(profile["map_scene"].clone()).unwrap());
        sources.extend(
            serde_json::from_value::<Vec<SourceFile>>(profile["code_sources"].clone()).unwrap(),
        );
        let selectors: BTreeMap<_,_> = ["TunnelFolder","StationFolder","UpwardsFolder","TransitionFolder","LinkFolder"].into_iter().map(|role| {
            (role,json!({"prefixes":["assets/bundled/prefabs/autospawn/tunnel-station/"],"prefabs":if ["TunnelFolder","StationFolder","LinkFolder"].contains(&role) {vec![PREFAB]} else {Vec::new()}}))
        }).collect();
        let doc = json!({"format":"mapper-tunnel-grid-source-inspection","schema":1,"sources":sources,
            "extra_sources":[{"relative_path":"Bundles/shared/content.bundle","sha256":"b".repeat(64),"bytes":1}],
            "code_profile_sha256":digest(profile_raw.as_bytes()),"extractor_sha256":metadata_runtime::tunnel_extractor_sha256(),"monument_sha256":monuments.sha256,
            "game_code_executed":false,"full_network_generated":false,
            "programs":[{"identity":["Assembly-CSharp","","GenerateDungeonGrid"],"object_id":1,"order":25,"mode":-1,
                "disabled":null,"hierarchy_active":true,"component_enabled":true,"selectors":selectors,
                "fields":{"ConnectionType":3,"CellSize":216.,"LinkHeight":1.5,"LinkRadius":3.,"LinkTransition":9.}}],
            "prefabs":{PREFAB:{"cells":[cell],"links":[link],"volumes":[volume],"environments":[],"entrances":[],"connectors":[],"nodes_visited":1}},
            "identities":{"prefabs":{PREFAB:123},"neighbours":{}}});
        (catalog, monuments, doc)
    }
    fn decode(
        doc: &Value,
        catalog: &Catalog,
        monuments: &monument_source::Source,
    ) -> Result<Source, String> {
        Source::decode(&serde_json::to_vec(doc).unwrap(), catalog, monuments)
    }
    #[test]
    fn recipe_binding_decoder_requires_the_same_monument_snapshot() {
        let (catalog, monuments, doc) = fixture();
        let raw = serde_json::to_vec(&doc).unwrap();
        let typed = Source::decode(&raw, &catalog, &monuments).unwrap();
        let bound = Source::decode_for_binding(&raw, &catalog, &monuments.sha256).unwrap();
        assert_eq!(bound.sha256, typed.sha256);
        assert_eq!(bound.monument_sha256, typed.monument_sha256);
        assert!(Source::decode_for_binding(&raw, &catalog, &"a".repeat(64)).is_err());
    }
    #[test]
    fn connector_rosters_preserve_inactive_order_and_transform_full_hierarchy_frames() {
        let (catalog, monuments, mut doc) = fixture();
        let mut first = doc["prefabs"][PREFAB]["cells"][0].clone();
        first["object_id"] = json!(70);
        first["infrastructure_type"] = json!(6);
        first["active_without_root"] = json!(false);
        first["matrix"][0][3] = json!(10.);
        let mut second = first.clone();
        second["object_id"] = json!(5);
        second["matrix"][2][3] = json!(20.);
        doc["prefabs"][PREFAB]["connectors"] = json!([first, second]);
        let source = decode(&doc, &catalog, &monuments).unwrap();
        assert!(!source.prefabs[PREFAB].connectors[0].hierarchy_active_without_root);
        let half = -std::f32::consts::FRAC_PI_4;
        let pose = Pose {
            position: [1., 2., 3.],
            rotation: [0., half.sin(), 0., half.cos()],
        };
        let sockets = source.path_sockets(PREFAB, 42, pose, 6).unwrap();
        assert_eq!(
            sockets.iter().map(|s| s.connector).collect::<Vec<_>>(),
            [70, 5]
        );
        for (socket, expected) in sockets.iter().zip([[1., 2., 13.], [-19., 2., 13.]]) {
            for (a, b) in socket.position.into_iter().zip(expected) {
                assert!((a - b).abs() < 0.0001);
            }
            assert!((socket.forward[0] + 1.).abs() < 0.0001);
            assert!(socket.forward[2].abs() < 0.0001);
        }
        assert!(source.path_sockets(PREFAB, 42, pose, 3).unwrap().is_empty());
        assert!(source.path_sockets(PREFAB, 0, pose, 6).is_err());
        doc["prefabs"][PREFAB]["connectors"][1]["object_id"] = json!(70);
        assert!(decode(&doc, &catalog, &monuments).is_err());
    }
    #[test]
    fn captured_registration_uses_shared_runtime_flags_and_root_inclusive_geometry() {
        use crate::{
            environment_checks::Prepared,
            environment_runtime::{Lifecycle, Model},
        };
        use papyrus_terrain::environment::Shape;
        let (catalog, monuments, mut doc) = fixture();
        assert!(
            decode(&doc, &catalog, &monuments).unwrap().prefabs[PREFAB]
                .registration
                .is_none()
        );
        let matrix = [
            [2., 0., 0., 7.],
            [0., 0., -4., 8.],
            [0., 3., 0., 9.],
            [0., 0., 0., 1.],
        ];
        let half = std::f32::consts::FRAC_PI_4;
        doc["prefabs"][PREFAB]["environment"] = json!({
            "root_transform":{"position":[0.,0.,0.],"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.]},
            "volumes":[{"object_id":51,"hierarchy_active":true,"component_enabled":true,
                "type_mask":65,"center":[1.,0.,0.],"size":[2.,4.,6.],"local_to_root":matrix,
                "rotation_chain":[[half.sin(),0.,0.,half.cos()]],"collider_types":[65],"collider_ids":[52],
                "field_names":["Center","Size","Type"],
                "game_object":{"object_id":5,"layer":18,"volume_components":[51],"collider_enabled":[false]}}],
            "checks":[],"issues":[],"collider_meshes":[]});
        let source = decode(&doc, &catalog, &monuments).unwrap();
        let prepared: &Prepared = source.prefabs[PREFAB].registration.as_ref().unwrap();
        let model = Model::reference().unwrap();
        let pose = Transform {
            position: [-10., 20., 30.],
            rotation: [0., half.sin(), 0., half.cos()],
            scale: [1.; 3],
        };
        let targets = |prepared: &Prepared, lifecycle| {
            prepared
                .targets(
                    42,
                    pose,
                    |record| model.modeled_registration(record, lifecycle).registration,
                    || false,
                )
                .unwrap()
                .unwrap()
        };
        let active = targets(prepared, Lifecycle::ActivatedAuthored);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key.instance, 42);
        assert_eq!(active[0].key.component, 51);
        assert_eq!(active[0].mask, 65);
        let Shape::Box(geometry) = active[0].shape else {
            panic!("Active box collider was unresolved")
        };
        // Registered orientation follows root * child. The child X rotation
        // and root Y rotation also catch reversing that multiplication order.
        for (actual, expected) in geometry
            .center
            .into_iter()
            .chain(geometry.extents)
            .chain(geometry.rotation)
            .zip([-1., 28., 21., 2., 6., 12., 0.5, 0.5, -0.5, 0.5])
        {
            assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
        }
        for lifecycle in [
            Lifecycle::SerializedOnly,
            Lifecycle::RootInactive,
            Lifecycle::Removed,
        ] {
            assert!(targets(prepared, lifecycle).is_empty());
        }
        assert!(matches!(
            targets(prepared, Lifecycle::ActivationUnknown)[0].shape,
            Shape::Unresolved { bounds: Some(_) }
        ));
        doc["prefabs"][PREFAB]["environment"]["volumes"][0]["component_enabled"] = json!(false);
        let disabled = decode(&doc, &catalog, &monuments).unwrap();
        assert!(matches!(
            targets(
                disabled.prefabs[PREFAB].registration.as_ref().unwrap(),
                Lifecycle::ActivatedAuthored
            )[0]
            .shape,
            Shape::Unresolved { bounds: None }
        ));
        doc["prefabs"][PREFAB]["environment"]["issues"] = json!(["Unresolved component"]);
        assert!(decode(&doc, &catalog, &monuments).is_err());
    }
    #[test]
    fn generated_effects_keep_source_order_ignore_topology_opacity_and_reject_unknown_effects() {
        let (catalog, monuments, mut doc) = fixture();
        assert!(
            decode(&doc, &catalog, &monuments).unwrap().prefabs[PREFAB]
                .effects
                .is_none()
        );
        let mut raise = doc["prefabs"][PREFAB]["cells"][0].clone();
        raise["kind"] = json!("TerrainHeightRaise");
        raise["matrix"][1][3] = json!(25.);
        raise["fields"] = json!({"Opacity":0.5,"Radius":20.,"Fade":5.});
        let mut topology = raise.clone();
        topology["kind"] = json!("TerrainTopologySet");
        topology["fields"] =
            json!({"Opacity":0.,"TopologyType":-2147483648_i32,"Radius":4.75,"Fade":0.});
        doc["prefabs"][PREFAB]["terrain_checks"] = json!([raise, topology]);
        let source = decode(&doc, &catalog, &monuments).unwrap();
        let effects = source.prefabs[PREFAB].effects.as_ref().unwrap().effects();
        assert_eq!(effects.len(), 2);
        assert!(matches!(
            effects[0],
            prefab_terrain_effects::Effect::RaiseHeight {
                position: [0., 25., 0.],
                opacity: 0.5,
                radius: 20.,
                fade: 5.
            }
        ));
        assert!(matches!(
            effects[1],
            prefab_terrain_effects::Effect::SetTopology {
                mask: 0x80000000,
                radius: 4.75,
                fade: 0.,
                ..
            }
        ));
        doc["prefabs"][PREFAB]["terrain_checks"] = json!([{"kind":"TerrainFutureModifier"}]);
        assert!(decode(&doc, &catalog, &monuments).is_err());
        doc["prefabs"][PREFAB]["terrain_checks"] = json!([]);
        assert!(
            decode(&doc, &catalog, &monuments).unwrap().prefabs[PREFAB]
                .effects
                .as_ref()
                .unwrap()
                .effects()
                .is_empty()
        );
    }
    #[test]
    fn terrain_capture_preserves_indexed_inactive_attributes_and_typed_flags() {
        let (catalog, monuments, mut doc) = fixture();
        assert!(
            decode(&doc, &catalog, &monuments).unwrap().prefabs[PREFAB]
                .terrain
                .is_none()
        );
        let mut height = doc["prefabs"][PREFAB]["cells"][0].clone();
        height["kind"] = json!("TerrainCheck");
        height["active_without_root"] = json!(false);
        height["scale"] = json!([0., 1., -1.]);
        height["matrix"][0][0] = json!(0.);
        height["matrix"][2][2] = json!(-1.);
        height["matrix"][0][3] = json!(20.);
        height["matrix"][1][3] = json!(30.);
        height["fields"] = json!({"Rotate":0,"Extents":64});
        let mut filter = height.clone();
        filter["kind"] = json!("TerrainFilter");
        filter["fields"] = json!({"Filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},
            "CheckPlacementMap":true,"CheckTerrainBounds":1,"CheckTopologyRadius":3.5});
        doc["prefabs"][PREFAB]["terrain_checks"] =
            json!([height, filter, {"kind":"TerrainCollisionTrigger","fields":{"Extents":{}}}]);
        let source = decode(&doc, &catalog, &monuments).unwrap();
        let rules = source.prefabs[PREFAB].terrain.as_ref().unwrap().rules();
        assert_eq!(rules.heights.len(), 1);
        assert_eq!(rules.filters.len(), 1);
        assert_eq!(rules.heights[0].position, [20., 30., 0.]);
        assert!(!rules.heights[0].rotate);
        assert!(rules.filters[0].check_bounds && rules.filters[0].check_placement);
        assert_eq!(rules.filters[0].radius, 3.5);
        doc["prefabs"][PREFAB]["terrain_checks"][0]["fields"]["Rotate"] = json!(2);
        assert!(decode(&doc, &catalog, &monuments).is_err());
        doc["prefabs"][PREFAB]["terrain_checks"] = json!([]);
        assert!(
            decode(&doc, &catalog, &monuments).unwrap().prefabs[PREFAB]
                .terrain
                .as_ref()
                .unwrap()
                .rules()
                .heights
                .is_empty()
        );
    }
    #[test]
    fn portal_context_keeps_environment_first_and_missing_data_budget_and_cancellation_explicit() {
        use papyrus_terrain::{
            environment::{Coverage, Key, Shape, Verdict, World},
            native_layers::{Layers, dimensions},
            tunnel_depth::Policy,
        };
        let (catalog, monuments, doc) = fixture();
        let mut source = decode(&doc, &catalog, &monuments).unwrap();
        source
            .selectors
            .get_mut("UpwardsFolder")
            .unwrap()
            .prefabs
            .push(PREFAB.into());
        let id = source.prefabs[PREFAB].id;
        let (hr, n) = dimensions(1000).unwrap();
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16383; hr * hr],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![0; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let policy = Policy {
            underground: 1,
            train_tunnels: 32,
            entrance: 4096,
            forbidden: 1,
            altitude: 1000.,
            terrain_raise: 9.,
            collision_raise: 10.,
            step: 3.,
            maximum_drop: 1000.,
        };
        let world = World::build(vec![], Coverage::Complete, 10000, || false)
            .unwrap()
            .unwrap();
        assert!(PortalChecks::new(&source, layers.view(), &world, policy).is_err());
        let prefab = source.prefabs.get_mut(PREFAB).unwrap();
        prefab.environments.clear();
        prefab.terrain = Some(
            prefab_terrain::Prepared::new(prefab_terrain::Rules {
                heights: vec![prefab_terrain::HeightCheck {
                    position: [0.; 3],
                    rotate: true,
                    extents: 0.,
                }],
                ..Default::default()
            })
            .unwrap(),
        );
        prefab.connectors = [(6, -30.), (6, 50.), (3, -100.)]
            .into_iter()
            .enumerate()
            .map(|(index, (kind, height))| crate::infrastructure::Connector {
                object_id: index as i64 + 1,
                infrastructure_type: kind,
                position: [0., height, 0.],
                forward: [0., 0., 1.],
                hierarchy_active_without_root: false,
            })
            .collect();
        let context = PortalChecks::new(&source, layers.view(), &world, policy).unwrap();
        assert!(
            context
                .check(id, [0.; 3], 10000, || false)
                .unwrap()
                .rejection
                .is_none()
        );
        assert_eq!(
            context
                .check(id, [0., 10., 0.], 10000, || false)
                .unwrap()
                .rejection,
            Some(PortalRejection::Terrain(prefab_terrain::Rejection::Height))
        );
        assert!(matches!(
            context.check(id, [0.; 3], 1, || false),
            Err(PortalError::Work(
                papyrus_terrain::tunnel_network::Failure::WorkLimit
            ))
        ));
        assert!(matches!(
            context.check(id, [0.; 3], 10000, || true),
            Err(PortalError::Work(
                papyrus_terrain::tunnel_network::Failure::Cancelled
            ))
        ));
        assert!(matches!(
            context.check(0, [0.; 3], 10000, || false),
            Err(PortalError::Invalid(_))
        ));
        let cut = f64::from(
            papyrus_terrain::spawn::sample_height_view(layers.view(), 0.5, 0.5).unwrap(),
        ) + 30.;
        for (maximum_cut, expected) in [
            (cut - 0.01, Some(PortalRejection::RailAccess)),
            (cut, None),
            (cut + 0.01, None),
        ] {
            let context = PortalChecks::new(&source, layers.view(), &world, policy).unwrap();
            let access = PortalRailAccess {
                rail_type: 6,
                maximum_cut,
                maximum_grade: 0.15,
                extension_spacing: 7.5,
                minimum_extension_steps: 8,
                half_width: 3.,
            };
            let result = context
                .check_rail_access(id, [0.; 3], access, 10000, || false)
                .unwrap();
            assert_eq!(result.rejection, expected);
            assert!(result.work >= 5);
            assert!(matches!(
                context.check_rail_access(id, [0.; 3], access, 4, || false),
                Err(PortalError::Work(papyrus_terrain::tunnel_network::Failure::WorkLimit))
            ));
            assert!(matches!(
                context.check_rail_access(id, [0.; 3], access, 10000, || true),
                Err(PortalError::Work(papyrus_terrain::tunnel_network::Failure::Cancelled))
            ));
        }
        for invalid in [f64::NAN, -1., 65.] {
            assert!(context
                .check_rail_access(
                    id,
                    [0.; 3],
                    PortalRailAccess {
                        rail_type: 6,
                        maximum_cut: invalid,
                        maximum_grade: 0.15,
                        extension_spacing: 7.5,
                        minimum_extension_steps: 8,
                        half_width: 3.,
                    },
                    10000,
                    || false,
                )
                .is_err());
        }
        // The socket itself remains clear while a bank blocks its approach.
        let x = hr / 2;
        let z = ((40. / 1000. + 0.5) * (hr - 1) as f32) as usize;
        layers.height[z * hr + x] = 18021; // Approximately 50 metres.
        let context = PortalChecks::new(&source, layers.view(), &world, policy).unwrap();
        assert_eq!(
            context.check_rail_access(id, [0.; 3], PortalRailAccess {
                rail_type: 6,
                maximum_cut: cut,
                maximum_grade: 0.15,
                extension_spacing: 7.5,
                minimum_extension_steps: 8,
                half_width: 3.,
            }, 10000, || false).unwrap().rejection,
            Some(PortalRejection::RailAccess)
        );
        layers.height[z * hr + x] = 16383;
        let geometry = Box3 {
            center: [0.; 3],
            rotation: [0., 0., 0., 1.],
            extents: [1.; 3],
        };
        source
            .prefabs
            .get_mut(PREFAB)
            .unwrap()
            .environments
            .push(Volume { mask: 1, geometry });
        let blocked = World::build(
            vec![environment::Volume {
                key: Key {
                    instance: 1,
                    component: 1,
                },
                mask: 1,
                shape: Shape::Box(geometry),
            }],
            Coverage::Complete,
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        layers.height.fill(-1);
        let context = PortalChecks::new(&source, layers.view(), &blocked, policy).unwrap();
        assert_eq!(
            context
                .check(id, [0.; 3], 10000, || false)
                .unwrap()
                .rejection,
            Some(PortalRejection::Environment)
        );
        let incomplete = World::build(vec![], Coverage::Incomplete, 10000, || false)
            .unwrap()
            .unwrap();
        let context = PortalChecks::new(&source, layers.view(), &incomplete, policy).unwrap();
        assert!(matches!(
            context.check(id, [0.; 3], 10000, || false),
            Err(PortalError::Environment(
                papyrus_terrain::tunnel_depth::Error::World(Verdict::Incomplete)
            ))
        ));
    }
    #[test]
    fn typed_model_retains_installed_ids_geometry_and_source_activation() {
        let (catalog, monuments, mut doc) = fixture();
        doc["programs"]
            .as_array_mut()
            .unwrap()
            .push(json!({"identity":["Assembly-CSharp","","PlaceMonuments"],
            "selectors":{"ResourceFolder":"tunnel-entrance"}}));
        doc["prefabs"][PREFAB]["cells"][0]["avoid_neighbours"] = json!([{"guid":"a".repeat(32)}]);
        doc["identities"]["neighbours"]["a".repeat(32)] = json!({"prefab_id":456});
        let source = decode(&doc, &catalog, &monuments).unwrap();
        assert!(source.active);
        assert_eq!(source.mode, -1);
        assert_eq!(source.fields.cell_size, 216.);
        assert_eq!(source.prefabs[PREFAB].cell.as_ref().unwrap().id, 123);
        assert_eq!(
            source.prefabs[PREFAB]
                .cell
                .as_ref()
                .unwrap()
                .avoid_neighbours,
            vec![456]
        );
        assert_eq!(source.prefabs[PREFAB].volume.unwrap().extents, [5.; 3]);
        assert_eq!(source.sources.len(), 7);
    }
    #[test]
    fn owner_snap_matches_exported_unity_yaw_and_station_exclusion() {
        let (catalog, monuments, doc) = fixture();
        let mut source = decode(&doc, &catalog, &monuments).unwrap();
        source.prefabs.get_mut(PREFAB).unwrap().entrance = Some(Entrance {
            frame: Transform {
                position: [4., 2., 8.],
                ..unit()
            },
            snap: Snap {
                radius: 3.,
                height: 1.5,
            },
            cell_size: 216.,
            connector: unit(),
        });
        let fitted = source
            .snap_entrance(PREFAB, [10., 100., 20.], 90., 4000.)
            .unwrap()
            .unwrap();
        let half = -90_f32.to_radians() * 0.5;
        let rotated = environment::rotate([0., half.sin(), 0., half.cos()], [4., 2., 8.]);
        let original = std::array::from_fn(|i| [10., 100., 20.][i] + rotated[i]);
        let snapped = Snap {
            radius: 3.,
            height: 1.5,
        }
        .position(original)
        .unwrap();
        for i in 0..3 {
            assert!((fitted[i] - ([10., 100., 20.][i] + snapped[i] - original[i])).abs() < 0.00001);
        }
        assert_eq!(
            source.entrance_spacing(PREFAB).unwrap().minimum_distance,
            540.
        );
        assert!(
            source
                .snap_entrance(PREFAB, [0.; 3], f32::NAN, 4000.)
                .is_err()
        );
        source
            .prefabs
            .get_mut(PREFAB)
            .unwrap()
            .entrance
            .as_mut()
            .unwrap()
            .frame
            .position = [0.; 3];
        assert!(
            source
                .snap_entrance(PREFAB, [108., 15., 108.], 0., 4000.)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            source
                .snap_entrance("other", [1., 2., 3.], 0., 4000.)
                .unwrap(),
            Some([1., 2., 3.])
        );
    }
    #[test]
    fn portal_surface_marker_uses_first_active_hierarchy_frame() {
        let (catalog, monuments, mut doc) = fixture();
        assert!(
            decode(&doc, &catalog, &monuments).unwrap().prefabs[PREFAB]
                .surface_offset
                .is_none()
        );
        let mut marker = doc["prefabs"][PREFAB]["cells"][0].clone();
        marker["position"] = json!([0., 45., -324.]);
        marker["matrix"] = json!([
            [1., 0., 0., 324.],
            [0., 1., 0., 45.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.]
        ]);
        let mut inactive = marker.clone();
        inactive["active_without_root"] = json!(false);
        inactive["matrix"][0][3] = json!(100.);
        doc["prefabs"][PREFAB]["monument_infos"] = json!([inactive, marker.clone()]);
        let source = decode(&doc, &catalog, &monuments).unwrap();
        assert_eq!(source.prefabs[PREFAB].surface_offset, Some([324., 45., 0.]));
        doc["prefabs"][PREFAB]["monument_infos"] = json!(vec![marker; 257]);
        assert!(decode(&doc, &catalog, &monuments).is_err());
    }
    #[test]
    fn equivalent_tunnel_capture_keeps_its_saved_provenance_and_source_checks() {
        let (catalog, monuments, mut doc) = fixture();
        let previous = "1585f5102f90916b8b203e1e6aac6983c666999840353ec46ad9c652bbab44ac";
        doc["extractor_sha256"] = json!(previous);
        let allowed = metadata_runtime::tunnel_extractor_candidates()
            .iter()
            .any(|v| v == previous);
        assert_eq!(decode(&doc, &catalog, &monuments).is_ok(), allowed);
        doc["sources"][0]["sha256"] = json!("0".repeat(64));
        assert!(decode(&doc, &catalog, &monuments).is_err());
    }
    #[test]
    fn changed_sources_adapters_or_input_rules_cannot_reuse_capture() {
        let (catalog, monuments, doc) = fixture();
        for field in ["monument_sha256", "code_profile_sha256", "extractor_sha256"] {
            let mut changed = doc.clone();
            changed[field] = json!("0".repeat(64));
            assert!(decode(&changed, &catalog, &monuments).is_err());
        }
        let mut changed = doc;
        changed["sources"][0]["sha256"] = json!("0".repeat(64));
        assert!(decode(&changed, &catalog, &monuments).is_err());
    }
    #[test]
    fn missing_pools_unknown_roles_and_unresolved_neighbours_are_explicit() {
        let (catalog, monuments, doc) = fixture();
        let mut changed = doc.clone();
        changed["programs"][0]["selectors"]["StationFolder"]["prefabs"] = json!([]);
        assert!(decode(&changed, &catalog, &monuments).is_err());
        let mut changed = doc.clone();
        changed["programs"][0]["selectors"]["UnknownFolder"] = json!({"prefixes":[],"prefabs":[]});
        assert!(decode(&changed, &catalog, &monuments).is_err());
        let mut changed = doc;
        changed["prefabs"][PREFAB]["cells"][0]["avoid_neighbours"] =
            json!([{"guid":"a".repeat(32)}]);
        assert!(decode(&changed, &catalog, &monuments).is_err());
    }
    #[test]
    fn reflected_environment_geometry_and_invalid_link_rotations_are_rejected() {
        let (catalog, monuments, doc) = fixture();
        let mut changed = doc.clone();
        let mut environment = doc["prefabs"][PREFAB]["volumes"][0].clone();
        environment["mask"] = json!(33);
        environment["matrix"][0][0] = json!(-1.);
        changed["prefabs"][PREFAB]["environments"] = json!([environment]);
        assert!(decode(&changed, &catalog, &monuments).is_err());
        let mut changed = doc;
        changed["prefabs"][PREFAB]["links"][0]["rotation_limit"] = json!(361);
        assert!(decode(&changed, &catalog, &monuments).is_err());
    }
}
