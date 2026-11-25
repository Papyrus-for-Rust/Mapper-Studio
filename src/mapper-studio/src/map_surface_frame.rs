//! Source-bound coarse surface preparation for immutable terrain revisions.
//! Cache keys retain weak allocation identities, never seeds or naked pointers.
use crate::{
    installation::{self, Installation, InstallationKind, SourceFile},
    map_surface::{self, Grid, HalfRounding, Settings},
};
use papyrus_terrain::{monument_composition::Composed, native_layers::Layers};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex, Weak},
};

pub const PROFILE: &str = "mapper-client-coarse-normal-bc3-e7156192-2";
const CODE: &str = "e7156192c3773f1f07cd58f0a7817b35705683bab4f40cd4b14599bc0a4f954f";
const METADATA: &str = "1aa177b0ddfea713a2050a40d03533f45d62e6f3c92fa75201afde3dd64b94d1";
const NATIVE: &str = "0f703d43b57672b8bb3dfd29bab0d484f8622fbac66dee7f1f058893ef22a99c";
const CONTENT: &str = "edc7d5d1dfa894a832e57620b4c0000ad01c0f6852bcea14b5dcde5417e3e5a0";
const MAPS: &str = "50d7c807360ad98b18b91588aafbec2a9d5bc7c2102cc90d4b250a63dc209a63";
const SHADER: &str = "9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Profile {
    Legacy,
    Current,
}
impl Profile {
    pub(crate) fn normal_version(self) -> &'static str {
        match self {
            Self::Legacy => crate::map_normal::VERSION,
            Self::Current => crate::map_normal::MIP_VERSION,
        }
    }
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Legacy => PROFILE,
            Self::Current => "mapper-client-surface-shore-199a470f-2",
        }
    }
}
pub(crate) fn settings(layers: &Layers, maximum_bytes: usize) -> Settings {
    settings_dimensions(layers.size, layers.height_resolution, maximum_bytes)
}
pub(crate) fn settings_dimensions(
    size: u32,
    height_resolution: usize,
    maximum_bytes: usize,
) -> Settings {
    Settings {
        height_resolution,
        size_x: size as f32,
        origin_y: -500.,
        vertical_span: 1000.,
        gradient_gain: 0.375,
        blur_pairs: 4,
        half_rounding: HalfRounding::TowardZero,
        maximum_work: 1_000_000_000,
        maximum_bytes,
    }
}

pub(crate) fn generate(
    height: &[i16],
    settings: Settings,
    profile: Profile,
    pool: Option<&rayon::ThreadPool>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if settings
        .height_resolution
        .checked_mul(settings.height_resolution)
        != Some(height.len())
    {
        return Err("Invalid bound surface height dimensions".into());
    }
    generate_sampled(
        |x, z| height.get(z * settings.height_resolution + x).copied(),
        settings,
        profile,
        pool,
        cancelled,
    )
}
/// Both inputs share one immutable encoded height provider. Temporary blur
/// storage is released before normal construction, and both retained inputs fit
/// the caller's remaining image allowance before publication.
pub(crate) fn generate_sampled(
    height: impl Fn(usize, usize) -> Option<i16> + Sync,
    mut settings: Settings,
    profile: Profile,
    pool: Option<&rayon::ThreadPool>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if profile == Profile::Current {
        settings.gradient_gain = 0.;
        settings.blur_pairs = 0;
    }
    let Some(mut grid) = map_surface::generate_sampled(&height, settings, &cancelled)? else {
        return Ok(None);
    };
    let remaining = settings
        .maximum_bytes
        .checked_sub(grid.retained_bytes())
        .ok_or("Bound surface exceeds image memory allowance")?;
    let size = settings.size_x as u32;
    if settings.size_x != size as f32 {
        return Err("Bound normal surface requires an integral world size".into());
    }
    let normal_settings = crate::map_normal::Settings {
        height_resolution: settings.height_resolution,
        size,
        origin_y: settings.origin_y,
        vertical_span: settings.vertical_span,
        maximum_work: 3_000_000_000usize
            .checked_sub(grid.report.work)
            .ok_or("Bound surface work allowance exceeded")?,
        maximum_bytes: remaining,
    };
    let normal = match profile {
        Profile::Legacy => {
            crate::map_normal::generate_sampled_in(&height, normal_settings, pool, &cancelled)
        }
        Profile::Current => crate::map_normal::generate_mipped_sampled_in(
            &height,
            normal_settings,
            pool,
            &cancelled,
        ),
    }?;
    let Some(normal) = normal else {
        return Ok(None);
    };
    grid.normal = Some(normal);
    Ok((!cancelled()).then_some(grid))
}

/// Called only after Palette has checked all three installation source files.
/// A shader hash alone does not identify the code that constructs its inputs.
pub(crate) fn bind(
    root: &Path,
    installation: &Installation,
    sources: &[SourceFile],
    shader: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Profile>, String> {
    if installation.kind != InstallationKind::Client
        || sources.len() != 3
        || sources[0] != installation.native
        || sources[1].relative_path != "Bundles/maps/maps.bundle"
        || sources[2].relative_path != "Bundles/shared/content.bundle"
    {
        return Ok(None);
    }
    let (profile, code, code_bytes, metadata, metadata_bytes) = if installation.native.sha256
        == NATIVE
        && shader == Some(SHADER)
        && sources[1].sha256 == MAPS
        && sources[2].sha256 == CONTENT
    {
        (Profile::Legacy, CODE, 304_039_424, METADATA, 68_271_512)
    } else if installation.native.sha256
        == "8dd4e23570729ddbd72844bdb8a5e04bfa671612b3e7d34841d17d2353f7015c"
        && shader == Some("60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1")
        && sources[1].sha256 == "8c268626b5e196e473acaaf7ee9c8e2d73e38b658f0d0f0615e91547cd15f6de"
        && sources[2].sha256 == "b6b5748193aff04b717a06793b2cb4534712baf3677588cfa0d5564ca3645679"
    {
        (
            Profile::Current,
            "199a470f2510320be42d127cc55e946f51a2fc0b0b3ed0bf8671c2a8bb73b252",
            270_636_368,
            "266b023e647e1fcedd7c4590ea110d5131bbb070bf23f3c42fb33f935cc9750a",
            68_754_532,
        )
    } else {
        return Ok(None);
    };
    for (path, expected, expected_bytes, limit) in [
        ("GameAssembly.dll", code, code_bytes, 512 * 1024 * 1024),
        (
            "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
            metadata,
            metadata_bytes,
            128 * 1024 * 1024,
        ),
    ] {
        if cancelled() {
            return Ok(None);
        }
        match std::fs::metadata(root.join(path)) {
            Ok(file) if file.len() != expected_bytes => return Ok(None),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        }
        // Older/different clients keep their existing renderer; no guessed
        // native layouts, coefficients or blur counts are applied to them.
        let source = match installation::hash_file(root, path, limit, &cancelled) {
            Ok(source) => source,
            Err(_) if !root.join(path).exists() => return Ok(None),
            Err(error) => return Err(error),
        };
        if source.sha256 != expected {
            return Ok(None);
        }
    }
    Ok((!cancelled()).then_some(profile))
}

fn fingerprint_water(
    water: &[i16],
    hr: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<[u8; 32]>, String> {
    if !(9..=4097).contains(&hr) || water.len() != hr * hr {
        return Err("Invalid bound shoreline water dimensions".into());
    }
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 65536];
    for chunk in water.chunks(bytes.len() / 2) {
        if cancelled() {
            return Ok(None);
        }
        for (&value, out) in chunk.iter().zip(bytes.chunks_exact_mut(2)) {
            if !(0..=32766).contains(&value) {
                return Err("Invalid bound shoreline water encoding".into());
            }
            out.copy_from_slice(&value.to_le_bytes());
        }
        hash.update(&bytes[..chunk.len() * 2]);
    }
    Ok(Some(hash.finalize().into()))
}

fn shoreline_input<'a>(
    layers: &'a Layers,
    height: &'a [i16],
    water: Option<&'a [i16]>,
) -> papyrus_terrain::shore_mainland::Input<'a> {
    let settings = settings(layers, 0);
    let mut terrain = layers.view();
    terrain.height = height;
    papyrus_terrain::shore_mainland::Input {
        terrain,
        water: water.map(|data| papyrus_terrain::water_map_query::Grid {
            resolution: layers.height_resolution,
            data,
            origin_y: settings.origin_y,
            vertical_span: settings.vertical_span,
        }),
        origin_y: settings.origin_y,
        vertical_span: settings.vertical_span,
        ocean_level: 0.,
    }
}

pub(crate) fn working_bytes(
    layers: &Layers,
    height: &[i16],
    water: Option<&[i16]>,
    profile: Profile,
) -> Result<usize, String> {
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if layers.height_resolution != hr || layers.surface_resolution != sr || height.len() != hr * hr {
        return Err("Invalid bound map surface dimensions".into());
    }
    let current = profile == Profile::Current;
    let resolution = (hr - 1) / 2;
    let cells = resolution * resolution;
    let coarse = map_surface::working_bytes(hr, !current);
    let normals = 4 * cells + crate::map_normal::working_bytes(resolution, current);
    if !current {
        return Ok(coarse.max(normals));
    }
    // The field producer reserves more space than the subsequent half-texture
    // upload, which overlaps the completed field but none of its scratch space.
    let shore = papyrus_terrain::shore_mainland::bounds(&shoreline_input(layers, height, water))?.0;
    let shore = usize::try_from(shore).map_err(|_| "Shoreline working size exceeds address space")?;
    let normal_mips = crate::map_normal::retained_bytes(resolution, true);
    Ok(coarse.max(normals).max(4 * cells + normal_mips + shore))
}

pub(crate) fn attach_shore(
    grid: &mut Grid,
    layers: &Layers,
    height: &[i16],
    water: Option<&[i16]>,
    maximum_bytes: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<()>, String> {
    let input = shoreline_input(layers, height, water);
    let peak = usize::try_from(papyrus_terrain::shore_mainland::bounds(&input)?.0)
        .map_err(|_| "Shoreline working size exceeds address space")?;
    let remaining = maximum_bytes
        .checked_sub(grid.retained_bytes())
        .ok_or("Bound shoreline exceeds the image working allowance")?;
    let Some(field) = papyrus_terrain::shore_mainland::generate(
        input,
        papyrus_terrain::shore_field::Limits {
            maximum_bytes: remaining as u64,
            maximum_work: 2_000_000_000,
        },
        cancelled,
    )?
    else {
        return Ok(None);
    };
    upload_shore(grid, field, peak, remaining, cancelled)
}

pub(crate) fn upload_shore(
    grid: &mut Grid,
    field: papyrus_terrain::shore_field::Field,
    peak: usize,
    remaining: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<()>, String> {
    let field_bytes = field.distances.len() * 4 + field.vectors.len() * 16;
    let texture_budget = remaining
        .checked_sub(field_bytes)
        .ok_or("Bound shoreline upload exceeds the image working allowance")?;
    let Some(texture) = field.texture(texture_budget as u64, cancelled)? else {
        return Ok(None);
    };
    grid.shore = Some(Arc::new(texture));
    grid.shore_working_bytes = peak;
    Ok(Some(()))
}

#[derive(Clone, Debug)]
enum Owner {
    Native(Weak<Layers>),
    Composed(Weak<Composed>),
}
impl Owner {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Native(a), Self::Native(b)) => Weak::ptr_eq(a, b),
            (Self::Composed(a), Self::Composed(b)) => Weak::ptr_eq(a, b),
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct Frame {
    owner: Owner,
    grid: Grid,
    profile: Profile,
    water_key: Option<[u8; 32]>,
}
impl Frame {
    pub(crate) fn for_output(self: &Arc<Self>, output: usize) -> Arc<Self> {
        let grid = self.grid.for_footprint(1. / output as f32);
        if grid.normal_lod() == self.grid.normal_lod() {
            return self.clone();
        }
        Arc::new(Self {
            owner: self.owner.clone(),
            grid,
            profile: self.profile,
            water_key: self.water_key,
        })
    }

    pub fn retained_bytes(&self) -> usize {
        self.grid.retained_bytes()
    }

    pub fn native(&self, layers: &Arc<Layers>) -> Option<&Grid> {
        self.owner
            .same(&Owner::Native(Arc::downgrade(layers)))
            .then_some(&self.grid)
    }
    pub fn composed(&self, maps: &Arc<Composed>) -> Option<&Grid> {
        self.owner
            .same(&Owner::Composed(Arc::downgrade(maps)))
            .then_some(&self.grid)
    }
}

#[derive(Default, Debug)]
pub(crate) struct Cache(Mutex<Vec<Weak<Frame>>>);
impl Cache {
    pub(crate) fn native(
        &self,
        layers: &Arc<Layers>,
        profile: Profile,
        output: usize,
        maximum_bytes: usize,
        pool: Option<&rayon::ThreadPool>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Arc<Frame>>, String> {
        self.native_with_water(
            layers,
            None,
            profile,
            output,
            maximum_bytes,
            pool,
            cancelled,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn native_with_water(
        &self,
        layers: &Arc<Layers>,
        water: Option<&[i16]>,
        profile: Profile,
        output: usize,
        maximum_bytes: usize,
        pool: Option<&rayon::ThreadPool>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Arc<Frame>>, String> {
        if cancelled() {
            return Ok(None);
        }
        let water_key = if profile == Profile::Current
            && let Some(water) = water
        {
            let Some(key) = fingerprint_water(water, layers.height_resolution, &cancelled)? else {
                return Ok(None);
            };
            Some(key)
        } else {
            None
        };
        self.prepare(
            Owner::Native(Arc::downgrade(layers)),
            water,
            water_key,
            profile,
            output,
            layers,
            &layers.height,
            maximum_bytes,
            pool,
            cancelled,
        )
    }
    pub(crate) fn composed(
        &self,
        maps: &Arc<Composed>,
        profile: Profile,
        output: usize,
        maximum_bytes: usize,
        pool: Option<&rayon::ThreadPool>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Arc<Frame>>, String> {
        let height = maps
            .gameplay_height
            .as_ref()
            .map_or(maps.layers.height.as_slice(), |h| &h.height);
        self.prepare(
            Owner::Composed(Arc::downgrade(maps)),
            maps.ocean.as_ref().map(|o| o.local_water.as_slice()),
            None,
            profile,
            output,
            &maps.layers,
            height,
            maximum_bytes,
            pool,
            cancelled,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &self,
        owner: Owner,
        water: Option<&[i16]>,
        water_key: Option<[u8; 32]>,
        profile: Profile,
        output: usize,
        layers: &Layers,
        height: &[i16],
        maximum_bytes: usize,
        pool: Option<&rayon::ThreadPool>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Arc<Frame>>, String> {
        if cancelled() {
            return Ok(None);
        }
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(layers.size)?;
        if layers.height_resolution != hr
            || layers.surface_resolution != sr
            || height.len() != hr * hr
        {
            return Err("Invalid bound map surface dimensions".into());
        }
        if !(2..=4096).contains(&output) {
            return Err("Invalid surface sampling resolution".into());
        }
        let settings = settings(layers, maximum_bytes);
        let lod = if profile == Profile::Current {
            (((hr - 1) / 2) as f32 * (1. / output as f32))
                .max(1.)
                .log2()
        } else {
            0.
        };
        if let Some(frame) = self.find(&owner, profile, Some(lod), water_key, true) {
            if frame.grid.working_bytes() > maximum_bytes {
                return Err("Cached map surface exceeds the image working budget".into());
            }
            return Ok(Some(frame));
        }
        let shared = self
            .find(&owner, profile, None, water_key, true)
            .or_else(|| self.find(&owner, profile, None, water_key, false));
        let mut grid = if let Some(frame) = shared {
            if frame.grid.working_bytes() > maximum_bytes {
                return Err("Cached map surface exceeds the image working budget".into());
            }
            let mut grid = frame.grid.clone();
            if frame.water_key != water_key {
                grid.shore = None;
                grid.shore_working_bytes = 0;
            }
            grid
        } else {
            let Some(grid) = generate(height, settings, profile, pool, &cancelled)? else {
                return Ok(None);
            };
            grid
        };
        if profile == Profile::Current && grid.shore().is_none() {
            let Some(()) =
                attach_shore(&mut grid, layers, height, water, maximum_bytes, &cancelled)?
            else {
                return Ok(None);
            };
        }
        let grid = if profile == Profile::Current {
            grid.for_footprint(1. / output as f32)
        } else {
            grid
        };
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(self.remember(Arc::new(Frame {
            owner,
            grid,
            profile,
            water_key,
        }))))
    }

    pub(crate) fn remember(&self, frame: Arc<Frame>) -> Arc<Frame> {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|w| w.strong_count() != 0);
        if let Some(existing) = entries.iter().filter_map(Weak::upgrade).find(|f| {
            f.owner.same(&frame.owner)
                && f.profile == frame.profile
                && f.grid.normal_lod() == frame.grid.normal_lod()
                && f.water_key == frame.water_key
        }) {
            return existing;
        }
        if entries.len() >= 64 {
            entries.remove(0);
        }
        entries.push(Arc::downgrade(&frame));
        frame
    }
    fn find(
        &self,
        owner: &Owner,
        profile: Profile,
        lod: Option<f32>,
        water_key: Option<[u8; 32]>,
        same_water: bool,
    ) -> Option<Arc<Frame>> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .find(|f| {
                f.owner.same(owner)
                    && f.profile == profile
                    && lod.is_none_or(|v| f.grid.normal_lod() == v)
                    && (!same_water || f.water_key == water_key)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_large_image_allowances_preserve_surface_output() {
        let height = vec![17000; 65 * 65];
        for profile in [Profile::Legacy, Profile::Current] {
            let baseline = generate(
                &height,
                settings_dimensions(1000, 65, 512 * 1024 * 1024),
                profile,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
            for mib in [1024, 2048, 4096] {
                let settings = settings_dimensions(1000, 65, mib * 1024 * 1024);
                let surface = generate(&height, settings, profile, None, || false)
                    .unwrap()
                    .unwrap();
                assert_eq!(surface.pixels(), baseline.pixels());
                assert_eq!(
                    surface.normal().unwrap().pixels(),
                    baseline.normal().unwrap().pixels()
                );
                assert_eq!(surface.working_bytes(), baseline.working_bytes());
            }
        }
    }
    #[test]
    fn combined_inputs_account_for_both_textures_and_lazy_reads_match() {
        let height = (0..65 * 65)
            .map(|i| (17000 + (i % 65) * 20) as i16)
            .collect::<Vec<_>>();
        let mut s = settings_dimensions(1000, 65, 32 * 32 * 4);
        s.blur_pairs = 0;
        assert!(generate(&height, s, Profile::Legacy, None, || false).is_err());
        s.maximum_bytes *= 2;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let a = generate(&height, s, Profile::Legacy, Some(&pool), || false)
            .unwrap()
            .unwrap();
        let b = generate_sampled(
            |x, z| Some(height[z * 65 + x]),
            s,
            Profile::Legacy,
            Some(&pool),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.pixels(), b.pixels());
        assert_eq!(a.normal().unwrap().pixels(), b.normal().unwrap().pixels());
        assert_eq!(a.report.working_bytes, 32 * 32 * 4 + 3 * 65 * 4);
        assert_eq!(a.retained_bytes(), s.maximum_bytes);
        assert_eq!(a.working_bytes(), s.maximum_bytes);
        assert!(
            generate(&height, s, Profile::Legacy, Some(&pool), || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn prepared_normals_follow_height_revisions_without_retaining_source_layers() {
        let cache = Cache::default();
        let mut layers = layers();
        let budget = 8 * 1024 * 1024;
        let first = cache
            .native(&layers, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        let before = first
            .native(&layers)
            .unwrap()
            .normal()
            .unwrap()
            .sample([0.5, 0.5])
            .unwrap();
        let address = layers.height.as_ptr();
        let n = layers.height_resolution;
        for (i, h) in Arc::make_mut(&mut layers).height.iter_mut().enumerate() {
            *h += (i % n) as i16 * 5;
        }
        assert_eq!(address, layers.height.as_ptr());
        assert!(first.native(&layers).is_none());
        let edited = cache
            .native(&layers, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        assert_ne!(
            before,
            edited
                .native(&layers)
                .unwrap()
                .normal()
                .unwrap()
                .sample([0.5, 0.5])
                .unwrap()
        );
        assert_eq!(Arc::strong_count(&layers), 1);
        assert!(Arc::try_unwrap(layers).is_ok());
    }
    fn layers() -> Arc<Layers> {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        Arc::new(Layers {
            size: 1000,
            terrain_seed: 4,
            biome_seed: 4,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        })
    }
    #[test]
    fn cache_tracks_revision_even_when_make_mut_keeps_the_same_height_address() {
        let cache = Cache::default();
        let mut layers = layers();
        let budget = 8 * 1024 * 1024;
        let first = cache
            .native(&layers, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        let again = cache
            .native(&layers, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(Arc::strong_count(&layers), 1);
        let original = first.native(&layers).unwrap().sample([0.5, 0.5]).unwrap();
        assert!(
            cache
                .native(&layers, Profile::Legacy, 2048, 1, None, || false)
                .is_err()
        );
        assert!(
            cache
                .native(&layers, Profile::Legacy, 2048, budget, None, || true)
                .unwrap()
                .is_none()
        );
        let address = layers.height.as_ptr();
        Arc::make_mut(&mut layers).height.fill(22000);
        assert_eq!(layers.height.as_ptr(), address);
        assert!(first.native(&layers).is_none());
        let edited = cache
            .native(&layers, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &edited));
        assert_ne!(
            edited.native(&layers).unwrap().sample([0.5, 0.5]).unwrap(),
            original
        );
        let weak = Arc::downgrade(&edited);
        drop(edited);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn baseline_and_composed_frames_never_alias_despite_identical_seeds() {
        let cache = Cache::default();
        let base = layers();
        let budget = 8 * 1024 * 1024;
        let baseline = cache
            .native(&base, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        let mut maps = papyrus_terrain::monument_composition::compose(
            &base,
            &vec![255; base.surface_resolution.pow(2)],
            &[],
            || false,
        )
        .unwrap()
        .unwrap();
        maps.layers.height.fill(22000);
        let maps = Arc::new(maps);
        let composed = cache
            .composed(&maps, Profile::Legacy, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        assert!(baseline.composed(&maps).is_none());
        assert!(composed.native(&base).is_none());
        assert_ne!(
            baseline.native(&base).unwrap().sample([0.5, 0.5]),
            composed.composed(&maps).unwrap().sample([0.5, 0.5])
        );
        assert_eq!(Arc::strong_count(&maps), 1);
        assert!(Arc::try_unwrap(maps).is_ok());
    }
    #[test]
    fn current_frames_share_pixels_across_sampling_scales_and_separate_producer_profiles() {
        let cache = Cache::default();
        let layers = layers();
        let budget = 16 * 1024 * 1024;
        let full = cache
            .native(&layers, Profile::Current, 2048, budget, None, || false)
            .unwrap()
            .unwrap();
        let small = cache
            .native(&layers, Profile::Current, 97, budget, None, || false)
            .unwrap()
            .unwrap();
        let repeat = cache
            .native(&layers, Profile::Current, 97, budget, None, || false)
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&small, &repeat));
        assert!(!Arc::ptr_eq(&full, &small));
        let a = full.native(&layers).unwrap();
        let b = small.native(&layers).unwrap();
        assert_eq!(a.pixels().as_ptr(), b.pixels().as_ptr());
        assert!(std::ptr::eq(a.shore().unwrap(), b.shore().unwrap()));
        assert_eq!(
            a.normal().unwrap().all_pixels().as_ptr(),
            b.normal().unwrap().all_pixels().as_ptr()
        );
        assert_eq!(a.normal_lod(), 0.);
        assert!(b.normal_lod() > 2.);
        assert_eq!(a.report.blur_passes, 0);
        assert!(a.pixels().iter().all(|p| p[1] == 0));
        let legacy = cache
            .native(&layers, Profile::Legacy, 97, budget, None, || false)
            .unwrap()
            .unwrap();
        let old = legacy.native(&layers).unwrap();
        assert_eq!(old.report.blur_passes, 8);
        assert_ne!(a.pixels().as_ptr(), old.pixels().as_ptr());
        assert_eq!(a.normal().unwrap().pixels(), old.normal().unwrap().pixels());
        assert!(
            a.pixels()
                .iter()
                .zip(old.pixels())
                .all(|(a, b)| a[0] == b[0])
        );
        assert!(a.normal().unwrap().all_pixels().len() > old.normal().unwrap().all_pixels().len());
        assert!(
            cache
                .native(&layers, Profile::Current, 0, budget, None, || false)
                .is_err()
        );
        assert!(
            cache
                .native(&layers, Profile::Current, 97, 1, None, || false)
                .is_err()
        );
    }

    #[test]
    fn shoreline_cache_tracks_water_contents_and_topology_without_retaining_sources() {
        let cache = Cache::default();
        let mut layers = layers();
        let hr = layers.height_resolution;
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let mut water: Vec<_> = (0..hr * hr)
            .map(|i| if i % hr < hr / 2 { encode(60.) } else { 0 })
            .collect();
        let budget = 32 * 1024 * 1024;
        let first = cache
            .native_with_water(
                &layers,
                Some(&water),
                Profile::Current,
                64,
                budget,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
        let resized = cache
            .native_with_water(
                &layers,
                Some(&water),
                Profile::Current,
                97,
                budget,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
        assert!(std::ptr::eq(
            first.grid.shore().unwrap(),
            resized.grid.shore().unwrap()
        ));
        assert!(first.grid.shore().unwrap().sample([0.25, 0.5]).unwrap()[2] > 0.);
        let address = water.as_ptr();
        for (i, value) in water.iter_mut().enumerate() {
            *value = if i % hr < hr / 2 { 0 } else { encode(60.) };
        }
        assert_eq!(address, water.as_ptr());
        let changed = cache
            .native_with_water(
                &layers,
                Some(&water),
                Profile::Current,
                64,
                budget,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.grid.shore().unwrap().sample([0.25, 0.5]).unwrap()[2] < 0.);
        assert!(first.grid.shore().unwrap().sample([0.25, 0.5]).unwrap()[2] > 0.);
        assert_eq!(
            first.grid.normal().unwrap().pixels().as_ptr(),
            changed.grid.normal().unwrap().pixels().as_ptr()
        );
        for (i, value) in water.iter_mut().enumerate() {
            *value = if i % hr < hr / 2 { encode(60.) } else { 0 };
        }
        let restored = cache
            .native_with_water(
                &layers,
                Some(&water),
                Profile::Current,
                64,
                budget,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &restored));
        let height_address = layers.height.as_ptr();
        Arc::make_mut(&mut layers).topology.fill(8192);
        assert_eq!(height_address, layers.height.as_ptr());
        assert!(first.native(&layers).is_none());
        let painted = cache
            .native_with_water(
                &layers,
                Some(&water),
                Profile::Current,
                64,
                budget,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            painted.grid.shore().unwrap().sample([0.25, 0.5]).unwrap()[3],
            2.
        );
        assert_eq!(
            first.grid.shore().unwrap().sample([0.25, 0.5]).unwrap()[3],
            0.
        );
        assert!(
            cache
                .native_with_water(
                    &layers,
                    Some(&water),
                    Profile::Current,
                    64,
                    budget,
                    None,
                    || true
                )
                .unwrap()
                .is_none()
        );
        water[0] = -1;
        assert!(
            cache
                .native_with_water(
                    &layers,
                    Some(&water),
                    Profile::Current,
                    64,
                    budget,
                    None,
                    || false
                )
                .is_err()
        );
        assert!(
            cache
                .native_with_water(&layers, None, Profile::Current, 64, 1, None, || false)
                .is_err()
        );
        assert_eq!(Arc::strong_count(&layers), 1);
        assert!(Arc::try_unwrap(layers).is_ok());
    }

    #[test]
    #[ignore = "requires an owned installation and its verified palette cache"]
    fn owned_selected_profile_prepares_shared_frames_and_matching_point_colors() {
        let root = std::path::PathBuf::from(std::env::var_os("MAPPER_RENDER_TEST_ROOT").unwrap());
        let path =
            std::path::PathBuf::from(std::env::var_os("MAPPER_RENDER_TEST_PALETTE").unwrap());
        let expected = match std::env::var("MAPPER_RENDER_TEST_PROFILE")
            .unwrap()
            .as_str()
        {
            "current" => Profile::Current,
            "legacy" => Profile::Legacy,
            _ => panic!("Unknown owned test profile"),
        };
        let installation = installation::inspect(&root, || false).unwrap();
        let palette = crate::terrain_palette::Palette::load(&root, &path, &installation, || false)
            .unwrap()
            .unwrap();
        assert_eq!(palette.surface_profile(), Some(expected));
        let mut terrain = (*layers()).clone();
        let hr = terrain.height_resolution;
        for (i, h) in terrain.height.iter_mut().enumerate() {
            let elevation =
                ((i % hr) as f32 / 23.).sin() * 25. + ((i / hr) as f32 / 37.).cos() * 12. - 10.;
            *h = ((elevation + 500.) / 1000. * 32766.).round() as i16;
        }
        let terrain = Arc::new(terrain);
        let large =
            crate::map_image::prepare_native_surface(&terrain, 2048, Some(&palette), || false)
                .unwrap()
                .unwrap();
        let rendered =
            crate::map_image::render_native_owned(&terrain, None, None, 97, Some(&palette), || {
                false
            })
            .unwrap()
            .unwrap();
        let small = rendered.surface.as_ref().unwrap();
        let a = large.native(&terrain).unwrap();
        let b = small.native(&terrain).unwrap();
        assert_eq!(a.pixels().as_ptr(), b.pixels().as_ptr());
        assert_eq!(
            a.normal().unwrap().all_pixels().as_ptr(),
            b.normal().unwrap().all_pixels().as_ptr()
        );
        let mut maximum_point_delta = 0;
        for y in [0, 15, 48, 70, 96] {
            for x in [0, 18, 48, 76, 96] {
                let p = [
                    ((x as f32 + 0.5) / 97. - 0.5) * 1000.,
                    (0.5 - (y as f32 + 0.5) / 97.) * 1000.,
                ];
                let sampled = crate::map_image::sample_native_surface(
                    &terrain,
                    None,
                    None,
                    p,
                    &palette,
                    Some(small),
                )
                .unwrap();
                for (a, b) in sampled
                    .to_array()
                    .into_iter()
                    .zip(rendered.image.pixels[y * 97 + x].to_array())
                {
                    maximum_point_delta = maximum_point_delta.max(a.abs_diff(b));
                }
            }
        }
        assert!(maximum_point_delta <= 1);
        assert_eq!(
            a.report.blur_passes,
            if expected == Profile::Current { 0 } else { 8 }
        );
        if expected == Profile::Current {
            assert!(b.normal_lod() > 2.);
        }
        if let Some(path) = std::env::var_os("MAPPER_RENDER_TEST_REPORT") {
            let report = serde_json::json!({"format":"mapper-owned-render-profile-verification","schema":1,
                "installation":installation,"palette_sha256":palette.sha256,"render_key":palette.source_render_key(),
                "terrain_input":"synthetic","output":97,"sampling_lod":b.normal_lod(),
                "normal_pixel_bytes":b.normal().unwrap().report.pixel_bytes,"surface_working_bytes":b.working_bytes(),
                "frames_share_pixels":true,"point_queries_checked":25,"maximum_point_channel_delta":maximum_point_delta,
                "game_runtime_executed":false});
            let file = std::fs::File::create_new(path).unwrap();
            serde_json::to_writer_pretty(file, &report).unwrap();
        }
    }

    #[test]
    fn source_binding_rejects_uninspected_code_and_partial_source_identity() {
        let root = tempfile::tempdir().unwrap();
        let installation = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: NATIVE.into(),
                bytes: 1,
            },
            scene_manifest: None,
        };
        let sources = vec![
            installation.native.clone(),
            SourceFile {
                relative_path: "Bundles/maps/maps.bundle".into(),
                sha256: MAPS.into(),
                bytes: 1,
            },
            SourceFile {
                relative_path: "Bundles/shared/content.bundle".into(),
                sha256: CONTENT.into(),
                bytes: 1,
            },
        ];
        assert!(
            bind(root.path(), &installation, &sources, Some(SHADER), || false)
                .unwrap()
                .is_none()
        );
        std::fs::write(
            root.path().join("GameAssembly.dll"),
            b"different client code",
        )
        .unwrap();
        assert!(
            bind(root.path(), &installation, &sources, Some(SHADER), || false)
                .unwrap()
                .is_none()
        );
        assert!(
            bind(root.path(), &installation, &sources, Some(SHADER), || true)
                .unwrap()
                .is_none()
        );
        assert!(
            bind(root.path(), &installation, &sources, None, || false)
                .unwrap()
                .is_none()
        );
        let mut changed = sources;
        changed[2].sha256 = "0".repeat(64);
        assert!(
            bind(root.path(), &installation, &changed, Some(SHADER), || false)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn color_overrides_reuse_geometry_but_have_their_own_render_key() {
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        let mut material = crate::map_colors::tests::fixture();
        material.shader_blob_sha256 = Some(SHADER.into());
        doc["cartography"] = serde_json::to_value(material).unwrap();
        let mut palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        assert!(!palette.has_surface_profile());
        let legacy = palette.source_render_key();
        palette.test_surface_profile();
        assert_ne!(palette.source_render_key(), legacy);
        let layers = layers();
        let budget = 8 * 1024 * 1024;
        let frame = palette
            .prepare_native_surface(&layers, budget, || false)
            .unwrap()
            .unwrap();
        let mut colors = crate::map_colors::Preferences {
            enabled: true,
            ..Default::default()
        };
        colors.overrides[0] = Some([20, 30, 40]);
        let recolored = palette.with_display_colors(&colors);
        let same = recolored
            .prepare_native_surface(&layers, budget, || false)
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&frame, &same));
        assert_eq!(palette.source_render_key(), recolored.source_render_key());
        assert_ne!(palette.render_key(), recolored.render_key());
        assert_eq!(palette.bytes(), recolored.bytes());
    }
}
