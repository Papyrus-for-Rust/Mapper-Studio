//! Version-bound river recipes and verified installation source snapshots.
use crate::{installation::Installation, river_mesh_source};
use papyrus_terrain::{river_carve, river_layout, river_pipeline, river_surface};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, sync::Arc};

pub(crate) const PROFILE: &str =
    include_str!("../../../compatibility/rivers/client-0f703d43-generation.json");
const CURRENT_PROFILE: &str =
    include_str!("../../../compatibility/rivers/client-8dd4e235-generation.json");
const PROFILES: [&str; 3] = [
    PROFILE,
    CURRENT_PROFILE,
    include_str!("../../../compatibility/rivers/client-58f3263f-generation.json"),
];
pub(crate) fn profile_for(installation: &Installation) -> Result<serde_json::Value, String> {
    installation.validate()?;
    if installation.kind != crate::installation::InstallationKind::Client {
        return Err("No river generation adapter for this installation".into());
    }
    for raw in PROFILES {
        let profile: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if profile["mesh_profile"]["native_sha256"].as_str()
            == Some(installation.native.sha256.as_str())
        {
            return Ok(profile);
        }
    }
    Err("No river generation adapter for this installation".into())
}
pub(crate) const LIMIT: usize = 20 * 1024 * 1024;
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub engine: String,
    pub source_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_document: Option<Arc<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objects: Option<crate::river_objects::Recipe>,
}
impl Recipe {
    pub(crate) fn verify_world(&self, world: &river_pipeline::World, seed: u32) -> Result<(), String> {
        if world.report.engine != self.engine
            || world.report.source_sha256.as_deref() != Some(self.source_sha256.as_str())
            || world.report.layout.seed != Some(seed)
        {
            return Err("River source or seed differs from the recipe".into());
        }
        if self.objects.as_ref().map(|r| &r.source_sha256)
            != world.objects.as_ref().map(|r| &r.source_sha256)
        {
            return Err("Requested river objects differ from the generated world".into());
        }
        if world.objects.as_ref().is_some_and(|scene| {
            scene.seed != seed
                || scene
                    .instances
                    .iter()
                    .any(|instance| instance.river >= world.paths.len())
        }) {
            return Err("River objects differ from the accepted river paths or seed".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(objects) = &self.objects {
            objects.validate()?;
        }
        if self.engine != river_pipeline::VERSION
            || !digest(&self.source_sha256)
            || self
                .source_document
                .as_ref()
                .is_some_and(|raw| raw.is_empty() || raw.len() > LIMIT)
        {
            return Err("Invalid river generation recipe".into());
        }
        Ok(())
    }
    pub fn prepare_export(&self, installation: &Installation) -> Result<Self, String> {
        self.validate()?;
        let raw = if let Some(raw) = &self.source_document {
            raw.clone()
        } else {
            let path = crate::client_cache::pinned_document(
                &crate::storage::local_data()?,
                "papyrus-river-cache",
                &self.source_sha256,
                "rivers.json",
            )?;
            String::from_utf8(Source::read_document(&path).map_err(|error| {
                format!("Cannot export river data {} from {}. Restore its cache entry or prepare rivers again from the matching Rust installation. {error}", self.source_sha256, path.display())
            })?).map_err(|e| e.to_string())?.into()
        };
        let source = Source::decode(raw.as_bytes(), installation)?;
        if source.sha256 != self.source_sha256 {
            return Err("Saved river source digest changed".into());
        }
        let mut recipe = source.recipe();
        recipe.objects = self
            .objects
            .as_ref()
            .map(crate::river_objects::Recipe::prepare_export)
            .transpose()?;
        Ok(recipe)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub recipe: Recipe,
    pub seed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u8>,
}
impl Key {
    pub fn validate(&self) -> Result<(), String> {
        self.recipe.validate()?;
        if !(1..=crate::MAX_SEED).contains(&self.seed) || self.limit.is_some_and(|limit| limit > 64) {
            return Err("Invalid independent river seed".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    format: String,
    schema: u32,
    id: String,
    reference_assembly_sha256: String,
    scope: String,
    pub(crate) mesh_profile: serde_json::Value,
    large_world_above: u32,
    large_world_maximum_rivers: usize,
    terrain_origin_y: f32,
    terrain_height: f32,
    pub(crate) layout: river_layout::Policy,
    pub(crate) carve: river_carve::Policy,
    pub(crate) surface: river_surface::Policy,
}
impl Profile {
    pub(crate) fn layout_for_recipe(&self, size: u32, limit: Option<u8>) -> river_layout::Policy {
        let mut layout = self.layout.clone();
        if let Some(limit) = limit {
            layout.maximum_rivers = usize::from(limit);
        } else if size > self.large_world_above {
            layout.maximum_rivers = self.large_world_maximum_rivers;
        }
        layout
    }
    fn validate(&self) -> Result<(), String> {
        self.layout.validate()?;
        self.carve.validate()?;
        self.surface.validate()?;
        if self.format != "papyrus-river-generation-profile"
            || self.schema != 1
            || self.id.is_empty()
            || self.id.len() > 128
            || self.scope.is_empty()
            || self.scope.len() > 4096
            || !digest(&self.reference_assembly_sha256)
            || !(1000..=6000).contains(&self.large_world_above)
            || self.large_world_maximum_rivers > 64
            || self.terrain_origin_y != -500.
            || self.terrain_height != 1000.
        {
            return Err("Unsupported river generation profile".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    profile: serde_json::Value,
    mesh_source: serde_json::Value,
}
pub struct Source {
    pub sha256: String,
    pub(crate) mesh: river_mesh_source::Source,
    pub(crate) profile: Profile,
    raw: Arc<str>,
}
impl Source {
    fn read_document(path: &Path) -> Result<Vec<u8>, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > LIMIT {
            return Err("River generation source exceeds 20 MiB".into());
        }
        Ok(raw)
    }
    pub(crate) fn decode(raw: &[u8], installation: &Installation) -> Result<Self, String> {
        Self::decode_profile(raw, installation, &profile_for(installation)?)
    }
    fn decode_profile(
        raw: &[u8],
        installation: &Installation,
        expected: &serde_json::Value,
    ) -> Result<Self, String> {
        if raw.len() > LIMIT {
            return Err("River generation source exceeds 20 MiB".into());
        }
        let doc: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid river source: {e}"))?;
        if doc.format != "papyrus-river-generation-source"
            || doc.schema != 1
            || &doc.profile != expected
        {
            return Err("No matching version-bound river adapter for this source".into());
        }
        let profile: Profile = serde_json::from_value(doc.profile).map_err(|e| e.to_string())?;
        profile.validate()?;
        if doc.mesh_source.get("profile") != Some(&profile.mesh_profile) {
            return Err(
                "River geometry and generation policies have different installation bindings"
                    .into(),
            );
        }
        let mesh = river_mesh_source::Source::decode(
            &serde_json::to_vec(&doc.mesh_source).map_err(|e| e.to_string())?,
            installation,
        )?;
        if mesh.reference_assembly() != profile.reference_assembly_sha256 {
            return Err(
                "River kernels and water template have different reference versions".into(),
            );
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            mesh,
            profile,
            raw: std::str::from_utf8(raw).map_err(|e| e.to_string())?.into(),
        })
    }
    pub fn load(
        root: &Path,
        path: &Path,
        installation: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let raw = Self::read_document(path)?;
        let source = Self::decode(&raw, installation)?;
        let verified = source.mesh.verify(root, &cancelled);
        if cancelled() {
            return Ok(None);
        }
        if !verified? {
            return Ok(None);
        }
        Ok(Some(source))
    }
    pub fn cached(
        root: &Path,
        installation: &Installation,
        recipe: &Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if recipe.source_document.is_some() {
            return Self::load_recipe(root, None, installation, recipe, cancelled);
        }
        let path = crate::client_cache::pinned_document(
            &crate::storage::local_data()?,
            "papyrus-river-cache",
            &recipe.source_sha256,
            "rivers.json",
        )?;
        Self::load_recipe(root, Some(&path), installation, recipe, cancelled)
    }
    pub(crate) fn load_recipe(
        root: &Path,
        path: Option<&Path>,
        installation: &Installation,
        recipe: &Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        recipe.validate()?;
        if cancelled() {
            return Ok(None);
        }
        let source = if let Some(raw) = &recipe.source_document {
            let source = Self::decode(raw.as_bytes(), installation)?;
            if cancelled() {
                return Ok(None);
            }
            source
        } else {
            let path = path.ok_or("Saved river cache path is unavailable")?;
            let Some(source) = Self::load(root, path, installation, &cancelled).map_err(|error| {
                format!("Cannot load saved river data {} from {}. Restore its cache entry or prepare rivers again from the matching Rust installation. {error}", recipe.source_sha256, path.display())
            })? else {
                return Ok(None);
            };
            source
        };
        if source.sha256 != recipe.source_sha256 {
            return Err("Saved river source digest changed".into());
        }
        if recipe.source_document.is_some() {
            let verified = source.mesh.verify(root, &cancelled);
            if cancelled() || !verified? {
                return Ok(None);
            }
        }
        Ok(Some(source))
    }
    pub fn recipe(&self) -> Recipe {
        Recipe {
            engine: river_pipeline::VERSION.into(),
            source_sha256: self.sha256.clone(),
            source_document: Some(self.raw.clone()),
            objects: None,
        }
    }
    pub fn generate(
        &self,
        layers: &papyrus_terrain::native_layers::Layers,
        seed: u32,
        sampler: impl FnMut(river_pipeline::Stage, &[f32]) -> Result<Option<Vec<f32>>, String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<river_pipeline::Output>, String> {
        self.generate_with_topology(layers, &layers.topology, seed, None, sampler, cancelled)
    }

    pub(crate) fn generate_with_topology(
        &self,
        layers: &papyrus_terrain::native_layers::Layers,
        classified_topology: &[i32],
        seed: u32,
        limit: Option<u8>,
        sampler: impl FnMut(river_pipeline::Stage, &[f32]) -> Result<Option<Vec<f32>>, String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<river_pipeline::Output>, String> {
        Key {
            recipe: self.recipe(),
            seed,
            limit,
        }
        .validate()?;
        if cancelled() {
            return Ok(None);
        }
        let (hr, _) = papyrus_terrain::native_layers::dimensions(layers.size)?;
        if layers.height_resolution != hr || layers.height.len() != hr * hr {
            return Err("River source does not match native terrain dimensions".into());
        }
        let layout = self.profile.layout_for_recipe(layers.size, limit);
        let dry = vec![0; layers.height.len()];
        let mut output = river_pipeline::generate_with_topology(
            layers,
            classified_topology,
            &dry,
            river_pipeline::Policies {
                layout: &layout,
                carve: &self.profile.carve,
                surface: &self.profile.surface,
                water: self.mesh.policy(),
                template: &self.mesh.mesh.buffer,
            },
            river_pipeline::Settings {
                seed: Some(seed),
                maximum_stage_work: 512_000_000,
                maximum_candidates: 4096,
                maximum_controls: 1_048_576,
                water: papyrus_terrain::river_water::Settings {
                    maximum_work: 128_000_000,
                    maximum_chunks: 4096,
                    maximum_geometry_bytes: 64 * 1024 * 1024,
                },
            },
            sampler,
            cancelled,
        )?;
        if let Some(output) = &mut output {
            output.report.source_sha256 = Some(self.sha256.clone());
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effective_river_policy_uses_the_source_size_threshold_for_generation_and_export() {
        for raw in PROFILES {
            let profile: Profile = serde_json::from_str(raw).unwrap();
            let boundary = profile.large_world_above;
            assert_eq!(profile.layout_for_recipe(boundary, None), profile.layout);
            let large = profile.layout_for_recipe(boundary + 1, None);
            for size in [boundary, boundary + 1] {
                for count in [0, 8] {
                    assert_eq!(
                        profile.layout_for_recipe(size, Some(count)).maximum_rivers,
                        usize::from(count),
                    );
                }
            }
            assert_eq!(large.maximum_rivers, profile.large_world_maximum_rivers);
            let mut expected = profile.layout.clone();
            expected.maximum_rivers = profile.large_world_maximum_rivers;
            assert_eq!(large, expected);
        }
    }
    #[test]
    fn generation_profile_selects_the_matching_installation_and_rejects_unknown_versions() {
        let directory = tempfile::tempdir().unwrap();
        let (mut installation, _) = crate::river_mesh_source::tests::fixture(directory.path());
        for raw in PROFILES {
            let profile: serde_json::Value = serde_json::from_str(raw).unwrap();
            installation.native.sha256 = profile["mesh_profile"]["native_sha256"]
                .as_str()
                .unwrap()
                .into();
            assert_eq!(profile_for(&installation).unwrap(), profile);
            let parsed: Profile = serde_json::from_value(profile).unwrap();
            parsed.validate().unwrap();
        }
        installation.native.sha256 = "a".repeat(64);
        assert!(profile_for(&installation).is_err());
    }
    #[test]
    fn complete_snapshot_binds_policies_template_and_installation_receipts() {
        let root = tempfile::tempdir().unwrap();
        let (installation, mesh) = crate::river_mesh_source::tests::fixture(root.path());
        let mut profile: serde_json::Value = serde_json::from_str(PROFILE).unwrap();
        profile["mesh_profile"] = mesh["profile"].clone();
        let doc = serde_json::json!({"format":"papyrus-river-generation-source","schema":1,"profile":profile,"mesh_source":mesh});
        let bytes = serde_json::to_vec(&doc).unwrap();
        let source = Source::decode_profile(&bytes, &installation, &profile).unwrap();
        source.recipe().validate().unwrap();
        let recipe = source.recipe();
        assert_eq!(recipe.source_document.as_ref().unwrap().as_bytes(), bytes);
        assert!(Arc::ptr_eq(
            recipe.source_document.as_ref().unwrap(),
            recipe.clone().source_document.as_ref().unwrap()
        ));
        let restored: Recipe =
            serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
        assert_eq!(restored, recipe);
        assert!(source.mesh.verify(root.path(), || false).unwrap());
        assert!(!source.mesh.verify(root.path(), || true).unwrap());
        let (height_resolution, surface_resolution) =
            papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution,
            surface_resolution,
            height: vec![0; height_resolution.pow(2)],
            topology: vec![0; surface_resolution.pow(2)],
            splat: vec![0; surface_resolution.pow(2) * 8],
            biome: vec![0; surface_resolution.pow(2) * 5],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.splat[..surface_resolution.pow(2)].fill(255);
        layers.biome[..surface_resolution.pow(2)].fill(255);
        let (_, mut world) = source
            .generate(
                &layers,
                37,
                |_, coordinates| Ok(Some(vec![0.; coordinates.len()])),
                || false,
            )
            .unwrap()
            .unwrap()
            .into_parts();
        assert!(world.paths.is_empty());
        recipe.verify_world(&world, 37).unwrap();
        assert!(recipe.verify_world(&world, 38).is_err());
        let mut with_objects = recipe.clone();
        with_objects.objects = Some(crate::river_objects::Recipe {
            source_sha256: "b".repeat(64),
            infrastructure_sha256: "c".repeat(64),
            catalog_sha256: "d".repeat(64),
            documents: None,
        });
        assert!(with_objects.verify_world(&world, 37).is_err());
        world.report.source_sha256 = None;
        assert!(recipe.verify_world(&world, 37).is_err());

        for (path, value) in [
            ("/profile/layout/step", serde_json::json!(8)),
            (
                "/mesh_source/profile/policy/mesh_offset",
                serde_json::json!(-2),
            ),
            (
                "/mesh_source/mesh/reference/path_id",
                serde_json::json!(123),
            ),
            (
                "/mesh_source/sources/0/sha256",
                serde_json::json!("f".repeat(64)),
            ),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                Source::decode_profile(
                    &serde_json::to_vec(&changed).unwrap(),
                    &installation,
                    &profile
                )
                .is_err()
            );
        }
        assert!(Source::decode(&bytes, &installation).is_err()); // No fake installation adapter.
        std::fs::write(root.path().join("GameAssembly.dll"), b"changed").unwrap();
        assert!(source.mesh.verify(root.path(), || false).is_err());
    }
    #[test]
    fn legacy_recipe_keeps_its_shape_and_missing_cache_errors_identify_the_pin() {
        let root = tempfile::tempdir().unwrap();
        let (installation, _) = crate::river_mesh_source::tests::fixture(root.path());
        let recipe = Recipe {
            engine: river_pipeline::VERSION.into(),
            source_sha256: "a".repeat(64),
            source_document: None,
            objects: None,
        };
        let encoded = serde_json::to_value(&recipe).unwrap();
        assert_eq!(encoded.as_object().unwrap().len(), 2);
        let restored: Recipe = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, recipe);
        let missing = root.path().join("absent/rivers.json");
        assert!(
            Source::load_recipe(root.path(), Some(&missing), &installation, &recipe, || true)
                .unwrap()
                .is_none()
        );
        let error =
            Source::load_recipe(root.path(), Some(&missing), &installation, &recipe, || {
                false
            })
            .err()
            .unwrap();
        assert!(error.contains("Cannot load saved river data"));
        assert!(error.contains(&recipe.source_sha256));
        assert!(error.contains(&missing.display().to_string()));
        assert!(!missing.exists());
    }
    #[test]
    fn river_recipe_identity_roundtrips_without_rerolling_raw_terrain_or_monuments() {
        let mut project = crate::Project {
            preview_engine: crate::native_preview::VERSION.into(),
            installation: Some(Installation {
                kind: crate::installation::InstallationKind::Client,
                native: crate::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "0f703d43b57672b8bb3dfd29bab0d484f8622fbac66dee7f1f058893ef22a99c"
                        .into(),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let raw = project.terrain_key();
        assert!(!serde_json::to_string(&raw).unwrap().contains("rivers"));
        project.rivers = Some(Recipe {
            engine: river_pipeline::VERSION.into(),
            source_sha256: "a".repeat(64),
            source_document: None,
            objects: None,
        });
        project.validate().unwrap();
        let first = project.terrain_key();
        assert_ne!(first, raw);
        assert_eq!(crate::native_preview::source_key(&first), raw);
        project.seeds[2].value += 1;
        project.seeds[2].inherited = false;
        assert_ne!(project.terrain_key(), first);
        assert_eq!(
            crate::native_preview::source_key(&project.terrain_key()),
            raw
        );
        let rivers = project.terrain_key();
        project.targets[0].x += 10.;
        project.seeds[4].value += 1;
        project.seeds[4].inherited = false;
        assert_eq!(project.terrain_key(), rivers);
        let profile = crate::profile::Profile::new("Rivers".into(), project.clone());
        profile.validate().unwrap();
        let restored: crate::profile::Profile =
            serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
        assert_eq!(restored.recipe, project);
        let mut bad = project.clone();
        bad.preview_engine = papyrus_terrain::landscape::VERSION.into();
        assert!(bad.validate().is_err());
        let mut bad = project.clone();
        bad.rivers.as_mut().unwrap().source_sha256 = "../outside".into();
        assert!(bad.validate().is_err());
        project.rivers = None;
        assert_eq!(project.terrain_key(), raw);
    }
}
