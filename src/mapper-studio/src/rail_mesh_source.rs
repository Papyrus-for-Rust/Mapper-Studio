//! Exact, source-bound rail mesh template LODs. Generated geometry stays local.
use crate::{
    installation::{self, SourceFile},
    rail_source::{Asset, Kind, Source as RailSource},
};
use papyrus_terrain::path_mesh::Template;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Read, path::Path};

const LIMIT: u64 = 32 * 1024 * 1024;
#[derive(Debug, Deserialize)]
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
    rail_source_sha256: String,
    sources: Vec<SourceFile>,
    code_sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    pub meshes: Vec<Mesh>,
    #[serde(skip)]
    pub sha256: String,
}
struct Expected<'a> {
    rail: &'a str,
    sources: &'a [SourceFile],
    code: &'a [SourceFile],
    assets: Vec<&'a Asset>,
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Source {
    pub fn parent_digest(&self) -> &str {
        &self.rail_source_sha256
    }
    /// Diagnostic deformation of an explicitly supplied path and settings.
    /// This does not enable the workstation rail stage or establish runtime parity.
    pub fn deform(
        &self,
        path: &papyrus_terrain::path_geometry::Path,
        query: Query,
        width: f32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<serde_json::Value>, String> {
        use papyrus_terrain::path_mesh;
        if cancelled() {
            return Ok(None);
        }
        if query.settings.width != width
            || query.lod >= self.meshes.len()
            || !(1..=2048).contains(&query.maximum_chunks)
        {
            return Err("Invalid rail mesh diagnostic settings or source width".into());
        }
        let templates = self.meshes.iter().map(|m| &m.buffer).collect::<Vec<_>>();
        let last = templates.last().ok_or("Missing rail mesh templates")?;
        let plan = path_mesh::Plan::new(path, last, query.settings)
            .map_err(|e| format!("Rail mesh plan failed: {e:?}"))?;
        let source = &templates[query.lod];
        let vertex_count = plan
            .repetitions
            .checked_mul(source.vertices.len())
            .ok_or("Rail mesh vertex overflow")?;
        let triangle_count = plan
            .repetitions
            .checked_mul(source.triangles.len())
            .ok_or("Rail mesh index overflow")?;
        if plan.chunks > query.maximum_chunks
            || vertex_count > 1000000
            || triangle_count > 1000000
            || vertex_count + triangle_count > query.settings.maximum_work
        {
            return Err("Rail mesh diagnostic work/output allowance exceeded".into());
        }
        let mut work = 0;
        let mut chunks = Vec::new();
        for index in 0..plan.chunks {
            let mut settings = query.settings;
            settings.maximum_work -= work;
            let Some(chunk) = path_mesh::deform_chunk(
                path,
                &templates,
                query.lod,
                settings,
                index,
                |_, _| None,
                &cancelled,
            )
            .map_err(|e| format!("Rail mesh deformation failed: {e:?}"))?
            else {
                return Ok(None);
            };
            work += chunk.work;
            chunks.push(if query.include_geometry {serde_json::to_value(chunk).map_err(|e|e.to_string())?} else {
                serde_json::json!({"origin":chunk.origin,"first_repetition":chunk.first_repetition,"repetitions":chunk.repetitions,
                    "vertices":chunk.mesh.vertices.len(),"triangles":chunk.mesh.triangles.len(),"center":chunk.mesh.center,"extents":chunk.mesh.extents})
            });
        }
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::json!({"engine":"papyrus-path-mesh-deform-1","source_sha256":self.sha256,
            "lod":query.lod,"settings":query.settings,"plan":plan,"vertices":vertex_count,"triangles":triangle_count,"work":work,
            "chunks":chunks,"path_deformation_built":true,"source_stage_policy_validated":false,
            "physics_cooking_validated":false,"runtime_assembly_complete":false}),
        ))
    }
    fn decode(
        raw: &[u8],
        expected: Expected<'_>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Rail mesh loading cancelled".into());
        }
        if raw.len() as u64 > LIMIT {
            return Err("Rail mesh source exceeds 32 MiB".into());
        }
        let mut doc: Self =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid rail mesh source: {e}"))?;
        if doc.format != "papyrus-rail-mesh-source"
            || doc.schema != 1
            || doc.adapter != "papyrus-rail-mesh-buffers-1"
            || doc.rail_source_sha256 != expected.rail
            || !hash(&doc.rail_source_sha256)
            || doc.sources != expected.sources
            || doc.code_sources != expected.code
            || !hash(&doc.extractor_sha256)
            || !(1..=16).contains(&doc.meshes.len())
            || doc.meshes.len() != expected.assets.len()
            || doc.extra_sources.len() > 3
        {
            return Err("Rail mesh source differs from selected rail stage or installation".into());
        }
        let mut seen = BTreeSet::new();
        for source in &doc.extra_sources {
            if !matches!(
                source.relative_path.as_str(),
                "Bundles/shared/content.bundle"
                    | "Bundles/shared/monuments.bundle"
                    | "Bundles/shared/assetscenes.bundle"
            ) || !seen.insert(&source.relative_path)
                || !hash(&source.sha256)
                || source.bytes == 0
                || source.bytes > 8 * 1024 * 1024 * 1024
            {
                return Err("Invalid rail mesh resource receipt".into());
            }
        }
        let mut vertices = 0;
        let mut triangles = 0;
        for (index, mesh) in doc.meshes.iter().enumerate() {
            if cancelled() {
                return Err("Rail mesh loading cancelled".into());
            }
            if &mesh.reference != expected.assets[index]
                || mesh.reference.index != index
                || mesh.name.is_empty()
                || mesh.name.len() > 512
            {
                return Err("Rail template LOD order/reference mismatch".into());
            }
            mesh.buffer.validate()?;
            vertices += mesh.buffer.vertices.len();
            triangles += mesh.buffer.triangles.len();
        }
        if vertices > 262144 || triangles > 524288 {
            return Err("Rail template aggregate exceeds bounds".into());
        }
        if cancelled() {
            return Err("Rail mesh loading cancelled".into());
        }
        doc.sha256 = format!("{:x}", Sha256::digest(raw));
        Ok(doc)
    }
    pub fn load(
        root: &Path,
        path: &Path,
        rail: &RailSource,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let stages = rail
            .stages
            .iter()
            .filter(|s| s.kind == Kind::GenerateRailMeshes)
            .collect::<Vec<_>>();
        if stages.len() != 1 {
            return Err("Missing unambiguous rail mesh stage".into());
        }
        let mut assets = stages[0]
            .assets
            .iter()
            .filter(|a| a.field == "RailMeshes")
            .collect::<Vec<_>>();
        if assets.is_empty() {
            assets = stages[0]
                .assets
                .iter()
                .filter(|a| a.field == "RailMesh")
                .collect();
        }
        assets.sort_by_key(|a| a.index);
        let doc = Self::decode(
            &raw,
            Expected {
                rail: &rail.sha256,
                sources: rail.source_receipts(),
                code: rail.gameplay_sources(),
                assets,
            },
            &cancelled,
        )?;
        for receipt in &doc.extra_sources {
            if installation::hash_file(
                root,
                &receipt.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Rail mesh bundle differs from selected installation".into());
            }
        }
        Ok(doc)
    }
    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({
            "source_sha256": self.sha256, "rail_source_sha256": self.rail_source_sha256,
            "lods": self.meshes.iter().enumerate().map(|(index, m)| serde_json::json!({
                "index": index, "name": m.name, "vertices": m.buffer.vertices.len(),
                "triangles": m.buffer.triangles.len(), "center": m.buffer.center, "extents": m.buffer.extents,
                "reference": m.reference
            })).collect::<Vec<_>>(),
            "deformation_bounds_lod": self.meshes.len() - 1, "collision_template_lod": 0,
            "simplified": false, "path_deformation_complete": false,
            "physics_cooking_validated": false, "runtime_assembly_complete": false
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub lod: usize,
    pub settings: papyrus_terrain::path_mesh::Settings,
    pub maximum_chunks: usize,
    pub include_geometry: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn fixture() -> (Asset, Value) {
        let asset = Asset {
            field: "RailMeshes".into(),
            index: 0,
            file_id: 2,
            path_id: 42,
            external_path: "archive:/test/test".into(),
        };
        let doc = json!({"format":"papyrus-rail-mesh-source","schema":1,"adapter":"papyrus-rail-mesh-buffers-1",
            "rail_source_sha256":"a".repeat(64),"sources":[],"code_sources":[],
            "extra_sources":[{"relative_path":"Bundles/shared/content.bundle","sha256":"c".repeat(64),"bytes":1}],
            "extractor_sha256":"b".repeat(64), "meshes":[{"reference":asset,"name":"synthetic rail",
                "buffer":{"vertices":[[0,0,0],[1,0,0],[0,0,2]],"normals":[[0,1,0],[0,1,0],[0,1,0]],
                "tangents":[[1,0,0,-1],[1,0,0,-1],[1,0,0,-1]],"uv":[[0,0],[1,0],[0,1]],"triangles":[[0,1,2]],
                "center":[0.5,0,1],"extents":[0.5,0,1]}}]});
        (asset, doc)
    }
    #[test]
    fn buffers_and_references_are_strict_source_bound_and_cancelable() {
        let (asset, doc) = fixture();
        let decode = |doc: &Value, cancel| {
            Source::decode(
                &serde_json::to_vec(doc).unwrap(),
                Expected {
                    rail: &"a".repeat(64),
                    sources: &[],
                    code: &[],
                    assets: vec![&asset],
                },
                || cancel,
            )
        };
        let source = decode(&doc, false).unwrap();
        assert_eq!(source.meshes[0].buffer.vertices[2], [0., 0., 2.]);
        assert_eq!(source.audit()["deformation_bounds_lod"], 0);
        assert_eq!(source.audit()["physics_cooking_validated"], false);
        assert!(decode(&doc, true).is_err());
        for (pointer, value) in [
            ("/rail_source_sha256", json!("f".repeat(64))),
            ("/meshes/0/reference/path_id", json!(43)),
            ("/meshes/0/reference/index", json!(1)),
            ("/meshes/0/buffer/normals", json!([])),
            ("/meshes/0/buffer/triangles/0/2", json!(3)),
            ("/meshes/0/buffer/triangles/0/0", json!(-1)),
            ("/meshes/0/buffer/vertices/0/0", json!(10001)),
            ("/meshes/0/buffer/extents/2", json!(0)),
            ("/extra_sources/0/relative_path", json!("../outside.bundle")),
            ("/extra_sources/0/sha256", json!("invalid")),
            ("/extra_sources/0/bytes", json!(0)),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&changed, false).is_err(), "{pointer}");
        }
        let mut changed = doc.clone();
        let duplicate = changed["extra_sources"][0].clone();
        changed["extra_sources"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(decode(&changed, false).is_err());
    }
    #[test]
    fn deformation_is_globally_bounded_and_does_not_mutate_the_source_templates() {
        let (asset, doc) = fixture();
        let source = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            Expected {
                rail: &"a".repeat(64),
                sources: &[],
                code: &[],
                assets: vec![&asset],
            },
            || false,
        )
        .unwrap();
        let path = papyrus_terrain::path_geometry::Path::from_controls(
            &[[0., 10., 0.], [0., 10., 30.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let query = || Query {
            lod: 0,
            settings: papyrus_terrain::path_mesh::Settings {
                width: 1.,
                chunk_repetitions: 5,
                maximum_work: 1000,
                snap_start: false,
                snap_end: false,
                mesh_offset: 0.,
                top_aligned: false,
            },
            maximum_chunks: 20,
            include_geometry: false,
        };
        let before = serde_json::to_vec(&source.meshes[0].buffer).unwrap();
        let report = source
            .deform(&path, query(), 1., || false)
            .unwrap()
            .unwrap();
        assert_eq!(report["vertices"], 45);
        assert_eq!(report["triangles"], 15);
        assert_eq!(report["chunks"].as_array().unwrap().len(), 3);
        assert_eq!(report["physics_cooking_validated"], false);
        assert_eq!(
            serde_json::to_vec(&source.meshes[0].buffer).unwrap(),
            before
        );
        for kind in 0..4 {
            let mut q = query();
            match kind {
                0 => q.settings.maximum_work = 1,
                1 => q.maximum_chunks = 1,
                2 => q.lod = 1,
                _ => q.settings.width = 2.,
            }
            assert!(source.deform(&path, q, 1., || false).is_err());
        }
        let calls = std::cell::Cell::new(0);
        assert!(
            source
                .deform(&path, query(), 1., || {
                    calls.set(calls.get() + 1);
                    calls.get() > 10
                })
                .unwrap()
                .is_none()
        );
    }
}
