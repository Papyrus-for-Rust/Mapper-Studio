//! Exact owned-installation river template capture. No automatic generation binding.
use crate::{
    installation::{self, Installation, SourceFile},
    rail_source::Asset,
};
use papyrus_terrain::{path_mesh::Template, river_water};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Read, path::Path};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    native_sha256: String,
    scene_sha256: String,
    reference_assembly_sha256: String,
    code_sources: Vec<SourceFile>,
    map_scene: SourceFile,
    policy: river_water::Policy,
    #[serde(default)]
    stage_layout: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    object_id: i64,
    mode: i32,
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    kind: String,
    object_id: i64,
    order: usize,
    mode: i32,
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mesh {
    pub reference: Asset,
    pub name: String,
    pub buffer: Template,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    sources: Vec<SourceFile>,
    code_sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    profile: Profile,
    mesh_stage: Stage,
    #[serde(default)]
    generation_stages: Vec<Program>,
    pub mesh: Mesh,
    #[serde(skip)]
    pub sha256: String,
}
fn hash(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Source {
    pub(crate) fn current_extractor(&self) -> bool {
        self.extractor_sha256 == crate::metadata_runtime::river_extractor_sha256()
    }
    pub fn policy(&self) -> &river_water::Policy {
        &self.profile.policy
    }
    pub fn reference_assembly(&self) -> &str {
        &self.profile.reference_assembly_sha256
    }
    pub(crate) fn decode(raw: &[u8], expected: &Installation) -> Result<Self, String> {
        expected.validate()?;
        if raw.len() > 16 * 1024 * 1024 {
            return Err("River mesh capture exceeds 16 MiB".into());
        }
        let mut d: Self =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid river mesh source: {e}"))?;
        d.profile.policy.validate()?;
        d.mesh.buffer.validate()?;
        let current = d.schema == 2;
        if current {
            let kinds = [
                "GenerateRiverLayout",
                "GenerateRiverTopology",
                "GenerateRiverTerrain",
                "GenerateRiverTexture",
                "GenerateRiverMeshes",
            ];
            let mut ids = BTreeSet::new();
            if d.generation_stages.len() != kinds.len()
                || d.generation_stages.iter().zip(kinds).any(|(s, kind)| {
                    s.kind != kind
                        || s.object_id <= 0
                        || !ids.insert(s.object_id)
                        || s.order > 65535
                        || !(-1..=3).contains(&s.mode)
                        || s.mode & 1 == 0
                        || s.disabled.is_some()
                        || !s.hierarchy_active
                })
                || d.generation_stages
                    .windows(2)
                    .any(|p| p[0].order >= p[1].order)
            {
                return Err("Unsupported fresh-client river generation sequence".into());
            }
            let mesh = &d.generation_stages[4];
            if (
                mesh.object_id,
                mesh.mode,
                mesh.disabled,
                mesh.component_enabled,
                mesh.hierarchy_active,
            ) != (
                d.mesh_stage.object_id,
                d.mesh_stage.mode,
                d.mesh_stage.disabled,
                d.mesh_stage.component_enabled,
                d.mesh_stage.hierarchy_active,
            ) {
                return Err("River mesh stage differs from its generation sequence".into());
            }
        } else if !d.generation_stages.is_empty() {
            return Err("Legacy river source cannot override its generation sequence".into());
        }
        if d.format != "papyrus-river-mesh-source"
            || (d.schema, d.adapter.as_str())
                != if current {
                    (2, "papyrus-river-mesh-buffers-2")
                } else {
                    (1, "papyrus-river-mesh-buffers-1")
                }
            || !hash(&d.extractor_sha256)
            || d.sources.len() != 4
            || d.sources[0] != expected.native
            || Some(&d.sources[1]) != expected.scene_manifest.as_ref()
            || d.sources[2].relative_path != "Bundles/shared/assetscenes.bundle"
            || d.sources[3].relative_path != "Bundles/maps/maps.bundle"
            || d.profile.format != "papyrus-river-water-mesh-profile"
            || d.profile.schema != d.schema
            || d.profile.stage_layout.as_deref()
                != if current {
                    Some("mode-description")
                } else {
                    None
                }
            || d.profile.id.is_empty()
            || d.profile.id.len() > 128
            || d.profile.native_sha256 != expected.native.sha256
            || d.profile.scene_sha256 != d.sources[2].sha256
            || !hash(&d.profile.reference_assembly_sha256)
            || d.profile.map_scene != d.sources[3]
            || d.profile.code_sources != d.code_sources
            || !(1..=2).contains(&d.code_sources.len())
            || d.extra_sources.len() > 3
            || d.mesh_stage.object_id == 0
            || if current {
                d.mesh_stage.disabled.is_some()
            } else {
                d.mesh_stage.mode != -1
                    || d.mesh_stage.disabled != Some(false)
                    || !d.mesh_stage.component_enabled
            }
            || !d.mesh_stage.hierarchy_active
            || d.mesh_stage.assets.len() > 96
            || d.mesh.name.is_empty()
            || d.mesh.name.len() > 256
        {
            return Err(
                "River mesh source differs from selected installation or recognized stage".into(),
            );
        }
        let selected = d
            .mesh_stage
            .assets
            .iter()
            .filter(|a| a.field == "RiverMesh")
            .collect::<Vec<_>>();
        if selected.len() != 1
            || selected[0] != &d.mesh.reference
            || d.mesh.reference.index != 0
            || d.mesh.reference.path_id == 0
        {
            return Err("River template differs from source RiverMesh field".into());
        }
        let mut seen = BTreeSet::new();
        for source in d
            .sources
            .iter()
            .chain(&d.code_sources)
            .chain(&d.extra_sources)
        {
            let allowed = matches!(
                source.relative_path.as_str(),
                "RustClient_Data/Plugins/x86_64/RustNative.dll"
                    | "RustDedicated_Data/Plugins/x86_64/RustNative.dll"
                    | "Bundles/AssetSceneManifest.json"
                    | "Bundles/shared/assetscenes.bundle"
                    | "Bundles/maps/maps.bundle"
                    | "Bundles/shared/content.bundle"
                    | "Bundles/shared/monuments.bundle"
                    | "GameAssembly.dll"
                    | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
                    | "RustClient_Data/Managed/Assembly-CSharp.dll"
                    | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
            );
            if !allowed
                || !seen.insert(&source.relative_path)
                || !hash(&source.sha256)
                || source.bytes == 0
                || source.bytes > 8 * 1024 * 1024 * 1024
            {
                return Err("Invalid or duplicate river source receipt".into());
            }
        }
        for source in &d.code_sources {
            if !matches!(
                source.relative_path.as_str(),
                "GameAssembly.dll"
                    | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
                    | "RustClient_Data/Managed/Assembly-CSharp.dll"
                    | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
            ) || source.bytes > 1024 * 1024 * 1024
            {
                return Err("Invalid river gameplay receipt".into());
            }
        }
        d.sha256 = format!("{:x}", Sha256::digest(raw));
        Ok(d)
    }
    pub fn load(
        root: &Path,
        path: &Path,
        expected: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let d = Self::decode(&raw, expected)?;
        if !d.verify(root, &cancelled)? {
            return Ok(None);
        }
        Ok((!cancelled()).then_some(d))
    }
    pub(crate) fn verify(&self, root: &Path, cancelled: impl Fn() -> bool) -> Result<bool, String> {
        for source in self
            .sources
            .iter()
            .chain(&self.code_sources)
            .chain(&self.extra_sources)
        {
            if cancelled() {
                return Ok(false);
            }
            if installation::hash_file(root, &source.relative_path, source.bytes, &cancelled)?
                != *source
            {
                return Err(format!(
                    "River mesh source changed: {}",
                    source.relative_path
                ));
            }
        }
        Ok(!cancelled())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::installation::InstallationKind;
    #[test]
    fn current_sequence_requires_all_stages_and_cannot_use_legacy_activation() {
        let root = tempfile::tempdir().unwrap();
        let (installation, mut doc) = fixture(root.path());
        doc["schema"] = 2.into();
        doc["adapter"] = "papyrus-river-mesh-buffers-2".into();
        doc["profile"]["schema"] = 2.into();
        doc["profile"]["stage_layout"] = "mode-description".into();
        doc["mesh_stage"]["disabled"] = serde_json::Value::Null;
        doc["generation_stages"] = serde_json::json!([
            {"kind":"GenerateRiverLayout","object_id":410,"order":12,"mode":-1,"disabled":null,"component_enabled":false,"hierarchy_active":true},
            {"kind":"GenerateRiverTopology","object_id":411,"order":13,"mode":-1,"disabled":null,"component_enabled":true,"hierarchy_active":true},
            {"kind":"GenerateRiverTerrain","object_id":412,"order":14,"mode":-1,"disabled":null,"component_enabled":true,"hierarchy_active":true},
            {"kind":"GenerateRiverTexture","object_id":413,"order":15,"mode":-1,"disabled":null,"component_enabled":true,"hierarchy_active":true},
            {"kind":"GenerateRiverMeshes","object_id":414,"order":16,"mode":-1,"disabled":null,"component_enabled":true,"hierarchy_active":true}
        ]);
        let decode = |doc: &serde_json::Value| {
            Source::decode(&serde_json::to_vec(doc).unwrap(), &installation)
        };
        decode(&doc).unwrap();
        for (pointer, value) in [
            ("/schema", 1.into()),
            ("/profile/stage_layout", "unknown".into()),
            ("/mesh_stage/disabled", false.into()),
            ("/generation_stages/0/disabled", false.into()),
            ("/generation_stages/0/hierarchy_active", false.into()),
            ("/generation_stages/0/mode", 2.into()),
            ("/generation_stages/0/object_id", 411.into()),
            ("/generation_stages/0/order", 13.into()),
            ("/generation_stages/0/kind", "unknown".into()),
            ("/generation_stages/4/object_id", 500.into()),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&changed).is_err(), "{pointer}");
        }
        doc["generation_stages"].as_array_mut().unwrap().pop();
        assert!(decode(&doc).is_err());
    }
    pub(crate) fn fixture(root: &Path) -> (Installation, serde_json::Value) {
        let names = [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "Bundles/maps/maps.bundle",
            "GameAssembly.dll",
        ];
        let mut receipts = Vec::new();
        for name in names {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name.as_bytes()).unwrap();
            receipts.push(installation::hash_file(root, name, 1024, &|| false).unwrap());
        }
        let expected = Installation {
            kind: InstallationKind::Client,
            native: receipts[0].clone(),
            scene_manifest: Some(receipts[1].clone()),
        };
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/client-0f703d43-water-mesh.json"
        ))
        .unwrap();
        let reference = serde_json::json!({"field":"RiverMesh","index":0,"file_id":0,"path_id":7,"external_path":""});
        let mut profile = profile;
        profile["native_sha256"] = receipts[0].sha256.clone().into();
        profile["scene_sha256"] = receipts[2].sha256.clone().into();
        profile["map_scene"] = serde_json::to_value(&receipts[3]).unwrap();
        profile["code_sources"] = serde_json::json!([receipts[4]]);
        let d = serde_json::json!({"format":"papyrus-river-mesh-source","schema":1,"adapter":"papyrus-river-mesh-buffers-1",
            "sources":&receipts[..4],"code_sources":[receipts[4]],"extra_sources":[],"extractor_sha256":"a".repeat(64),"profile":profile,
            "mesh_stage":{"object_id":414,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,"assets":[reference]},
            "mesh":{"reference":reference,"name":"test-water","buffer":{"vertices":[[-1,0,0],[-1,0,2],[1,0,0],[1,0,2]],"normals":[[0,1,0],[0,1,0],[0,1,0],[0,1,0]],
                "tangents":[[1,0,0,1],[1,0,0,1],[1,0,0,1],[1,0,0,1]],"uv":[[0,0],[0,1],[1,0],[1,1]],"triangles":[[0,1,2],[2,1,3]],"center":[0,0,1],"extents":[1,0,1]}}});
        (expected, d)
    }
    #[test]
    fn source_capture_is_bound_to_installation_files_and_exact_template_reference() {
        let root = tempfile::tempdir().unwrap();
        let (expected, doc) = fixture(root.path());
        let path = root.path().join("meshes.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let source = Source::load(root.path(), &path, &expected, || false)
            .unwrap()
            .unwrap();
        assert_eq!(source.mesh.buffer.vertices.len(), 4);
        std::fs::write(root.path().join("GameAssembly.dll"), b"changed").unwrap();
        assert!(Source::load(root.path(), &path, &expected, || false).is_err());
        assert!(
            Source::load(root.path(), &path, &expected, || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn malformed_reference_receipts_and_unrecognized_features_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (expected, doc) = fixture(root.path());
        for id in 0..5 {
            let mut d = doc.clone();
            match id {
                0 => d["mesh"]["reference"]["path_id"] = 9.into(),
                1 => {
                    d["extra_sources"] = serde_json::json!([{"relative_path":"../outside","sha256":"b".repeat(64),"bytes":1}])
                }
                2 => d["profile"]["policy"]["add_topology"] = true.into(),
                3 => d["sources"][0]["sha256"] = "b".repeat(64).into(),
                _ => d["mesh"]["buffer"]["triangles"] = serde_json::json!([[0, 1, 8]]),
            }
            assert!(Source::decode(&serde_json::to_vec(&d).unwrap(), &expected).is_err());
        }
    }
}
