use super::*;
use mapper_studio::{
    composition, native_processing::Source, prefab_assets::Catalog, prefab_textures::Bindings,
};
use papyrus_terrain::{monument_composition::Composed, placement::Solution};
use std::sync::{
    Arc, Mutex,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
use std::cell::RefCell;
use std::fmt::Write as _;

#[derive(Clone)]
struct Key {
    project: Arc<Project>,
    palette: Option<String>,
    catalog: String,
    bindings: String,
    layout: Option<[u8;32]>,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.layout == other.layout
            && self.palette == other.palette
            && self.catalog == other.catalog
            && self.bindings == other.bindings
            && (Arc::ptr_eq(&self.project, &other.project) || self.project == other.project)
    }
}

impl Key {
    fn same_map_after_override_edit(&self, next: &Self) -> bool {
        if self.layout != next.layout
            || self.palette != next.palette
            || self.catalog != next.catalog
            || self.bindings != next.bindings
        {
            return false;
        }
        let before = self.project.as_ref();
        let after = next.project.as_ref();
        if before.terrain_overrides == after.terrain_overrides
            && before.biome_overrides == after.biome_overrides
        {
            return false;
        }
        if before.effective_native_profile() != after.effective_native_profile() {
            return false;
        }
        let mut comparable = before.clone();
        comparable.terrain_overrides = after.terrain_overrides;
        comparable.biome_overrides = after.biome_overrides;
        comparable == *after
    }
}
struct Built {
    maps: Arc<Composed>,
    image: Arc<egui::ColorImage>,
    surface: Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    heights: Vec<i16>,
    solution: Solution,
    base_layers: Arc<papyrus_terrain::native_layers::Layers>,
    base_surface: Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    cables: mapper_studio::scene_wires::Preview,
    surface_bounds: Vec<[f32; 4]>,
    generated_monuments: Arc<[map_monuments::Monument]>,
}
struct Task {
    key: Key,
    palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Built>, String>>,
    live: Option<Arc<Mutex<Option<egui::ColorImage>>>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub(super) struct View {
    key: Key,
    pub palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    pub maps: Arc<Composed>,
    pub cables: mapper_studio::scene_wires::Preview,
    pub surface:Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    pub base_layers:Arc<papyrus_terrain::native_layers::Layers>,
    pub base_surface:Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    pub texture: egui::TextureHandle,
    pub image: Arc<egui::ColorImage>,
    pub heights: Vec<i16>,
    pub surface_bounds: Vec<[f32; 4]>,
    pub solution: Solution,
    pub generated_monuments: Arc<[map_monuments::Monument]>,
}
impl View {
    pub fn recipe(&self) -> &Arc<Project> {
        &self.key.project
    }

    pub fn accepted_road_layout(&self) -> Option<mapper_studio::source_road_planning::AcceptedRoadLayout> {
        mapper_studio::source_road_planning::AcceptedRoadLayout::capture(
            &self.key.project, &self.solution, &self.key.catalog, &self.key.bindings, &self.maps,
        )
    }
}
struct PreferenceMatch {
    maps: std::sync::Weak<Composed>,
    project: Project,
    compatible: bool,
}
#[derive(Default)]
pub(super) struct CompositionPanel {
    wanted: Option<Key>,
    last_visible: Option<Key>,
    accepted: Option<View>,
    task: Option<Task>,
    message: String,
    published_layout: Option<(Option<[u8;32]>, Layout)>,
    preference_cache: RefCell<Option<PreferenceMatch>>,
    layout_cache: Option<(Solution, [u8; 32])>,
    live_enabled: Arc<AtomicBool>,
}
pub(super) struct Inputs<'a> {
    pub source: Option<&'a Preview>,
    pub solution: Option<&'a Solution>,
    pub catalog: Option<Arc<Catalog>>,
    pub bindings: Option<Arc<Bindings>>,
    pub monuments: Option<Arc<mapper_studio::monument_source::Prepared>>,
    pub identities: Option<Arc<mapper_studio::client_preparation::ReadyIdentities>>,
    pub road_layout: Option<Arc<mapper_studio::source_road_planning::AcceptedRoadLayout>>,
}
impl CompositionPanel {
    pub fn set_live_preview(&self, enabled: bool) {
        self.live_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn take_live_image(&self) -> Option<egui::ColorImage> {
        self.task.as_ref()?.live.as_ref()?.lock().ok()?.take()
    }

    pub fn take_published_layout(&mut self, project: &Project) -> Option<(Option<[u8; 32]>, Layout)> {
        self.published_layout.take().filter(|(_, layout)| &layout.recipe == project)
    }
    pub fn discard_context(&mut self) {
        self.task=None;self.wanted=None;self.published_layout=None;
    }
    pub fn running(&self) -> bool {self.task.is_some()}
    pub fn progress_token(&self) -> Option<&Arc<AtomicBool>> {
        self.task.as_ref().map(|task| &task.cancel)
    }

    pub fn progress(&self) -> Option<(String, Option<f32>)> {
        let job = mapper_studio::loading_progress::active_for(self.progress_token()?)
            .into_iter().next()?;
        let fraction = job.fraction();
        Some((job.stage, fraction))
    }
    pub fn cancel_pending(&mut self) {self.task=None;}
    pub fn completion(&self,project:&Project)->Result<bool,String> {
        if self.view(project).is_some() {return Ok(true);}
        if self.task.is_some() {return Ok(false);}
        if self.wanted.as_ref().is_some_and(|key|key.project.as_ref()==project) {return Err(self.message.clone());}
        Err("Recipe composition inputs are unavailable".into())
    }

    #[cfg(test)]
    pub(super) fn seed_test_running(&mut self, project: &Project, cancel: Arc<AtomicBool>) {
        let key = Key {
            project: Arc::new(project.clone()),
            palette: None,
            catalog: "a".repeat(64),
            bindings: "b".repeat(64),
            layout: None,
        };
        let (_, receiver) = mpsc::channel();
        self.wanted = Some(key.clone());
        self.task = Some(Task { key, palette: None, cancel, receiver, live: None });
    }

    #[cfg(test)]
    pub fn seed_test_view(&mut self, ctx: &egui::Context, project: &Project) {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size).unwrap();
        let layers = papyrus_terrain::native_layers::Layers {
            size: project.size, terrain_seed: 1, biome_seed: 2,
            height_resolution: hr, surface_resolution: sr,
            height: vec![18000; hr * hr], biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8], topology: vec![0; sr * sr],
            normalized_biome_cells: 0, normalized_splat_cells: 0,
        };
        let maps = papyrus_terrain::monument_composition::compose(&layers, &vec![255; sr * sr], &[], || false)
            .unwrap().unwrap();
        let image = Arc::new(egui::ColorImage::filled([2, 2], Color32::GREEN));
        let key = Key { project: Arc::new(project.clone()), palette: None, catalog: "a".repeat(64), bindings: "b".repeat(64),layout:Some(composition::layout_fingerprint(&Solution {outcomes:vec![],required_missing:0,attempts:0})) };
        self.wanted = Some(key.clone());
        self.accepted = Some(View {
            key, palette: None, maps: Arc::new(maps), cables: Ok(None), surface:None,base_surface:None,base_layers:Arc::new(layers),
            texture: ctx.load_texture("test-composed-terrain", image.clone(), egui::TextureOptions::LINEAR),
            image, heights: vec![18000; 65 * 65], surface_bounds: Vec::new(),
            solution: Solution { outcomes: Vec::new(), required_missing: 0, attempts: 0 },
            generated_monuments: Arc::from([]),
        });
    }
    #[cfg(test)]
    pub(super) fn seed_test_roadside(&mut self, placements:serde_json::Value) {
        let view=self.accepted.as_mut().unwrap();
        let maps=Arc::get_mut(&mut view.maps).unwrap();
        maps.roads=Some(papyrus_terrain::road_network::Report {
            surfaces:Some(papyrus_terrain::road_network::SurfaceOutput {
                report:serde_json::json!({"staged_pipeline":{"roadside":{"placements":placements}}}),chunks:Vec::new(),
            }),..Default::default()
        });
        view.generated_monuments=Arc::from(map_monuments::generated(maps).unwrap());
    }
    #[cfg(test)]
    pub fn seed_test_palette(&mut self, palette: Arc<mapper_studio::terrain_palette::Palette>) {
        let view=self.accepted.as_mut().unwrap();
        view.key.palette=Some(palette.source_render_key());view.palette=Some(palette);
        self.wanted=Some(view.key.clone());
    }
    #[cfg(test)]
    pub fn seed_test_world(&mut self, ctx:&egui::Context, project:&Project,
        solution:Solution, catalog:String, bindings:String) -> egui::TextureId {
        self.seed_test_view(ctx,project);
        let view=self.accepted.as_mut().unwrap();
        view.key.catalog=catalog;view.key.bindings=bindings;
        view.key.layout=Some(composition::layout_fingerprint(&solution));
        view.solution=solution;
        Arc::get_mut(&mut view.maps).unwrap().roads=Some(papyrus_terrain::road_network::Report {
            roads:vec![papyrus_terrain::road_network::Road {from:papyrus_terrain::road_network::Destination::Loop,
                to:papyrus_terrain::road_network::Destination::Loop,points:vec![[-400.,20.,300.],[400.,20.,300.]]}],
            ..Default::default()
        });
        let mut image = egui::ColorImage::filled([64,64],Color32::from_rgb(83,106,67));
        for x in 0..64 { image.pixels[12*64+x]=Color32::from_rgb(40,40,40); }
        image.pixels[50*64+50]=Color32::from_rgb(180,100,40);
        view.image = Arc::new(image);
        view.texture.set(view.image.clone(),egui::TextureOptions::LINEAR);
        self.wanted=Some(view.key.clone());view.texture.id()
    }
    #[cfg(test)]
    pub fn seed_test_world_layers(
        &mut self,
        base: papyrus_terrain::native_layers::Layers,
        layers: papyrus_terrain::native_layers::Layers,
    ) {
        let view = self.accepted.as_mut().unwrap();
        view.base_layers = Arc::new(base);
        let maps = Arc::get_mut(&mut view.maps).unwrap();
        maps.alpha = vec![255; layers.surface_resolution * layers.surface_resolution];
        maps.layers = layers;
        view.image = Arc::new(mapper_studio::map_image::render_composed(
            maps, 64, view.palette.as_deref(), || false,
        ).unwrap().unwrap());
        view.texture.set(view.image.clone(), egui::TextureOptions::LINEAR);
    }
    #[cfg(test)]
    pub fn seed_test_completed(&mut self,ctx:&egui::Context,project:&Project,input:Option<&Solution>,solution:Solution,cancelled:bool) {
        self.discard_context();self.seed_test_view(ctx,project);
        let old=self.accepted.take().unwrap();let mut key=old.key;
        key.layout=input.map(composition::layout_fingerprint);self.wanted=Some(key.clone());
        let maps=Arc::try_unwrap(old.maps).unwrap_or_else(|_|panic!("Test world unexpectedly shared"));
        let(sender,receiver)=mpsc::channel();
        let image = Arc::new(egui::ColorImage::filled([2, 2], Color32::BLUE));
        sender.send(Ok(Some(Built {
            maps: Arc::new(maps),
            image,
            surface: None,
            heights: old.heights,
            solution,
            base_layers: old.base_layers,
            base_surface: None,
            cables: Ok(None),
            surface_bounds: old.surface_bounds,
            generated_monuments: old.generated_monuments,
        }))).unwrap();
        self.task=Some(Task {key,palette:None,cancel:Arc::new(AtomicBool::new(cancelled)),receiver,live:None});
        self.poll(ctx);
    }
    pub fn view(&self, project: &Project) -> Option<&View> {
        self.accepted.as_ref().filter(|v| {
            project.composition_engine.is_some()
                && v.key.project.as_ref() == project
                && self.wanted.as_ref() == Some(&v.key)
        })
    }
    /// Display-only previous world for changes to existing placement preferences.
    /// This is deliberately separate from `view` and all completion/export gates.
    pub fn preference_view(&self, project: &Project, catalog: &str, bindings: &str,
        palette: Option<&str>) -> Option<&View> {
        let view = self.accepted.as_ref()?;
        if view.key.catalog != catalog || view.key.bindings != bindings
            || view.key.palette.as_deref() != palette
            || !(view.key.project.composition_engine.as_deref()
                == Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION)
                || view.maps.roads.is_some() || view.maps.objects.is_some()
                || view.maps.decoration.is_some() || view.maps.rails.is_some())
        {
            return None;
        }
        // Picking asks once per visible object. Clone the recipe only when it
        // changes, and never retain an additional world through this cache.
        if let Some(cached)=&*self.preference_cache.borrow()
            && std::sync::Weak::ptr_eq(&cached.maps,&Arc::downgrade(&view.maps)) && &cached.project==project {
            return cached.compatible.then_some(view);
        }
        let mut previous = view.key.project.as_ref().clone();
        previous.targets = project.targets.clone();
        let compatible=previous == *project && view.key.project.targets.len() == project.targets.len()
            && view.key.project.targets.iter().zip(&project.targets).all(|(a,b)|
                a.instance_id == b.instance_id && a.prefab_path == b.prefab_path
                    && a.name == b.name && a.presence == b.presence);
        self.preference_cache.replace(Some(PreferenceMatch {maps:Arc::downgrade(&view.maps),project:project.clone(),compatible}));
        compatible.then_some(view)
    }
    fn select_context(&mut self,key:Option<Key>)->bool {
        if let (Some(before), Some(after)) = (&self.wanted, &key)
            && before.same_map_after_override_edit(after)
        {
            // Keep the same world and worker, but publish and export against
            // the exact current recipe, including its checkbox selections.
            if let Some(task) = &mut self.task
                && task.key == *before
            {
                task.key.project = after.project.clone();
            }
            if let Some(view) = &mut self.accepted
                && view.key == *before
            {
                view.key.project = after.project.clone();
            }
            if let Some(visible) = &mut self.last_visible
                && visible == before
            {
                visible.project = after.project.clone();
            }
            if let Some((_, layout)) = &mut self.published_layout
                && layout.recipe == *before.project
            {
                layout.recipe.clone_from(&after.project);
            }
            self.wanted = key;
            return false;
        }
        let changed=self.wanted!=key;
        if changed {self.task=None;self.wanted=key;self.message.clear();self.published_layout=None;}
        changed
    }
    fn can_reuse_topology(&self,key:&Key)->bool {
        self.accepted.as_ref().is_some_and(|v| {
            let mut old=v.key.project.as_ref().clone();let mut new=key.project.as_ref().clone();
            old.topology_edits=None;new.topology_edits=None;
            v.key.layout==key.layout && v.key.palette==key.palette && old==new
                && v.key.catalog==key.catalog && v.key.bindings==key.bindings
        })
    }
    pub(super) fn poll(&mut self,ctx:&egui::Context) {
        if let Some(task) = &self.task {
            let result = match task.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Composition worker ended unexpectedly".into()))
                }
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let accepted =
                    self.wanted.as_ref() == Some(&task.key) && !task.cancel.load(Ordering::Relaxed);
                let mut task_key = task.key.clone();
                let task_palette = task.palette.clone();
                self.task = None;
                if accepted {
                    match result {
                        Ok(Some(Built {
                            maps, image, surface, heights, solution, base_layers,
                            base_surface, cables, surface_bounds, generated_monuments,
                        })) => {
                            let input_layout=task_key.layout;
                            task_key.layout=Some(composition::layout_fingerprint(&solution));
                            self.wanted=Some(task_key.clone());
                            let recipe = task_key.project.as_ref().clone();
                            let assets = mapper_studio::placement_preview::is_earthwork(&recipe.placement_engine)
                                .then(|| [task_key.catalog.clone(), task_key.bindings.clone()]);
                            self.published_layout = Some((input_layout, Layout {
                                recipe,
                                solution: solution.clone(),
                                assets,
                            }));
                            let task_palette=task_palette.filter(|p|p.covers_layers(&maps.layers));
                            self.accepted = Some(View {
                                key: task_key,
                                cables,
                                palette: task_palette,
                                maps, surface, base_layers, base_surface,
                                texture: ctx.load_texture(
                                    "monument-terrain",
                                    image.clone(),
                                    egui::TextureOptions::LINEAR,
                                ),
                                image,
                                heights,
                                surface_bounds,
                                solution,
                                generated_monuments,
                            });
                            self.message.clear();
                        }
                        Ok(None) => {
                            self.message = "Composition cancelled; base terrain retained".into()
                        }
                        Err(e) => self.message = format!("Composition unavailable: {e}"),
                    }
                }
            }
        }
    }
    pub fn tick(&mut self, project: &Project, inputs: Inputs<'_>, ctx: &egui::Context) -> bool {
        let source_inputs = inputs
            .source
            .zip(inputs.catalog)
            .zip(inputs.bindings)
            .filter(|_| project.composition_engine.is_some() &&
                (inputs.solution.is_some() || composition::selects_monument_layout(project)));
        let key = source_inputs.as_ref().map(|((source, catalog), bindings)| {
            self.request_key(project, source, inputs.solution, catalog, bindings)
        });
        let changed=self.select_context(key.clone());
        self.poll(ctx);
        if changed
            && self.view(project).is_none()
            && let Some(((source, catalog), bindings)) = source_inputs
            && let Some(key) = key
            && let Some(layers) = &source.native_layers
        {
            let source_ocean = source.ocean.clone();
            let source = Source {
                terrain: None,
                native_base: source.raw_source.as_ref().map(|raw| raw.layers.clone()),
                rivers: source.rivers.clone(),
                palette: source.palette.clone(),
                key: mapper_studio::native_preview::processing_key(&source.key),
                // The worker renders the accepted composition from its layers.
                image: egui::ColorImage::new([0, 0], Vec::new()),
                heights: source.heights.clone(),
                layers: layers.clone(),
            };
            let palette = source.palette.clone();
            let (sender, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let reuse = inputs.solution.is_some() && self.can_reuse_topology(&key);
            let reused_base = if reuse { self.accepted.as_ref().map(|v| (v.base_layers.clone(), v.base_surface.clone())) } else { None };
            let reused = if reuse {
                self.accepted
                    .take()
                    .and_then(|view| match Arc::try_unwrap(view.maps) {
                        Ok(maps) => Some((maps, view.image, view.heights,view.cables)),
                        Err(maps) => {
                            self.accepted = Some(View { maps, ..view });
                            None
                        }
                    })
            } else {
                None
            };
            let replaying = reused.is_some();
            let roads_enabled = project.roads.is_some();
            let project = key.project.clone();
            let solution = inputs.solution.cloned();
            let identities = inputs.identities;
            let monuments = inputs.monuments;
            let road_layout = inputs.road_layout;
            let repaint = ctx.clone();
            let live = Arc::new(Mutex::new(None));
            let live_sink = live.clone();
            let live_enabled = self.live_enabled.clone();
            std::thread::spawn(move || {
                let _identities = mapper_studio::prefab_identities::Scope::with_prepared(
                    bindings.installation_root(), &catalog, identities.as_deref(),
                );
                let _monuments = mapper_studio::monument_source::Scope::with_prepared(
                    project.monument_rules.as_ref(), &catalog, &bindings, monuments.as_deref(),
                );
                let progress = mapper_studio::loading_progress::begin("Composing map", Some(signal.clone()));
                let reporter = progress.reporter();
                let preview_palette = source.palette.clone();
                let preview_cancel = signal.clone();
                let preview_repaint = repaint.clone();
                let last_preview = std::cell::Cell::new(None::<std::time::Instant>);
                let _live = composition::LivePreviewScope::new(move |maps| {
                    let started = std::time::Instant::now();
                    let cancelled = || preview_cancel.load(Ordering::Relaxed)
                        || !live_enabled.load(Ordering::Relaxed)
                        || started.elapsed() > std::time::Duration::from_millis(75);
                    if cancelled() || last_preview.get().is_some_and(|last| {
                        last.elapsed() < std::time::Duration::from_millis(350)
                    }) {
                        return;
                    }
                    last_preview.set(Some(started));
                    if let Ok(Some(image)) = mapper_studio::map_image::render_composition_thumbnail(
                        maps, preview_palette.as_deref(), cancelled,
                    ) && let Ok(mut slot) = live_sink.lock() {
                        *slot = Some(image);
                        preview_repaint.request_repaint();
                    }
                });
                let result = if let (Some((mut maps, image, heights,cables)), Some(solution)) =
                    (reused, solution.as_ref())
                {
                    reporter.stage("Applying topology edits");
                    composition::finish_topology(
                        &project,
                        solution,
                        &catalog,
                        &bindings,
                        &mut maps,
                        || signal.load(Ordering::Relaxed),
                    )
                    .map(|done| done.then_some((maps, Some(image), heights,solution.clone(),cables,None)))
                } else {
                    mapper_studio::source_road_planning::with_accepted_road_layout(road_layout, || composition::compose_resolved_layout_with_worker(
                        &source,
                        &project,
                        solution.as_ref(),
                        &catalog,
                        &bindings,
                        None,
                        || signal.load(Ordering::Relaxed),
                    ))
                    .and_then(|result| {
                        result
                            .map(|composition::ResolvedLayout {maps,solution,cables,base}| {
                                let heights = maps
                                    .layers
                                    .sample_heights(mapper_studio::native_preview::RESOLUTION)?;
                                Ok((maps, None, heights,solution,cables,base))
                            })
                            .transpose()
                    })
                };
                let result=result.and_then(|built| {
                    let Some((maps,previous_image,heights,solution,cables,base))=built else{return Ok(None);};
                    reporter.stage("Rendering map preview");
                    let maps=Arc::new(maps);
                    let (image, surface) = if let Some(image) = previous_image {
                        let surface = mapper_studio::map_image::prepare_composed_viewport_surface(
                            &maps, image.size[0], image.pixels.len() * size_of::<egui::Color32>(),
                            source.palette.as_deref(), || signal.load(Ordering::Relaxed))?;
                        (image, surface)
                    } else {
                        let Some(rendered)=mapper_studio::map_image::render_composed_viewport(&maps,source.palette.as_deref(),||signal.load(Ordering::Relaxed))? else{return Ok(None);};
                        (Arc::new(rendered.image), rendered.surface)
                    };
                    let (base_layers, base_surface) = if let Some(reused) = reused_base { reused } else {
                        let (layers, ocean) = base.map_or_else(
                            || (source.layers.clone(), source_ocean.clone()),
                            |base| (base.layers, base.ocean),
                        );
                        let retained = image.pixels.len() * size_of::<egui::Color32>()
                            + surface.as_ref().map_or(0, |frame| frame.retained_bytes());
                        let surface = mapper_studio::map_image::prepare_native_viewport_surface(
                            &layers, ocean.as_deref(), image.size[0], retained,
                            source.palette.as_deref(), || signal.load(Ordering::Relaxed))?;
                        (layers, surface)
                    };
                    if signal.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    let generated_monuments = Arc::from(map_monuments::generated(&maps)?);
                    let surface_bounds = terrain_3d::surface_bounds(terrain_3d::surface_instances(&maps));
                    Ok(Some(Built {
                        maps, image, surface, heights, solution, base_layers, base_surface,
                        cables, surface_bounds, generated_monuments,
                    }))
                });
                drop(progress);
                let _ = sender.send(result);
                repaint.request_repaint();
            });
            self.task = Some(Task {
                key,
                palette,
                cancel,
                receiver,
                live: Some(live),
            });
            self.message = if replaying {
                "Replaying manual topology and refreshing spawn diagnostics…"
            } else if roads_enabled {
                "Composing monuments and access roads in the background…"
            } else {
                "Composing monument terrain in the background…"
            }
            .into();
        }
        let visible = self.view(project).map(|v| v.key.clone());
        let redraw = visible != self.last_visible;
        self.last_visible = visible;
        redraw
    }
    fn request_key(
        &mut self,
        project: &Project,
        source: &Preview,
        solution: Option<&Solution>,
        catalog: &Catalog,
        bindings: &Bindings,
    ) -> Key {
        let layout = solution.map(|solution| {
            let (cached, fingerprint) = self.layout_cache.get_or_insert_with(|| {
                (solution.clone(), composition::layout_fingerprint(solution))
            });
            if !same_layout(cached, solution) {
                *cached = solution.clone();
                *fingerprint = composition::layout_fingerprint(solution);
            }
            *fingerprint
        });
        let project = self.wanted.as_ref()
            .filter(|key| key.project.as_ref() == project)
            .map_or_else(|| Arc::new(project.clone()), |key| key.project.clone());
        Key {
            project,
            palette: source.palette.as_ref().map(|palette| palette.source_render_key()),
            catalog: catalog.sha256.clone(),
            bindings: bindings.sha256.clone(),
            layout,
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, project: &Project) {
        let view = self.view(project);
        if let Some(view) = view {
            ui.label("Composition ready");
            if view.solution.required_missing > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, "Required monuments are missing.");
            }
            if view.maps.spawn_audit.as_ref().is_some_and(|audit| audit.character_candidate_cells == 0) {
                ui.colored_label(ui.visuals().warn_fg_color, "No qualifying player spawn area found.");
            }
            if view.maps.roads.as_ref().is_some_and(|roads| !roads.unresolved.is_empty() || !roads.missing_metadata.is_empty()) {
                ui.colored_label(ui.visuals().warn_fg_color, "Some monument roads are unresolved.");
            }
            ui.collapsing("Details", |ui| {
                diagnostic_ui::details(ui, &composition_report(&view.maps));
            });
        } else if self.message.is_empty() {
            ui.weak("Terrain stamps apply during generation.");
        } else {
            diagnostic_ui::message(ui, &self.message, "composition-details");
        }
        let current = view.is_some();
        if self.task.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                if ui.button("Cancel composition").clicked() {
                    self.task = None;
                    self.message = "Composition cancelled. Base terrain retained.".into();
                }
            });
        } else if self.wanted.is_some()
            && !current
            && ui.button("Retry composition").clicked()
        {
            self.wanted = None;
        }
    }
}

fn composition_report(maps: &Composed) -> String {
    let mut report = format!("{} height samples clamped.\n", maps.clamped_height_samples);
    if let Some(ocean) = &maps.ocean {
        let _ = writeln!(
            report,
            "Ocean rebuilt with {} sea cells and {} unfilled enclosed cells.",
            ocean.ocean_cells, ocean.enclosed_cells
        );
    }
    if let Some(roads) = &maps.roads {
        let _ = writeln!(
            report,
            "{} road corridors, {} unresolved monuments, {} missing metadata entries.",
            roads.roads.len(),
            roads.unresolved.len(),
            roads.missing_metadata.len()
        );
        if roads.budget_exhausted {
            report.push_str("Road search budget reached.\n");
        }
        if let Some(ring) = &roads.ring {
            let _ = writeln!(report, "Main road ring — {}.", ring.status);
            if let Some(reason) = ring.reason {
                let _ = writeln!(report, "{reason}.");
            }
            if let Some(reason) = ring.shaping_fallback {
                let _ = writeln!(
                    report,
                    "Original ring path retained after smoothing failed ({reason})."
                );
            }
        }
    }
    if let Some(audit) = &maps.spawn_audit {
        let _ = writeln!(
            report,
            "{} potential player spawn cells.",
            audit.character_candidate_cells
        );
    } else {
        report.push_str(
            "Spawn eligibility could not be checked because matching filters were unavailable.\n",
        );
    }
    if let Some(objects) = &maps.objects {
        let _ = writeln!(
            report,
            "{} live object instances admitted with {} initial monument blockers.",
            objects.instances.len(),
            objects.initial_blockers.modifiers
        );
    } else if let Some(decoration) = &maps.decoration {
        let _ = writeln!(
            report,
            "{} static decoration candidates.",
            decoration.candidates.len()
        );
        if decoration.budget_exhausted {
            report.push_str("Decoration candidate budget reached.\n");
        }
        if let Some(fit) = &decoration.fitting {
            let counts = &fit.counts;
            let _ = writeln!(
                report,
                "{} geometrically fitted, {} unsupported, {} rejected by anchors.",
                counts.fitted, counts.unsupported, counts.rejected_anchors
            );
        }
        if let Some(placement) = &decoration.terrain_placement {
            let _ = writeln!(
                report,
                "{} terrain placements, {} overlap rejections, {} height writes.",
                placement.counts.accepted, placement.counts.occupied, placement.height_writes
            );
            if let Some(blocking) = &placement.blocking {
                let _ = writeln!(
                    report,
                    "{} blockers covered {} cells.",
                    blocking.instances, blocking.cells
                );
            }
            if let Some(topology) = &placement.topology {
                let _ = writeln!(
                    report,
                    "{} topology cells changed, {} filter rejections, {} read conflicts.",
                    topology.changed_cells, topology.filter_rejections, topology.read_conflicts
                );
            }
            if let Some(materials) = &placement.materials {
                let _ = writeln!(
                    report,
                    "{} material cells painted, {} filter rejections, {} paint conflicts.",
                    materials.changed_cells, materials.filter_rejections, materials.read_conflicts
                );
            }
            if let Some(height) = &placement.height_effects {
                let _ = writeln!(
                    report,
                    "{} source height samples changed (maximum raise {:.1} m, maximum cut {:.1} m).",
                    height.changed_samples, height.maximum_raise, height.maximum_lower
                );
            }
            if let Some(collider) = &placement.collider_effects {
                let _ = writeln!(
                    report,
                    "{} raw collider samples changed (maximum raise {:.1} m).",
                    collider.changed_samples, collider.maximum_raise
                );
            }
        }
    }
    if let Some(topology) = &maps.topology_overrides {
        let _ = writeln!(
            report,
            "{} manual topology strokes changed {} cells.",
            topology.strokes, topology.changed_cells
        );
    }
    report.push_str("Generation diagnostics cover terrain and placement. In-game behavior requires a separate check.");
    report
}

// Match the fingerprint's IEEE representations, including signed zero and NaN
// payloads. Ordinary float equality cannot decide whether a digest is reusable.
fn same_layout(a: &Solution, b: &Solution) -> bool {
    a.required_missing == b.required_missing
        && a.attempts == b.attempts
        && a.outcomes.len() == b.outcomes.len()
        && a.outcomes.iter().zip(&b.outcomes).all(|(a, b)| {
            let pose_bits = |p: &papyrus_terrain::placement::Position| {
                [p.x, p.y, p.z, p.yaw, p.clearance].map(f32::to_bits)
            };
            a.id == b.id
                && a.position.as_ref().map(pose_bits) == b.position.as_ref().map(pose_bits)
                && a.distance.map(f32::to_bits) == b.distance.map(f32::to_bits)
                && a.within_radius == b.within_radius
                && a.reason == b.reason
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
    #[test]
    fn current_monument_only_world_retains_paint_during_preference_edits() {
        let ctx = egui::Context::default();
        let project = Project {
            size: 1000,
            composition_engine: Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into()),
            ..Default::default()
        };
        let mut panel = CompositionPanel::default();
        let texture = panel.seed_test_world(
            &ctx, &project, Solution { outcomes: vec![], required_missing: 0, attempts: 0 },
            "a".repeat(64), "b".repeat(64),
        );
        Arc::get_mut(&mut panel.accepted.as_mut().unwrap().maps).unwrap().roads = None;
        let mut edited = project.clone();
        edited.targets[0].x += 30.;
        edited.targets[0].yaw = Some(37.);
        let retained = panel.preference_view(&edited, &"a".repeat(64), &"b".repeat(64), None).unwrap();
        assert_eq!(retained.texture.id(), texture);
        assert!(retained.maps.roads.is_none());
        assert!(panel.view(&edited).is_none());
        assert_ne!(panel.completion(&edited), Ok(true));
        edited.seeds[0].value += 1;
        assert!(panel.preference_view(&edited, &"a".repeat(64), &"b".repeat(64), None).is_none());
    }
    #[test]
    fn retained_world_is_display_only_and_bound_to_existing_preferences_and_sources() {
        let ctx=egui::Context::default();let project=Project{size:1000,..Default::default()};
        let mut panel=CompositionPanel::default();
        panel.seed_test_world(&ctx,&project,Solution{outcomes:vec![],required_missing:0,attempts:0},"a".repeat(64),"b".repeat(64));
        let mut edited=project.clone();edited.targets[0].x+=30.;edited.targets[0].yaw=Some(37.);
        let available=|p:&Project|panel.preference_view(p,&"a".repeat(64),&"b".repeat(64),None).is_some();
        assert!(available(&edited));assert!(panel.view(&edited).is_none());
        assert_ne!(panel.completion(&edited),Ok(true));
        assert!(panel.preference_view(&edited,&"c".repeat(64),&"b".repeat(64),None).is_none());
        assert!(panel.preference_view(&edited,&"a".repeat(64),&"c".repeat(64),None).is_none());
        assert!(panel.preference_view(&edited,&"a".repeat(64),&"b".repeat(64),Some("changed-palette")).is_none());
        for kind in 0..8 {
            let mut other=edited.clone();
            match kind {
                0=>other.targets[0].prefab_path=Some("different.prefab".into()),
                1=>other.targets[0].presence=Presence::Excluded,
                2=>other.targets[0].instance_id+=100,
                3=>{other.targets.pop();},
                4=>other.seeds[0].value+=1,
                5=>other.seeds[3].value+=1,
                6=>other.composition_engine=None,
                _=>other.performance.monuments=false,
            }
            assert!(!available(&other),"Retained world survived incompatible change {kind}");
        }
    }
    #[test]
    fn retained_world_survives_failed_and_cancelled_rebuilds_without_becoming_current() {
        for cancelled in [false,true] {
            let ctx=egui::Context::default();let project=Project{size:1000,..Default::default()};
            let mut panel=CompositionPanel::default();
            let texture=panel.seed_test_world(&ctx,&project,Solution{outcomes:vec![],required_missing:0,attempts:0},"a".repeat(64),"b".repeat(64));
            let mut edited=project.clone();edited.targets[0].x+=25.;
            let mut key=panel.accepted.as_ref().unwrap().key.clone();key.project=Arc::new(edited.clone());
            panel.select_context(Some(key.clone()));
            let (sender,receiver)=mpsc::channel();sender.send(Err("Fixture placement failure".into())).unwrap();
            panel.task=Some(Task{key,palette:None,cancel:Arc::new(AtomicBool::new(cancelled)),receiver,live:None});
            panel.poll(&ctx);
            assert_eq!(panel.preference_view(&edited,&"a".repeat(64),&"b".repeat(64),None).unwrap().texture.id(),texture);
            assert!(panel.view(&edited).is_none());assert_ne!(panel.completion(&edited),Ok(true));
            // Publishing another accepted world must retire cached compatibility.
            panel.seed_test_world(&ctx,&edited,Solution{outcomes:vec![],required_missing:0,attempts:0},"a".repeat(64),"b".repeat(64));
            assert!(panel.view(&edited).is_some());
            let mut incompatible=edited.clone();incompatible.targets[0].presence=Presence::Excluded;
            assert!(panel.preference_view(&incompatible,&"a".repeat(64),&"b".repeat(64),None).is_none());
        }
    }
    #[test]
    fn automatic_stamp_status_fits_narrow_panels_after_recipe_upgrade() {
        for dark in [false,true] { for (revision,name) in [
            (papyrus_terrain::monument_composition::WATER_VERSION,"legacy"),
            (papyrus_terrain::monument_composition::SOURCE_TEXTURE_VERSION,"source"),
            (papyrus_terrain::monument_composition::SOURCE_CLIFF_VERSION,"cliff"),
            (papyrus_terrain::monument_composition::SOURCE_RASTER_VERSION,"raster"),
            (papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION,"height")] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark {egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut panel=CompositionPanel::default();
            let mut project=Project {preview_engine:mapper_studio::native_preview::VERSION.into(),..Default::default()};
            project.composition_engine=Some(revision.into());
            let mut before=project.clone();before.composition_engine=Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());let size=[340,900];let mut textures=snapshot::Textures::default();let mut last=None;
            mapper_studio::generation_defaults::apply(&mut project);
            assert_eq!(project, before);
            for _ in 0..2 {
                let mut output=ctx.run_ui(egui::RawInput {screen_rect:Some(Rect::from_min_size(Pos2::ZERO,Vec2::new(size[0] as f32,size[1] as f32))),..Default::default()},|ui|{ui.set_max_width(324.);panel.show(ui,&project);});
                textures.update(&output.textures_delta);output.textures_delta.clear();last=Some(output);
            }
            let output=last.unwrap();let mut present=false;
            for shape in &output.shapes {if let egui::Shape::Text(t)=&shape.shape {
                assert!(t.pos.x+t.galley.size().x<=340.,"Overflow: {}",t.galley.job.text);
                present|=t.galley.job.text=="Terrain stamps apply during generation.";
                assert_ne!(t.galley.job.text, "Retry composition");
            }}
            assert!(present);assert_eq!(project,before);
            if let Some(dir)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                let path=std::path::Path::new(&dir).join(format!("stamp-controls-{}-{}.png",if dark {"dark"}else{"light"},name));
                let image=textures.raster(&ctx,output,size);
                mapper_studio::image_export::write_png(&path,&image,||false).unwrap();
            }
        }}
    }

    #[test]
    fn composition_ready_keeps_report_in_details_and_clears_when_recipe_changes() {
        let ctx = egui::Context::default();
        ctx.global_style_mut(|style| style.animation_time = 0.);
        let mut panel = CompositionPanel::default();
        let mut project = Project {
            size: 1000,
            composition_engine: Some(
                papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into(),
            ),
            ..Default::default()
        };
        panel.seed_test_view(&ctx, &project);

        let render = |panel: &mut CompositionPanel, project: &Project, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(220., 450.))),
                    events,
                    ..Default::default()
                },
                |ui| panel.show(ui, project),
            );
            output.textures_delta.clear();
            output
                .shapes
                .into_iter()
                .filter_map(|shape| {
                    let egui::Shape::Text(text) = shape.shape else {
                        return None;
                    };
                    Some((
                        text.galley.job.text.clone(),
                        text.pos + text.galley.size() * 0.5,
                    ))
                })
                .collect::<Vec<_>>()
        };
        let initial = render(&mut panel, &project, vec![]);
        assert!(
            initial
                .iter()
                .any(|(label, _)| label == "Composition ready")
        );
        assert!(
            !initial
                .iter()
                .any(|(label, _)| label.contains("height samples"))
        );
        let details = initial
            .iter()
            .find(|(label, _)| label == "Details")
            .unwrap()
            .1;
        for pressed in [true, false] {
            render(
                &mut panel,
                &project,
                vec![
                    egui::Event::PointerMoved(details),
                    egui::Event::PointerButton {
                        pos: details,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let expanded = render(&mut panel, &project, vec![]);
        assert!(
            expanded
                .iter()
                .any(|(label, _)| label == "Copy full details")
        );
        assert!(
            expanded
                .iter()
                .any(|(label, _)| label.contains("height samples"))
        );
        project.master_seed += 1;
        let stale = render(&mut panel, &project, vec![]);
        assert!(
            !stale
                .iter()
                .any(|(label, _)| label == "Composition ready" || label.contains("height samples"))
        );
    }

    #[test]
    fn composition_progress_follows_the_active_job_and_clears_when_cancelled() {
        let token = Arc::new(AtomicBool::new(false));
        let (_sender, receiver) = mpsc::channel();
        let mut panel = CompositionPanel {
            task: Some(Task {
                key: Key {
                    project: Arc::new(Project::default()),
                    palette: None,
                    catalog: "a".repeat(64),
                    bindings: "b".repeat(64),
                    layout: None,
                },
                palette: None,
                cancel: token.clone(),
                receiver,
                live: None,
            }),
            ..Default::default()
        };
        {
            let _other = mapper_studio::loading_progress::begin(
                "Reading Rust installation",
                Some(Arc::new(AtomicBool::new(false))),
            );
            assert_eq!(panel.progress(), None);
        }
        let progress = mapper_studio::loading_progress::begin("Composing map", Some(token.clone()));
        let reporter = progress.reporter();
        reporter.stage("Simulating erosion");
        reporter.update("", 2, Some(8), mapper_studio::loading_progress::Unit::Items);
        assert_eq!(panel.progress(), Some(("Simulating erosion".into(), Some(0.25))));
        reporter.stage("Placing cliffs");
        assert_eq!(panel.progress(), Some(("Placing cliffs".into(), None)));
        panel.cancel_pending();
        assert!(token.load(Ordering::Relaxed));
        assert_eq!(panel.progress(), None);
    }

    #[test]
    fn default_override_edits_reuse_the_world_and_publish_the_current_recipe() {
        use mapper_studio::{installation, native_profile, native_preview};

        let profile: native_profile::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-58f3263f.json"
        )).unwrap();
        let original = Project {
            size: 1000,
            preview_engine: native_preview::VERSION.into(),
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: profile.native_sha256,
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let mut enabled = original.clone();
        enabled.terrain_overrides = Some(native_profile::TerrainOverrides::from_settings(&profile.settings));
        enabled.biome_overrides = Some(native_profile::BiomeOverrides::from_settings(&profile.settings));
        assert_eq!(enabled.terrain_key(), original.terrain_key());

        let ctx = egui::Context::default();
        let mut panel = CompositionPanel::default();
        panel.seed_test_view(&ctx, &original);
        let old_key = panel.wanted.clone().unwrap();
        let mut next_key = old_key.clone();
        next_key.project = Arc::new(enabled.clone());
        panel.last_visible = Some(old_key.clone());

        let view = panel.view(&original).unwrap();
        let maps = view.maps.clone();
        let texture_id = view.texture.id();
        let solution = view.solution.clone();
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Some(Built {
            maps: maps.clone(),
            image: view.image.clone(),
            surface: None,
            heights: view.heights.clone(),
            solution: solution.clone(),
            base_layers: view.base_layers.clone(),
            base_surface: None,
            cables: Ok(None),
            surface_bounds: view.surface_bounds.clone(),
            generated_monuments: view.generated_monuments.clone(),
        }))).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        panel.task = Some(Task {
            key: old_key,
            palette: None,
            cancel: cancel.clone(),
            receiver,
            live: None,
        });

        assert!(!panel.select_context(Some(next_key.clone())));
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(panel.view(&original).is_none());
        let view = panel.view(&enabled).unwrap();
        assert_eq!(view.texture.id(), texture_id);
        assert!(Arc::ptr_eq(&view.maps, &maps));
        assert!(panel.last_visible.as_ref() == Some(&next_key));
        panel.poll(&ctx);
        assert!(Arc::ptr_eq(&panel.view(&enabled).unwrap().maps, &maps));
        assert_eq!(panel.take_published_layout(&enabled).unwrap().1.recipe, enabled);

        let mut changed = enabled.clone();
        let biomes = changed.biome_overrides.as_mut().unwrap();
        biomes.angle = (biomes.angle + 90_000) % 360_000;
        let mut changed_key = next_key;
        changed_key.project = Arc::new(changed.clone());
        assert!(panel.select_context(Some(changed_key)));
        assert!(panel.view(&changed).is_none());
    }

    #[test]
    fn changed_solution_for_same_recipe_invalidates_world_and_cancels_queued_reply() {
        let ctx=egui::Context::default();let project=Project {size:1000,..Default::default()};
        let mut panel=CompositionPanel::default();panel.seed_test_view(&ctx,&project);
        let old=panel.wanted.clone().unwrap();let mut next=old.clone();
        let changed=Solution {outcomes:vec![],required_missing:1,attempts:0};
        next.layout=Some(composition::layout_fingerprint(&changed));
        assert_ne!(old.layout,next.layout);
        assert!(panel.can_reuse_topology(&old));
        assert!(!panel.can_reuse_topology(&next),"Changed placement reused old stamped terrain");
        let(sender,receiver)=mpsc::channel();sender.send(Err("Old layout failure".into())).unwrap();
        let cancel=Arc::new(AtomicBool::new(false));
        panel.task=Some(Task {key:old,palette:None,cancel:cancel.clone(),receiver,live:None});
        assert!(panel.select_context(Some(next)));
        panel.poll(&ctx);
        assert!(cancel.load(Ordering::Relaxed));assert!(panel.task.is_none());
        assert!(panel.view(&project).is_none());assert!(panel.message.is_empty());
        assert!(panel.take_published_layout(&project).is_none());
    }
    #[test]
    fn optional_composition_frame_does_not_wait_for_the_image_pool() {
        let project = Project { size: 1000, ..Default::default() };
        let mut panel = CompositionPanel::default();
        panel.seed_test_view(&egui::Context::default(), &project);
        let maps = &panel.accepted.as_ref().unwrap().maps;
        std::thread::scope(|scope| {
            let (ready, entered) = mpsc::channel();
            let (release, held) = mpsc::channel::<()>();
            scope.spawn(move || {
                mapper_studio::generation_resources::with_cpu_pool(
                    || false,
                    |_, _| {
                        ready.send(()).unwrap();
                        let _ = held.recv();
                        Ok(Some(()))
                    },
                ).unwrap();
            });
            entered.recv().unwrap();
            let (complete, received) = mpsc::channel();
            scope.spawn(move || {
                complete.send(mapper_studio::map_image::render_composition_thumbnail(
                    maps, None, || false,
                )).unwrap();
            });
            let result = received.recv_timeout(std::time::Duration::from_secs(5));
            drop(release);
            assert!(result.expect("live frame waited for the busy image pool").unwrap().is_none());
        });
    }

    #[test]
    fn losing_source_context_cancels_task_and_discards_queued_result() {
        let project = Project::default();
        let key = Key {
            project: Arc::new(project.clone()),
            palette: None,
            catalog: "a".repeat(64),
            bindings: "b".repeat(64),
            layout: Some([0;32]),
        };
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Err("Queued failure from an obsolete context".into()))
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let old_preview = Arc::new(Mutex::new(Some(egui::ColorImage::filled([2, 2], Color32::BLUE))));
        let mut panel = CompositionPanel {
            wanted: Some(key.clone()),
            task: Some(Task {
                key,
                palette: None,
                cancel: cancel.clone(),
                receiver,
                live: Some(old_preview.clone()),
            }),
            ..Default::default()
        };
        assert!(panel.take_live_image().is_some());
        assert!(panel.take_live_image().is_none());
        panel.tick(
            &project,
            Inputs {
                source: None,
                solution: None,
                catalog: None,
                bindings: None,
                monuments: None,
                identities: None,
                road_layout: None,
            },
            &egui::Context::default(),
        );
        assert!(cancel.load(Ordering::Relaxed));
        assert!(panel.task.is_none());
        // Even a frame that reaches the old worker slot late cannot appear.
        *old_preview.lock().unwrap() = Some(egui::ColorImage::filled([2, 2], Color32::RED));
        assert!(panel.take_live_image().is_none());
        assert!(panel.view(&project).is_none());
        assert!(panel.message.is_empty());
    }
}

#[cfg(test)]
mod performance_tests;
