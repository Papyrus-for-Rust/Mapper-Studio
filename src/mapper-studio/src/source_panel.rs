use eframe::egui;
use mapper_studio::{
    Project, client_preparation::ReboundRecipe,
    installation::{self, Binding, Installation},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

const WATCH_INTERVAL: Duration = Duration::from_secs(5);

struct Task {
    receiver: Receiver<(installation::ChangeWatch, Result<Installation, String>)>,
    cancel: Arc<AtomicBool>,
    root: PathBuf,
    installation: Option<Installation>,
    preview_engine: String,
}
struct RebindTask {
    saved: Project,
    binding: Binding,
    receiver: Receiver<Result<(installation::ChangeWatch, ReboundRecipe), String>>,
    cancel: Arc<AtomicBool>,
}
struct PendingSource {
    previous: Installation,
    binding: Binding,
}

struct Watch {
    root: PathBuf,
    snapshot: installation::ChangeWatch,
    checked: Instant,
}

fn prepare_replacement<T>(
    root: &Path,
    prepare: impl FnOnce() -> Result<T, String>,
) -> Result<(installation::ChangeWatch, T), String> {
    let watch = installation::ChangeWatch::capture(root);
    let prepared = prepare()?;
    if watch.changed(root) {
        return Err("Rust files changed during preparation. Recheck source.".into());
    }
    Ok((watch, prepared))
}
impl Drop for RebindTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct SourcePanel {
    binding: Option<Binding>,
    task: Option<Task>,
    pending: Option<PendingSource>,
    message: String,
    selected: Option<PathBuf>,
    watch: Option<Watch>,
    watch_probe: Option<Receiver<(PathBuf, installation::ChangeWatch)>>,
    source_changed: bool,
    rebind: Option<RebindTask>,
    prepared: Option<(PathBuf, ReboundRecipe)>,
}
impl SourcePanel {
    pub fn cancel_inspection(&mut self) {
        let task = self.task.take();
        if task.is_some() || self.pending.is_some() || self.rebind.is_some() {
            self.message.clear();
        }
        if let Some(task) = task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        self.pending = None;
        self.rebind = None;
        self.prepared = None;
        self.watch_probe = None;
    }

    pub fn forget(&mut self, keep_selection: bool) {
        self.cancel_inspection();
        self.binding = None;
        self.watch = None;
        self.message.clear();
        if !keep_selection {
            self.selected = None;
        }
        self.source_changed = true;
    }
    pub fn inspecting(&self)->bool{self.task.is_some() || self.rebind.is_some()}
    pub fn inspection_token(&self) -> Option<&Arc<AtomicBool>> {
        self.task.as_ref().map(|task| &task.cancel)
            .or_else(|| self.rebind.as_ref().map(|task| &task.cancel))
    }

    pub fn bound_root(&self, project: &Project) -> Option<PathBuf> {
        self.bound_root_ref(project).map(Path::to_path_buf)
    }

    pub fn bound_root_ref(&self, project: &Project) -> Option<&Path> {
        if self.inspecting() {
            return None;
        }
        self.binding
            .as_ref()
            .filter(|b| project.installation.as_ref() == Some(&b.identity))
            .map(|b| b.root.as_path())
    }
    pub fn start(&mut self, root: PathBuf, project: &Project, ctx: &egui::Context) {
        self.cancel_inspection();
        if self.binding.is_none() && self.selected_root().as_deref() != Some(root.as_path()) {
            self.watch = None;
        }
        self.selected = Some(root.clone());
        let (tx, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let path = root.clone();
        ctx.request_repaint_after(Duration::from_millis(100));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _loading =
                mapper_studio::loading_progress::begin("Rust installation", Some(signal.clone()));
            let watch = installation::ChangeWatch::capture(&path);
            let result = installation::inspect(&path, || signal.load(Ordering::Relaxed));
            let _ = tx.send((watch, result));
            ctx.request_repaint();
        });
        self.task = Some(Task {
            receiver,
            cancel,
            root,
            installation: project.installation.clone(),
            preview_engine: project.preview_engine.clone(),
        });
        self.message = "Inspecting Rust installation…".into();
    }
    fn poll(&mut self, project: &mut Project) {
        let Some(task) = &self.task else {
            return;
        };
        let (watch, result) = match task.receiver.try_recv() {
            Ok((watch, result)) => (Some(watch), result),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                (None, Err("Installation inspection worker stopped".into()))
            }
        };
        let task = self.task.take().unwrap();
        if task.cancel.load(Ordering::Relaxed) {
            if let Some(root) = self.bound_root(project) {
                self.selected = Some(root);
            }
            self.message = "Inspection cancelled; source unchanged.".into();
            return;
        }
        if task.installation != project.installation {
            self.message = "Recipe source changed. Recheck the Rust folder.".into();
            return;
        }
        match result {
            Err(error) => {
                if let Some(watch) = watch
                    && self.binding.as_ref().is_none_or(|binding| binding.root == task.root)
                {
                    self.update_watch(task.root.clone(), watch);
                }
                self.invalidate_root(&task.root);
                self.message = error;
            }
            Ok(identity) => {
                let watch = watch.expect("Successful inspections include file hints");
                let binding = Binding {
                    root: task.root,
                    identity,
                };
                if let Some(old) = project.installation.as_ref()
                    && old != &binding.identity
                {
                    self.invalidate_root(&binding.root);
                    if self.watch.as_ref().is_none_or(|active| active.root == binding.root) {
                        self.update_watch(binding.root.clone(), watch);
                    }
                    self.pending = Some(PendingSource { previous: old.clone(), binding });
                    self.message = "Installation differs from this project's saved source. Review before replacing it.".into();
                } else {
                    let first_source = project.installation.is_none();
                    if first_source && task.preview_engine == project.preview_engine {
                        project.preview_engine = mapper_studio::native_preview::VERSION.into();
                    }
                    project.installation = Some(binding.identity.clone());
                    self.message = if mapper_studio::native_profile::for_source(
                        &binding.identity,
                        project.native_profile.as_ref(),
                    ).is_ok() {
                        "Rust installation verified. Monument assets are discovered automatically."
                    } else {
                        "Rust installation verified. This version needs a native generation adapter."
                    }.into();
                    self.source_changed |= self.binding.as_ref().is_some_and(|old| old.root != binding.root);
                    if self.update_watch(binding.root.clone(), watch) {
                        self.source_changed = true;
                    }
                    self.binding = Some(binding);
                }
            }
        }
    }

    fn invalidate_root(&mut self, root: &Path) {
        if self.binding.as_ref().is_some_and(|binding| binding.root == root) {
            self.binding = None;
            self.source_changed = true;
        }
    }
    pub fn tick(&mut self, project: &mut Project, ctx: &egui::Context) {
        if self.pending.as_ref().is_some_and(|pending| {
            project.installation.as_ref() != Some(&pending.previous)
        }) {
            self.pending = None;
            self.message = "Recipe source changed. Recheck the Rust folder.".into();
        }
        self.poll_rebind(project);
        self.poll(project);
        if self.inspecting() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.pending.is_none() || self.bound_root_ref(project).is_some() {
            self.tick_watch(project, ctx);
        }
    }

    fn start_rebind(&mut self, project: &Project, ctx: &egui::Context) {
        let Some(pending) = &self.pending else {
            return;
        };
        let target = &pending.binding;
        let binding = Binding { root: target.root.clone(), identity: target.identity.clone() };
        let root = binding.root.clone();
        let installation = binding.identity.clone();
        let request = project.clone();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _progress = mapper_studio::loading_progress::begin("Switching Rust installation", Some(signal.clone()));
            let _verification = installation::VerificationScope::default();
            let cancelled = || signal.load(Ordering::Relaxed);
            let result = prepare_replacement(&root, || {
                if installation::inspect(&root, &cancelled)? != installation {
                    return Err("The selected Rust installation changed. Recheck source.".into());
                }
                mapper_studio::client_preparation::rebind_recipe(
                    &root, &installation, &request, cancelled,
                )
            });
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        self.rebind = Some(RebindTask { saved: project.clone(), binding, receiver, cancel });
        self.message = "Preparing the recipe for this Rust installation…".into();
    }

    fn poll_rebind(&mut self, project: &mut Project) {
        let Some(task) = &self.rebind else {
            return;
        };
        if &task.saved != project {
            self.rebind = None;
            self.message = "The recipe changed during preparation. Try switching again.".into();
            return;
        }
        let result = match task.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Installation preparation stopped unexpectedly".into()),
        };
        let task = self.rebind.take().unwrap();
        if task.cancel.load(Ordering::Relaxed) {
            self.message = "Installation switch cancelled.".into();
            return;
        }
        match result {
            Ok((watch, prepared)) => {
                *project = prepared.project.clone();
                let root = task.binding.root.clone();
                self.binding = Some(Binding { root: root.clone(), identity: task.binding.identity.clone() });
                self.pending = None;
                self.update_watch(root.clone(), watch);
                self.watch_probe = None;
                self.prepared = Some((root, prepared));
                self.source_changed = true;
                self.message = "Rust installation switched.".into();
            }
            Err(error) => self.message = error,
        }
    }

    pub fn take_prepared_recipe(&mut self) -> Option<(PathBuf, ReboundRecipe)> {
        self.prepared.take()
    }

    fn update_watch(&mut self, root: PathBuf, snapshot: installation::ChangeWatch) -> bool {
        let changed = self.watch.as_ref().is_some_and(|watch| watch.root == root && watch.snapshot != snapshot);
        self.watch = Some(Watch { root, snapshot, checked: Instant::now() });
        changed
    }

    fn tick_watch(&mut self, project: &Project, ctx: &egui::Context) {
        if let Some(probe) = &self.watch_probe {
            match probe.try_recv() {
                Ok((root, next)) => {
                    self.watch_probe = None;
                    if self.watch.as_ref().is_some_and(|watch| watch.root == root)
                        && self.update_watch(root.clone(), next)
                    {
                        self.invalidate_root(&root);
                        if self.pending.is_none() {
                            self.start(root, project, ctx);
                            self.message = "Rust files changed. Checking the installation…".into();
                            return;
                        }
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.watch_probe = None;
                    if let Some(watch) = &mut self.watch {
                        watch.checked = Instant::now();
                    }
                }
            }
        }
        let Some(watch) = &self.watch else {
            return;
        };
        let delay = WATCH_INTERVAL.saturating_sub(watch.checked.elapsed());
        if !delay.is_zero() {
            ctx.request_repaint_after(delay);
        } else {
            let root = watch.root.clone();
            let (sender, receiver) = mpsc::channel();
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                let snapshot = installation::ChangeWatch::capture(&root);
                let _ = sender.send((root, snapshot));
                repaint.request_repaint();
            });
            self.watch_probe = Some(receiver);
        }
    }
    fn keep_saved_source(&mut self, project: &Project, ctx: &egui::Context) {
        let Some(pending) = self.pending.take() else { return };
        if let Some(root) = self.bound_root(project) {
            if let Some(watch) = &mut self.watch
                && watch.root == root
            {
                watch.checked = Instant::now() - WATCH_INTERVAL;
            }
            self.selected = Some(root);
            self.message = "Saved source retained.".into();
        } else if let Some(watch) = &self.watch
            && watch.root != pending.binding.root
        {
            self.start(watch.root.clone(), project, ctx);
        } else {
            self.message = "Saved source retained.".into();
        }
    }
    pub fn take_source_changed(&mut self) -> bool {
        std::mem::take(&mut self.source_changed)
    }
    pub fn has_pending_source(&self) -> bool {
        self.pending.is_some()
    }
    pub fn selected_root(&self) -> Option<PathBuf> {
        self.selected
            .clone()
            .or_else(|| self.binding.as_ref().map(|b| b.root.clone()))
    }
    /// Shared by first-run setup and Settings; both use the same inspection path.
    pub fn folder_picker(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        if ui
            .add_enabled(
                self.task.is_none(),
                egui::Button::new("Choose Rust folder…"),
            )
            .clicked()
            && let Some(root) = rfd::FileDialog::new()
                .set_title("Choose Rust client or dedicated-server root")
                .pick_folder()
        {
            self.start(root, project, ui.ctx());
        }
        if let Some(task) = &self.task {
            ui.spinner();
            ui.label("Checking this Rust installation…");
            if ui.button("Cancel source inspection").clicked() {
                task.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    pub fn setup_message(&self) -> &str {
        &self.message
    }
    pub fn settings(&mut self, ui: &mut egui::Ui, project: &mut Project, can_switch: bool) {
        ui.strong("Rust installation");
        ui.small("Source for game terrain and assets");
        if let Some(identity) = &project.installation {
            ui.label(format!("{:?} · Windows x64", identity.kind));
            ui.small(format!(
                "Native SHA-256: {}…",
                &identity.native.sha256[..12]
            ));
            ui.small(if identity.scene_manifest.is_some() {
                "Scene manifest fingerprinted"
            } else {
                "Scene manifest missing; asset discovery incomplete"
            });
            let matched = self
                .binding
                .as_ref()
                .is_some_and(|b| &b.identity == identity);
            if matched {
                ui.small("Installation verified");
            } else if self.pending.is_none() {
                ui.label("Choose this project’s matching Rust installation.");
            }
        } else {
            ui.label("No Rust folder selected");
        }
        if let Some(binding) = &self.binding {
            ui.small(binding.root.display().to_string());
        }

        ui.add_enabled_ui(can_switch, |ui| {
            if ui
                .add_enabled(
                    self.selected_root().is_some(),
                    egui::Button::new("Recheck source"),
                )
                .clicked()
            {
                self.start(self.selected_root().unwrap(), project, ui.ctx());
            }
        });
        if self.pending.is_some() {
            if let Some(task) = &self.rebind {
                if ui.button("Cancel switch").clicked() {
                    task.cancel.store(true, Ordering::Relaxed);
                }
            } else {
                if ui.add_enabled(can_switch, egui::Button::new("Use this different installation")).clicked() {
                    self.start_rebind(project, ui.ctx());
                }
                if ui.button("Keep saved source").clicked() {
                    self.keep_saved_source(project, ui.ctx());
                }
            }
        }
        if !self.message.is_empty() {
            ui.small(&self.message);
        }
        let native_available = project.installation.as_ref().is_some_and(|s| {
            mapper_studio::native_profile::for_source(s, project.native_profile.as_ref()).is_ok()
        });
        if project.installation.is_some() && !native_available {
            ui.small("Native generation unavailable: import a tested compatibility profile for this DLL and a supported ABI family.");
        }
        if mapper_studio::native_preview::is_native(&project.preview_engine) {
            ui.label("Terrain source · Installed Rust");
        } else if project.installation.is_some() {
            ui.label("Terrain source · Independent");
            if ui.add_enabled(can_switch && native_available, egui::Button::new("Use installed Rust terrain"))
                .on_hover_text("Use the selected installation for the current recipe.")
                .clicked()
            {
                project.preview_engine = mapper_studio::native_preview::VERSION.into();
            }
        }
        ui.collapsing("Native compatibility profile", |ui| {
            let resolved = project.installation.as_ref().and_then(|s| mapper_studio::native_profile::for_source(s, project.native_profile.as_ref()).ok());
            if let Some(profile) = &resolved {
                ui.label(&profile.id);
                ui.small(format!("ABI: {}", profile.abi));
                ui.small(if project.native_profile.is_some() { "Profile is saved inside this recipe and generation profiles." } else { "Using retained defaults for older recipes." });
            } else { ui.small("No matching profile."); }
            if ui.add_enabled(project.installation.is_some() && self.task.is_none(), egui::Button::new("Import native profile…")).clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("Papyrus native profile", &["json"]).pick_file() {
                let result = mapper_studio::native_profile::Profile::load(&path).and_then(|profile| {
                    profile.matches(project.installation.as_ref().ok_or("Select a Rust installation first")?)?;
                    Ok(profile)
                });
                match result {
                    Ok(profile) => { project.native_profile = Some(profile); self.message = "Native profile recorded. Rebuild the terrain preview to apply it.".into(); }
                    Err(error) => self.message = error,
                }
            }
            if ui.add_enabled(resolved.is_some(), egui::Button::new("Export native profile…")).clicked()
                && let Some(path) = rfd::FileDialog::new().set_file_name("native-profile.json").add_filter("Papyrus native profile", &["json"]).save_file() {
                self.message = match mapper_studio::profile::write_document(&path, &resolved.unwrap(), true) {
                    Ok(()) => "Native compatibility profile exported.".into(), Err(error) => error,
                };
            }
            ui.small("Profiles declare an ABI and settings for an exact DLL fingerprint. New ABI families still require adapter code and testing. Importing a profile does not verify the declared ABI.");
        });
    }
}

impl Drop for SourcePanel {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(hash: char) -> Installation {
        Installation {
            kind: installation::InstallationKind::Client,
            native: installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: hash.to_string().repeat(64),
                bytes: 128,
            },
            scene_manifest: None,
        }
    }
    fn completed(project: &Project, result: Installation, cancelled: bool) -> SourcePanel {
        let root = PathBuf::from("test-installation");
        let (tx, receiver) = mpsc::channel();
        tx.send((installation::ChangeWatch::capture(&root), Ok(result))).unwrap();
        let mut panel = SourcePanel::default();
        panel.task = Some(Task {
            receiver,
            cancel: Arc::new(AtomicBool::new(cancelled)),
            root,
            installation: project.installation.clone(),
            preview_engine: project.preview_engine.clone(),
        });
        panel
    }
    #[test]
    fn unknown_first_binding_keeps_installed_terrain_intent_and_reports_adapter() {
        let mut project = Project::default();
        let seeds = project.seeds;
        let mut panel = completed(&project, identity('a'), false);
        panel.poll(&mut project);
        assert_eq!(project.preview_engine, mapper_studio::native_preview::VERSION);
        assert_eq!(project.seeds, seeds);
        project.validate().unwrap();
        assert!(mapper_studio::native_profile::resolve(&project.terrain_key()).is_err());
        assert!(panel.bound_root(&project).is_some());
        assert!(panel.message.contains("needs a native generation adapter"));
    }
    #[test]
    fn rebinding_preserves_an_existing_independent_project() {
        let mut project = Project {
            installation: Some(identity('a')),
            ..Default::default()
        };
        let saved = project.clone();
        let mut panel = completed(&project, identity('a'), false);
        panel.binding = Some(Binding {
            root: PathBuf::from("test-installation"),
            identity: identity('a'),
        });
        assert!(panel.bound_root(&project).is_none());
        panel.poll(&mut project);
        assert_eq!(project, saved);
        assert!(panel.bound_root(&project).is_some());
    }
    #[test]
    fn bundle_update_unbinds_source_and_requests_asset_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Bundles/shared")).unwrap();
        let bundle = dir.path().join("Bundles/shared/content.bundle");
        std::fs::write(&bundle, b"old").unwrap();
        let source = identity('a');
        let mut project = Project {
            installation: Some(source.clone()),
            ..Default::default()
        };
        let mut panel = SourcePanel::default();
        panel.binding = Some(Binding {
            root: dir.path().into(),
            identity: source,
        });
        panel.selected = Some(dir.path().into());
        panel.watch = Some(Watch {
            root: dir.path().into(),
            snapshot: installation::ChangeWatch::capture(dir.path()),
            checked: Instant::now() - WATCH_INTERVAL,
        });
        std::fs::write(bundle, b"new update").unwrap();
        let ctx = egui::Context::default();
        panel.tick(&mut project, &ctx);
        assert!(panel.watch_probe.is_some());
        assert!(panel.bound_root(&project).is_some());
        let deadline = Instant::now() + Duration::from_secs(5);
        while panel.watch_probe.is_some() && Instant::now() < deadline {
            panel.tick(&mut project, &ctx);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(panel.take_source_changed());
        assert!(!panel.take_source_changed());
        assert!(panel.bound_root(&project).is_none());
        assert_eq!(panel.selected_root().as_deref(), Some(dir.path()));
        assert!(panel.watch.is_some());
    }

    #[test]
    fn switching_folders_discards_a_pending_probe() {
        let folder = tempfile::tempdir().unwrap();
        let previous = folder.path().join("previous");
        let next = folder.path().join("next");
        let (sender, receiver) = mpsc::channel();
        let mut panel = SourcePanel::default();
        panel.selected = Some(previous.clone());
        panel.watch_probe = Some(receiver);
        panel.start(next.clone(), &Project::default(), &egui::Context::default());
        let snapshot = installation::ChangeWatch::capture(&previous);
        assert!(sender.send((previous, snapshot)).is_err());
        assert!(panel.watch_probe.is_none());
        assert_eq!(panel.selected_root().as_deref(), Some(next.as_path()));
    }

    #[test]
    fn reviewing_a_different_folder_keeps_the_saved_source_watch() {
        let folder = tempfile::tempdir().unwrap();
        let previous = folder.path().to_owned();
        let assembly = previous.join("GameAssembly.dll");
        std::fs::write(&assembly, b"first").unwrap();
        let ctx = egui::Context::default();
        let mut project = Project { installation: Some(identity('a')), ..Default::default() };
        let original = project.clone();
        for changed in [false, true] {
            let mut panel = completed(&project, identity('b'), false);
            panel.selected = Some("test-installation".into());
            panel.binding = Some(Binding { root: previous.clone(), identity: identity('a') });
            panel.update_watch(previous.clone(), installation::ChangeWatch::capture(&previous));
            panel.poll(&mut project);
            assert_eq!(panel.watch.as_ref().unwrap().root, previous);
            assert!(panel.has_pending_source());
            if changed {
                std::fs::write(&assembly, b"different length").unwrap();
                let (sender, receiver) = mpsc::channel();
                sender.send((previous.clone(), installation::ChangeWatch::capture(&previous))).unwrap();
                panel.watch_probe = Some(receiver);
                panel.tick(&mut project, &ctx);
                assert!(panel.take_source_changed());
                assert!(panel.bound_root(&project).is_none());
                assert!(panel.has_pending_source());
            }
            panel.keep_saved_source(&project, &ctx);
            assert!(!panel.has_pending_source());
            assert_eq!(panel.selected_root(), Some(previous.clone()));
            assert_eq!(panel.inspecting(), changed);
            assert_eq!(project, original);
        }
    }

    #[test]
    fn keeping_a_saved_source_after_an_update_does_not_repeat_the_same_inspection() {
        let mut project = Project { installation: Some(identity('a')), ..Default::default() };
        let mut panel = completed(&project, identity('b'), false);
        let root = panel.task.as_ref().unwrap().root.clone();
        let different = tempfile::tempdir().unwrap();
        std::fs::write(different.path().join("GameAssembly.dll"), b"previous hints").unwrap();
        panel.update_watch(root.clone(), installation::ChangeWatch::capture(different.path()));
        panel.binding = Some(Binding { root: root.clone(), identity: identity('a') });
        panel.selected = Some(root.clone());
        panel.poll(&mut project);
        assert!(panel.take_source_changed());
        assert!(panel.bound_root(&project).is_none());
        assert_eq!(panel.watch.as_ref().unwrap().snapshot, installation::ChangeWatch::capture(&root));

        panel.keep_saved_source(&project, &egui::Context::default());
        assert!(!panel.inspecting());
        assert!(!panel.has_pending_source());
        assert_eq!(project.installation, Some(identity('a')));
    }

    #[test]
    fn replacement_uses_current_file_hints_and_rejects_changes_during_preparation() {
        let folder = tempfile::tempdir().unwrap();
        let assembly = folder.path().join("GameAssembly.dll");
        std::fs::write(&assembly, b"original").unwrap();
        let selected_at = installation::ChangeWatch::capture(folder.path());
        std::fs::write(&assembly, b"changed before approval").unwrap();

        let (watch, data) = prepare_replacement(folder.path(), || Ok("prepared data")).unwrap();
        assert_ne!(watch, selected_at);
        assert!(!watch.changed(folder.path()));
        assert_eq!(data, "prepared data");

        let result = prepare_replacement(folder.path(), || {
            std::fs::write(&assembly, b"shorter").unwrap();
            Ok("stale data")
        });
        assert_eq!(result.unwrap_err(), "Rust files changed during preparation. Recheck source.");
    }
    #[test]
    fn changed_source_requires_explicit_replacement_and_matching_rebind_is_clean() {
        let mut p = Project {
            installation: Some(identity('a')),
            ..Default::default()
        };
        let old = p.clone();
        let mut panel = completed(&p, identity('b'), false);
        panel.poll(&mut p);
        assert_eq!(p, old);
        assert!(panel.pending.is_some());
        assert!(panel.binding.is_none());
        let mut matching = completed(&p, identity('a'), false);
        matching.poll(&mut p);
        assert_eq!(p, old);
        assert!(matching.binding.is_some());
        assert!(matching.pending.is_none());
    }
    #[test]
    fn cancelled_and_stale_inspection_never_change_the_project() {
        let mut p = Project::default();
        let old = p.clone();
        let mut panel = completed(&p, identity('a'), true);
        panel.poll(&mut p);
        assert_eq!(p, old);
        assert!(panel.binding.is_none());
        let mut panel = completed(&p, identity('a'), false);
        p.installation = Some(identity('b'));
        let changed = p.clone();
        panel.poll(&mut p);
        assert_eq!(p, changed);
        assert!(panel.binding.is_none());
    }

    #[test]
    fn inspecting_a_folder_preserves_recipe_edits_and_an_explicit_backend_change() {
        let mut project = Project {
            preview_engine: mapper_studio::LEGACY_PREVIEW_ENGINE.into(),
            ..Default::default()
        };
        let mut panel = completed(&project, identity('a'), false);
        project.shuffle_monuments();
        project.size = 4000;
        project.preview_engine = papyrus_terrain::landscape::VERSION.into();
        let mut expected = project.clone();
        expected.installation = Some(identity('a'));
        panel.poll(&mut project);
        assert_eq!(project, expected);
        assert!(panel.bound_root(&project).is_some());
    }

    #[test]
    fn pending_replacement_survives_edits_but_expires_when_the_recipe_source_changes() {
        let mut project = Project {
            installation: Some(identity('a')),
            ..Default::default()
        };
        let mut panel = completed(&project, identity('b'), false);
        let ctx = egui::Context::default();
        panel.tick(&mut project, &ctx);
        assert!(panel.has_pending_source());
        project.shuffle_monuments();
        panel.tick(&mut project, &ctx);
        assert!(panel.has_pending_source());
        project.installation = Some(identity('c'));
        panel.tick(&mut project, &ctx);
        assert!(!panel.has_pending_source());
        assert!(panel.bound_root(&project).is_none());
    }

    #[test]
    fn rebind_failure_cancellation_and_late_recipe_edits_preserve_saved_bindings() {
        let saved = Project {
            installation: Some(identity('a')),
            monument_assets: Some(mapper_studio::AssetIdentity {
                catalog_sha256: "a".repeat(64),
                bindings_sha256: "b".repeat(64),
            }),
            ..Default::default()
        };
        for (cancelled, edited) in [(false, false), (true, false), (false, true)] {
            let mut project = saved.clone();
            let mut panel = completed(&project, identity('b'), false);
            let previous = PathBuf::from("previous-rust");
            panel.binding = Some(Binding {
                root: previous.clone(),
                identity: identity('a'),
            });
            panel.poll(&mut project);
            assert_eq!(panel.bound_root(&project), Some(previous.clone()));
            let (sender, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(cancelled));
            panel.rebind = Some(RebindTask {
                saved: saved.clone(),
                binding: Binding { root: "new-rust".into(), identity: identity('b') },
                receiver,
                cancel: cancel.clone(),
            });
            sender.send(Err("Replacement data unavailable".into())).unwrap();
            if edited {
                project.shuffle_monuments();
            }
            assert!(panel.bound_root(&project).is_none());
            let expected = project.clone();
            panel.poll_rebind(&mut project);
            assert_eq!(project, expected);
            assert!(panel.pending.is_some());
            assert!(!panel.inspecting());
            assert!(panel.take_prepared_recipe().is_none());
            assert!(!panel.take_source_changed());
            assert!(cancel.load(Ordering::Relaxed));
            assert_eq!(panel.bound_root(&project), Some(previous));
        }
    }

    #[test]
    fn first_verified_binding_selects_installed_terrain_without_changing_seeds() {
        let mut p = Project::default();
        let seeds = p.seeds;
        let mut source = identity('a');
        source.native.sha256 =
            "0f703d43b57672b8bb3dfd29bab0d484f8622fbac66dee7f1f058893ef22a99c".into();
        let mut panel = completed(&p, source, false);
        panel.poll(&mut p);
        assert_eq!(p.preview_engine, mapper_studio::native_preview::VERSION);
        assert_eq!(p.seeds, seeds);
        p.validate().unwrap();
        assert!(panel.bound_root(&p).is_some());
    }
}
