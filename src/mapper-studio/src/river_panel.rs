use eframe::egui;
use mapper_studio::{Project, installation::Installation, river_generation::Recipe};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
};
struct Task {
    source: Installation,
    recipe: Option<Recipe>,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Recipe>, String>>,
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct Panel {
    prepared: Option<(PathBuf, Installation, Recipe)>,
    task: Option<Task>,
    attempted: Option<(Installation, Recipe)>,
    message: String,
}
impl Panel {
    pub(super) fn ready_for_import(&self, project: &Project, root: Option<&std::path::Path>) -> Option<Recipe> {
        let (path, source, recipe) = self.prepared.as_ref()?;
        (Some(path.as_path()) == root && project.installation.as_ref() == Some(source))
            .then(|| recipe.clone())
    }
    pub fn accept_prepared(&mut self, root: PathBuf, source: Installation, recipe: Option<Recipe>) {
        self.prepared = recipe.map(|recipe| (root, source, recipe));
    }
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    pub fn readiness(&self, project: &Project) -> Result<(), String> {
        if self.busy() {
            return Err("Wait for river data to finish loading".into());
        }
        if !self.message.is_empty()
            && self.attempted.as_ref().is_some_and(|(source, recipe)| {
                project.installation.as_ref() == Some(source)
                    && project.rivers.as_ref() == Some(recipe)
            })
        {
            return Err(self.message.clone());
        }
        Ok(())
    }
    pub fn tick(&mut self, project: &mut Project, root: Option<PathBuf>, ctx: &egui::Context) {
        if let Some((prepared_root, prepared_source, prepared)) = &self.prepared
            && root.as_ref() == Some(prepared_root)
            && project.installation.as_ref() == Some(prepared_source)
            && let Some(recipe) = &mut project.rivers
            && recipe.source_sha256 == prepared.source_sha256
            && recipe.objects.is_none()
        {
            recipe.objects = prepared.objects.clone();
        }
        if self.task.is_none()
            && mapper_studio::native_preview::is_native(&project.preview_engine)
            && let (Some(source), Some(recipe), Some(root)) =
                (&project.installation, &project.rivers, root)
            && recipe.source_document.is_none()
            && self
                .attempted
                .as_ref()
                .is_none_or(|(previous_source, previous_recipe)| {
                    previous_source != source || previous_recipe != recipe
                })
        {
            self.attempted = Some((source.clone(), recipe.clone()));
            self.start(root, source.clone(), Some(recipe.clone()), ctx);
        }
        let Some(task) = &self.task else {
            return;
        };
        if task.cancel.load(Ordering::Relaxed) {
            self.task = None;
            self.message = "River preparation cancelled".into();
            return;
        }
        if project.installation.as_ref() != Some(&task.source)
            || project.rivers != task.recipe
            || !mapper_studio::native_preview::is_native(&project.preview_engine)
        {
            self.task = None;
            return;
        }
        match task.receiver.try_recv() {
            Ok(result) => {
                self.task = None;
                match result {
                    Ok(Some(recipe)) => {
                        project.rivers = Some(recipe);
                        if project.composition_engine.is_some()
                            && !papyrus_terrain::monument_composition::water_aware(
                                project.composition_engine.as_deref(),
                            )
                        {
                            project.composition_engine =
                                Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
                        }
                        self.message.clear();
                    }
                    Ok(None) => {
                        self.message = "This installation has no supported river adapter".into()
                    }
                    Err(error) => self.message = error,
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.task = None;
                self.message = "River preparation stopped".into();
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100))
            }
        }
    }
    fn start(
        &mut self,
        root: PathBuf,
        source: Installation,
        recipe: Option<Recipe>,
        ctx: &egui::Context,
    ) {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let expected = source.clone();
        let pinned = recipe.clone();
        let ctx = ctx.clone();
        let prepared = self
            .prepared
            .as_ref()
            .filter(|(path, installation, prepared)| {
                path == &root
                    && installation == &source
                    && pinned
                        .as_ref()
                        .is_none_or(|recipe| recipe.source_sha256 == prepared.source_sha256)
            })
            .map(|(_, _, recipe)| recipe.clone());
        if let Some(prepared) = prepared {
            let _ = sender.send(Ok(Some(prepared)));
        } else {
            std::thread::spawn(move || {
                let _loading =
                    mapper_studio::loading_progress::begin("River data", Some(signal.clone()));
                let result =
                    mapper_studio::river_extract::prepare(&root, &expected, pinned.as_ref(), || {
                        signal.load(Ordering::Relaxed)
                    })
                    .map(|p| p.map(|p| {
                        let mut recipe = p.source.recipe();
                        recipe.objects = pinned.as_ref().and_then(|r| r.objects.clone());
                        recipe
                    }));
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        }
        self.task = Some(Task {
            source,
            recipe,
            cancel,
            receiver,
        });
        self.message.clear();
    }
    pub fn show(&mut self, ui: &mut egui::Ui, project: &mut Project, root: Option<PathBuf>) {
        let enabled = project.rivers.is_some() || self.task.is_some();
        let native = mapper_studio::native_preview::is_native(&project.preview_engine);
        let ctx = ui.ctx().clone();
        let (header, changed) = super::panels::recipe_section(
            ui, "Water & rivers", enabled, enabled || (native && root.is_some()), false,
            "Generate rivers and carve channels using the installation’s river data.",
        );
        if let Some(enabled) = changed {
            if !enabled {self.task=None;project.rivers=None;}
            else if let (Some(root),Some(source))=(root,project.installation.clone()) {
                self.start(root,source,None,&ctx);
            }
        }
        header.body(|ui| {
            if project.rivers.is_some() {
                let mut custom = project.options.river_limit.is_some();
                if ui.checkbox(&mut custom, "Custom river settings").changed() {
                    project.options.river_limit = custom.then_some(3);
                }
                if let Some(limit) = &mut project.options.river_limit {
                    let label = ui.label("River limit");
                    ui.add(egui::DragValue::new(limit).range(0..=64)).labelled_by(label.id)
                        .on_hover_text("Maximum ordinary rivers. Installation-defined special rivers keep their own rules.");
                }
            }
            if self.task.is_some() {
                ui.spinner();
            }
            if !self.message.is_empty() {
                ui.add(egui::Label::new(&self.message).truncate())
                    .on_hover_text(&self.message);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> Project {
        Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            installation: Some(Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: mapper_studio::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            rivers: Some(Recipe {
                engine: papyrus_terrain::river_pipeline::VERSION.into(),
                source_sha256: "b".repeat(64),
                source_document: None,
                objects: None,
            }),
            ..Default::default()
        }
    }
    #[test]
    fn prepared_metadata_attaches_only_to_the_requested_recipe_and_survives_seed_changes() {
        for stale in [false, true] {
            let mut project = project();
            let before = project.rivers.clone();
            let source = project.installation.clone().unwrap();
            let (sender, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let mut ready = before.clone().unwrap();
            ready.source_document = Some("source document".into());
            sender.send(Ok(Some(ready.clone()))).unwrap();
            let mut panel = Panel {
                task: Some(Task {
                    source,
                    recipe: before,
                    cancel: cancel.clone(),
                    receiver,
                }),
                ..Default::default()
            };
            project.seeds[2].value += 1;
            project.seeds[2].inherited = false;
            if stale {
                project.rivers = None;
            }
            panel.tick(&mut project, None, &egui::Context::default());
            assert_eq!(project.rivers, (!stale).then_some(ready));
            assert!(cancel.load(Ordering::Relaxed));
            assert!(!panel.busy());
        }
    }
    #[test]
    fn startup_adds_prepared_attachments_to_existing_river_recipes() {
        let mut project = project();
        project.rivers.as_mut().unwrap().source_document = Some("saved river data".into());
        let mut prepared = project.rivers.clone().unwrap();
        prepared.objects = Some(mapper_studio::river_objects::Recipe {
            source_sha256: "c".repeat(64), infrastructure_sha256: "d".repeat(64),
            catalog_sha256: "e".repeat(64), documents: None,
        });
        let source = project.installation.clone().unwrap();
        let root = PathBuf::from("prepared-client-without-files");
        let mut panel = Panel::default();
        panel.accept_prepared(root.clone(), source, Some(prepared.clone()));
        panel.tick(&mut project, Some(root), &egui::Context::default());
        assert_eq!(project.rivers, Some(prepared));
        assert!(!panel.busy());
    }
    #[test]
    fn startup_river_data_binds_without_reading_the_installation_again() {
        let mut project = project();
        let source = project.installation.clone().unwrap();
        let root = PathBuf::from("prepared-client-without-files");
        let mut recipe = project.rivers.clone().unwrap();
        recipe.source_document = Some(Arc::from("prepared numeric source"));
        let mut panel = Panel::default();
        panel.accept_prepared(root.clone(), source, Some(recipe.clone()));
        panel.tick(&mut project, Some(root), &egui::Context::default());
        assert!(!panel.busy());
        assert_eq!(project.rivers, Some(recipe));
    }

    #[test]
    fn cancelled_results_and_failed_metadata_cannot_start_generation() {
        let mut project = project();
        let source = project.installation.clone().unwrap();
        let recipe = project.rivers.clone().unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Some(recipe.clone()))).unwrap();
        let mut panel = Panel {
            prepared: None,
            task: Some(Task {
                source: source.clone(),
                recipe: Some(recipe.clone()),
                cancel: Arc::new(AtomicBool::new(true)),
                receiver,
            }),
            attempted: Some((source, recipe.clone())),
            message: String::new(),
        };
        panel.tick(&mut project, None, &egui::Context::default());
        assert_eq!(project.rivers, Some(recipe));
        assert!(!panel.busy());
        assert!(panel.readiness(&project).is_err());
        project.rivers = None;
        assert!(panel.readiness(&project).is_ok());
    }
}
