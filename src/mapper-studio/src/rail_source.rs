//! Source-bound surface-rail stage contracts. Network/bed/mesh generation remains separate.
use crate::{
    infrastructure,
    installation::{self, SourceFile},
    prefab_assets::Catalog,
};
use papyrus_terrain::path_geometry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
const LIMIT: u64 = 4 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub enum Kind {
    GenerateRailRing,
    GenerateRailLayout,
    GenerateRailBranching,
    GenerateRailSiding,
    GenerateRailTerrain,
    GenerateRailTopology,
    GenerateRailTexture,
    GenerateRailMeshes,
    CollateTrainTracks,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub kind: Kind,
    pub object_id: i64,
    pub order: usize,
    pub mode: i32,
    pub disabled: Option<bool>,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
    pub minimum_world_size: Option<u32>,
    pub description: String,
    pub assets: Vec<Asset>,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub field: String,
    pub index: usize,
    pub file_id: u32,
    pub path_id: i64,
    pub external_path: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathPolicy {
    engine: String,
    horizontal_iterations: u16,
    vertical_iterations: u16,
    resample_distance: f32,
}
pub type CostPolicy = papyrus_terrain::rail_costmap::Policy;
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    format: String,
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage_layout: Option<String>,
    pub id: String,
    native_sha256: String,
    scene_sha256: String,
    reference_gameplay_sha256: String,
    code_sources: Vec<SourceFile>,
    server_realm: i32,
    pub rail_type: i32,
    topology_flags: BTreeMap<String, u32>,
    width: f32,
    inner_padding: f32,
    outer_padding: f32,
    inner_fade: f32,
    outer_fade: f32,
    terrain_offset: f32,
    splat_mask: u32,
    path: PathPolicy,
    pub costmap: CostPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<papyrus_terrain::rail_ring::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approaches: Option<papyrus_terrain::rail_approaches::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bed: Option<papyrus_terrain::rail_bed::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<crate::rail_mesh_stage::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branching: Option<papyrus_terrain::rail_branching::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub siding: Option<papyrus_terrain::rail_siding::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservations: Option<papyrus_terrain::rail_reservations::Policy>,
    cached_stages: Vec<Kind>,
    train_spline_offset: f32,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    sources: Vec<SourceFile>,
    code_sources: Vec<SourceFile>,
    catalog_sha256: String,
    infrastructure_sha256: String,
    profile_sha256: String,
    extractor_sha256: String,
    pub profile: Profile,
    pub stages: Vec<Stage>,
    #[serde(skip)]
    pub sha256: String,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn code_path(s: &str) -> bool {
    matches!(
        s,
        "GameAssembly.dll"
            | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            | "RustClient_Data/Managed/Assembly-CSharp.dll"
            | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
    )
}
impl Profile {
    pub fn mesh_width(&self) -> f32 {
        self.width
    }
    fn validate(&self, catalog: &Catalog) -> Result<(), String> {
        if let Some(reservations) = &self.reservations {
            reservations.validate()?;
        }
        if let Some(siding) = &self.siding {
            siding.validate()?;
        }
        if let Some(branching) = &self.branching {
            branching.validate()?;
        }
        if let Some(mesh) = &self.mesh {
            mesh.validate()?;
        }
        if let Some(ring) = &self.ring {
            ring.validate()?;
        }
        if let Some(approaches) = &self.approaches {
            approaches.validate()?;
        }
        if let Some(bed) = &self.bed {
            bed.validate()?;
            self.bed_shape()?;
        }
        let c = &self.costmap;
        let p = &self.path;
        let known = self.topology_flags.values().fold(0, |a, b| a | b);
        if self.format != "papyrus-rail-profile"
            || self.schema != 1
            || self
                .stage_layout
                .as_deref()
                .is_some_and(|layout| layout != "mode-description")
            || self.id.is_empty()
            || self.id.len() > 128
            || self.native_sha256 != catalog.installation.native.sha256
            || catalog
                .sources
                .get(2)
                .is_none_or(|s| s.sha256 != self.scene_sha256)
            || !hash(&self.reference_gameplay_sha256)
            || self.server_realm <= 0
            || self.server_realm.count_ones() != 1
            || self.rail_type < 0
            || self.rail_type > 64
            || self.topology_flags.is_empty()
            || self.topology_flags.len() > 32
            || self
                .topology_flags
                .iter()
                .any(|(k, v)| k.is_empty() || k.len() > 64 || v.count_ones() != 1)
            || self.topology_flags.values().collect::<BTreeSet<_>>().len()
                != self.topology_flags.len()
            || [
                "Rail", "Railside", "Cliff", "Ocean", "Monument", "Building", "Lake", "River",
            ]
            .iter()
            .any(|k| !self.topology_flags.contains_key(*k))
            || !self.width.is_finite()
            || !(0.1..=32.).contains(&self.width)
            || [
                self.inner_padding,
                self.outer_padding,
                self.inner_fade,
                self.outer_fade,
            ]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=128.).contains(v))
            || !self.terrain_offset.is_finite()
            || self.terrain_offset.abs() > 8.
            || self.splat_mask.count_ones() != 1
            || self.splat_mask > 255
            || p.engine != path_geometry::VERSION
            || p.horizontal_iterations > 256
            || p.vertical_iterations > 256
            || !p.resample_distance.is_finite()
            || !(0.1..=100.).contains(&p.resample_distance)
            || [c.spacing, c.near_radius, c.clearance_radius]
                .iter()
                .any(|v| !v.is_finite() || !(0.1..=128.).contains(v))
            || [
                c.blocked_topology,
                c.clearance_topology,
                c.water_topology,
                c.cliff_topology,
            ]
            .iter()
            .any(|v| *v == 0 || v & !known != 0)
            || !c.maximum_slope.is_finite()
            || !c.steep_slope.is_finite()
            || !c.moderate_slope.is_finite()
            || !(0. ..=90.).contains(&c.maximum_slope)
            || !(0. ..c.maximum_slope).contains(&c.steep_slope)
            || !(0. ..c.steep_slope).contains(&c.moderate_slope)
            || c.normal_cost == 0
            || c.normal_cost > c.moderate_cost
            || c.moderate_cost > c.steep_cost
            || c.steep_cost > 1000000
            || !self.train_spline_offset.is_finite()
            || self.train_spline_offset.abs() > 8.
            || self.cached_stages.len() > 9
            || self.cached_stages.iter().collect::<BTreeSet<_>>().len() != self.cached_stages.len()
        {
            return Err("Unsupported or unbound rail profile".into());
        }
        let mut paths = BTreeSet::new();
        if self.code_sources.is_empty()
            || self.code_sources.len() > 2
            || self.code_sources.iter().any(|s| {
                !code_path(&s.relative_path)
                    || s.bytes == 0
                    || s.bytes > 1024 * 1024 * 1024
                    || !hash(&s.sha256)
                    || !paths.insert(&s.relative_path)
            })
        {
            return Err("Invalid rail gameplay receipts".into());
        }
        Ok(())
    }
    pub fn path_settings(&self, maximum_work: usize) -> path_geometry::Settings {
        path_geometry::Settings {
            horizontal_iterations: self.path.horizontal_iterations,
            vertical_iterations: self.path.vertical_iterations,
            resample_distance: self.path.resample_distance,
            maximum_work,
        }
    }
    pub fn bed_shape(&self) -> Result<papyrus_terrain::rail_bed::Shape, String> {
        let flag = |name: &str| {
            self.topology_flags
                .get(name)
                .copied()
                .ok_or_else(|| format!("Rail bed source lacks topology role {name}"))
        };
        Ok(papyrus_terrain::rail_bed::Shape {
            width: self.width,
            inner_padding: self.inner_padding,
            outer_padding: self.outer_padding,
            inner_fade: self.inner_fade,
            outer_fade: self.outer_fade,
            terrain_offset: self.terrain_offset,
            splat_channel: self.splat_mask.trailing_zeros() as usize,
            rail: flag("Rail")?,
            railside: flag("Railside")?,
            cliff: flag("Cliff")?,
            eligible_railside: flag("Field")? | flag("Beach")? | flag("Forest")?,
            river: flag("River")?,
            bank: flag("Riverside")?,
            preserve_topology: flag("Monument")? | flag("Building")?,
        })
    }
}
impl Source {
    pub fn source_receipts(&self) -> &[SourceFile] {
        &self.sources
    }
    pub fn infrastructure_digest(&self) -> &str {
        &self.infrastructure_sha256
    }
    pub fn gameplay_sources(&self) -> &[SourceFile] {
        &self.code_sources
    }
    pub fn train_spline_offset(&self) -> f32 {
        self.profile.train_spline_offset
    }
    pub(crate) fn decode(
        raw: &[u8],
        catalog: &Catalog,
        infrastructure: &infrastructure::Source,
    ) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Rail source exceeds 4 MiB".into());
        }
        let mut doc: Self =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid rail source: {e}"))?;
        if doc.format != "papyrus-rail-source"
            || !matches!(
                (
                    doc.schema,
                    doc.adapter.as_str(),
                    doc.profile.stage_layout.as_deref()
                ),
                (1, "papyrus-surface-rail-stages-1", None)
                    | (2, "papyrus-surface-rail-stages-2", Some("mode-description"))
            )
            || doc.profile.stage_layout.as_deref() != infrastructure.stage_layout()
            || doc.catalog_sha256 != catalog.sha256
            || infrastructure.catalog_sha256 != catalog.sha256
            || doc.infrastructure_sha256 != infrastructure.sha256
            || !hash(&doc.infrastructure_sha256)
            || catalog.sources.len() != 3
            || doc.sources.len() != 4
            || doc.sources[..3] != catalog.sources
            || doc.sources[3].relative_path != "Bundles/maps/maps.bundle"
            || doc.sources[3].bytes == 0
            || doc.sources[3].bytes > 512 * 1024 * 1024
            || !hash(&doc.sources[3].sha256)
            || !hash(&doc.profile_sha256)
            || !hash(&doc.extractor_sha256)
            || doc.stages.is_empty()
            || doc.stages.len() > 64
            || doc.code_sources != doc.profile.code_sources
        {
            return Err("Rail source differs from verified catalog, connectors or adapter".into());
        }
        doc.profile.validate(catalog)?;
        let mut ids = BTreeSet::new();
        let mut kinds = BTreeSet::new();
        for (i, s) in doc.stages.iter().enumerate() {
            if s.order != i
                || (doc.schema == 1) != s.disabled.is_some()
                || s.object_id == 0
                || !ids.insert(s.object_id)
                || !kinds.insert(s.kind)
                || !matches!(s.mode, -1..=3)
                || s.description.is_empty()
                || s.description.len() > 256
                || s.minimum_world_size.is_some_and(|v| v > 6000)
                || (s.kind == Kind::GenerateRailRing) != s.minimum_world_size.is_some()
                || s.assets.len() > 32
                || s.kind != Kind::GenerateRailMeshes && !s.assets.is_empty()
            {
                return Err("Invalid rail stage coverage or ordering".into());
            }
            let mut resources = BTreeSet::new();
            for a in &s.assets {
                if !matches!(
                    a.field.as_str(),
                    "RailMesh" | "RailMeshes" | "RailMaterial" | "RailPhysicMaterial"
                ) || a.index >= 16
                    || a.field != "RailMeshes" && a.index != 0
                    || a.path_id == 0
                    || a.file_id > 64
                    || (a.file_id == 0) != a.external_path.is_empty()
                    || a.external_path.len() > 512
                    || a.external_path.contains(['\0', '\r', '\n'])
                    || !resources.insert((&a.field, a.index))
                {
                    return Err("Invalid unresolved rail resource handle".into());
                }
            }
        }
        doc.sha256 = format!("{:x}", Sha256::digest(raw));
        Ok(doc)
    }
    /// Inputs are already installation-verified catalog and connector snapshots.
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        infrastructure: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Rail source verification cancelled".into());
        }
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let doc = Self::decode(&raw, catalog, infrastructure)?;
        if installation::hash_file(
            root,
            "Bundles/maps/maps.bundle",
            512 * 1024 * 1024,
            &cancelled,
        )? != doc.sources[3]
        {
            return Err("Rail map scene differs from selected installation".into());
        }
        for receipt in &doc.code_sources {
            if installation::hash_file(
                root,
                &receipt.relative_path,
                1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Rail gameplay code differs from selected installation".into());
            }
        }
        if cancelled() {
            return Err("Rail source verification cancelled".into());
        }
        Ok(doc)
    }
    pub fn skip(&self, s: &Stage, size: u32, cached: bool, rails: bool) -> Option<&'static str> {
        if s.disabled == Some(true) {
            Some("disabled in source")
        } else if s.mode & self.profile.server_realm == 0 {
            Some("not in server realm")
        } else if cached && !self.profile.cached_stages.contains(&s.kind) {
            Some("not run on cached worlds")
        } else if s.minimum_world_size.is_some_and(|min| size < min) {
            Some("below source minimum world size")
        } else if !rails && matches!(s.kind, Kind::GenerateRailRing | Kind::GenerateRailLayout) {
            Some("AboveGroundRails option disabled")
        } else {
            None
        }
    }
    pub fn audit(
        &self,
        infrastructure: &infrastructure::Source,
        size: u32,
        cached: bool,
        rails: bool,
    ) -> Result<serde_json::Value, String> {
        if infrastructure.sha256 != self.infrastructure_sha256 {
            return Err("Rail connector source changed during inspection".into());
        }
        let sockets=infrastructure.prefabs.iter().filter_map(|(path,p)| {
            let count=p.connectors.iter().filter(|c|c.infrastructure_type==self.profile.rail_type).count();
            (count>0).then(||serde_json::json!({"prefab":path,"connectors":count,"complete_metadata":p.complete}))
        }).collect::<Vec<_>>();
        Ok(
            serde_json::json!({"source_sha256":self.sha256,"profile":self.profile,"size":size,"cached_world":cached,"assume_above_ground_rails":rails,
            "generation_ready":false,"resource_buffers_decoded":false,"stages":self.stages.iter().map(|s|serde_json::json!({"kind":s.kind,"order":s.order,"minimum_world_size":s.minimum_world_size,"server_selected":self.skip(s,size,cached,rails).is_none(),"skip":self.skip(s,size,cached,rails),"mode":s.mode,"disabled":s.disabled,"component_enabled":s.component_enabled,"hierarchy_active":s.hierarchy_active,"assets":s.assets})).collect::<Vec<_>>(),
            "connector_prefabs":sockets,"include_inactive_connectors":true,
            "pending":["Ring/connection graph routing, branches and sidings","Terrain bed, topology and material passes","Resource decoding, rail meshes, spline collation and train traversal","Rail-side monuments, physical collision and game validation"]}),
        )
    }
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an owned installation and captured rail sources"]
    fn owned_current_rail_stage_and_mesh_capture_loads_through_verified_readers() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_RAIL_CAPTURE_TEST_ROOT");
        let _verification = installation::VerificationScope::default();
        let installation = installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_RAIL_CAPTURE_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let infrastructure = infrastructure::Source::load(
            &path("MAPPER_RAIL_CAPTURE_TEST_INFRASTRUCTURE"),
            &catalog,
            || false,
        )
        .unwrap();
        let source = Source::load(
            &root,
            &path("MAPPER_RAIL_CAPTURE_TEST_SOURCE"),
            &catalog,
            &infrastructure,
            || false,
        )
        .unwrap();
        let meshes = crate::rail_mesh_source::Source::load(
            &root,
            &path("MAPPER_RAIL_CAPTURE_TEST_MESHES"),
            &source,
            || false,
        )
        .unwrap();
        assert_eq!(source.schema, 2);
        assert_eq!(source.stages.len(), 9);
        assert!(source.stages.iter().all(|stage| stage.disabled.is_none()));
        assert_eq!(meshes.meshes.len(), 3);
        assert_eq!(meshes.parent_digest(), source.sha256);
        std::fs::write(
            path("MAPPER_RAIL_CAPTURE_TEST_REPORT"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"format":"mapper-owned-current-rail-capture-verification",
                "schema":1,"source_sha256":source.sha256,"mesh_sha256":meshes.sha256,
                "stage_count":source.stages.len(),"stage_layout":source.profile.stage_layout,
                "meshes":meshes.audit(),"numeric_generation_policy_reviewed":false,
                "generation_executed":false,"game_runtime_validated":false}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    pub(crate) fn fixture() -> (
        tempfile::TempDir,
        Catalog,
        infrastructure::Source,
        serde_json::Value,
    ) {
        let root = tempfile::tempdir().unwrap();
        let (catalog, mut infra) = infrastructure::tests::fixture();
        let prefab = catalog.entries.keys().next().unwrap();
        infra["prefabs"][prefab]["connectors"][0]["infrastructureType"] = 6.into();
        infra["prefabs"][prefab]["connectors"][0]["hierarchyActiveWithoutRoot"] = false.into();
        let path = root.path().join("infrastructure.json");
        std::fs::write(&path, serde_json::to_vec(&infra).unwrap()).unwrap();
        let infrastructure = infrastructure::Source::load(&path, &catalog, || false).unwrap();
        std::fs::create_dir_all(root.path().join("Bundles/maps")).unwrap();
        std::fs::write(root.path().join("Bundles/maps/maps.bundle"), b"maps").unwrap();
        std::fs::write(root.path().join("GameAssembly.dll"), b"gameplay").unwrap();
        let code =
            installation::hash_file(root.path(), "GameAssembly.dll", 1024, &|| false).unwrap();
        let mut profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rails/client-0f703d43.json"
        ))
        .unwrap();
        profile["native_sha256"] = "a".repeat(64).into();
        profile["scene_sha256"] = "c".repeat(64).into();
        profile["code_sources"] = serde_json::json!([code]);
        let mut sources = catalog.sources.clone();
        sources.push(
            installation::hash_file(root.path(), "Bundles/maps/maps.bundle", 1024, &|| false)
                .unwrap(),
        );
        let kinds = [
            Kind::GenerateRailRing,
            Kind::GenerateRailLayout,
            Kind::GenerateRailBranching,
            Kind::GenerateRailSiding,
            Kind::GenerateRailTerrain,
            Kind::GenerateRailTopology,
            Kind::GenerateRailTexture,
            Kind::GenerateRailMeshes,
            Kind::CollateTrainTracks,
        ];
        let stages = kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Stage {
                kind,
                object_id: i as i64 + 1,
                order: i,
                mode: if kind == Kind::CollateTrainTracks {
                    2
                } else {
                    -1
                },
                disabled: Some(false),
                component_enabled: false,
                hierarchy_active: false,
                minimum_world_size: (kind == Kind::GenerateRailRing).then_some(4250),
                description: format!("{kind:?}"),
                assets: vec![],
            })
            .collect::<Vec<_>>();
        let doc = serde_json::json!({"format":"papyrus-rail-source","schema":1,"adapter":"papyrus-surface-rail-stages-1","sources":sources,"code_sources":[code],"catalog_sha256":catalog.sha256,"infrastructure_sha256":infrastructure.sha256,"profile_sha256":"d".repeat(64),"extractor_sha256":"e".repeat(64),"profile":profile,"stages":stages});
        (root, catalog, infrastructure, doc)
    }
    #[test]
    fn actual_server_realm_and_cache_policy_do_not_depend_on_unity_enable_state() {
        let (_root, catalog, infrastructure, doc) = fixture();
        let source = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        let fresh = source.audit(&infrastructure, 4000, false, true).unwrap();
        assert_eq!(
            fresh["stages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["server_selected"] == true)
                .count(),
            8
        );
        assert_eq!(
            fresh["stages"][0]["skip"],
            "below source minimum world size"
        );
        assert_eq!(fresh["stages"][8]["server_selected"], true);
        assert_eq!(fresh["connector_prefabs"][0]["connectors"], 1);
        assert_eq!(
            source.audit(&infrastructure, 4500, false, true).unwrap()["stages"][0]["server_selected"],
            true
        );
        let cached = source.audit(&infrastructure, 4500, true, true).unwrap();
        assert_eq!(
            cached["stages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["server_selected"] == true)
                .count(),
            2
        );
        assert_eq!(
            source.audit(&infrastructure, 4500, false, false).unwrap()["stages"][1]["skip"],
            "AboveGroundRails option disabled"
        );
        let mut disabled = doc.clone();
        disabled["stages"][8]["disabled"] = true.into();
        let disabled = Source::decode(
            &serde_json::to_vec(&disabled).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        assert_eq!(
            disabled.audit(&infrastructure, 4500, true, true).unwrap()["stages"][8]["server_selected"],
            false
        );
    }
    #[test]
    fn current_stage_layout_is_bound_to_its_parent_and_legacy_boolean_records_stay_legacy() {
        let (_root, catalog, infrastructure, mut doc) = fixture();
        let legacy = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&legacy.stages).unwrap(), doc["stages"]);
        assert!(
            serde_json::to_value(&legacy.profile)
                .unwrap()
                .get("stage_layout")
                .is_none()
        );
        let mut parent = serde_json::to_value(&infrastructure).unwrap();
        parent["schema"] = 2.into();
        parent["adapter"] = "papyrus-path-connectors-2".into();
        parent["profile"]["stage_layout"] = "mode-description".into();
        let mut current: infrastructure::Source = serde_json::from_value(parent).unwrap();
        current.sha256 = infrastructure.sha256.clone();
        doc["schema"] = 2.into();
        doc["adapter"] = "papyrus-surface-rail-stages-2".into();
        doc["profile"]["stage_layout"] = "mode-description".into();
        for stage in doc["stages"].as_array_mut().unwrap() {
            stage["disabled"] = serde_json::Value::Null;
        }
        let decode = |doc: &serde_json::Value| {
            Source::decode(&serde_json::to_vec(doc).unwrap(), &catalog, &current)
        };
        let source = decode(&doc).unwrap();
        assert!(source.stages.iter().all(|s| s.disabled.is_none()));
        assert_eq!(
            source.audit(&current, 4500, false, true).unwrap()["stages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["server_selected"] == true)
                .count(),
            9
        );
        assert!(
            Source::decode(
                &serde_json::to_vec(&doc).unwrap(),
                &catalog,
                &infrastructure
            )
            .is_err()
        );
        for (pointer, value) in [
            ("/schema", serde_json::json!(1)),
            ("/schema", serde_json::json!(3)),
            (
                "/adapter",
                serde_json::json!("papyrus-surface-rail-stages-1"),
            ),
            ("/profile/stage_layout", serde_json::Value::Null),
            ("/profile/stage_layout", serde_json::json!("future")),
            ("/stages/0/disabled", serde_json::json!(false)),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&bad).is_err(), "accepted {pointer}");
        }
        let mut old = serde_json::to_value(&legacy.stages).unwrap();
        old[0]["disabled"] = serde_json::Value::Null;
        let mut old_doc = doc;
        old_doc["schema"] = 1.into();
        old_doc["adapter"] = "papyrus-surface-rail-stages-1".into();
        old_doc["profile"]
            .as_object_mut()
            .unwrap()
            .remove("stage_layout");
        old_doc["stages"] = old;
        assert!(
            Source::decode(
                &serde_json::to_vec(&old_doc).unwrap(),
                &catalog,
                &infrastructure
            )
            .is_err()
        );
    }
    #[test]
    fn stage_profile_order_and_gameplay_receipts_are_validated_against_the_selected_installation() {
        let (root, catalog, infrastructure, doc) = fixture();
        let path = root.path().join("rails.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(Source::load(root.path(), &path, &catalog, &infrastructure, || false).is_ok());
        assert!(Source::load(root.path(), &path, &catalog, &infrastructure, || true).is_err());
        for (pointer, value) in [
            ("/catalog_sha256", serde_json::json!("f".repeat(64))),
            ("/infrastructure_sha256", serde_json::json!("f".repeat(64))),
            ("/stages/0/order", serde_json::json!(5)),
            ("/stages/1/kind", serde_json::json!("GenerateRailUnknown")),
            ("/profile/path/engine", serde_json::json!("unknown-path")),
            ("/profile/ring/engine", serde_json::json!("unknown-ring")),
            ("/profile/bed/engine", serde_json::json!("unknown-bed")),
            (
                "/profile/approaches/engine",
                serde_json::json!("unknown-approaches"),
            ),
            (
                "/profile/costmap/blocked_topology",
                serde_json::json!(2147483648_u32),
            ),
            ("/profile/splat_mask", serde_json::json!(3)),
            (
                "/code_sources/0/relative_path",
                serde_json::json!("../GameAssembly.dll"),
            ),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                Source::decode(
                    &serde_json::to_vec(&changed).unwrap(),
                    &catalog,
                    &infrastructure
                )
                .is_err(),
                "accepted {pointer}"
            );
        }
        std::fs::write(root.path().join("GameAssembly.dll"), b"different code").unwrap();
        assert!(
            Source::load(root.path(), &path, &catalog, &infrastructure, || false)
                .err()
                .unwrap()
                .contains("gameplay")
        );
        std::fs::write(
            root.path().join("Bundles/maps/maps.bundle"),
            b"different scene",
        )
        .unwrap();
        assert!(
            Source::load(root.path(), &path, &catalog, &infrastructure, || false)
                .err()
                .unwrap()
                .contains("map scene")
        );
    }
    #[test]
    fn older_snapshots_do_not_inherit_new_ring_policy() {
        let (_root, catalog, infrastructure, mut doc) = fixture();
        doc["profile"].as_object_mut().unwrap().remove("ring");
        doc["profile"].as_object_mut().unwrap().remove("approaches");
        doc["profile"].as_object_mut().unwrap().remove("bed");
        let source = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        assert!(source.profile.ring.is_none());
        assert!(source.profile.approaches.is_none());
        assert!(source.profile.bed.is_none());
        assert!(source.profile.mesh.is_none());
        assert!(source.audit(&infrastructure, 4500, false, true).is_ok());
    }
    #[test]
    fn mesh_stage_policy_is_explicit_and_unknown_transform_work_is_rejected() {
        let (_root, catalog, infrastructure, mut doc) = fixture();
        assert!(
            Source::decode(
                &serde_json::to_vec(&doc).unwrap(),
                &catalog,
                &infrastructure
            )
            .unwrap()
            .profile
            .mesh
            .is_none()
        );
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rails/client-0f703d43-meshes.json"
        ))
        .unwrap();
        doc["profile"]["mesh"] = profile["mesh"].clone();
        assert!(
            Source::decode(
                &serde_json::to_vec(&doc).unwrap(),
                &catalog,
                &infrastructure
            )
            .unwrap()
            .profile
            .mesh
            .is_some()
        );
        doc["profile"]["mesh"]["random_scale"] = 0.5.into();
        assert!(
            Source::decode(
                &serde_json::to_vec(&doc).unwrap(),
                &catalog,
                &infrastructure
            )
            .is_err()
        );
    }
    #[test]
    fn approach_bridge_keeps_missing_networks_and_source_mismatches_explicit() {
        let (_root, catalog, infrastructure, doc) = fixture();
        let source = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        let height = [16000i16; 4];
        let topo = [0i32; 4];
        let placement = [0u8; 4];
        let grid = papyrus_terrain::rail_costmap::build(
            papyrus_terrain::rail_costmap::Terrain {
                size: 1000.,
                height_resolution: 2,
                height: &height,
                topology_resolution: 2,
                topology: &topo,
                placement_resolution: 2,
                placement: &placement,
            },
            &source.profile.costmap,
            128000000,
            || false,
        )
        .unwrap()
        .unwrap();
        let mut sockets = crate::rail_preview::Sockets {
            layout_sha256: "a".repeat(64),
            infrastructure_sha256: infrastructure.sha256.clone(),
            sockets: vec![],
            generated: None,
        };
        let ring = papyrus_terrain::rail_ring::Outcome::Unresolved {
            reason: "fixture",
            route_attempts: 0,
            expansions: 0,
            waypoint_moves: 0,
        };
        let settings = papyrus_terrain::rail_approaches::Settings {
            seed: 7,
            maximum_grade: 1.,
            maximum_total_expansions: 1000,
            maximum_geometry_work: 1000000,
        };
        let report =
            crate::rail_preview::approach_report(&grid, &source, &sockets, &ring, settings, || {
                false
            })
            .unwrap();
        assert_eq!(report["result"]["status"], "unresolved");
        assert_eq!(report["input_socket_count"], 0);
        assert_eq!(report["generation_ready"], false);
        assert_eq!(report["socket_connections_validated"], false);
        assert!(
            crate::rail_preview::approach_report(&grid, &source, &sockets, &ring, settings, || {
                true
            })
            .is_err()
        );
        sockets.infrastructure_sha256 = "f".repeat(64);
        assert!(
            crate::rail_preview::approach_report(&grid, &source, &sockets, &ring, settings, || {
                false
            })
            .is_err()
        );
    }
    #[test]
    fn prepared_ring_curve_preserves_approach_reports_and_work_accounting() {
        let (_root, catalog, infrastructure, doc) = fixture();
        let source = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        let grid = papyrus_terrain::rail_costmap::build(
            papyrus_terrain::rail_costmap::Terrain {
                size: 1000.,
                height_resolution: 2,
                height: &[19660; 4],
                topology_resolution: 2,
                topology: &[0; 4],
                placement_resolution: 2,
                placement: &[0; 4],
            },
            &source.profile.costmap,
            128000000,
            || false,
        )
        .unwrap()
        .unwrap();
        let cells = (40..60)
            .map(|x| [x, 40])
            .chain((40..60).map(|z| [60, z]))
            .chain((41..=60).rev().map(|x| [x, 60]))
            .chain((41..=60).rev().map(|z| [40, z]))
            .chain([[40, 40]])
            .collect::<Vec<_>>();
        let points = cells
            .iter()
            .map(|&cell| grid.point(cell).unwrap())
            .collect::<Vec<_>>();
        let ring = papyrus_terrain::rail_ring::Outcome::Found {
            waypoints: cells.clone(),
            cells,
            points: points.clone(),
            route_attempts: 0,
            expansions: 0,
            waypoint_moves: 0,
        };
        let sockets = crate::rail_preview::Sockets {
            layout_sha256: "a".repeat(64),
            infrastructure_sha256: infrastructure.sha256.clone(),
            sockets: vec![],
            generated: None,
        };
        let mut settings = papyrus_terrain::rail_approaches::Settings {
            seed: 7,
            maximum_grade: 1.,
            maximum_total_expansions: 1000,
            maximum_geometry_work: 1000000,
        };
        let curve = papyrus_terrain::path_geometry::build(
            &points,
            source.profile.path_settings(settings.maximum_geometry_work),
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        let combined =
            crate::rail_preview::approach_report(&grid, &source, &sockets, &ring, settings, || {
                false
            })
            .unwrap();
        let split = crate::rail_preview::approach_report_prepared(
            &grid,
            &source,
            &sockets,
            &ring,
            settings,
            Some(crate::rail_preview::ApproachCurve::Generated(&curve)),
            || false,
        )
        .unwrap();
        assert_eq!(combined, split);
        assert_eq!(combined["result"]["status"], "proposed");
        settings.maximum_geometry_work = curve.work();
        let exhausted = crate::rail_preview::approach_report_prepared(
            &grid,
            &source,
            &sockets,
            &ring,
            settings,
            Some(crate::rail_preview::ApproachCurve::Generated(&curve)),
            || false,
        )
        .unwrap();
        assert_eq!(exhausted["result"]["status"], "budget_exceeded");
        assert!(
            crate::rail_preview::approach_report_prepared(
                &grid,
                &source,
                &sockets,
                &ring,
                settings,
                Some(crate::rail_preview::ApproachCurve::Generated(&curve)),
                || true
            )
            .is_err()
        );
    }
}
