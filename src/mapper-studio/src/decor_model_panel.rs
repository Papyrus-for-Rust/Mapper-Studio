use super::*;
use mapper_studio::{
    decor_models, monument_source::Model, prefab_assets::Catalog, prefab_textures::Bindings,
};
use papyrus_terrain::monument_composition::Composed;
use std::sync::{
    Arc, Weak,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};

#[derive(Default)]
struct SceneryInstances {
    maps: Weak<Composed>,
    instances: Vec<SceneryInstance>,
}

impl SceneryInstances {
    fn refresh(&mut self, maps: &Arc<Composed>, build: impl FnOnce() -> Vec<SceneryInstance>) {
        // The weak reference keeps the allocation identity valid without
        // retaining the map or its layers.
        if !std::ptr::eq(self.maps.as_ptr(), Arc::as_ptr(maps)) {
            self.maps = Arc::downgrade(maps);
            self.instances = build();
        }
    }
}

struct SceneryInstance {
    model: Arc<Model>,
    position: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}
#[derive(Clone, PartialEq)]
struct Key {
    catalog: String,
    bindings: String,
    source: String,
    vines: Option<String>,
    road_source: Option<String>,
}
struct Task {
    key: Key,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<decor_models::World>, String>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct DecorModelsPanel {
    key: Option<Key>,
    task: Option<Task>,
    source: Option<decor_models::World>,
    scenery: SceneryInstances,
    message: String,
    attempted: bool,
}
impl DecorModelsPanel {
    pub fn instances<'a>(
        &'a mut self,
        maps: Option<&Arc<Composed>>,
    ) -> impl Iterator<Item = terrain_3d::ModelInstance<'a>> {
        let source = self.source.as_ref().zip(self.key.as_ref())
            .filter(|(source, key)| source.covers_base(&key.source))
            .map(|(source, _)| source);
        if let Some((source, maps)) = source.zip(maps) {
            self.scenery.refresh(maps, || {
                source
                    .composition_instances(maps)
                    .into_iter()
                    .map(|instance| SceneryInstance {
                        model: instance.model.clone(),
                        position: instance.pose.position,
                        rotation: instance.pose.rotation,
                        scale: instance.pose.scale,
                    })
                    .collect()
            });
        } else {
            self.scenery = SceneryInstances::default();
        }
        self.scenery
            .instances
            .iter()
            .map(|instance| terrain_3d::ModelInstance {
                model: terrain_3d::Geometry::Static(&instance.model),
                position: instance.position,
                rotation: instance.rotation,
                scale: instance.scale,
                tint: None,
            })
    }

    pub fn tick(
        &mut self,
        project: &Project,
        catalog: Option<Arc<Catalog>>,
        bindings: Option<Arc<Bindings>>,
        visible: bool,
        ctx: &egui::Context,
    ) {
        if let Some((catalog, bindings)) = catalog.as_deref().zip(bindings.as_deref()) {
            if self.source.as_ref().is_some_and(|source| !source.matches_installation(catalog, bindings)) {
                self.source = None;
                self.scenery = SceneryInstances::default();
            }
            if self.task.as_ref().is_some_and(|task| {
                task.key.catalog != catalog.sha256 || task.key.bindings != bindings.sha256
            }) {
                self.task = None;
            }
        }
        let inputs = project.decoration.as_ref().zip(catalog).zip(bindings);
        let vines = project.objects.as_ref().and_then(|objects| {
            objects
                .jungle_vines_source_sha256
                .as_ref()
                .map(|pin| (objects, pin))
        });
        let key = inputs.as_ref().map(|((r, c), b)| Key {
            catalog: c.sha256.clone(),
            bindings: b.sha256.clone(),
            source: r.source_sha256.clone(),
            vines: vines.map(|(_, pin)| pin.clone()),
            road_source: vines.map(|(objects, _)| objects.road_source_sha256.clone()),
        });
        if self.key != key {
            self.scenery = SceneryInstances::default();
            self.message.clear();
            self.attempted = false;
            self.key = key.clone();
        }
        let result = self.task.as_ref().and_then(|task| match task.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("Decoration model worker ended".into())),
        });
        if let Some(result) = result {
            let task = self.task.take().unwrap();
            if !task.cancel.load(Ordering::Relaxed) {
                match result {
                    Ok(Some(source)) => {
                        self.source = Some(source);
                        self.scenery = SceneryInstances::default();
                        self.message.clear();
                    }
                    Ok(None) if self.key.as_ref() == Some(&task.key) => {
                        self.message = "Model loading cancelled".into();
                    }
                    Err(error) if self.key.as_ref() == Some(&task.key) => self.message = error,
                    _ => {}
                }
            }
        }
        if self.key.is_some() && self.source.as_ref().is_some_and(|source| source.covers(project)) {
            self.attempted = true;
        }
        if visible && !self.attempted && self.task.is_none()
            && let Some((((_, catalog), bindings), key)) = inputs.zip(key)
        {
            self.attempted = true;
            let project = project.clone();
            let previous = self.source.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let repaint = ctx.clone();
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                let _loading =
                    mapper_studio::loading_progress::begin("Scenery models", Some(signal.clone()));
                let result = decor_models::prepare_project_reusing(
                    &project, &catalog, &bindings, previous.as_ref(),
                    || signal.load(Ordering::Relaxed),
                );
                let _ = sender.send(result);
                repaint.request_repaint();
            });
            self.task = Some(Task { key, cancel, receiver });
            self.message = "Loading scenery previews…".into();
        }
    }
    pub fn loading_status(&self) -> Option<&str> {
        self.task.as_ref().map(|_| "Loading scenery previews…")
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        if self.key.is_none() || !self.attempted {
            return;
        }
        if let Some(source) = self.source.as_ref().filter(|source| {
            self.key.as_ref().is_some_and(|key| source.covers_base(&key.source))
        }) {
            let (nonempty, total) = source.model_counts();
            ui.small(format!("{nonempty}/{total} scenery previews"))
            .on_hover_text("Authored geometry with simplified, untextured LODs. Runtime child variants and physics require game validation.");
        } else {
            let message = if self.task.is_some() {
                "Loading scenery previews…"
            } else {
                "Scenery previews unavailable"
            };
            ui.small(message).on_hover_text(&self.message);
        }
        if let Some(task) = &self.task {
            ui.spinner();
            if ui.button("Cancel model loading").clicked() {
                task.cancel.store(true, Ordering::Relaxed);
            }
        } else if ui.button("Reload scenery previews").clicked() {
            self.attempted = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenery_lookup_reuses_the_same_composition_and_releases_stale_maps() {
        let ctx = egui::Context::default();
        let project = Project {
            size: 1000,
            composition_engine: Some(
                papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into(),
            ),
            ..Default::default()
        };
        let mut composition = composition_panel::CompositionPanel::default();
        composition.seed_test_view(&ctx, &project);
        let first = composition.view(&project).unwrap().maps.clone();

        let mut scenery = SceneryInstances::default();
        let mut builds = 0;
        scenery.refresh(&first, || {
            builds += 1;
            Vec::new()
        });
        scenery.refresh(&first, || {
            panic!("Scenery models resolved again for the same map")
        });
        assert_eq!(builds, 1);

        composition.seed_test_view(&ctx, &project);
        drop(first);
        assert!(scenery.maps.upgrade().is_none());
        let next = &composition.view(&project).unwrap().maps;
        scenery.refresh(next, || {
            builds += 1;
            Vec::new()
        });
        assert_eq!(builds, 2);
        assert!(Arc::ptr_eq(&scenery.maps.upgrade().unwrap(), next));
    }

    #[test]
    fn bound_scenery_waits_for_3d_and_attempts_each_source_once() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let _assets = app.prefabs.seed_test_verified_stamp(&ctx, &mut app.project);
        app.project.decoration = Some(mapper_studio::decor_source::Recipe::new("f".repeat(64)));
        let catalog = app.prefabs.catalog(&app.project);
        let bindings = app.prefabs.bindings(&app.project);
        let mut panel = DecorModelsPanel::default();
        for (visible, attempted) in [
            (false, false),
            (false, false),
            (true, true),
            (false, true),
            (true, true),
        ] {
            panel.tick(
                &app.project,
                catalog.clone(),
                bindings.clone(),
                visible,
                &ctx,
            );
            assert!(panel.key.is_some());
            assert_eq!(panel.attempted, attempted);
            if !attempted {
                assert!(panel.task.is_none());
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while panel.task.is_some() {
            panel.tick(&app.project, catalog.clone(), bindings.clone(), true, &ctx);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panel.tick(&app.project, catalog, bindings, true, &ctx);
        assert!(panel.task.is_none());
    }
    #[test]
    fn missing_preview_status_fits_compact_settings_in_both_themes() {
        for light in [false, true] {
            for width in [210., 290.] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if light {
                    egui::Visuals::light()
                } else {
                    egui::Visuals::dark()
                });
                let mut panel = DecorModelsPanel {
                    key: Some(Key {
                        catalog: "a".repeat(64),
                        bindings: "b".repeat(64),
                        source: "c".repeat(64),
                        vines: Some("d".repeat(64)),
                        road_source: Some("e".repeat(64)),
                    }),
                    attempted: true,
                    message: "Source validation failed for a deeply nested installation path"
                        .repeat(20),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(Default::default(), |ui| {
                    ui.set_width(width);
                    let before = ui.cursor().min;
                    panel.show(ui);
                    assert!(ui.min_rect().max.x <= before.x + width + 1.);
                    assert!(ui.cursor().min.y - before.y < 70.);
                });
                assert!(panel.message.len() > 1000);
                let button = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text)
                            if text.galley.text() == "Reload scenery previews" =>
                        {
                            Some(text.pos + text.galley.size() * 0.5)
                        }
                        _ => None,
                    })
                    .unwrap();
                output.textures_delta.clear();
                let mut clicked = ctx.run_ui(
                    egui::RawInput {
                        events: vec![
                            egui::Event::PointerMoved(button),
                            egui::Event::PointerButton {
                                pos: button,
                                button: egui::PointerButton::Primary,
                                pressed: true,
                                modifiers: egui::Modifiers::NONE,
                            },
                            egui::Event::PointerButton {
                                pos: button,
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        ..Default::default()
                    },
                    |ui| {
                        ui.set_width(width);
                        panel.show(ui);
                    },
                );
                clicked.textures_delta.clear();
                assert!(!panel.attempted);
            }
        }
    }
}
