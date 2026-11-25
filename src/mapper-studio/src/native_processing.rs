//! Separates immutable native generation from cancellable Rust processing stages.
use crate::{TerrainKey, native_preview, terrain_cache};
use eframe::egui::ColorImage;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Clone)]
pub struct Source {
    pub terrain: Option<Arc<crate::terrain_stages::Prepared>>,
    /// Immutable native terrain preceding derived terrain stages.
    pub native_base: Option<Arc<papyrus_terrain::native_layers::Layers>>,
    pub rivers: Option<Arc<papyrus_terrain::river_pipeline::World>>,
    pub palette: Option<Arc<crate::terrain_palette::Palette>>,
    pub key: TerrainKey,
    pub image: ColorImage,
    pub heights: Arc<Vec<i16>>,
    pub layers: Arc<papyrus_terrain::native_layers::Layers>,
}
impl Source {
    pub fn native_base_layers(&self) -> Result<&papyrus_terrain::native_layers::Layers, String> {
        self.native_base_arc().map(Arc::as_ref)
    }

    pub(crate) fn native_base_arc(
        &self,
    ) -> Result<&Arc<papyrus_terrain::native_layers::Layers>, String> {
        let Some(base) = self.native_base.as_ref() else {
            return if self.rivers.is_some() {
                Err("Derived river terrain is missing its original native source".into())
            } else {
                Ok(&self.layers)
            };
        };
        let current = &self.layers;
        if (
            base.size,
            base.terrain_seed,
            base.biome_seed,
            base.height_resolution,
            base.surface_resolution,
        ) != (
            current.size,
            current.terrain_seed,
            current.biome_seed,
            current.height_resolution,
            current.surface_resolution,
        ) {
            return Err("Derived terrain has a different native source frame".into());
        }
        Ok(base)
    }
}

pub struct Processed {
    /// The immutable cache/session source before optional river derivation.
    pub raw_source: Option<Source>,
    pub source: Source,
    pub ocean: Option<Arc<papyrus_terrain::ocean::Ocean>>,
    pub reused_source: bool,
    pub ocean_ms: f64,
    pub cache_status: String,
}

/// Recheck the retained palette against its installation and surface shader.
pub fn prepared_palette_matches(
    root: &Path,
    installation: &crate::installation::Installation,
    palette: &crate::terrain_palette::Palette,
    cancelled: impl Fn() -> bool,
) -> bool {
    palette.defaults().is_some_and(|material| material.shader_blob_sha256.is_some())
        && palette.matches_prepared(root, installation, &cancelled).unwrap_or(false)
}
pub fn build(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cached: Option<Source>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&'static str, f32),
) -> Result<Option<Processed>, String> {
    build_with_snapshots(
        root,
        worker,
        key,
        cached,
        cancel,
        false,
        |label, fraction, _| progress(label, fraction),
    )
}
#[allow(clippy::too_many_arguments)]
pub fn build_with_snapshots(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cached: Option<Source>,
    cancel: &AtomicBool,
    live: bool,
    progress: impl FnMut(&'static str, f32, Option<ColorImage>),
) -> Result<Option<Processed>, String> {
    build_initial(
        root,
        worker,
        key,
        cached,
        cancel,
        Presentation::Preview {
            palette: None,
            live,
        },
        false,
        progress,
    )
}

enum Presentation {
    Preview {
        palette: Option<Arc<crate::terrain_palette::Palette>>,
        live: bool,
    },
    Map,
}

/// Complete ordered worlds derive rivers after early terrain placement and erosion.
/// Standalone preview callers retain the river pass in `build_with_snapshots`.
#[allow(clippy::too_many_arguments)]
pub fn build_for_project_with_snapshots(
    root: &Path,
    worker: &Path,
    project: &crate::Project,
    cached: Option<Source>,
    prepared_palette: Option<Arc<crate::terrain_palette::Palette>>,
    cancel: &AtomicBool,
    live: bool,
    progress: impl FnMut(&'static str, f32, Option<ColorImage>),
) -> Result<Option<Processed>, String> {
    build_initial(
        root,
        worker,
        &project.terrain_key(),
        cached,
        cancel,
        Presentation::Preview {
            palette: prepared_palette,
            live,
        },
        rivers_follow_terrain(project),
        progress,
    )
}

/// Prepare the composition inputs for a game-map export. Palette extraction
/// and intermediate images are only needed by preview consumers.
pub(crate) fn build_for_map(
    root: &Path,
    worker: &Path,
    project: &crate::Project,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&'static str, f32),
) -> Result<Option<Processed>, String> {
    build_initial(
        root,
        worker,
        &project.terrain_key(),
        None,
        cancel,
        Presentation::Map,
        rivers_follow_terrain(project),
        |label, fraction, _| progress(label, fraction),
    )
}

pub fn rivers_follow_terrain(project: &crate::Project) -> bool {
    project.composition_engine.is_some() && crate::terrain_stages::enabled(project)
}

pub(crate) fn initial_key(key: &TerrainKey, defer_rivers: bool) -> TerrainKey {
    let mut initial = key.clone();
    if defer_rivers {
        initial.rivers = None;
    }
    initial
}

#[allow(clippy::too_many_arguments)]
fn build_initial(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cached: Option<Source>,
    cancel: &AtomicBool,
    presentation: Presentation,
    defer_rivers: bool,
    mut progress: impl FnMut(&'static str, f32, Option<ColorImage>),
) -> Result<Option<Processed>, String> {
    let initial_key = initial_key(key, defer_rivers);
    let cached = cached.filter(|source| {
        source.rivers.is_none()
            && source.native_base.is_none()
            && source.terrain.is_none()
            && source.key == native_preview::source_key(key)
    });
    let mut reused_source = cached.is_some();
    let mut cache_status = "Reused session terrain".to_string();
    let mut source = if let Some(source) = cached {
        progress("Checking Rust files", 0.05, None);
        if !native_preview::verify_source(root, key, cancel)? {
            return Ok(None);
        }
        source
    } else {
        let directory = terrain_cache::directory();
        progress("Loading saved terrain", 0.03, None);
        let saved = directory
            .as_ref()
            .map(|path| terrain_cache::load(path, key, || cancel.load(Ordering::Relaxed)));
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match saved {
            Some(Ok(Some(source))) => {
                progress("Checking Rust files", 0.05, None);
                if !native_preview::verify_source(root, key, cancel)? {
                    return Ok(None);
                }
                reused_source = true;
                cache_status = "Loaded verified terrain variant from disk".into();
                source
            }
            other => {
                cache_status = match other {
                    Some(Err(e)) => format!("Saved terrain rejected: {e}. Regenerated"),
                    None => "Disk cache unavailable; generated terrain is kept in memory".into(),
                    _ => "Generated a new terrain variant".into(),
                };
                let native = match &presentation {
                    Presentation::Map => native_preview::generate_map_layers(
                        root, worker, key, cancel, &mut progress,
                    )?,
                    Presentation::Preview { live, .. } => native_preview::generate_with_snapshots(
                        root, worker, key, cancel, *live, &mut progress,
                    )?,
                };
                let Some(native) = native else {
                    return Ok(None);
                };
                let source = Source {
                    terrain: None,
                    native_base: None,
                    rivers: None,
                    palette: None,
                    key: native_preview::source_key(key),
                    image: native.image,
                    heights: Arc::new(native.heights),
                    layers: native.layers,
                };
                if let Some(directory) = directory {
                    let mut timing = crate::composition::StageTimer::start("native_snapshot");
                    progress("Saving immutable terrain variant", 0.95, None);
                    match terrain_cache::save(&directory, &source, || {
                        cancel.load(Ordering::Relaxed)
                    }) {
                        Ok(()) => cache_status.push_str("; saved to disk"),
                        Err(e) => cache_status.push_str(&format!("; cache save unavailable: {e}")),
                    }
                    crate::composition::record_timing(&mut timing, "cache_write_finished");
                }
                source
            }
        }
    };
    let render = matches!(presentation, Presentation::Preview { .. });
    if let Presentation::Preview { palette: prepared_palette, .. } = presentation
        && let Some(installation) = &key.source
    {
        let palette = prepared_palette.or(source.palette.take());
        let cancelled = || cancel.load(Ordering::Relaxed);
        let prepared = palette.filter(|palette| {
            prepared_palette_matches(root, installation, palette, cancelled)
        });
        let palette = if let Some(prepared) = prepared {
            Ok(Some(prepared))
        } else if cancelled() {
            return Ok(None);
        } else {
            progress("Loading terrain colors", 0.955, None);
            crate::palette_extract::prepare(root, installation, cancelled)
                .map(|value| value.map(Arc::new))
        };
        match palette {
            Ok(Some(palette)) if palette.covers_layers(&source.layers)=> {
                cache_status.push_str(&format!("; installation terrain colors {}",&palette.sha256[..12]));
                source.palette=Some(palette);
            },
            Ok(Some(_))=>cache_status.push_str("; approximate cartographic colors (installation palette lacks generated biome coverage)"),
            Ok(None)=>cache_status.push_str("; approximate cartographic colors (installation palette unavailable)"),
            Err(error)=>cache_status.push_str(&format!("; approximate cartographic colors: {error}")),
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let raw_source = (render && key.rivers.is_some()).then(|| source.clone());
    let Some(mut source) = derive_rivers(
        root,
        worker,
        &initial_key,
        source,
        &|| cancel.load(Ordering::Relaxed),
        |label, fraction| progress(label, fraction, None),
    )?
    else {
        return Ok(None);
    };
    if render && (source.rivers.is_some() || source.image.pixels.is_empty()) {
        let Some(image) = crate::map_image::render_terrain_thumbnail(
            &source.layers,
            None,
            source.rivers.as_ref().map(|world| world.local_water.as_slice()),
            source.palette.as_deref(),
            || cancel.load(Ordering::Relaxed),
        )?
        else {
            return Ok(None);
        };
        source.image = image;
    }
    let mut processed = process(
        &initial_key,
        source,
        reused_source,
        cancel,
        |label, fraction| progress(label, fraction, None),
    )?;
    if let Some(processed) = &mut processed {
        processed.cache_status = cache_status;
        processed.raw_source = raw_source;
    }
    Ok(processed)
}
/// Apply river layers and sample their heights. The caller draws the preview
/// after choosing which terrain and alpha mask it needs to display.
pub(crate) fn derive_rivers(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    source: Source,
    cancel: &impl Fn() -> bool,
    mut progress: impl FnMut(&'static str, f32),
) -> Result<Option<Source>, String> {
    let Some(rivers) = &key.rivers else {
        return Ok(Some(source));
    };
    let _verification = crate::installation::VerificationScope::default();
    rivers.validate()?;
    let installation = key
        .source
        .as_ref()
        .ok_or("River generation requires a selected installation")?;
    progress("Verifying installation river source", 0.956);
    let Some(definition) =
        crate::river_generation::Source::cached(root, installation, &rivers.recipe, cancel)?
    else {
        return Ok(None);
    };
    let profile = crate::native_profile::resolve(key)?;
    let Some(classified_topology) = papyrus_terrain::ocean::reconstruct_topology(
        &source.layers.height,
        source.layers.height_resolution,
        &source.layers.topology,
        source.layers.surface_resolution,
        cancel,
    )?
    else {
        return Ok(None);
    };
    let native_base = source
        .native_base
        .clone()
        .unwrap_or_else(|| source.layers.clone());
    // The distance-based carve and paint queries often use the same
    // coordinates. Keep their exact prefix separate from layout and water.
    let mut surface_noise = crate::native_noise::PrefixCache::default();
    // Only topology differs from the immutable terrain during river planning.
    let Some(output) = definition.generate_with_topology(
        &source.layers,
        &classified_topology,
        rivers.seed,
        rivers.limit,
        |stage, coordinates| {
            use papyrus_terrain::river_pipeline::Stage;
            let (label, fraction) = match stage {
                Stage::Layout => ("Tracing river paths", 0.957),
                Stage::Topology => ("Marking river topology", 0.958),
                Stage::Carve => ("Carving river terrain", 0.959),
                Stage::Texture => ("Painting river materials", 0.960),
                Stage::Water => ("Building river water", 0.961),
            };
            progress(label, fraction);
            match stage {
                Stage::Topology | Stage::Carve | Stage::Texture => {
                    surface_noise.sample(root, worker, installation, &profile, coordinates, cancel)
                }
                Stage::Layout | Stage::Water => {
                    crate::native_noise::sample(root, worker, installation, &profile, coordinates, cancel)
                }
            }
        },
        cancel,
    )?
    else {
        return Ok(None);
    };
    drop(classified_topology);
    let (mut layers, mut world) = output.into_parts();
    if rivers.recipe.objects.is_some() {
        progress("Placing river objects", 0.9615);
        if !crate::river_objects::generate(
            &mut layers,
            &mut world,
            root,
            installation,
            rivers,
            cancel,
        )? {
            return Ok(None);
        }
    }
    let heights = layers.sample_heights(native_preview::RESOLUTION)?;
    Ok(Some(Source {
        terrain: source.terrain,
        native_base: Some(native_base),
        key: native_preview::processing_key(key),
        image: source.image,
        heights: Arc::new(heights),
        layers: Arc::new(layers),
        palette: source.palette,
        rivers: Some(Arc::new(world)),
    }))
}
pub fn process(
    key: &TerrainKey,
    source: Source,
    reused_source: bool,
    cancel: &AtomicBool,
    progress: impl FnMut(&'static str, f32),
) -> Result<Option<Processed>, String> {
    process_with_cancel(
        key,
        source,
        reused_source,
        &|| cancel.load(Ordering::Relaxed),
        progress,
    )
}

pub(crate) fn process_with_cancel(
    key: &TerrainKey,
    mut source: Source,
    reused_source: bool,
    cancel: &impl Fn() -> bool,
    mut progress: impl FnMut(&'static str, f32),
) -> Result<Option<Processed>, String> {
    if cancel() {
        return Ok(None);
    }
    if !native_preview::is_native(&key.engine)
        || source.key != native_preview::processing_key(key)
        || source.rivers.is_some() != key.rivers.is_some()
        || source.layers.size != key.size
        || source.layers.terrain_seed != key.seed
        || Some(source.layers.biome_seed) != key.biome_seed
    {
        return Err("Native processing source does not match recipe".into());
    }
    let started = Instant::now();
    let ocean = if key.engine == native_preview::OCEAN_VERSION || key.rivers.is_some() {
        progress(
            "Reconstructing boundary-connected ocean",
            if key.rivers.is_some() { 0.962 } else { 0.96 },
        );
        let layers = &source.layers;
        let local_water = source.rivers.as_ref().map(|rivers| rivers.local_water.as_slice());
        if local_water.is_some_and(|water| water.len() != layers.height.len()) {
            return Err("River water dimensions differ from derived terrain".into());
        }
        let Some(ocean) = papyrus_terrain::ocean::reconstruct_with_water(
            &layers.height,
            layers.height_resolution,
            &layers.topology,
            layers.surface_resolution,
            local_water,
            cancel,
        )?
        else {
            return Ok(None);
        };
        if local_water.is_some() {
            Arc::make_mut(&mut source.layers)
                .topology
                .clone_from(&ocean.topology);
        }
        Some(Arc::new(ocean))
    } else {
        None
    };
    let ocean_ms = if ocean.is_some() {
        started.elapsed().as_secs_f64() * 1000.
    } else {
        0.
    };
    if cancel() {
        return Ok(None);
    }
    Ok(Some(Processed {
        raw_source: None,
        source,
        ocean,
        reused_source,
        ocean_ms,
        cache_status: if reused_source {
            "Reused session terrain"
        } else {
            "Generated terrain"
        }
        .into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn river_deferral_requires_complete_ordered_native_composition() {
        let mut project = crate::Project {
            preview_engine: native_preview::VERSION.into(),
            composition_engine: Some(
                papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into(),
            ),
            monument_rules: Some(
                serde_json::from_value(serde_json::json!({
                    "engine": crate::monument_source::VERSION,
                    "source_sha256": "a".repeat(64),
                    "surface_modifiers": crate::monument_source::SOURCE_PRIORITY_VERSION,
                }))
                .unwrap(),
            ),
            ..Default::default()
        };
        project.rivers = Some(crate::river_generation::Recipe {
            engine: papyrus_terrain::river_pipeline::VERSION.into(),
            source_sha256: "b".repeat(64),
            source_document: None,
            objects: None,
        });
        assert!(rivers_follow_terrain(&project));
        let ordered_key = project.terrain_key();
        assert!(ordered_key.rivers.is_some());
        assert_eq!(
            initial_key(&ordered_key, true),
            native_preview::source_key(&ordered_key),
        );
        assert_eq!(initial_key(&ordered_key, false), ordered_key);
        project.monument_rules.as_mut().unwrap().surface_modifiers = None;
        assert!(!rivers_follow_terrain(&project));
        // Terrain keys alone cannot distinguish the two processing orders.
        assert_eq!(ordered_key, project.terrain_key());
        project.monument_rules.as_mut().unwrap().surface_modifiers =
            Some(crate::monument_source::SOURCE_PRIORITY_VERSION.into());
        project.composition_engine = None;
        assert!(!rivers_follow_terrain(&project));
        project.composition_engine =
            Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
        project.preview_engine = papyrus_terrain::landscape::VERSION.into();
        assert!(!rivers_follow_terrain(&project));
    }
}
