//! Immutable, installation-bound road templates. Game geometry remains local.
use crate::{
    infrastructure,
    installation::{self, SourceFile},
    rail_mesh_source::Mesh,
    rail_source::Asset,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Read, path::Path};
const LIMIT: u64 = 32 * 1024 * 1024;
pub const ENGINE: &str = "papyrus-road-surface-1";
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub chunk_repetitions: usize,
    pub maximum_hierarchy_exclusive: u32,
    pub spline: bool,
    pub snap_to_terrain: bool,
    pub snap_open_endpoints: bool,
    pub normal_smoothing: f32,
    pub random_scale: f32,
    pub mesh_offset: f32,
    pub noise_export: String,
    pub noise_frequency: f32,
    pub noise_unsigned_scale: f32,
    pub noise_unsigned_bias: f32,
    pub scale_width_with_length: bool,
    pub top_aligned: bool,
    pub round_vertices: u32,
}

impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != ENGINE
            || !(1..=32).contains(&self.chunk_repetitions)
            || !(1..=4).contains(&self.maximum_hierarchy_exclusive)
            || !self.spline
            || self.normal_smoothing != 0.
            || self.scale_width_with_length
            || self.round_vertices != 0
            || self.noise_export != "snoise1_32"
            || !self.random_scale.is_finite()
            || !(0.01..=8.).contains(&self.random_scale)
            || !self.mesh_offset.is_finite()
            || self.mesh_offset.abs() > 32.
            || !self.noise_frequency.is_finite()
            || !(0.000001..=1.).contains(&self.noise_frequency)
            || [self.noise_unsigned_scale, self.noise_unsigned_bias]
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 8.)
        {
            return Err("Unsupported source road surface policy".into());
        }
        Ok(())
    }
    pub fn radius(&self, width: f32, noise: f32) -> Option<f32> {
        if !width.is_finite() || !(0.1..=128.).contains(&width) || !noise.is_finite() {
            return None;
        }
        let radius = width * 0.5;
        let factor = (noise * self.noise_unsigned_scale + self.noise_unsigned_bias).clamp(0., 1.);
        let value = radius + (radius * self.random_scale - radius) * factor;
        (value.is_finite() && value > 0. && value <= 128.).then_some(value)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    #[serde(default)]
    stage_layout: Option<String>,
    native_sha256: String,
    scene_sha256: String,
    code_sources: Vec<SourceFile>,
    map_scene: SourceFile,
    policy: Policy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    object_id: i64,
    order: usize,
    mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
    lod_array_count: usize,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    infrastructure_sha256: String,
    sources: Vec<SourceFile>,
    code_sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    profile: Profile,
    mesh_stage: Stage,
    pub meshes: Vec<Mesh>,
    #[serde(skip)]
    pub sha256: String,
}
pub fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn extraction_profile(
    parent: &infrastructure::Source,
) -> Result<Option<&'static str>, String> {
    for raw in [
        include_str!("../../../compatibility/infrastructure/client-0f703d43-surfaces.json"),
        include_str!("../../../compatibility/infrastructure/client-8dd4e235-surfaces.json"),
        include_str!("../../../compatibility/infrastructure/client-58f3263f-surfaces.json"),
    ] {
        let profile: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        profile.policy.validate()?;
        if parent
            .source_receipts()
            .first()
            .is_some_and(|r| r.sha256 == profile.native_sha256)
            && parent
                .source_receipts()
                .get(2)
                .is_some_and(|r| r.sha256 == profile.scene_sha256)
            && parent.gameplay_receipts() == profile.code_sources
            && parent.map_receipt() == Some(&profile.map_scene)
            && parent.stage_layout() == profile.stage_layout.as_deref()
        {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}
impl Source {
    pub fn verify_installation(
        &self,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        for receipt in self
            .sources
            .iter()
            .chain(&self.code_sources)
            .chain(&self.extra_sources)
        {
            if crate::installation::hash_file(
                root,
                &receipt.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Road mesh source changed in the selected installation".into());
            }
        }
        if cancelled() {
            return Err("Road mesh verification cancelled".into());
        }
        Ok(())
    }
    pub fn policy(&self) -> &Policy {
        &self.profile.policy
    }
    pub fn parent_digest(&self) -> &str {
        &self.infrastructure_sha256
    }
    fn decode(
        raw: &[u8],
        parent: &infrastructure::Source,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        if parent.source_receipts().len() != 3
            || !(1..=2).contains(&parent.gameplay_receipts().len())
            || !hash(&parent.sha256)
        {
            return Err("Invalid parent road source receipts".into());
        }
        for (index, receipt) in parent.source_receipts().iter().enumerate() {
            let valid = match index {
                0 => matches!(
                    receipt.relative_path.as_str(),
                    "RustClient_Data/Plugins/x86_64/RustNative.dll"
                        | "RustDedicated_Data/Plugins/x86_64/RustNative.dll"
                ),
                1 => receipt.relative_path == "Bundles/AssetSceneManifest.json",
                _ => receipt.relative_path == "Bundles/shared/assetscenes.bundle",
            };
            if !valid || !hash(&receipt.sha256) || receipt.bytes == 0 {
                return Err("Invalid parent road resource path".into());
            }
        }
        if raw.len() as u64 > LIMIT {
            return Err("Road mesh source exceeds 32 MiB".into());
        }
        let mut doc: Self =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid road mesh source: {e}"))?;
        let map = parent
            .map_receipt()
            .ok_or("Road meshes need source-bound map scene receipts")?;
        if map.relative_path != "Bundles/maps/maps.bundle" {
            return Err("Invalid road map scene path".into());
        }
        if parent.gameplay_receipts().iter().any(|r| {
            !matches!(
                r.relative_path.as_str(),
                "GameAssembly.dll"
                    | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
                    | "RustClient_Data/Managed/Assembly-CSharp.dll"
                    | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
            ) || !hash(&r.sha256)
                || r.bytes == 0
                || r.bytes > 1024 * 1024 * 1024
        }) {
            return Err("Invalid road gameplay receipts".into());
        }
        let mut expected = parent.source_receipts().to_vec();
        expected.push(map.clone());
        let current = match parent.stage_layout() {
            None => false,
            Some("mode-description") => true,
            _ => return Err("Unsupported parent road mesh activation layout".into()),
        };
        if doc.format != "papyrus-road-mesh-source"
            || (doc.schema, doc.adapter.as_str())
                != if current {
                    (2, "papyrus-road-mesh-buffers-2")
                } else {
                    (1, "papyrus-road-mesh-buffers-1")
                }
            || doc.profile.stage_layout.as_deref() != parent.stage_layout()
            || doc.infrastructure_sha256 != parent.sha256
            || !hash(&doc.extractor_sha256)
            || doc.sources != expected
            || doc.code_sources != parent.gameplay_receipts()
            || doc.profile.format != "papyrus-road-surface-profile"
            || doc.profile.schema != 1
            || doc.profile.id.is_empty()
            || doc.profile.id.len() > 64
            || doc.profile.native_sha256 != expected[0].sha256
            || doc.profile.scene_sha256 != expected[2].sha256
            || doc.profile.code_sources != doc.code_sources
            || doc.profile.map_scene != *map
        {
            return Err("Road mesh source differs from its installation or road recipe".into());
        }
        doc.profile.policy.validate()?;
        let stage = &doc.mesh_stage;
        if stage.object_id == 0
            || stage.order != 0
            || stage.mode != -1
            || stage.disabled != if current { None } else { Some(false) }
            || !stage.component_enabled
            || !stage.hierarchy_active
            || stage.assets.len() > 21
            || !(1..=16).contains(&doc.meshes.len())
        {
            return Err("Unsupported road mesh scene stage".into());
        }
        let field = if stage.lod_array_count == 0 {
            "RoadMesh"
        } else {
            "RoadMeshes"
        };
        let refs: Vec<_> = stage.assets.iter().filter(|a| a.field == field).collect();
        if refs.len() != doc.meshes.len()
            || (stage.lod_array_count != 0 && stage.lod_array_count != refs.len())
        {
            return Err("Road mesh LOD roster mismatch".into());
        }
        let mut seen = BTreeSet::new();
        for a in &stage.assets {
            if !matches!(
                a.field.as_str(),
                "RoadMesh"
                    | "RoadMeshes"
                    | "RoadMaterial"
                    | "RoadRingMaterial"
                    | "RoadPhysicMaterial"
            ) || a.path_id == 0
                || a.index >= 16
                || !seen.insert((&a.field, a.index))
                || (a.file_id == 0) != a.external_path.is_empty()
                || (!a.external_path.is_empty()
                    && (!a.external_path.starts_with("archive:/") || a.external_path.len() > 512))
            {
                return Err("Invalid road mesh resource reference".into());
            }
        }
        let (mut vertices, mut faces) = (0, 0);
        for (i, mesh) in doc.meshes.iter().enumerate() {
            if cancelled() {
                return Err("Road mesh loading cancelled".into());
            }
            if &mesh.reference != refs[i]
                || mesh.reference.index != i
                || mesh.name.is_empty()
                || mesh.name.len() > 512
            {
                return Err("Road mesh template order mismatch".into());
            }
            mesh.buffer.validate()?;
            vertices += mesh.buffer.vertices.len();
            faces += mesh.buffer.triangles.len();
        }
        if vertices > 262144 || faces > 524288 || doc.extra_sources.len() > 3 {
            return Err("Road mesh aggregate exceeds bounds".into());
        }
        let mut paths = BTreeSet::new();
        for receipt in &doc.extra_sources {
            if !matches!(
                receipt.relative_path.as_str(),
                "Bundles/shared/content.bundle" | "Bundles/shared/monuments.bundle"
            ) || !paths.insert(&receipt.relative_path)
                || !hash(&receipt.sha256)
                || receipt.bytes == 0
                || receipt.bytes > 8 * 1024 * 1024 * 1024
            {
                return Err("Invalid road mesh bundle receipt".into());
            }
        }
        doc.sha256 = format!("{:x}", Sha256::digest(raw));
        Ok(doc)
    }
    pub fn read(
        path: &Path,
        parent: &infrastructure::Source,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        Self::decode(&raw, parent, cancelled)
    }
    pub fn discover(
        parent: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let root = crate::storage::local_data()?.join("papyrus-road-mesh-cache");
        if !root.is_dir() {
            return Ok(None);
        }
        let mut best: Option<Self> = None;
        for (i, entry) in std::fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i >= 256 {
                return Err("Too many road mesh snapshots".into());
            }
            if cancelled() {
                return Ok(None);
            }
            let path = entry.map_err(|e| e.to_string())?.path().join("meshes.json");
            if let Ok(doc) = Self::read(&path, parent, &cancelled)
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    == Some(&doc.sha256)
                && best.as_ref().is_none_or(|old| doc.sha256 > old.sha256)
            {
                best = Some(doc);
            }
        }
        Ok(best)
    }
    pub fn cached(
        root: &Path,
        digest: &str,
        parent: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if !hash(digest) {
            return Err("Invalid road surface fingerprint".into());
        }
        let path = crate::client_cache::pinned_document(
            &crate::storage::local_data()?,
            "papyrus-road-mesh-cache",
            digest,
            "meshes.json",
        )?;
        let doc = Self::read(&path, parent, &cancelled)?;
        if doc.sha256 != digest {
            return Err("Road surface snapshot changed".into());
        }
        for receipt in doc
            .sources
            .iter()
            .chain(&doc.code_sources)
            .chain(&doc.extra_sources)
        {
            if installation::hash_file(
                root,
                &receipt.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Road surface installation changed".into());
            }
        }
        Ok(doc)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::{Value, json};

    pub(crate) fn fixture() -> (infrastructure::Source, Value) {
        let (_, mut parent) = infrastructure::tests::fixture();
        let profile: Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-surfaces.json"
        ))
        .unwrap();
        let code = json!([{"relative_path":"GameAssembly.dll","sha256":"f".repeat(64),"bytes":1}]);
        let map =
            json!({"relative_path":"Bundles/maps/maps.bundle","sha256":"1".repeat(64),"bytes":1});
        parent["profile"]["code_sources"] = code.clone();
        parent["profile"]["map_scene"] = map.clone();
        let mut parent: infrastructure::Source = serde_json::from_value(parent).unwrap();
        parent.sha256 = "2".repeat(64);
        let asset = json!({"field":"RoadMeshes","index":0,"file_id":2,"path_id":42,"external_path":"archive:/test/test"});
        let mut sources = serde_json::to_value(parent.source_receipts())
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        sources.push(map.clone());
        let doc = json!({"format":"papyrus-road-mesh-source","schema":1,"adapter":"papyrus-road-mesh-buffers-1",
            "infrastructure_sha256":parent.sha256,"sources":sources,"code_sources":code,"extra_sources":[],"extractor_sha256":"3".repeat(64),
            "profile":{"format":"papyrus-road-surface-profile","schema":1,"id":"synthetic","native_sha256":"a".repeat(64),"scene_sha256":"c".repeat(64),"code_sources":code,"map_scene":map,"policy":profile["policy"]},
            "mesh_stage":{"object_id":1,"order":0,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,"lod_array_count":1,"assets":[asset]},
            "meshes":[{"reference":asset,"name":"synthetic road","buffer":{"vertices":[[0,0,0],[1,0,0],[0,0,2]],"normals":[[0,1,0],[0,1,0],[0,1,0]],"tangents":[[1,0,0,-1],[1,0,0,-1],[1,0,0,-1]],"uv":[[0,0],[1,0],[0,1]],"triangles":[[0,1,2]],"center":[0.5,0,1],"extents":[0.5,0,1]}}]});
        (parent, doc)
    }
    #[test]
    fn current_mesh_sources_require_matching_headers_and_explicit_absent_disabled() {
        let (parent, mut doc) = fixture();
        let mut value = serde_json::to_value(&parent).unwrap();
        value["profile"]["stage_layout"] = "mode-description".into();
        let mut current: infrastructure::Source = serde_json::from_value(value).unwrap();
        current.sha256 = parent.sha256.clone();
        doc["schema"] = 2.into();
        doc["adapter"] = "papyrus-road-mesh-buffers-2".into();
        doc["profile"]["stage_layout"] = "mode-description".into();
        doc["mesh_stage"]["disabled"] = Value::Null;
        let decode = |value: &Value| {
            Source::decode(&serde_json::to_vec(value).unwrap(), &current, &|| false)
        };
        decode(&doc).unwrap();
        assert!(Source::decode(&serde_json::to_vec(&doc).unwrap(), &parent, &|| false).is_err());
        for (pointer, value) in [
            ("/schema", json!(1)),
            ("/adapter", json!("papyrus-road-mesh-buffers-1")),
            ("/profile/stage_layout", json!("future")),
            ("/mesh_stage/disabled", json!(false)),
            ("/mesh_stage/disabled", json!(true)),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&bad).is_err(), "{pointer}");
        }
        doc["mesh_stage"]
            .as_object_mut()
            .unwrap()
            .remove("disabled");
        assert!(decode(&doc).is_err());
    }
    #[test]
    fn source_rejects_mismatched_installations_unsupported_policies_and_bad_geometry() {
        let (parent, doc) = fixture();
        let decode = |v: &Value, cancel| {
            Source::decode(&serde_json::to_vec(v).unwrap(), &parent, &|| cancel)
        };
        let source = decode(&doc, false).unwrap();
        assert!(hash(&source.sha256));
        assert_eq!(source.meshes[0].buffer.vertices.len(), 3);
        assert!(decode(&doc, true).is_err());
        for (pointer, value) in [
            ("/infrastructure_sha256", json!("4".repeat(64))),
            ("/sources/0/sha256", json!("5".repeat(64))),
            ("/profile/code_sources/0/sha256", json!("5".repeat(64))),
            ("/profile/policy/normal_smoothing", json!(1)),
            ("/profile/policy/noise_export", json!("unknown")),
            ("/profile/policy/noise_frequency", json!(0)),
            ("/mesh_stage/hierarchy_active", json!(false)),
            ("/mesh_stage/lod_array_count", json!(2)),
            ("/meshes/0/reference/path_id", json!(43)),
            ("/meshes/0/buffer/normals", json!([])),
            ("/meshes/0/buffer/triangles/0/2", json!(3)),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&bad, false).is_err(), "{pointer}");
        }
        let mut invalid_parent = parent;
        invalid_parent.sha256.clear();
        assert!(
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &invalid_parent, &|| {
                false
            })
            .is_err()
        );
    }
    #[test]
    fn radius_preserves_source_unsigned_bias_and_clamped_lerp() {
        let (parent, doc) = fixture();
        let source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &parent, &|| false).unwrap();
        let p = source.policy();
        assert_eq!(p.radius(10., -2.), Some(5.));
        assert_eq!(p.radius(10., -1.), Some(4.375));
        assert_eq!(p.radius(10., 0.), Some(3.75));
        assert_eq!(p.radius(10., 1.), Some(3.75));
        assert_eq!(p.radius(10., f32::NAN), None);
        assert_eq!(p.radius(0., 0.), None);
    }
}
