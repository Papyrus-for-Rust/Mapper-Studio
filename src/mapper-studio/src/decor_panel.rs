use super::*;
use mapper_studio::{decor_source::Source, prefab_assets::Catalog};
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
#[derive(Clone, PartialEq, Eq)]
struct Key {
    root: PathBuf,
    catalog: String,
    pin: Option<String>,
}
struct Task {
    key: Key,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Arc<Source>, String>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct DecorPanel {
    wanted: Option<Key>,
    task: Option<Task>,
    source: Option<Arc<Source>>,
    message: String,
    live_inputs: Option<mapper_studio::object_preparation::Inputs>,
    live_task: mapper_studio::object_preparation::Preparation,
    live_message: String,
    live_error: Option<String>,
    pending_decoration: Option<(Project, mapper_studio::decor_source::Recipe)>,
    prepared_cliffs: Option<mapper_studio::client_preparation::ReadyCliffs>,
}
#[derive(Default)]
pub(super) struct RecipeWork {
    live_inputs: Option<mapper_studio::object_preparation::Inputs>,
    live_task: mapper_studio::object_preparation::Preparation,
    live_message: String,
    live_error: Option<String>,
    pending_decoration: Option<(Project, mapper_studio::decor_source::Recipe)>,
}
impl RecipeWork {
    pub(super) fn readiness(&self) -> Result<(), String> {
        recipe_readiness(self.live_error.is_some(), self.pending_decoration.is_some() || self.live_task.running())
    }
}

fn recipe_readiness(failed: bool, pending: bool) -> Result<(), String> {
    if failed {
        Err("Scenery setup failed. Retry or disable it in Scenery & objects.".into())
    } else if pending {
        Err("Preparing scenery placement".into())
    } else {
        Ok(())
    }
}

impl DecorPanel {
    pub(super) fn swap_recipe_work(&mut self, work: &mut RecipeWork) {
        std::mem::swap(&mut self.live_inputs, &mut work.live_inputs);
        std::mem::swap(&mut self.live_task, &mut work.live_task);
        std::mem::swap(&mut self.live_message, &mut work.live_message);
        std::mem::swap(&mut self.live_error, &mut work.live_error);
        std::mem::swap(&mut self.pending_decoration, &mut work.pending_decoration);
    }

    pub fn accept_cliffs(
        &mut self,
        prepared: Option<mapper_studio::client_preparation::ReadyCliffs>,
    ) {
        self.prepared_cliffs = prepared;
    }
    pub fn bind_cliffs(
        &self,
        project: &mut Project,
        root: &std::path::Path,
        catalog: &str,
    ) -> bool {
        self.prepared_cliffs
            .as_ref()
            .is_some_and(|ready| ready.apply(project, root, catalog))
    }
    pub fn readiness(&self) -> Result<(), String> {
        recipe_readiness(self.live_error.is_some(), self.pending_decoration.is_some() || self.live_task.running())
    }
    pub fn accept_prepared(&mut self, project: &Project, root: PathBuf, catalog: &Catalog, source: Arc<Source>) {
        let pin = project.decoration.as_ref().map(|r| r.source_sha256.clone());
        if pin.as_ref().is_some_and(|pin| pin != &source.sha256) { return; }
        self.task = None;
        self.wanted = Some(Key { root, catalog: catalog.sha256.clone(), pin });
        self.source = Some(source);
        self.message.clear();
    }
    pub fn tick(
        &mut self,
        project: &Project,
        root: Option<PathBuf>,
        catalog: Option<Arc<Catalog>>,
        bindings: Option<Arc<mapper_studio::prefab_textures::Bindings>>,
        ctx: &egui::Context,
    ) {
        self.tick_recipe_work(project, root.as_deref(), catalog.clone(), bindings, ctx);

        let inputs = root
            .zip(catalog)
            .filter(|_| mapper_studio::native_preview::is_native(&project.preview_engine));
        let key = inputs.as_ref().map(|(root, c)| Key {
            root: root.clone(),
            catalog: c.sha256.clone(),
            pin: project.decoration.as_ref().map(|r| r.source_sha256.clone()),
        });
        if self.task.is_none() && let (Some(old), Some(next), Some(source)) = (&self.wanted, &key, &self.source)
            && old.root == next.root && old.catalog == next.catalog
            && next.pin.as_ref().is_none_or(|pin| pin == &source.sha256)
        {
            self.wanted = key.clone();
        }
        if self.wanted != key {
            self.task = None;
            self.source = None;
            self.message.clear();
            self.wanted = key.clone();
            if let Some(((root, catalog), key)) = inputs.zip(key) {
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let pin = key.pin.clone();
                let (sender, receiver) = mpsc::channel();
                let repaint = ctx.clone();
                std::thread::spawn(move || {
                    let _verification = mapper_studio::installation::VerificationScope::default();
            let _loading = mapper_studio::loading_progress::begin("Scenery data", Some(signal.clone()));
                    let result = mapper_studio::object_extract::prepare_decor_shared(
                        &root,
                        &catalog,
                        pin.as_deref(),
                        || signal.load(Ordering::Relaxed),
                    );
                    let _ = sender.send(result);
                    repaint.request_repaint();
                });
                self.task = Some(Task {
                    key,
                    cancel,
                    receiver,
                });
            }
        }
        let result = self
            .task
            .as_ref()
            .and_then(|t| match t.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Decoration source worker ended".into()))
                }
                Err(TryRecvError::Empty) => None,
            });
        if let Some(result) = result {
            let task = self.task.take().unwrap();
            if self.wanted.as_ref() == Some(&task.key) && !task.cancel.load(Ordering::Relaxed) {
                match result {
                    Ok(source) => self.source = Some(source),
                    Err(e) => self.message = e,
                }
            }
        }
    }
    pub(super) fn tick_recipe_work(
        &mut self,
        project: &Project,
        root: Option<&std::path::Path>,
        catalog: Option<Arc<Catalog>>,
        bindings: Option<Arc<mapper_studio::prefab_textures::Bindings>>,
        ctx: &egui::Context,
    ) {
        if self
            .pending_decoration
            .as_ref()
            .is_some_and(|(before, _)| before != project)
        {
            self.pending_decoration = None;
            self.live_task.cancel();
            self.live_error = None;
            self.live_message =
                "Decoration preparation cancelled because the recipe changed.".into();
        }
        let live_inputs = root.and_then(|root| {
            catalog
                .zip(bindings)
                .filter(|(_, bindings)| bindings.installation_root() == root)
                .and_then(|(catalog, bindings)| {
                    mapper_studio::object_preparation::Inputs::refresh(
                        self.live_inputs.take(),
                        project,
                        catalog,
                        bindings,
                        self.pending_decoration.as_ref().map(|(_, decoration)| decoration),
                    )
                })
        });
        if self.live_task.discard_changed(live_inputs.as_ref()) {
            self.pending_decoration = None;
            self.live_error = None;
            self.live_message = "Verification canceled because the recipe or installation changed. Enable shared placement again when ready.".into();
        }
        self.live_inputs = live_inputs;
        if self.pending_decoration.is_some() && !self.live_task.running() && self.live_error.is_none() {
            if let Some(inputs) = self
                .live_inputs
                .as_ref()
                .and_then(|i| i.with_project(project))
            {
                let repaint = ctx.clone();
                self.live_task
                    .start(inputs, move || repaint.request_repaint());
            } else {
                self.pending_decoration = None;
                self.live_message="Verified installation data became unavailable. Enable decoration again when ready.".into();
            }
        }

    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &mut Project,
        pins: Option<mapper_studio::AssetIdentity>,
    ) {
        let native = mapper_studio::native_preview::is_native(&project.preview_engine);
        let enabled = project.decoration.is_some() || self.pending_decoration.is_some();
        let ready = native && self.source.is_some() && pins.is_some() && project.composition_engine.is_some();
        let ctx = ui.ctx().clone();
        let (header, changed) = super::panels::recipe_section(
            ui, "Scenery & objects", enabled, enabled || ready, false,
            "Place scenery and roadside objects using installation data.",
        );
        if let Some(enabled) = changed {
            self.live_error = None;
            if enabled {
                let mut next = project.clone();
                if let Some(pins) = pins {
                    if let Err(e) =
                        next.check_monument_assets(&pins.catalog_sha256, &pins.bindings_sha256)
                    {
                        self.message = e;
                        header.body(|ui| ui.small(&self.message));
                        return;
                    }
                    next.monument_assets.get_or_insert(pins);
                }
                next.decoration = self.source.as_deref().map(Source::recipe);
                if (next.roads.is_some() || next.rails.is_some()) && next.objects.is_none() {
                    self.pending_decoration = next.decoration.clone().map(|r| (project.clone(), r));
                    self.live_message="Preparing roadside and scenery placement…".into();
                    ctx.request_repaint();
                } else {
                    match next.validate() {
                        Ok(()) => *project = next,
                        Err(e) => self.message = e,
                    }
                }
            } else {
                self.pending_decoration = None;
                project.decoration = None;
                project.objects = None;
                self.live_task.cancel();
            }
        }
        header.body(|ui| self.scenery_settings(ui, project));
    }

    fn scenery_settings(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        if !mapper_studio::native_preview::is_native(&project.preview_engine) {
            ui.weak("Scenery requires native terrain");
            return;
        }
        let enabled = project.decoration.is_some() || self.pending_decoration.is_some();
        if let Some(source) = &self.source {
            ui.small(format!(
                "{} stages",
                source.stages.len()
            )).on_hover_text("Uses the recipe's Decoration seed.");
            if enabled
                && [
                    mapper_studio::decor_source::HEIGHT_VERSION,
                    mapper_studio::decor_source::COLLIDER_VERSION,
                ]
                .contains(&source.recipe().engine.as_str())
                && project
                    .decoration
                    .as_ref()
                    .is_some_and(|r| r.engine != source.recipe().engine)
                && ui
                    .button("Use current decoration terrain adapter")
                    .clicked()
            {
                project.decoration = Some(source.recipe());
            }
        } else if self.task.is_some() {
            ui.spinner();
            ui.small("Preparing installation decoration data…");
            if ui.button("Cancel decoration preparation").clicked() {
                self.task = None;
                self.message = "Decoration preparation cancelled.".into();
            }
        } else if !self.message.is_empty() {
            ui.small(&self.message);
            if self.wanted.is_some() && ui.button("Retry decoration preparation").clicked() {
                self.wanted = None;
            }
        }
        self.live_controls(ui, project);
    }
    fn apply_live(
        &mut self,
        project: &mut Project,
        recipe: mapper_studio::object_generation::Recipe,
    ) -> Result<Project, String> {
        let mut next = project.clone();
        if let Some((before, decoration)) = &self.pending_decoration {
            if before != project {
                return Err("Recipe changed during decoration preparation".into());
            }
            next.decoration = Some(decoration.clone());
        }
        next.objects = Some(recipe);
        next.validate()?;
        self.pending_decoration = None;
        Ok(std::mem::replace(project, next))
    }
    pub fn poll_live(&mut self, project: &mut Project, ctx: &egui::Context) -> Option<Project> {
        if !self.live_task.running() {
            return None;
        }
        use mapper_studio::object_preparation::Event;
        let mut previous = None;
        let inputs = self
            .live_inputs
            .as_ref()
            .and_then(|i| i.with_project(project));
        if let Some(event) = self.live_task.poll(inputs.as_ref()) {
            match event {
                Event::Ready(recipe) => match self.apply_live(project, recipe) {
                    Ok(before) => {
                        previous = Some(before);
                        ctx.request_repaint();
                        self.live_error = None;
                        self.live_message = "Scenery placement ready".into();
                    }
                    Err(error) => self.live_error = Some(error),
                },
                Event::Failed(error) => {
                    self.live_error = Some(error);
                    self.live_message.clear();
                }
                Event::Changed => {
                    self.pending_decoration = None;
                    self.live_error = None;
                    self.live_message = "Verification canceled because the recipe changed. Enable shared placement again when ready.".into();
                }
            }
        }
        if self.live_task.running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        previous
    }
    fn live_controls(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        let inputs = self
            .live_inputs
            .as_ref()
            .and_then(|i| i.with_project(project));
        ui.add_space(6.);
        let mut enabled = project.objects.is_some() || self.live_task.running() || self.live_error.is_some();
        if ui.add_enabled(enabled || inputs.is_some(), egui::Checkbox::new(&mut enabled, "Roadside objects"))
            .on_hover_text("Uses the installation's stage order, current terrain and monument exclusion areas. Terrain changes are shared with scenery. Complete prefab assembly and game validation remain in development.").changed() {
            self.live_error = None;
            if enabled {
                if let Some(inputs) = inputs.as_ref().and_then(|inputs| inputs.with_project(project)) {
                    let repaint = ui.ctx().clone();
                    self.live_message.clear();
                    self.live_task.start(inputs, move || repaint.request_repaint());
                }
            } else {
                self.live_task.cancel();
                if self.pending_decoration.take().is_some() {
                    self.live_message="Decoration preparation cancelled; existing rails and recipe retained.".into();return;
                }
                project.objects = None;
                let had_rails=project.rails.take().is_some();
                self.live_message = if had_rails{"Shared placement and the dependent rail recipe disabled; batch decoration is active."}else{"Shared placement disabled; the saved batch decoration mode is active."}.into();
            }
        }
        if self.live_task.running() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Preparing and verifying placement data…");
            });
            if ui.button("Cancel verification").clicked() {
                self.live_task.cancel();
                self.pending_decoration = None;
                self.live_message = "Verification canceled; placement settings were kept.".into();
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        } else if project.objects.is_none() && self.live_inputs.is_none() {
            ui.small("Enable source-backed roads, decoration and monument checks to use shared placement.");
        }
        if !self.live_message.is_empty() {
            ui.small(&self.live_message);
        }
        if let Some(error) = &self.live_error {
            ui.colored_label(ui.visuals().warn_fg_color, "Scenery setup failed")
                .on_hover_text(error);
            if ui.add_enabled(inputs.is_some(), egui::Button::new("Retry verification")).clicked()
                && let Some(inputs) = inputs
            {
                self.live_error = None;
                let repaint = ui.ctx().clone();
                self.live_task.start(inputs, move || repaint.request_repaint());
            }
        }
    }
}

#[cfg(test)]
#[path = "decor_panel/performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
mod live_tests {
    use super::*;
    #[test]
    fn compact_scenery_controls_preserve_status_and_fit_narrow_docks() {
        mod snapshot {
            include!("../examples/support/egui_snapshot.rs");
        }
        for dark in [false, true] {
            for width in [210, 290] {
                for failed in [false, true] {
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    workbench_style::install(&ctx);
                    let mut project = rail_project();
                    project.decoration = Some(decoration());
                    project.objects = Some(mapper_studio::object_generation::Recipe::new(
                        "3".repeat(64),
                    ));
                    let before = project.clone();
                    let mut panel = DecorPanel::default();
                    if failed {
                        panel.live_error = Some("Fixture verification failed".into());
                    }
                    let mut textures = snapshot::Textures::default();
                    let mut last = None;
                    for _ in 0..2 {
                        let mut out = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    egui::vec2(width as f32, 260.),
                                )),
                                ..Default::default()
                            },
                            |ui| {
                                ui.set_max_width(width as f32 - 16.);
                                crate::panels::open_recipe_section(ui, "Scenery & objects");
                                let pins = project.monument_assets.clone();
                                panel.show(ui, &mut project, pins);
                            },
                        );
                        textures.update(&out.textures_delta);
                        out.textures_delta.clear();
                        last = Some(out);
                    }
                    let out = last.unwrap();
                    let mut labels = Vec::new();
                    for shape in &out.shapes {
                        if let egui::Shape::Text(t) = &shape.shape {
                            let rect = t.galley.rect.translate(t.pos.to_vec2());
                            assert!(
                                rect.max.x <= width as f32 && shape.clip_rect.contains_rect(rect),
                                "Clipped scenery text: {}",
                                t.galley.job.text
                            );
                            labels.push(t.galley.job.text.as_str());
                        }
                    }
                    for label in ["Scenery & objects", "Roadside objects"] {
                        assert!(labels.contains(&label));
                    }
                    assert!(!labels.contains(&"Placement mode"));
                    assert_eq!(labels.contains(&"Scenery setup failed"), failed);
                    assert_eq!(labels.contains(&"Retry verification"), failed);
                    assert_eq!(project, before);
                    if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                        let image = textures.raster(&ctx, out, [width, 260]);
                        mapper_studio::image_export::write_png(
                            &std::path::Path::new(&dir).join(format!(
                                "scenery-{}-{width}-{}.png",
                                if dark { "dark" } else { "light" },
                                if failed { "failed" } else { "ready" }
                            )),
                            &image,
                            || false,
                        )
                        .unwrap();
                    }
                }
            }
        }
    }
    #[test]
    fn invalid_completion_preserves_the_requested_scenery_for_retry() {
        let mut project = rail_project();
        let before = project.clone();
        let mut panel = DecorPanel {
            pending_decoration: Some((before.clone(), decoration())),
            ..Default::default()
        };
        assert!(
            panel
                .apply_live(
                    &mut project,
                    mapper_studio::object_generation::Recipe::new("invalid".into())
                )
                .is_err()
        );
        assert_eq!(project, before);
        assert!(panel.pending_decoration.is_some());
        panel
            .apply_live(
                &mut project,
                mapper_studio::object_generation::Recipe::new("3".repeat(64)),
            )
            .unwrap();
        assert!(panel.pending_decoration.is_none());
        assert_eq!(project.decoration, Some(decoration()));
        project.validate().unwrap();
    }
    #[test]
    fn failed_verification_remains_visible_and_can_be_disabled() {
        let directory = tempfile::tempdir().unwrap();
        let (mut panel, mut project, catalog, bindings) =
            super::performance_tests::inputs(directory.path(), None);
        project.rails = None;
        project.objects = None;
        let before = project.clone();
        let ctx = egui::Context::default();
        panel.tick(
            &project,
            Some(directory.path().to_owned()),
            Some(catalog.clone()),
            Some(bindings.clone()),
            &ctx,
        );
        click(&ctx, &mut panel, &mut project, "Roadside objects");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while panel.live_task.running() && std::time::Instant::now() < deadline {
            assert!(panel.poll_live(&mut project, &ctx).is_none());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(panel.live_error.is_some());
        assert!(panel.readiness().is_err());
        assert_eq!(project, before);
        panel.tick(
            &project,
            Some(directory.path().to_owned()),
            Some(catalog),
            Some(bindings),
            &ctx,
        );
        assert!(!panel.live_task.running());
        click(&ctx, &mut panel, &mut project, "Retry verification");
        assert!(panel.live_task.running());
        assert!(panel.readiness().is_err());
        click(&ctx, &mut panel, &mut project, "Roadside objects");
        assert!(!panel.live_task.running());
        assert!(panel.poll_live(&mut project, &ctx).is_none());
        assert!(panel.live_error.is_none());
        assert!(panel.readiness().is_ok());
        assert_eq!(project, before);
    }
    #[test]
    fn pending_scenery_prevents_generation_from_using_the_previous_recipe() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        let before = app.project.clone();
        app.decoration_panel.pending_decoration = Some((before.clone(), decoration()));
        app.begin_generation(
            mapper_studio::generation_plan::BatchPlan::exact(before.clone()),
            &ctx,
        );
        assert!(!app.generation.busy());
        assert!(app.task.is_none());
        assert_eq!(app.project, before);
        assert_eq!(app.status, "Preparing scenery placement");
        let mut other_recipe = before;
        other_recipe.master_seed += 1;
        other_recipe.refresh_inherited_seeds();
        app.begin_generation(
            mapper_studio::generation_plan::BatchPlan::exact(other_recipe.clone()),
            &ctx,
        );
        assert!(app.generation.busy());
        assert_eq!(app.project, other_recipe);
        app.cancel_generation();
        app.decoration_panel.pending_decoration = None;
        assert!(app.decoration_panel.readiness().is_ok());
    }
    pub(super) fn rail_project() -> Project {
        let project = Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            composition_engine: Some(papyrus_terrain::monument_composition::VERSION.into()),
            installation: Some(mapper_studio::installation::Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: mapper_studio::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            monument_assets: Some(mapper_studio::AssetIdentity {
                catalog_sha256: "b".repeat(64),
                bindings_sha256: "c".repeat(64),
            }),
            monument_rules: Some(mapper_studio::monument_source::Recipe {
                engine: mapper_studio::monument_source::VERSION.into(),
                source_sha256: "d".repeat(64),
                coastal_profile: None,
                tier_search: None,
                earthwork: None,
                coastal_normals: None,
                coastal_navigation: None,
                coastal_search: None,
                canonical_yaw: None,
                land_alignment: None,
                surface_modifiers: None,
                stage_rules: None,
                dungeon_source: None, offshore_source: None,
            }),
            roads: Some(mapper_studio::infrastructure::Recipe::new("e".repeat(64))),
            rails: Some(mapper_studio::rail_generation::Recipe {
                engine: mapper_studio::rail_generation::VERSION.into(),
                source_sha256: "f".repeat(64),
                mesh_sha256: "1".repeat(64),
                settings: mapper_studio::rail_generation::initial_settings(7),
            }),
            ..Default::default()
        };
        project.validate().unwrap();
        project
    }
    fn decoration() -> mapper_studio::decor_source::Recipe {
        mapper_studio::decor_source::Recipe {
            engine: mapper_studio::decor_source::COLLIDER_VERSION.into(),
            source_sha256: "2".repeat(64),
        }
    }
    fn render(
        ctx: &egui::Context,
        panel: &mut DecorPanel,
        project: &mut Project,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let pins = project.monument_assets.clone();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(640., 900.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                crate::panels::open_recipe_section(ui, "Scenery & objects");
                panel.show(ui, project, pins.clone());
            },
        );
        output.textures_delta.clear();
        output
    }
    fn click(ctx: &egui::Context, panel: &mut DecorPanel, project: &mut Project, label: &str) {
        let output = render(ctx, panel, project, vec![]);
        let pos = if label == "Scenery & objects" {
            crate::panels::recipe_checkbox_position(&output, label)
        } else {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            }).unwrap_or_else(|| panic!("Missing decoration control {label}"))
        };
        for pressed in [true, false] {
            render(
                ctx,
                panel,
                project,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn enabling_scenery_with_roads_prepares_shared_placement_without_requiring_rails() {
        let ctx = egui::Context::default();
        let mut project = rail_project();
        project.rails = None;
        let before = project.clone();
        let mut source: Source = serde_json::from_value(serde_json::json!({
            "format":"papyrus-decor-source", "schema":4,
            "adapter":"papyrus-static-decor-collider-4", "scene":"fixture",
            "sources":[], "extractor_sha256":"a".repeat(64),
            "stages":[], "prefabs":{}, "issues":[]
        })).unwrap();
        source.sha256 = "2".repeat(64);
        let mut panel = DecorPanel {source:Some(Arc::new(source)), ..Default::default()};
        click(&ctx,&mut panel,&mut project,"Scenery & objects");
        assert_eq!(project,before);
        assert_eq!(panel.pending_decoration.as_ref().unwrap().1,decoration());
        panel.apply_live(&mut project,mapper_studio::object_generation::Recipe::new("3".repeat(64))).unwrap();
        assert!(project.rails.is_none());
        assert_eq!(project.objects.as_ref().unwrap().engine,mapper_studio::object_generation::VERSION);
        assert_eq!(project.decoration,Some(decoration()));
        project.validate().unwrap();
    }
    #[test]
    fn deferred_decoration_and_objects_commit_together_and_undo_preserves_existing_rails() {
        let before = rail_project();
        let mut project = before.clone();
        let mut panel = DecorPanel {
            pending_decoration: Some((before.clone(), decoration())),
            ..Default::default()
        };
        let terrain = project.terrain_key();
        panel
            .apply_live(
                &mut project,
                mapper_studio::object_generation::Recipe::new("3".repeat(64)),
            )
            .unwrap();
        assert_eq!(project.rails, before.rails);
        assert_eq!(project.targets, before.targets);
        assert_eq!(project.seeds, before.seeds);
        assert_eq!(project.terrain_key(), terrain);
        assert_eq!(project.decoration, Some(decoration()));
        assert!(project.objects.is_some());
        project.validate().unwrap();
        let mut history = History::default();
        history.record(before.clone(), &project);
        history.undo(&mut project);
        assert_eq!(project, before);
        history.redo(&mut project);
        assert!(project.objects.is_some());
    }
    #[test]
    fn deferred_changes_and_cancellation_cannot_remove_existing_rails_or_apply_objects() {
        let ctx = egui::Context::default();
        let before = rail_project();
        let mut project = before.clone();
        let mut panel = DecorPanel {
            pending_decoration: Some((before.clone(), decoration())),
            ..Default::default()
        };
        click(
            &ctx,
            &mut panel,
            &mut project,
            "Scenery & objects",
        );
        assert!(panel.pending_decoration.is_none());
        assert_eq!(project, before);
        panel.pending_decoration = Some((before, decoration()));
        project.size = 5000;
        let changed = project.clone();
        assert!(
            panel
                .apply_live(
                    &mut project,
                    mapper_studio::object_generation::Recipe::new("3".repeat(64))
                )
                .is_err()
        );
        assert_eq!(project, changed);
    }
    #[test]
    fn automatic_metadata_preparation_has_explicit_cancel_and_retry_without_recipe_edits() {
        let ctx = egui::Context::default();
        let mut project = rail_project();
        let before = project.clone();
        let mut panel = DecorPanel::default();
        let key = Key {
            root: "fixture".into(),
            catalog: "b".repeat(64),
            pin: None,
        };
        panel.wanted = Some(key.clone());
        let (_sender, receiver) = mpsc::channel();
        let signal = Arc::new(AtomicBool::new(false));
        panel.task = Some(Task {
            key,
            cancel: signal.clone(),
            receiver,
        });
        click(
            &ctx,
            &mut panel,
            &mut project,
            "Cancel decoration preparation",
        );
        assert!(signal.load(Ordering::Relaxed));
        assert!(panel.task.is_none());
        assert_eq!(project, before);
        click(
            &ctx,
            &mut panel,
            &mut project,
            "Retry decoration preparation",
        );
        assert!(panel.wanted.is_none());
        assert_eq!(project, before);
    }
    #[test]
    fn saved_live_mode_can_be_disabled_while_installation_data_is_unavailable() {
        let ctx = egui::Context::default();
        let mut panel = DecorPanel::default();
        let mut project = Project {
            objects: Some(mapper_studio::object_generation::Recipe::new("a".repeat(64))),
            ..Project::default()
        };
        let render = |events: Vec<egui::Event>, panel: &mut DecorPanel, project: &mut Project| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640., 400.),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| panel.live_controls(ui, project),
            );
            output.textures_delta.clear();
            output
        };
        let output = render(vec![], &mut panel, &mut project);
        let point = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(text)
                    if text.galley.job.text == "Roadside objects" =>
                {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("placement mode must be visible");
        let event = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = render(
            vec![egui::Event::PointerMoved(point), event(true)],
            &mut panel,
            &mut project,
        );
        let _ = render(vec![event(false)], &mut panel, &mut project);
        assert!(project.objects.is_none());
        assert!(!panel.live_task.running());
    }
}
