//! Separate ordered inputs for source-style road layout. Legacy infrastructure
//! ordering/identities are never silently upgraded. Enum values stay opaque
//! until a verified selected-code role policy interprets them.
use crate::{infrastructure, installation, prefab_assets::Catalog};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
const LIMIT: u64 = 16 * 1024 * 1024;
pub const ADAPTER: &str = "papyrus-ordered-monument-road-inputs-1";
const ORDER: &str = "root-first-depth-first-component-order";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Monument {
    pub object_id: i64,
    pub monument_type: i32,
    pub bounds_center: [f32; 3],
    pub bounds_extent: [f32; 3],
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Issue {
    object_id: i64,
    reason: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Prefab {
    version: u32,
    space: String,
    connector_order: String,
    complete: bool,
    pub monument: Option<Monument>,
    pub connectors: Vec<infrastructure::Connector>,
    issues: Vec<Issue>,
    nodes_visited: usize,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Assets {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    infrastructure_sha256: String,
    sources: Vec<installation::SourceFile>,
    extractor_sha256: String,
    prefabs: BTreeMap<String, Prefab>,
    #[serde(skip)]
    pub sha256: String,
}
pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Assets {
    fn validate(
        &self,
        catalog: &str,
        infrastructure: &str,
        receipts: &[installation::SourceFile],
        paths: &BTreeSet<&str>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if self.format != "papyrus-road-layout-assets"
            || self.schema != 1
            || self.adapter != ADAPTER
            || !valid_digest(&self.extractor_sha256)
            || self.catalog_sha256 != catalog
            || !valid_digest(catalog)
            || self.infrastructure_sha256 != infrastructure
            || !valid_digest(infrastructure)
            || self.sources != receipts
            || !(5..=8).contains(&self.sources.len())
            || !(1..=4096).contains(&self.prefabs.len())
            || self.prefabs.len() != paths.len()
        {
            return Err("Ordered road assets differ from the selected verified sources".into());
        }
        for (path, prefab) in &self.prefabs {
            if cancelled() {
                return Err("Road-layout asset verification cancelled".into());
            }
            if !paths.contains(path.as_str())
                || prefab.version != 1
                || prefab.space != "prefab-root-local-unity"
                || prefab.connector_order != ORDER
                || prefab.connectors.len() > 256
                || prefab.nodes_visited > 65536
                || (prefab.complete && (prefab.nodes_visited == 0 || !prefab.issues.is_empty()))
                || (!prefab.complete && prefab.issues.is_empty())
                || prefab.issues.len() > 128
                || prefab.issues.iter().any(|i| {
                    i.object_id == i64::MIN || i.reason.is_empty() || i.reason.len() > 1024
                })
            {
                return Err("Invalid ordered road prefab coverage or hierarchy order".into());
            }
            let mut ids = BTreeSet::new();
            if let Some(m) = &prefab.monument {
                if m.object_id == 0
                    || !(0..=1024).contains(&m.monument_type)
                    || m.bounds_center
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || m.bounds_extent
                        .iter()
                        .any(|v| !v.is_finite() || !(0. ..=10000.).contains(v))
                {
                    return Err("Invalid root road-layout monument metadata".into());
                }
                ids.insert(m.object_id);
            }
            for c in &prefab.connectors {
                let length = c.forward.iter().map(|v| v * v).sum::<f32>().sqrt();
                if c.object_id == 0
                    || !ids.insert(c.object_id)
                    || c.infrastructure_type < 0
                    || c.position
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || !length.is_finite()
                    || (length - 1.).abs() > 0.001
                {
                    return Err("Invalid ordered road connector identity or pose".into());
                }
            }
        }
        Ok(())
    }
    pub fn load(
        path: &Path,
        root: &Path,
        catalog: &Catalog,
        source: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Ordered road assets exceed 16 MiB".into());
        }
        let Some(assets) = Self::decode(&bytes, catalog, source, &cancelled)? else {
            return Ok(None);
        };
        if assets.verify_installation(root, &cancelled)?.is_none() {
            return Ok(None);
        }
        Ok(Some(assets))
    }
    pub(crate) fn decode(
        bytes: &[u8],
        catalog: &Catalog,
        source: &infrastructure::Source,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        if bytes.len() as u64 > LIMIT {
            return Err("Ordered road assets exceed 16 MiB".into());
        }
        let mut assets: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if source.catalog_sha256 != catalog.sha256 {
            return Err("Road-layout infrastructure and catalog differ".into());
        }
        let receipts = source.road_object_receipts()?;
        let paths = catalog.entries.keys().map(String::as_str).collect();
        let validation = assets.validate(
            &catalog.sha256,
            &source.sha256,
            &receipts,
            &paths,
            cancelled,
        );
        if cancelled() {
            return Ok(None);
        }
        validation?;
        assets.sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(Some(assets))
    }
    pub(crate) fn verify_installation(
        &self,
        root: &Path,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<()>, String> {
        for expected in &self.sources {
            let actual =
                installation::hash_file(root, &expected.relative_path, expected.bytes, cancelled);
            if cancelled() {
                return Ok(None);
            }
            if actual? != *expected {
                return Err(
                    "Ordered road asset source changed in the selected installation".into(),
                );
            }
        }
        Ok(Some(()))
    }
    pub(crate) fn extractor_sha256(&self) -> &str {
        &self.extractor_sha256
    }
    pub(crate) fn same_prefabs(&self, other: &Self) -> Result<bool, String> {
        Ok(
            serde_json::to_vec(&self.prefabs).map_err(|e| e.to_string())?
                == serde_json::to_vec(&other.prefabs).map_err(|e| e.to_string())?,
        )
    }
    pub fn prefab(&self, path: &str) -> Result<&Prefab, String> {
        self.prefabs
            .get(path)
            .filter(|p| p.complete)
            .ok_or_else(|| format!("Missing or incomplete ordered road metadata for {path}"))
    }
    pub fn prefabs(&self) -> &BTreeMap<String, Prefab> {
        &self.prefabs
    }
    pub fn infrastructure_sha256(&self) -> &str {
        &self.infrastructure_sha256
    }
    pub fn catalog_sha256(&self) -> &str {
        &self.catalog_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    const PREFAB: &str = "assets/bundled/prefabs/test.prefab";
    fn receipts() -> Vec<installation::SourceFile> {
        [
            "RustDedicated_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "RustDedicated_Data/Managed/Assembly-CSharp.dll",
            "Bundles/maps/maps.bundle",
        ]
        .iter()
        .map(|p| installation::SourceFile {
            relative_path: p.to_string(),
            sha256: "c".repeat(64),
            bytes: 100,
        })
        .collect()
    }
    fn document() -> Value {
        json!({"format":"papyrus-road-layout-assets","schema":1,"adapter":ADAPTER,
            "catalog_sha256":"a".repeat(64),"infrastructure_sha256":"b".repeat(64),"extractor_sha256":"d".repeat(64),
            "sources":receipts(),"prefabs":{PREFAB:{"version":1,"space":"prefab-root-local-unity",
                "connectorOrder":ORDER,"complete":true,"nodesVisited":2,"issues":[],
                "monument":{"objectId":99,"monumentType":7,"boundsCenter":[1.,0.,2.],"boundsExtent":[40.,20.,60.]},
                "connectors":[{"objectId":1000,"infrastructureType":0,"position":[1.,2.,3.],"forward":[0.,0.,1.],"hierarchyActiveWithoutRoot":true},
                    {"objectId":80,"infrastructureType":2,"position":[10.,3.,20.],"forward":[1.,0.,0.],"hierarchyActiveWithoutRoot":false}]}}})
    }
    fn check(value: Value) -> Result<Assets, String> {
        let assets: Assets = serde_json::from_value(value).map_err(|e| e.to_string())?;
        assets.validate(
            &"a".repeat(64),
            &"b".repeat(64),
            &receipts(),
            &BTreeSet::from([PREFAB]),
            &|| false,
        )?;
        Ok(assets)
    }
    #[test]
    fn hierarchy_order_inactive_exits_and_opaque_monument_types_survive_validation() {
        let assets = check(document()).unwrap();
        let prefab = assets.prefab(PREFAB).unwrap();
        assert_eq!(
            prefab
                .connectors
                .iter()
                .map(|c| c.object_id)
                .collect::<Vec<_>>(),
            [1000, 80]
        );
        assert!(!prefab.connectors[1].hierarchy_active_without_root);
        assert_eq!(prefab.monument.as_ref().unwrap().monument_type, 7);
        let mut signed = document();
        signed["prefabs"][PREFAB]["monument"]["objectId"] = (-99).into();
        signed["prefabs"][PREFAB]["connectors"][0]["objectId"] = (-1000).into();
        assert!(check(signed).is_ok());
        let mut doc = document();
        doc["prefabs"][PREFAB]["monument"] = Value::Null;
        assert!(
            check(doc)
                .unwrap()
                .prefab(PREFAB)
                .unwrap()
                .monument
                .is_none()
        );
    }
    #[test]
    fn foreign_receipts_legacy_order_missing_coverage_and_bad_geometry_are_rejected() {
        for field in [
            "catalog_sha256",
            "infrastructure_sha256",
            "extractor_sha256",
        ] {
            let mut doc = document();
            doc[field] = "invalid".into();
            assert!(check(doc).is_err());
        }
        let mut doc = document();
        doc["sources"][0]["sha256"] = "e".repeat(64).into();
        assert!(check(doc).is_err());
        let mut doc = document();
        doc["prefabs"][PREFAB]["connectorOrder"] = "object-id-sorted".into();
        assert!(check(doc).is_err());
        let mut doc = document();
        doc["prefabs"] = json!({});
        assert!(check(doc).is_err());
        let mut doc = document();
        doc["prefabs"][PREFAB]["monument"]["boundsExtent"][0] = (-1).into();
        assert!(check(doc).is_err());
        let mut doc = document();
        doc["prefabs"][PREFAB]["connectors"][1]["objectId"] = 1000.into();
        assert!(check(doc).is_err());
        let mut doc = document();
        doc["prefabs"][PREFAB]["connectors"][0]["objectId"] = 99.into();
        assert!(check(doc).is_err());
        let mut doc = document();
        doc["prefabs"][PREFAB]["connectors"][0]["forward"] = json!([0., 0., 0.]);
        assert!(check(doc).is_err());
    }
    #[test]
    fn incomplete_prefabs_are_reported_and_cannot_be_retrieved_as_complete_inputs() {
        let mut doc = document();
        doc["prefabs"][PREFAB]["complete"] = false.into();
        doc["prefabs"][PREFAB]["issues"] =
            json!([{"objectId":99,"reason":"Child MonumentInfo requires a transform adapter"}]);
        let assets = check(doc).unwrap();
        assert!(assets.prefab(PREFAB).is_err());
        assert!(assets.prefab("missing.prefab").is_err());
        assert!(
            assets
                .validate(
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &receipts(),
                    &BTreeSet::from([PREFAB]),
                    &|| true
                )
                .is_err()
        );
    }
}
