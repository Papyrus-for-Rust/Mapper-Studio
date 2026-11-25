//! Source-bound procedural static-decoration metadata; does not instantiate runtime populations.
use crate::{
    Project,
    installation::{self, SourceFile},
    prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use papyrus_terrain::{decor, decor_fit, decor_topology, footprints, monument_composition::Composed};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
pub const COLLIDER_VERSION: &str = papyrus_terrain::decor_collider::VERSION;
pub const HEIGHT_VERSION: &str = papyrus_terrain::decor_height::VERSION;
pub const MATERIAL_VERSION: &str = papyrus_terrain::decor_material::VERSION;
pub const TRANSFORM_VERSION: &str = "papyrus-decor-transforms-7";
pub const ROOT_VERSION: &str = "papyrus-decor-root-transforms-6";
const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub engine: String,
    pub source_sha256: String,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Recipe {
    pub fn new(source_sha256: String) -> Self {
        Self {
            engine: decor::VERSION.into(),
            source_sha256,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if ![
            decor::VERSION,
            decor_fit::VERSION,
            footprints::VERSION,
            footprints::BLOCKER_VERSION,
            decor_topology::VERSION,
            ROOT_VERSION,
            TRANSFORM_VERSION,
            MATERIAL_VERSION,
            HEIGHT_VERSION,
            COLLIDER_VERSION,
        ]
        .contains(&self.engine.as_str())
            || !hash(&self.source_sha256)
        {
            Err("Invalid static decoration recipe".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attribute {
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_to_root: Option<[[f32; 4]; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collider: Option<papyrus_terrain::height_mesh::Mesh>,
    position: [f32; 3],
    fields: serde_json::Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prefab {
    rotation: [f32; 4],
    scale: [f32; 3],
    attributes: Vec<Attribute>,
    components: BTreeMap<String, usize>,
    issues: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage_layout: Option<String>,
    scene: String,
    sources: Vec<SourceFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_sources: Vec<SourceFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    code_sources: Vec<SourceFile>,
    extractor_sha256: String,
    pub stages: Vec<decor::Stage>,
    prefabs: BTreeMap<String, Prefab>,
    issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terrain_policy: Option<TerrainPolicy>,
    #[serde(skip)]
    pub sha256: String,
    #[serde(skip)]
    live_prefabs: OnceLock<Result<LivePrefabs, String>>,
}
struct Retained {
    root: PathBuf,
    catalog: String,
    source: Arc<Source>,
}
static RETAINED: Mutex<Option<Retained>> = Mutex::new(None);

pub(crate) fn clear_session() {
    if let Ok(mut retained) = RETAINED.lock() {
        *retained = None;
    }
}

fn cache_root() -> Result<PathBuf, String> {
    Ok(crate::storage::local_data()?.join("papyrus-decor-cache"))
}
#[derive(Clone)]
pub struct LivePrefabs {
    pub topology_flags: BTreeMap<String, u32>,
    pub fitting: BTreeMap<String, decor_fit::PreparedPrefab>,
    pub terrain: BTreeMap<String, footprints::Prefab>,
    pub known_topology: u32,
    pub protected_topology: u32,
    pub ocean_mask: u32,
    pub placement_role_exceptions: u32,
    pub placement_material_role_exceptions: u32,
}
impl Source {
    pub(crate) fn coastal_prefabs(
        &self,
    ) -> Result<BTreeMap<String, papyrus_terrain::coastal_cliffs::Prefab>, String> {
        let mut prefabs = BTreeMap::new();
        for (path, prefab) in &self.prefabs {
            let mut placement = papyrus_terrain::coastal_cliffs::Prefab::default();
            for kind in [
                "DecorSocketMale",
                "DecorSocketFemale",
                "PlaceCliffParameters",
            ] {
                let attributes: Vec<_> = prefab
                    .attributes
                    .iter()
                    .filter(|a| a.kind == kind)
                    .collect();
                if attributes.len() > 1
                    || prefab.components.get(kind).copied().unwrap_or(0) != attributes.len()
                {
                    return Err(format!(
                        "Ambiguous coastal cliff attribute {kind} for {path}"
                    ));
                }
                let Some(attribute) = attributes.first() else {
                    continue;
                };
                if kind == "PlaceCliffParameters" {
                    placement.parameters = Some(read(attribute)?);
                } else {
                    if attribute
                        .fields
                        .as_object()
                        .is_none_or(|fields| !fields.is_empty())
                    {
                        return Err("Unsupported coastal cliff socket fields".into());
                    }
                    if kind == "DecorSocketMale" {
                        placement.male = Some(attribute.position);
                    } else {
                        placement.female = Some(attribute.position);
                    }
                }
            }
            placement.validate()?;
            prefabs.insert(path.clone(), placement);
        }
        Ok(prefabs)
    }
    pub(crate) fn cliff_priorities(&self) -> Result<BTreeMap<String, i32>, String> {
        let mut priorities = BTreeMap::new();
        for (path, prefab) in &self.prefabs {
            let parameters: Vec<_> = prefab
                .attributes
                .iter()
                .filter(|a| a.kind == "PrefabParameters")
                .collect();
            if parameters.len() > 1 {
                return Err("Ambiguous uniform cliff prefab priority".into());
            }
            let priority = if let Some(attribute) = parameters.first() {
                let fields = attribute
                    .fields
                    .as_object()
                    .ok_or("Invalid cliff prefab parameters")?;
                if fields.len() != 2
                    || fields.get("Count").and_then(serde_json::Value::as_f64) != Some(1.)
                {
                    return Err("Unsupported cliff prefab parameters".into());
                }
                fields
                    .get("Priority")
                    .and_then(serde_json::Value::as_i64)
                    .filter(|v| (0..=4).contains(v))
                    .ok_or("Unsupported cliff prefab priority")? as i32
            } else {
                // PrefabPriority.Default in the reviewed, source-bound programs.
                2
            };
            priorities.insert(path.clone(), priority);
        }
        Ok(priorities)
    }
    pub(crate) fn verify_road_binding(
        &self,
        road: &crate::road_object_source::Source,
    ) -> Result<(), String> {
        if self.stage_layout.as_deref() != road.stage_layout() {
            return Err("Road and decoration scene layouts differ".into());
        }
        road.verify_common_receipts(&self.sources, &self.extra_sources)?;
        road.verify_common_receipts(&self.code_sources, &[])
    }
    pub fn prepare_live(&self, catalog: &Catalog) -> Result<LivePrefabs, String> {
        if self.schema < 4 {
            return Err("Live decoration requires its raw-contributor geometry source".into());
        }
        let policy = self
            .terrain_policy
            .as_ref()
            .ok_or("Live decoration requires bound terrain roles")?;
        policy.validate(catalog)?;
        // Every map owns its terrain edits and optional extra prefabs. Keep
        // the read-only fitting compilation on the shared source.
        self.live_prefabs
            .get_or_init(|| self.compile_live(policy))
            .as_ref()
            .cloned()
            .map_err(Clone::clone)
    }

    fn compile_live(&self, policy: &TerrainPolicy) -> Result<LivePrefabs, String> {
        let ocean_mask = *policy
            .topology_flags
            .get("Ocean")
            .ok_or("Missing Ocean role")?
            | *policy
                .topology_flags
                .get("Oceanside")
                .ok_or("Missing Oceanside role")?;
        let mut fitting = BTreeMap::new();
        let mut terrain = BTreeMap::new();
        for (path, p) in &self.prefabs {
            fitting.insert(
                path.clone(),
                decor_fit::PreparedPrefab::new(compile_extended(p, true))?,
            );
            terrain.insert(path.clone(), live_prefab(p, policy));
        }
        Ok(LivePrefabs {
            topology_flags: policy.topology_flags.clone(),
            fitting,
            terrain,
            known_topology: policy.topology_flags.values().fold(0, |a, b| a | b),
            protected_topology: policy.protected_topology,
            ocean_mask,
            placement_role_exceptions: *policy
                .topology_flags
                .get("Roadside")
                .ok_or("Missing Roadside role")?
                | *policy
                    .topology_flags
                    .get("Ocean")
                    .ok_or("Missing Ocean role")?,
            placement_material_role_exceptions: *policy
                .topology_flags
                .get("Ocean")
                .ok_or("Missing Ocean role")?,
        })
    }
    pub(crate) fn prepare_authored_live(&self, catalog: &Catalog) -> Result<LivePrefabs, String> {
        let mut prepared = self.prepare_live(catalog)?;
        for prefab in prepared.terrain.values_mut() {
            // footprint_prefab inserts exactly one synthetic root before all
            // authored anchors and placement checkpoints. Keep those records.
            if prefab
                .support
                .first()
                .is_none_or(|support| support.position != [0.; 3] || support.radius != 0.)
            {
                return Err("Missing legacy synthetic support provenance".into());
            }
            prefab.support.remove(0);
        }
        Ok(prepared)
    }
    pub(crate) fn preparation_matches(&self, profile: &str, code: &[SourceFile]) -> bool {
        matches!(self.schema, 5 | 6)
            && self.code_sources == code
            && self
                .terrain_policy
                .as_ref()
                .is_some_and(|p| p.profile_sha256 == profile)
    }
    pub fn gameplay_sources(&self) -> &[SourceFile] {
        &self.code_sources
    }
    pub(crate) fn material_removal(&self) -> Option<papyrus_terrain::decor_material::Removal> {
        self.terrain_policy.as_ref().and_then(|p| p.material_removal)
    }
    pub(crate) fn producer_sha256(&self) -> &str {
        &self.extractor_sha256
    }
    pub fn prefab_paths(&self) -> impl Iterator<Item = &str> {
        self.prefabs.keys().map(String::as_str)
    }
    pub fn recipe(&self) -> Recipe {
        Recipe {
            engine: if self.schema >= 4 {
                COLLIDER_VERSION
            } else if self.schema >= 3 {
                HEIGHT_VERSION
            } else if self.schema == 2 {
                decor_fit::VERSION
            } else {
                decor::VERSION
            }
            .into(),
            source_sha256: self.sha256.clone(),
        }
    }
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        Self::load_bound(root, path, catalog, None, cancelled)
    }
    fn load_bound(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        verified: Option<&[SourceFile]>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Static decoration source verification cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::from_bytes_bound(root, &bytes, catalog, verified, cancelled)
    }
    pub(crate) fn from_bytes_bound(
        root: &Path,
        bytes: &[u8],
        catalog: &Catalog,
        verified: Option<&[SourceFile]>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Static decoration source verification cancelled".into());
        }
        if bytes.len() as u64 > LIMIT {
            return Err("Static decoration source exceeds 16 MiB".into());
        }
        let mut doc: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid decoration source: {e}"))?;
        doc.validate(catalog)?;
        doc.verify_installation(root, verified, &cancelled)?;
        doc.sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(doc)
    }
    fn verify_installation(
        &self,
        root: &Path,
        verified: Option<&[SourceFile]>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), String> {
        let maps = installation::hash_file(
            root,
            "Bundles/maps/maps.bundle",
            512 * 1024 * 1024,
            cancelled,
        )?;
        if self.sources.last() != Some(&maps) {
            return Err("Procedural scene bundle differs from the selected installation".into());
        }
        for source in &self.extra_sources {
            if cancelled() {
                return Err("Contributor source verification cancelled".into());
            }
            if verified.is_some_and(|v| v.contains(source)) {
                continue;
            }
            let actual = installation::hash_file(
                root,
                &source.relative_path,
                8 * 1024 * 1024 * 1024,
                cancelled,
            )?;
            if actual != *source {
                return Err("Contributor bundle differs from the selected installation".into());
            }
        }
        for expected in &self.code_sources {
            if installation::hash_file(
                root,
                &expected.relative_path,
                1024 * 1024 * 1024,
                cancelled,
            )? != *expected
            {
                return Err(
                    "Decoration gameplay code differs from the selected installation".into(),
                );
            }
        }
        if cancelled() {
            return Err("Static decoration source verification cancelled".into());
        }
        Ok(())
    }
    fn validate(&self, catalog: &Catalog) -> Result<(), String> {
        if self.format != "papyrus-decor-source"
            || !((self.schema == 1 && self.adapter == "papyrus-static-decor-1")
                || (self.schema == 2 && self.adapter == "papyrus-static-decor-2")
                || (self.schema == 3 && self.adapter == "papyrus-static-decor-3")
                || (self.schema == 4 && self.adapter == "papyrus-static-decor-collider-4")
                || (self.schema == 5 && self.adapter == "papyrus-static-decor-collider-5")
                || (self.schema == 6 && self.adapter == "papyrus-static-decor-collider-6"))
            || match self.schema {
                6 => self.stage_layout.as_deref() != Some("mode-description"),
                _ => self.stage_layout.is_some(),
            }
            || self.scene.is_empty()
            || self.scene.len() > 256
            || self.sources.len() != 4
            || self.sources[..3] != catalog.sources
            || self.sources[3].relative_path != "Bundles/maps/maps.bundle"
            || self.sources[3].bytes == 0
            || self.sources[3].bytes > 512 * 1024 * 1024
            || !hash(&self.sources[3].sha256)
            || !hash(&self.extractor_sha256)
            || self.stages.is_empty()
            || self.stages.len() > 128
            || self.prefabs.len() > 4096
            || self.issues.len() > 4096
            || self.issues.iter().any(|s| s.len() > 1024)
        {
            return Err("Static decoration schema or source receipts mismatch".into());
        }
        let mut code_paths = std::collections::BTreeSet::new();
        if (self.schema >= 5) != !self.code_sources.is_empty()
            || self.code_sources.len() > 2
            || self.code_sources.iter().any(|r| {
                !matches!(
                    r.relative_path.as_str(),
                    "GameAssembly.dll"
                        | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
                        | "RustClient_Data/Managed/Assembly-CSharp.dll"
                        | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
                ) || !code_paths.insert(&r.relative_path)
                    || r.bytes == 0
                    || r.bytes > 1024 * 1024 * 1024
                    || !hash(&r.sha256)
            })
        {
            return Err("Decoration gameplay receipt revision or paths are invalid".into());
        }
        if (self.schema >= 3) != self.terrain_policy.is_some() {
            return Err("Footprint policy revision mismatch".into());
        }
        let mut paths = std::collections::BTreeSet::new();
        if self.extra_sources.len() > 2
            || self.schema < 4 && !self.extra_sources.is_empty()
            || self.extra_sources.iter().any(|s| {
                !matches!(
                    s.relative_path.as_str(),
                    "Bundles/shared/content.bundle" | "Bundles/shared/monuments.bundle"
                ) || s.bytes == 0
                    || s.bytes > 8 * 1024 * 1024 * 1024
                    || !hash(&s.sha256)
                    || !paths.insert(&s.relative_path)
            })
        {
            return Err("Invalid contributor source receipts".into());
        }
        if let Some(policy) = &self.terrain_policy {
            policy.validate(catalog)?;
        }
        let mut ids = std::collections::BTreeSet::new();
        for (i, s) in self.stages.iter().enumerate() {
            s.validate()?;
            if s.order != i
                || !ids.insert(&s.id)
                || s.prefabs.iter().any(|p| !self.prefabs.contains_key(p))
            {
                return Err("Static decoration order or prefab coverage mismatch".into());
            }
        }
        let referenced: std::collections::BTreeSet<_> =
            self.stages.iter().flat_map(|s| s.prefabs.iter()).collect();
        if referenced.len() != self.prefabs.len() {
            return Err("Unreferenced static prefab metadata".into());
        }
        let mut vertices = 0usize;
        let mut triangles = 0usize;
        for p in self.prefabs.values() {
            if p.attributes.iter().any(|a| {
                (self.schema >= 2) != a.root.is_some()
                    || (self.schema >= 3) != a.local_to_root.is_some()
            }) {
                return Err("Decoration attribute revision/root provenance mismatch".into());
            }
            for a in &p.attributes {
                if let Some(mesh) = &a.collider {
                    if self.schema < 4 || a.kind != "AddToHeightMap" {
                        return Err("Contributor geometry revision/component mismatch".into());
                    }
                    mesh.validate()?;
                    vertices += mesh.vertices.len();
                    triangles += mesh.triangles.len();
                    if vertices > 262144 || triangles > 524288 {
                        return Err("Contributor geometry exceeds total budget".into());
                    }
                }
                if let Some(m) = a.local_to_root
                    && (m
                        .iter()
                        .flatten()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                        || m[3] != [0., 0., 0., 1.]
                        || (0..3).any(|i| (m[i][3] - a.position[i]).abs() > 0.001))
                {
                    return Err("Invalid component-to-root transform".into());
                }
            }
            let norm = p.rotation.iter().map(|v| v * v).sum::<f32>();
            if !norm.is_finite()
                || (norm - 1.).abs() > 0.001
                || p.scale
                    .iter()
                    .any(|s| !s.is_finite() || *s <= 0. || *s > 100.)
                || p.attributes.len() > 4096
                || p.components.len() > 256
                || p.components
                    .iter()
                    .any(|(k, v)| k.is_empty() || k.len() > 128 || *v > 65536)
                || p.issues.len() > 4096
                || p.issues.iter().any(|s| s.len() > 1024)
                || p.attributes.iter().any(|a| {
                    a.kind.is_empty()
                        || a.kind.len() > 128
                        || a.position
                            .iter()
                            .any(|v| !v.is_finite() || v.abs() > 10000.)
                        || !a.fields.is_object()
                })
            {
                return Err("Invalid static prefab placement attributes".into());
            }
        }
        Ok(())
    }
    pub fn cache(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        Self::cache_in(root, path, catalog, &cache_root()?, cancelled)
    }
    fn cache_in(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        cache: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let doc = Self::load(root, path, catalog, &cancelled)?;
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let destination = cache.join(&doc.sha256);
        if destination.exists() {
            if Self::load(root, &destination.join("decor.json"), catalog, &cancelled)?.sha256
                != doc.sha256
            {
                return Err("Decoration cache fingerprint changed".into());
            }
        } else {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(&bytes)) != doc.sha256 {
                return Err("Decoration input changed during caching".into());
            }
            let stage = tempfile::Builder::new()
                .prefix(".decor-")
                .tempdir_in(cache)
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            let mut file = std::fs::File::create(stage.path().join("decor.json"))
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            drop(file);
            if cancelled() {
                return Err("Decoration source caching cancelled".into());
            }
            std::fs::rename(stage.path(), destination).map_err(|e| e.to_string())?;
        }
        Ok(doc)
    }
    pub fn discover(
        root: &Path,
        catalog: &Catalog,
        pin: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let cache = cache_root()?;
        if let Some(pin) = pin {
            if !hash(pin) {
                return Err("Invalid decoration source fingerprint".into());
            }
            let source = Self::load(
                root,
                &crate::client_cache::pinned_document(
                    &crate::storage::local_data()?,
                    "papyrus-decor-cache",
                    pin,
                    "decor.json",
                )?,
                catalog,
                &cancelled,
            )?;
            return if source.sha256 == pin {
                Ok(source)
            } else {
                Err("Pinned decoration source changed".into())
            };
        }
        if !cache.is_dir() {
            return Err("No static decoration source prepared for this installation".into());
        }
        let mut best: Option<Self> = None;
        for (i, entry) in std::fs::read_dir(cache)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i >= 256 {
                return Err("Too many static decoration source snapshots".into());
            }
            if cancelled() {
                return Err("Decoration source discovery cancelled".into());
            }
            let directory = entry.map_err(|e| e.to_string())?.path();
            if let Ok(source) = Self::load(root, &directory.join("decor.json"), catalog, &cancelled)
                && directory.file_name().and_then(|s| s.to_str()) == Some(&source.sha256)
                && best
                    .as_ref()
                    .is_none_or(|b| (source.schema, &source.sha256) > (b.schema, &b.sha256))
            {
                best = Some(source);
            }
        }
        best.ok_or_else(|| {
            "No matching static decoration source prepared for this installation".into()
        })
    }
    pub fn cached(
        catalog: &Catalog,
        bindings: &Bindings,
        recipe: &Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        recipe.validate()?;
        let source = Self::load_bound(
            bindings.installation_root(),
            &crate::client_cache::require_pinned_document(
                &crate::storage::local_data()?,
                "papyrus-decor-cache",
                &recipe.source_sha256,
                "decor.json",
                "scenery",
            )?,
            catalog,
            Some(bindings.source_receipts()),
            cancelled,
        )?;
        if source.sha256 != recipe.source_sha256 {
            return Err("Pinned static decoration source changed".into());
        }
        Ok(source)
    }
    pub(crate) fn retain_verified(self, root: &Path, catalog: &Catalog) -> Arc<Self> {
        let source = Arc::new(self);
        if let Ok(mut retained) = RETAINED.lock() {
            *retained = Some(Retained {
                root: root.to_owned(),
                catalog: catalog.sha256.clone(),
                source: source.clone(),
            });
        }
        source
    }

    pub fn cached_shared(
        catalog: &Catalog,
        bindings: &Bindings,
        recipe: &Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Arc<Self>, String> {
        if cancelled() {
            return Err("Static decoration source verification cancelled".into());
        }
        let _verification = installation::VerificationScope::default();
        let root = bindings.installation_root();
        let retained = RETAINED.lock().ok().and_then(|entry| {
            entry.as_ref().filter(|entry| {
                entry.root == root
                    && entry.catalog == catalog.sha256
                    && entry.source.sha256 == recipe.source_sha256
                    && entry.source.sources.starts_with(&catalog.sources)
            }).map(|entry| entry.source.clone())
        });
        let Some(source) = retained else {
            return Self::cached(catalog, bindings, recipe, cancelled)
                .map(|source| source.retain_verified(root, catalog));
        };
        recipe.validate()?;
        let path = crate::client_cache::require_pinned_document(
            &crate::storage::local_data()?,
            "papyrus-decor-cache",
            &recipe.source_sha256,
            "decor.json",
            "scenery",
        )?;
        let directory = path.parent().ok_or("Missing scenery capture directory")?;
        if installation::hash_file(directory, "decor.json", LIMIT, &cancelled)?.sha256 != source.sha256 {
            return Err("Pinned static decoration source changed".into());
        }
        source.verify_installation(root, Some(bindings.source_receipts()), &cancelled)?;
        Ok(source)
    }
    pub fn generate(
        &self,
        project: &Project,
        maps: &Composed,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<decor::Report>, String> {
        if maps.layers.size != project.size
            || maps.layers.terrain_seed != project.seeds[0].value
            || maps.layers.biome_seed != project.seeds[1].value
            || project.decoration.as_ref().is_none_or(|r| {
                r.source_sha256 != self.sha256
                    || ![
                        decor::VERSION,
                        decor_fit::VERSION,
                        footprints::VERSION,
                        footprints::BLOCKER_VERSION,
                        decor_topology::VERSION,
                        ROOT_VERSION,
                        TRANSFORM_VERSION,
                        MATERIAL_VERSION,
                        HEIGHT_VERSION,
                        COLLIDER_VERSION,
                    ]
                    .contains(&r.engine.as_str())
            })
        {
            return Err("Static decoration terrain or recipe/source mismatch".into());
        }
        if self.schema < 4
            && project
                .decoration
                .as_ref()
                .is_some_and(|r| r.engine == COLLIDER_VERSION)
        {
            return Err("Collider placement requires its pinned revision-4 geometry source".into());
        }
        let fitting = project.decoration.as_ref().is_some_and(|r| {
            [
                decor_fit::VERSION,
                footprints::VERSION,
                footprints::BLOCKER_VERSION,
                decor_topology::VERSION,
                ROOT_VERSION,
                TRANSFORM_VERSION,
                MATERIAL_VERSION,
                HEIGHT_VERSION,
                COLLIDER_VERSION,
            ]
            .contains(&r.engine.as_str())
        });
        if fitting && self.schema < 2 {
            return Err("Authored fitting requires revision-2 decoration attributes; re-extract and explicitly rebind the source".into());
        }
        let Some(mut report) = decor::generate(
            &maps.layers,
            &maps.alpha,
            &self.stages,
            project.seeds[5].value,
            &self.sha256,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        report.source_issues = self.issues.clone();
        for (path, p) in &self.prefabs {
            report
                .source_issues
                .extend(p.issues.iter().map(|i| format!("{path}: {i}")));
        }
        if fitting {
            let extended = project.decoration.as_ref().is_some_and(|r| {
                [
                    TRANSFORM_VERSION,
                    MATERIAL_VERSION,
                    HEIGHT_VERSION,
                    COLLIDER_VERSION,
                ]
                .contains(&r.engine.as_str())
            });
            let water = if extended {
                self.terrain_policy
                    .as_ref()
                    .zip(maps.ocean.as_ref())
                    .map(|(policy, ocean)| {
                        let mask = policy
                            .topology_flags
                            .get("Ocean")
                            .copied()
                            .ok_or("Missing Ocean topology role for water query")?
                            | policy
                                .topology_flags
                                .get("Oceanside")
                                .copied()
                                .ok_or("Missing Oceanside topology role for water query")?;
                        Ok::<_, String>(decor_fit::Water {
                            local_heights: &ocean.local_water,
                            resolution: maps.layers.height_resolution,
                            ocean_mask: mask,
                            ocean_level: 0.,
                            terrain_minimum: -500.,
                        })
                    })
                    .transpose()?
            } else {
                None
            };
            let prefabs = self
                .prefabs
                .iter()
                .map(|(path, p)| {
                    let mut compiled = if project.decoration.as_ref().is_some_and(|r| {
                        [
                            ROOT_VERSION,
                            TRANSFORM_VERSION,
                            MATERIAL_VERSION,
                            HEIGHT_VERSION,
                            COLLIDER_VERSION,
                        ]
                        .contains(&r.engine.as_str())
                    }) {
                        compile_extended(p, extended)
                    } else {
                        compile(p)
                    };
                    if extended
                        && water.is_none()
                        && compiled
                            .transforms
                            .iter()
                            .any(|t| matches!(t, decor_fit::Transform::Swim))
                    {
                        compiled
                            .unsupported
                            .push("DecorSwim requires a retained water raster".into());
                    }
                    (path.clone(), compiled)
                })
                .collect();
            let fitted = if extended {
                decor_fit::generate_extended(
                    &maps.layers,
                    &maps.alpha,
                    &self.stages,
                    &report.candidates,
                    &prefabs,
                    decor_fit::Settings {
                        seed: project.seeds[5].value,
                        water,
                    },
                    &cancelled,
                )
            } else {
                decor_fit::generate(
                    &maps.layers,
                    &maps.alpha,
                    &self.stages,
                    &report.candidates,
                    &prefabs,
                    project.seeds[5].value,
                    &cancelled,
                )
            };
            let Some(fit) = fitted? else {
                return Ok(None);
            };
            report.pending_checks = vec!["Geometric fitting is provisional; placement occupancy, environment/water checks and collisions remain unvalidated".into(),"Terrain footprints/effects and complete prefab assembly remain pending".into()];
            report.fitting = Some(fit);
        }
        Ok((!cancelled()).then_some(report))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerrainPolicy {
    profile_id: String,
    profile_sha256: String,
    native_sha256: String,
    scene_sha256: String,
    topology_flags: BTreeMap<String, u32>,
    protected_topology: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    material_removal: Option<papyrus_terrain::decor_material::Removal>,
}
trait TerrainMasks {
    fn flags(&self) -> &BTreeMap<String, u32>;
    fn protected(&self) -> u32;
    fn material_removal(&self) -> Option<papyrus_terrain::decor_material::Removal> {
        None
    }
}
impl TerrainMasks for TerrainPolicy {
    fn flags(&self) -> &BTreeMap<String, u32> {
        &self.topology_flags
    }
    fn protected(&self) -> u32 {
        self.protected_topology
    }
    fn material_removal(&self) -> Option<papyrus_terrain::decor_material::Removal> {
        self.material_removal
    }
}
struct RoadMasks<'a> {
    flags: &'a BTreeMap<String, u32>,
    protected: u32,
}
impl TerrainMasks for RoadMasks<'_> {
    fn flags(&self) -> &BTreeMap<String, u32> {
        self.flags
    }
    fn protected(&self) -> u32 {
        self.protected
    }
}
impl TerrainPolicy {
    fn validate(&self, catalog: &Catalog) -> Result<(), String> {
        if let Some(removal) = self.material_removal {
            removal.validate()?;
            if !["Building", "Mountain"].iter().any(|role| {
                self.topology_flags.get(*role) == Some(&removal.excluded_topology)
            }) {
                return Err("Material removal differs from the source exclusion role".into());
            }
        }
        let names = [
            "Monument", "Building", "Road", "Roadside", "Rail", "Railside", "Ocean", "River",
            "Lake",
        ];
        let mut expected = 0;
        for name in names {
            expected |= *self
                .topology_flags
                .get(name)
                .ok_or("Missing protected topology role")?;
        }
        if self.profile_id.is_empty()
            || self.profile_id.len() > 128
            || !hash(&self.profile_sha256)
            || self.native_sha256 != catalog.installation.native.sha256
            || catalog
                .sources
                .iter()
                .find(|s| s.relative_path == "Bundles/shared/assetscenes.bundle")
                .is_none_or(|s| s.sha256 != self.scene_sha256)
            || self.topology_flags.len() > 32
            || self
                .topology_flags
                .iter()
                .any(|(k, v)| k.is_empty() || k.len() > 64 || v.count_ones() != 1)
            || self
                .topology_flags
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.topology_flags.len()
            || self.protected_topology != expected
        {
            return Err("Footprint topology policy/source mismatch".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct FootprintFields {
    ring: Vec<Vector>,
    center: Vector,
    run_starts: Vec<usize>,
    run_closed: Vec<u8>,
    fill_offset: f32,
    max_fill: f32,
    reject_above_gap: f32,
    rim_width: f32,
    feather: f32,
    opacity: f32,
    fill_interior: Switch,
    max_tilt_degrees: f32,
}
fn footprint_prefab(p: &Prefab) -> footprints::Prefab {
    let mut out = footprints::Prefab {
        blockers: vec![],
        raises: vec![],
        topology: None,
        materials: None,
        height_modifiers: None,
        contributors: None,
        support: vec![footprints::Support {
            position: [0.; 3],
            radius: 0.,
        }],
        footprint: None,
        unsupported: vec![],
        pending: vec!["Physics, environment/water checks and prefab assembly".into()],
    };
    for a in &p.attributes {
        match a.kind.as_str() {
            "TerrainAnchor" => match read::<AnchorFields>(a) {
                Ok(f) => out.support.push(footprints::Support {
                    position: a.position,
                    radius: f.radius,
                }),
                Err(e) => out.unsupported.push(e),
            },
            "TerrainFootprint" => {
                let parse = (|| -> Result<footprints::Footprint, String> {
                    let f: FootprintFields = read(a)?;
                    if f.run_starts != [0] || f.run_closed != [1] {
                        return Err("Multiple/open footprint runs are unsupported".into());
                    }
                    let matrix = a
                        .local_to_root
                        .ok_or("Missing component-to-root transform")?;
                    Ok(footprints::Footprint {
                        ring: f.ring.into_iter().map(Vector::array).collect(),
                        center: f.center.array(),
                        local_to_root: matrix,
                        fill_offset: f.fill_offset,
                        max_fill: f.max_fill,
                        reject_above_gap: f.reject_above_gap,
                        rim_width: f.rim_width,
                        feather: f.feather,
                        opacity: f.opacity,
                        fill_interior: f.fill_interior.enabled()?,
                        max_tilt_degrees: f.max_tilt_degrees,
                    })
                })();
                match parse {
                    Ok(f) => {
                        if out.footprint.replace(f).is_some() {
                            out.unsupported.push("Multiple footprint components".into());
                        }
                    }
                    Err(e) => out.unsupported.push(e),
                }
            }
            k if k.starts_with("TerrainHeight")
                || k.starts_with("TerrainSplat")
                || k == "AddToHeightMap" =>
            {
                out.unsupported.push(format!(
                    "Additional terrain effect requires an adapter: {k}"
                ))
            }
            k if k.starts_with("TerrainTopology") || k == "TerrainPlacementBlocked" => {
                out.pending.push(k.into())
            }
            _ => {}
        }
    }
    if out.footprint.is_none() {
        out.unsupported
            .push("No supported authored footprint".into());
    }
    out.pending.sort();
    out.pending.dedup();
    out
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct BlockerFields {
    opacity: f32,
    radius: f32,
    fade: f32,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct RaiseFields {
    opacity: f32,
    radius: f32,
    fade: f32,
    height_mode: u8,
}
fn blocker_prefab(p: &Prefab) -> footprints::Prefab {
    let mut out = footprint_prefab(p);
    out.pending.retain(|s| s != "TerrainPlacementBlocked");
    for a in &p.attributes {
        let result = (|| -> Result<(), String> {
            if a.kind == "TerrainPlacementBlocked" {
                let f: BlockerFields = read(a)?;
                if !f.opacity.is_finite()
                    || !(0. ..=1.).contains(&f.opacity)
                    || !f.radius.is_finite()
                    || !(0. ..=128.).contains(&f.radius)
                    || !f.fade.is_finite()
                    || !(0. ..=128.).contains(&f.fade)
                {
                    return Err("Invalid TerrainPlacementBlocked parameters".into());
                }
                out.blockers.push(footprints::Blocker {
                    position: a.position,
                    radius: f.radius,
                    fade: f.fade,
                });
            }
            if a.kind == "TerrainFilter" {
                let f: FilterFields = read(a)?;
                if f.check_placement_map.enabled()? {
                    out.support.push(footprints::Support {
                        position: a.position,
                        radius: 0.,
                    });
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            out.unsupported.push(e);
        }
    }
    if !out.blockers.is_empty() {
        out.unsupported
            .retain(|s| s != "No supported authored footprint");
        let mut supported = true;
        for a in p.attributes.iter().filter(|a| a.kind == "TerrainHeightSet") {
            match read::<RaiseFields>(a) {
                Ok(f)
                    if f.height_mode == 1
                        && f.opacity.is_finite()
                        && (0. ..=1.).contains(&f.opacity)
                        && f.radius.is_finite()
                        && (0. ..=128.).contains(&f.radius)
                        && f.fade.is_finite()
                        && (0. ..=128.).contains(&f.fade) =>
                {
                    out.raises.push(footprints::Raise {
                        position: a.position,
                        radius: f.radius,
                        fade: f.fade,
                        opacity: f.opacity,
                    })
                }
                _ => {
                    supported = false;
                    out.unsupported
                        .push("Unsupported TerrainHeightSet mode or fields".into());
                }
            }
        }
        if supported {
            out.unsupported
                .retain(|s| s != "Additional terrain effect requires an adapter: TerrainHeightSet");
        }
    }
    out
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct TopologyFields {
    opacity: f32,
    radius: f32,
    fade: f32,
    topology_type: i32,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct MaterialFields {
    opacity: f32,
    radius: f32,
    fade: f32,
    splat_type: u32,
}
fn material_prefab(p: &Prefab, policy: &impl TerrainMasks) -> footprints::Prefab {
    let mut out = topology_prefab(p, policy);
    for a in &p.attributes {
        if a.kind.starts_with("Terrain")
            && !matches!(
                a.kind.as_str(),
                "TerrainAnchor"
                    | "TerrainFilter"
                    | "TerrainFootprint"
                    | "TerrainPlacementBlocked"
                    | "TerrainHeightSet"
                    | "TerrainSplatSet"
                    | "TerrainSplatRemove"
                    | "TerrainTopologyAdd"
                    | "TerrainTopologySet"
            )
        {
            out.unsupported
                .push(format!("Unknown required terrain component: {}", a.kind));
        }
    }
    let mut modifiers = Vec::new();
    for a in p
        .attributes
        .iter()
        .filter(|a| matches!(a.kind.as_str(), "TerrainSplatSet" | "TerrainSplatRemove"))
    {
        let result = (|| -> Result<_, String> {
            let f: MaterialFields = read(a)?;
            // The bound native ABI has eight splat channels. Unknown/multi-bit
            // source enum values are not guessed into a channel.
            if !f.splat_type.is_power_of_two() || f.splat_type > 128 {
                return Err("Unknown source splat channel".into());
            }
            let modifier = papyrus_terrain::decor_material::Modifier {
                operation: if a.kind == "TerrainSplatRemove" {
                    papyrus_terrain::decor_material::Operation::Remove(
                        policy
                            .material_removal()
                            .ok_or("Missing installation-bound material removal policy")?,
                    )
                } else {
                    papyrus_terrain::decor_material::Operation::Set
                },
                position: a.position,
                radius: f.radius,
                fade: f.fade,
                opacity: f.opacity,
                channel: f.splat_type.trailing_zeros() as u8,
            };
            papyrus_terrain::decor_material::validate(std::slice::from_ref(&modifier))?;
            Ok(modifier)
        })();
        match result {
            Ok(m) => modifiers.push(m),
            Err(e) => out.unsupported.push(e),
        }
    }
    out.unsupported.retain(|s| {
        !matches!(
            s.as_str(),
            "Additional terrain effect requires an adapter: TerrainSplatSet"
                | "Additional terrain effect requires an adapter: TerrainSplatRemove"
        )
    });
    if !modifiers.is_empty() {
        out.unsupported
            .retain(|s| s != "No supported authored footprint");
    }
    out.materials = Some(modifiers);
    out
}
fn height_prefab(p: &Prefab, policy: &impl TerrainMasks) -> footprints::Prefab {
    let mut out = material_prefab(p, policy);
    let mut modifiers = Vec::new();
    for a in p.attributes.iter().filter(|a| a.kind == "TerrainHeightSet") {
        let result = (|| -> Result<_, String> {
            let f: RaiseFields = read(a)?;
            let mode = match f.height_mode {
                0 => papyrus_terrain::decor_height::Mode::Set,
                1 => papyrus_terrain::decor_height::Mode::Raise,
                2 => papyrus_terrain::decor_height::Mode::Lower,
                _ => return Err("Unknown source height mode".into()),
            };
            let modifier = papyrus_terrain::decor_height::Modifier {
                position: a.position,
                radius: f.radius,
                fade: f.fade,
                opacity: f.opacity,
                mode,
            };
            papyrus_terrain::decor_height::validate(std::slice::from_ref(&modifier))?;
            Ok(modifier)
        })();
        match result {
            Ok(m) => modifiers.push(m),
            Err(e) => out.unsupported.push(e),
        }
    }
    out.raises.clear();
    out.unsupported.retain(|s| {
        s != "Additional terrain effect requires an adapter: TerrainHeightSet"
            && s != "Unsupported TerrainHeightSet mode or fields"
    });
    if !modifiers.is_empty() {
        out.unsupported
            .retain(|s| s != "No supported authored footprint");
    }
    out.unsupported.sort();
    out.unsupported.dedup();
    out.height_modifiers = Some(modifiers);
    out
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContributorFields {
    #[serde(rename = "DestroyGameObject")]
    _destroy_game_object: Switch,
}
fn collider_prefab(p: &Prefab, policy: &impl TerrainMasks) -> footprints::Prefab {
    let mut out = height_prefab(p, policy);
    let mut contributors = Vec::new();
    for a in p.attributes.iter().filter(|a| a.kind == "AddToHeightMap") {
        let result = (|| -> Result<_, String> {
            let _: ContributorFields = read(a)?;
            let c = papyrus_terrain::decor_collider::Contributor {
                mesh: a
                    .collider
                    .clone()
                    .ok_or("Missing referenced height contributor mesh")?,
                local_to_root: a
                    .local_to_root
                    .ok_or("Missing height contributor transform")?,
            };
            papyrus_terrain::decor_collider::validate(std::slice::from_ref(&c))?;
            Ok(c)
        })();
        match result {
            Ok(c) => contributors.push(c),
            Err(e) => out.unsupported.push(e),
        }
    }
    out.unsupported
        .retain(|s| s != "Additional terrain effect requires an adapter: AddToHeightMap");
    if !contributors.is_empty() {
        out.unsupported
            .retain(|s| s != "No supported authored footprint");
    }
    out.contributors = Some(contributors);
    out.unsupported.sort();
    out.unsupported.dedup();
    out
}
fn live_prefab(p: &Prefab, policy: &impl TerrainMasks) -> footprints::Prefab {
    let mut out = collider_prefab(p, policy);
    // The live object transaction can reserve authored supports and apply
    // topology-only effects without inventing a terrain polygon. Other required
    // component/geometry failures remain explicit rejections.
    out.unsupported
        .retain(|e| e != "No supported authored footprint");
    out
}
fn topology_prefab(p: &Prefab, policy: &impl TerrainMasks) -> footprints::Prefab {
    let mut out = blocker_prefab(p);
    let mut topology = decor_topology::Prefab::default();
    let known = policy.flags().values().fold(0, |a, b| a | b);
    for a in &p.attributes {
        let result = (|| -> Result<(), String> {
            match a.kind.as_str() {
                "TerrainTopologyAdd" | "TerrainTopologySet" => {
                    let f: TopologyFields = read(a)?;
                    if !f.opacity.is_finite() || !(0. ..=1.).contains(&f.opacity) {
                        return Err("Invalid topology opacity".into());
                    }
                    topology.modifiers.push(decor_topology::Modifier {
                        position: a.position,
                        radius: f.radius,
                        fade: f.fade,
                        mask: f.topology_type as u32,
                        operation: if a.kind == "TerrainTopologyAdd" {
                            decor_topology::Operation::Add
                        } else {
                            decor_topology::Operation::Set
                        },
                    });
                }
                "TerrainFilter" => {
                    let f: FilterFields = read(a)?;
                    topology.queries.push(decor_topology::Query {
                        check_placement: f.check_placement_map.enabled()?,
                        position: a.position,
                        radius: f.check_topology_radius,
                        filter: f.filter,
                    });
                }
                "TerrainAnchor" => topology.anchors.push(a.position),
                k if k.starts_with("TerrainTopology") => {
                    return Err(format!("Unsupported topology modifier: {k}"));
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = result {
            out.unsupported.push(e);
        }
    }
    if let Err(e) = decor_topology::validate(&topology, known, policy.protected()) {
        out.unsupported.push(e);
        topology = decor_topology::Prefab::default();
    }
    out.pending
        .retain(|s| s != "TerrainTopologyAdd" && s != "TerrainTopologySet");
    out.topology = Some(topology);
    out
}
impl Source {
    pub fn place(
        &self,
        project: &Project,
        solution: &papyrus_terrain::placement::Solution,
        catalog: &Catalog,
        maps: &mut Composed,
        report: &mut decor::Report,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        self.place_with_height_reservations(
            project, solution, catalog, maps, report, None, cancelled,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn place_with_height_reservations(
        &self,
        project: &Project,
        solution: &papyrus_terrain::placement::Solution,
        catalog: &Catalog,
        maps: &mut Composed,
        report: &mut decor::Report,
        height_reservations: Option<&papyrus_terrain::height_reservations::HeightReservations>,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if self.schema < 3
            || project.decoration.as_ref().is_none_or(|r| {
                ![
                    footprints::VERSION,
                    footprints::BLOCKER_VERSION,
                    decor_topology::VERSION,
                    ROOT_VERSION,
                    TRANSFORM_VERSION,
                    MATERIAL_VERSION,
                    HEIGHT_VERSION,
                    COLLIDER_VERSION,
                ]
                .contains(&r.engine.as_str())
                    || r.source_sha256 != self.sha256
            })
        {
            return Err("Footprint placement requires its pinned revision-3 source".into());
        }
        if report.source_sha256 != self.sha256
            || report.seed != project.seeds[5].value
            || maps.layers.size != project.size
        {
            return Err("Footprint report/terrain mismatch".into());
        }
        let policy = self
            .terrain_policy
            .as_ref()
            .ok_or("Missing footprint policy")?;
        policy.validate(catalog)?;
        let fitted = report
            .fitting
            .as_ref()
            .ok_or("Footprints require geometric fitting")?;
        let collider = project
            .decoration
            .as_ref()
            .is_some_and(|r| r.engine == COLLIDER_VERSION);
        let height = project
            .decoration
            .as_ref()
            .is_some_and(|r| [HEIGHT_VERSION, COLLIDER_VERSION].contains(&r.engine.as_str()));
        let material = project.decoration.as_ref().is_some_and(|r| {
            [MATERIAL_VERSION, HEIGHT_VERSION, COLLIDER_VERSION].contains(&r.engine.as_str())
        });
        let topology = project.decoration.as_ref().is_some_and(|r| {
            [
                decor_topology::VERSION,
                ROOT_VERSION,
                TRANSFORM_VERSION,
                MATERIAL_VERSION,
                HEIGHT_VERSION,
                COLLIDER_VERSION,
            ]
            .contains(&r.engine.as_str())
        });
        let blocking = project.decoration.as_ref().is_some_and(|r| {
            [
                footprints::BLOCKER_VERSION,
                decor_topology::VERSION,
                ROOT_VERSION,
                TRANSFORM_VERSION,
                MATERIAL_VERSION,
                HEIGHT_VERSION,
                COLLIDER_VERSION,
            ]
            .contains(&r.engine.as_str())
        });
        let prefabs = self
            .prefabs
            .iter()
            .map(|(path, p)| {
                (
                    path.clone(),
                    if collider {
                        collider_prefab(p, policy)
                    } else if height {
                        height_prefab(p, policy)
                    } else if material {
                        material_prefab(p, policy)
                    } else if topology {
                        topology_prefab(p, policy)
                    } else if blocking {
                        blocker_prefab(p)
                    } else {
                        footprint_prefab(p)
                    },
                )
            })
            .collect();
        let mut obstacles = Vec::new();
        for outcome in &solution.outcomes {
            let Some(p) = &outcome.position else {
                continue;
            };
            let target = project
                .targets
                .iter()
                .find(|t| t.instance_id == outcome.id)
                .ok_or("Unknown footprint-protection target")?;
            let bounds = target
                .prefab_path
                .as_ref()
                .and_then(|path| catalog.entries.get(path))
                .and_then(|a| a.bounds.as_ref())
                .ok_or("Missing authored protection bounds")?;
            obstacles.push(footprints::Obstacle {
                center: [p.x, p.z],
                extent: bounds.horizontal_extent(),
                yaw: p.yaw,
            });
        }
        let input = footprints::Input {
            height_reservations,
            layers: &maps.layers,
            alpha: &maps.alpha,
            candidates: &report.candidates,
            stages: &self.stages,
            poses: &fitted.poses,
            prefabs: &prefabs,
            obstacles: &obstacles,
            protected_topology: policy.protected_topology,
        };
        let output = if collider {
            footprints::apply_colliders(
                input,
                policy.topology_flags.values().fold(0, |a, b| a | b),
                &cancelled,
            )
        } else if height {
            footprints::apply_height(
                input,
                policy.topology_flags.values().fold(0, |a, b| a | b),
                &cancelled,
            )
        } else if material {
            footprints::apply_materials(
                input,
                policy.topology_flags.values().fold(0, |a, b| a | b),
                &cancelled,
            )
        } else if topology {
            footprints::apply_topology(
                input,
                policy.topology_flags.values().fold(0, |a, b| a | b),
                &cancelled,
            )
        } else if blocking {
            footprints::apply_blockers(input, &cancelled)
        } else {
            footprints::apply(input, &cancelled)
        }?;
        let Some(output) = output else {
            return Ok(false);
        };
        if cancelled() {
            return Ok(false);
        }
        maps.layers.height = output.height;
        if let Some(topology) = output.topology {
            maps.layers.topology = topology;
        }
        if let Some(splat) = output.splat {
            maps.layers.splat = splat;
        }
        report.terrain_placement = Some(output.report);
        report.pending_checks=vec![if collider {"Applied raw collider surface raises with height, material, topology and blocker effects transactionally; PhysX cooking, complete prefab assembly and game validation remain pending"} else if height {"Applied ordered source height modes with material, topology and blocker effects transactionally; collider contributions and game validation remain pending"} else if material {"Applied supported height, blocker, material and topology effects transactionally with retained-filter dependencies; other effects remain pending"} else if topology {"Applied supported terrain fills, blockers and topology modifiers with dependency checks; other effects remain pending"} else if blocking { "Applied supported terrain fills, blockers and conservative reservations; topology/other effects remain pending" } else { "Applied footprint fill and conservative reservations; topology/other effects remain pending" }.into(),"Physics, environment/water checks and complete prefab assembly remain unvalidated".into()];
        Ok(true)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    x: f32,
    y: f32,
    z: f32,
}
impl Vector {
    fn array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct RotationFields {
    min_rotation: Vector,
    max_rotation: Vector,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct FlipFields {
    flip_axis: u8,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct ScaleFields {
    min_scale: Vector,
    max_scale: Vector,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct OffsetFields {
    min_offset: Vector,
    max_offset: Vector,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct AlignFields {
    normal_alignment: f32,
    gradient_alignment: f32,
    slope_offset: Vector,
    slope_scale: Vector,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct AnchorFields {
    extents: f32,
    offset: f32,
    radius: f32,
    slope_scale: f32,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Switch {
    Bool(bool),
    Int(u8),
}
impl Switch {
    fn enabled(self) -> Result<bool, String> {
        match self {
            Self::Bool(v) => Ok(v),
            Self::Int(0) => Ok(false),
            Self::Int(1) => Ok(true),
            _ => Err("Invalid serialized switch".into()),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct FilterFields {
    filter: papyrus_terrain::spawn::Filter,
    check_placement_map: Switch,
    check_terrain_bounds: Switch,
    check_topology_radius: f32,
}
fn read<T: serde::de::DeserializeOwned>(a: &Attribute) -> Result<T, String> {
    serde_json::from_value(a.fields.clone()).map_err(|e| format!("{}: {e}", a.kind))
}
fn compile(p: &Prefab) -> decor_fit::Prefab {
    compile_scoped(p, false)
}
fn compile_scoped(p: &Prefab, root_only: bool) -> decor_fit::Prefab {
    compile_impl(p, root_only, CompileMode::Legacy)
}
fn compile_extended(p: &Prefab, extended: bool) -> decor_fit::Prefab {
    compile_impl(
        p,
        true,
        if extended {
            CompileMode::Extended
        } else {
            CompileMode::Legacy
        },
    )
}

fn read_path_prefab(bytes: &[u8]) -> Result<(Prefab, Vec<serde_json::Value>), String> {
    if bytes.len() > LIMIT as usize {
        return Err("Path decoration fitting metadata exceeds 16 MiB".into());
    }
    let mut fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let holes = match fields.remove("hole_shapes") {
        Some(serde_json::Value::Array(holes)) if holes.len() <= 256 => holes,
        None => Vec::new(),
        _ => return Err("Invalid path decoration hole-shape roster".into()),
    };
    let prefab: Prefab = serde_json::from_value(serde_json::Value::Object(fields))
        .map_err(|error| error.to_string())?;
    if prefab.attributes.len() > 4096
        || prefab.components.len() > 256
        || prefab.issues.len() > 4096
    {
        return Err("Path decoration fitting metadata exceeds its record bounds".into());
    }
    let hole_attributes = prefab
        .attributes
        .iter()
        .filter(|attribute| attribute.kind == "TerrainHoleShape")
        .count();
    if holes.len()
        != prefab
            .components
            .get("TerrainHoleShape")
            .copied()
            .unwrap_or(0)
        || holes.len() != hole_attributes
    {
        return Err("Path decoration hole shapes differ from the component roster".into());
    }
    Ok((prefab, holes))
}
fn path_fitting(prefab: &Prefab) -> Result<decor_fit::PreparedPrefab, String> {
    let fitting = compile_impl(prefab, true, CompileMode::PathAttached);
    if !fitting.unsupported.is_empty() {
        return Err(format!(
            "Unsupported path decoration fitting: {}",
            fitting.unsupported.join(", ")
        ));
    }
    decor_fit::PreparedPrefab::new(fitting)
}
/// Prepare captured roadside decoration from its prefab basis and point filters.
/// Terrain effects and referenced hole shapes need their separate admission pass.
pub fn prepare_path_fitting(bytes: &[u8]) -> Result<decor_fit::PreparedPrefab, String> {
    let (prefab, _) = read_path_prefab(bytes)?;
    path_fitting(&prefab)
}

/// Full numeric preparation for roadside prefab fitting and terrain admission.
/// Client-rendered holes remain associated with the native prefab instance.
pub struct PathPrefab {
    pub fitting: decor_fit::PreparedPrefab,
    pub terrain: footprints::Prefab,
    pub runtime_holes: Vec<RuntimeHole>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHole {
    pub object_id: i64,
    pub local_to_root: [[f32; 4]; 4],
    pub asset: RuntimeHoleAsset,
    pub radius: f32,
    pub enabled: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHoleAsset {
    pub name: String,
    pub id: u32,
    pub start_instance_count: u32,
    pub asset_file: String,
    pub object_id: i64,
    pub mesh_file: String,
    pub mesh_object_id: i64,
    pub mesh: RuntimeHoleMesh,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHoleMesh {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HoleFields {
    radius: f32,
}
impl RuntimeHole {
    fn validate(&self) -> Result<(), String> {
        let text =
            |value: &str| !value.is_empty() && value.len() <= 256 && !value.contains('\0');
        let matrix = &self.local_to_root;
        let determinant = matrix[0][0]
            * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
            - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
            + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
        if self.object_id == 0
            || self.asset.object_id == 0
            || self.asset.mesh_object_id == 0
            || !text(&self.asset.name)
            || !text(&self.asset.asset_file)
            || !text(&self.asset.mesh_file)
            || self.asset.id > i32::MAX as u32
            || self.asset.start_instance_count > 1_048_576
            || !self.radius.is_finite()
            || !(0. ..=10000.).contains(&self.radius)
            || matrix
                .as_flattened()
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 10000.)
            || matrix[3] != [0., 0., 0., 1.]
            || !determinant.is_finite()
            || determinant.abs() <= 1e-9
            || !(3..=65536).contains(&self.asset.mesh.vertices.len())
            || !(1..=131072).contains(&self.asset.mesh.triangles.len())
            || self
                .asset
                .mesh
                .vertices
                .iter()
                .flatten()
                .any(|value| !value.is_finite() || value.abs() > 10000.)
            || self.asset.mesh.triangles.iter().any(|triangle| {
                triangle
                    .iter()
                    .any(|&index| index as usize >= self.asset.mesh.vertices.len())
            })
        {
            return Err("Invalid referenced runtime terrain-hole shape".into());
        }
        Ok(())
    }
}

/// The installation-bound adapter owns component execution policy. This compiles
/// numeric terrain effects and retains TerrainHoleShape as client prefab data.
pub fn prepare_path_prefab(
    bytes: &[u8],
    flags: &BTreeMap<String, u32>,
    protected: u32,
) -> Result<PathPrefab, String> {
    let known = flags.values().fold(0, |mask, flag| mask | flag);
    if flags.is_empty()
        || flags.len() > 32
        || protected & !known != 0
        || flags
            .iter()
            .any(|(name, flag)| name.is_empty() || name.len() > 64 || !flag.is_power_of_two())
        || flags
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != flags.len()
    {
        return Err("Invalid path prefab topology roles".into());
    }
    let (prefab, holes) = read_path_prefab(bytes)?;
    let fitting = path_fitting(&prefab)?;
    let runtime_holes: Vec<RuntimeHole> = holes
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())?;
    let mut ids = std::collections::BTreeSet::new();
    let mut matched = vec![false; runtime_holes.len()];
    for hole in &runtime_holes {
        hole.validate()?;
        if !ids.insert(hole.object_id) {
            return Err("Duplicate runtime terrain-hole component".into());
        }
    }
    for attribute in prefab
        .attributes
        .iter()
        .filter(|attribute| attribute.kind == "TerrainHoleShape")
    {
        let fields: HoleFields = read(attribute)?;
        let index = runtime_holes
            .iter()
            .enumerate()
            .position(|(index, hole)| {
                !matched[index]
                    && Some(hole.local_to_root) == attribute.local_to_root
                    && hole.radius == fields.radius
            })
            .ok_or("Runtime terrain-hole reference differs from its placement component")?;
        matched[index] = true;
    }
    let mut terrain = live_prefab(&prefab, &RoadMasks { flags, protected });
    if !runtime_holes.is_empty() {
        terrain
            .unsupported
            .retain(|issue| issue != "Unknown required terrain component: TerrainHoleShape");
        terrain
            .pending
            .push("TerrainHoleShape retained by the client prefab runtime".into());
    }
    if let Some(footprint) = &terrain.footprint {
        footprint.validate()?;
    }
    if !terrain.unsupported.is_empty() {
        return Err(format!(
            "Unsupported path prefab terrain effects: {}",
            terrain.unsupported.join(", ")
        ));
    }
    Ok(PathPrefab {
        fitting,
        terrain,
        runtime_holes,
    })
}
/// Road objects share numeric transforms/anchors but start from the path basis.
/// PathList's spawn path does not execute prefab TerrainFilter components.
pub(crate) fn compile_road_prefab(
    p: &crate::road_object_source::Prefab,
) -> Result<decor_fit::Prefab, String> {
    let prefab = road_prefab(p)?;
    let mut result = compile_extended(&prefab, true);
    result.filters.clear();
    result.unsupported.extend(
        result
            .pending
            .iter()
            .filter(|s| s.starts_with("Nested Decor"))
            .cloned(),
    );
    Ok(result)
}
fn road_prefab(p: &crate::road_object_source::Prefab) -> Result<Prefab, String> {
    let mut attributes = Vec::with_capacity(p.attributes.len());
    for raw in &p.attributes {
        if raw.get("kind").and_then(|v| v.as_str()) == Some("TerrainFilter") {
            continue;
        }
        let mut raw = raw.clone();
        raw.as_object_mut()
            .ok_or("Invalid road object attribute")?
            .remove("references");
        attributes.push(
            serde_json::from_value(raw)
                .map_err(|e| format!("Invalid road fitting attribute: {e}"))?,
        );
    }
    Ok(Prefab {
        rotation: p.rotation,
        scale: p.scale,
        attributes,
        components: p.components.clone(),
        issues: p.issues.clone(),
    })
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct AddHeightFields {
    opacity: f32,
    radius: f32,
    fade: f32,
    delta: f32,
}
pub(crate) fn compile_road_effects(
    p: &crate::road_object_source::Prefab,
    flags: &BTreeMap<String, u32>,
    protected: u32,
) -> Result<footprints::Prefab, String> {
    crate::road_object_connectors::local(p)?;
    let prefab = road_prefab(p)?;
    let mut out = collider_prefab(&prefab, &RoadMasks { flags, protected });
    let mut modifiers = Vec::new();
    for a in &prefab.attributes {
        let modifier = match a.kind.as_str() {
            "TerrainHeightAdd" => {
                let f: AddHeightFields = read(a)?;
                Some(papyrus_terrain::decor_height::Modifier {
                    position: a.position,
                    radius: f.radius,
                    fade: f.fade,
                    opacity: f.opacity,
                    mode: papyrus_terrain::decor_height::Mode::Add { delta: f.delta },
                })
            }
            "TerrainHeightSet" => {
                let f: RaiseFields = read(a)?;
                let mode = match f.height_mode {
                    0 => papyrus_terrain::decor_height::Mode::Set,
                    1 => papyrus_terrain::decor_height::Mode::Raise,
                    2 => papyrus_terrain::decor_height::Mode::Lower,
                    _ => return Err("Unsupported road height mode".into()),
                };
                Some(papyrus_terrain::decor_height::Modifier {
                    position: a.position,
                    radius: f.radius,
                    fade: f.fade,
                    opacity: f.opacity,
                    mode,
                })
            }
            _ => None,
        };
        if let Some(m) = modifier {
            modifiers.push(m);
        }
    }
    papyrus_terrain::decor_height::validate(&modifiers)?;
    out.height_modifiers = Some(modifiers);
    out.unsupported.retain(|s| {
        s != "No supported authored footprint"
            && s != "Additional terrain effect requires an adapter: TerrainHeightAdd"
            && s != "Unknown required terrain component: TerrainHeightAdd"
            && s != "Unknown required terrain component: TerrainPathConnect"
    });
    Ok(out)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompileMode {
    Legacy,
    Extended,
    PathAttached,
}
fn compile_impl(p: &Prefab, root_only: bool, mode: CompileMode) -> decor_fit::Prefab {
    let extended = mode != CompileMode::Legacy;
    let mut result = decor_fit::Prefab {
        rotation: p.rotation,
        scale: p.scale,
        transforms: vec![],
        anchors: vec![],
        filters: vec![],
        unsupported: p.issues.clone(),
        pending: vec![
            "Collision, environment/water validation and complete prefab assembly".into(),
        ],
    };
    for a in &p.attributes {
        if root_only
            && a.root == Some(false)
            && (matches!(a.kind.as_str(), "DecorRotate" | "DecorScale" | "DecorAlign")
                || extended && matches!(a.kind.as_str(), "DecorFlip" | "DecorSwim")
                || mode == CompileMode::PathAttached && a.kind == "DecorOffset")
        {
            result
                .pending
                .push(format!("Nested {} requires prefab assembly", a.kind));
            continue;
        }
        let outcome = (|| -> Result<(), String> {
            match a.kind.as_str() {
                "DecorFlip" if extended => {
                    if a.root != Some(true) {
                        return Err("Unknown-root DecorFlip".into());
                    }
                    let fields: FlipFields = read(a)?;
                    if fields.flip_axis > 2 {
                        return Err("Unknown DecorFlip axis".into());
                    }
                    result.transforms.push(decor_fit::Transform::Flip {
                        axis: fields.flip_axis,
                    });
                }
                "DecorSwim" if extended => {
                    if a.root != Some(true) || a.fields != serde_json::json!({}) {
                        return Err("Unknown-root/fields DecorSwim".into());
                    }
                    result.transforms.push(decor_fit::Transform::Swim);
                    result
                        .pending
                        .push("Water collision-ignore and volumes remain unvalidated".into());
                }
                "DecorRotate" => {
                    if a.root != Some(true) {
                        return Err("Nested/unknown-root DecorRotate".into());
                    }
                    let f: RotationFields = read(a)?;
                    result.transforms.push(decor_fit::Transform::Rotate {
                        min: f.min_rotation.array(),
                        max: f.max_rotation.array(),
                    });
                }
                "DecorScale" => {
                    if a.root != Some(true) {
                        return Err("Nested/unknown-root DecorScale".into());
                    }
                    let f: ScaleFields = read(a)?;
                    result.transforms.push(decor_fit::Transform::Scale {
                        min: f.min_scale.array(),
                        max: f.max_scale.array(),
                    });
                }
                "DecorOffset" if mode == CompileMode::PathAttached => {
                    if a.root != Some(true) {
                        return Err("Nested/unknown-root DecorOffset".into());
                    }
                    let fields: OffsetFields = read(a)?;
                    result.transforms.push(decor_fit::Transform::Offset {
                        min: fields.min_offset.array(),
                        max: fields.max_offset.array(),
                    });
                }
                "DecorAlign" => {
                    if a.root != Some(true) {
                        return Err("Nested/unknown-root DecorAlign".into());
                    }
                    let f: AlignFields = read(a)?;
                    let offset = f.slope_offset.array();
                    let slope_scale = f.slope_scale.array();
                    if extended {
                        result.transforms.push(decor_fit::Transform::AlignSlope {
                            normal: f.normal_alignment,
                            gradient: f.gradient_alignment,
                            offset,
                            scale: slope_scale,
                        });
                        return Ok(());
                    }
                    if offset != [0.; 3] || slope_scale != [1.; 3] {
                        return Err("Slope offset/scale alignment requires another adapter".into());
                    }
                    result.transforms.push(decor_fit::Transform::Align {
                        normal: f.normal_alignment,
                        gradient: f.gradient_alignment,
                    });
                }
                "TerrainAnchor" => {
                    let f: AnchorFields = read(a)?;
                    result.anchors.push(decor_fit::Anchor {
                        position: a.position,
                        extents: f.extents,
                        offset: f.offset,
                        radius: f.radius,
                        slope_scale: f.slope_scale,
                    });
                }
                "TerrainFilter" => {
                    let f: FilterFields = read(a)?;
                    let check_placement = f.check_placement_map.enabled()?;
                    let _ = f.check_terrain_bounds.enabled()?;
                    result.filters.push(decor_fit::PointFilter {
                        check_placement,
                        position: a.position,
                        filter: f.filter,
                        topology_radius: f.check_topology_radius,
                    });
                }
                "PrefabParameters" => {}
                "DecorSocketMale" | "DecorSocketFemale" => {
                    if a.fields.as_object().is_none_or(|fields| !fields.is_empty()) {
                        return Err("Unsupported decoration socket fields".into());
                    }
                }
                k if k.starts_with("Decor") => result
                    .unsupported
                    .push(format!("Unsupported transform: {k}")),
                k => result.pending.push(k.into()),
            }
            Ok(())
        })();
        if let Err(e) = outcome {
            result.unsupported.push(e);
        }
    }
    result.pending.sort();
    result.pending.dedup();
    result.unsupported.sort();
    result.unsupported.dedup();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path_terrain_fixture() -> serde_json::Value {
        let mut raw = serde_json::to_value(path_offset_prefab()).unwrap();
        let matrix = serde_json::json!([
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.]
        ]);
        raw["attributes"].as_array_mut().unwrap().extend([
            serde_json::json!({"kind":"TerrainHeightSet","root":true,"position":[0.,0.,0.],
                "fields":{"Opacity":1.,"Radius":2.,"Fade":0.,"HeightMode":1}}),
            serde_json::json!({"kind":"TerrainTopologyAdd","root":true,"position":[0.,0.,0.],
                "fields":{"Opacity":1.,"Radius":2.,"Fade":0.,"TopologyType":512}}),
            serde_json::json!({"kind":"TerrainHoleShape","root":false,"position":[0.,0.,0.],
                "local_to_root":matrix,"fields":{"radius":1.}}),
        ]);
        raw["components"]["TerrainHeightSet"] = 1.into();
        raw["components"]["TerrainTopologyAdd"] = 1.into();
        raw["components"]["TerrainHoleShape"] = 1.into();
        raw["hole_shapes"] = serde_json::json!([{"object_id":100,"local_to_root":matrix,"radius":1.,"enabled":true,
            "asset":{"name":"fixture","id":0,"start_instance_count":256,"asset_file":"asset","object_id":10,
                "mesh_file":"mesh","mesh_object_id":20,"mesh":{"vertices":[[0.,0.,0.],[1.,0.,0.],[0.,0.,1.]],"triangles":[[0,1,2]]}}}]);
        raw
    }
    #[test]
    fn path_prefab_prepares_numeric_terrain_and_preserves_validated_client_hole_data() {
        let flags = BTreeMap::from([("Field".into(), 1), ("Decor".into(), 512)]);
        let prepared = prepare_path_prefab(
            &serde_json::to_vec(&path_terrain_fixture()).unwrap(),
            &flags,
            0,
        )
        .unwrap();
        assert!(prepared.terrain.unsupported.is_empty());
        assert_eq!(prepared.terrain.height_modifiers.as_ref().unwrap().len(), 1);
        assert_eq!(
            prepared.terrain.topology.as_ref().unwrap().modifiers.len(),
            1
        );
        assert_eq!(prepared.runtime_holes.len(), 1);
        assert_eq!(prepared.runtime_holes[0].asset.mesh.vertices.len(), 3);
        assert!(
            prepared
                .terrain
                .pending
                .iter()
                .any(|issue| issue.contains("TerrainHoleShape"))
        );
        assert!(matches!(
            prepared.fitting.metadata().transforms[0],
            decor_fit::Transform::Offset { .. }
        ));
    }
    #[test]
    fn path_prefab_rejects_malformed_or_mismatched_holes_and_unhandled_terrain_operations() {
        let flags = BTreeMap::from([("Field".into(), 1), ("Decor".into(), 512)]);
        for case in 0..9 {
            let mut raw = path_terrain_fixture();
            match case {
                0 => raw["hole_shapes"][0]["asset"]["mesh"]["triangles"][0][0] = 99.into(),
                1 => raw["hole_shapes"][0]["local_to_root"][0][3] = 1.into(),
                2 => raw["hole_shapes"][0]["radius"] = 2.into(),
                3 => raw["hole_shapes"][0]["asset"]["future_shape_rule"] = true.into(),
                4 => raw["hole_shapes"][0]["local_to_root"][3][0] = 1.into(),
                5 => raw["hole_shapes"][0]["asset"]["mesh"]["vertices"] = serde_json::json!([]),
                6 => {
                    let attribute = raw["attributes"][3].clone();
                    raw["attributes"].as_array_mut().unwrap().push(attribute);
                    let hole = raw["hole_shapes"][0].clone();
                    raw["hole_shapes"].as_array_mut().unwrap().push(hole);
                    raw["components"]["TerrainHoleShape"] = 2.into();
                }
                7 => raw["attributes"][1]["kind"] = "TerrainHeightUnknown".into(),
                _ => raw["attributes"].as_array_mut().unwrap().push(serde_json::json!({
                    "kind":"TerrainFootprint","root":true,"position":[0.,0.,0.],
                    "local_to_root":[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],
                    "fields":{"Ring":[{"x":0.,"y":0.,"z":0.},{"x":1.,"y":0.,"z":0.},{"x":0.,"y":0.,"z":1.}],
                        "Center":{"x":0.,"y":0.,"z":0.},"RunStarts":[0],"RunClosed":[1],
                        "FillOffset":0.,"MaxFill":-1.,"RejectAboveGap":0.,"RimWidth":0.,"Feather":0.,
                        "Opacity":1.,"FillInterior":true,"MaxTiltDegrees":10.}
                })),
            }
            assert!(
                prepare_path_prefab(&serde_json::to_vec(&raw).unwrap(), &flags, 0).is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn path_prefab_requires_bounded_distinct_topology_roles_and_a_known_protected_mask() {
        let raw = serde_json::to_vec(&path_terrain_fixture()).unwrap();
        for flags in [
            BTreeMap::new(),
            BTreeMap::from([("Zero".into(), 0)]),
            BTreeMap::from([("First".into(), 512), ("Alias".into(), 512)]),
        ] {
            assert!(prepare_path_prefab(&raw, &flags, 0).is_err());
        }
        assert!(
            prepare_path_prefab(
                &raw,
                &BTreeMap::from([("Field".into(), 1), ("Decor".into(), 512)]),
                2
            )
            .is_err()
        );
    }
    fn path_offset_prefab() -> Prefab {
        serde_json::from_value(serde_json::json!({
            "rotation":[0.,0.,0.,1.], "scale":[1.,1.,1.],
            "attributes":[{
                "kind":"DecorOffset", "root":true, "position":[0.,0.,0.],
                "fields":{"MinOffset":{"x":0.,"y":-0.4,"z":0.},
                          "MaxOffset":{"x":0.,"y":-0.7,"z":0.}}
            }],
            "components":{"DecorOffset":1}, "issues":[]
        }))
        .unwrap()
    }

    #[test]
    fn path_fitting_compiles_root_offsets_through_a_separate_source_scope() {
        let prefab = path_offset_prefab();
        let legacy = compile_extended(&prefab, true);
        assert!(
            legacy
                .unsupported
                .iter()
                .any(|issue| issue == "Unsupported transform: DecorOffset")
        );
        let prepared = prepare_path_fitting(&serde_json::to_vec(&prefab).unwrap()).unwrap();
        assert!(matches!(&prepared.metadata().transforms[..],
            [decor_fit::Transform::Offset { min, max }] if *min == [0.,-0.4,0.] && *max == [0.,-0.7,0.]));
        assert!(prepared.metadata().unsupported.is_empty());
    }

    #[test]
    fn path_fitting_keeps_nested_offsets_in_the_assembly_requirements() {
        let mut prefab = path_offset_prefab();
        prefab.attributes[0].root = Some(false);
        let prepared = prepare_path_fitting(&serde_json::to_vec(&prefab).unwrap()).unwrap();
        assert!(prepared.metadata().transforms.is_empty());
        assert!(
            prepared
                .metadata()
                .pending
                .iter()
                .any(|issue| issue == "Nested DecorOffset requires prefab assembly")
        );
        prefab.attributes[0].root = None;
        assert!(prepare_path_fitting(&serde_json::to_vec(&prefab).unwrap()).is_err());
    }

    #[test]
    fn path_fitting_rejects_unknown_fields_bad_offsets_and_mismatched_hole_rosters() {
        let prefab = path_offset_prefab();
        let raw = serde_json::to_value(&prefab).unwrap();
        for case in 0..5 {
            let mut bad = raw.clone();
            match case {
                0 => bad["attributes"][0]["fields"]["FutureOffsetRule"] = 1.into(),
                1 => bad["attributes"][0]["fields"]["MinOffset"]["y"] = "invalid".into(),
                2 => bad["components"]["TerrainHoleShape"] = 1.into(),
                3 => bad["hole_shapes"] = serde_json::json!([{}]),
                _ => bad["future_prefab_rule"] = 1.into(),
            }
            assert!(
                prepare_path_fitting(&serde_json::to_vec(&bad).unwrap()).is_err(),
                "case {case}"
            );
        }
        let mut matching = raw;
        matching["components"]["TerrainHoleShape"] = 1.into();
        matching["hole_shapes"] = serde_json::json!([{}]);
        matching["attributes"].as_array_mut().unwrap().push(serde_json::json!({
            "kind":"TerrainHoleShape", "root":false, "position":[0.,0.,0.], "fields":{"radius":1.}
        }));
        let prepared = prepare_path_fitting(&serde_json::to_vec(&matching).unwrap()).unwrap();
        assert!(
            prepared
                .metadata()
                .pending
                .iter()
                .any(|issue| issue == "TerrainHoleShape")
        );
    }

    #[test]
    fn live_topology_only_prefabs_do_not_need_fake_polygons_but_unknown_effects_still_block() {
        let flags = BTreeMap::from([
            ("Field".into(), 1),
            ("Building".into(), 512),
            ("Monument".into(), 1024),
        ]);
        let policy = RoadMasks {
            flags: &flags,
            protected: 1024,
        };
        let mut p:Prefab=serde_json::from_value(serde_json::json!({"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],"attributes":[{"kind":"TerrainTopologyAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":3.,"Fade":0.,"TopologyType":512}}],"components":{"TerrainTopologyAdd":1},"issues":[]})).unwrap();
        assert!(
            collider_prefab(&p, &policy)
                .unsupported
                .iter()
                .any(|s| s == "No supported authored footprint")
        );
        let live = live_prefab(&p, &policy);
        assert!(live.unsupported.is_empty());
        assert_eq!(live.topology.unwrap().modifiers.len(), 1);
        for enabled in [0, 1] {
            let mut with_filter: Prefab =
                serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
            with_filter.attributes.push(serde_json::from_value(serde_json::json!({"kind":"TerrainFilter","root":false,"position":[2.,0.,3.],"fields":{"Filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"CheckPlacementMap":enabled,"CheckTerrainBounds":0,"CheckTopologyRadius":0.}})).unwrap());
            let compiled = live_prefab(&with_filter, &policy);
            assert!(compiled.unsupported.is_empty());
            assert_eq!(
                compiled.topology.unwrap().queries[0].check_placement,
                enabled == 1
            );
        }
        p.attributes.push(serde_json::from_value(serde_json::json!({"kind":"TerrainFuturePaint","root":true,"position":[0.,0.,0.],"fields":{}})).unwrap());
        assert!(!live_prefab(&p, &policy).unsupported.is_empty());
    }
    use crate::{
        AssetIdentity,
        installation::{Installation, InstallationKind},
    };
    fn fixture(root: &Path) -> (Source, Catalog) {
        std::fs::create_dir_all(root.join("Bundles/maps")).unwrap();
        std::fs::write(root.join("Bundles/maps/maps.bundle"), b"scene").unwrap();
        let file = |path: &str, c: char| SourceFile {
            relative_path: path.into(),
            sha256: c.to_string().repeat(64),
            bytes: 1,
        };
        let mut sources = vec![
            file("RustClient_Data/Plugins/x86_64/RustNative.dll", 'a'),
            file("Bundles/AssetSceneManifest.json", 'b'),
            file("Bundles/shared/assetscenes.bundle", 'c'),
        ];
        let catalog = Catalog {
            sha256: "d".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources.clone(),
            entries: Default::default(),
        };
        sources.push(
            installation::hash_file(root, "Bundles/maps/maps.bundle", 1024, &|| false).unwrap(),
        );
        let path = "assets/bundled/prefabs/autospawn/decor/rocks/a.prefab";
        let stages=serde_json::from_value(serde_json::json!([{"id":"1","order":0,"name":"Rocks","kind":"uniform","folder":"decor/rocks","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":1,"anchor_mode":0,"distance":10.,"dithering":5.,"density":0.,"cluster":null,"prefabs":[path]}])).unwrap();
        let doc = Source {
            format: "papyrus-decor-source".into(),
            schema: 1,
            adapter: "papyrus-static-decor-1".into(),
            stage_layout: None,
            scene: "Synthetic".into(),
            sources,
            extra_sources: vec![],
            code_sources: vec![],
            extractor_sha256: "e".repeat(64),
            stages,
            prefabs: BTreeMap::from([(
                path.into(),
                Prefab {
                    rotation: [0., 0., 0., 1.],
                    scale: [1., 1., 1.],
                    attributes: vec![],
                    components: BTreeMap::new(),
                    issues: vec![],
                },
            )]),
            issues: vec![],
            terrain_policy: None,
            sha256: String::new(),
            live_prefabs: OnceLock::new(),
        };
        (doc, catalog)
    }
    #[test]
    fn cache_publication_closes_staged_files_and_reuses_the_saved_document() {
        let directory = tempfile::tempdir().unwrap();
        let (source, catalog) = fixture(directory.path());
        let input = directory.path().join("source.json");
        let bytes = serde_json::to_vec(&source).unwrap();
        std::fs::write(&input, &bytes).unwrap();
        let cache = directory.path().join("cache");
        let first = Source::cache_in(directory.path(), &input, &catalog, &cache, || false)
            .unwrap();
        let saved = cache.join(&first.sha256).join("decor.json");
        assert_eq!(std::fs::read(&saved).unwrap(), bytes);
        let modified = std::fs::metadata(&saved).unwrap().modified().unwrap();
        let second = Source::cache_in(directory.path(), &input, &catalog, &cache, || false)
            .unwrap();
        assert_eq!(second.sha256, first.sha256);
        assert_eq!(std::fs::metadata(&saved).unwrap().modified().unwrap(), modified);
        let cancelled = directory.path().join("cancelled-cache");
        assert!(Source::cache_in(directory.path(), &input, &catalog, &cancelled, || true)
            .is_err());
        assert!(!cancelled.exists());
        assert_eq!(std::fs::read(&input).unwrap(), bytes);
    }
    #[test]
    fn gameplay_bound_sources_reject_changed_code_and_legacy_sources_remain_readable() {
        let root = tempfile::tempdir().unwrap();
        let (mut source, catalog) = fixture(root.path());
        source.schema = 4;
        source.adapter = "papyrus-static-decor-collider-4".into();
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/topology/client-0f703d43.json"
        ))
        .unwrap();
        let flags: BTreeMap<String, u32> =
            serde_json::from_value(profile["flags"].clone()).unwrap();
        let protected = [
            "Monument", "Building", "Road", "Roadside", "Rail", "Railside", "Ocean", "River",
            "Lake",
        ]
        .iter()
        .fold(0, |bits, name| bits | flags[*name]);
        source.terrain_policy = Some(TerrainPolicy {
            material_removal: None,
            profile_id: profile["id"].as_str().unwrap().into(),
            profile_sha256: "f".repeat(64),
            native_sha256: "a".repeat(64),
            scene_sha256: "c".repeat(64),
            topology_flags: flags,
            protected_topology: protected,
        });
        let legacy = root.path().join("legacy.json");
        std::fs::write(&legacy, serde_json::to_vec(&source).unwrap()).unwrap();
        source.schema = 5;
        source.adapter = "papyrus-static-decor-collider-5".into();
        std::fs::write(
            root.path().join("GameAssembly.dll"),
            b"owned gameplay fixture",
        )
        .unwrap();
        source.code_sources = vec![
            installation::hash_file(root.path(), "GameAssembly.dll", 1024, &|| false).unwrap(),
        ];
        source.validate(&catalog).unwrap();
        let prepared = source.prepare_live(&catalog).unwrap();
        assert!(prepared.terrain.values().all(|p| p.support.len() == 1));
        let authored = source.prepare_authored_live(&catalog).unwrap();
        assert!(authored.terrain.values().all(|p| p.support.is_empty()));
        let mut with_anchors: Source =
            serde_json::from_value(serde_json::to_value(&source).unwrap()).unwrap();
        let prefab = with_anchors.prefabs.values_mut().next().unwrap();
        for position in [[0.; 3], [2., 0., 3.]] {
            prefab.attributes.push(
                serde_json::from_value(serde_json::json!({
                    "kind":"TerrainAnchor", "root":false,"position":position,
                    "fields":{"Extents":2.,"Offset":0.,"Radius":1.,"SlopeScale":0.}
                }))
                .unwrap(),
            );
        }
        let prepared = with_anchors.prepare_authored_live(&catalog).unwrap();
        let support = &prepared.terrain.values().next().unwrap().support;
        assert_eq!(support.len(), 2);
        assert_eq!(support[0].position, [0.; 3]);
        assert_eq!(support[1].position, [2., 0., 3.]);
        assert!(support.iter().all(|s| s.radius == 1.));
        let path = root.path().join("current.json");
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        let loaded = Source::load(root.path(), &path, &catalog, || false).unwrap();
        assert_eq!(loaded.gameplay_sources(), source.gameplay_sources());
        let current = serde_json::to_vec(&source).unwrap();
        assert!(
            !serde_json::from_slice::<serde_json::Value>(&current)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("stage_layout")
        );
        let mut updated: Source = serde_json::from_slice(&current).unwrap();
        updated.schema = 6;
        updated.adapter = "papyrus-static-decor-collider-6".into();
        updated.stage_layout = Some("mode-description".into());
        updated.validate(&catalog).unwrap();
        let updated_raw = serde_json::to_vec(&updated).unwrap();
        let updated_path = root.path().join("updated.json");
        std::fs::write(&updated_path, &updated_raw).unwrap();
        assert_eq!(
            Source::load(root.path(), &updated_path, &catalog, || false)
                .unwrap()
                .stage_layout
                .as_deref(),
            Some("mode-description")
        );
        for (pointer, value) in [
            ("/schema", serde_json::json!(5)),
            (
                "/adapter",
                serde_json::json!("papyrus-static-decor-collider-5"),
            ),
            ("/stage_layout", serde_json::Value::Null),
            ("/stage_layout", serde_json::json!("future")),
            ("/code_sources", serde_json::json!([])),
        ] {
            let mut changed: serde_json::Value = serde_json::from_slice(&updated_raw).unwrap();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                serde_json::from_value::<Source>(changed)
                    .unwrap()
                    .validate(&catalog)
                    .is_err(),
                "{pointer}"
            );
        }
        for invalid in ["duplicate", "traversal", "empty", "wrong_revision"] {
            let mut copy: Source = serde_json::from_slice(&current).unwrap();
            match invalid {
                "duplicate" => copy.code_sources.push(copy.code_sources[0].clone()),
                "traversal" => copy.code_sources[0].relative_path = "../GameAssembly.dll".into(),
                "empty" => copy.code_sources.clear(),
                _ => {
                    copy.schema = 4;
                    copy.adapter = "papyrus-static-decor-collider-4".into();
                }
            }
            assert!(copy.validate(&catalog).is_err(), "{invalid}");
        }
        std::fs::write(root.path().join("GameAssembly.dll"), b"changed gameplay").unwrap();
        assert!(
            Source::load(root.path(), &path, &catalog, || false)
                .err()
                .unwrap()
                .contains("gameplay code")
        );
        assert!(
            Source::load(root.path(), &legacy, &catalog, || false)
                .unwrap()
                .gameplay_sources()
                .is_empty()
        );
    }
    #[test]
    fn collider_snapshot_validates_geometry_receipts_and_explicit_recipe_upgrade() {
        let root = tempfile::tempdir().unwrap();
        let (mut source, catalog) = fixture(root.path());
        source.schema = 4;
        source.adapter = "papyrus-static-decor-collider-4".into();
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/topology/client-0f703d43.json"
        ))
        .unwrap();
        let flags: BTreeMap<String, u32> =
            serde_json::from_value(profile["flags"].clone()).unwrap();
        let protected = [
            "Monument", "Building", "Road", "Roadside", "Rail", "Railside", "Ocean", "River",
            "Lake",
        ]
        .iter()
        .fold(0, |mask, key| mask | flags[*key]);
        source.terrain_policy = Some(TerrainPolicy {
            material_removal: None,
            profile_id: "fixture".into(),
            profile_sha256: "f".repeat(64),
            native_sha256: "a".repeat(64),
            scene_sha256: "c".repeat(64),
            topology_flags: flags,
            protected_topology: protected,
        });
        source
            .prefabs
            .values_mut()
            .next()
            .unwrap()
            .attributes
            .push(Attribute {
                kind: "AddToHeightMap".into(),
                root: Some(false),
                position: [0.; 3],
                local_to_root: Some([
                    [1., 0., 0., 0.],
                    [0., 1., 0., 0.],
                    [0., 0., 1., 0.],
                    [0., 0., 0., 1.],
                ]),
                fields: serde_json::json!({"DestroyGameObject":1}),
                collider: Some(papyrus_terrain::height_mesh::Mesh {
                    kind: "raw-triangle-mesh".into(),
                    enabled: true,
                    trigger: false,
                    convex: false,
                    cooking_options: 30,
                    backfaces: false,
                    vertices: vec![[0., 0., 0.], [1., 1., 0.], [0., 0., 1.]],
                    triangles: vec![[0, 2, 1]],
                }),
            });
        std::fs::create_dir_all(root.path().join("Bundles/shared")).unwrap();
        let bundle = root.path().join("Bundles/shared/content.bundle");
        std::fs::write(&bundle, b"collision mesh source").unwrap();
        source.extra_sources.push(
            installation::hash_file(root.path(), "Bundles/shared/content.bundle", 1024, &|| {
                false
            })
            .unwrap(),
        );
        source.validate(&catalog).unwrap();
        assert_eq!(source.recipe().engine, COLLIDER_VERSION);
        // Geometry extraction must not silently enable collider placement in revision 9.
        let prefab = source.prefabs.values().next().unwrap();
        assert!(
            height_prefab(prefab, source.terrain_policy.as_ref().unwrap())
                .unsupported
                .iter()
                .any(|reason| reason.contains("AddToHeightMap"))
        );
        let compiled = collider_prefab(prefab, source.terrain_policy.as_ref().unwrap());
        assert!(compiled.unsupported.is_empty());
        assert_eq!(compiled.contributors.as_ref().unwrap().len(), 1);
        let mut invalid: Prefab =
            serde_json::from_slice(&serde_json::to_vec(prefab).unwrap()).unwrap();
        invalid.attributes[0].collider.as_mut().unwrap().convex = true;
        assert!(
            !collider_prefab(&invalid, source.terrain_policy.as_ref().unwrap())
                .unsupported
                .is_empty()
        );
        invalid.attributes[0].collider = None;
        assert!(
            !collider_prefab(&invalid, source.terrain_policy.as_ref().unwrap())
                .unsupported
                .is_empty()
        );
        let path = root.path().join("decor.json");
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        let restored = Source::load(root.path(), &path, &catalog, || false).unwrap();
        assert_eq!(
            serde_json::to_vec(&source).unwrap(),
            serde_json::to_vec(&restored).unwrap()
        );
        assert!(Source::load(root.path(), &path, &catalog, || true).is_err());
        std::fs::write(&bundle, b"different source").unwrap();
        assert!(
            Source::load(root.path(), &path, &catalog, || false)
                .err()
                .unwrap()
                .contains("Contributor")
        );
        let receipt = source.extra_sources[0].clone();
        source.extra_sources.push(receipt.clone());
        assert!(source.validate(&catalog).is_err());
        source.extra_sources.pop();
        source.extra_sources[0].relative_path = "../content.bundle".into();
        assert!(source.validate(&catalog).is_err());
        source.extra_sources[0] = receipt;
        let attribute = &mut source.prefabs.values_mut().next().unwrap().attributes[0];
        attribute.collider.as_mut().unwrap().triangles[0][0] = 3;
        assert!(source.validate(&catalog).is_err());
        let attribute = &mut source.prefabs.values_mut().next().unwrap().attributes[0];
        attribute.collider.as_mut().unwrap().triangles[0][0] = 0;
        attribute.kind = "TerrainAnchor".into();
        assert!(source.validate(&catalog).is_err());
        source.prefabs.values_mut().next().unwrap().attributes[0].kind = "AddToHeightMap".into();
        source.schema = 3;
        source.adapter = "papyrus-static-decor-3".into();
        source.extra_sources.clear();
        assert!(source.validate(&catalog).is_err());
    }
    #[test]
    fn source_requires_current_scene_bundle_and_complete_prefab_coverage() {
        let root = tempfile::tempdir().unwrap();
        let (mut doc, catalog) = fixture(root.path());
        let path = root.path().join("decor.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(hash(
            &Source::load(root.path(), &path, &catalog, || false)
                .unwrap()
                .sha256
        ));
        assert!(Source::load(root.path(), &path, &catalog, || true).is_err());
        std::fs::write(
            root.path().join("Bundles/maps/maps.bundle"),
            b"updated scene",
        )
        .unwrap();
        assert!(Source::load(root.path(), &path, &catalog, || false).is_err());
        doc.prefabs.clear();
        assert!(doc.validate(&catalog).is_err());
    }
    #[test]
    fn fitting_revision_and_typed_fields_fail_closed_without_changing_legacy_recipes() {
        let temp = tempfile::tempdir().unwrap();
        let (mut src, catalog) = fixture(temp.path());
        src.sha256 = "f".repeat(64);
        assert_eq!(src.recipe().engine, decor::VERSION);
        src.schema = 2;
        src.adapter = "papyrus-static-decor-2".into();
        assert_eq!(src.recipe().engine, decor_fit::VERSION);
        let prefab = src.prefabs.values_mut().next().unwrap();
        prefab.attributes.push(Attribute{ collider: None,kind:"DecorRotate".into(),root:Some(true),local_to_root:None,position:[0.;3],fields:serde_json::json!({"MinRotation":{"x":0.,"y":37.,"z":0.},"MaxRotation":{"x":0.,"y":37.,"z":0.}})});
        assert!(compile(prefab).unsupported.is_empty());
        prefab.attributes[0].fields["NewUnknownSetting"] = 1.into();
        assert!(!compile(prefab).unsupported.is_empty());
        prefab.attributes[0]
            .fields
            .as_object_mut()
            .unwrap()
            .remove("NewUnknownSetting");
        prefab.attributes[0].root = Some(false);
        assert!(!compile(prefab).unsupported.is_empty());
        prefab.attributes[0].root = None;
        assert!(src.validate(&catalog).is_err());
        assert!(
            Recipe {
                engine: decor_fit::VERSION.into(),
                source_sha256: "a".repeat(64)
            }
            .validate()
            .is_ok()
        );
    }
    #[test]
    fn root_scope_keeps_child_decor_out_of_parent_pose_but_retains_child_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let (mut source, _) = fixture(dir.path());
        let prefab = source.prefabs.values_mut().next().unwrap();
        prefab.attributes = vec![
            Attribute {
                collider: None,
                kind: "DecorRotate".into(),
                root: Some(true),
                local_to_root: None,
                position: [0.; 3],
                fields: serde_json::json!({"MinRotation":{"x":0,"y":37,"z":0},"MaxRotation":{"x":0,"y":37,"z":0}}),
            },
            Attribute {
                collider: None,
                kind: "DecorScale".into(),
                root: Some(false),
                local_to_root: None,
                position: [10., 0., 0.],
                fields: serde_json::json!({"MinScale":{"x":2,"y":2,"z":2},"MaxScale":{"x":2,"y":2,"z":2}}),
            },
            Attribute {
                collider: None,
                kind: "TerrainAnchor".into(),
                root: Some(false),
                local_to_root: None,
                position: [10., 0., 0.],
                fields: serde_json::json!({"Extents":1,"Offset":0,"Radius":0,"SlopeScale":0}),
            },
        ];
        let legacy = compile(prefab);
        assert!(
            legacy
                .unsupported
                .iter()
                .any(|r| r == "Nested/unknown-root DecorScale")
        );
        let current = compile_scoped(prefab, true);
        assert!(current.unsupported.is_empty());
        assert_eq!(current.transforms.len(), 1);
        assert_eq!(current.anchors.len(), 1);
        assert!(
            current
                .pending
                .iter()
                .any(|r| r == "Nested DecorScale requires prefab assembly")
        );
        prefab.attributes[1].root = None;
        assert!(!compile_scoped(prefab, true).unsupported.is_empty());
        prefab.attributes[1].root = Some(true);
        let current = compile_scoped(prefab, true);
        assert!(current.unsupported.is_empty());
        assert_eq!(current.transforms.len(), 2);
    }
    #[test]
    fn extended_transform_fields_are_typed_and_legacy_rejections_remain() {
        let dir = tempfile::tempdir().unwrap();
        let (mut source, _) = fixture(dir.path());
        let p = source.prefabs.values_mut().next().unwrap();
        p.attributes = vec![
            Attribute {
                collider: None,
                kind: "DecorFlip".into(),
                root: Some(true),
                local_to_root: None,
                position: [0.; 3],
                fields: serde_json::json!({"FlipAxis":1}),
            },
            Attribute {
                collider: None,
                kind: "DecorSwim".into(),
                root: Some(true),
                local_to_root: None,
                position: [0.; 3],
                fields: serde_json::json!({}),
            },
        ];
        assert_eq!(compile_extended(p, false).unsupported.len(), 2);
        let fitted = compile_extended(p, true);
        assert!(fitted.unsupported.is_empty());
        assert_eq!(fitted.transforms.len(), 2);
        p.attributes[0].fields["FlipAxis"] = 3.into();
        assert!(!compile_extended(p, true).unsupported.is_empty());
        p.attributes[0].fields["UnknownField"] = 1.into();
        assert!(!compile_extended(p, true).unsupported.is_empty());
        p.attributes[0].root = Some(false);
        p.attributes[1].root = Some(false);
        let fitted = compile_extended(p, true);
        assert!(fitted.unsupported.is_empty());
        assert!(fitted.transforms.is_empty());
        assert!(
            fitted
                .pending
                .iter()
                .any(|r| r == "Nested DecorSwim requires prefab assembly")
        );
    }
    #[test]
    fn compiler_separates_fitted_geometry_from_required_unimplemented_effects() {
        let temp = tempfile::tempdir().unwrap();
        let (mut src, _) = fixture(temp.path());
        let p = src.prefabs.values_mut().next().unwrap();
        p.attributes = vec![
            Attribute {
                collider: None,
                kind: "TerrainFilter".into(),
                root: Some(false),
                local_to_root: None,
                position: [1., 2., 3.],
                fields: serde_json::json!({"Filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"CheckPlacementMap":1,"CheckTerrainBounds":0,"CheckTopologyRadius":3.5}),
            },
            Attribute {
                collider: None,
                kind: "TerrainFootprint".into(),
                root: Some(true),
                local_to_root: None,
                position: [0.; 3],
                fields: serde_json::json!({}),
            },
        ];
        let fitted = compile(p);
        assert!(fitted.unsupported.is_empty());
        assert_eq!(fitted.filters.len(), 1);
        assert_eq!(fitted.filters[0].position, [1., 2., 3.]);
        assert!(fitted.filters[0].check_placement);
        assert!(
            !fitted
                .pending
                .iter()
                .any(|s| s == "Placement-map occupancy")
        );
        assert!(fitted.pending.iter().any(|s| s == "TerrainFootprint"));
    }
    #[test]
    fn footprint_revision_requires_bound_topology_roles_and_component_matrices() {
        let temp = tempfile::tempdir().unwrap();
        let (mut src, catalog) = fixture(temp.path());
        src.schema = 3;
        src.adapter = "papyrus-static-decor-3".into();
        assert!(src.validate(&catalog).is_err());
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/topology/client-0f703d43.json"
        ))
        .unwrap();
        let flags: BTreeMap<String, u32> =
            serde_json::from_value(profile["flags"].clone()).unwrap();
        let protected = [
            "Monument", "Building", "Road", "Roadside", "Rail", "Railside", "Ocean", "River",
            "Lake",
        ]
        .iter()
        .fold(0, |m, k| m | flags[*k]);
        src.terrain_policy = Some(TerrainPolicy {
            material_removal: None,
            profile_id: "fixture".into(),
            profile_sha256: "f".repeat(64),
            native_sha256: "a".repeat(64),
            scene_sha256: "c".repeat(64),
            topology_flags: flags,
            protected_topology: protected,
        });
        src.validate(&catalog).unwrap();
        assert_eq!(src.recipe().engine, HEIGHT_VERSION);
        for role in ["Building", "Mountain", "Road"] {
            let policy = src.terrain_policy.as_mut().unwrap();
            policy.material_removal = Some(papyrus_terrain::decor_material::Removal {
                replacement_channels: [2, 4, 4, 1, 0],
                excluded_topology: policy.topology_flags[role],
            });
            assert_eq!(src.validate(&catalog).is_ok(), role != "Road");
        }
        src.terrain_policy.as_mut().unwrap().material_removal = None;
        src.terrain_policy.as_mut().unwrap().protected_topology = 0;
        assert!(src.validate(&catalog).is_err());
        src.terrain_policy.as_mut().unwrap().protected_topology = protected;
        src.prefabs
            .values_mut()
            .next()
            .unwrap()
            .attributes
            .push(Attribute {
                collider: None,
                kind: "TerrainAnchor".into(),
                root: Some(false),
                local_to_root: None,
                position: [0.; 3],
                fields: serde_json::json!({}),
            });
        assert!(src.validate(&catalog).is_err());
    }
    #[test]
    fn footprint_adapter_preserves_child_space_and_rejects_unknown_runs() {
        let temp = tempfile::tempdir().unwrap();
        let (mut src, _) = fixture(temp.path());
        let p = src.prefabs.values_mut().next().unwrap();
        p.attributes.push(Attribute{ collider: None,kind:"TerrainFootprint".into(),root:Some(false),local_to_root:Some([[1.,0.,0.,10.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]]),position:[10.,0.,0.],fields:serde_json::json!({"Ring":[{"x":-2.,"y":0.,"z":-2.},{"x":2.,"y":0.,"z":-2.},{"x":0.,"y":0.,"z":2.}],"Center":{"x":0.,"y":0.,"z":0.},"RunStarts":[0],"RunClosed":[1],"FillOffset":0.,"MaxFill":4.,"RejectAboveGap":0.,"RimWidth":2.,"Feather":3.,"Opacity":1.,"FillInterior":1,"MaxTiltDegrees":35.})});
        let spec = footprint_prefab(p);
        assert!(spec.unsupported.is_empty());
        assert_eq!(spec.footprint.unwrap().local_to_root[0][3], 10.);
        p.attributes[0].fields["RunClosed"] = serde_json::json!([0]);
        assert!(!footprint_prefab(p).unsupported.is_empty());
    }
    #[test]
    fn portable_decoration_recipe_preserves_terrain_and_checks_composition() {
        let temp = tempfile::tempdir().unwrap();
        let (_, catalog) = fixture(temp.path());
        let mut project = Project {
            installation: Some(catalog.installation),
            preview_engine: crate::native_preview::VERSION.into(),
            monument_assets: Some(AssetIdentity {
                catalog_sha256: catalog.sha256,
                bindings_sha256: "f".repeat(64),
            }),
            ..Project::default()
        };
        let key = project.terrain_key();
        project.decoration = Some(Recipe::new("d".repeat(64)));
        project.validate().unwrap();
        assert_eq!(key, project.terrain_key());
        let restored: Project =
            serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
        assert_eq!(project, restored);
        let profile = temp.path().join("decor.rec");
        crate::profile::Profile::new("Decor fixture".into(), project.clone())
            .save(&profile)
            .unwrap();
        assert_eq!(crate::profile::Profile::load(&profile).unwrap().recipe, {
            let mut current = project.clone();
            crate::generation_defaults::apply(&mut current);
            current
        });
        let result = crate::headless::generate(
            &project,
            &crate::headless::Options {
                skip_composition: true,
                ..Default::default()
            },
            &std::sync::atomic::AtomicBool::new(false),
            |_, _| {},
        );
        assert!(matches!(result,Err(e) if e.contains("skip requested decoration")));
        project.composition_engine = None;
        assert!(project.validate().is_err());
    }
    #[test]
    fn blocker_adapter_preserves_authored_centers_and_rejects_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        let (mut src, _) = fixture(root.path());
        let p = src.prefabs.values_mut().next().unwrap();
        p.attributes.clear();
        p.attributes.push(Attribute {
            collider: None,
            kind: "TerrainPlacementBlocked".into(),
            root: Some(false),
            local_to_root: Some([
                [1., 0., 0., 7.],
                [0., 1., 0., 0.],
                [0., 0., 1., 2.],
                [0., 0., 0., 1.],
            ]),
            position: [7., 0., 2.],
            fields: serde_json::json!({"Opacity":0.,"Radius":16.,"Fade":4.}),
        });
        let compiled = blocker_prefab(p);
        assert!(compiled.unsupported.is_empty());
        assert_eq!(compiled.blockers.len(), 1);
        assert_eq!(compiled.blockers[0].position, [7., 0., 2.]);
        assert_eq!(compiled.blockers[0].radius, 16.);
        assert!(
            !compiled
                .pending
                .iter()
                .any(|s| s == "TerrainPlacementBlocked")
        );
        assert!(
            footprint_prefab(p)
                .unsupported
                .iter()
                .any(|s| s == "No supported authored footprint")
        );
        p.attributes[0].fields["NewUnknownEffect"] = serde_json::json!(true);
        assert!(!blocker_prefab(p).unsupported.is_empty());
    }
    #[test]
    fn blocker_height_adapter_only_accepts_inspected_raise_mode() {
        let root = tempfile::tempdir().unwrap();
        let (mut src, _) = fixture(root.path());
        let p = src.prefabs.values_mut().next().unwrap();
        p.attributes = vec![
            Attribute {
                collider: None,
                kind: "TerrainPlacementBlocked".into(),
                root: Some(true),
                local_to_root: None,
                position: [0.; 3],
                fields: serde_json::json!({"Opacity":1.,"Radius":16.,"Fade":0.}),
            },
            Attribute {
                collider: None,
                kind: "TerrainHeightSet".into(),
                root: Some(true),
                local_to_root: None,
                position: [1., 2., 3.],
                fields: serde_json::json!({"Opacity":0.3,"Radius":12.,"Fade":5.,"HeightMode":1}),
            },
        ];
        let f = blocker_prefab(p);
        assert!(f.unsupported.is_empty());
        assert_eq!(f.raises.len(), 1);
        assert_eq!(f.raises[0].position, [1., 2., 3.]);
        for mode in [0, 2, 3] {
            p.attributes[1].fields["HeightMode"] = serde_json::json!(mode);
            assert!(!blocker_prefab(p).unsupported.is_empty());
        }
    }
    #[test]
    fn topology_adapter_preserves_raw_masks_order_and_rejects_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        let (mut src, _) = fixture(root.path());
        let p = src.prefabs.values_mut().next().unwrap();
        let policy = TerrainPolicy {
            material_removal: None,
            profile_id: "fixture".into(),
            profile_sha256: "a".repeat(64),
            native_sha256: "b".repeat(64),
            scene_sha256: "c".repeat(64),
            topology_flags: BTreeMap::from([
                ("Protected".into(), 1),
                ("Decor".into(), 8),
                ("Other".into(), 2),
            ]),
            protected_topology: 1,
        };
        let attr = |kind: &str, fields| Attribute {
            collider: None,
            kind: kind.into(),
            root: Some(false),
            local_to_root: None,
            position: [3., 0., 5.],
            fields,
        };
        p.attributes = vec![
            attr(
                "TerrainPlacementBlocked",
                serde_json::json!({"Opacity":1.,"Radius":2.,"Fade":0.}),
            ),
            attr(
                "TerrainTopologySet",
                serde_json::json!({"Opacity":0.,"Radius":4.,"Fade":1.,"TopologyType":2}),
            ),
            attr(
                "TerrainTopologyAdd",
                serde_json::json!({"Opacity":1.,"Radius":8.,"Fade":2.,"TopologyType":8}),
            ),
        ];
        let out = topology_prefab(p, &policy);
        assert!(out.unsupported.is_empty());
        let t = out.topology.unwrap();
        assert_eq!(t.modifiers[0].mask, 2);
        assert_eq!(t.modifiers[1].mask, 8);
        assert_eq!(t.modifiers[0].position, [3., 0., 5.]);
        assert!(matches!(
            t.modifiers[0].operation,
            decor_topology::Operation::Set
        ));
        assert!(matches!(
            t.modifiers[1].operation,
            decor_topology::Operation::Add
        ));
        assert!(
            !out.pending
                .iter()
                .any(|s| s == "TerrainTopologySet" || s == "TerrainTopologyAdd")
        );
        assert!(
            blocker_prefab(p)
                .pending
                .iter()
                .any(|s| s == "TerrainTopologySet")
        );
        p.attributes[1].fields["TopologyType"] = serde_json::json!(16);
        assert!(!topology_prefab(p, &policy).unsupported.is_empty());
        p.attributes[1].fields["TopologyType"] = serde_json::json!(2);
        p.attributes[1].fields["FutureBehavior"] = serde_json::json!(true);
        assert!(!topology_prefab(p, &policy).unsupported.is_empty());
    }
    #[test]
    fn every_recipe_revision_remains_explicit_and_round_trips() {
        for engine in [
            decor::VERSION,
            decor_fit::VERSION,
            footprints::VERSION,
            footprints::BLOCKER_VERSION,
            decor_topology::VERSION,
            ROOT_VERSION,
            TRANSFORM_VERSION,
            MATERIAL_VERSION,
            HEIGHT_VERSION,
            COLLIDER_VERSION,
        ] {
            let recipe = Recipe {
                engine: engine.into(),
                source_sha256: "a".repeat(64),
            };
            recipe.validate().unwrap();
            let restored: Recipe =
                serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
            assert_eq!(recipe, restored);
        }
    }
    #[test]
    fn height_adapter_supports_all_direct_modes_without_doubling_legacy_raises_or_ignoring_colliders()
     {
        let root = tempfile::tempdir().unwrap();
        let (mut source, _) = fixture(root.path());
        let p = source.prefabs.values_mut().next().unwrap();
        let policy = TerrainPolicy {
            material_removal: None,
            profile_id: "fixture".into(),
            profile_sha256: "a".repeat(64),
            native_sha256: "b".repeat(64),
            scene_sha256: "c".repeat(64),
            topology_flags: BTreeMap::from([("Protected".into(), 1), ("Decor".into(), 8)]),
            protected_topology: 1,
        };
        p.attributes = vec![Attribute {
            collider: None,
            kind: "TerrainHeightSet".into(),
            root: Some(false),
            local_to_root: None,
            position: [3., 4., 5.],
            fields: serde_json::json!({"Opacity":0.3,"Radius":65.,"Fade":80.,"HeightMode":1}),
        }];
        assert!(!material_prefab(p, &policy).unsupported.is_empty());
        for mode in 0..=2 {
            p.attributes[0].fields["HeightMode"] = mode.into();
            let current = height_prefab(p, &policy);
            assert!(current.unsupported.is_empty());
            assert!(current.raises.is_empty());
            assert_eq!(current.height_modifiers.as_ref().unwrap().len(), 1);
            assert_eq!(
                current.height_modifiers.as_ref().unwrap()[0].position,
                [3., 4., 5.]
            );
        }
        p.attributes[0].fields["HeightMode"] = 3.into();
        assert!(
            height_prefab(p, &policy)
                .unsupported
                .iter()
                .any(|s| s.contains("height mode"))
        );
        p.attributes[0].fields["HeightMode"] = 1.into();
        p.attributes.push(Attribute {
            collider: None,
            kind: "TerrainPlacementBlocked".into(),
            root: Some(false),
            local_to_root: None,
            position: [0.; 3],
            fields: serde_json::json!({"Opacity":1.,"Radius":2.,"Fade":0.}),
        });
        assert_eq!(material_prefab(p, &policy).raises.len(), 1);
        let current = height_prefab(p, &policy);
        assert!(current.raises.is_empty());
        assert_eq!(current.height_modifiers.unwrap().len(), 1);
        p.attributes.push(Attribute {
            collider: None,
            kind: "AddToHeightMap".into(),
            root: Some(false),
            local_to_root: None,
            position: [0.; 3],
            fields: serde_json::json!({"DestroyGameObject":1}),
        });
        assert!(
            height_prefab(p, &policy)
                .unsupported
                .iter()
                .any(|s| s.contains("AddToHeightMap"))
        );
    }
    #[test]
    fn material_adapter_accepts_authored_channels_but_keeps_other_required_effects_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let (mut source, _) = fixture(root.path());
        let p = source.prefabs.values_mut().next().unwrap();
        let policy = TerrainPolicy {
            material_removal: None,
            profile_id: "fixture".into(),
            profile_sha256: "a".repeat(64),
            native_sha256: "b".repeat(64),
            scene_sha256: "c".repeat(64),
            topology_flags: BTreeMap::from([("Protected".into(), 1), ("Decor".into(), 8)]),
            protected_topology: 1,
        };
        p.attributes = vec![Attribute {
            collider: None,
            kind: "TerrainSplatSet".into(),
            root: Some(false),
            local_to_root: None,
            position: [3., 0., 5.],
            fields: serde_json::json!({"Opacity":0.55,"Radius":150.,"Fade":45.,"SplatType":16}),
        }];
        assert!(!topology_prefab(p, &policy).unsupported.is_empty());
        let current = material_prefab(p, &policy);
        assert!(current.unsupported.is_empty());
        let modifiers = current.materials.unwrap();
        assert_eq!(modifiers.len(), 1);
        assert_eq!(modifiers[0].channel, 4);
        assert_eq!(modifiers[0].position, [3., 0., 5.]);
        for unknown in [0, 3, 256] {
            p.attributes[0].fields["SplatType"] = unknown.into();
            assert!(!material_prefab(p, &policy).unsupported.is_empty());
        }
        p.attributes[0].fields["SplatType"] = 16.into();
        p.attributes[0].fields["UnknownField"] = 1.into();
        assert!(!material_prefab(p, &policy).unsupported.is_empty());
        p.attributes[0]
            .fields
            .as_object_mut()
            .unwrap()
            .remove("UnknownField");
        p.attributes.push(Attribute {
            collider: None,
            kind: "TerrainBiomeSet".into(),
            root: Some(false),
            local_to_root: None,
            position: [0.; 3],
            fields: serde_json::json!({}),
        });
        assert!(
            material_prefab(p, &policy)
                .unsupported
                .iter()
                .any(|s| s.contains("TerrainBiomeSet"))
        );
        p.attributes.pop();
        p.attributes.push(Attribute {
            collider: None,
            kind: "TerrainHeightSet".into(),
            root: Some(false),
            local_to_root: None,
            position: [0.; 3],
            fields: serde_json::json!({"Opacity":1.,"Radius":1.,"Fade":0.,"HeightMode":0}),
        });
        assert!(
            material_prefab(p, &policy)
                .unsupported
                .iter()
                .any(|s| s.contains("TerrainHeightSet"))
        );
        p.attributes.pop();
        p.attributes[0].kind = "TerrainSplatRemove".into();
        assert!(
            material_prefab(p, &policy)
                .unsupported
                .iter()
                .any(|s| s.contains("installation-bound material removal policy"))
        );
        let removal = papyrus_terrain::decor_material::Removal {
            replacement_channels: [2, 4, 4, 1, 0],
            excluded_topology: 1,
        };
        let policy = TerrainPolicy {
            material_removal: Some(removal),
            ..policy
        };
        let compiled = material_prefab(p, &policy);
        assert!(compiled.unsupported.is_empty());
        assert!(matches!(compiled.materials.as_ref().unwrap()[0].operation,
            papyrus_terrain::decor_material::Operation::Remove(value) if value == removal));
    }

}
