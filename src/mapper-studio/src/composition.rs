//! Verified local asset adapter for the independent composition engine.
use crate::{
    Project, native_preview,
    native_processing::Source,
    prefab_assets::{Asset, Catalog, Modifier},
    prefab_textures::Bindings,
};
use eframe::egui::ColorImage;
use papyrus_terrain::{
    monument_composition::{self, Composed, Image, Stamp},
    placement::Solution,
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

struct DecodedStamps {
    binding: usize,
    maximum: usize,
    bytes: usize,
    entries: VecDeque<DecodedStamp>,
    hits: usize,
    alias_hits: usize,
    read_bytes: usize,
}

struct DecodedStamp {
    guid: String,
    aliases: Vec<String>,
    pixels: Arc<Vec<u8>>,
}

impl DecodedStamps {
    fn get(&mut self, guid: &str) -> Option<Arc<Vec<u8>>> {
        let index = self.entries.iter().position(|entry| {
            entry.guid == guid || entry.aliases.iter().any(|alias| alias == guid)
        })?;
        self.promote(index)
    }

    fn promote(&mut self, index: usize) -> Option<Arc<Vec<u8>>> {
        let entry = self.entries.remove(index)?;
        let data = entry.pixels.clone();
        self.entries.push_back(entry);
        self.hits += 1;
        Some(data)
    }

    fn reuse_alias(&mut self, bindings: &Bindings, guid: &str) -> Result<Option<Arc<Vec<u8>>>, String> {
        let textures = bindings.textures();
        let Some(texture) = textures.get(guid).filter(|t| t.storage.is_some()) else {
            return Ok(None);
        };
        let Some(index) = self.entries.iter().position(|entry| {
            textures
                .get(&entry.guid)
                .is_some_and(|candidate| texture.has_same_compressed_pixels(candidate))
        }) else {
            return Ok(None);
        };
        // Even identical receipts can refer to different files. Check this
        // GUID's stored bytes before sharing the already verified pixels.
        bindings.verify_stored_rgba(texture)?;
        self.entries[index].aliases.push(guid.to_owned());
        self.alias_hits += 1;
        Ok(self.promote(index))
    }

    fn retain(&mut self, guid: &str, data: &Arc<Vec<u8>>) {
        self.read_bytes = self.read_bytes.saturating_add(data.len());
        let needed = data.capacity();
        if needed > self.maximum {
            return;
        }
        while self.bytes + needed > self.maximum {
            let removed = self.entries.pop_front().expect("retained byte accounting");
            self.bytes -= removed.pixels.capacity();
        }
        self.bytes += needed;
        self.entries.push_back(DecodedStamp {
            guid: guid.into(),
            aliases: Vec::new(),
            pixels: data.clone(),
        });
    }
}

thread_local! {
    static DECODED_STAMPS: RefCell<Option<DecodedStamps>> = const { RefCell::new(None) };
}

// Retain exact verified bytes only while one composition owns its binding.
// The normal per-stamp budget still charges every enabled decoded texture.
struct StampDecodeScope<'a> {
    previous: Option<DecodedStamps>,
    _bindings: &'a Bindings,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl<'a> StampDecodeScope<'a> {
    fn new(bindings: &'a Bindings) -> Self {
        let maximum = 64.min(crate::generation_resources::current().terrain_memory_mib / 32)
            * 1024 * 1024;
        let previous = DECODED_STAMPS.with(|slot| slot.replace(Some(DecodedStamps {
            binding: std::ptr::from_ref(bindings).addr(),
            maximum,
            bytes: 0,
            entries: VecDeque::new(),
            hits: 0,
            alias_hits: 0,
            read_bytes: 0,
        })));
        Self { previous, _bindings: bindings, _thread: std::marker::PhantomData }
    }
}

impl Drop for StampDecodeScope<'_> {
    fn drop(&mut self) {
        let completed = DECODED_STAMPS.with(|slot| slot.replace(self.previous.take()));
        if let Some(cache) = completed
            && std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some()
        {
            eprintln!("MAPPER_STAMP_TEXTURE_CACHE {}", serde_json::json!({
                "hits":cache.hits,"alias_hits":cache.alias_hits,"decoded_bytes":cache.read_bytes,"retained_bytes":cache.bytes,
                "maximum_bytes":cache.maximum,
            }));
        }
    }
}

fn cached_stamp_rgba(bindings: &Bindings, guid: &str) -> Result<Option<Arc<Vec<u8>>>, String> {
    let key = std::ptr::from_ref(bindings).addr();
    DECODED_STAMPS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(cache) = slot.as_mut().filter(|cache| cache.binding == key) else {
            return Ok(None);
        };
        if let Some(data) = cache.get(guid) {
            return Ok(Some(data));
        }
        cache.reuse_alias(bindings, guid)
    })
}

fn retain_stamp_rgba(bindings: &Bindings, guid: &str, data: &Arc<Vec<u8>>) {
    let key = std::ptr::from_ref(bindings).addr();
    DECODED_STAMPS.with(|slot| {
        if let Some(cache) = slot.borrow_mut().as_mut().filter(|cache| cache.binding == key) {
            cache.retain(guid, data);
        }
    });
}

fn stamp_rgba(bindings: &Bindings, guid: &str) -> Result<Arc<Vec<u8>>, String> {
    if let Some(data) = cached_stamp_rgba(bindings, guid)? {
        return Ok(data);
    }
    let data = Arc::new(bindings.read_rgba(guid)?);
    retain_stamp_rgba(bindings, guid, &data);
    Ok(data)
}

#[cfg(test)]
mod decoded_stamp_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn retains_only_the_recent_payloads_that_fit_the_allocation_limit() {
        let mut cache = DecodedStamps {
            binding: 0, maximum: 8, bytes: 0, entries: VecDeque::new(), hits: 0, alias_hits: 0, read_bytes: 0,
        };
        let texture = |byte| Arc::new(vec![byte; 4]);
        let first = texture(1);
        cache.retain("first", &first);
        cache.retain("second", &texture(2));
        assert!(Arc::ptr_eq(&cache.get("first").unwrap(), &first));
        cache.retain("third", &texture(3));
        assert!(cache.get("second").is_none());
        assert!(cache.get("first").is_some());
        let mut oversized = Vec::with_capacity(16);
        oversized.push(4);
        cache.retain("oversized", &Arc::new(oversized));
        assert!(cache.get("oversized").is_none());
        assert_eq!((cache.bytes, cache.hits, cache.read_bytes), (8, 2, 13));
    }

    #[test]
    fn shares_identically_encoded_pixels_only_after_verifying_each_alias_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut catalog, receipt, mut doc, first) = crate::prefab_textures::tests::fixture(dir.path());
        let second = "b".repeat(32);
        let damaged = "c".repeat(32);
        let guids = &mut catalog.entries.get_mut("assets/test.prefab").unwrap().texture_guids;
        guids.insert("height".into(), second.clone());
        guids.insert("biome".into(), damaged.clone());
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&[1, 2, 3, 255]).unwrap();
        let compressed = encoder.finish().unwrap();
        doc["adapter"] = "papyrus-guid-rgba-zlib-2".into();
        doc["decoded_bytes"] = 12.into();
        for guid in [&first, &second, &damaged] {
            let path = format!("textures/{guid}.rgba.zlib");
            std::fs::write(dir.path().join(&path), &compressed).unwrap();
            let mut texture = doc["textures"][&first].clone();
            texture["rgba_path"] = path.into();
            texture["storage"] = serde_json::json!({
                "format":"zlib", "bytes":compressed.len(), "sha256":format!("{:x}", Sha256::digest(&compressed))
            });
            doc["textures"][guid] = texture;
        }
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        let bindings = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
        let _scope = StampDecodeScope::new(&bindings);
        let bytes = stamp_rgba(&bindings, &first).unwrap();
        assert!(Arc::ptr_eq(&bytes, &stamp_rgba(&bindings, &second).unwrap()));
        let damaged_path = dir.path().join(format!("textures/{damaged}.rgba.zlib"));
        std::fs::write(damaged_path, vec![0; compressed.len()]).unwrap();
        assert!(stamp_rgba(&bindings, &damaged).is_err());
        DECODED_STAMPS.with(|slot| {
            let slot = slot.borrow();
            let cache = slot.as_ref().unwrap();
            assert_eq!((cache.entries.len(), cache.alias_hits, cache.read_bytes), (1, 1, 4));
        });
    }
}

/// Cache identity only, not a placement certificate. Recipes/assets remain
/// separate dependencies. IEEE bits and length prefixes preserve exact inputs.
pub fn layout_fingerprint(solution: &Solution) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"papyrus-composition-layout-1");
    for n in [
        solution.required_missing,
        solution.attempts,
        solution.outcomes.len(),
    ] {
        hash.update((n as u64).to_le_bytes());
    }
    for o in &solution.outcomes {
        hash.update(o.id.to_le_bytes());
        hash.update([u8::from(o.position.is_some())]);
        if let Some(p) = &o.position {
            for v in [p.x, p.y, p.z, p.yaw, p.clearance] {
                hash.update(v.to_bits().to_le_bytes());
            }
        }
        hash.update([u8::from(o.distance.is_some()), u8::from(o.within_radius)]);
        if let Some(v) = o.distance {
            hash.update(v.to_bits().to_le_bytes());
        }
        hash.update((o.reason.len() as u64).to_le_bytes());
        hash.update(o.reason.as_bytes());
    }
    hash.finalize().into()
}

/// A composed world and the authoritative placement used to produce it.
pub struct TerrainBase {
    pub layers: std::sync::Arc<papyrus_terrain::native_layers::Layers>,
    pub ocean: Option<std::sync::Arc<papyrus_terrain::ocean::Ocean>>,
}
pub struct ResolvedLayout {
    pub base: Option<TerrainBase>,
    pub maps: Composed,
    pub solution: Solution,
    pub cables: crate::scene_wires::Preview,
}

/// The composition worker selects the layout after it prepares terrain or
/// establishes the coupled road and monument constraints.
pub fn selects_monument_layout(project: &Project) -> bool {
    project.composition_engine.is_some()
        && native_preview::is_native(&project.preview_engine)
        && (crate::terrain_stages::enabled(project) || crate::world_recovery::enabled(project))
}

#[allow(clippy::too_many_arguments)]
pub fn compose_resolved_layout_with_worker<'a>(
    source: &Source,
    project: &Project,
    solution: impl Into<Option<&'a Solution>>,
    catalog: &Catalog,
    bindings: &Bindings,
    worker: Option<&std::path::Path>,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<ResolvedLayout>, String> {
    compose_resolved_layout(source, project, solution, catalog, bindings, worker, true, cancelled)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_resolved_layout<'a>(
    source: &Source,
    project: &Project,
    solution: impl Into<Option<&'a Solution>>,
    catalog: &Catalog,
    bindings: &Bindings,
    worker: Option<&std::path::Path>,
    preview: bool,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<ResolvedLayout>, String> {
    let _verification = crate::installation::VerificationScope::default();
    let _identities = crate::prefab_identities::Scope::default();
    let _monuments = crate::monument_source::Scope::default();
    let _infrastructure = crate::infrastructure::Scope::new();
    let _stamp_textures = StampDecodeScope::new(bindings);
    let progress = crate::loading_progress::begin("Composing map", None);
    let Some(prepared) =
        crate::terrain_stages::prepare(source, project, catalog, bindings, worker, &cancelled)?
    else {
        return Ok(None);
    };
    let source = prepared.as_ref();
    progress.reporter().stage("Placing monuments");
    let staged_solution = if source.terrain.is_some() && !crate::world_recovery::enabled(project) {
        let Some(canonical) =
            crate::terrain_stages::solve(source, project, catalog, bindings, &cancelled)?
        else {
            return Ok(None);
        };
        Some(canonical.solution)
    } else {
        None
    };
    let mut resolved = if crate::world_recovery::enabled(project) {
        let Some(resolved) =
            crate::world_recovery::compose(source, project, catalog, bindings, worker, &cancelled)?
        else {
            return Ok(None);
        };
        resolved
    } else {
        let solution = staged_solution
            .as_ref()
            .or_else(|| solution.into())
            .ok_or("Composition requires a monument layout for this recipe")?;
        let Some(maps) = compose_layout_with_worker(
            source, project, solution, catalog, bindings, worker, &cancelled,
        )?
        else {
            return Ok(None);
        };
        ResolvedLayout {
            base: None,
            maps,
            solution: solution.clone(),
            cables: Ok(None),
        }
    };
    if cancelled() {
        return Ok(None);
    }
    if preview && crate::native_preview::is_native(&project.preview_engine) {
        resolved.cables = crate::scene_wires::from_world(
            &crate::map_export::Input {
                project,
                maps: &resolved.maps,
                solution: &resolved.solution,
                root: bindings.installation_root(),
                catalog,
            },
            Default::default(),
            &cancelled,
        )
        .map(|scene| scene.map(std::sync::Arc::new));
    }
    if let Some(terrain) = &source.terrain {
        resolved.base = Some(TerrainBase {
            layers: source.layers.clone(),
            ocean: terrain.ocean.clone(),
        });
    }
    Ok((!cancelled()).then_some(resolved))
}

struct Pixels {
    width: usize,
    height: usize,
    rgba: Arc<Vec<u8>>,
}

/// Compatibility helper for callers requesting the original preview dimensions.
/// All overview paths use the same biome tint, lighting and surface-cell sampling.
pub fn render_overview(source: &Source, maps: &Composed) -> Result<ColorImage, String> {
    let n = crate::native_preview::RESOLUTION;
    if source.image.size != [n, n]
        || source.image.pixels.len() != n * n
        || source.layers.size != maps.layers.size
        || source.layers.height_resolution != maps.layers.height_resolution
        || source.layers.surface_resolution != maps.layers.surface_resolution
    {
        return Err("Composition overview dimensions mismatch".into());
    }
    crate::map_image::render_composed(maps, n, source.palette.as_deref(), || false)?
        .ok_or_else(|| "Composition overview cancelled".into())
}

impl Pixels {
    fn image(&self) -> Image<'_> {
        Image {
            width: self.width,
            height: self.height,
            rgba: &self.rgba,
        }
    }
}
pub struct Prepared {
    modifier: Modifier,
    size: [f64; 3],
    offset: [f64; 3],
    splat_mask: u8,
    textures: BTreeMap<String, Pixels>,
}
impl Prepared {
    /// Origin-centered conservative circle covering both authored objects and all modifier cells.
    pub fn placement_clearance(&self, extent: [f32; 2]) -> f32 {
        let height_half = if self.textures.contains_key("blend") {
            [self.size[0] / 2., self.size[2] / 2.]
        } else {
            [self.stamp([0.; 3], 0.).radius; 2]
        };
        let radius = self.stamp([0.; 3], 0.).radius;
        let x = (self.offset[0].abs() + height_half[0].max(radius)).max(f64::from(extent[0]));
        let z = (self.offset[2].abs() + height_half[1].max(radius)).max(f64::from(extent[1]));
        x.hypot(z) as f32
    }
    pub fn bytes(&self) -> usize {
        self.textures.values().map(|p| p.rgba.len()).sum()
    }
    pub fn stamp(&self, position: [f64; 3], yaw_degrees: f64) -> Stamp<'_> {
        let image = |name: &str| self.textures.get(name).map(Pixels::image);
        Stamp {
            source_cliff_generation: false,
            source_quad_raster: false,
            source_height_blending: false,
            source_texture_sampling: false,
            position,
            yaw_degrees,
            size: self.size,
            offset: self.offset,
            radius: if self.modifier.radius == 0. {
                self.size[0] / 2.
            } else {
                f64::from(self.modifier.radius)
            },
            fade: f64::from(self.modifier.fade),
            height: image("height"),
            blend: image("blend"),
            splat: image("splat0").zip(image("splat1")).map(|(a, b)| [a, b]),
            splat_mask: self.splat_mask,
            alpha: image("alpha"),
            biome: image("biome"),
            biome_mask: (self.modifier.biome_mask & 31) as u8,
            topology: image("topology"),
            topology_mask: self.modifier.topology_mask,
            replace_topology: self.modifier.remove_existing_topology,
            auto_cliff_splat: self.modifier.auto_cliff_splat,
            auto_cliff_topology: self.modifier.auto_cliff_topology,
        }
    }
}

impl Prepared {
    pub fn stamp_for_recipe(
        &self,
        position: [f64; 3],
        yaw: f64,
        version: Option<&str>,
    ) -> Stamp<'_> {
        let mut stamp = self.stamp(position, yaw);
        stamp.source_texture_sampling = monument_composition::source_textures(version);
        stamp.source_cliff_generation = matches!(
            version,
            Some(
                monument_composition::SOURCE_CLIFF_VERSION
                    | monument_composition::SOURCE_RASTER_VERSION
                    | monument_composition::SOURCE_HEIGHT_VERSION
            )
        );
        stamp.source_quad_raster = matches!(
            version,
            Some(
                monument_composition::SOURCE_RASTER_VERSION
                    | monument_composition::SOURCE_HEIGHT_VERSION
            )
        );
        stamp.source_height_blending = version == Some(monument_composition::SOURCE_HEIGHT_VERSION);
        stamp
    }
}
pub fn prepare(
    asset: &Asset,
    bindings: &Bindings,
    world_size: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    prepare_with_budget(asset, bindings, world_size, cancelled, 256 * 1024 * 1024)
}

pub(crate) fn enabled_texture_keys(
    asset: &Asset,
) -> Result<impl Iterator<Item = &'static str>, String> {
    let modifier = asset.modifier.as_ref().ok_or("Missing modifier settings")?;
    let splat_mask = asset.splat_mask.ok_or("Missing splat mask")?;
    Ok([
        ("height", modifier.height_map),
        (
            "blend",
            modifier.height_map && asset.texture_guids.contains_key("blend"),
        ),
        ("splat0", splat_mask != 0),
        ("splat1", splat_mask != 0),
        ("alpha", modifier.alpha_map),
        ("biome", modifier.biome_mask & 31 != 0),
        ("topology", modifier.topology_mask != 0),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name)))
}

pub(crate) fn prepare_with_budget(
    asset: &Asset,
    bindings: &Bindings,
    world_size: u32,
    cancelled: impl Fn() -> bool,
    budget: usize,
) -> Result<Option<Prepared>, String> {
    let modifier = asset
        .modifier
        .as_ref()
        .ok_or("No supported terrain modifier settings for this prefab")?;
    let size = asset
        .modifier_size
        .ok_or("Missing modifier dimensions")?
        .map(f64::from);
    let offset = asset
        .modifier_offset
        .ok_or("Missing modifier offset")?
        .map(f64::from);
    if asset
        .bounds
        .as_ref()
        .is_some_and(|b| b.minimum_world_size > world_size)
    {
        return Err("Monument requires a larger world size".into());
    }
    if asset.texture_guids.contains_key("water") && modifier.water_map != Some(false) {
        let supported = crate::monument_water_hook::verified_noop(bindings, &cancelled)?;
        if cancelled() { return Ok(None); }
        if !supported {
            return Err("Monument water placement needs a compatible selected Rust installation".into());
        }
    }
    let splat_mask = asset.splat_mask.ok_or("Missing splat mask")?;
    let mut requested = Vec::new();
    let mut loaded = 0_u64;
    for name in enabled_texture_keys(asset)? {
        if cancelled() {
            return Ok(None);
        }
        let guid = asset
            .texture_guids
            .get(name)
            .ok_or_else(|| format!("Missing enabled {name} texture"))?;
        let descriptor = bindings
            .textures()
            .get(guid)
            .ok_or("Unresolved modifier texture")?;
        loaded += descriptor.bytes;
        if loaded > budget as u64 {
            return Err("Composition texture budget exceeds 256 MiB".into());
        }
        requested.push((name, guid.as_str(), descriptor));
    }
    let limits = crate::generation_resources::current();
    let encoded_budget = (limits.terrain_memory_mib / 32).min(32) as u64 * 1024 * 1024;
    let encoded = requested.iter().map(|(_, _, texture)| {
        texture.storage.as_ref().map_or(texture.bytes, |stored| stored.bytes)
    }).sum::<u64>();
    let mut textures = BTreeMap::new();
    if loaded >= 12 * 1024 * 1024
        && encoded <= encoded_budget
        && requested.iter().filter(|(_, _, texture)| texture.storage.is_some()).count() >= 2
        && limits.effective_workers() >= 2
        && let Some(complete) = crate::generation_resources::try_with_cpu_pool(|pool| {
            // Keep the decoded cache on the calling thread. Its work is bounded
            // by the stamp budget, and only independent cache misses use workers.
            let mut cached = Vec::with_capacity(requested.len());
            let mut pending = Vec::new();
            for &(_, guid, _) in &requested {
                if cancelled() {
                    return Ok(false);
                }
                cached.push(cached_stamp_rgba(bindings, guid));
                if matches!(cached.last(), Some(Ok(None))) && !pending.contains(&guid) {
                    pending.push(guid);
                }
            }
            let mut decoded = pool.install(|| {
                pending.par_iter().map(|&guid| {
                    (guid, (bindings.read_rgba(guid).map(Arc::new), false))
                }).collect::<BTreeMap<_, _>>()
            });
            if cancelled() {
                return Ok(false);
            }
            for (&(name, guid, descriptor), hit) in requested.iter().zip(cached) {
                let rgba = match hit? {
                    Some(data) => data,
                    None => {
                        let (result, retained) = decoded.get_mut(guid).expect("scheduled stamp texture");
                        let data = result.as_ref().map_err(Clone::clone)?;
                        if !*retained {
                            retain_stamp_rgba(bindings, guid, data);
                            *retained = true;
                        }
                        data.clone()
                    }
                };
                textures.insert(name.into(), Pixels {
                    width: descriptor.width, height: descriptor.height, rgba,
                });
            }
            Ok(true)
        })?
    {
        if !complete {
            return Ok(None);
        }
    } else {
        for (name, guid, descriptor) in requested {
            if cancelled() {
                return Ok(None);
            }
            textures.insert(name.into(), Pixels {
                width: descriptor.width, height: descriptor.height,
                rgba: stamp_rgba(bindings, guid)?,
            });
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Prepared {
        modifier: modifier.clone(),
        size,
        offset,
        splat_mask,
        textures,
    }))
}

/// Caller must keep this result separate from Source, use a matching layout
/// revision and reject it when project, catalog or binding identities change.
pub fn compose_layout(
    source: &Source,
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<Composed>, String> {
    compose_layout_with_worker(
        source, project, solution, catalog, bindings, None, cancelled,
    )
}

/// A prefab without a terrain modifier contributes its placement, while its
/// authored object bounds and pose still require validation.
pub fn omit_terrain_modifier(
    project: &Project,
    asset: &Asset,
    position: &papyrus_terrain::placement::Position,
) -> Result<bool, String> {
    if !project
            .monument_rules
            .as_ref()
            .is_some_and(|r| r.omits_terrain_stamp(asset))
    {
        return Ok(false);
    }
    let (bounds, _) = crate::placement_preview::bounds_for_recipe(project, asset)?;
    if ![
        position.x,
        position.y,
        position.z,
        position.yaw,
        position.clearance,
    ]
    .iter()
    .all(|v| v.is_finite())
        || [position.x, position.y, position.z]
            .iter()
            .any(|v| v.abs() > 65536.)
        || position.clearance < 0.
        || !bounds[0]
            .at([position.x, position.z], position.yaw)
            .inside_world(project.size as f32)
    {
        return Err("Invalid modifier-free monument pose or world bounds".into());
    }
    Ok(true)
}

/// Compose monument stamps and their ocean classification before roads, rails,
/// objects, overrides and final audits. This is an intermediate private revision.
pub fn compose_monuments(
    source: &Source,
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, String> {
    project.validate()?;
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    if !project
        .composition_engine
        .as_deref()
        .is_some_and(monument_composition::supported)
    {
        return Err("Enable the supported monument composition engine for this recipe".into());
    }
    if source.key != native_preview::processing_key(&project.terrain_key())
        || source.rivers.is_some() != project.rivers.is_some()
        || source.layers.size != project.size
        || source.layers.terrain_seed != project.seeds[0].value
        || source.layers.biome_seed != project.seeds[1].value
        || project.installation.as_ref() != Some(&catalog.installation)
        || bindings.catalog_sha256() != catalog.sha256
    {
        return Err("Composition source/catalog identity mismatch".into());
    }
    if solution.outcomes.len() != project.targets.len() {
        return Err("Composition layout does not account for every target".into());
    }
    let mut prepared = Vec::new();
    let streaming = crate::placement_preview::is_balanced(&project.placement_engine);
    let mut stream_jobs = Vec::new();
    let mut poses = Vec::new();
    let mut bytes = 0;
    let mut seen = std::collections::HashSet::new();
    for outcome in &solution.outcomes {
        if cancelled() {
            return Ok(None);
        }
        if !seen.insert(outcome.id) {
            return Err("Duplicate composition outcome identity".into());
        }
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Layout contains an unknown target")?;
        if crate::terrain_stages::already_stamped(source, outcome.id, &outcome.position)? { continue; }
        let Some(position) = &outcome.position else {
            continue;
        };
        if !project.performance.monuments || target.presence == crate::Presence::Excluded {
            return Err("Layout contradicts monument content policy".into());
        }
        let path = target
            .prefab_path
            .as_deref()
            .ok_or("Target has no prefab identifier")?;
        let asset = catalog
            .entries
            .get(path)
            .ok_or("Prefab missing from verified catalog")?;
        if omit_terrain_modifier(project, asset, position)? {
            continue;
        }
        if streaming {
            stream_jobs.push((
                asset,
                [
                    f64::from(position.x),
                    f64::from(position.y),
                    f64::from(position.z),
                ],
                f64::from(position.yaw),
            ));
            continue;
        }
        let Some(item) = prepare_with_budget(
            asset,
            bindings,
            project.size,
            &cancelled,
            256 * 1024 * 1024 - bytes,
        )?
        else {
            return Ok(None);
        };
        bytes += item.bytes();
        if bytes > 256 * 1024 * 1024 {
            return Err("Composition texture budget exceeds 256 MiB".into());
        }
        prepared.push(item);
        poses.push((
            [
                f64::from(position.x),
                f64::from(position.y),
                f64::from(position.z),
            ],
            f64::from(position.yaw),
        ));
    }
    let stamps: Vec<_> = prepared
        .iter()
        .zip(poses)
        .map(|(p, (position, yaw))| {
            p.stamp_for_recipe(position, yaw, project.composition_engine.as_deref())
        })
        .collect();
    // Native generation currently has no authored/imported alpha layer. Keep this
    // explicit until map import supplies its own immutable base alpha.
    let alpha: std::borrow::Cow<'_, [u8]> = source.terrain.as_ref().map_or_else(
        || std::borrow::Cow::Owned(vec![255; source.layers.surface_resolution.pow(2)]),
        |terrain| std::borrow::Cow::Borrowed(terrain.alpha.as_slice()),
    );
    let result = if streaming {
        monument_composition::compose_streamed(&source.layers, &alpha, &cancelled, |apply| {
            for (asset, position, yaw) in stream_jobs {
                let Some(item) = prepare_with_budget(
                    asset, bindings, project.size, &cancelled, 256 * 1024 * 1024,
                )? else {
                    return Ok(false);
                };
                if !apply(&item.stamp_for_recipe(position, yaw, project.composition_engine.as_deref()))? {
                    return Ok(false);
                }
            }
            Ok(true)
        })?
    } else {
        monument_composition::compose(&source.layers, &alpha, &stamps, &cancelled)?
    };
    let Some(mut maps) = result else {
        return Ok(None);
    };
    if !crate::custom_monuments::compose_packages(
        project,
        solution,
        catalog,
        bindings.installation_root(),
        &mut maps,
        &cancelled,
    )? {
        return Ok(None);
    }
    if monument_composition::water_aware(project.composition_engine.as_deref()) {
        let Some(ocean) = papyrus_terrain::ocean::reconstruct(
            &maps.layers.height,
            maps.layers.height_resolution,
            &maps.layers.topology,
            maps.layers.surface_resolution,
            &cancelled,
        )? else {
            return Ok(None);
        };
        maps.layers.topology.clone_from(&ocean.topology);
        maps.ocean = Some(ocean);
    }
    maps.rivers = source.rivers.clone();
    Ok((!cancelled()).then_some(maps))
}

/// Complete composition retains the existing generation sequence. The explicit
/// monument-only boundary lets staged adapters plan roads before later painting.
pub fn compose_layout_with_worker(
    source: &Source,
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    worker: Option<&std::path::Path>,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<Composed>, String> {
    let _verification = crate::installation::VerificationScope::default();
    let _monuments = crate::monument_source::Scope::default();
    let _infrastructure = crate::infrastructure::Scope::new();
    if crate::world_recovery::enabled(project) {
        return Err("Coupled placement requires the resolved-layout composition API".into());
    }
    compose_layout_with_session(
        source, project, solution, catalog, bindings, worker, cancelled, None,
    )
    .map_err(Failure::message)
}

pub(crate) enum Failure {
    Message(String),
    Roads(crate::road_generation::Rejection),
}
impl Failure {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Message(message) => message,
            Self::Roads(error) => error.message,
        }
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}

type PreviewListener = std::rc::Rc<dyn Fn(&Composed)>;
thread_local! {
    static LIVE_PREVIEW: std::cell::RefCell<Option<PreviewListener>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Supplies intermediate composition views to the calling worker.
pub struct LivePreviewScope {
    previous: Option<PreviewListener>,
}
impl LivePreviewScope {
    pub fn new(listener: impl Fn(&Composed) + 'static) -> Self {
        let previous = LIVE_PREVIEW.with(|slot| slot.replace(Some(std::rc::Rc::new(listener))));
        Self { previous }
    }
}
impl Drop for LivePreviewScope {
    fn drop(&mut self) {
        LIVE_PREVIEW.with(|slot| slot.replace(self.previous.take()));
    }
}

fn publish_live_preview(maps: &Composed) {
    let listener = LIVE_PREVIEW.with(|slot| slot.borrow().clone());
    if let Some(listener) = listener {
        listener(maps);
    }
}

pub(crate) struct StageTimer {
    scope: &'static str,
    scope_id: u64,
    started: std::time::Instant,
    previous: std::time::Instant,
}

impl StageTimer {
    pub(crate) fn start(scope: &'static str) -> Option<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
        std::env::var_os("MAPPER_COMPOSITION_TIMINGS")?;
        let started = std::time::Instant::now();
        Some(Self {
            scope,
            scope_id: NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
            started,
            previous: started,
        })
    }
}

pub(crate) fn record_timing(timing: &mut Option<StageTimer>, stage: &str) {
    if let Some(timer) = timing {
        use std::io::Write;
        let now = std::time::Instant::now();
        let event = serde_json::json!({
            "scope": timer.scope,
            "scope_id": timer.scope_id,
            "stage": stage,
            "elapsed_ms": (now - timer.started).as_secs_f64() * 1000.,
            "since_previous_ms": (now - timer.previous).as_secs_f64() * 1000.,
        });
        timer.previous = now;
        let _ = writeln!(std::io::stderr().lock(), "MAPPER_COMPOSITION_TIMING {event}");
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_layout_with_session(
    source: &Source,
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    worker: Option<&std::path::Path>,
    cancelled: impl Fn() -> bool + Sync,
    mut session: Option<&mut crate::road_generation::Session>,
) -> Result<Option<Composed>, Failure> {
    let mut timing = StageTimer::start("composition");
    record_timing(&mut timing, "begin");
    let mut excluded = std::collections::BTreeSet::new();
    let mut rejected = Vec::new();
    let mut required_portals = None;
    let mut layout_variant = 0;
    let mut detour = None;
    let mut tried_detour = false;
    let Some(mut maps) = compose_monuments(source, project, solution, catalog, bindings, &cancelled)?
    else {
        return Ok(None);
    };
    record_timing(&mut timing, "monuments");
    publish_live_preview(&maps);
    #[cfg(feature = "road-diagnostics")]
    crate::coastal_placement::capture_stage(
        "monuments",
        project,
        solution,
        catalog,
        bindings,
        &maps,
        &cancelled,
    )?;
    let ring = if crate::rail_generation::before_roads(project) {
        let Some(plan) = crate::rail_generation::prepare_ring(
            project, solution, catalog, bindings, &maps, &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(plan)
    } else {
        None
    };
    record_timing(&mut timing, "rail_ring");
    let mut early_rails = loop {
        let Some(generated) = crate::tunnel_generation::generate_with_exclusions(
            project,
            solution,
            catalog,
            bindings,
            maps,
            ring.as_ref(),
            &excluded,
            layout_variant,
            detour,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let required = *required_portals.get_or_insert(generated.portals.len());
        maps = generated.maps;
        record_timing(&mut timing, "tunnels");
        let mut unsupported_root = None;
        let rail_result = if generated.portals.len() < required {
            Err(crate::rail_generation::Rejection::Layout(format!(
                "Rail recovery could not preserve all {required} generated tunnel entrances"
            )))
        } else if let Some(plan) = &ring {
            let sockets = if let Some(source) = &generated.source {
                let Some(sockets) = plan.sockets_with_tunnel_portals(
                    source, &generated.portals, &cancelled,
                )? else {
                    return Ok(None);
                };
                Some(sockets)
            } else {
                None
            };
            let result = crate::rail_generation::resume_with_rejection(
                plan, sockets.as_ref(), project, solution, &mut maps, &cancelled,
            );
            if !tried_detour
                && matches!(result, Err(crate::rail_generation::Rejection::Layout(_)))
                && let Some(sockets) = &sockets
            {
                unsupported_root = plan.unsupported_portal_root(
                    &generated.portals, sockets, &maps.layers,
                )?;
            }
            match result {
                Ok(None) => return Ok(None),
                result => result,
            }
        } else {
            Ok(None)
        };
        let early_rails = match rail_result {
            Ok(pending) => pending,
            Err(crate::rail_generation::Rejection::Layout(message))
                if required > 0 && rejected.len() < 3 =>
            {
                if cancelled() {
                    return Ok(None);
                }
                if let Some(session) = session.as_deref_mut() {
                    let approaches = crate::rail_generation::rebuild_approach_limit(
                        project, solution, catalog, bindings, &cancelled,
                    )?;
                    let work = crate::world_recovery::rebuild_bound(
                        source, project, solution, catalog, bindings, approaches,
                    )?;
                    if !session.reserve_world_rebuild(work)? {
                        return Err(format!("Rail entrance recovery reached the shared generation allowance. {message}").into());
                    }
                }
                let failed_variant = layout_variant;
                let failed_detour = detour.take();
                if let Some(root) = unsupported_root {
                    detour = Some(root);
                    tried_detour = true;
                } else if generated.portals.len() < required {
                    layout_variant += 1;
                } else {
                    let before = excluded.len();
                    let tunnels = generated
                        .source
                        .as_ref()
                        .ok_or("Rail recovery has no source for its generated entrances")?;
                    for portal in &generated.portals {
                        let prefab = tunnels
                            .prefabs
                            .get(&portal.prefab)
                            .ok_or("Rejected rail entrance is absent from its installed source")?;
                        excluded.insert(crate::tunnel_generation::portal_site(
                            prefab.id,
                            portal.pose.position,
                        ));
                    }
                    if excluded.len() == before {
                        return Err(format!(
                            "Rail entrance recovery repeated a rejected site. {message}"
                        )
                        .into());
                    }
                }
                let witness = serde_json::json!({
                    "attempt":rejected.len(), "layout_variant":failed_variant,
                    "detour_root":failed_detour, "error":message, "portals":generated.portals,
                });
                if timing.is_some() {
                    use std::io::Write;
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "MAPPER_RAIL_LAYOUT_REJECTION {witness}"
                    );
                }
                rejected.push(witness);
                // The rejected attempt can only have published underground
                // effects. Restore the pre-tunnel buffers before picking a new
                // layout so stamps and rejected terrain effects never stack.
                generated.retry_terrain.restore(&mut maps.layers);
                maps.tunnels = None;
                record_timing(&mut timing, "restore_monument_terrain");
                continue;
            }
            Err(crate::rail_generation::Rejection::Layout(message)) if !rejected.is_empty() => {
                let heading = if generated.portals.len() < required {
                    rejected[0]["error"].as_str().unwrap_or(&message)
                } else {
                    &message
                };
                let detail = serde_json::json!({
                    "last_error": &message,
                    "attempts": &rejected,
                });
                return Err(format!("{heading}\nRail recovery details: {detail}").into());
            }
            Err(error) => return Err(error.message().into()),
        };
        if !rejected.is_empty() {
            let receipt = serde_json::json!({
                "engine":"mapper-rail-portal-recovery-1", "attempts":rejected.len()+1,
                "required_portals":required, "rejected":rejected,
            });
            if let Some(tunnels) = &mut maps.tunnels {
                tunnels.report["rail_recovery"] = receipt.clone();
            }
            if let Some(rails) = &mut maps.rails {
                rails.report["portal_recovery"] = receipt;
            }
        }
        break early_rails;
    };
    drop(ring);
    record_timing(&mut timing, "rail_approaches");
    if let Some(recipe) = &project.roads {
        if recipe.staged.is_some() {
            let default_worker;
            let selected_worker = match worker {
                Some(path) => path,
                None => {
                    default_worker = crate::native_preview::worker_path()?;
                    &default_worker
                }
            };
            let context = crate::road_generation::Context {
                project,
                solution,
                catalog,
                bindings,
                worker: selected_worker,
            };
            let staged = if let Some(session) = session {
                let mut world = crate::road_planning::RetryWorld::new(&maps);
                loop {
                    match session.resume_prepared(
                        context,
                        &mut world,
                        early_rails.as_ref().map(|r| r.source()),
                        &cancelled,
                    ) {
                        Err(error) if session.retry_same_world(&error) => continue,
                        result => break result.map_err(Failure::Roads)?,
                    }
                }
            } else if let Some(rails) = early_rails.as_ref() {
                crate::road_generation::compose_retaining_rails(
                    context,
                    &maps,
                    rails.source(),
                    &cancelled,
                )?
            } else {
                crate::road_generation::compose(context, &maps, &cancelled)?
            };
            let Some(staged) = staged else {
                return Ok(None);
            };
            maps = staged;
        } else {
            let infrastructure =
                crate::infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?;
            let ring_costs = if let Some(profile) = &recipe.ring_costmap {
                let Some(costs) = profile.prepare(
                    project,
                    solution,
                    catalog,
                    bindings,
                    &infrastructure,
                    &maps.layers,
                    &cancelled,
                )?
                else {
                    return Ok(None);
                };
                Some(costs)
            } else {
                None
            };
            let access_placement = if let Some(profile) = &recipe.access_costmap {
                let Some(blockers) = profile.prepare_blockers(
                    project,
                    solution,
                    catalog,
                    bindings,
                    &infrastructure,
                    &cancelled,
                )?
                else {
                    return Ok(None);
                };
                Some(blockers)
            } else {
                None
            };
            let Some(roads) = crate::infrastructure::generate_with_source_costs(
                project,
                solution,
                catalog,
                &infrastructure,
                bindings.installation_root(),
                &mut maps,
                ring_costs.as_ref(),
                access_placement.as_deref(),
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            maps.roads = Some(roads);
        }
        maps.roads
            .as_mut()
            .ok_or("Road generation omitted its report")?
            .recipe = Some(recipe.result_receipt(project.seeds[3].value)?);
    }
    if project.path_edits.iter().any(|e| e.route.kind != crate::path_edit::Kind::Rail) {
        let roads = maps.roads.as_ref().ok_or("The edited road network is missing")?;
        crate::path_edit::verify_road_edit(roads, &project.path_edits)?;
    }
    record_timing(&mut timing, "roads");
    publish_live_preview(&maps);
    #[cfg(feature = "road-diagnostics")]
    crate::coastal_placement::capture_stage(
        "roads", project, solution, catalog, bindings, &maps, &cancelled,
    )?;
    let rail_pending = if early_rails.is_some() {
        early_rails.take()
    } else if project.rails.is_some() {
        let Some(pending) = crate::rail_generation::prepare(
            project, solution, catalog, bindings, &mut maps, &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(pending)
    } else {
        None
    };
    if project.path_edits.iter().any(|e| e.route.kind == crate::path_edit::Kind::Rail) {
        let rails = maps.rails.as_ref().ok_or("The edited rail network is missing")?;
        crate::path_edit::verify_rail_edit(&rails.tracks, &project.path_edits)?;
    }
    if crate::rail_generation::before_roads(project) {
        let pending = rail_pending
            .as_ref()
            .ok_or("Missing source-ordered rail preparation")?;
        let paths = maps
            .rails
            .as_ref()
            .ok_or("Missing retained rails")?
            .tracks
            .iter()
            .map(|t| t.path.clone())
            .collect::<Vec<_>>();
        let Some(texture) = crate::rail_texture_stage::paint(
            pending.source(),
            pending.infrastructure(),
            &maps.layers,
            &paths,
            crate::road_surface_stage::HeightGeometry {
                origin_y: -500.,
                vertical_span: 1000.,
            },
            512_000_000,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        maps.layers.splat = texture.splat;
        let scene = maps.rails.as_mut().unwrap();
        scene.report["late_texture"] =
            serde_json::to_value(texture.report).map_err(|e| e.to_string())?;
        scene.report["placement_reset_after_topology"] = true.into();
    }
    record_timing(&mut timing, "rail_texture");
    let (mut road_height_reservations, verified_road_surface) = if project.roads.as_ref().is_some_and(|r| r.staged.is_some())
    {
        let default_worker;
        let selected_worker = match worker {
            Some(path) => path,
            None => {
                default_worker = crate::native_preview::worker_path()?;
                &default_worker
            }
        };
        let Some((reservations, verified_surface)) = crate::road_generation::height_reservations(
            crate::road_generation::Context {
                project,
                solution,
                catalog,
                bindings,
                worker: selected_worker,
            },
            &maps,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        (Some(reservations), verified_surface)
    } else {
        (None, None)
    };
    if crate::rail_generation::before_roads(project) {
        let pending = rail_pending
            .as_ref()
            .ok_or("Missing retained rail protection source")?;
        let paths = maps
            .rails
            .as_ref()
            .ok_or("Missing retained rails")?
            .tracks
            .iter()
            .map(|t| t.path.clone())
            .collect::<Vec<_>>();
        let Some(retained) = crate::retained_rail_height::RetainedRails::prepare(
            pending.source(),
            pending.infrastructure(),
            &paths,
            32_000_000,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let Some(held) = retained.height_support(
            &maps.layers,
            crate::road_surface_stage::HeightGeometry {
                origin_y: -500.,
                vertical_span: 1000.,
            },
            128_000_000,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let mask = road_height_reservations
            .as_mut()
            .ok_or("Missing shared object height protection")?;
        for &i in held.samples() {
            if cancelled() {
                return Ok(None);
            };
            mask.reserve(i)?;
        }
        maps.rails.as_mut().unwrap().report["object_height_support"] = serde_json::json!({"samples":held.samples().len(),"work":held.preparation_work,"scope":"retained rail driving strips and bilinear dependencies"});
    }
    record_timing(&mut timing, "height_reservations");
    if !crate::coastal_placement::reserve_boat_routes(
        project,
        solution,
        catalog,
        bindings,
        &mut maps,
        &mut road_height_reservations,
        &cancelled,
    )? {
        return Ok(None);
    }
    record_timing(&mut timing, "boat_routes");
    if project.objects.is_none()
        && !crate::cliff_topology_stage::apply_early(project, catalog, bindings, &mut maps, &cancelled)?
    {
        return Ok(None);
    }
    if project.objects.is_some() {
        let limits = crate::generation_resources::current();
        let forecast_bytes = if crate::rail_generation::before_roads(project)
            && limits.effective_workers() > 1 {
            // Both the previous and new cache may be live during an edit.
            (limits.terrain_memory_mib / 32).min(64) * 1024 * 1024
        } else { 0 };
        let composed = papyrus_terrain::uniform_cliffs::with_forecast_replay(forecast_bytes, || {
            crate::object_generation::compose_with_height_reservations(
                project, solution, catalog, bindings, &mut maps, road_height_reservations.as_ref(), &cancelled,
            ).map(|complete| complete.then_some(()))
        })?;
        if composed.is_none() {
            return Ok(None);
        }
        if let Some(ocean) = &mut maps.ocean {
            if !ocean.refresh_topology(
                &maps.layers.height,
                maps.layers.height_resolution,
                &maps.layers.topology,
                &cancelled,
            )? {
                return Ok(None);
            }
            maps.layers.topology.clone_from(&ocean.topology);
        }
    } else if let Some(recipe) = &project.decoration {
        let source = crate::decor_source::Source::cached_shared(catalog, bindings, recipe, &cancelled)?;
        let Some(mut report) = source.generate(project, &maps, &cancelled)? else {
            return Ok(None);
        };
        if [
            papyrus_terrain::footprints::VERSION,
            papyrus_terrain::footprints::BLOCKER_VERSION,
            papyrus_terrain::decor_topology::VERSION,
            crate::decor_source::ROOT_VERSION,
            crate::decor_source::TRANSFORM_VERSION,
            crate::decor_source::MATERIAL_VERSION,
            crate::decor_source::HEIGHT_VERSION,
            crate::decor_source::COLLIDER_VERSION,
        ]
        .contains(&recipe.engine.as_str())
        {
            if !source.place_with_height_reservations(
                project,
                solution,
                catalog,
                &mut maps,
                &mut report,
                road_height_reservations.as_ref(),
                &cancelled,
            )? {
                return Ok(None);
            }
            if let Some(ocean) = &mut maps.ocean {
                if !ocean.refresh_topology(
                    &maps.layers.height,
                    maps.layers.height_resolution,
                    &maps.layers.topology,
                    &cancelled,
                )? {
                    return Ok(None);
                }
                maps.layers.topology.clone_from(&ocean.topology);
            }
        }
        maps.decoration = Some(report);
    }
    record_timing(&mut timing, "objects");
    if project.roads.as_ref().is_some_and(|r| r.staged.is_some()) {
        let default_worker;
        let selected_worker = match worker {
            Some(path) => path,
            None => {
                default_worker = crate::native_preview::worker_path()?;
                &default_worker
            }
        };
        if !crate::road_generation::validate_final(
            crate::road_generation::Context {
                project,
                solution,
                catalog,
                bindings,
                worker: selected_worker,
            },
            &mut maps,
            verified_road_surface,
            &cancelled,
        )? {
            return Ok(None);
        }
    }
    record_timing(&mut timing, "final_roads");
    if let Some(recipe) = &project.roads
        && recipe.staged.is_none()
        && let Some(digest) = &recipe.surface_sha256
    {
        let parent = crate::infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?;
        let root = bindings.installation_root();
        let mesh_source =
            crate::road_surface_source::Source::cached(root, digest, &parent, &cancelled)?;
        let default_worker;
        let worker = match worker {
            Some(path) => path,
            None => {
                default_worker = crate::native_preview::worker_path()?;
                &default_worker
            }
        };
        let Some(surface) = crate::road_surface_stage::generate(
            crate::road_surface_stage::Context {
                project,
                root,
                worker,
                layers: &maps.layers,
            },
            maps.roads
                .as_ref()
                .ok_or("Missing generated roads for surfaces")?,
            &mesh_source,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        maps.roads
            .as_mut()
            .ok_or("Missing generated roads")?
            .surfaces = Some(surface);
    }
    if maps.roads.as_ref().is_some_and(|r| r.surfaces.is_some()) {
        let Some(gameplay_height) = papyrus_terrain::gameplay_height::roads(
            &maps.layers,
            maps.roads.as_ref().ok_or("Missing generated roads")?,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        maps.gameplay_height = Some(gameplay_height);
    }
    if let Some(pending) = rail_pending
        && !crate::rail_generation::finish(pending, &mut maps, &cancelled)?
    {
        return Ok(None);
    }
    record_timing(&mut timing, "gameplay_height_and_rail_meshes");
    if project.objects.is_some() || maps.rails.is_some() {
        publish_live_preview(&maps);
    }
    if project.performance.monuments
        && let Some(recipe) = &project.monument_rules
        && let Some(binding) = &recipe.dungeon_source
    {
        let monuments =
            crate::monument_source::Source::cached_shared(recipe, catalog, bindings, &cancelled)?;
        let dungeons = crate::dungeon_source::Source::cached(
            &binding.source_sha256,
            bindings.installation_root(),
            catalog,
            &monuments,
            &cancelled,
        )?;
        let Some(scene) = dungeons.generate(project, solution, &maps.layers, &cancelled)? else {
            return Ok(None);
        };
        maps.dungeons = Some(scene);
    }
    record_timing(&mut timing, "dungeons");
    if !crate::terrain_stages::paint(source, &mut maps, &cancelled)? { return Ok(None); }
    record_timing(&mut timing, "erosion_materials");
    if project.performance.monuments
        && let Some(recipe) = &project.monument_rules
        && let Some(binding) = &recipe.offshore_source
    {
        let monuments = crate::monument_source::Source::cached_shared(recipe, catalog, bindings, &cancelled)?;
        let source = crate::offshore_generation::cached(binding, bindings.installation_root(), catalog, &cancelled)?;
        let Some(scene) = crate::offshore_generation::generate(project, solution, &maps, catalog, &monuments, &source, &cancelled)? else {
            return Ok(None);
        };
        maps.offshore = Some(scene);
    }
    record_timing(&mut timing, "offshore");
    maps.rivers = source.rivers.clone();
    if let Some(rivers) = &maps.rivers {
        let ocean = maps
            .ocean
            .as_mut()
            .ok_or("River composition lost its water-aware terrain state")?;
        if rivers.local_water.len() != ocean.local_water.len() {
            return Err("Composed river water dimensions differ".into());
        }
        for (to, from) in ocean
            .local_water
            .chunks_mut(65536)
            .zip(rivers.local_water.chunks(65536))
        {
            if cancelled() {
                return Ok(None);
            }
            to.copy_from_slice(from);
        }
    }
    if !crate::cliff_topology_stage::apply(project, catalog, bindings, &mut maps, &cancelled)? {
        return Ok(None);
    }
    record_timing(&mut timing, "cliff_topology");
    if !crate::clutter_topology_stage::apply(project, catalog, bindings, &mut maps, &cancelled)? {
        return Ok(None);
    }
    record_timing(&mut timing, "clutter_topology");
    if project.options.jungle_beach_spawns.unwrap_or_default() {
        let definition = crate::spawn_source::Source::discover(catalog, bindings, &cancelled)?
            .ok_or("Jungle beach spawns require the installation's character spawn filter")?;
        if !definition.enable_jungle_beaches(&mut maps.layers, &cancelled)? {
            return Ok(None);
        }
    }
    if !finish_topology(project, solution, catalog, bindings, &mut maps, &cancelled)? {
        return Ok(None);
    }
    #[cfg(feature = "road-diagnostics")]
    crate::coastal_placement::capture_stage(
        "final", project, solution, catalog, bindings, &maps, &cancelled,
    )?;
    if !crate::coastal_placement::validate_final(
        project, solution, catalog, bindings, &maps, &cancelled,
    )? {
        return Ok(None);
    }
    record_timing(&mut timing, "finished");
    Ok((!cancelled()).then_some(maps))
}
/// Replay final manual overrides from the pristine composed topology, then refresh
/// spawn diagnostics. This path never regenerates terrain or monument placement.
pub fn finish_topology(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if let Some(before) = maps.topology_before_overrides.take() {
        maps.layers.topology = before;
    }
    maps.topology_overrides = None;
    let spawn_source = if let Some(edits) = project
        .topology_edits
        .as_ref()
        .filter(|e| e.enabled && !e.strokes.is_empty())
    {
        Some(crate::spawn_source::Source::cached(
            catalog,
            bindings,
            &edits.source_sha256,
            &cancelled,
        )?)
    } else {
        crate::spawn_source::Source::discover(catalog, bindings, &cancelled)?
    };
    if let Some(edits) = project
        .topology_edits
        .as_ref()
        .filter(|e| e.enabled && !e.strokes.is_empty())
    {
        let source = spawn_source
            .as_ref()
            .ok_or("Manual topology requires matching installation-derived flag metadata")?;
        if source.sha256 != edits.source_sha256 {
            return Err("Manual topology is bound to a different source; disable the layer or restore its matching installation metadata".into());
        }
        let Some(out) = papyrus_terrain::topology_paint::apply(
            &maps.layers.topology,
            &maps.alpha,
            maps.layers.surface_resolution,
            edits,
            source.topology_mask(),
            &cancelled,
        )?
        else {
            return Ok(false);
        };
        maps.topology_before_overrides =
            Some(std::mem::replace(&mut maps.layers.topology, out.topology));
        maps.topology_overrides = Some(out.report);
    }
    maps.spawn_audit = None;
    if let Some(spawn_source) = spawn_source {
        let Some(audit) = spawn_source.audit(project, solution, catalog, maps, &cancelled)? else {
            return Ok(false);
        };
        maps.spawn_audit = Some(audit);
    }
    Ok(!cancelled())
}

#[cfg(test)]
mod overview_tests {
    use super::*;
    use eframe::egui::Color32;
    use papyrus_terrain::native_layers::{Layers, dimensions};
    use std::sync::Arc;
    #[test]
    fn layout_identity_tracks_pose_presence_metadata_and_float_bits() {
        use papyrus_terrain::placement::{Outcome, Position};
        let solution = Solution {
            outcomes: vec![Outcome {
                id: 7,
                position: Some(Position {
                    x: 0.,
                    y: 0.,
                    z: 0.,
                    yaw: 0.,
                    clearance: 80.,
                }),
                distance: Some(1.),
                within_radius: true,
                reason: "accepted".into(),
            }],
            required_missing: 0,
            attempts: 1,
        };
        let key = layout_fingerprint(&solution);
        assert_eq!(key, layout_fingerprint(&solution.clone()));
        for field in 0..11 {
            let mut changed = solution.clone();
            match field {
                0 => changed.outcomes[0].position.as_mut().unwrap().x = 1.,
                1 => changed.outcomes[0].position.as_mut().unwrap().y = -0.,
                2 => changed.outcomes[0].position.as_mut().unwrap().z = 1.,
                3 => changed.outcomes[0].position.as_mut().unwrap().yaw = 90.,
                4 => changed.outcomes[0].position = None,
                5 => changed.outcomes[0].id += 1,
                6 => changed.outcomes[0].reason.push('!'),
                7 => changed.outcomes[0].distance = None,
                8 => changed.outcomes[0].within_radius = false,
                9 => changed.required_missing += 1,
                _ => changed.attempts += 1,
            }
            assert_ne!(
                key,
                layout_fingerprint(&changed),
                "Unbound layout field {field}"
            );
        }
    }

    #[test]
    fn biome_only_composition_refreshes_overview_without_raw_material_patches() {
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[cells..2 * cells].fill(255);
        layers.splat[4 * cells..5 * cells].fill(255);
        let n = crate::native_preview::RESOLUTION;
        let source = Source {
            terrain: None,
            native_base: None,
            rivers: None,
            palette: None,
            key: Project::default().terrain_key(),
            image: ColorImage::filled([n, n], Color32::from_rgb(113, 132, 80)),
            heights: Arc::new(layers.sample_heights(n).unwrap()),
            layers: Arc::new(layers),
        };
        let mut maps =
            monument_composition::compose(&source.layers, &vec![255; cells], &[], || false)
                .unwrap()
                .unwrap();
        maps.layers.biome[cells..2 * cells].fill(0);
        maps.layers.biome[3 * cells..4 * cells].fill(255);
        let image = render_overview(&source, &maps).unwrap();
        // Flat terrain: 80% grass material + 20% arctic biome, with flat lighting.
        assert!(
            image
                .pixels
                .iter()
                .all(|p| *p == Color32::from_rgb(136, 150, 109))
        );
        assert_eq!(
            source.image.pixels[n * n / 2],
            Color32::from_rgb(113, 132, 80)
        );
        assert_eq!(source.layers.biome[cells + cells / 2], 255);
        assert_eq!(source.layers.splat, maps.layers.splat);
        assert_eq!(source.layers.height, maps.layers.height);
        maps.layers.biome.pop();
        assert!(render_overview(&source, &maps).is_err());
    }
}
