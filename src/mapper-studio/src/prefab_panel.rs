use eframe::egui;
use crate::diagnostic_ui;
use mapper_studio::{
    AssetIdentity, Project, installation::Installation, prefab_assets::Catalog, prefab_textures::Bindings,
    monument_addons, splat_preview,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

#[cfg(test)]
pub(super) fn palette_fixture(source: &Installation) -> Arc<mapper_studio::terrain_palette::Palette> {
        let materials=(0..8).map(|i|serde_json::json!({"name":format!("Material {i}"),"biome_colors":(vec![[0.2,0.3,0.4,1.];5])})).collect::<Vec<_>>();
        let colors=(0..11).map(|i|[i as f32*0.08,0.3,0.2,1.]).collect::<Vec<_>>();
        let doc=serde_json::json!({"format":"papyrus-terrain-palette","schema":1,"adapter":"papyrus-terrain-palette-1","extractor_sha256":"b".repeat(64),"sources":[source.native,{"relative_path":"Bundles/maps/maps.bundle","sha256":"c".repeat(64),"bytes":1},{"relative_path":"Bundles/shared/content.bundle","sha256":"d".repeat(64),"bytes":1}],"scene":"BuildPlayer-Procedural Map","binding":{"scene_file":"BuildPlayer-Procedural Map","world_setup":1,"terrain":2,"generator":3,"config_file":"CAB-config","config_object":4},"config_paths":["assets/test.asset"],"numeric_space":"unity-serialized-color","biomes":["Arid","Temperate","Tundra","Arctic","Jungle"],"materials":materials,
            "cartography":{"material_path":"assets/prefabs/tools/map/papermap.mat","serialized_file":"CAB-config","material_object":5,"shader_object":6,"shader_name":"Custom/Map","colors":colors,"sun_direction":[0.95,2.87,2.37],"sun_power":0.65,"brightness":1.05,"contrast":0.94}});
        Arc::new(mapper_studio::terrain_palette::Palette::decode(&serde_json::to_vec(&doc).unwrap(),source).unwrap())
}

struct Task {
    source: Installation,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Catalog, String>>,
}
struct BindingTask {
    source: Installation,
    catalog: String,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Bindings, String>>,
}
#[derive(Clone)]
struct PreparationRequest {
    source: Installation,
    root: PathBuf,
    pins: Option<AssetIdentity>,
}
impl PreparationRequest {
    fn matches(&self, root: Option<&Path>, project: &Project) -> bool {
        root == Some(self.root.as_path())
            && project.installation.as_ref() == Some(&self.source)
            && project.monument_assets == self.pins
    }
}
struct CachedTask {
    request: PreparationRequest,
    superseded: bool,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<(Catalog, Bindings, mapper_studio::client_preparation::Report), String>>,
}
impl Drop for CachedTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Drop for BindingTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Clone, PartialEq, Eq)]
struct StampKey {
    catalog: String,
    bindings: String,
    path: String,
}
struct StampTask {
    key: StampKey,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<splat_preview::Preview>, String>>,
}
impl Drop for StampTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub(super) struct Stamp {
    key: StampKey,
    pub texture: egui::TextureHandle,
    pub geometry: splat_preview::Geometry,
    pub default_yaw: f32,
    image: egui::ColorImage,
    contextual_base: egui::ColorImage,
    contextual_biome: Option<egui::ColorImage>,
    contextual_hit: Option<egui::ColorImage>,
    material_weights: Option<splat_preview::WeightImage<8>>,
    biome_weights: Option<splat_preview::WeightImage<5>>,
    palette_hit: Option<egui::ColorImage>,
    contextual: std::cell::RefCell<std::collections::BTreeMap<(u64, bool), ContextualStamp>>,
}
struct ContextualStamp {
    // Keep allocation identity without retaining the large terrain grids.
    // A bare address can be reused after the previous terrain is dropped.
    source: Weak<papyrus_terrain::native_layers::Layers>,
    composed: Option<Weak<papyrus_terrain::monument_composition::Composed>>,
    palette: Option<Weak<mapper_studio::terrain_palette::Palette>>,
    surface:Option<Weak<mapper_studio::map_surface_frame::Frame>>,
    pose: [f32; 3],
    texture: egui::TextureHandle,
    cartographic_hit: Option<([usize;2],Vec<u8>)>,
}
#[derive(Clone, Copy,Default)]
pub(super) struct StampContext<'a> {
    pub base: Option<&'a Arc<papyrus_terrain::native_layers::Layers>>,
    pub composed: Option<&'a Arc<papyrus_terrain::monument_composition::Composed>>,
    pub palette: Option<&'a Arc<mapper_studio::terrain_palette::Palette>>,
    pub base_surface:Option<&'a Arc<mapper_studio::map_surface_frame::Frame>>,
    pub composed_surface:Option<&'a Arc<mapper_studio::map_surface_frame::Frame>>,
}
impl<'a> StampContext<'a> {
    fn frame(self)->Option<&'a Arc<mapper_studio::map_surface_frame::Frame>> {
        if self.composed.is_some(){self.composed_surface}else{self.base_surface}
    }
    fn grid(self)->Option<&'a mapper_studio::map_surface::Grid> {
        if let Some(maps)=self.composed {self.frame()?.composed(maps)}else{self.frame()?.native(self.base?)}
    }
}
fn same_frame(a:&Option<Weak<mapper_studio::map_surface_frame::Frame>>,b:&Option<Weak<mapper_studio::map_surface_frame::Frame>>)->bool {
    match(a,b){(None,None)=>true,(Some(a),Some(b))=>Weak::ptr_eq(a,b),_=>false}
}
impl Stamp {
    pub fn hit_test_displayed(&self,point:[f32;2],pose:[f32;3],contextual:bool,context:StampContext<'_>,instance_key:(u64,bool),baked:bool)->bool {
        let Some(palette)=context.palette.filter(|p|contextual && p.cartography().is_some() && self.material_weights.is_some()) else {
            return self.hit_test_source(point,pose,contextual,context.palette.is_some());
        };
        if baked {
            if !splat_preview::hit_test_material_weights(self.material_weights.as_ref().unwrap(),self.geometry,point,pose){return false;}
            let Some((base,maps))=context.base.zip(context.composed) else{return false;};
            let before=mapper_studio::map_image::sample_native_surface(base,None,maps.ocean.as_ref(),point,palette,context.base_surface.map(Arc::as_ref));
            let after=mapper_studio::map_image::sample_composed_surface(maps,point,palette,context.composed_surface.map(Arc::as_ref));
            // Keep the footprint selectable when the preview omitted a shading frame.
            return before.zip(after).map_or_else(
                || palette.has_surface_profile() && (context.base_surface.is_none() || context.composed_surface.is_none()),
                |(a,b)| (0..3).any(|c|a[c].abs_diff(b[c])>=2),
            );
        }
        let surface_key=context.frame().map(Arc::downgrade);
        let composed=context.composed.map(Arc::downgrade);
        let source=context.base.filter(|_|composed.is_none()).map(Arc::downgrade).unwrap_or_default();
        let cached=self.contextual.borrow();
        let Some(frame)=cached.get(&instance_key).filter(|v|v.pose==pose && Weak::ptr_eq(&v.source,&source)
            && match(&v.composed,&composed){(None,None)=>true,(Some(a),Some(b))=>Weak::ptr_eq(a,b),_=>false}
            && same_frame(&v.surface,&surface_key) && v.palette.as_ref().is_some_and(|p|Weak::ptr_eq(p,&Arc::downgrade(palette)))) else{return false;};
        frame.cartographic_hit.as_ref().is_some_and(|(size,mask)|splat_preview::hit_test_mask(*size,mask,self.geometry,point,pose))
    }
    #[cfg(test)]
    pub fn hit_test(&self, point: [f32; 2], pose: [f32; 3], contextual: bool) -> bool {
        self.hit_test_source(point, pose, contextual, false)
    }
    pub fn hit_test_source(
        &self,
        point: [f32; 2],
        pose: [f32; 3],
        contextual: bool,
        palette: bool,
    ) -> bool {
        splat_preview::hit_test(
            if contextual
                && palette
                && let Some(image) = self.palette_hit.as_ref()
            {
                image
            } else if contextual {
                self.contextual_hit
                    .as_ref()
                    .unwrap_or(&self.contextual_base)
            } else {
                &self.image
            },
            self.geometry,
            point,
            pose,
        )
    }
    fn prune_context(&self, project: &Project) {
        self.contextual.borrow_mut().retain(|(id, _), _| {
            project
                .targets
                .iter()
                .any(|t| t.instance_id == *id && t.prefab_path.as_deref() == Some(&self.key.path))
        });
    }
    #[cfg(test)]
    pub fn texture_at(
        &self,
        ctx: &egui::Context,
        layers: Option<&Arc<papyrus_terrain::native_layers::Layers>>,
        instance_key: (u64, bool),
        pose: [f32; 3],
        overview: bool,
    ) -> egui::TextureId {
        self.texture_at_source(ctx, (layers, None), instance_key, pose, overview)
    }
    #[cfg(test)]
    pub fn texture_at_source(
        &self,
        ctx: &egui::Context,
        context: (
            Option<&Arc<papyrus_terrain::native_layers::Layers>>,
            Option<&Arc<mapper_studio::terrain_palette::Palette>>,
        ),
        instance_key: (u64, bool),
        pose: [f32; 3],
        overview: bool,
    ) -> egui::TextureId {
        let (layers, palette) = context;
        self.texture_at_background(
            ctx,
            StampContext {base_surface:None,composed_surface:None,
                base: layers,
                composed: None,
                palette,
            },
            instance_key,
            pose,
            overview,
        )
    }
    pub fn texture_at_background(
        &self,
        ctx: &egui::Context,
        context: StampContext<'_>,
        instance_key: (u64, bool),
        pose: [f32; 3],
        overview: bool,
    ) -> egui::TextureId {
        let Some(layers) = context
            .composed
            .map(|m| &m.layers)
            .or_else(|| context.base.map(Arc::as_ref))
            .filter(|_| overview)
        else {
            return self.texture.id();
        };
        // Borrow the displayed composition without cloning its large grids.
        // Keep only weak allocation identities in the per-pose texture cache.
        let surface_key=context.frame().map(Arc::downgrade);
        let composed = context.composed.map(Arc::downgrade);
        let source = context
            .base
            .filter(|_| composed.is_none())
            .map(Arc::downgrade)
            .unwrap_or_default();
        let palette = context.palette.filter(|_| self.material_weights.is_some());
        let palette_key = palette.map(Arc::downgrade);
        let mut cached = self.contextual.borrow_mut();
        if cached.get(&instance_key).is_none_or(|v| {
            !Weak::ptr_eq(&v.source, &source)
                || v.pose != pose || !same_frame(&v.surface,&surface_key)
                || match (&v.composed, &composed) {
                    (None, None) => false,
                    (Some(a), Some(b)) => !Weak::ptr_eq(a, b),
                    _ => true,
                }
                || match (&v.palette, &palette_key) {
                    (None, None) => false,
                    (Some(a), Some(b)) => !Weak::ptr_eq(a, b),
                    _ => true,
                }
        }) {
            let image = if let Some(palette) = palette {
                splat_preview::contextual_image_with_water(
                    self.material_weights.as_ref().unwrap(),
                    self.biome_weights.as_ref(),
                    self.geometry,
                    layers,
                    pose,
                    palette,
                    context.grid(),
                    context.composed.and_then(|m|m.gameplay_height.as_ref().map(|h|h.height.as_slice())),
                    context.composed.and_then(|m|m.ocean.as_ref()),
                )
            } else {
                splat_preview::contextual_image_with_biome(
                    &self.contextual_base,
                    self.contextual_biome.as_ref(),
                    self.geometry,
                    layers,
                    pose,
                )
            };
            let cartographic_hit=palette.filter(|p|p.cartography().is_some()).map(|_|(image.size,image.pixels.iter().map(|p|p.a()).collect()));
            if let Some(old) = cached.get_mut(&instance_key) {
                old.texture.set(image, egui::TextureOptions::LINEAR);
                old.surface=surface_key;
                old.source = source;
                old.composed = composed;
                old.palette = palette_key;
                old.pose = pose;
                old.cartographic_hit=cartographic_hit;
            } else {
                cached.insert(
                    instance_key,
                    ContextualStamp {
                        surface:surface_key,
                        source,
                        composed,
                        palette: palette_key,
                        pose,
                        cartographic_hit,
                        texture: ctx.load_texture(
                            "contextual-monument-paint",
                            image,
                            egui::TextureOptions::LINEAR,
                        ),
                    },
                );
            }
        }
        cached[&instance_key].texture.id()
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct PrefabPanel {
    prepared_generation: Option<mapper_studio::client_preparation::Report>,
    prepared_identities: Option<PreparedIdentities>,
    prepared_palette: Option<(PathBuf, Arc<mapper_studio::terrain_palette::Palette>)>,
    preparation_details: String,
    accepted: Option<Arc<Catalog>>,
    task: Option<Task>,
    message: String,
    bindings: Option<(String, Arc<Bindings>)>,
    binding_task: Option<BindingTask>,
    stamp_task: Option<StampTask>,
    stamp: Option<Stamp>,
    wanted: Option<StampKey>,
    stamp_source: Option<[String; 2]>,
    pub show_stamp: bool,
    cached_task: Option<CachedTask>,
    attempted_preparation: Option<PreparationRequest>,
    cached_stamps: std::collections::BTreeMap<String, Stamp>,
    failed_stamps: std::collections::BTreeSet<String>,
}
struct PreparedIdentities {
    root: PathBuf,
    catalog: String,
    data: Arc<mapper_studio::client_preparation::ReadyIdentities>,
    addon_check: std::cell::RefCell<Option<AddonCheck>>,
}
struct AddonCheck {
    addons: Vec<monument_addons::AddOn>,
    choices: monument_addons::Choices,
    custom: Vec<mapper_studio::custom_monuments::Placement>,
    options: mapper_studio::recipe_options::Options,
    map_size: u32,
    state: AddonCheckState,
}
type AddonResult = Result<(), monument_addons::OverlayIssue>;
enum AddonCheckState {
    Pending { receiver: Receiver<AddonResult>, previous_issue: Option<monument_addons::OverlayIssue> },
    Ready(Instant, AddonResult),
}

impl AddonCheck {
    fn new(project: &Project, state: AddonCheckState) -> Self {
        Self {
            addons: project.monument_addons.clone(),
            choices: project.monument_overlay_choices.clone(),
            custom: project.custom_monuments.clone(),
            options: project.options.clone(),
            map_size: project.size,
            state,
        }
    }

    fn matches(&self, project: &Project) -> bool {
        self.addons == project.monument_addons
            && self.choices == project.monument_overlay_choices
            && self.custom == project.custom_monuments
            && (self.custom.is_empty() || (self.map_size == project.size && self.options == project.options))
    }

    fn for_recipe(&mut self, project: &Project) -> Option<Result<(), AddonReadinessError>> {
        if let AddonCheckState::Pending { receiver, previous_issue } = &self.state {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    let status = if self.matches(project) {
                        previous_issue.clone().map_or(AddonReadinessError::Pending, AddonReadinessError::Invalid)
                    } else {
                        AddonReadinessError::Pending
                    };
                    return Some(Err(status));
                }
                Err(TryRecvError::Disconnected) => Err("Overlay validation worker stopped".to_owned().into()),
            };
            self.state = AddonCheckState::Ready(Instant::now(), result);
        }
        let AddonCheckState::Ready(checked, result) = &self.state else { unreachable!() };
        (self.matches(project)
            && (result.is_ok() || checked.elapsed() < Duration::from_secs(2)))
            .then(|| result.clone().map_err(AddonReadinessError::Invalid))
    }
}
impl PrefabPanel {
    pub fn preparation_token(&self) -> Option<&Arc<AtomicBool>> {
        self.cached_task.as_ref().map(|task| &task.cancel)
    }

    pub fn loading_assets(&self) -> bool {
        self.cached_task.is_some() || self.task.is_some() || self.binding_task.is_some()
    }

    pub fn take_prepared_generation(&mut self) -> Option<mapper_studio::client_preparation::Report> {
        self.prepared_generation.take()
    }
    fn identities_for(&self, project: &Project, root: &std::path::Path) -> Option<&PreparedIdentities> {
        let prepared = self.prepared_identities.as_ref()?;
        let catalog = self.accepted.as_ref()?;
        (prepared.root == root
            && prepared.catalog == catalog.sha256
            && project.installation.as_ref() == Some(&catalog.installation))
            .then_some(prepared)
    }

    pub fn prepared_identities(
        &self, project: &Project, root: &std::path::Path,
    ) -> Option<Arc<mapper_studio::client_preparation::ReadyIdentities>> {
        self.identities_for(project, root).map(|prepared| prepared.data.clone())
    }

    pub fn check_addons(&self, project: &Project, root: &std::path::Path, refresh: bool) -> Result<(), AddonReadinessError> {
        if !project.performance.monuments
            || (!project.has_monument_addons() && !mapper_studio::custom_monuments::has_enabled_placements(project))
        {
            return Ok(());
        }
        let prepared = self.identities_for(project, root).ok_or(AddonReadinessError::Unavailable)?;
        let mut cached = prepared.addon_check.borrow_mut();
        if !refresh {
            if let Some(result) = cached.as_mut().and_then(|check| check.for_recipe(project)) {
                return result;
            }
            let recipe = project.clone();
            let identities = Arc::clone(&prepared.data);
            let (sender, receiver) = mpsc::channel();
            let previous_issue = cached.as_ref().filter(|check| check.matches(project)).and_then(|check| {
                match &check.state {
                    AddonCheckState::Ready(_, Err(issue)) => Some(issue.clone()),
                    _ => None,
                }
            });
            let state = match std::thread::Builder::new().name("mapper-monument-check".into()).spawn(move || {
                let _ = sender.send(identities.check_monument_content(&recipe));
            }) {
                Ok(_) => AddonCheckState::Pending { receiver, previous_issue },
                Err(error) => AddonCheckState::Ready(Instant::now(),
                    Err(format!("Could not check custom monument content. {error}").into())),
            };
            let check = cached.insert(AddonCheck::new(project, state));
            return check.for_recipe(project).unwrap_or(Err(AddonReadinessError::Pending));
        }
        // Explicit actions check the library again in case files changed after
        // the last readiness result, before accepting the recipe into the queue.
        let result = prepared.data.check_monument_content(project);
        *cached = Some(AddonCheck::new(project, AddonCheckState::Ready(Instant::now(), result.clone())));
        result.map_err(AddonReadinessError::Invalid)
    }
    pub fn prepared_palette(
        &self,
        project: &Project,
        root: Option<&std::path::Path>,
    ) -> Option<Arc<mapper_studio::terrain_palette::Palette>> {
        let (prepared_root, palette) = self.prepared_palette.as_ref()?;
        (root == Some(prepared_root.as_path())
            && self.accepted.as_ref().is_some_and(|catalog| {
                project.installation.as_ref() == Some(&catalog.installation)
            }))
        .then(|| palette.clone())
    }
    fn set_stamp_source(&mut self, source: Option<[&str; 2]>) {
        if self.stamp_source.as_ref().map(|pair| [pair[0].as_str(), pair[1].as_str()]) != source {
            self.stamp_task = None;
            self.stamp = None;
            self.wanted = None;
            self.cached_stamps.clear();
            self.failed_stamps.clear();
            self.stamp_source = source.map(|pair| pair.map(str::to_owned));
        }
    }
    fn set_stamp_request(&mut self, key: Option<StampKey>) {
        self.stamp_task = None;
        if let Some(old) = self.stamp.take()
            && self
                .stamp_source
                .as_ref()
                .is_some_and(|s| old.key.catalog == s[0] && old.key.bindings == s[1])
        {
            if self.cached_stamps.len() >= 64 {
                self.cached_stamps.pop_first();
            }
            self.cached_stamps.insert(old.key.path.clone(), old);
        }
        self.stamp = key
            .as_ref()
            .and_then(|k| self.cached_stamps.remove(&k.path));
        self.wanted = key;
    }
    #[cfg(test)]
    pub fn seed_test_stamp_image(
        &mut self,
        ctx: &egui::Context,
        project: &mut Project,
        path: &str,
        image: egui::ColorImage,
    ) {
        self.seed_test_stamp(ctx, project, path);
        let stamp = self.stamp.as_mut().unwrap();
        stamp.image = image.clone();
        stamp.contextual_base = image.clone();
        stamp.texture.set(image, egui::TextureOptions::LINEAR);
    }
    #[cfg(test)]
    pub fn seed_test_biome_stamp(
        &mut self,
        ctx: &egui::Context,
        project: &mut Project,
        path: &str,
    ) {
        self.seed_test_stamp(ctx, project, path);
        let stamp = self.stamp.as_mut().unwrap();
        stamp.image.pixels.fill(egui::Color32::TRANSPARENT);
        stamp
            .texture
            .set(stamp.image.clone(), egui::TextureOptions::LINEAR);
        stamp.contextual_base = stamp.image.clone();
        stamp.contextual_biome = Some(egui::ColorImage::filled(
            [2, 2],
            egui::Color32::from_rgb(49, 101, 60),
        ));
        stamp.contextual_hit = Some(splat_preview::contextual_coverage(
            &stamp.contextual_base,
            stamp.contextual_biome.as_ref().unwrap(),
        ));
    }
    #[cfg(test)]
    pub fn seed_test_cartographic_stamp(&mut self,ctx:&egui::Context,project:&mut Project,path:&str,channel:Option<usize>)->Arc<mapper_studio::terrain_palette::Palette> {
        self.seed_test_stamp(ctx,project,path);
        self.seed_test_cartographic_weights(project, channel)
    }
    #[cfg(test)]
    pub fn seed_test_cartographic_weights(&mut self, project: &Project, channel: Option<usize>) -> Arc<mapper_studio::terrain_palette::Palette> {
        let stamp=self.stamp.as_mut().unwrap();
        stamp.contextual.borrow_mut().clear();
        stamp.geometry=splat_preview::Geometry{size:[300.,300.],offset:[0.,0.]};
        let mut material=[0;8];if let Some(c)=channel{material[c]=255;}
        stamp.material_weights=Some(splat_preview::WeightImage{size:[2,2],pixels:vec![material;4]});
        stamp.biome_weights=Some(splat_preview::WeightImage{size:[2,2],pixels:vec![[0,0,0,255,0];4]});
        stamp.palette_hit=Some(splat_preview::palette_coverage(stamp.material_weights.as_ref().unwrap(),stamp.biome_weights.as_ref()));
        palette_fixture(project.installation.as_ref().unwrap())
    }
    pub fn accept_prepared(
        &mut self,
        root: PathBuf,
        catalog: Catalog,
        bindings: Bindings,
        mut prepared: mapper_studio::client_preparation::Report,
    ) {
        let unavailable = prepared.steps.iter().filter(|s| s.error.is_some()).count();
        self.preparation_details = prepared.steps.iter().map(|step| {
            let label = format!("{} · {:.1}s", step.name, step.elapsed.as_secs_f64());
            step.error.as_ref().map_or(label.clone(), |error| {
                let reason = error.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or(error);
                format!("{label}\n{reason}")
            })
        }).collect::<Vec<_>>().join("\n");
        self.prepared_identities = prepared.identities.take().map(|data| PreparedIdentities {
            root: root.clone(),
            catalog: catalog.sha256.clone(),
            data,
            addon_check: Default::default(),
        });
        self.bindings = Some((catalog.sha256.clone(), Arc::new(bindings)));
        self.accepted = Some(Arc::new(catalog));
        self.prepared_palette = prepared
            .palette
            .take()
            .map(|palette| (root, palette));
        self.wanted = None;
        self.stamp = None;
        self.cached_stamps.clear();
        self.failed_stamps.clear();
        self.message = if unavailable == 0 { "Generation assets ready.".into() }
            else { format!("Generation data prepared. {unavailable} components are unavailable for this installation.") };
        self.prepared_generation = Some(prepared);
    }
    pub fn discover(&mut self, project: &Project, root: Option<PathBuf>, ctx: &egui::Context) {
        let (Some(root), Some(source)) = (root, project.installation.clone()) else {
            return;
        };
        if self.loading_assets() {
            return;
        }
        let request = PreparationRequest {
            source: source.clone(),
            root: root.clone(),
            pins: project.monument_assets.clone(),
        };
        self.attempted_preparation = Some(request.clone());
        let expected = source;
        let generation_project = project.clone();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let ctx = ctx.clone();
        let preparation_root = root.clone();
        std::thread::spawn(move || {
            let _loading = mapper_studio::loading_progress::begin("Preparing Rust installation", Some(signal.clone()));
            let result = mapper_studio::client_preparation::load_for_editor(
                &preparation_root, &expected, &generation_project, || {
                    signal.load(Ordering::Relaxed)
                },
            );
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.cached_task = Some(CachedTask {
            request,
            superseded: false,
            cancel,
            receiver,
        });
        self.message =
            "Preparing monument metadata and stamp textures. First-time setup can take several minutes…".into();
    }
    pub fn tick_all<'a>(
        &mut self,
        project: &Project,
        root: Option<PathBuf>,
        selected: Option<&'a str>,
        paths: impl IntoIterator<Item = &'a str>,
        ctx: &egui::Context,
    ) {
        // Across all prefabs this retains at most one variant per live target.
        for stamp in self.stamp.iter().chain(self.cached_stamps.values()) {
            stamp.prune_context(project);
        }
        let bound = root.is_some();
        if let Some(task) = &mut self.cached_task {
            if !task.request.matches(root.as_deref(), project) {
                task.superseded = true;
                task.cancel.store(true, Ordering::Relaxed);
            }
            let result = match task.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(_) => Some(Err("Asset verification worker stopped".into())),
            };
            if let Some(result) = result {
                let task = self.cached_task.take().unwrap();
                if task.superseded {
                    // A request may have become current again while its old worker was stopping.
                    self.attempted_preparation = None;
                }
                if !task.cancel.load(Ordering::Relaxed) {
                    match result {
                        Ok((catalog, bindings, prepared)) => {
                            self.accept_prepared(task.request.root.clone(), catalog, bindings, prepared);
                        }
                        Err(error) => self.message = error,
                    }
                } else {
                    self.message = "Asset verification cancelled or source changed.".into();
                }
            }
        }
        if let Some(root) = root
            && !self.loading_assets()
            && !self.attempted_preparation.as_ref().is_some_and(|request| request.matches(Some(&root), project))
            && (self.catalog_ref(project).is_none()
                || self.asset_pair(project).is_some_and(|(_, bindings)| bindings.installation_root() != root)
                || project.monument_assets.is_some() && !self.generation_assets_ready(project, &root))
        {
            self.discover(project, Some(root), ctx);
        }
        if !bound {
            self.cached_stamps.clear();
            self.failed_stamps.clear();
        }
        let mut requested = Vec::new();
        if let Some(path) = selected {
            requested.push(path);
        }
        for path in paths {
            if requested.len() >= 64 {
                break;
            }
            if !requested.contains(&path) {
                requested.push(path);
            }
        }
        let in_flight = self
            .stamp_task
            .as_ref()
            .map(|t| t.key.path.as_str())
            .and_then(|path| requested.iter().copied().find(|requested| *requested == path));
        let next = in_flight
            .or_else(|| {
                requested
                    .iter()
                    .copied()
                    .find(|p| self.stamp(project, p).is_none() && !self.failed_stamps.contains(*p))
            })
            .or(selected);
        self.tick(project, bound, next, ctx);
    }
    pub fn readiness(&mut self, ui: &mut egui::Ui, project: &Project, root: Option<PathBuf>) {
        ui.strong("Assets");
        let ready = root.as_deref().is_some_and(|root| self.generation_assets_ready(project, root));
        if ready && let Some(catalog) = self.catalog_ref(project) {
            ui.label(format!(
                "{} prefabs · textures ready",
                catalog.entries.len()
            )).on_hover_text("Terrain footprints use authored ground paint. Static models provide the 3D preview.");
        } else {
            ui.label(if self.cached_task.is_some() {
                "Preparing assets…"
            } else {
                "Assets unavailable"
            }).on_hover_text("Game assets load automatically from the selected installation and are cached for later sessions.");
        }
        if let Some(task) = &self.cached_task {
            ui.spinner();
            if ui.button("Cancel asset verification").clicked() {
                task.cancel.store(true, Ordering::Relaxed);
            }
        }
        if !ready && !self.message.is_empty() {
            diagnostic_ui::message(ui, &self.message, "installation-assets-error");
            diagnostic_ui::copy_error(ui, &self.message, None);
        }
        if !self.preparation_details.is_empty() || !self.failed_stamps.is_empty() || (ready && !self.message.is_empty()) {
            ui.collapsing("Preparation details", |ui| {
                if !self.preparation_details.is_empty() { diagnostic_ui::details(ui, &self.preparation_details); }
                if ready && !self.message.is_empty() { diagnostic_ui::message(ui, &self.message, "ready-assets-details"); }
                if !self.failed_stamps.is_empty() {
                    ui.small(format!("{} stamp previews unavailable", self.failed_stamps.len()))
                        .on_hover_text("Inspect the monument's asset metadata for its preview requirements.");
                }
            });
        }
    }
    #[cfg(test)]
    pub fn seed_test_verified_stamp(&mut self,ctx:&egui::Context,project:&mut Project)->tempfile::TempDir {
        let (directory,verified,panel,paths)=tests::verified_preview_fixture();*project=verified;*self=panel;
        let path=paths[0].clone();let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
        while self.stamp(project,&path).is_none(){self.tick_all(project,Some(directory.path().into()),Some(&path),[path.as_str()],ctx);assert!(std::time::Instant::now()<deadline,"Verified fixture stamp did not load");std::thread::sleep(std::time::Duration::from_millis(1));}
        directory
    }
    #[cfg(test)]
    pub fn seed_test_stamp(
        &mut self,
        ctx: &egui::Context,
        project: &mut Project,
        path: &str,
    ) -> egui::TextureId {
        use mapper_studio::installation::{InstallationKind, SourceFile};
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        };
        project.installation = Some(source.clone());
        self.accepted = Some(Arc::new(Catalog {
            sha256: "b".repeat(64),
            installation: source,
            entries: Default::default(),
            sources: Vec::new(),
        }));
        let key = StampKey {
            catalog: "b".repeat(64),
            bindings: "c".repeat(64),
            path: path.into(),
        };
        let texture = ctx.load_texture(
            "test-stamp",
            egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
            egui::TextureOptions::LINEAR,
        );
        let id = texture.id();
        if let Some(previous) = self.stamp.take() {
            self.cached_stamps
                .insert(previous.key.path.clone(), previous);
        }
        self.wanted = Some(key.clone());
        self.stamp = Some(Stamp {
            key,
            texture,
            image: egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
            contextual_base: egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
            material_weights: None,
            biome_weights: None,
            palette_hit: None,
            contextual_biome: None,
            contextual_hit: None,
            contextual: Default::default(),
            geometry: splat_preview::Geometry {
                size: [180., 120.],
                offset: [10., 20.],
            },
            default_yaw: 37.,
        });
        id
    }
    pub fn stamp(&self, project: &Project, path: &str) -> Option<&Stamp> {
        let catalog = self.catalog(project)?;
        self.stamp
            .as_ref()
            .filter(|s| s.key.path == path)
            .or_else(|| self.cached_stamps.get(path))
            .filter(|s| {
                s.key.path == path
                    && s.key.catalog == catalog.sha256
                    && self
                        .bindings(project)
                        .is_none_or(|b| s.key.bindings == b.sha256)
            })
    }
    pub fn tick(
        &mut self,
        project: &Project,
        bound: bool,
        path: Option<&str>,
        ctx: &egui::Context,
    ) {
        self.poll(project, bound);
        if let Some(task) = &self.binding_task {
            let matches = bound
                && self
                    .catalog(project)
                    .is_some_and(|c| c.sha256 == task.catalog)
                && project.installation.as_ref() == Some(&task.source);
            if !matches {
                task.cancel.store(true, Ordering::Relaxed);
            }
            let result = match task.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Texture verification worker stopped".into()))
                }
            };
            if let Some(result) = result {
                let task = self.binding_task.take().unwrap();
                if !matches || task.cancel.load(Ordering::Relaxed) {
                    self.message =
                        "Texture verification cancelled or source changed; result discarded."
                            .into();
                } else {
                    match result {
                        Ok(bindings) => {
                            self.message = format!(
                                "Verified {} exact texture bindings. Selected splat preview is prepared in the background.",
                                bindings.textures().len()
                            );
                            self.bindings = Some((task.catalog.clone(), Arc::new(bindings)));
                            self.wanted = None;
                        }
                        Err(e) => self.message = e,
                    }
                }
            }
        }
        let sources = bound.then(|| self.catalog(project).zip(self.bindings(project))).flatten();
        let stamp_source = sources.as_ref().map(|(catalog, bindings)| {
            [catalog.sha256.as_str(), bindings.sha256.as_str()]
        });
        self.set_stamp_source(stamp_source);
        let selected = sources.as_ref().zip(path).and_then(|((catalog, bindings), path)| {
            catalog.entries.get(path).map(|asset| (catalog, bindings, path, asset))
        });
        if self.wanted.as_ref().map(|key| key.path.as_str()) != selected.map(|(_, _, path, _)| path) {
            let key = selected.map(|(catalog, bindings, path, _)| {
                StampKey {
                    catalog: catalog.sha256.clone(),
                    bindings: bindings.sha256.clone(),
                    path: path.into(),
                }
            });
            self.set_stamp_request(key);
            if self.stamp.is_none()
                && let Some((_, bindings, _, asset)) = selected
                && let Some(key) = self.wanted.clone()
            {
                let bindings = bindings.clone();
                let asset = asset.clone();
                let (sender, receiver) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _loading = mapper_studio::loading_progress::begin("Monuments and terrain stamps", Some(signal.clone()));
                    let result =
                        splat_preview::load(&bindings, &asset, || signal.load(Ordering::Relaxed));
                    let _ = sender.send(result);
                    ctx.request_repaint();
                });
                self.stamp_task = Some(StampTask { key, cancel, receiver });
            }
        }
        if let Some(task) = &self.stamp_task {
            let result = match task.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Splat preview worker stopped".into())),
            };
            if let Some(result) = result {
                let task = self.stamp_task.take().unwrap();
                if self.wanted.as_ref() == Some(&task.key) && !task.cancel.load(Ordering::Relaxed) {
                    match result {
                        Ok(Some(preview)) => {
                            let contextual_base = splat_preview::contextual_base(&preview.image);
                            let contextual_biome =
                                preview.biome.as_ref().map(splat_preview::contextual_base);
                            let contextual_hit = contextual_biome
                                .as_ref()
                                .map(|b| splat_preview::contextual_coverage(&contextual_base, b));
                            self.stamp = Some(Stamp {
                                key: task.key.clone(),
                                geometry: preview.geometry,
                                default_yaw: preview.default_yaw,
                                image: preview.image.clone(),
                                contextual_base,
                                contextual_biome,
                                contextual_hit,
                                material_weights: preview.material_weights,
                                biome_weights: preview.biome_weights,
                                palette_hit: preview.palette_hit,
                                contextual: Default::default(),
                                texture: ctx.load_texture(
                                    "monument-splat-preview",
                                    preview.image,
                                    egui::TextureOptions::LINEAR,
                                ),
                            });
                            self.message = "Splat preview ready; source terrain unchanged.".into();
                        }
                        Ok(None) => self.message = "Splat preview cancelled.".into(),
                        Err(e) => {
                            self.failed_stamps.insert(task.key.path.clone());
                            self.message =
                                format!("Splat preview unavailable for {}: {e}", task.key.path);
                        }
                    }
                }
            }
        }
    }
    pub fn catalog(&self, project: &Project) -> Option<Arc<Catalog>> {
        self.catalog_ref(project).cloned()
    }
    #[cfg(test)]
    pub(super) fn seed_catalog(&mut self, catalog: Arc<Catalog>) {
        self.accepted = Some(catalog);
    }
    fn catalog_ref(&self, project: &Project) -> Option<&Arc<Catalog>> {
        self.accepted
            .as_ref()
            .filter(|c| project.installation.as_ref() == Some(&c.installation))
    }
    pub fn bindings(&self, project: &Project) -> Option<Arc<Bindings>> {
        self.asset_pair(project).map(|(_, bindings)| bindings.clone())
    }
    fn asset_pair(&self, project: &Project) -> Option<(&Catalog, &Arc<Bindings>)> {
        let catalog = self.catalog_ref(project)?;
        self.bindings
            .as_ref()
            .filter(|(hash, bindings)| {
                *hash == catalog.sha256 && bindings.catalog_sha256() == catalog.sha256
            })
            .map(|(_, bindings)| (catalog.as_ref(), bindings))
    }
    pub fn generation_assets_ready(&self, project: &Project, root: &Path) -> bool {
        self.asset_pair(project).is_some_and(|(catalog, bindings)| {
            bindings.installation_root() == root && project.monument_assets.as_ref().is_none_or(|pins| {
                pins.catalog_sha256 == catalog.sha256 && pins.bindings_sha256 == bindings.sha256
            })
        })
    }
    fn poll(&mut self, project: &Project, bound: bool) {
        let Some(task) = &self.task else {
            return;
        };
        if !bound || project.installation.as_ref() != Some(&task.source) {
            task.cancel.store(true, Ordering::Relaxed);
        }
        let result = match task.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Prefab verification worker stopped".into()),
        };
        let task = self.task.take().unwrap();
        if task.cancel.load(Ordering::Relaxed)
            || project.installation.as_ref() != Some(&task.source)
        {
            self.message =
                "Prefab verification cancelled or source changed; result discarded.".into();
            return;
        }
        match result {
            Ok(catalog) => {
                self.message = format!(
                    "Verified snapshot: {} prefab records. Load verified texture bindings for splat previews.",
                    catalog.entries.len()
                );
                self.accepted = Some(Arc::new(catalog));
                self.bindings = None;
                self.wanted = None;
                self.stamp = None;
            }
            Err(error) => self.message = error,
        }
    }
    pub fn details(&mut self, ui: &mut egui::Ui, project: &mut Project, selected: Option<&str>) {
        ui.collapsing("Monument details", |ui| {
            if let Some((catalog, bindings)) = self.catalog(project).zip(self.bindings(project)) {
                let identity = mapper_studio::AssetIdentity { catalog_sha256: catalog.sha256.clone(), bindings_sha256: bindings.sha256.clone() };
                if project.monument_assets.as_ref() == Some(&identity) {
                    ui.small("Monument asset fingerprints saved in recipe.");
                } else {
                    if project.monument_assets.is_some() { ui.label("Loaded assets differ from this recipe. Composition is blocked until you load matching assets or replace the fingerprints."); }
                    if ui.button(if project.monument_assets.is_some() { "Replace saved asset fingerprints" } else { "Remember these assets in recipe" }).clicked() { project.monument_assets = Some(identity); }
                }
            }
            if self.bindings.is_some() && self.catalog(project).is_some() {
                ui.checkbox(&mut self.show_stamp, "Show selected splat preview")
                    .on_hover_text(
                        "Also shown automatically during dragging. Terrain remains unchanged.",
                    );
                if self.stamp_task.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Preparing selected splat…");
                    });
                }
                if self.stamp.is_none()
                    && self.stamp_task.is_none()
                    && ui.button("Retry selected splat preview").clicked()
                {
                    self.wanted = None;
                }
            }
            let selected_catalog=self.catalog(project);
            if let Some(asset)=selected.and_then(|p|selected_catalog.as_ref()?.entries.get(p)) {
                ui.separator();ui.strong("Water-body inspection");
                if let Some(report)=&asset.water_geometry {
                    ui.label(format!("{} static nodes · {} water bodies · {} map contributors",report.nodes_visited,report.water_bodies.len(),report.contributors.len()));
                    ui.small(format!("{} initially active contributors have flat box geometry.",report.flat_box_contributors()));
                    if !report.complete {
                        ui.label("Static hierarchy inspection is incomplete.");
                        ui.collapsing(format!("Inspection issues ({})",report.issues.len()),|ui|{for issue in &report.issues {ui.label(&issue.reason);}});
                    }
                    ui.small("Inspection only. Runtime-spawned water and local-water rasterization are pending.");
                } else {ui.small("Water geometry is uninspected. Rebuild the catalog with the current extractor to inspect it.");}
            }
            if !self.message.is_empty() {
                ui.small(&self.message);
            }

        });
    }
}

pub(super) enum AddonReadinessError {
    Unavailable,
    Pending,
    Invalid(mapper_studio::monument_addons::OverlayIssue),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mapper_studio::installation::{InstallationKind, SourceFile};

    #[test]
    fn overlay_readiness_waits_without_using_another_recipes_result() {
        let mut recipe = Project::default();
        recipe.monument_overlay_choices.insert(12, "a".repeat(64));
        let (sender, receiver) = mpsc::channel();
        let mut check = AddonCheck::new(&recipe, AddonCheckState::Pending { receiver, previous_issue: None });
        let mut changed = recipe.clone();
        changed.monument_overlay_choices.insert(12, "b".repeat(64));
        assert!(matches!(
            check.for_recipe(&changed), Some(Err(AddonReadinessError::Pending))
        ));

        let issue = monument_addons::OverlayIssue {
            anchor: Some(12),
            message: "Missing overlay".into(),
        };
        sender.send(Err(issue.clone())).unwrap();
        assert!(check.for_recipe(&changed).is_none());
        assert!(matches!(
            check.for_recipe(&recipe),
            Some(Err(AddonReadinessError::Invalid(error))) if error.anchor == issue.anchor && error.message == issue.message
        ));
        check.state = AddonCheckState::Ready(Instant::now() - Duration::from_secs(2), Err(issue));
        assert!(check.for_recipe(&recipe).is_none());
    }

    #[test]
    fn preview_palette_is_shared_only_with_its_prepared_installation() {
        use std::path::Path;
        let ctx = egui::Context::default();
        let mut panel = PrefabPanel::default();
        let mut project = Project::default();
        panel.seed_test_stamp(&ctx, &mut project, "assets/test.prefab");
        let palette = palette_fixture(project.installation.as_ref().unwrap());
        let root = Path::new("Regular Rust");
        panel.prepared_palette = Some((root.to_owned(), palette.clone()));
        assert!(Arc::ptr_eq(&panel.prepared_palette(&project, Some(root)).unwrap(), &palette));
        assert!(panel.prepared_palette(&project, Some(Path::new("Staging"))).is_none());
        assert!(panel.prepared_palette(&project, None).is_none());
        project.installation.as_mut().unwrap().native.sha256 = "b".repeat(64);
        assert!(panel.prepared_palette(&project, Some(root)).is_none());
    }
    fn flat_cartographic_layers()->Arc<papyrus_terrain::native_layers::Layers> {
        let mut splat=vec![0;32];splat[16..20].fill(255);let mut biome=vec![0;20];biome[4..8].fill(255);
        Arc::new(papyrus_terrain::native_layers::Layers{size:1000,terrain_seed:1,biome_seed:2,height_resolution:2,surface_resolution:2,height:vec![18000;4],biome,splat,topology:vec![0;4],normalized_splat_cells:0,normalized_biome_cells:0})
    }
    #[test]
    fn cartographic_baked_paint_uses_visible_final_colors_without_requiring_an_overlay_cache() {
        let ctx=egui::Context::default();let mut panel=PrefabPanel::default();let mut project=Project::default();let path=project.targets[0].prefab_path.clone().unwrap();
        let palette=panel.seed_test_cartographic_stamp(&ctx,&mut project,&path,Some(0));let base=flat_cartographic_layers();
        let stamp=panel.stamp(&project,&path).unwrap();assert!(stamp.contextual.borrow().is_empty());
        for changed in [false,true] {
            let mut layers=base.as_ref().clone();if changed {layers.splat.fill(0);layers.splat[..4].fill(255);}
            let maps=Arc::new(papyrus_terrain::monument_composition::Composed{custom_baked:Vec::new(),layers,alpha:vec![255;4],rivers:None,rails:None,gameplay_height:None,objects:None,dungeons:None,tunnels:None,offshore:None,clutter_topology:None,cliff_topology:None,topology_before_overrides:None,topology_overrides:None,spawn_audit:None,decoration:None,roads:None,clamped_height_samples:0,ocean:None});
            let context=StampContext {base_surface:None,composed_surface:None,base:Some(&base),composed:Some(&maps),palette:Some(&palette)};
            assert_eq!(stamp.hit_test_displayed([80.,0.],[0.,0.,37.],true,context,(1,false),true),changed);
            assert!(!stamp.hit_test_displayed([300.,0.],[0.,0.,37.],true,context,(1,false),true));
        }
        // An unrelated road can change the visible ground within a biome-only
        // monument's bounds. It must not make that invisible biome paint pickable.
        let bio_palette=panel.seed_test_cartographic_stamp(&ctx,&mut project,&path,None);
        let bio=panel.stamp(&project,&path).unwrap();let mut layers=base.as_ref().clone();layers.splat.fill(0);layers.splat[..4].fill(255);
        let maps=Arc::new(papyrus_terrain::monument_composition::Composed{custom_baked:Vec::new(),layers,alpha:vec![255;4],rivers:None,rails:None,gameplay_height:None,objects:None,dungeons:None,tunnels:None,offshore:None,clutter_topology:None,cliff_topology:None,topology_before_overrides:None,topology_overrides:None,spawn_audit:None,decoration:None,roads:None,clamped_height_samples:0,ocean:None});
        assert!(!bio.hit_test_displayed([80.,0.],[0.,0.,37.],true,StampContext {base_surface:None,composed_surface:None,base:Some(&base),composed:Some(&maps),palette:Some(&bio_palette)},(1,false),true));
    }
    #[test]
    fn cartographic_overlay_masks_follow_the_displayed_pose_palette_and_instance() {
        let ctx=egui::Context::default();let mut panel=PrefabPanel::default();let mut project=Project::default();let path=project.targets[0].prefab_path.clone().unwrap();
        let palette=panel.seed_test_cartographic_stamp(&ctx,&mut project,&path,Some(0));let base=flat_cartographic_layers();
        let stamp=panel.stamp(&project,&path).unwrap();let context=StampContext {base_surface:None,composed_surface:None,base:Some(&base),composed:None,palette:Some(&palette)};let pose=[0.,0.,37.];
        assert!(!stamp.hit_test_displayed([80.,0.],pose,true,context,(1,true),false));
        stamp.texture_at_background(&ctx,context,(1,true),pose,true);
        assert!(stamp.hit_test_displayed([80.,0.],pose,true,context,(1,true),false));
        assert!(!stamp.hit_test_displayed([80.,0.],pose,true,context,(2,true),false));
        assert!(!stamp.hit_test_displayed([80.,0.],[1.,0.,37.],true,context,(1,true),false));
        let mut prefs=mapper_studio::map_colors::Preferences{enabled:true,..Default::default()};prefs.overrides[0]=Some([32,64,96]);
        let custom=Arc::new(palette.with_display_colors(&prefs));let changed=StampContext{palette:Some(&custom),..context};
        assert!(!stamp.hit_test_displayed([80.,0.],pose,true,changed,(1,true),false));
        stamp.texture_at_background(&ctx,changed,(1,true),pose,true);
        assert!(stamp.hit_test_displayed([80.,0.],pose,true,changed,(1,true),false));
        assert_eq!(Arc::strong_count(&base),1);assert_eq!(Arc::strong_count(&custom),1);
    }
    #[test]
    fn water_hidden_stamp_paint_cannot_capture_clicks_and_rekeys_with_the_map() {
        let ctx = egui::Context::default();
        let mut panel = PrefabPanel::default();
        let mut project = Project::default();
        let path = project.targets[0].prefab_path.clone().unwrap();
        let palette = panel.seed_test_cartographic_stamp(&ctx, &mut project, &path, Some(0));
        let base = flat_cartographic_layers();
        let stamp = panel.stamp(&project, &path).unwrap();
        for water in [true, false, true] {
            let ocean = water.then(|| papyrus_terrain::ocean::Ocean {
                resolution: 2,
                classification: vec![papyrus_terrain::ocean::LAND; 4],
                topology: vec![0; 4],
                local_water: vec![((200_f32 + 500.) / 1000. * 32766.).round() as i16; 4],
                ocean_cells: 0,
                enclosed_cells: 0,
                changed_topology_cells: 0,
            });
            let maps = Arc::new(papyrus_terrain::monument_composition::Composed {
                custom_baked: Vec::new(),
                layers: base.as_ref().clone(), alpha: vec![255; 4], rivers: None, rails: None,
                gameplay_height: None, objects: None, dungeons: None, tunnels: None, offshore: None, clutter_topology: None, cliff_topology: None, topology_before_overrides: None,
                topology_overrides: None, spawn_audit: None, decoration: None, roads: None,
                clamped_height_samples: 0, ocean,
            });
            let context = StampContext {base_surface: None, composed_surface: None,
                base: Some(&base), composed: Some(&maps), palette: Some(&palette)};
            let pose = [0., 0., 37.];
            stamp.texture_at_background(&ctx, context, (1, true), pose, true);
            assert_eq!(stamp.hit_test_displayed([80., 0.], pose, true, context, (1, true), false), !water);
            if water {
                assert!(stamp.contextual.borrow().get(&(1, true)).unwrap()
                    .cartographic_hit.as_ref().unwrap().1.iter().all(|alpha| *alpha == 0));
            }
        }
        assert_eq!(Arc::strong_count(&base), 1);
    }

    pub(super) fn verified_preview_fixture() -> (tempfile::TempDir, Project, PrefabPanel, Vec<String>) {
        use mapper_studio::prefab_assets::{Asset, Modifier};
        use sha2::{Digest, Sha256};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut sources = Vec::new();
        for path in [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "Bundles/shared/content.bundle",
            "Bundles/shared/monuments.bundle",
            "Bundles/maps/maps.bundle",
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            let mut pe = [0; 70];
            let bytes = if path.ends_with("RustNative.dll") {
                pe[..2].copy_from_slice(b"MZ");
                pe[60..64].copy_from_slice(&64_u32.to_le_bytes());
                pe[64..].copy_from_slice(b"PE\0\0\x64\x86");
                &pe[..]
            } else {
                path.as_bytes()
            };
            std::fs::write(root.join(path), bytes).unwrap();
            sources.push(SourceFile {
                relative_path: path.into(),
                sha256: format!("{:x}", Sha256::digest(bytes)),
                bytes: bytes.len() as u64,
            });
        }
        let mut project = Project {
            installation: Some(Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            }),
            ..Default::default()
        };
        let guid = "a".repeat(32);
        let pixels = [255u8, 0, 0, 255];
        let paths: Vec<String> = project
            .targets
            .iter()
            .filter_map(|t| t.prefab_path.clone())
            .collect();
        let asset = Asset {
            priority: None,
            bounds: None,
            yaw: None,
            modifier_size: Some([100.; 3]),
            modifier_offset: Some([0.; 3]),
            texture_guids: [
                ("splat0".into(), guid.clone()),
                ("splat1".into(), guid.clone()),
            ]
            .into(),
            splat_mask: Some(1),
            modifier_absent: false,
            modifier: Some(Modifier {
                height_map: false,
                water_map: None,
                alpha_map: false,
                biome_mask: 0,
                topology_mask: 0,
                radius: 50.,
                fade: 0.,
                auto_cliff_splat: false,
                auto_cliff_topology: false,
                remove_existing_topology: false,
            }),
            water_geometry: None,
            no_build_geometry: None,
            display_name: None,
        };
        let catalog = Catalog {
            sha256: "b".repeat(64),
            installation: project.installation.clone().unwrap(),
            sources: sources[..3].to_vec(),
            entries: paths.iter().map(|p| (p.clone(), asset.clone())).collect(),
        };
        std::fs::create_dir(root.join("textures")).unwrap();
        std::fs::write(root.join(format!("textures/{guid}.rgba")), pixels).unwrap();
        let document = serde_json::json!({"format":"papyrus-prefab-textures","schema":1,"adapter":"papyrus-guid-rgba-1","catalog_sha256":catalog.sha256,"sources":sources,
            "pixel_format":"RGBA8-top-left","decoded_bytes":4,"textures":{guid.clone():{"asset_path":"assets/monument/blend.png","bundle_relative_path":"Bundles/shared/monuments.bundle","rgba_path":format!("textures/{guid}.rgba"),"width":1,"height":1,"bytes":4,"sha256":format!("{:x}",Sha256::digest(pixels))}}});
        let receipt = root.join("bindings.json");
        std::fs::write(&receipt, serde_json::to_vec(&document).unwrap()).unwrap();
        let bindings = Bindings::load(root, &receipt, &catalog, || false).unwrap();
        project.monument_assets = Some(mapper_studio::AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: bindings.sha256.clone(),
        });
        let panel = PrefabPanel {
            bindings: Some((catalog.sha256.clone(), Arc::new(bindings))),
            accepted: Some(Arc::new(catalog)),
            ..Default::default()
        };
        (directory, project, panel, paths)
    }
    #[test]
    fn idle_and_deselection_keep_all_verified_stamps_without_reload_or_texture_churn() {
        let (directory, project, mut panel, paths) = verified_preview_fixture();
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            panel.tick_all(&project, Some(directory.path().into()), None, paths.iter().map(String::as_str), &ctx);
            ctx.tex_manager().write().take_delta().clear();
            if paths.iter().all(|p| panel.stamp(&project, p).is_some()) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Preview preparation did not finish: {}",
                panel.message
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let ids: Vec<_> = paths
            .iter()
            .map(|p| panel.stamp(&project, p).unwrap().texture.id())
            .collect();
        for selected in std::iter::repeat_n(None, 12)
            .chain(paths.iter().map(|p| Some(p.as_str())))
            .chain(std::iter::repeat_n(None, 12))
        {
            panel.tick_all(
                &project,
                Some(directory.path().into()),
                selected,
                paths.iter().map(String::as_str),
                &ctx,
            );
            assert!(
                panel.stamp_task.is_none(),
                "Ready previews started loading again"
            );
            assert_eq!(
                paths
                    .iter()
                    .map(|p| panel.stamp(&project, p).map(|s| s.texture.id()))
                    .collect::<Vec<_>>(),
                ids.iter().copied().map(Some).collect::<Vec<_>>()
            );
            let mut delta = ctx.tex_manager().write().take_delta();
            assert!(delta.set.is_empty());
            assert!(delta.free.is_empty());
            delta.clear();
        }
        panel.tick_all(&project, None, None, paths.iter().map(String::as_str), &ctx);
        assert!(
            panel.stamp.is_none() && panel.cached_stamps.is_empty() && panel.stamp_task.is_none()
        );
    }
    #[test]
    fn idle_keeps_unsupported_preview_failures_in_the_current_source_epoch() {
        let (directory, project, mut panel, paths) = verified_preview_fixture();
        let ctx = egui::Context::default();
        Arc::get_mut(panel.accepted.as_mut().unwrap())
            .unwrap()
            .entries
            .get_mut(&paths[2])
            .unwrap()
            .modifier_size = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            panel.tick_all(&project, Some(directory.path().into()), None, paths.iter().map(String::as_str), &ctx);
            ctx.tex_manager().write().take_delta().clear();
            if paths[..2]
                .iter()
                .all(|p| panel.stamp(&project, p).is_some())
                && panel.failed_stamps.contains(&paths[2])
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{}", panel.message);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        for _ in 0..16 {
            panel.tick_all(&project, Some(directory.path().into()), None, paths.iter().map(String::as_str), &ctx);
            assert!(panel.stamp_task.is_none());
            assert!(panel.failed_stamps.contains(&paths[2]));
            assert!(
                paths[..2]
                    .iter()
                    .all(|p| panel.stamp(&project, p).is_some())
            );
        }
    }
    #[test]
    fn changing_verified_bindings_discards_queued_pixels_and_old_cache() {
        let (directory, mut project, mut panel, paths) = verified_preview_fixture();
        let ctx = egui::Context::default();
        let identity = [
            panel.accepted.as_ref().unwrap().sha256.clone(),
            panel.bindings.as_ref().unwrap().1.sha256.clone(),
        ];
        let catalog = panel.accepted.clone().unwrap();
        let installation = project.installation.clone();
        panel.set_stamp_source(Some([identity[0].as_str(), identity[1].as_str()]));
        panel.seed_test_stamp(&ctx, &mut project, &paths[0]);
        panel.accepted = Some(catalog);
        project.installation = installation;
        let receipt = directory.path().join("bindings.json");
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        let stamp = panel.stamp.as_mut().unwrap();
        stamp.key.bindings = identity[1].clone();
        let old_key = stamp.key.clone();
        panel.wanted = Some(old_key.clone());
        let old_texture = stamp.texture.id();
        panel.failed_stamps.insert("unavailable.prefab".into());
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        panel.stamp_task = Some(StampTask {
            key: old_key,
            cancel: cancel.clone(),
            receiver,
        });
        sender
            .send(Ok(Some(splat_preview::Preview {
                image: egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
                material_weights: None,
                biome_weights: None,
                palette_hit: None,
                biome: None,
                geometry: splat_preview::Geometry {
                    size: [10.; 2],
                    offset: [0.; 2],
                },
                default_yaw: 0.,
            })))
            .unwrap();
        ctx.tex_manager().write().take_delta().clear();
        let mut next = document;
        next["textures"]["a".repeat(32)]["asset_path"] = "assets/monument/renamed.png".into();
        let next_path = directory.path().join("bindings-next.json");
        std::fs::write(&next_path, serde_json::to_vec(&next).unwrap()).unwrap();
        let bindings = Bindings::load(
            directory.path(),
            &next_path,
            panel.accepted.as_ref().unwrap(),
            || false,
        )
        .unwrap();
        let next_identity = [identity[0].clone(), bindings.sha256.clone()];
        panel.bindings = Some((identity[0].clone(), Arc::new(bindings)));
        panel.tick_all(&project, Some(directory.path().into()), None, [], &ctx);
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(panel.stamp_source, Some(next_identity));
        assert!(
            panel.stamp.is_none()
                && panel.cached_stamps.is_empty()
                && panel.failed_stamps.is_empty()
                && panel.stamp_task.is_none()
        );
        let mut delta = ctx.tex_manager().write().take_delta();
        assert!(delta.set.is_empty());
        assert!(delta.free.contains(&old_texture));
        delta.clear();
    }
    #[test]
    #[ignore = "Requires an owned installation and matching local texture caches"]
    fn native_ready_stamps_survive_idle_selection_changes_and_release_on_unbind() {
        use std::time::{Duration, Instant};
        let path =
            |key| PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("Set {key}")));
        let root = path("MAPPER_TEST_RUST_ROOT");
        let project = Project::load(&path("MAPPER_TEST_COASTAL_RECIPE")).unwrap();
        let before = project.clone();
        let catalog = mapper_studio::prefab_assets::load(
            &root,
            &path("MAPPER_TEST_CATALOG"),
            project.installation.as_ref().unwrap(),
            || false,
        )
        .unwrap();
        let bindings =
            Bindings::load(&root, &path("MAPPER_TEST_BINDINGS"), &catalog, || false).unwrap();
        project
            .check_monument_assets(&catalog.sha256, &bindings.sha256)
            .unwrap();
        let mut panel = PrefabPanel {
            bindings: Some((catalog.sha256.clone(), Arc::new(bindings))),
            accepted: Some(Arc::new(catalog)),
            ..Default::default()
        };
        let paths: Vec<_> = project
            .targets
            .iter()
            .filter(|t| t.presence != mapper_studio::Presence::Excluded)
            .filter_map(|t| t.prefab_path.clone())
            .collect();
        assert!(
            !paths.is_empty() && paths.len() <= 16,
            "Use a small supported native fixture"
        );
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            panel.tick_all(&project, Some(root.clone()), None, paths.iter().map(String::as_str), &ctx);
            ctx.tex_manager().write().take_delta().clear();
            if paths.iter().all(|p| panel.stamp(&project, p).is_some()) {
                break;
            }
            assert!(panel.failed_stamps.is_empty(), "{}", panel.message);
            assert!(Instant::now() < deadline, "{}", panel.message);
            std::thread::sleep(Duration::from_millis(5));
        }
        let textures: Vec<_> = paths
            .iter()
            .map(|p| panel.stamp(&project, p).unwrap().texture.id())
            .collect();
        let mut idle_frames = 0;
        for selected in std::iter::repeat_n(None, 24)
            .chain(paths.iter().map(|p| Some(p.as_str())))
            .chain(std::iter::repeat_n(None, 24))
        {
            panel.tick_all(&project, Some(root.clone()), selected, paths.iter().map(String::as_str), &ctx);
            assert!(panel.stamp_task.is_none());
            assert_eq!(
                paths
                    .iter()
                    .map(|p| panel.stamp(&project, p).map(|s| s.texture.id()))
                    .collect::<Vec<_>>(),
                textures.iter().copied().map(Some).collect::<Vec<_>>()
            );
            let mut delta = ctx.tex_manager().write().take_delta();
            assert!(delta.set.is_empty());
            assert!(delta.free.is_empty());
            delta.clear();
            if selected.is_none() {
                idle_frames += 1;
            }
        }
        panel.tick_all(&project, None, None, paths.iter().map(String::as_str), &ctx);
        assert!(
            panel.stamp.is_none() && panel.cached_stamps.is_empty() && panel.stamp_task.is_none()
        );
        let mut delta = ctx.tex_manager().write().take_delta();
        assert!(textures.iter().all(|id| delta.free.contains(id)));
        delta.clear();
        assert_eq!(project, before);
        let report = serde_json::json!({"format":"mapper-native-stamp-idle-cache-verification","schema":1,
            "prefabs":paths,"textures":textures.iter().map(|id|format!("{id:?}")).collect::<Vec<_>>(),
            "idle_frames":idle_frames,"selection_cycles":textures.len(),"texture_allocations_unchanged":true,
            "no_idle_preparation_jobs":true,"no_idle_texture_uploads_or_frees":true,"unbind_releases_all_previews":true,
            "recipe_unchanged":true,"desktop_windows_opened":0,"gpu_initialized":false});
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path("MAPPER_TEST_REPORT"))
            .unwrap();
        serde_json::to_writer_pretty(file, &report).unwrap();
    }
    #[test]
    fn picking_uses_the_alpha_mask_of_the_displayed_stamp_lod() {
        let ctx = egui::Context::default();
        let mut panel = PrefabPanel::default();
        let mut project = Project::default();
        let path = project.targets[0].prefab_path.clone().unwrap();
        panel.seed_test_stamp(&ctx, &mut project, &path);
        let stamp = panel.stamp.as_mut().unwrap();
        stamp.geometry = splat_preview::Geometry {
            size: [512., 128.],
            offset: [0., 0.],
        };
        stamp.image = egui::ColorImage::filled([512, 2], egui::Color32::TRANSPARENT);
        for y in 0..2 {
            stamp.image.pixels[y * 512 + 1] = egui::Color32::WHITE;
        }
        stamp.contextual_base = splat_preview::contextual_base(&stamp.image);
        // The reduced first texel contains half-coverage from the thin strip.
        // Its displayed extent includes a point transparent in the full image.
        assert!(!stamp.hit_test([-255.5, 0.], [0., 0., 0.], false));
        assert!(stamp.hit_test([-255.5, 0.], [0., 0., 0.], true));
    }
    #[test]
    fn changed_palette_refreshes_drag_texture_without_retaining_old_palette() {
        let ctx = egui::Context::default();
        let mut panel = PrefabPanel::default();
        let mut project = Project::default();
        let path = project.targets[0].prefab_path.clone().unwrap();
        panel.seed_test_stamp(&ctx, &mut project, &path);
        let stamp = panel.stamp.as_mut().unwrap();
        stamp.material_weights = Some(splat_preview::WeightImage {
            size: [1, 1],
            pixels: vec![[128, 0, 0, 0, 0, 0, 0, 0]],
        });
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        };
        let make_palette = |color: f32| {
            let doc = serde_json::json!({"format":"papyrus-terrain-palette","schema":1,"adapter":"papyrus-terrain-palette-1","extractor_sha256":"b".repeat(64),
                "sources":[source.native,{"relative_path":"Bundles/maps/maps.bundle","sha256":"c".repeat(64),"bytes":1},{"relative_path":"Bundles/shared/content.bundle","sha256":"d".repeat(64),"bytes":1}],
                "scene":"BuildPlayer-Procedural Map","binding":{"scene_file":"BuildPlayer-Procedural Map","world_setup":1,"terrain":2,"generator":3,"config_file":"CAB-config","config_object":4},
                "config_paths":[],"numeric_space":"unity-serialized-color","biomes":["Arid","Temperate","Tundra","Arctic","Jungle"],
                "materials":(0..8).map(|i|serde_json::json!({"name":format!("Material {i}"),"biome_colors":vec![[color,0.3,0.4,1.];5]})).collect::<Vec<_>>()});
            Arc::new(
                mapper_studio::terrain_palette::Palette::decode(
                    &serde_json::to_vec(&doc).unwrap(),
                    &source,
                )
                .unwrap(),
            )
        };
        let layers = Arc::new(papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 2,
            surface_resolution: 2,
            height: vec![18000; 4],
            biome: [vec![255; 4], vec![0; 16]].concat(),
            splat: [vec![255; 4], vec![0; 28]].concat(),
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        let first = make_palette(0.2);
        let handle = stamp.texture_at_source(
            &ctx,
            (Some(&layers), Some(&first)),
            (1, false),
            [0., 0., 37.],
            true,
        );
        let weak = Arc::downgrade(&first);
        ctx.tex_manager().write().take_delta().clear();
        let second = make_palette(0.8);
        assert_eq!(
            handle,
            stamp.texture_at_source(
                &ctx,
                (Some(&layers), Some(&second)),
                (1, false),
                [0., 0., 37.],
                true
            )
        );
        let mut delta = ctx.tex_manager().write().take_delta();
        assert!(delta.set.iter().any(|(texture, _)| *texture == handle));
        delta.clear();
        assert_eq!(Arc::strong_count(&second), 1);
        drop(first);
        assert!(weak.upgrade().is_none());
        assert!(Weak::ptr_eq(
            stamp.contextual.borrow()[&(1, false)]
                .palette
                .as_ref()
                .unwrap(),
            &Arc::downgrade(&second)
        ));
    }
    #[test]
    fn composed_stamp_context_recolors_in_place_and_does_not_retain_map_grids() {
        fn uploaded(ctx: &egui::Context, id: egui::TextureId) -> Vec<egui::Color32> {
            let mut delta = ctx.tex_manager().write().take_delta();
            let pixels = delta
                .set
                .iter()
                .find_map(|(key, images)| {
                    if *key != id {
                        return None;
                    }
                    let egui::ImageData::Color(image) = &images.last()?.image;
                    Some(image.pixels.clone())
                })
                .expect("A changed terrain background must update stamp pixels");
            delta.clear();
            pixels
        }
        let ctx = egui::Context::default();
        let mut panel = PrefabPanel::default();
        let mut project = Project::default();
        let path = project.targets[0].prefab_path.clone().unwrap();
        panel.seed_test_stamp(&ctx, &mut project, &path);
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let base = Arc::new(papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: [vec![255; cells], vec![0; cells * 4]].concat(),
            splat: [vec![255; cells], vec![0; cells * 7]].concat(),
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        let stamp = panel.stamp(&project, &path).unwrap();
        let key = (project.targets[0].instance_id, true);
        let context = StampContext {base_surface:None,composed_surface:None,
            base: Some(&base),
            composed: None,
            palette: None,
        };
        let handle = stamp.texture_at_background(&ctx, context, key, [0., 0., 37.], true);
        let base_pixels = uploaded(&ctx, handle);
        let make_composed = |biome: usize| {
            let mut maps =
                papyrus_terrain::monument_composition::compose(&base, &vec![255; cells], &[], || false)
                    .unwrap()
                    .unwrap();
            maps.layers.biome.fill(0);
            maps.layers.biome[biome * cells..(biome + 1) * cells].fill(255);
            Arc::new(maps)
        };
        let first = make_composed(3);
        let composed = StampContext {
            composed: Some(&first),
            ..context
        };
        assert_eq!(
            handle,
            stamp.texture_at_background(&ctx, composed, key, [0., 0., 37.], true)
        );
        let first_pixels = uploaded(&ctx, handle);
        assert_ne!(base_pixels, first_pixels);
        assert_eq!(Arc::strong_count(&first), 1);
        assert_eq!(Arc::strong_count(&base), 1);
        stamp.texture_at_background(&ctx, composed, key, [0., 0., 37.], true);
        let mut delta = ctx.tex_manager().write().take_delta();
        assert!(delta.set.is_empty());
        delta.clear();
        let weak = Arc::downgrade(&first);
        drop(first);
        assert!(weak.upgrade().is_none());
        let second = make_composed(4);
        assert_eq!(
            handle,
            stamp.texture_at_background(
                &ctx,
                StampContext {
                    composed: Some(&second),
                    ..context
                },
                key,
                [0., 0., 37.],
                true
            )
        );
        assert_ne!(uploaded(&ctx, handle), first_pixels);
        assert_eq!(
            handle,
            stamp.texture_at_background(&ctx, context, key, [0., 0., 37.], true)
        );
        assert_eq!(uploaded(&ctx, handle), base_pixels);
        assert!(stamp.contextual.borrow()[&key].composed.is_none());
        assert_eq!(Arc::strong_count(&second), 1);
    }
    #[test]
    fn duplicate_prefabs_keep_distinct_contextual_textures_and_prune_removed_targets() {
        let ctx = egui::Context::default();
        let mut panel = PrefabPanel::default();
        let mut project = Project::default();
        let path = project.targets[0].prefab_path.clone().unwrap();
        panel.seed_test_stamp(&ctx, &mut project, &path);
        let id = project.targets[0].instance_id;
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut layers = Arc::new(papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        let stamp = panel.stamp(&project, &path).unwrap();
        let first = stamp.texture_at(&ctx, Some(&layers), (id, false), [0., 0., 37.], true);
        let other = stamp.texture_at(&ctx, Some(&layers), (999, false), [-200., 0., 123.], true);
        assert_ne!(first, other);
        assert_eq!(
            first,
            stamp.texture_at(&ctx, Some(&layers), (id, false), [10., 20., 40.], true)
        );
        assert_eq!(
            other,
            stamp.texture_at(&ctx, Some(&layers), (999, false), [-200., 0., 123.], true)
        );
        stamp.prune_context(&project);
        assert!(stamp.contextual.borrow().contains_key(&(id, false)));
        assert!(!stamp.contextual.borrow().contains_key(&(999, false)));
        let old_source = stamp.contextual.borrow()[&(id, false)].source.clone();
        assert_eq!(Arc::strong_count(&layers), 1);
        // A changed immutable terrain must recolor even with identical seeds,
        // pose and texture handle. The old grid must not be held by the cache.
        ctx.tex_manager().write().take_delta().clear();
        let changed = Arc::make_mut(&mut layers);
        changed.biome.fill(0);
        changed.biome[3 * sr * sr..4 * sr * sr].fill(255);
        assert!(old_source.upgrade().is_none());
        assert_eq!(
            first,
            stamp.texture_at(&ctx, Some(&layers), (id, false), [10., 20., 40.], true)
        );
        let mut delta = ctx.tex_manager().write().take_delta();
        assert!(delta.set.iter().any(|(texture, _)| *texture == first));
        delta.clear();
        assert!(Weak::ptr_eq(
            &stamp.contextual.borrow()[&(id, false)].source,
            &Arc::downgrade(&layers)
        ));
        drop(layers);
        assert!(
            stamp.contextual.borrow()[&(id, false)]
                .source
                .upgrade()
                .is_none()
        );
    }
    #[test]
    fn changed_source_or_cancelled_verification_cannot_publish() {
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        };
        for cancelled in [false, true] {
            let mut project = Project::default();
            if cancelled {
                project.installation = Some(source.clone());
            }
            let (sender, receiver) = mpsc::channel();
            let catalog = Catalog {
                sha256: "b".repeat(64),
                installation: source.clone(),
                entries: Default::default(),
                sources: Vec::new(),
            };
            let mut panel = PrefabPanel {
                task: Some(Task {
                    source: source.clone(),
                    cancel: Arc::new(AtomicBool::new(cancelled)),
                    receiver,
                }),
                ..Default::default()
            };
            assert!(sender.send(Ok(catalog)).is_ok());
            panel.poll(&project, true);
            assert!(panel.accepted.is_none());
            assert!(panel.message.contains("discarded"));
        }
    }
    #[test]
    fn preparation_restarts_for_changed_pins_or_a_restored_request() {
        let ctx = egui::Context::default();
        for restore_original in [false, true] {
            let (directory, original, mut panel, _) = verified_preview_fixture();
            let root = directory.path().to_path_buf();
            assert!(panel.generation_assets_ready(&original, &root));
            assert!(!panel.generation_assets_ready(&original, &root.join("other")));

            let request = PreparationRequest {
                source: original.installation.clone().unwrap(),
                root: root.clone(),
                pins: original.monument_assets.clone(),
            };
            let (sender, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            panel.attempted_preparation = Some(request.clone());
            panel.cached_task = Some(CachedTask {
                request, superseded: false, cancel: cancel.clone(), receiver,
            });
            let mut changed = original.clone();
            changed.monument_assets.as_mut().unwrap().bindings_sha256 = "f".repeat(64);
            assert!(!panel.generation_assets_ready(&changed, &root));
            panel.tick_all(&changed, Some(root.clone()), None, [], &ctx);
            assert!(cancel.load(Ordering::Relaxed));
            assert!(Arc::ptr_eq(panel.preparation_token().unwrap(), &cancel));

            // This also covers undoing the pin change while the old worker exits.
            let active = if restore_original { &original } else { &changed };
            panel.accepted = None;
            panel.bindings = None;
            drop(sender);
            panel.tick_all(active, Some(root.clone()), None, [], &ctx);
            assert!(panel.attempted_preparation.as_ref().unwrap().matches(Some(&root), active));
            assert!(!Arc::ptr_eq(panel.preparation_token().unwrap(), &cancel));
        }
    }

    #[test]
    fn preparation_does_not_restart_a_manually_cancelled_request() {
        let ctx = egui::Context::default();
        let (directory, project, mut panel, _) = verified_preview_fixture();
        let root = directory.path().to_path_buf();
        let request = PreparationRequest {
            source: project.installation.clone().unwrap(), root: root.clone(),
            pins: project.monument_assets.clone(),
        };
        let (sender, receiver) = mpsc::channel();
        panel.accepted = None;
        panel.bindings = None;
        panel.attempted_preparation = Some(request.clone());
        panel.cached_task = Some(CachedTask {
            request, superseded: false, cancel: Arc::new(AtomicBool::new(true)), receiver,
        });
        drop(sender);
        panel.tick_all(&project, Some(root), None, [], &ctx);
        assert!(!panel.loading_assets());
    }

    #[test]
    fn selected_stamp_cannot_leak_into_a_different_source_or_target() {
        let ctx = egui::Context::default();
        let mut project = Project::default();
        let mut panel = PrefabPanel::default();
        let path = "assets/test.prefab";
        panel.seed_test_stamp(&ctx, &mut project, path);
        assert!(panel.stamp(&project, path).is_some());
        assert!(panel.stamp(&project, "assets/other.prefab").is_none());
        project.installation = None;
        assert!(panel.stamp(&project, path).is_none());
        panel.tick(&project, false, Some(path), &ctx);
        assert!(panel.stamp.is_none());
    }
    #[test]
    fn queued_stamp_result_is_discarded_when_its_selection_is_invalidated() {
        let ctx = egui::Context::default();
        let mut project = Project::default();
        let mut panel = PrefabPanel::default();
        panel.seed_test_stamp(&ctx, &mut project, "assets/test.prefab");
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        panel.stamp_task = Some(StampTask {
            key: panel.wanted.clone().unwrap(),
            cancel: cancel.clone(),
            receiver,
        });
        assert!(
            sender
                .send(Ok(Some(splat_preview::Preview {
                    material_weights: None,
                    biome_weights: None,
                    palette_hit: None,
                    biome: None,
                    image: egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
                    geometry: splat_preview::Geometry {
                        size: [10., 10.],
                        offset: [0., 0.]
                    },
                    default_yaw: 0.
                })))
                .is_ok()
        );
        panel.tick(&project, false, None, &ctx);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(panel.stamp.is_none());
        assert!(panel.stamp_task.is_none());
    }
}
