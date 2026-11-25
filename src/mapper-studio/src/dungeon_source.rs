//! Receipt-bound dungeon modules and numeric assembly against the final terrain.
use crate::{
    installation::{self, SourceFile},
    metadata_runtime, monument_source,
    prefab_assets::Catalog,
};
use metadata_runtime::dungeon_extractor_sha256 as extractor;
use papyrus_terrain::{
    dungeon_base::{self as dungeon, Pose},
    environment::{self, Transform},
    native_layers::Layers,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
const LIMIT: u64 = 16 * 1024 * 1024;
const PREFIX: &str = "assets/bundled/prefabs/autospawn/";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub engine: String,
    pub source_sha256: String,
}
impl Binding {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != dungeon::VERSION || !hash(&self.source_sha256) {
            return Err("Unsupported dungeon assembly binding".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    native_sha256: String,
    scene_sha256: String,
    stage_profile_id: String,
    environment_mask: u32,
    connection_type: i32,
    socket_roles: BTreeMap<String, u8>,
    link_roles: BTreeMap<String, u8>,
    scope: String,
}
fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn profile(catalog: &Catalog) -> Result<Option<(Profile, &'static str)>, String> {
    let Some(stage) = crate::monument_stages::capture_code_profile(catalog)? else {
        return Ok(None);
    };
    for raw in [
        include_str!("../../../compatibility/dungeons/client-0f703d43.json"),
        include_str!("../../../compatibility/dungeons/client-8dd4e235.json"),
        include_str!("../../../compatibility/dungeons/client-58f3263f.json"),
    ] {
        let p: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if p.native_sha256 == catalog.installation.native.sha256
            && catalog.sources.iter().any(|s| {
                s.relative_path == "Bundles/shared/assetscenes.bundle" && s.sha256 == p.scene_sha256
            })
        {
            if p.format != "papyrus-dungeon-profile"
                || p.schema != 1
                || p.id.is_empty()
                || p.id.len() > 128
                || p.scope.len() > 4096
                || p.stage_profile_id
                    != serde_json::from_str::<serde_json::Value>(stage)
                        .map_err(|e| e.to_string())?["id"]
                        .as_str()
                        .unwrap_or("")
                || !p.environment_mask.is_power_of_two()
                || !(0..=64).contains(&p.connection_type)
                || p.socket_roles
                    != BTreeMap::from([
                        ("Horizontal".into(), 0),
                        ("Vertical".into(), 1),
                        ("Pillar".into(), 2),
                    ])
                || p.link_roles
                    != BTreeMap::from([
                        ("Room".into(), 0),
                        ("Corridor".into(), 1),
                        ("End".into(), 2),
                    ])
            {
                return Err("Dungeon adapter differs from its stage code or enum roles".into());
            }
            return Ok(Some((p, stage)));
        }
    }
    Ok(None)
}
#[derive(Clone, Deserialize)]
struct Local {
    object_id: i64,
    transform_id: i64,
    parent_transform: Option<i64>,
    position: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}
impl Local {
    fn pose(&self) -> Result<Pose, String> {
        if self.object_id == 0
            || self.transform_id == 0
            || self.parent_transform == Some(0)
            || self
                .scale
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err("Invalid dungeon local transform".into());
        }
        Transform {
            position: self.position,
            rotation: self.rotation,
            scale: [1.; 3],
        }
        .validate()?;
        Ok(Pose {
            position: self.position,
            rotation: self.rotation,
        })
    }
}
#[derive(Deserialize)]
struct Socket {
    #[serde(flatten)]
    local: Local,
    kind: u8,
    male: bool,
    female: bool,
    matrix: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
}
#[derive(Deserialize)]
struct Volume {
    #[serde(flatten)]
    local: Local,
    center: [f32; 3],
    size: [f32; 3],
}
#[derive(Deserialize)]
struct Link {
    #[serde(flatten)]
    local: Local,
    kind: u8,
    cost: u32,
    maximum_floor: i32,
    maximum_local: i32,
    maximum_global: i32,
    count_identifier: i32,
    sockets: Vec<Socket>,
    volumes: Vec<Volume>,
}
#[derive(Deserialize)]
struct Transition {
    #[serde(flatten)]
    local: Local,
    kind: u8,
    neighbour1: u8,
    neighbour2: u8,
}
#[derive(Deserialize)]
struct Environment {
    #[serde(flatten)]
    local: Local,
    mask: i32,
    center: [f32; 3],
    size: [f32; 3],
    matrix: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
}
#[derive(Deserialize)]
struct Entrance {
    #[serde(flatten)]
    local: Local,
    matrix: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prefab {
    links: Vec<Link>,
    transitions: Vec<Transition>,
    environment_volumes: Vec<Environment>,
    entrances: Vec<Entrance>,
    nodes_visited: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    object_id: i64,
    order: usize,
    mode: i32,
    disabled: Option<bool>,
    hierarchy_active: bool,
    component_enabled: Option<bool>,
    connection_type: i32,
    folders: BTreeMap<String, String>,
    prefabs: BTreeMap<String, Vec<String>>,
    roots: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    activation: String,
    catalog_sha256: String,
    monument_sha256: String,
    code_profile_sha256: String,
    extractor_sha256: String,
    sources: Vec<SourceFile>,
    stages: Vec<Stage>,
    prefabs: BTreeMap<String, Prefab>,
    full_stage_execution_validated: bool,
}
pub struct Source {
    pub sha256: String,
    document: Document,
    profile: Profile,
}
struct Footprint {
    transform: Transform,
    center: [f32; 3],
    size: [f32; 3],
}
struct Geometry {
    prefabs: Vec<dungeon::Prefab>,
    environments: Vec<Vec<Footprint>>,
}
fn safe_path(path: &str) -> bool {
    path.starts_with(PREFIX)
        && path.ends_with(".prefab")
        && path.len() <= 512
        && !path.contains(['\\', '\0'])
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == ".." || p == ".")
}
impl Source {
    pub fn binding(&self) -> Binding {
        Binding {
            engine: dungeon::VERSION.into(),
            source_sha256: self.sha256.clone(),
        }
    }
    fn decode(
        raw: &[u8],
        catalog: &Catalog,
        monuments: &monument_source::Source,
    ) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Dungeon capture exceeds 16 MiB".into());
        }
        let document: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid dungeon capture: {e}"))?;
        let (profile, stage_raw) =
            profile(catalog)?.ok_or("Dungeon adapter unavailable for this installation")?;
        let stage: serde_json::Value =
            serde_json::from_str(stage_raw).map_err(|e| e.to_string())?;
        let mut expected = catalog.sources.clone();
        expected
            .push(serde_json::from_value(stage["map_scene"].clone()).map_err(|e| e.to_string())?);
        expected.extend(
            serde_json::from_value::<Vec<SourceFile>>(stage["code_sources"].clone())
                .map_err(|e| e.to_string())?,
        );
        let hierarchy = stage["activation"].as_str() == Some("active-in-hierarchy-client-uncached");
        if document.format != "papyrus-dungeon-source"
            || document.schema != 1
            || document.activation != if hierarchy { "hierarchy" } else { "legacy" }
            || document.catalog_sha256 != catalog.sha256
            || document.monument_sha256 != monuments.sha256
            || !crate::source_profile::matches(stage_raw, &document.code_profile_sha256)
            || !hash(&document.extractor_sha256)
            || document.sources != expected
            || document.full_stage_execution_validated
            || !(1..=16).contains(&document.stages.len())
            || !(1..=1024).contains(&document.prefabs.len())
        {
            return Err(
                "Dungeon capture differs from its installation, input rules or adapter".into(),
            );
        }
        let mut ids = BTreeSet::new();
        let mut orders = BTreeSet::new();
        let mut selected = BTreeSet::new();
        let roles: BTreeSet<_> = [
            "EntranceFolder",
            "LinkFolder",
            "EndFolder",
            "TransitionFolder",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for stage in &document.stages {
            if stage.object_id <= 0
                || !ids.insert(stage.object_id)
                || stage.order > 65535
                || !orders.insert(stage.order)
                || !(-1..=3).contains(&stage.mode)
                || stage.connection_type != profile.connection_type
                || stage.disabled.is_none() != hierarchy
                || stage.component_enabled.is_some() != hierarchy
                || stage.folders.keys().cloned().collect::<BTreeSet<_>>() != roles
                || stage.prefabs.keys().cloned().collect::<BTreeSet<_>>() != roles
                || stage.roots.len() > 512
            {
                return Err("Invalid dungeon stage metadata".into());
            }
            for (role, paths) in &stage.prefabs {
                let prefix = &stage.folders[role];
                if !prefix.starts_with(PREFIX)
                    || !prefix.ends_with('/')
                    || prefix.contains(['\\', '\0'])
                    || prefix.split('/').any(|p| p == ".." || p == ".")
                    || !(1..=512).contains(&paths.len())
                    || !paths.windows(2).all(|p| p[0] < p[1])
                    || paths
                        .iter()
                        .any(|p| !safe_path(p) || !p.starts_with(prefix))
                {
                    return Err("Invalid dungeon folder or prefab selectors".into());
                }
                selected.extend(paths.iter().cloned());
            }
            let roots: Vec<_> = monuments
                .prefabs
                .iter()
                .filter(|(_, p)| {
                    p.placement_attributes.as_ref().is_some_and(|a| {
                        a.path_connectors
                            .iter()
                            .any(|c| c.infrastructure_type == stage.connection_type)
                    })
                })
                .map(|(path, _)| path.clone())
                .collect();
            if roots != stage.roots {
                return Err("Dungeon roots differ from authored connector roles".into());
            }
            selected.extend(roots);
        }
        if selected != document.prefabs.keys().cloned().collect() {
            return Err("Dungeon prefab roster is incomplete".into());
        }
        let result = Self {
            sha256: digest(raw),
            document,
            profile,
        };
        // Conversion validates every consumed transform before any search begins.
        let geometry = result.geometry()?;
        let indices = result
            .document
            .prefabs
            .keys()
            .enumerate()
            .map(|(i, p)| (p.as_str(), i))
            .collect::<BTreeMap<_, _>>();
        for stage in &result.document.stages {
            dungeon::validate(&geometry.prefabs, &pools(stage, &indices), &[]).map_err(failure)?;
            if stage.roots.iter().any(|path| {
                result.document.prefabs[path].entrances.len() != 1
                    || result.document.prefabs[path].links.len() != 1
            }) {
                return Err("Dungeon root lacks its assembly origin or link".into());
            }
        }
        Ok(result)
    }
    fn geometry(&self) -> Result<Geometry, String> {
        let mut prefabs = Vec::new();
        let mut environments = Vec::new();
        for p in self.document.prefabs.values() {
            if p.links.len() + p.transitions.len() != 1
                || p.entrances.len() > 1
                || p.environment_volumes.len() > 256
                || !(1..=65536).contains(&p.nodes_visited)
            {
                return Err("Invalid dungeon component roster".into());
            }
            let link = p
                .links
                .first()
                .map(|l| -> Result<_, String> {
                    l.local.pose()?;
                    let mut sockets = Vec::new();
                    for s in &l.sockets {
                        let pose = s.local.pose()?;
                        let world = local(s.matrix, &s.rotation_chain)?;
                        sockets.push(dungeon::Socket {
                            kind: s.kind,
                            male: s.male,
                            female: s.female,
                            position: pose.position,
                            rotation: pose.rotation,
                            forward: environment::rotate(world.rotation, [0., 0., 1.]),
                        });
                    }
                    let volumes = l
                        .volumes
                        .iter()
                        .map(|v| {
                            Ok(dungeon::Volume {
                                pose: v.local.pose()?,
                                center: v.center,
                                size: v.size,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    Ok(dungeon::Link {
                        kind: l.kind,
                        cost: l.cost,
                        maximum_floor: l.maximum_floor,
                        maximum_local: l.maximum_local,
                        maximum_global: l.maximum_global,
                        count_identifier: l.count_identifier,
                        sockets,
                        volumes,
                    })
                })
                .transpose()?;
            let transition = p
                .transitions
                .first()
                .map(|t| -> Result<_, String> {
                    t.local.pose()?;
                    Ok(dungeon::Transition {
                        kind: t.kind,
                        neighbours: [t.neighbour1, t.neighbour2],
                    })
                })
                .transpose()?;
            let mut volumes = Vec::new();
            for e in &p.environment_volumes {
                e.local.pose()?;
                let transform = local(e.matrix, &e.rotation_chain)?;
                environment::volume_box(
                    transform,
                    e.center,
                    e.size,
                    Transform {
                        position: [0.; 3],
                        rotation: [0., 0., 0., 1.],
                        scale: [1.; 3],
                    },
                )?;
                if e.mask as u32 & self.profile.environment_mask != 0 {
                    volumes.push(Footprint {
                        transform,
                        center: e.center,
                        size: e.size,
                    });
                }
            }
            for e in &p.entrances {
                e.local.pose()?;
                local(e.matrix, &e.rotation_chain)?;
            }
            prefabs.push(dungeon::Prefab { link, transition });
            environments.push(volumes);
        }
        Ok(Geometry {
            prefabs,
            environments,
        })
    }
    pub fn load(
        path: &Path,
        root: &Path,
        catalog: &Catalog,
        monuments: &monument_source::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let source = Self::decode(&bytes, catalog, monuments)?;
        for receipt in &source.document.sources {
            if cancelled() {
                return Err("Dungeon verification cancelled".into());
            }
            if installation::hash_file(
                root,
                &receipt.relative_path,
                2 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Dungeon installation sources changed".into());
            }
        }
        Ok(source)
    }
    pub fn cached(
        sha: &str,
        root: &Path,
        catalog: &Catalog,
        monuments: &monument_source::Source,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        if !hash(sha) {
            return Err("Invalid dungeon source fingerprint".into());
        }
        let path = crate::client_cache::pinned_document(
            &crate::storage::local_data()?,
            "papyrus-dungeon-cache",
            sha,
            "dungeons.json",
        )?;
        if !path.try_exists().map_err(|e| e.to_string())? {
            let restored = Self::prepare(root, catalog, monuments, cancelled)?
                .ok_or("Dungeon restoration cancelled or unavailable")?;
            if restored.sha256 != sha {
                return Err("Saved dungeon source requires a different capture adapter".into());
            }
            return Ok(restored);
        }
        let source = Self::load(&path, root, catalog, monuments, cancelled)?;
        if source.sha256 != sha {
            return Err("Pinned dungeon metadata changed".into());
        }
        Ok(source)
    }
    pub fn prepare(
        root: &Path,
        catalog: &Catalog,
        monuments: &monument_source::Source,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let Some((_, stage)) = profile(catalog)? else {
            return Ok(None);
        };
        let local = crate::storage::local_data()?;
        let cache = local.join("papyrus-dungeon-cache");
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        let key = digest(
            format!(
                "{}:{}:{}:{}",
                catalog.sha256,
                monuments.sha256,
                digest(stage.as_bytes()),
                extractor()
            )
            .as_bytes(),
        );
        let receipt = cache.join(format!("{key}.json"));
        if receipt.is_file() {
            let sha = read_receipt(&receipt)?;
            if cache
                .join(&sha)
                .join("dungeons.json")
                .try_exists()
                .map_err(|e| e.to_string())?
            {
                return Self::cached(&sha, root, catalog, monuments, cancelled).map(Some);
            }
        }
        let temporary = tempfile::Builder::new()
            .prefix(".dungeons-")
            .tempdir_in(&cache)
            .map_err(|e| e.to_string())?;
        let profile_path = temporary.path().join("profile.json");
        std::fs::write(&profile_path, stage).map_err(|e| e.to_string())?;
        let output = temporary.path().join("dungeons.json");
        let catalog_path = crate::asset_discovery::catalog_path(
            &local.join("papyrus-prefab-cache"),
            &catalog.sha256,
        )?;
        let monument_path = crate::client_cache::pinned_document(
            &local,
            "papyrus-monument-cache",
            &monuments.sha256,
            "monuments.json",
        )?;
        if !metadata_runtime::run(
            metadata_runtime::Script::Dungeons,
            &[
                "--rust-root".into(),
                root.into(),
                "--catalog".into(),
                catalog_path.into(),
                "--monuments".into(),
                monument_path.into(),
                "--code-profile".into(),
                profile_path.into(),
                "--output".into(),
                output.as_os_str().into(),
            ],
            cancelled,
        )? {
            return Ok(None);
        }
        let source = Self::load(&output, root, catalog, monuments, cancelled)?;
        let destination = cache.join(&source.sha256);
        std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
        let mut payload =
            tempfile::NamedTempFile::new_in(&destination).map_err(|e| e.to_string())?;
        let mut input = std::fs::File::open(&output)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1);
        if std::io::copy(&mut input, payload.as_file_mut()).map_err(|e| e.to_string())? > LIMIT {
            return Err("Dungeon output changed beyond its capture bound".into());
        }
        payload.as_file().sync_all().map_err(|e| e.to_string())?;
        match payload.persist_noclobber(destination.join("dungeons.json")) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        Self::cached(&source.sha256, root, catalog, monuments, cancelled)?;
        if cancelled() {
            return Ok(None);
        }
        let mut file = tempfile::NamedTempFile::new_in(&cache).map_err(|e| e.to_string())?;
        serde_json::to_writer(file.as_file_mut(), &source.sha256).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        match file.persist_noclobber(&receipt) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_receipt(&receipt)? != source.sha256 {
                    return Err(
                        "Concurrent dungeon preparation published different metadata".into(),
                    );
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        Ok(Some(source))
    }
    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({"engine":dungeon::VERSION,"source_sha256":self.sha256,"profile":self.profile.id,"stages":self.document.stages.len(),"prefabs":self.document.prefabs.len(),"roots":self.document.stages.iter().map(|s|s.roots.len()).sum::<usize>(),"game_runtime_validated":false})
    }
    pub fn generate(
        &self,
        project: &crate::Project,
        solution: &papyrus_terrain::placement::Solution,
        layers: &Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<dungeon::Scene>, String> {
        if layers.size != project.size
            || project
                .monument_rules
                .as_ref()
                .is_none_or(|r| r.source_sha256 != self.document.monument_sha256)
        {
            return Err("Dungeon generation inputs differ from their source".into());
        }
        let (height_resolution, surface_resolution) =
            papyrus_terrain::native_layers::dimensions(layers.size)?;
        if layers.height_resolution != height_resolution
            || layers.surface_resolution != surface_resolution
            || layers.height.len() != height_resolution * height_resolution
        {
            return Err("Invalid dungeon terrain inputs".into());
        }
        let geometry = self.geometry()?;
        let paths: Vec<_> = self.document.prefabs.keys().cloned().collect();
        let indices = paths
            .iter()
            .enumerate()
            .map(|(i, p)| (p.as_str(), i))
            .collect::<BTreeMap<_, _>>();
        let targets = project
            .targets
            .iter()
            .map(|t| (t.instance_id, t))
            .collect::<BTreeMap<_, _>>();
        let mut result = dungeon::Scene {
            engine: dungeon::VERSION.into(),
            source_sha256: self.sha256.clone(),
            monument_sha256: self.document.monument_sha256.clone(),
            layout_fingerprint: crate::composition::layout_fingerprint(solution),
            seed: project.seeds[4].value,
            stages: Vec::new(),
            instances: Vec::new(),
        };
        let mut remaining_work = 64_000_000_usize;
        let mut stages: Vec<_> = self.document.stages.iter().collect();
        stages.sort_by_key(|s| s.order);
        for stage in stages {
            if cancelled() {
                return Ok(None);
            }
            let hierarchy = self.document.activation == "hierarchy";
            let active = if hierarchy {
                stage.hierarchy_active
            } else {
                !stage.disabled.unwrap()
            };
            let realm = if hierarchy { 1 } else { 2 };
            if !active || stage.mode & realm == 0 || !project.performance.monuments {
                continue;
            }
            let mut roots = Vec::new();
            for outcome in &solution.outcomes {
                let Some(position) = &outcome.position else {
                    continue;
                };
                let target = targets
                    .get(&outcome.id)
                    .ok_or("Dungeon layout has an unknown monument")?;
                let Some(path) = target
                    .prefab_path
                    .as_deref()
                    .filter(|p| stage.roots.iter().any(|r| r == p))
                else {
                    continue;
                };
                let data = &self.document.prefabs[path];
                let entrance = &data.entrances[0];
                let origin = local(entrance.matrix, &entrance.rotation_chain)?;
                let (sin, cos) = (-position.yaw.to_radians() * 0.5).sin_cos();
                let pose = Pose {
                    position: [position.x, position.y, position.z],
                    rotation: [0., sin, 0., cos],
                };
                roots.push(dungeon::Root {
                    owner: outcome.id,
                    prefab: indices[path],
                    pose: Pose {
                        position: pose.point(origin.position),
                        rotation: environment::multiply(pose.rotation, origin.rotation),
                    },
                });
            }
            let root_ids = roots.iter().map(|r| r.owner).collect();
            let seed = project.seeds[4].value ^ (stage.object_id as u32).rotate_left(13);
            let assembly = dungeon::generate(
                &geometry.prefabs,
                pools(stage, &indices),
                &roots,
                seed,
                dungeon::Limits {
                    maximum_work: remaining_work,
                    maximum_segments: 4096_usize.saturating_sub(result.instances.len()),
                },
                |prefab, pose, work| {
                    let transform = Transform {
                        position: pose.position,
                        rotation: pose.rotation,
                        scale: [1.; 3],
                    };
                    for footprint in &geometry.environments[prefab] {
                        let bounds = environment::volume_box(
                            footprint.transform,
                            footprint.center,
                            footprint.size,
                            transform,
                        )
                        .map_err(dungeon::Failure::Invalid)?;
                        if !outside_terrain(layers, bounds, work)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                },
                &cancelled,
            );
            let assembly = match assembly {
                Ok(value) => value,
                Err(dungeon::Failure::Cancelled) => return Ok(None),
                Err(error) => return Err(failure(error)),
            };
            remaining_work = remaining_work.saturating_sub(assembly.work);
            let before = result.instances.len();
            for segment in assembly.segments.into_iter().filter(|s| !s.root) {
                result.instances.push(dungeon::Instance {
                    owner: segment.owner,
                    stage: stage.object_id,
                    prefab: paths[segment.prefab].clone(),
                    pose: segment.pose,
                });
            }
            result.stages.push(dungeon::StageReport {
                object_id: stage.object_id,
                order: stage.order,
                roots: root_ids,
                work: assembly.work,
                instances: result.instances.len() - before,
                audit: assembly.audit,
            });
        }
        Ok((!cancelled()).then_some(result))
    }
}
fn read_receipt(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let sha: String = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if !hash(&sha) {
        return Err("Invalid dungeon preparation receipt".into());
    }
    Ok(sha)
}
fn pools(stage: &Stage, indices: &BTreeMap<&str, usize>) -> dungeon::Pools {
    let role = |name: &str| {
        stage.prefabs[name]
            .iter()
            .map(|p| indices[p.as_str()])
            .collect()
    };
    dungeon::Pools {
        entrances: role("EntranceFolder"),
        links: role("LinkFolder"),
        ends: role("EndFolder"),
        transitions: role("TransitionFolder"),
    }
}
fn failure(error: dungeon::Failure) -> String {
    match error {
        dungeon::Failure::Invalid(error) => error,
        dungeon::Failure::Budget => "Dungeon assembly work or instance allowance reached".into(),
        dungeon::Failure::Cancelled => "Dungeon assembly cancelled".into(),
    }
}
fn outside_terrain(
    layers: &Layers,
    bounds: environment::Box3,
    work: &mut dungeon::Work<'_>,
) -> Result<bool, dungeon::Failure> {
    let corners = [[-1., -1.], [1., -1.], [-1., 1.], [1., 1.]].map(|[x, z]| {
        let offset = environment::rotate(
            bounds.rotation,
            [x * bounds.extents[0], 0., z * bounds.extents[2]],
        );
        [bounds.center[0] + offset[0], bounds.center[2] + offset[2]]
    });
    let size = layers.size as f32;
    let n = layers.height_resolution;
    let quad = papyrus_terrain::source_stamp_raster::Quad::from_world(
        n,
        [-size * 0.5; 2],
        [1. / size; 2],
        corners,
    )
    .map_err(dungeon::Failure::Invalid)?;
    let [min_x, max_x, min_z, max_z] = quad.bounds();
    work.spend((max_x - min_x) * (max_z - min_z))?;
    let minimum = bounds.bounds().minimum[1] as f32 - 1.;
    for z in min_z..max_z {
        work.spend(0)?;
        for x in min_x..max_x {
            if quad.contains(x, z)
                && papyrus_terrain::erosion::decode(layers.height[z * n + x]) >= minimum
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn local(matrix: [[f32; 4]; 4], rotations: &[[f32; 4]]) -> Result<Transform, String> {
    if rotations.is_empty()
        || rotations.len() > 129
        || matrix[3] != [0., 0., 0., 1.]
        || matrix
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
    {
        return Err("Invalid dungeon hierarchy transform".into());
    }
    for &rotation in rotations {
        Transform {
            position: [0.; 3],
            rotation,
            scale: [1.; 3],
        }
        .validate()?;
    }
    crate::environment_checks::local_transform(matrix, rotations, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dungeon_bindings_roundtrip_with_recipes_and_require_their_stage_source() {
        let mut saved =
            serde_json::json!({"engine":monument_source::VERSION,"source_sha256":"b".repeat(64)});
        let old: monument_source::Recipe = serde_json::from_value(saved.clone()).unwrap();
        old.validate().unwrap();
        assert_eq!(serde_json::to_value(old).unwrap(), saved);
        saved["stage_rules"] = serde_json::json!({"engine":crate::monument_stages::HIERARCHY_VERSION,"source_sha256":"c".repeat(64)});
        saved["dungeon_source"] =
            serde_json::json!({"engine":dungeon::VERSION,"source_sha256":"a".repeat(64)});
        let recipe: monument_source::Recipe = serde_json::from_value(saved.clone()).unwrap();
        recipe.validate().unwrap();
        assert_eq!(serde_json::to_value(recipe).unwrap(), saved);
        for pointer in ["/dungeon_source/engine", "/dungeon_source/source_sha256"] {
            let mut bad = saved.clone();
            *bad.pointer_mut(pointer).unwrap() = "unknown".into();
            assert!(
                serde_json::from_value::<monument_source::Recipe>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        saved.as_object_mut().unwrap().remove("stage_rules");
        assert!(
            serde_json::from_value::<monument_source::Recipe>(saved)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    #[test]
    fn cache_receipts_cannot_select_paths_or_exceed_their_small_input_bound() {
        let temporary = tempfile::tempdir().unwrap();
        let receipt = temporary.path().join("receipt.json");
        std::fs::write(&receipt, serde_json::to_vec(&"a".repeat(64)).unwrap()).unwrap();
        assert_eq!(read_receipt(&receipt).unwrap(), "a".repeat(64));
        for value in ["../elsewhere".into(), "a".repeat(2048), "A".repeat(64)] {
            std::fs::write(&receipt, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(read_receipt(&receipt).is_err());
        }
    }
    #[test]
    fn hierarchy_transforms_keep_the_entrance_offset_and_reject_shear() {
        let mut matrix = [
            [1., 0., 0., 0.],
            [0., 1., 0., 8.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let rotations = [[0., 0., 0., 1.]];
        assert_eq!(local(matrix, &rotations).unwrap().position, [0., 8., 0.]);
        matrix[0][1] = 0.2;
        assert!(local(matrix, &rotations).is_err());
        assert!(local(matrix, &[]).is_err());
    }
}
