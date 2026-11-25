//! Presentation-only repaint worker. Never invokes terrain or monument generation.
use eframe::egui;
use mapper_studio::{map_colors::Preferences, terrain_palette::Palette};
use papyrus_terrain::{monument_composition::Composed, native_layers::Layers, ocean::Ocean};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
#[derive(Clone)]
pub(super) enum Base {
    Native(Arc<Layers>, Option<Arc<Ocean>>),
    Composed(Arc<Composed>),
}
impl Base {
    fn key(&self) -> BaseKey {
        match self {
            Self::Native(layers, ocean) => {
                BaseKey::Native(Arc::downgrade(layers), ocean.as_ref().map(Arc::downgrade))
            }
            Self::Composed(maps) => BaseKey::Composed(Arc::downgrade(maps)),
        }
    }
    fn render(
        &self,
        palette: &Palette,
        cancel: &AtomicBool,
    ) -> Result<Option<mapper_studio::map_image::Rendered>, String> {
        match self {
            Self::Native(l, o) => mapper_studio::map_image::render_native_viewport(
                l,
                o.as_deref(),
                Some(palette),
                || cancel.load(Ordering::Relaxed),
            ),
            Self::Composed(m) => {
                mapper_studio::map_image::render_composed_viewport(m, Some(palette), || {
                    cancel.load(Ordering::Relaxed)
                })
            }
        }
    }
}
// Retain identity, not raster ownership. In particular, a finished repaint must
// not prevent CompositionPanel from unwrapping its map for topology replay.
#[derive(Clone, Debug)]
enum BaseKey {
    Native(Weak<Layers>, Option<Weak<Ocean>>),
    Composed(Weak<Composed>),
}
impl BaseKey {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Native(a, wa), Self::Native(b, wb)) => {
                Weak::ptr_eq(a, b)
                    && match (wa, wb) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Weak::ptr_eq(a, b),
                        _ => false,
                    }
            }
            (Self::Composed(a), Self::Composed(b)) => Weak::ptr_eq(a, b),
            _ => false,
        }
    }
}
struct Task {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Painted>, String>>,
    clean: Option<(BaseKey, String)>,
}
#[derive(Debug)]
struct CleanPaint {
    surface: Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    key: (BaseKey, String),
    palette: Arc<Palette>,
    image: egui::ColorImage,
}
#[derive(Debug)]
struct Painted {
    surface: Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    image: egui::ColorImage,
    clean: Option<CleanPaint>,
}
struct Cached {
    key: (BaseKey, String),
    view: View,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub(super) struct View {
    pub surface: Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    pub palette: Arc<Palette>,
    pub texture: egui::TextureHandle,
    pub image: Arc<egui::ColorImage>,
}
#[derive(Default)]
pub(super) struct Panel {
    wanted: Option<(BaseKey, String)>,
    task: Option<Task>,
    accepted: Option<View>,
    native: Option<Cached>,
    composed: Option<Cached>,
    pending_palette: Option<Arc<Palette>>,
    failed_clean: Option<(BaseKey, String)>,
    pub message: String,
}
impl Panel {
    pub fn view(&self) -> Option<&View> {
        self.accepted.as_ref()
    }
    pub fn running(&self) -> bool {
        self.task.is_some()
    }
    pub fn view_for(&self, base: &Base, palette: &Palette, prefs: &Preferences) -> Option<&View> {
        if !prefs.enabled
            || !prefs.overrides.iter().any(Option::is_some)
            || palette.defaults().is_none()
        {
            return None;
        }
        let colors = format!("{}:{prefs:?}", palette.render_key());
        self.wanted
            .as_ref()
            .filter(|(key, style)| key.same(&base.key()) && *style == colors)
            .and(self.accepted.as_ref())
    }
    pub fn completion_for(
        &self,
        base: &Base,
        palette: &Palette,
        prefs: &Preferences,
    ) -> Result<bool, String> {
        if !prefs.enabled
            || !prefs.overrides.iter().any(Option::is_some)
            || palette.defaults().is_none()
            || self.view_for(base, palette, prefs).is_some()
        {
            return Ok(true);
        }
        let colors = format!("{}:{prefs:?}", palette.render_key());
        if self
            .wanted
            .as_ref()
            .is_none_or(|(key, style)| !key.same(&base.key()) || *style != colors)
            || self.task.is_some()
        {
            return Ok(false);
        }
        Err(if self.message.is_empty() {
            "Map colors produced no accepted image".into()
        } else {
            self.message.clone()
        })
    }
    #[cfg(test)]
    pub fn tick(
        &mut self,
        source: Option<(Base, Arc<Palette>)>,
        prefs: &Preferences,
        ctx: &egui::Context,
    ) {
        self.tick_with_clean(source, None, prefs, ctx);
    }
    fn slot(&mut self, key: &BaseKey) -> &mut Option<Cached> {
        match key {
            BaseKey::Native(..) => &mut self.native,
            BaseKey::Composed(..) => &mut self.composed,
        }
    }
    pub fn clean_view(
        &self,
        layers: &Arc<Layers>,
        ocean: &Option<Arc<Ocean>>,
        palette: &Palette,
        prefs: &Preferences,
    ) -> Option<&View> {
        if !prefs.enabled || !prefs.overrides.iter().any(Option::is_some) {
            return None;
        }
        let key = BaseKey::Native(Arc::downgrade(layers), ocean.as_ref().map(Arc::downgrade));
        let colors = format!("{}:{prefs:?}", palette.render_key());
        if self
            .wanted
            .as_ref()
            .is_some_and(|(k, c)| k.same(&key) && *c == colors)
        {
            return self.accepted.as_ref();
        }
        self.native
            .as_ref()
            .filter(|c| c.key.0.same(&key) && c.key.1 == colors)
            .map(|c| &c.view)
    }
    pub fn tick_with_clean(
        &mut self,
        source: Option<(Base, Arc<Palette>)>,
        clean: Option<(Base, Arc<Palette>)>,
        prefs: &Preferences,
        ctx: &egui::Context,
    ) {
        let source = source.filter(|(_, p)| {
            prefs.enabled && prefs.overrides.iter().any(Option::is_some) && p.defaults().is_some()
        });
        let key = source
            .as_ref()
            .map(|(base, p)| (base.key(), format!("{}:{prefs:?}", p.render_key())));
        let clean = clean.filter(|(b, p)| {
            source
                .as_ref()
                .is_some_and(|(b, _)| matches!(b, Base::Composed(_)))
                && matches!(b, Base::Native(..))
                && p.defaults().is_some()
        });
        let clean_key = clean
            .as_ref()
            .map(|(b, p)| (b.key(), format!("{}:{prefs:?}", p.render_key())));
        let has_clean = |cache: &Option<Cached>| {
            clean_key.as_ref().is_none_or(|key| {
                cache
                    .as_ref()
                    .is_some_and(|c| c.key.0.same(&key.0) && c.key.1 == key.1)
            })
        };
        let same = match (&self.wanted, &key) {
            (None, None) => true,
            (Some((a, ka)), Some((b, kb))) => ka == kb && a.same(b),
            _ => false,
        } && (has_clean(&self.native)
            || self
                .failed_clean
                .as_ref()
                .zip(clean_key.as_ref())
                .is_some_and(|(a, b)| a.0.same(&b.0) && a.1 == b.1)
            || self
                .task
                .as_ref()
                .is_some_and(|task| match (&task.clean, &clean_key) {
                    (Some(a), Some(b)) => a.0.same(&b.0) && a.1 == b.1,
                    (None, None) => true,
                    _ => false,
                }));
        if !same {
            self.task = None;
            if let (Some(view), Some(old)) = (self.accepted.take(), self.wanted.take()) {
                let slot = self.slot(&old.0);
                *slot = Some(Cached { key: old, view });
            }
            self.pending_palette = None;
            self.failed_clean = None;
            self.wanted = key;
            self.message.clear();
            if let Some((base, palette)) = source {
                let key = self.wanted.as_ref().unwrap().clone();
                let clean_ready = has_clean(&self.native);
                if let Some(cached) = self
                    .slot(&key.0)
                    .take()
                    .filter(|c| c.key.0.same(&key.0) && c.key.1 == key.1 && clean_ready)
                {
                    self.accepted = Some(cached.view);
                    return;
                }
                let clean = clean
                    .filter(|(b, p)| {
                        matches!(base, Base::Composed(_))
                            && matches!(b, Base::Native(..))
                            && p.defaults().is_some()
                    })
                    .map(|(b, p)| {
                        let key = (b.key(), format!("{}:{prefs:?}", p.render_key()));
                        (b, Arc::new(p.with_display_colors(prefs)), key)
                    })
                    .filter(|(_, _, key)| {
                        self.native
                            .as_ref()
                            .is_none_or(|c| !c.key.0.same(&key.0) || c.key.1 != key.1)
                    });
                let palette = Arc::new(palette.with_display_colors(prefs));
                let (sender, receiver) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let paint = ctx.clone();
                let style = palette.clone();
                std::thread::spawn(move || {
                    let _loading =
                        mapper_studio::loading_progress::begin("Terrain colors", Some(signal.clone()));
                    let result = base.render(&style, &signal);
                    // A received reply must not race the compositor's exclusive
                    // ownership check, even if this thread is descheduled here.
                    drop(base);
                    let result = result.and_then(|image| {
                        let Some(mapper_studio::map_image::Rendered { image, surface }) = image else {
                            return Ok(None);
                        };
                        let clean = if let Some((base, palette, key)) = clean {
                            let image = base.render(&palette, &signal)?;
                            drop(base);
                            let Some(mapper_studio::map_image::Rendered { image, surface }) = image
                            else {
                                return Ok(None);
                            };
                            Some(CleanPaint {
                                surface,
                                key,
                                palette,
                                image,
                            })
                        } else {
                            None
                        };
                        if signal.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        Ok(Some(Painted {
                            image,
                            surface,
                            clean,
                        }))
                    });
                    let _ = sender.send(result);
                    paint.request_repaint();
                });
                self.task = Some(Task {
                    cancel,
                    receiver,
                    clean: clean_key,
                });
                self.pending_palette = Some(palette);
                self.message = "Updating map colors…".into();
            }
        }
        if !prefs.enabled || !prefs.overrides.iter().any(Option::is_some) {
            self.native = None;
            self.composed = None;
        }
        let result = self
            .task
            .as_ref()
            .and_then(|t| match t.receiver.try_recv() {
                Ok(v) => Some(v),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Map color worker ended unexpectedly".into()))
                }
            });
        if let Some(result) = result {
            let failed_clean = self.task.as_ref().and_then(|t| t.clean.clone());
            self.task = None;
            match result {
                Ok(Some(Painted {
                    image,
                    surface,
                    clean,
                })) => {
                    if let Some(CleanPaint {
                        key,
                        palette,
                        image,
                        surface,
                    }) = clean
                    {
                        let image = Arc::new(image);
                        self.native = Some(Cached {
                            key,
                            view: View {
                                surface,
                                palette,
                                texture: ctx.load_texture(
                                    "custom-clean-map-colors",
                                    image.clone(),
                                    egui::TextureOptions::LINEAR,
                                ),
                                image,
                            },
                        });
                    }
                    let image = Arc::new(image);
                    self.accepted = Some(View {
                        surface,
                        palette: self.pending_palette.take().unwrap(),
                        texture: ctx.load_texture(
                            "custom-map-colors",
                            image.clone(),
                            egui::TextureOptions::LINEAR,
                        ),
                        image,
                    });
                    self.message.clear();
                }
                Ok(None) => {
                    self.failed_clean = failed_clean;
                    self.message = "Map color update cancelled.".into();
                }
                Err(e) => {
                    self.failed_clean = failed_clean;
                    self.message = format!("Map colors: {e}");
                }
            }
        }
    }
    pub fn controls(
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        defaults: Option<&mapper_studio::map_colors::Material>,
    ) -> bool {
        let before = prefs.clone();
        ui.checkbox(&mut prefs.enabled, "Use custom map colors")
            .on_hover_text("Defaults come from the selected installation’s PaperMap material. Overrides apply to map previews and exported images.");
        let Some(defaults) = defaults else {
            ui.small("Generate an installation-backed terrain preview to load its map colors.");
            if ui
                .add_enabled(
                    prefs.enabled || prefs.overrides.iter().any(Option::is_some),
                    egui::Button::new("Restore installation defaults"),
                )
                .clicked()
            {
                *prefs = Preferences::default();
            }
            return *prefs != before;
        };
        ui.add_enabled_ui(prefs.enabled, |ui| {
            egui::Grid::new("map-color-properties")
                .min_col_width(110.)
                .show(ui, |ui| {
                    for (i, name) in mapper_studio::map_colors::NAMES.iter().enumerate() {
                        ui.label(*name);
                        let mut rgb = prefs.overrides[i].unwrap_or_else(|| {
                            std::array::from_fn(|c| {
                                (defaults.colors[i][c] * 255.).round().clamp(0., 255.) as u8
                            })
                        });
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            prefs.overrides[i] = Some(rgb);
                        }
                        if ui
                            .add_enabled(prefs.overrides[i].is_some(), egui::Button::new("Reset"))
                            .clicked()
                        {
                            prefs.overrides[i] = None;
                        }
                        ui.end_row();
                    }
                });
        });
        if ui
            .add_enabled(
                prefs.enabled || prefs.overrides.iter().any(Option::is_some),
                egui::Button::new("Restore installation defaults"),
            )
            .clicked()
        {
            *prefs = Preferences::default();
        }
        *prefs != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }
    fn material() -> mapper_studio::map_colors::Material {
        serde_json::from_value(serde_json::json!({"material_path":"assets/prefabs/tools/map/papermap.mat","serialized_file":"CAB-test","material_object":1,"shader_object":2,"shader_name":"Custom/Map","colors":(vec![[0.2,0.3,0.4,1.];11]),"sun_direction":[0.95,2.87,2.37],"sun_power":0.65,"brightness":1.05,"contrast":0.94})).unwrap()
    }
    fn tiny_layers() -> Arc<Layers> {
        Arc::new(Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 2,
            surface_resolution: 2,
            height: vec![18000; 4],
            biome: vec![0; 20],
            splat: vec![0; 32],
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        })
    }
    #[test]
    fn native_identity_tracks_water_and_map_replacements_without_retaining_their_grids() {
        let layers = tiny_layers();
        let ocean = Arc::new(Ocean {
            resolution: 2,
            classification: vec![0; 4],
            topology: vec![0; 4],
            local_water: vec![0; 4],
            ocean_cells: 0,
            enclosed_cells: 0,
            changed_topology_cells: 0,
        });
        let base = Base::Native(layers.clone(), Some(ocean.clone()));
        let key = base.key();
        assert!(key.same(&base.key()));
        assert!(!key.same(&Base::Native(layers.clone(), None).key()));
        assert!(!key.same(&Base::Native(layers.clone(), Some(Arc::new((*ocean).clone()))).key()));
        assert!(!key.same(&Base::Native(tiny_layers(), Some(ocean.clone())).key()));
        drop(base);
        assert_eq!(Arc::strong_count(&layers), 1);
        assert_eq!(Arc::strong_count(&ocean), 1);
        let weak_layers = Arc::downgrade(&layers);
        let weak_ocean = Arc::downgrade(&ocean);
        drop(layers);
        drop(ocean);
        assert!(weak_layers.upgrade().is_none() && weak_ocean.upgrade().is_none());
        assert!(!key.same(&Base::Native(tiny_layers(), None).key()));
    }
    #[test]
    fn actual_color_worker_releases_composed_grids_before_delivering_its_image() {
        let ctx = egui::Context::default();
        let mut project = mapper_studio::Project {
            size: 1000,
            ..Default::default()
        };
        let palette = crate::prefab_panel::PrefabPanel::default().seed_test_cartographic_stamp(
            &ctx,
            &mut project,
            "assets/test-monument.prefab",
            Some(0),
        );
        let mut composition = crate::composition_panel::CompositionPanel::default();
        composition.seed_test_view(&ctx, &project);
        let maps = composition.view(&project).unwrap().maps.clone();
        drop(composition);
        let mut prefs = Preferences {
            enabled: true,
            ..Default::default()
        };
        prefs.overrides[4] = Some([255, 0, 0]);
        let mut panel = Panel::default();
        panel.tick(Some((Base::Composed(maps.clone()), palette)), &prefs, &ctx);
        let result = panel
            .task
            .as_ref()
            .unwrap()
            .receiver
            .recv_timeout(std::time::Duration::from_secs(90))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.image.size, [2048, 2048]);
        assert_eq!(
            Arc::strong_count(&maps),
            1,
            "Completed repaint worker retained the composed grids"
        );
        panel.tick(None, &prefs, &ctx);
        assert!(panel.task.is_none());
    }
    #[test]
    fn composed_colors_prepare_and_reuse_clean_drag_colors_without_pinning_grids() {
        let ctx = egui::Context::default();
        let mut project = mapper_studio::Project {
            size: 1000,
            ..Default::default()
        };
        let palette = crate::prefab_panel::PrefabPanel::default().seed_test_cartographic_stamp(
            &ctx,
            &mut project,
            "assets/test-monument.prefab",
            Some(0),
        );
        let mut composition = crate::composition_panel::CompositionPanel::default();
        composition.seed_test_view(&ctx, &project);
        let maps = composition.view(&project).unwrap().maps.clone();
        drop(composition);
        let layers = Arc::new(maps.layers.clone());
        let mut prefs = Preferences {
            enabled: true,
            ..Default::default()
        };
        prefs.overrides[4] = Some([255, 0, 0]);
        let mut panel = Panel::default();
        panel.tick_with_clean(
            Some((Base::Composed(maps.clone()), palette.clone())),
            Some((Base::Native(layers.clone(), None), palette.clone())),
            &prefs,
            &ctx,
        );
        let reply = panel
            .task
            .as_ref()
            .unwrap()
            .receiver
            .recv_timeout(std::time::Duration::from_secs(90))
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(reply.clean.is_some());
        assert_eq!(Arc::strong_count(&maps), 1);
        assert_eq!(Arc::strong_count(&layers), 1);
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Some(reply))).unwrap();
        panel.task.as_mut().unwrap().receiver = receiver;
        panel.tick_with_clean(
            Some((Base::Composed(maps.clone()), palette.clone())),
            Some((Base::Native(layers.clone(), None), palette.clone())),
            &prefs,
            &ctx,
        );
        let composed = panel.view().unwrap().texture.id();
        let clean = panel
            .clean_view(&layers, &None, &palette, &prefs)
            .unwrap()
            .texture
            .id();
        assert_ne!(clean, composed);
        panel.tick(
            Some((Base::Native(layers.clone(), None), palette.clone())),
            &prefs,
            &ctx,
        );
        assert!(panel.task.is_none());
        assert_eq!(panel.view().unwrap().texture.id(), clean);
        panel.tick_with_clean(
            Some((Base::Composed(maps.clone()), palette.clone())),
            Some((Base::Native(layers.clone(), None), palette.clone())),
            &prefs,
            &ctx,
        );
        assert!(panel.task.is_none());
        assert_eq!(panel.view().unwrap().texture.id(), composed);
        assert!(
            panel
                .clean_view(&Arc::new((*layers).clone()), &None, &palette, &prefs)
                .is_none()
        );
        let mut changed = prefs.clone();
        changed.overrides[4] = Some([0, 255, 0]);
        assert!(
            panel
                .clean_view(&layers, &None, &palette, &changed)
                .is_none()
        );
        assert_eq!(Arc::strong_count(&maps), 1);
        assert_eq!(Arc::strong_count(&layers), 1);
        // A replacement clean raster must be prepared even while the composed
        // image identity stays the same. Repeated ticks must retain that job.
        let replacement = Arc::new((*layers).clone());
        panel.tick_with_clean(
            Some((Base::Composed(maps.clone()), palette.clone())),
            Some((Base::Native(replacement.clone(), None), palette.clone())),
            &prefs,
            &ctx,
        );
        assert!(panel.view().is_none());
        let cancel = panel.task.as_ref().unwrap().cancel.clone();
        panel.tick_with_clean(
            Some((Base::Composed(maps.clone()), palette.clone())),
            Some((Base::Native(replacement.clone(), None), palette.clone())),
            &prefs,
            &ctx,
        );
        assert!(Arc::ptr_eq(&cancel, &panel.task.as_ref().unwrap().cancel));
        assert!(!cancel.load(Ordering::Relaxed));
        let task = panel.task.take().unwrap();
        task.cancel.store(true, Ordering::Relaxed);
        let _ = task
            .receiver
            .recv_timeout(std::time::Duration::from_secs(90))
            .unwrap();
        drop(task);
        assert_eq!(Arc::strong_count(&maps), 1);
        assert_eq!(Arc::strong_count(&replacement), 1);
        panel.tick(None, &Preferences::default(), &ctx);
        assert!(panel.native.is_none() && panel.composed.is_none() && panel.view().is_none());
    }
    #[test]
    fn cancelled_or_failed_clean_pair_is_not_restarted_on_every_frame() {
        let ctx = egui::Context::default();
        let mut project = mapper_studio::Project {
            size: 1000,
            ..Default::default()
        };
        let palette = crate::prefab_panel::PrefabPanel::default().seed_test_cartographic_stamp(
            &ctx,
            &mut project,
            "assets/test-monument.prefab",
            Some(0),
        );
        let mut composition = crate::composition_panel::CompositionPanel::default();
        composition.seed_test_view(&ctx, &project);
        let maps = composition.view(&project).unwrap().maps.clone();
        let layers = Arc::new(maps.layers.clone());
        let mut prefs = Preferences {
            enabled: true,
            ..Default::default()
        };
        prefs.overrides[4] = Some([255, 0, 0]);
        for failure in [Ok(None), Err("Texture decode failed".to_string())] {
            let main = Base::Composed(maps.clone());
            let clean = Base::Native(layers.clone(), None);
            let colors = format!("{}:{prefs:?}", palette.render_key());
            let (sender, receiver) = mpsc::channel();
            sender.send(failure).unwrap();
            let mut panel = Panel {
                wanted: Some((main.key(), colors.clone())),
                task: Some(Task {
                    cancel: Arc::new(AtomicBool::new(false)),
                    receiver,
                    clean: Some((clean.key(), colors)),
                }),
                pending_palette: Some(Arc::new(palette.with_display_colors(&prefs))),
                ..Default::default()
            };
            panel.tick_with_clean(
                Some((main, palette.clone())),
                Some((clean, palette.clone())),
                &prefs,
                &ctx,
            );
            assert!(panel.task.is_none() && panel.view().is_none());
            let message = panel.message.clone();
            assert!(!message.is_empty());
            for _ in 0..3 {
                panel.tick_with_clean(
                    Some((Base::Composed(maps.clone()), palette.clone())),
                    Some((Base::Native(layers.clone(), None), palette.clone())),
                    &prefs,
                    &ctx,
                );
                assert!(panel.task.is_none());
                assert_eq!(panel.message, message);
            }
            panel.tick(None, &Preferences::default(), &ctx);
            assert!(panel.failed_clean.is_none());
        }
    }
    #[test]
    fn completed_color_cache_does_not_pin_the_compositors_maps() {
        let ctx = egui::Context::default();
        let mut project = mapper_studio::Project {
            size: 1000,
            ..Default::default()
        };
        let palette = crate::prefab_panel::PrefabPanel::default().seed_test_cartographic_stamp(
            &ctx,
            &mut project,
            "assets/test-monument.prefab",
            Some(0),
        );
        let mut composition = crate::composition_panel::CompositionPanel::default();
        composition.seed_test_view(&ctx, &project);
        let maps = composition.view(&project).unwrap().maps.clone();
        drop(composition);
        let height_pointer = maps.layers.height.as_ptr();
        let topology_pointer = maps.layers.topology.as_ptr();
        let mut prefs = Preferences {
            enabled: true,
            ..Default::default()
        };
        prefs.overrides[4] = Some([255, 0, 0]);
        let base = Base::Composed(maps.clone());
        let (sender, receiver) = mpsc::channel();
        let image = egui::ColorImage::filled([2, 2], egui::Color32::RED);
        sender
            .send(Ok(Some(Painted {
                surface: None,
                image: image.clone(),
                clean: None,
            })))
            .unwrap();
        let mut panel = Panel {
            wanted: Some((base.key(), format!("{}:{prefs:?}", palette.render_key()))),
            task: Some(Task {
                cancel: Arc::new(AtomicBool::new(false)),
                receiver,
                clean: None,
            }),
            pending_palette: Some(Arc::new(palette.with_display_colors(&prefs))),
            ..Default::default()
        };
        panel.tick(Some((base, palette)), &prefs, &ctx);
        assert_eq!(panel.view().unwrap().image.as_ref(), &image);
        assert!(panel.task.is_none());
        assert_eq!(
            Arc::strong_count(&maps),
            1,
            "Completed color cache retained the composed grids"
        );
        let maps =
            Arc::try_unwrap(maps).unwrap_or_else(|_| panic!("Compositor cannot reuse its map"));
        assert_eq!(maps.layers.height.as_ptr(), height_pointer);
        assert_eq!(maps.layers.topology.as_ptr(), topology_pointer);
        assert!(panel.view().is_some());
        panel.tick(None, &prefs, &ctx);
        assert!(panel.view().is_none());
    }
    #[test]
    fn restoring_defaults_cancels_and_discards_a_queued_repaint() {
        let ctx = egui::Context::default();
        let layers = Arc::new(Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 2,
            surface_resolution: 2,
            height: vec![18000; 4],
            biome: vec![0; 20],
            splat: vec![0; 32],
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        let original = layers.height.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let mut panel = Panel {
            wanted: Some((Base::Native(layers.clone(), None).key(), "previous".into())),
            task: Some(Task {
                cancel: cancel.clone(),
                receiver,
                clean: None,
            }),
            ..Default::default()
        };
        sender
            .send(Ok(Some(Painted {
                surface: None,
                image: egui::ColorImage::filled([2, 2], egui::Color32::RED),
                clean: None,
            })))
            .unwrap();
        panel.tick(None, &Preferences::default(), &ctx);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(panel.view().is_none());
        assert!(panel.task.is_none());
        assert!(panel.wanted.is_none());
        assert_eq!(layers.height, original);
        assert_eq!(Arc::strong_count(&layers), 1);
    }
    #[test]
    fn reset_control_restores_source_defaults_without_recipe_edits() {
        let ctx = egui::Context::default();
        let defaults = material();
        let mut prefs = Preferences {
            enabled: true,
            ..Default::default()
        };
        prefs.overrides[4] = Some([255, 0, 0]);
        let project = mapper_studio::Project::default();
        let recipe = project.clone();
        let mut point = None;
        let mut draw = |events| {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400., 800.),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| {
                    Panel::controls(ui, &mut prefs, Some(&defaults));
                },
            );
            for shape in &frame.shapes {
                if let egui::Shape::Text(t) = &shape.shape
                    && t.galley.job.text == "Restore installation defaults"
                {
                    point = Some(t.pos + t.galley.size() * 0.5);
                }
            }
            frame.textures_delta.clear();
        };
        draw(vec![]);
        let point = point.unwrap();
        for pressed in [true, false] {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400., 800.),
                    )),
                    events: vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    focused: true,
                    ..Default::default()
                },
                |ui| {
                    Panel::controls(ui, &mut prefs, Some(&defaults));
                },
            );
            frame.textures_delta.clear();
        }
        assert_eq!(prefs, Preferences::default());
        assert_eq!(project, recipe);
    }
    #[test]
    fn color_controls_fit_narrow_settings_in_both_themes_and_handle_missing_source() {
        for dark in [false, true] {
            for available in [false, true] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ctx.all_styles_mut(|s| {
                    s.spacing.item_spacing = egui::vec2(8., 8.);
                    for kind in [egui::TextStyle::Body, egui::TextStyle::Button] {
                        s.text_styles.insert(kind, egui::FontId::proportional(14.));
                    }
                    s.text_styles
                        .insert(egui::TextStyle::Small, egui::FontId::proportional(12.));
                });
                let mut prefs = Preferences {
                    enabled: true,
                    ..Default::default()
                };
                prefs.overrides[4] = Some([72, 119, 46]);
                let before = prefs.clone();
                let defaults = material();
                let size = [340, 720];
                let mut textures = snapshot::Textures::default();
                let mut last = None;
                for _ in 0..2 {
                    let mut frame = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(340., 720.),
                            )),
                            focused: true,
                            ..Default::default()
                        },
                        |ui| {
                            ui.set_max_width(324.);
                            Panel::controls(ui, &mut prefs, available.then_some(&defaults));
                        },
                    );
                    textures.update(&frame.textures_delta);
                    frame.textures_delta.clear();
                    last = Some(frame);
                }
                let frame = last.unwrap();
                for shape in &frame.shapes {
                    if let egui::Shape::Text(t) = &shape.shape {
                        assert!(
                            t.pos.x + t.galley.size().x <= 340.,
                            "Overflows settings: {}",
                            t.galley.job.text
                        );
                        assert!(t.pos.y + t.galley.size().y <= 720., "Vertical overflow");
                    }
                }
                assert_eq!(prefs, before);
                if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                    let path = std::path::Path::new(&dir).join(format!(
                        "map-colors-{}-{}.png",
                        if dark { "dark" } else { "light" },
                        if available { "ready" } else { "unavailable" }
                    ));
                    assert!(!path.exists());
                    let image = textures.raster(&ctx, frame, size);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
            }
        }
    }
}
