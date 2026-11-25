//! Source-bound raw collision triangles, for inspection only. No procedural activation/cooking.
use crate::{
    installation::{self, SourceFile},
    prefab_assets::Catalog,
    water_geometry::{Geometry, Issue},
};
use papyrus_terrain::water_mesh::Surface;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mesh {
    pub object_id: i64,
    pub convex: bool,
    pub is_trigger: bool,
    pub cooking_options: u32,
    pub mesh_source: String,
    pub mesh_file: String,
    pub mesh_name: String,
    pub mesh_object_id: i64,
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prefab {
    pub colliders: Vec<Mesh>,
    pub issues: Vec<Issue>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    extractor_sha256: String,
    sources: Vec<SourceFile>,
    prefabs: BTreeMap<String, Prefab>,
}
pub struct Cache {
    pub sha256: String,
    document: Document,
    incomplete_prefabs: BTreeSet<String>,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Cache {
    /// Raw source mesh for an inspected collider. Runtime eligibility/cooking is unresolved.
    pub fn mesh(&self, prefab: &str, collider: i64) -> Option<&Mesh> {
        self.document
            .prefabs
            .get(prefab)?
            .colliders
            .iter()
            .find(|m| m.object_id == collider)
    }
    /// Catalog must already be source-verified through prefab_assets::load.
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Water collision inspection cancelled".into());
        }
        catalog.installation.validate()?;
        let mut raw = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() as u64 > LIMIT {
            return Err("Water collision cache exceeds 16 MiB".into());
        }
        let d: Document = serde_json::from_slice(&raw)
            .map_err(|e| format!("Invalid water collision cache: {e}"))?;
        if d.format != "papyrus-water-collision-cache"
            || d.schema != 1
            || d.adapter != "papyrus-static-triangle-colliders-1"
            || d.catalog_sha256 != catalog.sha256
            || !hash(&d.catalog_sha256)
            || !hash(&d.extractor_sha256)
            || !(3..=4).contains(&d.sources.len())
            || catalog.sources.len() != 3
            || d.sources[..3] != catalog.sources
            || d.prefabs.len() > 128
        {
            return Err("Water collision cache/catalog adapter or source mismatch".into());
        }
        let data_root = catalog
            .installation
            .native
            .relative_path
            .split('/')
            .next()
            .ok_or("Invalid installation data root")?;
        let resource = format!("{data_root}/Resources/unity default resources");
        if let Some(source) = d.sources.get(3)
            && (source.relative_path != resource
                || !hash(&source.sha256)
                || source.bytes == 0
                || source.bytes > 64 * 1024 * 1024
                || installation::hash_file(root, &resource, 64 * 1024 * 1024, &cancelled)?
                    != *source)
        {
            return Err("Water collision resource source changed or is invalid".into());
        }
        let expected: BTreeSet<_> = catalog
            .entries
            .iter()
            .filter(|(_, a)| {
                a.water_geometry
                    .as_ref()
                    .is_some_and(|r| !r.water_bodies.is_empty() || !r.contributors.is_empty())
            })
            .map(|(p, _)| p.clone())
            .collect();
        if d.prefabs.keys().cloned().collect::<BTreeSet<_>>() != expected {
            return Err("Water collision prefab coverage differs from catalog".into());
        }
        let mut total_vertices = 0;
        let mut total_triangles = 0;
        let mut used_resource = false;
        for (path, prefab) in &d.prefabs {
            if cancelled() {
                return Err("Water collision inspection cancelled".into());
            }
            let report = catalog.entries[path]
                .water_geometry
                .as_ref()
                .ok_or("Uninspected water geometry")?;
            report.validate()?;
            let expected: BTreeSet<_> = report
                .water_bodies
                .iter()
                .flat_map(|b| b.triggers.iter())
                .chain(report.contributors.iter().map(|c| &c.collider))
                .filter(|c| matches!(c.geometry, Geometry::Mesh))
                .map(|c| c.object_id)
                .collect();
            if prefab.colliders.len() > 4096 || prefab.issues.len() > 256 {
                return Err("Water collision record count exceeds bounds".into());
            }
            let mut found = BTreeSet::new();
            for m in &prefab.colliders {
                if !expected.contains(&m.object_id)
                    || !found.insert(m.object_id)
                    || m.mesh_object_id == 0
                    || m.cooking_options > 31
                    || m.mesh_file.is_empty()
                    || m.mesh_file.len() > 256
                    || m.mesh_file.contains(['/', '\\', '\0'])
                    || m.mesh_name.is_empty()
                    || m.mesh_name.len() > 256
                    || m.mesh_name.chars().any(char::is_control)
                    || !["Bundles/shared/assetscenes.bundle", resource.as_str()]
                        .contains(&m.mesh_source.as_str())
                {
                    return Err(
                        "Water mesh reference is invalid or absent from the inspected hierarchy"
                            .into(),
                    );
                }
                used_resource |= m.mesh_source == resource;
                total_vertices += m.vertices.len();
                total_triangles += m.triangles.len();
                if total_vertices > 131072 || total_triangles > 262144 {
                    return Err("Water collision geometry exceeds total budget".into());
                }
                Surface::new(&m.vertices, &m.triangles)?;
            }
            for issue in &prefab.issues {
                if !expected.contains(&issue.object_id)
                    || !found.insert(issue.object_id)
                    || issue.reason.is_empty()
                    || issue.reason.len() > 1024
                    || issue.reason.contains('\0')
                {
                    return Err("Invalid or conflicting water mesh inspection issue".into());
                }
            }
            if found != expected {
                return Err(
                    "Water collision mesh coverage is incomplete without explicit issues".into(),
                );
            }
        }
        if used_resource && d.sources.len() != 4 {
            return Err("Missing water collision resource fingerprint".into());
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            incomplete_prefabs: d
                .prefabs
                .keys()
                .filter(|p| {
                    catalog.entries[*p]
                        .water_geometry
                        .as_ref()
                        .is_some_and(|r| !r.complete)
                })
                .cloned()
                .collect(),
            document: d,
        })
    }
    pub fn audit(
        &self,
        details: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<serde_json::Value, String> {
        if cancelled() {
            return Err("Water collision inspection cancelled".into());
        }
        let mut vertices = 0;
        let mut triangles = 0;
        let mut meshes = 0;
        let mut issues = 0;
        let mut work = 0;
        let mut prefabs = Vec::new();
        for (path, prefab) in &self.document.prefabs {
            if cancelled() {
                return Err("Water collision inspection cancelled".into());
            }
            let mut records = Vec::new();
            for m in &prefab.colliders {
                let surface = Surface::new(&m.vertices, &m.triangles)?;
                let height = surface.highest(
                    (surface.minimum[0] + surface.maximum[0]) / 2.,
                    (surface.minimum[2] + surface.maximum[2]) / 2.,
                    true,
                    &mut work,
                    262144,
                    &cancelled,
                )?;
                vertices += m.vertices.len();
                triangles += m.triangles.len();
                meshes += 1;
                let mut record = serde_json::json!({"object_id":m.object_id,"vertices":m.vertices.len(),"triangles":m.triangles.len(),"convex":m.convex,"is_trigger":m.is_trigger,"cooking_options":m.cooking_options,"mesh_source":m.mesh_source,"mesh_file":m.mesh_file,"mesh_name":m.mesh_name,"mesh_object_id":m.mesh_object_id,"minimum":surface.minimum,"maximum":surface.maximum,"double_sided_center_height":height});
                if details {
                    record["raw_geometry"] = serde_json::to_value(m).map_err(|e| e.to_string())?;
                }
                records.push(record);
            }
            issues += prefab.issues.len();
            prefabs.push(
                serde_json::json!({"prefab":path,"static_hierarchy_complete":!self.incomplete_prefabs.contains(path),"colliders":records,"issues":prefab.issues}),
            );
        }
        Ok(
            serde_json::json!({"format":"papyrus-water-collision-audit","schema":1,"cache_sha256":self.sha256,"catalog_sha256":self.document.catalog_sha256,"prefab_count":prefabs.len(),"meshes":meshes,"vertices":vertices,"triangles":triangles,"unresolved_meshes":issues,"incomplete_static_prefabs":self.incomplete_prefabs,"query_triangle_tests":work,"space":"prefab-root-local-unity","physics_cooked":false,"activation_resolved":false,"rasterized":false,"game_validated":false,"prefabs":prefabs}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        installation::{Installation, InstallationKind},
        prefab_assets::Asset,
    };
    use std::collections::HashMap;
    const PREFAB: &str = "assets/bundled/prefabs/autospawn/monument/test.prefab";
    fn fixture(root: &Path) -> (Catalog, serde_json::Value) {
        let mut sources = Vec::new();
        for relative in [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "RustClient_Data/Resources/unity default resources",
        ] {
            std::fs::create_dir_all(root.join(relative).parent().unwrap()).unwrap();
            std::fs::write(root.join(relative), relative.as_bytes()).unwrap();
            sources.push(installation::hash_file(root, relative, 1024, &|| false).unwrap());
        }
        let collider = serde_json::json!({"objectId":12,"active":false,"componentEnabled":true,"hierarchyActive":false,"hierarchyActiveWithoutRoot":true,"geometry":{"shape":"mesh"}});
        let report=serde_json::from_value(serde_json::json!({"version":1,"space":"prefab-root-local-unity","nodesVisited":2,"waterBodies":[],"contributors":[{
            "objectId":11,"active":false,"componentEnabled":true,"hierarchyActive":false,"hierarchyActiveWithoutRoot":true,"bodyType":8,"addTopology":false,"collider":collider}],"issues":[{"objectId":10,"reason":"Unresolved unrelated static reference"}],"complete":false})).unwrap();
        let catalog = Catalog {
            sha256: "c".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources[..3].to_vec(),
            entries: HashMap::from([(
                PREFAB.into(),
                Asset {
                    priority: None,
                    bounds: None,
                    yaw: None,
                    modifier_size: None,
                    modifier_offset: None,
                    texture_guids: BTreeMap::new(),
                    splat_mask: None,
                    modifier: None,
                    modifier_absent: false,
                    water_geometry: Some(report),
                    no_build_geometry: None,
                    display_name: None,
                },
            )]),
        };
        let receipt = serde_json::json!({"format":"papyrus-water-collision-cache","schema":1,"adapter":"papyrus-static-triangle-colliders-1","catalog_sha256":catalog.sha256,"extractor_sha256":"d".repeat(64),"sources":sources,"prefabs":{(PREFAB):{
            "colliders":[{"object_id":12,"convex":false,"is_trigger":true,"cooking_options":30,"mesh_source":"RustClient_Data/Resources/unity default resources","mesh_file":"unity default resources","mesh_name":"Synthetic mesh","mesh_object_id":99,"vertices":[[0.,2.,0.],[0.,2.,1.],[1.,2.,0.]],"triangles":[[0,1,2]]}],"issues":[]}}});
        (catalog, receipt)
    }
    fn save(path: &Path, value: &serde_json::Value) {
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }
    #[test]
    fn queries_keep_unresolved_hierarchies_and_runtime_limits_explicit() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, value) = fixture(root.path());
        let path = root.path().join("collision.json");
        save(&path, &value);
        let cache = Cache::load(root.path(), &path, &catalog, || false).unwrap();
        assert!(cache.mesh(PREFAB, 12).is_some());
        assert!(cache.mesh(PREFAB, 99).is_none());
        let report = cache.audit(false, || false).unwrap();
        assert_eq!(report["meshes"], 1);
        assert_eq!(report["triangles"], 1);
        assert_eq!(
            report["prefabs"][0]["colliders"][0]["double_sided_center_height"],
            2.
        );
        assert_eq!(report["prefabs"][0]["static_hierarchy_complete"], false);
        assert_eq!(
            report["incomplete_static_prefabs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for field in [
            "physics_cooked",
            "activation_resolved",
            "rasterized",
            "game_validated",
        ] {
            assert_eq!(report[field], false);
        }
        assert!(
            cache.audit(true, || false).unwrap()["prefabs"][0]["colliders"][0]["raw_geometry"]
                .is_object()
        );
        assert!(cache.audit(false, || true).is_err());
        assert!(Cache::load(root.path(), &path, &catalog, || true).is_err());
        std::fs::write(
            root.path()
                .join("RustClient_Data/Resources/unity default resources"),
            b"changed",
        )
        .unwrap();
        assert!(Cache::load(root.path(), &path, &catalog, || false).is_err());
    }
    #[test]
    fn malformed_meshes_sources_and_unreported_coverage_gaps_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, original) = fixture(root.path());
        let path = root.path().join("collision.json");
        for case in 0..9 {
            let mut value = original.clone();
            match case {
                0 => value["catalog_sha256"] = "a".repeat(64).into(),
                1 => value["sources"][3]["relative_path"] = "../outside".into(),
                2 => value["prefabs"][PREFAB]["colliders"][0]["triangles"][0][2] = 99.into(),
                3 => value["prefabs"][PREFAB]["colliders"][0]["vertices"][0][0] = 1e9.into(),
                4 => value["prefabs"][PREFAB]["colliders"] = serde_json::json!([]),
                5 => {
                    let duplicate = value["prefabs"][PREFAB]["colliders"][0].clone();
                    value["prefabs"][PREFAB]["colliders"]
                        .as_array_mut()
                        .unwrap()
                        .push(duplicate);
                }
                6 => value["prefabs"][PREFAB]["colliders"][0]["object_id"] = 13.into(),
                7 => {
                    value["sources"].as_array_mut().unwrap().pop();
                }
                _ => value["adapter"] = "future-cooking".into(),
            }
            save(&path, &value);
            assert!(
                Cache::load(root.path(), &path, &catalog, || false).is_err(),
                "case {case}"
            );
        }
        let mut unresolved = original;
        unresolved["prefabs"][PREFAB]["colliders"] = serde_json::json!([]);
        unresolved["prefabs"][PREFAB]["issues"] =
            serde_json::json!([{"objectId":12,"reason":"Unsupported external mesh source"}]);
        save(&path, &unresolved);
        let report = Cache::load(root.path(), &path, &catalog, || false)
            .unwrap()
            .audit(false, || false)
            .unwrap();
        assert_eq!(report["unresolved_meshes"], 1);
        assert_eq!(report["meshes"], 0);
    }
}
