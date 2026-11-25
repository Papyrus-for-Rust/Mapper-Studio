//! Installation-bound terrain color metadata. Presentation only; never terrain state.
use crate::installation::{self, Installation, SourceFile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs::File, io::Read, path::Path};

const LIMIT: u64 = 32 * 1024;
const BIOMES: [&str; 5] = ["Arid", "Temperate", "Tundra", "Arctic", "Jungle"];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub scene_file: String,
    pub world_setup: i64,
    pub terrain: i64,
    pub generator: i64,
    pub config_file: String,
    pub config_object: i64,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Material {
    pub name: String,
    pub biome_colors: Vec<[f32; 4]>,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    extractor_sha256: String,
    sources: Vec<SourceFile>,
    scene: String,
    binding: Binding,
    config_paths: Vec<String>,
    numeric_space: String,
    biomes: Vec<String>,
    materials: Vec<Material>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cartography: Option<crate::map_colors::Material>,
}
#[derive(Clone, Debug)]
pub struct Palette {
    pub sha256: String,
    doc: Document,
    raw: std::sync::Arc<[u8]>,
    display_colors: crate::map_colors::Preferences,
    effective_map: Option<crate::map_colors::Material>,
    surface_profile: Option<crate::map_surface_frame::Profile>,
    surface_cache: std::sync::Arc<crate::map_surface_frame::Cache>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    raw: Vec<u8>,
    colors: crate::map_colors::Preferences,
    profile: Option<String>,
}
impl PartialEq for Palette {
    fn eq(&self, other: &Self) -> bool {
        self.sha256 == other.sha256
            && self.doc == other.doc
            && self.raw == other.raw
            && self.display_colors == other.display_colors
            && self.effective_map == other.effective_map
            && self.surface_profile == other.surface_profile
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn name(s: &str, limit: usize) -> bool {
    !s.trim().is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
}
fn same_cartography(
    a: &Option<crate::map_colors::Material>,
    b: &Option<crate::map_colors::Material>,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let mut old = a.clone();
            let mut new = b.clone();
            // Old captures remain readable, but newly inspected shader identity
            // enriches only otherwise identical source metadata. Conflicting
            // nonempty fingerprints cannot hide behind this migration.
            if old.shader_blob_sha256.is_none() || new.shader_blob_sha256.is_none() {
                old.shader_blob_sha256 = None;
                new.shader_blob_sha256 = None;
            }
            old == new
        }
        (None, None) => true,
        _ => false,
    }
}
fn publish_palette(folder: &Path, destination: &Path, expected: &[u8]) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(folder, destination) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let mut raw = Vec::new();
                if File::open(destination.join("palette.json"))
                    .and_then(|f| f.take(LIMIT + 1).read_to_end(&mut raw))
                    .is_ok()
                    && raw.as_slice() == expected
                {
                    return Ok(());
                }
                if !cfg!(windows)
                    || !matches!(error.raw_os_error(), Some(5 | 32 | 33))
                    || attempt == 7
                {
                    return Err(error.to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis((25 << attempt).min(250)));
                attempt += 1;
            }
        }
    }
}
impl Palette {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            raw: self.raw.to_vec(),
            colors: self.display_colors.clone(),
            profile: self.surface_profile.map(|profile| profile.key().to_owned()),
        }
    }
    /// Compare the saved display state without decoding the same palette again.
    pub fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        self.display_colors == snapshot.colors
            && self.surface_profile.map(|profile| profile.key()) == snapshot.profile.as_deref()
            && self.bytes() == snapshot.raw.as_slice()
    }
    pub fn from_snapshot(snapshot: Snapshot, installation: &Installation) -> Result<Self, String> {
        let mut palette = Self::decode(&snapshot.raw, installation)?;
        palette.surface_profile = match snapshot.profile.as_deref() {
            None => None,
            Some(value) if value == crate::map_surface_frame::Profile::Legacy.key() => {
                Some(crate::map_surface_frame::Profile::Legacy)
            }
            Some(value) if value == crate::map_surface_frame::Profile::Current.key() => {
                Some(crate::map_surface_frame::Profile::Current)
            }
            _ => return Err("Unsupported saved terrain display profile".into()),
        };
        Ok(palette.with_display_colors(&snapshot.colors))
    }
    pub(crate) fn persist_in(&self, cache: &Path) -> Result<(), String> {
        if !hash(&self.sha256) || format!("{:x}", Sha256::digest(&self.raw)) != self.sha256 {
            return Err("Terrain palette identity changed before publication".into());
        }
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let destination = cache.join(&self.sha256);
        if destination.exists() {
            let mut raw = Vec::new();
            File::open(destination.join("palette.json"))
                .map_err(|e| e.to_string())?
                .take(LIMIT + 1)
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
            return if raw.as_slice() == self.bytes() {
                Ok(())
            } else {
                Err("Existing terrain palette cache differs".into())
            };
        }
        let staging = tempfile::Builder::new()
            .prefix(".palette-publish-")
            .tempdir_in(cache)
            .map_err(|e| e.to_string())?;
        let folder = staging.path().join(&self.sha256);
        std::fs::create_dir(&folder).map_err(|e| e.to_string())?;
        std::fs::write(folder.join("palette.json"), self.bytes()).map_err(|e| e.to_string())?;
        publish_palette(&folder, &destination, self.bytes())
    }
    pub fn discover(
        root: &Path,
        expected: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::discover_in(
            &crate::storage::local_data()?.join("papyrus-terrain-palette-cache"),
            root,
            expected,
            cancelled,
        )
    }
    pub(crate) fn discover_in(
        cache: &Path,
        root: &Path,
        expected: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() || !cache.is_dir() {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        for (i, entry) in std::fs::read_dir(cache)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if cancelled() {
                return Ok(None);
            }
            if i >= 256 {
                return Err("Terrain palette cache exceeds discovery budget".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !hash(&id) || !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let path = entry.path().join("palette.json");
            let Ok(file) = File::open(path) else {
                continue;
            };
            let mut raw = Vec::new();
            file.take(LIMIT + 1)
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
            if let Ok(palette) = Self::decode(&raw, expected)
                && palette.sha256 == id
            {
                candidates.push(palette);
            }
        }
        if candidates.is_empty() {
            return Ok(None);
        }
        // Reject impossible candidates once per source, smallest first. An
        // updated maps bundle can rule out a palette before the content bundle
        // needs to be opened. Only actual hashes can authorize reuse.
        let _verification = installation::VerificationScope::default();
        for (i, (path, limit)) in [
            (expected.native.relative_path.as_str(), 64 * 1024 * 1024u64),
            ("Bundles/maps/maps.bundle", 512 * 1024 * 1024),
            ("Bundles/shared/content.bundle", 8 * 1024 * 1024 * 1024),
        ]
        .into_iter()
        .enumerate()
        {
            if cancelled() {
                return Ok(None);
            }
            if let Ok(metadata) = std::fs::metadata(root.join(path))
                && metadata.is_file()
                && (1..=limit).contains(&metadata.len())
                && !candidates
                    .iter()
                    .any(|p| p.doc.sources[i].bytes == metadata.len())
            {
                return Ok(None);
            }
            let actual = installation::hash_file(root, path, limit, &cancelled)?;
            candidates.retain(|p| p.doc.sources[i] == actual);
            if candidates.is_empty() {
                return Ok(None);
            }
        }
        if candidates.iter().any(|p| p.doc.cartography.is_some()) {
            candidates.retain(|p| p.doc.cartography.is_some());
        }
        let shader_ids: BTreeSet<_> = candidates
            .iter()
            .filter_map(|p| p.defaults().and_then(|m| m.shader_blob_sha256.as_deref()))
            .collect();
        if shader_ids.len() > 1 {
            return Err("Conflicting terrain palettes for the same installation sources".into());
        }
        candidates.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        let Some(first) = candidates.first() else {
            return Ok(None);
        };
        if candidates.iter().any(|p| {
            p.doc.binding != first.doc.binding
                || p.doc.biomes != first.doc.biomes
                || p.doc.materials != first.doc.materials
                || !same_cartography(&p.doc.cartography, &first.doc.cartography)
        }) {
            return Err("Conflicting terrain palettes for the same installation sources".into());
        }
        if candidates
            .iter()
            .any(|p| p.defaults().is_some_and(|m| m.shader_blob_sha256.is_some()))
        {
            candidates.retain(|p| p.defaults().is_some_and(|m| m.shader_blob_sha256.is_some()));
        }
        if cancelled() {
            return Ok(None);
        }
        let mut palette = candidates.remove(0);
        palette.bind_surface(root, expected, &cancelled)?;
        Ok((!cancelled()).then_some(palette))
    }
    pub fn decode(raw: &[u8], expected: &Installation) -> Result<Self, String> {
        expected.validate()?;
        if raw.len() as u64 > LIMIT {
            return Err("Terrain palette exceeds metadata budget".into());
        }
        let doc: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid terrain palette: {e}"))?;
        if doc.format != "papyrus-terrain-palette"
            || doc.schema != 1
            || doc.adapter != "papyrus-terrain-palette-1"
            || doc.numeric_space != "unity-serialized-color"
            || !hash(&doc.extractor_sha256)
            || doc.sources.len() != 3
            || doc.sources[0] != expected.native
            || doc.scene != "BuildPlayer-Procedural Map"
            || doc.binding.scene_file != doc.scene
            || !(4..=5).contains(&doc.biomes.len())
            || !doc.biomes.iter().zip(BIOMES).all(|(a, b)| a == b)
            || doc.materials.len() != 8
        {
            return Err(
                "Terrain palette differs from selected installation or supported schema".into(),
            );
        }
        for (s, path, limit) in [
            (
                &doc.sources[1],
                "Bundles/maps/maps.bundle",
                512 * 1024 * 1024u64,
            ),
            (
                &doc.sources[2],
                "Bundles/shared/content.bundle",
                8 * 1024 * 1024 * 1024u64,
            ),
        ] {
            if s.relative_path != path || !hash(&s.sha256) || s.bytes == 0 || s.bytes > limit {
                return Err("Invalid terrain palette source receipt".into());
            }
        }
        let binding = &doc.binding;
        let ids = [binding.world_setup, binding.terrain, binding.generator];
        if ids.iter().any(|&v| v == 0 || v == i64::MIN)
            || ids.into_iter().collect::<BTreeSet<_>>().len() != 3
            || binding.config_object == 0
            || binding.config_object == i64::MIN
            || !name(&binding.config_file, 256)
            || binding.config_file.contains(['/', '\\'])
            || (binding.config_file == binding.scene_file && ids.contains(&binding.config_object))
            || doc.config_paths.len() > 16
            || doc.config_paths.iter().any(|p| {
                !name(p, 1024)
                    || !p.starts_with("assets/")
                    || p.contains('\\')
                    || p.split('/').any(|part| part == ".." || part.is_empty())
            })
            || doc.config_paths.iter().collect::<BTreeSet<_>>().len() != doc.config_paths.len()
        {
            return Err("Invalid scene/config binding in terrain palette".into());
        }
        let mut names = BTreeSet::new();
        for material in &doc.materials {
            if !name(&material.name, 128)
                || !names.insert(&material.name)
                || material.biome_colors.len() != doc.biomes.len()
                || material
                    .biome_colors
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            {
                return Err("Invalid terrain palette material/biome colors".into());
            }
        }
        if let Some(map) = &doc.cartography {
            map.validate()?;
        }
        Ok(Self {
            effective_map: doc.cartography.clone(),
            raw: raw.to_vec().into(),
            sha256: format!("{:x}", Sha256::digest(raw)),
            doc,
            display_colors: Default::default(),
            surface_profile: None,
            surface_cache: Default::default(),
        })
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
        let _verification = installation::VerificationScope::default();
        let mut raw = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let mut palette = Self::decode(&raw, expected)?;
        if !palette.matches_sources(root, expected, &cancelled)? {
            return if cancelled() {
                Ok(None)
            } else {
                Err("Terrain palette source changed in the selected installation".into())
            };
        }
        palette.bind_surface(root, expected, &cancelled)?;
        Ok((!cancelled()).then_some(palette))
    }

    fn matches_sources(
        &self,
        root: &Path,
        expected: &Installation,
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        if self.doc.sources.first() != Some(&expected.native) {
            return Ok(false);
        }
        let _verification = installation::VerificationScope::default();
        for (source, limit) in self.doc.sources.iter().zip([
            64 * 1024 * 1024u64,
            512 * 1024 * 1024,
            8 * 1024 * 1024 * 1024,
        ]) {
            if cancelled() {
                return Ok(false);
            }
            if installation::hash_file(root, &source.relative_path, limit, cancelled)? != *source {
                return Ok(false);
            }
        }
        Ok(!cancelled())
    }

    pub(crate) fn matches_prepared(
        &self,
        root: &Path,
        expected: &Installation,
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        let _verification = installation::VerificationScope::default();
        if !self.matches_sources(root, expected, cancelled)? {
            return Ok(false);
        }
        Ok(self.verified_surface(root, expected, cancelled)? == self.surface_profile && !cancelled())
    }
    fn bind_surface(
        &mut self,
        root: &Path,
        installation: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        self.surface_profile = self.verified_surface(root, installation, cancelled)?;
        Ok(())
    }
    fn verified_surface(
        &self,
        root: &Path,
        installation: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<crate::map_surface_frame::Profile>, String> {
        crate::map_surface_frame::bind(
            root,
            installation,
            &self.doc.sources,
            self.defaults()
                .and_then(|m| m.shader_blob_sha256.as_deref()),
            cancelled,
        )
    }
    pub(crate) fn surface_profile(&self) -> Option<crate::map_surface_frame::Profile> {
        self.surface_profile
    }
    pub fn has_surface_profile(&self) -> bool {
        self.surface_profile.is_some()
    }
    #[cfg(test)]
    pub(crate) fn test_surface_profile(&mut self) {
        self.surface_profile = Some(crate::map_surface_frame::Profile::Legacy);
    }
    #[cfg(test)]
    pub(crate) fn test_current_surface_profile(&mut self) {
        self.surface_profile = Some(crate::map_surface_frame::Profile::Current);
    }
    pub fn source_render_key(&self) -> String {
        if let Some(profile) = self.surface_profile {
            format!(
                "{}:{}:{}",
                self.sha256,
                profile.key(),
                profile.normal_version()
            )
        } else {
            self.sha256.clone()
        }
    }
    pub(crate) fn surface_for_output(
        &self,
        frame: std::sync::Arc<crate::map_surface_frame::Frame>,
        output: usize,
    ) -> std::sync::Arc<crate::map_surface_frame::Frame> {
        let adjusted = frame.for_output(output);
        if std::sync::Arc::ptr_eq(&frame, &adjusted) {
            return frame;
        }
        self.surface_cache.remember(adjusted)
    }
    pub fn prepare_native_surface(
        &self,
        layers: &std::sync::Arc<papyrus_terrain::native_layers::Layers>,
        maximum_bytes: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
        let Some(profile) = self.surface_profile else {
            return Ok(None);
        };
        self.surface_cache.native(
            layers,
            profile,
            crate::map_image::DISPLAY_RESOLUTION,
            maximum_bytes,
            None,
            cancelled,
        )
    }
    pub fn prepare_composed_surface(
        &self,
        maps: &std::sync::Arc<papyrus_terrain::monument_composition::Composed>,
        maximum_bytes: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
        let Some(profile) = self.surface_profile else {
            return Ok(None);
        };
        self.surface_cache.composed(
            maps,
            profile,
            crate::map_image::DISPLAY_RESOLUTION,
            maximum_bytes,
            None,
            cancelled,
        )
    }
    pub(crate) fn prepare_native_surface_in(
        &self,
        layers: &std::sync::Arc<papyrus_terrain::native_layers::Layers>,
        water: Option<&[i16]>,
        maximum_bytes: usize,
        output: usize,
        pool: &rayon::ThreadPool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
        let Some(profile) = self.surface_profile else {
            return Ok(None);
        };
        self.surface_cache.native_with_water(
            layers,
            water,
            profile,
            output,
            maximum_bytes,
            Some(pool),
            cancelled,
        )
    }
    pub(crate) fn prepare_composed_surface_in(
        &self,
        maps: &std::sync::Arc<papyrus_terrain::monument_composition::Composed>,
        maximum_bytes: usize,
        output: usize,
        pool: &rayon::ThreadPool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
        let Some(profile) = self.surface_profile else {
            return Ok(None);
        };
        self.surface_cache
            .composed(maps, profile, output, maximum_bytes, Some(pool), cancelled)
    }
    pub fn materials(&self) -> &[Material] {
        &self.doc.materials
    }
    pub fn binding(&self) -> &Binding {
        &self.doc.binding
    }
    pub fn sources(&self) -> &[SourceFile] {
        &self.doc.sources
    }
    pub fn bytes(&self) -> &[u8] {
        &self.raw
    }
    pub fn covers_layers(&self, layers: &papyrus_terrain::native_layers::Layers) -> bool {
        let cells = layers
            .surface_resolution
            .saturating_mul(layers.surface_resolution);
        if self.cartography().is_some() {
            return layers.splat.len() == cells.saturating_mul(8);
        }
        layers.biome.len() == cells.saturating_mul(5)
            && layers.biome[self.biome_count() * cells..]
                .iter()
                .all(|v| *v == 0)
    }
    pub fn biome_count(&self) -> usize {
        self.doc.biomes.len()
    }
    pub fn cartography(&self) -> Option<&crate::map_colors::Material> {
        self.effective_map.as_ref()
    }
    pub fn defaults(&self) -> Option<&crate::map_colors::Material> {
        self.doc.cartography.as_ref()
    }
    pub fn with_display_colors(&self, colors: &crate::map_colors::Preferences) -> Self {
        let mut result = self.clone();
        result.display_colors = colors.clone();
        result.effective_map = result.doc.cartography.as_ref().map(|m| m.effective(colors));
        result
    }
    pub fn render_key(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.source_render_key().as_bytes());
        hash.update(serde_json::to_vec(&self.display_colors).expect("Byte-only color preferences"));
        format!("{:x}", hash.finalize())
    }
    /// Source colors mixed by material/biome weights, without lighting or gamma
    /// conversion. Unsupported biome coverage never invents a replacement color.
    pub fn evaluate(&self, materials: [f32; 8], biomes: [f32; 5]) -> Option<[f32; 4]> {
        self.evaluate_with_biomes(materials, || biomes)
    }

    pub(crate) fn evaluate_with_biomes(
        &self,
        materials: [f32; 8],
        biomes: impl FnOnce() -> [f32; 5],
    ) -> Option<[f32; 4]> {
        if let Some(map) = self.cartography() {
            return materials
                .iter()
                .all(|v| v.is_finite() && *v >= 0.)
                .then(|| map.ground(materials));
        }
        let biomes = biomes();
        if materials
            .iter()
            .chain(biomes.iter())
            .any(|v| !v.is_finite() || *v < 0.)
            || biomes[self.biome_count()..].iter().any(|v| *v != 0.)
        {
            return None;
        }
        let m = materials.iter().sum::<f32>();
        let b = biomes.iter().sum::<f32>();
        if !m.is_finite() || !b.is_finite() || m <= 0. || b <= 0. {
            return None;
        }
        let mut color = [0.; 4];
        for (i, weight) in materials.into_iter().enumerate() {
            if weight == 0. {
                continue;
            }
            for (j, biome) in biomes[..self.biome_count()].iter().enumerate() {
                if *biome == 0. {
                    continue;
                }
                let contribution = (weight / m) * (*biome / b);
                for (c, value) in color.iter_mut().enumerate() {
                    *value += contribution * self.doc.materials[i].biome_colors[j][c];
                }
            }
        }
        Some(color)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture(count: usize) -> (Installation, serde_json::Value) {
        let source = SourceFile {
            relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
            sha256: "a".repeat(64),
            bytes: 1,
        };
        let installation = Installation {
            kind: installation::InstallationKind::Client,
            native: source.clone(),
            scene_manifest: None,
        };
        let materials=(0..8).map(|i|serde_json::json!({"name":format!("Material {i}"),"biome_colors":(0..count).map(|j|[i as f32/8.,j as f32/5.,0.5,1.]).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let doc = serde_json::json!({"format":"papyrus-terrain-palette","schema":1,"adapter":"papyrus-terrain-palette-1","extractor_sha256":"b".repeat(64),
            "sources":[source,{"relative_path":"Bundles/maps/maps.bundle","sha256":"c".repeat(64),"bytes":1},{"relative_path":"Bundles/shared/content.bundle","sha256":"d".repeat(64),"bytes":1}],
            "scene":"BuildPlayer-Procedural Map","binding":{"scene_file":"BuildPlayer-Procedural Map","world_setup":1,"terrain":2,"generator":3,"config_file":"CAB-config","config_object":4},
            "config_paths":["assets/test.asset"],"numeric_space":"unity-serialized-color","biomes":BIOMES[..count],"materials":materials});
        (installation, doc)
    }
    #[test]
    fn saved_display_palette_restores_exact_colors_and_surface_profile_without_rust_files() {
        let (installation,mut doc)=fixture(5);
        doc["cartography"]=serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let original=Palette::decode(&serde_json::to_vec(&doc).unwrap(),&installation).unwrap();
        for profile in [None,Some(crate::map_surface_frame::Profile::Legacy),Some(crate::map_surface_frame::Profile::Current)] {
            let mut original=original.clone();original.surface_profile=profile;
            let mut colors=crate::map_colors::Preferences {enabled:true,..Default::default()};
            colors.overrides[10]=Some([12,34,56]);
            let original=original.with_display_colors(&colors);
            let saved=serde_json::to_vec(&original.snapshot()).unwrap();
            let restored=Palette::from_snapshot(serde_json::from_slice(&saved).unwrap(),&installation).unwrap();
            assert_eq!(restored,original);assert_eq!(restored.render_key(),original.render_key());
            let mut bad:Snapshot=serde_json::from_slice(&saved).unwrap();bad.profile=Some("unknown".into());
            assert!(Palette::from_snapshot(bad,&installation).is_err());
        }
    }
    #[test]
    fn competing_palette_publication_accepts_only_identical_complete_entries() {
        for existing in [b"palette".as_slice(), b"conflict".as_slice()] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let destination = root.path().join("destination");
            std::fs::create_dir(&source).unwrap();
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(source.join("palette.json"), b"palette").unwrap();
            std::fs::write(destination.join("palette.json"), existing).unwrap();
            let result = publish_palette(&source, &destination, b"palette");
            assert_eq!(result.is_ok(), existing == b"palette");
            assert_eq!(
                std::fs::read(destination.join("palette.json")).unwrap(),
                existing
            );
            assert_eq!(
                std::fs::read(source.join("palette.json")).unwrap(),
                b"palette"
            );
        }
    }
    #[cfg(windows)]
    #[test]
    fn palette_publication_recovers_after_a_real_file_lock_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("palette.json"), b"palette").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(source.join("palette.json"))
            .unwrap();
        let error = std::fs::rename(&source, &destination).unwrap_err();
        assert!(matches!(error.raw_os_error(), Some(5 | 32 | 33)));
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(75));
            drop(held);
        });
        let result = publish_palette(&source, &destination, b"palette");
        release.join().unwrap();
        result.unwrap();
        assert!(!source.exists());
        assert_eq!(
            std::fs::read(destination.join("palette.json")).unwrap(),
            b"palette"
        );
    }
    #[cfg(windows)]
    #[test]
    fn permanent_palette_lock_preserves_the_unpublished_source() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("palette.json"), b"palette").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(source.join("palette.json"))
            .unwrap();
        let result = publish_palette(&source, &destination, b"palette");
        drop(held);
        assert!(result.is_err());
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read(source.join("palette.json")).unwrap(),
            b"palette"
        );
    }
    #[test]
    fn cache_publication_checks_sources_identity_conflicts_and_cancellation() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("game");
        let cache = folder.path().join("cache");
        let (mut installation, mut doc) = fixture(5);
        for (i, path) in [
            installation.native.relative_path.clone(),
            "Bundles/maps/maps.bundle".into(),
            "Bundles/shared/content.bundle".into(),
        ]
        .into_iter()
        .enumerate()
        {
            let file = root.join(&path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, [i as u8 + 1]).unwrap();
            let receipt = installation::hash_file(&root, &path, 1024, &|| false).unwrap();
            if i == 0 {
                installation.native = receipt.clone();
            }
            doc["sources"][i] = serde_json::to_value(receipt).unwrap();
        }
        let palette = Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        assert!(palette.matches_prepared(&root, &installation, &|| false).unwrap());
        assert!(!palette.matches_prepared(&root, &installation, &|| true).unwrap());
        palette.persist_in(&cache).unwrap();
        palette.persist_in(&cache).unwrap();
        assert_eq!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap()
                .unwrap(),
            palette
        );
        assert!(
            Palette::discover_in(&cache, &root, &installation, || true)
                .unwrap()
                .is_none()
        );
        // A new extractor with identical semantics is acceptable; a contradictory
        // palette for the exact same game receipts must never be selected.
        doc["extractor_sha256"] = "e".repeat(64).into();
        Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation)
            .unwrap()
            .persist_in(&cache)
            .unwrap();
        assert!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap()
                .is_some()
        );
        doc["materials"][0]["biome_colors"][0][0] = 0.9.into();
        Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation)
            .unwrap()
            .persist_in(&cache)
            .unwrap();
        assert!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap_err()
                .contains("Conflicting")
        );
        std::fs::write(root.join("Bundles/shared/content.bundle"), [99]).unwrap();
        assert!(!palette.matches_prepared(&root, &installation, &|| false).unwrap());
        assert!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap()
                .is_none()
        );
        let maps = root.join("Bundles/maps/maps.bundle");
        let content = root.join("Bundles/shared/content.bundle");
        std::fs::remove_file(content).unwrap();
        for changed_maps in [&[99_u8][..], &[99, 100][..]] {
            std::fs::write(&maps, changed_maps).unwrap();
            assert!(
                Palette::discover_in(&cache, &root, &installation, || false)
                    .unwrap()
                    .is_none()
            );
        }
        std::fs::write(maps, [2]).unwrap();
        assert!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap_err()
                .contains("Cannot read Bundles/shared/content.bundle")
        );
        let mut changed = palette.clone();
        changed.sha256 = "../escape".into();
        assert!(changed.persist_in(&cache).is_err());
        std::fs::write(
            cache.join(&palette.sha256).join("palette.json"),
            b"foreign data",
        )
        .unwrap();
        assert!(palette.persist_in(&cache).is_err());
        assert_eq!(
            std::fs::read(cache.join(&palette.sha256).join("palette.json")).unwrap(),
            b"foreign data"
        );
    }
    #[test]
    fn four_and_five_biome_colors_preserve_material_identity_and_weighted_mixing() {
        for count in [4, 5] {
            let (installation, mut doc) = fixture(count);
            doc["materials"][1]["name"] = "Sand Flat".into();
            let p = Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
            assert_eq!(p.materials()[1].name, "Sand Flat");
            let mut m = [0.; 8];
            m[2] = 1.;
            m[6] = 3.;
            let mut b = [0.; 5];
            b[1] = 3.;
            b[3] = 1.;
            let c = p.evaluate(m, b).unwrap();
            assert_eq!(p.evaluate_with_biomes(m, || b), Some(c));
            for (a, b) in c.into_iter().zip([0.625, 0.3, 0.5, 1.]) {
                assert!((a - b).abs() < 1e-6);
            }
            b = [0.; 5];
            b[4] = 1.;
            assert_eq!(p.evaluate(m, b).is_some(), count == 5);
            assert!(p.evaluate([0.; 8], [1.; 5]).is_none());
            m[0] = f32::NAN;
            assert!(p.evaluate(m, b).is_none());
        }
    }
    #[test]
    fn cartographic_defaults_and_custom_keys_preserve_raw_palette_and_restore_exactly() {
        let (installation, mut doc) = fixture(5);
        doc["cartography"] = serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let source = Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        let mut prefs = crate::map_colors::Preferences {
            enabled: true,
            ..Default::default()
        };
        prefs.overrides[4] = Some([255, 0, 0]);
        let changed = source.with_display_colors(&prefs);
        assert_eq!(source.bytes(), changed.bytes());
        assert_eq!(source.sha256, changed.sha256);
        assert_ne!(source.render_key(), changed.render_key());
        assert_ne!(source.cartography(), changed.cartography());
        assert_eq!(changed.with_display_colors(&Default::default()), source);
        let mut w = [0.; 8];
        w[4] = 1.;
        assert_eq!(
            source.evaluate(w, [0.; 5]),
            source.evaluate(w, [1., 0., 0., 0., 0.])
        );
        assert_eq!(
            source.evaluate_with_biomes(w, || panic!("PaperMap ignores biome weights")),
            source.evaluate(w, [0.; 5])
        );
        doc["cartography"]["shader_name"] = "Unknown/Map".into();
        assert!(Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).is_err());
    }
    #[test]
    fn cartographic_cache_upgrade_preserves_legacy_entries_and_rejects_conflicting_new_maps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        let cache = dir.path().join("cache");
        let (mut installation, mut doc) = fixture(5);
        for (i, path) in [
            installation.native.relative_path.clone(),
            "Bundles/maps/maps.bundle".into(),
            "Bundles/shared/content.bundle".into(),
        ]
        .iter()
        .enumerate()
        {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, [i as u8]).unwrap();
            let receipt = installation::hash_file(&root, path, 1024, &|| false).unwrap();
            if i == 0 {
                installation.native = receipt.clone();
            }
            doc["sources"][i] = serde_json::to_value(receipt).unwrap();
        }
        let legacy = Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        legacy.persist_in(&cache).unwrap();
        doc["cartography"] = serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let upgraded = Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        upgraded.persist_in(&cache).unwrap();
        assert_eq!(
            Palette::discover_in(&cache, &root, &installation, || false).unwrap(),
            Some(upgraded)
        );
        assert_eq!(
            std::fs::read(cache.join(&legacy.sha256).join("palette.json")).unwrap(),
            legacy.bytes()
        );
        doc["cartography"]["shader_blob_sha256"] = "a".repeat(64).into();
        let identified =
            Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        identified.persist_in(&cache).unwrap();
        assert_eq!(
            Palette::discover_in(&cache, &root, &installation, || false).unwrap(),
            Some(identified.clone())
        );
        doc["cartography"]["shader_blob_sha256"] = "b".repeat(64).into();
        let conflicting =
            Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        conflicting.persist_in(&cache).unwrap();
        assert!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap_err()
                .contains("Conflicting")
        );
        // Remove only the conflicting synthetic entry; legacy cache files stay.
        std::fs::remove_file(cache.join(&conflicting.sha256).join("palette.json")).unwrap();
        std::fs::remove_dir(cache.join(&conflicting.sha256)).unwrap();
        doc["cartography"]["shader_blob_sha256"] = "a".repeat(64).into();
        doc["cartography"]["brightness"] = 0.8.into();
        Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation)
            .unwrap()
            .persist_in(&cache)
            .unwrap();
        assert!(
            Palette::discover_in(&cache, &root, &installation, || false)
                .unwrap_err()
                .contains("Conflicting")
        );
    }
    #[test]
    fn malformed_or_mismatched_source_binding_colors_and_semantics_are_rejected() {
        let (installation, doc) = fixture(5);
        for (pointer, value) in [
            ("/sources/0/sha256", serde_json::json!("e".repeat(64))),
            (
                "/sources/1/relative_path",
                serde_json::json!("../maps.bundle"),
            ),
            ("/sources/2/bytes", serde_json::json!(0)),
            ("/binding/terrain", serde_json::json!(1)),
            ("/binding/config_file", serde_json::json!("../config")),
            ("/materials/1/name", serde_json::json!("Material 0")),
            ("/materials/0/biome_colors/0/0", serde_json::json!(1.1)),
            ("/biomes/4", serde_json::json!("Unknown")),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                Palette::decode(&serde_json::to_vec(&bad).unwrap(), &installation).is_err(),
                "{pointer}"
            );
        }
        assert!(Palette::decode(&vec![0; LIMIT as usize + 1], &installation).is_err());
    }
}
