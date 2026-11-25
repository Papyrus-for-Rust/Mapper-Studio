//! Cosmetic source-bound static decoration LODs. Never used for collision or placement.
use crate::{
    decor_source, installation::SourceFile, monument_source::Model, prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::Arc,
};
const LIMIT: usize = 32 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    decoration_sha256: String,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    assembly_validated: bool,
    models: BTreeMap<String, Model>,
}
pub struct Source {
    pub sha256: String,
    pub decoration_sha256: String,
    pub models: BTreeMap<String, Arc<Model>>,
}
pub struct Instance<'a> {
    pub model: &'a Arc<Model>,
    pub pose: &'a papyrus_terrain::decor_fit::Pose,
}
#[derive(Clone)]
pub struct World {
    catalog: String,
    bindings: String,
    base: Arc<Source>,
    vines: Option<VineModels>,
}
#[derive(Clone)]
struct VineModels {
    binding: String,
    road: String,
    source: Arc<Source>,
}
impl VineModels {
    fn matches(&self, objects: &crate::object_generation::Recipe, pin: &str) -> bool {
        self.binding == pin && self.road == objects.road_source_sha256
    }
}
impl World {
    pub fn matches_installation(&self, catalog: &Catalog, bindings: &Bindings) -> bool {
        self.catalog == catalog.sha256 && self.bindings == bindings.sha256
    }

    pub fn covers(&self, project: &crate::Project) -> bool {
        project.decoration.as_ref().is_some_and(|recipe| self.covers_base(&recipe.source_sha256))
            && project.objects.as_ref().is_none_or(|objects| {
                objects.jungle_vines_source_sha256.as_ref().is_none_or(|pin|
                    self.vines.as_ref().is_some_and(|vines| vines.matches(objects, pin)))
            })
    }

    pub fn covers_base(&self, pin: &str) -> bool {
        self.base.decoration_sha256 == pin
    }
    pub fn model_counts(&self) -> (usize, usize) {
        let models = self.base.models.values().chain(
            self.vines
                .iter()
                .flat_map(|vines| vines.source.models.values()),
        );
        models.fold((0, 0), |(visible, total), model| {
            (
                visible + usize::from(!model.triangles.is_empty()),
                total + 1,
            )
        })
    }
    pub fn composition_instances<'a>(
        &'a self,
        maps: &'a papyrus_terrain::monument_composition::Composed,
    ) -> Vec<Instance<'a>> {
        let Some(scene) = &maps.objects else {
            return self.base.composition_instances(maps);
        };
        self.object_instances(scene)
    }
    fn object_instances<'a>(
        &'a self,
        scene: &'a papyrus_terrain::object_scene::Scene,
    ) -> Vec<Instance<'a>> {
        if scene.decoration_source_sha256 != self.base.decoration_sha256 {
            return Vec::new();
        }
        let mut instances = self.base.scene_instances(scene);
        if let Some(vines) = &self.vines
            && scene.jungle_vines_source_sha256.as_deref() == Some(&vines.binding)
            && scene.road_source_sha256 == vines.road
        {
            instances.extend(vines.source.scene_instances(scene));
        }
        instances
    }
}

fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Source {
    fn quality(&self) -> (usize, usize, usize, &str) {
        (
            self.models
                .values()
                .filter(|m| !m.triangles.is_empty())
                .count(),
            self.models.values().filter(|m| m.complete).count(),
            usize::MAX - self.models.values().map(|m| m.issues.len()).sum::<usize>(),
            &self.sha256,
        )
    }
    fn decode(
        raw: &[u8],
        catalog: &Catalog,
        receipts: &[SourceFile],
        decoration_sha: &str,
        paths: BTreeSet<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Decoration model loading cancelled".into());
        }
        if raw.len() > LIMIT {
            return Err("Decoration model source exceeds 32 MiB".into());
        }
        let d: Document = serde_json::from_slice(raw)
            .map_err(|e| format!("Invalid decoration model source: {e}"))?;
        if d.format != "papyrus-decor-model-source"
            || d.schema != 1
            || d.adapter != "papyrus-decor-static-lod-1"
            || !hash(&d.decoration_sha256)
            || d.decoration_sha256 != decoration_sha
            || d.sources != catalog.sources
            || !hash(&d.extractor_sha256)
            || d.assembly_validated
            || d.models.is_empty()
            || d.models.len() > 4096
            || d.models.keys().map(String::as_str).collect::<BTreeSet<_>>() != paths
            || d.extra_sources.len() > 2
        {
            return Err(
                "Decoration model source differs from verified installation or recipe".into(),
            );
        }
        let mut seen = BTreeSet::new();
        for receipt in &d.extra_sources {
            if !matches!(
                receipt.relative_path.as_str(),
                "Bundles/shared/content.bundle" | "Bundles/shared/monuments.bundle"
            ) || !seen.insert(&receipt.relative_path)
                || !receipts.contains(receipt)
            {
                return Err(
                    "Decoration model bundle is not in verified installation receipts".into(),
                );
            }
        }
        let mut vertices = 0;
        let mut triangles = 0;
        for model in d.models.values() {
            if cancelled() {
                return Err("Decoration model loading cancelled".into());
            }
            vertices += model.vertices.len();
            triangles += model.triangles.len();
            if model.vertices.len() > 200000
                || vertices > 1000000
                || model.triangles.len() > 256
                || triangles > 100000
                || model.issues.len() > 256
                || model.issues.iter().any(|s| s.len() > 1024)
                || model
                    .vertices
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || model
                    .triangles
                    .iter()
                    .flatten()
                    .any(|i| *i as usize >= model.vertices.len())
                || model.complete && (model.triangles.is_empty() || !model.issues.is_empty())
            {
                return Err("Invalid or excessive decoration model geometry".into());
            }
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            decoration_sha256: d.decoration_sha256,
            models: d.models.into_iter().map(|(path, model)| (path, Arc::new(model))).collect(),
        })
    }
    pub fn load(
        path: &Path,
        source: &decor_source::Source,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Decoration model loading cancelled".into());
        }
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        Self::decode(
            &raw,
            catalog,
            bindings.source_receipts(),
            &source.sha256,
            source.prefab_paths().collect(),
            cancelled,
        )
    }
    pub fn discover(
        source: &decor_source::Source,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let root = crate::storage::local_data()?.join("papyrus-decor-model-cache");
        if !root.is_dir() {
            return Ok(None);
        }
        let mut paths = Vec::new();
        for (i, entry) in std::fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i >= 256 {
                return Err("Too many decoration model snapshots".into());
            }
            if cancelled() {
                return Ok(None);
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.file_name().and_then(|s| s.to_str()).is_some_and(hash) {
                paths.push(path);
            }
        }
        paths.sort();
        let mut best: Option<Self> = None;
        for directory in paths.into_iter().rev() {
            if cancelled() {
                return Ok(None);
            }
            if let Ok(model) = Self::load(
                &directory.join("models.json"),
                source,
                catalog,
                bindings,
                &cancelled,
            ) && directory.file_name().and_then(|s| s.to_str()) == Some(model.sha256.as_str())
                && best.as_ref().is_none_or(|s| model.quality() > s.quality())
            {
                best = Some(model);
            }
        }
        Ok(best)
    }
    /// Only committed terrain placements are drawn. Fitted or rejected candidates
    /// never become visible accepted objects, and stale recipes cannot reuse models.
    pub fn composition_instances<'a>(
        &'a self,
        maps: &'a papyrus_terrain::monument_composition::Composed,
    ) -> Vec<Instance<'a>> {
        if let Some(scene) = &maps.objects {
            if scene.decoration_source_sha256 != self.decoration_sha256 {
                return vec![];
            }
            return self.scene_instances(scene);
        }
        maps.decoration
            .as_ref()
            .map_or_else(Vec::new, |report| self.instances(report))
    }
    fn scene_instances<'a>(
        &'a self,
        scene: &'a papyrus_terrain::object_scene::Scene,
    ) -> Vec<Instance<'a>> {
        scene
            .instances
            .iter()
            .filter(|i| matches!(i.kind, papyrus_terrain::object_scene::Kind::Decoration))
            .take(100000)
            .filter_map(|i| {
                let model = self.models.get(&i.prefab)?;
                (!model.triangles.is_empty()).then_some(Instance {
                    model,
                    pose: &i.pose,
                })
            })
            .collect()
    }
    pub fn instances<'a>(&'a self, report: &'a papyrus_terrain::decor::Report) -> Vec<Instance<'a>> {
        if report.source_sha256 != self.decoration_sha256 {
            return vec![];
        }
        let Some(fitting) = &report.fitting else {
            return vec![];
        };
        let Some(terrain) = &report.terrain_placement else {
            return vec![];
        };
        terrain
            .applied
            .iter()
            .take(100000)
            .filter_map(|a| {
                let pose = fitting.poses.get(a.pose)?;
                let candidate = report.candidates.get(pose.candidate)?;
                let path = report
                    .stages
                    .get(candidate.stage)?
                    .prefabs
                    .get(candidate.prefab)?;
                let model = self.models.get(path)?;
                (!model.triangles.is_empty()).then_some(Instance { model, pose })
            })
            .collect()
    }
}
/// Local development/runtime preparation. Geometry is published by the extractor
/// atomically; cancel kills only the directly owned worker and retains no partial cache.
pub fn prepare(
    root: &Path,
    source: &decor_source::Source,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    prepare_input(root, source, catalog, bindings, None, cancelled)
}
fn prepare_input(
    root: &Path,
    source: &decor_source::Source,
    catalog: &Catalog,
    bindings: &Bindings,
    embedded: Option<&[u8]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-decor-model-cache");
    if cancelled() {
        return Ok(None);
    }
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".decor-model-prepare-")
        .tempdir_in(&cache)
        .map_err(|e| e.to_string())?;
    let source_path = if let Some(raw) = embedded {
        if raw.len() > 16 * 1024 * 1024 || format!("{:x}", Sha256::digest(raw)) != source.sha256 {
            return Err("Preview metadata differs from its verified source".into());
        }
        let path = temporary.path().join("source.json");
        std::fs::write(&path, raw).map_err(|error| error.to_string())?;
        path
    } else {
        crate::client_cache::pinned_document(
            &local,
            "papyrus-decor-cache",
            &source.sha256,
            "decor.json",
        )?
    };
    let output = temporary.path().join("output");
    if !crate::metadata_runtime::run(
        crate::metadata_runtime::Script::DecorModels,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--source".into(),
            source_path.as_os_str().to_owned(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Ok(None);
    }
    let document = crate::metadata_runtime::document(&output, "models.json")?;
    let models = Source::load(&document, source, catalog, bindings, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    crate::metadata_runtime::publish(&document, &cache, &models.sha256, 32 * 1024 * 1024)?;
    Ok((!cancelled()).then_some(models))
}

/// Reopen prepared preview geometry against the current recipe and installation.
pub fn cached_world(
    project: &crate::Project,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<World>, String> {
    project_world(project, catalog, bindings, None, false, cancelled)
}
/// Recover missing preview caches using the selected installation.
pub fn prepare_project(
    project: &crate::Project,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<World>, String> {
    prepare_project_reusing(project, catalog, bindings, None, cancelled)
}

/// Keep the currently loaded model families when their source receipts agree.
pub fn prepare_project_reusing(
    project: &crate::Project,
    catalog: &Catalog,
    bindings: &Bindings,
    previous: Option<&World>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<World>, String> {
    project_world(project, catalog, bindings, previous, true, cancelled)
}
fn project_world(
    project: &crate::Project,
    catalog: &Catalog,
    bindings: &Bindings,
    previous: Option<&World>,
    capture: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<World>, String> {
    let _verification = crate::installation::VerificationScope::default();
    let previous = previous.filter(|world| world.matches_installation(catalog, bindings));
    let decoration = project.decoration.as_ref().ok_or("Scenery is disabled")?;
    let base = if let Some(previous) = previous.filter(|world| world.covers_base(&decoration.source_sha256)) {
        previous.base.clone()
    } else {
        let metadata = decor_source::Source::cached(catalog, bindings, decoration, &cancelled)?;
        Arc::new(match Source::discover(&metadata, catalog, bindings, &cancelled)? {
            Some(models) => models,
            None if capture => {
                let Some(models) = prepare(bindings.installation_root(), &metadata, catalog, bindings, &cancelled)?
                else { return Ok(None); };
                models
            }
            None => return Err("Scenery model cache is unavailable".into()),
        })
    };
    // Retain one vine family even when the current recipe has vines disabled.
    let mut vine_models = previous.and_then(|world| world.vines.clone());
    if let Some(objects) = &project.objects
        && let Some(pin) = &objects.jungle_vines_source_sha256
        && !vine_models.as_ref().is_some_and(|vines| vines.matches(objects, pin))
    {
        let root = bindings.installation_root();
        let infrastructure = crate::infrastructure::Source::cached(
            catalog,
            project
                .roads
                .as_ref()
                .ok_or("Missing scenery road source")?,
            &cancelled,
        )?;
        let road = crate::road_object_source::Source::cached(
            root,
            &objects.road_source_sha256,
            &infrastructure,
            &cancelled,
        )?;
        let vines = crate::jungle_vine_source::cached(root, catalog, &road, pin, &cancelled)?;
        let source = match Source::discover(vines.metadata(), catalog, bindings, &cancelled)? {
            Some(models) => models,
            None if capture => {
                let Some(models) = prepare_input(
                    root,
                    vines.metadata(),
                    catalog,
                    bindings,
                    Some(vines.metadata_bytes()),
                    &cancelled,
                )?
                else {
                    return Ok(None);
                };
                models
            }
            None => return Err("Vine model cache is unavailable".into()),
        };
        vine_models = Some(VineModels {
            binding: pin.clone(),
            road: objects.road_source_sha256.clone(),
            source: Arc::new(source),
        });
    }
    Ok((!cancelled()).then_some(World {
        catalog: catalog.sha256.clone(),
        bindings: bindings.sha256.clone(),
        base,
        vines: vine_models,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an owned client, prepared recipe and metadata runtime"]
    fn owned_world_models_prepare_and_reopen_vines_with_source_receipts() {
        eprintln!("Inspecting installation");
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_SCENERY_MODEL_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        eprintln!("Loading catalog");
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_SCENERY_MODEL_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        eprintln!("Loading texture bindings");
        let bindings = Bindings::load(
            &root,
            &path("MAPPER_SCENERY_MODEL_TEST_BINDINGS"),
            &catalog,
            || false,
        )
        .unwrap();
        eprintln!("Loading saved recipe");
        let project = crate::Project::load(&path("MAPPER_SCENERY_MODEL_TEST_PROJECT")).unwrap();
        eprintln!("Preparing world preview models");
        let world = prepare_project(&project, &catalog, &bindings, || false)
            .unwrap()
            .unwrap();
        let vines = world.vines.as_ref().expect("prepared vine models");
        assert_eq!(vines.source.models.len(), 3);
        assert!(
            vines
                .source
                .models
                .values()
                .all(|model| model.complete && model.issues.is_empty())
        );
        assert_eq!(
            Some(vines.binding.as_str()),
            project
                .objects
                .as_ref()
                .unwrap()
                .jungle_vines_source_sha256
                .as_deref()
        );
        eprintln!("Reopening cached world models");
        let reopened = cached_world(&project, &catalog, &bindings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.base.sha256, world.base.sha256);
        assert_eq!(
            reopened.vines.as_ref().unwrap().source.sha256,
            vines.source.sha256
        );
        assert_eq!(reopened.model_counts(), world.model_counts());
        assert!(cached_world(&project, &catalog, &bindings, || true).is_err());
        let model_path = crate::storage::local_data()
            .unwrap()
            .join("papyrus-decor-model-cache")
            .join(&vines.source.sha256)
            .join("models.json");
        std::fs::write(
            path("MAPPER_SCENERY_MODEL_TEST_REPORT"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "base_models_sha256":world.base.sha256,"vine_models_sha256":vines.source.sha256,
                "vine_binding":vines.binding,"vine_models":vines.source.models.len(),
                "vine_models_path":model_path,"model_counts":world.model_counts(),
                "normal_preparation_and_cache_reopen_passed":true,
                "runtime_game_loading_tested":false
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires an owned client and metadata runtime"]
    fn owned_current_scenery_models_prepare_and_reopen_through_normal_cache() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_SCENERY_MODEL_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_SCENERY_MODEL_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let bindings = Bindings::load(
            &root,
            &path("MAPPER_SCENERY_MODEL_TEST_BINDINGS"),
            &catalog,
            || false,
        )
        .unwrap();
        let project = crate::Project::load(&path("MAPPER_SCENERY_MODEL_TEST_PROJECT")).unwrap();
        let source = decor_source::Source::cached(
            &catalog,
            &bindings,
            project.decoration.as_ref().unwrap(),
            || false,
        )
        .unwrap();
        let models = prepare(&root, &source, &catalog, &bindings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(models.models.len(), source.prefab_paths().count());
        assert!(
            models
                .models
                .values()
                .any(|model| !model.triangles.is_empty())
        );
        let reopened = Source::discover(&source, &catalog, &bindings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.sha256, models.sha256);
        let report = serde_json::json!({
            "format": "mapper-owned-scenery-model-preparation",
            "schema": 1,
            "decoration_sha256": source.sha256,
            "models_sha256": models.sha256,
            "roster": models.models.len(),
            "nonempty_models": models.models.values().filter(|m| !m.triangles.is_empty()).count(),
            "complete_models": models.models.values().filter(|m| m.complete).count(),
            "issue_count": models.models.values().map(|m| m.issues.len()).sum::<usize>(),
            "normal_preparation_and_cache_reopen_passed": true,
            "runtime_assembly_validated": false,
        });
        std::fs::write(
            path("MAPPER_SCENERY_MODEL_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    fn fixture() -> (Catalog, serde_json::Value) {
        let receipt = SourceFile {
            relative_path: "Bundles/shared/assetscenes.bundle".into(),
            sha256: "a".repeat(64),
            bytes: 1,
        };
        let catalog = Catalog {
            sha256: "b".repeat(64),
            installation: crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: receipt.clone(),
                scene_manifest: None,
            },
            sources: vec![receipt],
            entries: Default::default(),
        };
        let document = serde_json::json!({"format":"papyrus-decor-model-source","schema":1,"adapter":"papyrus-decor-static-lod-1","decoration_sha256":"c".repeat(64),"sources":catalog.sources,"extra_sources":[],"extractor_sha256":"d".repeat(64),"assembly_validated":false,
            "models":{"a.prefab":{"vertices":[[0,0,0],[1,0,0],[0,1,0]],"triangles":[[0,1,2]],"issues":[],"complete":true,"simplified":false}}});
        (catalog, document)
    }
    #[test]
    fn models_require_exact_source_roster_receipts_bounded_geometry_and_cancellation() {
        let (catalog, doc) = fixture();
        let decode = |d: &serde_json::Value| {
            Source::decode(
                &serde_json::to_vec(d).unwrap(),
                &catalog,
                &[],
                &"c".repeat(64),
                BTreeSet::from(["a.prefab"]),
                || false,
            )
        };
        let source = decode(&doc).unwrap();
        assert_eq!(source.models.len(), 1);
        assert!(
            Source::decode(
                &serde_json::to_vec(&doc).unwrap(),
                &catalog,
                &[],
                &"c".repeat(64),
                BTreeSet::from(["a.prefab"]),
                || true
            )
            .is_err()
        );
        for (pointer, value) in [
            ("/decoration_sha256", serde_json::json!("e".repeat(64))),
            ("/assembly_validated", serde_json::json!(true)),
            ("/models/a.prefab/triangles/0/2", serde_json::json!(3)),
            ("/models/a.prefab/vertices/0/0", serde_json::json!(10001)),
            (
                "/models/a.prefab/issues",
                serde_json::json!(["missing geometry"]),
            ),
            (
                "/models/a.prefab/triangles",
                serde_json::to_value(vec![[0, 1, 2]; 257]).unwrap(),
            ),
            (
                "/extra_sources",
                serde_json::json!([{"relative_path":"../content.bundle","sha256":"f".repeat(64),"bytes":1}]),
            ),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&changed).is_err(), "accepted {pointer}");
        }
        let mut incomplete = doc.clone();
        incomplete["models"]["a.prefab"]["complete"] = false.into();
        incomplete["models"]["a.prefab"]["issues"] = serde_json::json!(["renderer missing"]);
        assert!(source.quality() > decode(&incomplete).unwrap().quality());
        let mut changed = doc;
        changed["models"]["unknown.prefab"] = changed["models"]["a.prefab"].clone();
        assert!(decode(&changed).is_err());
    }
    fn report() -> papyrus_terrain::decor::Report {
        use papyrus_terrain::{decor, decor_fit, footprints};
        let pose = |candidate, position| decor_fit::Pose {
            candidate,
            position,
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        };
        decor::Report {
            engine: decor::VERSION,
            source_sha256: "c".repeat(64),
            seed: 1,
            candidates: vec![
                decor::Candidate {
                    stage: 0,
                    prefab: 0,
                    position: [0.; 3],
                },
                decor::Candidate {
                    stage: 0,
                    prefab: 0,
                    position: [10.; 3],
                },
            ],
            stages: vec![decor::StageReport {
                id: "a".into(),
                name: "a".into(),
                seed: 1,
                requested_attempts: 2,
                attempted: 2,
                candidates: 2,
                passing_candidates: 2,
                rejected_filter: 0,
                rejected_bounds: 0,
                rejected_holes: 0,
                skipped: None,
                budget_exhausted: false,
                prefabs: vec!["a.prefab".into()],
            }],
            validated_instances: 0,
            budget_exhausted: false,
            source_issues: vec![],
            pending_checks: vec![],
            fitting: Some(decor_fit::Report {
                engine: decor_fit::VERSION,
                poses: vec![pose(0, [0.; 3]), pose(1, [10.; 3])],
                counts: Default::default(),
                prefabs: BTreeMap::new(),
                height_samples: 0,
                sample_visits: 0,
                unprocessed_candidates: 0,
                budget_exhausted: false,
                physics_validated: false,
            }),
            terrain_placement: Some(footprints::Report {
                height_reserved_samples: None,
                engine: footprints::VERSION,
                resolution: 512,
                protected_topology: 1,
                permitted_role_exceptions: None,
                permitted_material_role_exceptions: None,
                maximum_work: None,
                protection_scopes: None,
                counts: Default::default(),
                prefabs: BTreeMap::new(),
                applied: vec![footprints::Applied {
                    pose: 1,
                    height_writes: 0,
                    reserved_cells: 1,
                    maximum_fill: 0.,
                    blocked_cells: None,
                    protection_exception: None,
                    protection_scope: None,
                }],
                height_writes: 0,
                reserved_cells: 1,
                initial_protected_cells: 0,
                sample_visits: 0,
                unprocessed_poses: 0,
                budget_exhausted: false,
                physics_validated: false,
                blocking: None,
                topology: None,
                materials: None,
                height_effects: None,
                collider_effects: None,
                occupancy: vec![],
            }),
        }
    }
    #[test]
    fn world_models_keep_vine_receipts_and_committed_poses_separate() {
        use papyrus_terrain::{object_scene, placement_blockers};
        let (catalog, doc) = fixture();
        let decode = |doc: &serde_json::Value, path| {
            Source::decode(
                &serde_json::to_vec(doc).unwrap(),
                &catalog,
                &[],
                &"c".repeat(64),
                BTreeSet::from([path]),
                || false,
            )
            .unwrap()
        };
        let base = decode(&doc, "a.prefab");
        let mut other = doc.clone();
        let model = other["models"]
            .as_object_mut()
            .unwrap()
            .remove("a.prefab")
            .unwrap();
        other["models"]["vine.prefab"] = model;
        let world = World {
            catalog: catalog.sha256.clone(),
            bindings: "f".repeat(64),
            base: Arc::new(base),
            vines: Some(VineModels {
                binding: "d".repeat(64),
                road: "a".repeat(64),
                source: Arc::new(decode(&other, "vine.prefab")),
            }),
        };
        assert_eq!(world.model_counts(), (2, 2));
        let instance = |id, prefab: &str| object_scene::Instance {
            id,
            kind: object_scene::Kind::Decoration,
            road: None,
            prefab: prefab.into(),
            pose: papyrus_terrain::decor_fit::Pose {
                candidate: id,
                position: [id as f32, 20., -30.],
                rotation: [0., 1., 0., 0.],
                scale: [2.; 3],
            },
        };
        let mut scene = object_scene::Scene {
            utility: None,
            initial_rail_reservations: None,
            engine: crate::object_generation::VERSION,
            road_seed: 1,
            decoration_seed: 2,
            road_source_sha256: "a".repeat(64),
            decoration_source_sha256: "c".repeat(64),
            uniform_cliffs_source_sha256: None,
            coastal_cliffs_source_sha256: None,
            jungle_vines_source_sha256: Some("d".repeat(64)),
            monument_source_sha256: "b".repeat(64),
            initial_blockers_sha256: None,
            initial_blockers: placement_blockers::Report {
                engine: placement_blockers::VERSION,
                instances: 0,
                modifiers: 0,
                clipped_modifiers: 0,
                sample_visits: 0,
                blocked_cells: 0,
            },
            instances: vec![
                instance(1, "a.prefab"),
                instance(2, "vine.prefab"),
                instance(3, "unknown"),
            ],
            sockets: Vec::new(),
            decoration: Vec::new(),
            terrain: report().terrain_placement.unwrap(),
            external_phases: Vec::new(),
            assembled_instances: 0,
            physics_validated: false,
        };
        let instances = world.object_instances(&scene);
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[1].pose.position, [2., 20., -30.]);
        assert_eq!(instances[1].pose.rotation, [0., 1., 0., 0.]);
        assert_eq!(instances[1].pose.scale, [2.; 3]);
        scene.jungle_vines_source_sha256 = Some("e".repeat(64));
        assert_eq!(world.object_instances(&scene).len(), 1);
        scene.jungle_vines_source_sha256 = None;
        assert_eq!(world.object_instances(&scene).len(), 1);
        scene.jungle_vines_source_sha256 = Some("d".repeat(64));
        scene.road_source_sha256 = "e".repeat(64);
        assert_eq!(world.object_instances(&scene).len(), 1);
        scene.road_source_sha256 = "a".repeat(64);
        scene.instances[1].kind = object_scene::Kind::Road;
        assert_eq!(world.object_instances(&scene).len(), 1);
        scene.decoration_source_sha256 = "f".repeat(64);
        assert!(world.object_instances(&scene).is_empty());
    }

    #[test]
    fn two_recipes_and_vine_switches_share_the_same_model_allocations() {
        let directory = tempfile::tempdir().unwrap();
        let (catalog, receipt, _, _) = crate::prefab_textures::tests::fixture(directory.path());
        let bindings = Bindings::load(directory.path(), &receipt, &catalog, || false).unwrap();
        let (decode_catalog, document) = fixture();
        let base = Arc::new(Source::decode(
            &serde_json::to_vec(&document).unwrap(), &decode_catalog, &[], &"c".repeat(64),
            BTreeSet::from(["a.prefab"]), || false,
        ).unwrap());
        let vine = Arc::new(Source {
            sha256: "e".repeat(64),
            decoration_sha256: "d".repeat(64),
            models: base.models.clone(),
        });
        let mut objects = crate::object_generation::Recipe::new("a".repeat(64));
        objects.jungle_vines_source_sha256 = Some("d".repeat(64));
        let mut recipe = crate::Project {
            decoration: Some(decor_source::Recipe::new(base.decoration_sha256.clone())),
            master_seed: 3456,
            ..Default::default()
        };
        let retained = World {
            catalog: catalog.sha256.clone(),
            bindings: bindings.sha256.clone(),
            base: base.clone(),
            vines: Some(VineModels {
                binding: "d".repeat(64),
                road: objects.road_source_sha256.clone(),
                source: vine.clone(),
            }),
        };
        // This installation fixture has no model receipts. Both calls succeed
        // only if the loader can use the geometry already in memory.
        let first = prepare_project_reusing(&recipe, &catalog, &bindings, Some(&retained), || false)
            .unwrap().unwrap();
        recipe.master_seed = 7890;
        recipe.objects = Some(objects);
        let second = prepare_project_reusing(&recipe, &catalog, &bindings, Some(&first), || false)
            .unwrap().unwrap();
        assert!(second.covers(&recipe));
        assert!(Arc::ptr_eq(&first.base, &second.base));
        assert!(Arc::ptr_eq(&second.vines.unwrap().source, &vine));
    }

    #[test]
    fn only_committed_matching_poses_are_visible_and_removal_hides_old_geometry() {
        let (catalog, doc) = fixture();
        let source = Source::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &catalog,
            &[],
            &"c".repeat(64),
            BTreeSet::from(["a.prefab"]),
            || false,
        )
        .unwrap();
        let mut report = report();
        let instances = source.instances(&report);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].pose.position, [10.; 3]);
        report.terrain_placement.as_mut().unwrap().applied.clear();
        assert!(source.instances(&report).is_empty());
        report
            .terrain_placement
            .as_mut()
            .unwrap()
            .applied
            .push(papyrus_terrain::footprints::Applied {
                pose: 999,
                height_writes: 0,
                reserved_cells: 0,
                maximum_fill: 0.,
                blocked_cells: None,
                protection_exception: None,
                protection_scope: None,
            });
        assert!(source.instances(&report).is_empty());
        report.source_sha256 = "f".repeat(64);
        assert!(source.instances(&report).is_empty());
    }
}
