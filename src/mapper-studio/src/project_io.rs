//! Background project I/O. Accepted documents change only after complete loading.
use super::*;
use std::sync::{
    Arc,
    atomic::AtomicU8,
    mpsc::{self, Receiver},
};

enum Completed {
    Saved {
        path: PathBuf,
        recipe: Box<Project>,
        revision: u64,
        recipes_revision: u64,
    },
    Opened(Box<Opened>),
}
struct Opened {
    path: PathBuf,
    recipe: Project,
    recipes: recipe_tabs::RecipeBook,
    generation: generation_ui::Generation,
    open: Vec<u64>,
    active: Option<u64>,
}
struct Task {
    signal: Arc<TaskSignal>,
    receiver: Receiver<Result<Option<Completed>, String>>,
    label: &'static str,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}

#[derive(Default)]
pub(super) struct TaskSignal(AtomicU8);

impl TaskSignal {
    const RUNNING: u8 = 0;
    const CANCELLED: u8 = 1;
    const PUBLISHING: u8 = 2;

    fn transition(&self, next: u8) -> bool {
        // The signal arbitrates cancellation and publication. The channel
        // carries the operation result and synchronizes the finished data.
        self.0
            .compare_exchange(Self::RUNNING, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    pub(super) fn cancel(&self) -> bool {
        self.transition(Self::CANCELLED)
    }

    pub(super) fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed) == Self::CANCELLED
    }

    pub(super) fn can_cancel(&self) -> bool {
        self.0.load(Ordering::Relaxed) == Self::RUNNING
    }

    pub(super) fn begin_publish(&self) -> bool {
        self.transition(Self::PUBLISHING)
    }
}
#[derive(Default)]
pub(super) struct ProjectIo {
    task: Option<Task>,
    followup: Option<Action>,
    pub saved_generation: u64,
    pub saved_recipes: u64,
    opened: Option<Box<Opened>>,
    issue: Option<String>,
}
impl ProjectIo {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    pub fn discard_opened(&mut self) {
        self.opened = None;
    }
    pub fn issue(&self) -> Option<&str> {
        self.issue.as_deref()
    }
    pub fn report_issue(&mut self, error: String) {
        self.issue = Some(error);
    }
    pub fn clear_issue(&mut self) {
        self.issue = None;
    }
}

impl Studio {
    pub(super) fn save_project_to(
        &mut self,
        path: PathBuf,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        if self.project_io.busy() {
            return Err("A project operation is already running".into());
        }
        self.project_io.issue = None;
        let mut capture = self.capture_project()?;
        capture.set_recipes(&self.recipe_tabs.book)?;
        let recipes_revision = self.recipe_tabs.revision;
        let recipe = self.document_recipe().clone();
        recipe.validate()?;
        let revision = self.generation.revision;
        let signal = Arc::new(TaskSignal::default());
        let worker = signal.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let (workspace, sources) = capture.encode(&|| worker.cancelled())?;
                if !mapper_studio::project_bundle::write_with_publish_gate(
                    &path,
                    recipe.clone(),
                    workspace,
                    &sources,
                    || worker.cancelled(),
                    || worker.begin_publish(),
                )? {
                    return Ok(None);
                }
                Ok(Some(Completed::Saved {
                    path,
                    recipe: Box::new(recipe),
                    revision,
                    recipes_revision,
                }))
            })();
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        self.project_io.followup = self.pending.take();
        self.project_io.task = Some(Task {
            signal,
            receiver,
            label: "Saving project…",
        });
        self.status = "Saving project…".into();
        ctx.request_repaint();
        Ok(())
    }
    pub(super) fn open_project_from(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.project_io.busy() {
            return;
        }
        self.project_io.issue = None;
        self.project_io.opened = None;
        let signal = Arc::new(TaskSignal::default());
        let worker = signal.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let Some(restored) =
                    generation_ui::Generation::restore(&path, &repaint, &|| worker.cancelled())?
                else {
                    return Ok(None);
                };
                Ok(Some(Completed::Opened(Box::new(Opened {
                    path,
                    recipe: restored.recipe,
                    recipes: restored.recipes,
                    generation: restored.generation,
                    open: restored.open,
                    active: restored.active,
                }))))
            })();
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        self.project_io.task = Some(Task {
            signal,
            receiver,
            label: "Opening project…",
        });
        self.status = "Opening project…".into();
        ctx.request_repaint();
    }
    pub(super) fn defer_project_action(&mut self, action: Action) -> bool {
        if self.project_io.busy() {
            self.project_io.followup = Some(action);
            return true;
        }
        false
    }
    pub(super) fn poll_project_io(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.project_io.task else {
            return;
        };
        let result = match task.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
                return;
            }
            Err(TryRecvError::Disconnected) => Err("Project worker stopped".into()),
        };
        let result = if task.signal.cancelled() {
            Ok(None)
        } else {
            result
        };
        self.project_io.task = None;
        let followup = self.project_io.followup.take();
        match result {
            Ok(Some(Completed::Saved {
                path,
                recipe,
                revision,
                recipes_revision,
            })) => {
                self.saved = *recipe;
                self.project_io.saved_generation = revision;
                self.project_io.saved_recipes = recipes_revision;
                self.path = Some(path);
                self.status = "Project saved".into();
                if let Some(action) = followup {
                    self.request(action, ctx);
                }
            }
            Ok(Some(Completed::Opened(opened))) => {
                self.project_io.opened = Some(opened);
                if let Some(action) = followup {
                    self.project_io.opened = None;
                    self.request(action, ctx);
                } else if self.dirty() {
                    self.pending = Some(Action::AcceptOpened);
                } else {
                    self.accept_opened_project();
                }
            }
            Ok(None) => {
                self.status = "Project operation cancelled".into();
                self.pending = followup;
            }
            Err(error) => {
                self.project_io.issue = Some(error.clone());
                self.status = format!("Project operation failed. {error}");
                self.pending = followup;
            }
        }
    }
    pub(super) fn accept_opened_project(&mut self) {
        let Some(opened) = self.project_io.opened.take() else {
            return;
        };
        self.replace_project(opened.recipe, Some(opened.path));
        self.recipe_tabs.book = opened.recipes;
        self.recipe_tabs.rebuild_tabs(&mut self.dock);
        self.generation.replace_preserving_worker_budgets(opened.generation);
        self.generation.sync_restrictions(&self.restrictions);
        if let Some(font) = self.monument_icons.labels.source(self.project.installation.as_ref()) {
            self.generation.sync_map_font(&font);
        }
        self.project_io.saved_generation = self.generation.revision;
        for id in opened.open {
            self.show_tab(Tab::Generated(id));
            self.apply_tab_request();
        }
        let active = opened.active;
        if let Some(id) = active {
            self.show_tab(Tab::Generated(id));
            self.apply_tab_request();
        }
        self.installation_setup_open = active.is_none() && self.preferences.rust_folder.is_none();
        self.status = "Project opened".into();
    }
    pub(super) fn project_io_controls(&mut self, ui: &mut egui::Ui) {
        if let Some(task) = &self.project_io.task {
            ui.spinner();
            ui.label(if task.signal.cancelled() {
                "Cancelling…"
            } else {
                task.label
            });
            if ui
                .add_enabled(
                    task.signal.can_cancel(),
                    egui::Button::new("Cancel").small(),
                )
                .on_hover_text("Cancel the current project operation")
                .clicked()
            {
                self.cancel_project_io();
            }
        } else if let Some(error) = &self.project_io.issue {
            ui.colored_label(ui.visuals().error_fg_color, "Project error")
                .on_hover_text(error);
        }
    }
    pub(super) fn cancel_project_io(&mut self) {
        if let Some(task) = &self.project_io.task
            && task.signal.cancel()
        {
            self.status = "Cancelling…".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_during_new_project_setup_accepts_defaults_and_protects_user_changes() {
        for (saved_document, user_edit) in [(false, false), (false, true), (true, false)] {
            let ctx = egui::Context::default();
            let mut app = crate::tests::fixture(&ctx);
            app.replace_project(
                Studio::new_workbench_project(),
                saved_document.then(|| "old.mapper".into()),
            );
            let previous = app.project.clone();
            let (sender, receiver) = mpsc::channel();
            app.project_io.task = Some(Task {
                signal: Arc::new(TaskSignal::default()),
                receiver,
                label: "Opening project",
            });
            let pristine = app.pristine_new_project();
            app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
            app.history.record(previous.clone(), &app.project);
            app.accept_new_project_defaults(pristine);
            if user_edit {
                let before = app.project.clone();
                app.project.size = 3000;
                app.history.record(before, &app.project);
            }
            let current = app.project.clone();
            let incoming = Project::default();
            sender
                .send(Ok(Some(Completed::Opened(Box::new(Opened {
                    path: "loaded.mapper".into(),
                    recipe: incoming.clone(),
                    recipes: Default::default(),
                    generation: Default::default(),
                    open: vec![],
                    active: None,
                })))))
                .unwrap();
            app.poll_project_io(&ctx);
            if saved_document || user_edit {
                assert!(matches!(app.pending, Some(Action::AcceptOpened)));
                assert_eq!(app.project, current);
                assert!(app.project_io.opened.is_some());
            } else {
                assert!(app.pending.is_none());
                assert_eq!(app.project, incoming);
                assert_eq!(
                    app.path.as_deref(),
                    Some(std::path::Path::new("loaded.mapper"))
                );
                assert!(app.project_io.opened.is_none());
            }
        }
    }

    #[test]
    fn named_recipes_round_trip_through_project_io() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        let first = app.project.clone();
        let mut other = first.clone();
        other.master_seed = 77234;
        other.refresh_inherited_seeds();
        assert!(app.import_named_recipe("Island Test", other.clone()));
        let active = app.recipe_tabs.active();
        assert!(app.dirty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("named-recipes.mapper");
        app.save_project_to(path.clone(), &ctx).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.project_io.busy() && std::time::Instant::now() < deadline {
            app.poll_project_io(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!app.dirty(), "{}", app.status);
        app.replace_project(first.clone(), None);
        app.open_project_from(path, &ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.project_io.busy() && std::time::Instant::now() < deadline {
            app.poll_project_io(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.recipe_tabs.active(), active);
        assert_eq!(app.recipe_tabs.name(active), "Island Test");
        assert_eq!(app.document_recipe(), &other);
        assert_eq!(app.recipe_by_id(FIRST_RECIPE), &first);
        assert!(app.dock.find_tab(&Tab::Recipe(active)).is_some());
        assert!(app.dock.find_tab(&Tab::Recipe(FIRST_RECIPE)).is_some());
        assert!(!app.dirty());
    }

    #[test]
    fn cancellation_keeps_the_save_busy_until_the_worker_acknowledges_it() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        app.project.targets[0].x += 10.;
        let original = app.project.clone();
        let signal = Arc::new(TaskSignal::default());
        let (sender, receiver) = mpsc::channel();
        app.project_io.task = Some(Task {
            signal: signal.clone(),
            receiver,
            label: "Saving project…",
        });
        app.project_io.followup = Some(Action::Close);
        app.cancel_project_io();
        assert!(signal.cancelled());
        app.poll_project_io(&ctx);
        assert!(app.project_io.busy());
        assert!(app.pending.is_none());
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("existing.mapper");
        std::fs::write(&destination, b"previous project").unwrap();
        let written = mapper_studio::project_bundle::write_with_publish_gate(
            &destination,
            original.clone(),
            serde_json::Value::Null,
            &[],
            || false,
            || signal.begin_publish(),
        )
        .unwrap();
        assert!(!written);
        assert_eq!(std::fs::read(destination).unwrap(), b"previous project");
        sender.send(Ok(None)).unwrap();
        app.poll_project_io(&ctx);
        assert!(!app.project_io.busy());
        assert_eq!(app.status, "Project operation cancelled");
        assert_eq!(app.project, original);
        assert!(app.dirty());
        assert!(matches!(app.pending, Some(Action::Close)));
        assert!(!app.allow_close);
    }

    #[test]
    fn a_save_that_has_started_publishing_reports_its_real_result() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        app.project.targets[0].x += 10.;
        let recipe = app.document_recipe().clone();
        let (sender, receiver) = mpsc::channel();
        let signal = Arc::new(TaskSignal::default());
        app.project_io.task = Some(Task {
            signal: signal.clone(),
            receiver,
            label: "Saving project…",
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("published.mapper");
        let written = mapper_studio::project_bundle::write_with_publish_gate(
            &path,
            recipe.clone(),
            serde_json::Value::Null,
            &[],
            || signal.cancelled(),
            || {
                assert!(signal.begin_publish());
                app.cancel_project_io();
                assert!(!signal.cancelled());
                true
            },
        )
        .unwrap();
        assert!(written && path.is_file());
        sender
            .send(Ok(Some(Completed::Saved {
                path: path.clone(),
                recipe: Box::new(recipe.clone()),
                revision: app.generation.revision,
                recipes_revision: 0,
            })))
            .unwrap();
        app.poll_project_io(&ctx);
        assert_eq!(app.status, "Project saved");
        assert_eq!(app.path, Some(path));
        assert_eq!(app.saved, recipe);
        assert!(!app.dirty());
    }

    #[test]
    fn project_operation_states_keep_the_menu_bar_height_stable() {
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let mut app = crate::tests::fixture(&ctx);
            app.path = Some(PathBuf::from(format!(
                "{}.mapper",
                "Long project name ".repeat(15)
            )));
            let mut heights = Vec::new();
            for state in 0..3 {
                let (_sender, receiver) = mpsc::channel();
                app.project_io.task = (state == 1).then(|| Task {
                    signal: Arc::new(TaskSignal::default()),
                    receiver,
                    label: "Saving project…",
                });
                app.project_io.issue =
                    (state == 2).then(|| "The selected folder could not be written".into());
                let mut height = 0.;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1050., 100.))),
                        ..Default::default()
                    },
                    |ui| {
                        app.toolbar(ui);
                        height = ui.min_rect().height();
                    },
                );
                output.textures_delta.clear();
                heights.push(height);
            }
            assert!(
                heights
                    .iter()
                    .all(|height| (*height - heights[0]).abs() < 1.),
                "{heights:?}"
            );
        }
    }
}
