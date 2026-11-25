//! Disk-backed accepted map output. Completed tabs retain no world-sized grids.
use super::*;
use mapper_studio::project_bundle::RetainedFile;
use papyrus_terrain::monument_composition::Composed;
use std::sync::{
    Arc, Weak,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
use std::task::Poll;

pub(super) struct Archive {
    pub(super) file: RetainedFile,
    pub image: Option<Arc<mapper_studio::image_snapshot::Snapshot>>,
    pub image_issue: String,
}

impl Archive {
    pub(super) fn new(
        file: Arc<tempfile::TempPath>,
        image: Option<Arc<mapper_studio::image_snapshot::Snapshot>>,
        image_issue: String,
    ) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self { file: RetainedFile::new(file)?, image, image_issue }))
    }
}
struct Build {
    key: BuildKey,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Arc<Archive>>, String>>,
}
#[derive(Clone)]
struct BuildKey {
    id: u64,
    recipe: Arc<Project>,
    maps: Weak<Composed>,
    root: PathBuf,
    catalog_sha256: String,
}
impl BuildKey {
    fn matches(&self, id: u64, project: &Arc<Project>, maps: &Arc<Composed>, assets: &Assets) -> bool {
        self.id == id
            && std::ptr::eq(self.maps.as_ptr(), Arc::as_ptr(maps))
            && self.root == assets.root
            && self.catalog_sha256 == assets.catalog.sha256
            && (Arc::ptr_eq(&self.recipe, project) || self.recipe == *project)
    }
}
impl Drop for Build {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
struct Export {
    signal: Arc<project_io::TaskSignal>,
    receiver: Receiver<Result<ExportOutcome, String>>,
    path: PathBuf,
}

enum ExportOutcome {
    Cancelled,
    Published { webhook_error: Option<String> },
}

pub(super) struct Notification {
    endpoint: String,
    size: u32,
    seed: u32,
}

impl Notification {
    pub fn export_label(configured: bool, applied: bool) -> &'static str {
        match (configured, applied) {
            (true, true) => "Export applied .map and notify…",
            (false, true) => "Export applied .map…",
            (true, false) => "Export .map and notify…",
            (false, false) => "Export .map…",
        }
    }
    pub fn from_recipe(recipe: &Project) -> Option<Self> {
        recipe.options.webhook_enabled.then(|| Self {
            endpoint: recipe.options.webhook_url.clone(),
            size: recipe.size,
            seed: recipe.master_seed,
        })
    }

    fn send(&self, path: &std::path::Path) -> Result<(), String> {
        use std::{io::Write, process::{Command, Stdio}};
        let destination = self.endpoint.strip_prefix("https://")
            .filter(|rest| !rest.is_empty() && !rest.chars().any(|c| {
                c.is_control() || c.is_whitespace() || matches!(c, '"' | '\\')
            })).ok_or("Enter an HTTPS webhook endpoint in the recipe.")?;
        let filename = path.file_name().and_then(|name| name.to_str()).unwrap_or("Rust map");
        let body = serde_json::json!({
            "content": format!("Exported {filename} ({} m, seed {})", self.size, self.seed),
            "allowed_mentions": {"parse": []},
            "filename": filename,
            "size": self.size,
            "seed": self.seed,
        }).to_string().replace('\\', "\\\\").replace('"', "\\\"");
        let mut command = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        // Supply the token through curl's stdin config. Suppress per-user
        // config, redirects, URL expansion, and response bodies.
        let mut child = command.args([
            "-q", "--config", "-", "--proto", "=https", "--globoff",
            "--connect-timeout", "3", "--max-time", "8",
            "--fail", "--silent", "--output", if cfg!(windows) { "NUL" } else { "/dev/null" },
        ]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|_| "Cannot start the HTTPS webhook client.")?;
        let input = format!("url = \"https://{destination}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{body}\"\n");
        let wrote = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(input.as_bytes()).is_ok());
        let status = child.wait().map_err(|_| "Could not finish the webhook request.")?;
        if !wrote || !status.success() {
            return Err("The webhook endpoint did not accept the export notification.".into());
        }
        Ok(())
    }
}
impl Drop for Export {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}
#[derive(Default)]
pub(super) struct Panel {
    build: Option<Build>,
    ready: Option<(BuildKey, Result<Arc<Archive>, String>)>,
    export: Option<Export>,
    message: String,
    message_is_error: bool,
    exported: Option<export_panel::ExportedFile>,
}
impl Panel {
    pub fn preparing(&self) -> bool {
        self.build.is_some()
    }
    pub fn cancel_build(&mut self) {
        self.build = None;
        self.ready = None;
    }
    pub fn prepare(
        &mut self,
        id: u64,
        view: &composition_panel::View,
        assets: Assets,
        ctx: &egui::Context,
    ) -> Option<Result<Arc<Archive>, String>> {
        let recipe = view.recipe();
        match self.poll_build(id, recipe, &view.maps, &assets, ctx) {
            Some(Poll::Ready(result)) => return Some(result),
            Some(Poll::Pending) => return None,
            None => {}
        }
        let project = recipe.clone();
        let key = BuildKey {
            id,
            recipe: project.clone(),
            maps: Arc::downgrade(&view.maps),
            root: assets.root.clone(),
            catalog_sha256: assets.catalog.sha256.clone(),
        };
        let solution = view.solution.clone();
        let maps = view.maps.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        if let Err(error) = std::thread::Builder::new().name("mapper-map-archive".into()).spawn(move || {
            let result = build_archive(&project, &maps, &solution, &assets, &signal);
            let _ = sender.send(result);
            repaint.request_repaint();
        }) {
            let result = Err(format!("Could not start map archive worker. {error}"));
            self.ready = Some((key, result.clone()));
            return Some(result);
        }
        self.build = Some(Build { key, cancel, receiver });
        None
    }

    fn poll_build(
        &mut self,
        id: u64,
        project: &Arc<Project>,
        maps: &Arc<Composed>,
        assets: &Assets,
        ctx: &egui::Context,
    ) -> Option<Poll<Result<Arc<Archive>, String>>> {
        let matches = |key: &BuildKey| key.matches(id, project, maps, assets);
        if let Some((key, result)) = &self.ready
            && matches(key)
        {
            return Some(Poll::Ready(result.clone()));
        }
        if let Some(build) = &self.build
            && matches(&build.key)
        {
            match build.receiver.try_recv() {
                Ok(result) => {
                    let result = result.and_then(|r| r.ok_or("Map archive cancelled".into()));
                    return Some(Poll::Ready(self.store_result(result)));
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(80));
                    return Some(Poll::Pending);
                }
                Err(TryRecvError::Disconnected) => {
                    let result = Err("Map archive worker stopped".into());
                    return Some(Poll::Ready(self.store_result(result)));
                }
            }
        }
        self.cancel_build();
        None
    }

    fn store_result(&mut self, result: Result<Arc<Archive>, String>) -> Result<Arc<Archive>, String> {
        let build = self.build.take().expect("received a map archive result without a worker");
        self.ready = Some((build.key.clone(), result.clone()));
        result
    }
    pub fn export(
        &mut self,
        archive: Arc<Archive>,
        label: &str,
        notification: Option<Notification>,
        directory: Option<PathBuf>,
        ctx: &egui::Context,
    ) {
        if self.exporting() {
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export Rust map")
            .add_filter("Rust map", &["map"])
            .set_file_name(format!("{}.map", label.replace(['/', '\\', ':'], "-")));
        if let Some(directory) = directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        self.exported = None;
        let signal = Arc::new(project_io::TaskSignal::default());
        let worker = signal.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        let destination = path.clone();
        if let Err(error) = std::thread::Builder::new().name("mapper-map-export".into()).spawn(move || {
            let result = copy_archive(
                &archive, &destination, || worker.cancelled(), || {
                    let publishing = worker.begin_publish();
                    repaint.request_repaint();
                    publishing
                },
            ).map(|published| {
                if published {
                    ExportOutcome::Published {
                        webhook_error: notification.and_then(|notification| notification.send(&destination).err()),
                    }
                } else {
                    ExportOutcome::Cancelled
                }
            });
            let _ = sender.send(result);
            repaint.request_repaint();
        }) {
            self.message = format!("Could not start map export. {error}");
            self.message_is_error = true;
            ctx.request_repaint();
            return;
        }
        self.export = Some(Export { signal, receiver, path });
        self.message = "Exporting map…".into();
        self.message_is_error = false;
        ctx.request_repaint();
    }

    pub fn exporting(&self) -> bool {
        self.export.is_some()
    }

    pub fn poll_export(&mut self) -> Option<PathBuf> {
        let outcome = self.export.as_ref()?.receiver.try_recv();
        if matches!(outcome, Err(TryRecvError::Empty)) {
            return None;
        }
        let export = self.export.take().unwrap();
        let completed = matches!(outcome, Ok(Ok(ExportOutcome::Published { .. })))
            .then(|| export.path.clone());
        self.message_is_error = matches!(outcome,
            Ok(Err(_)) | Err(TryRecvError::Disconnected) | Ok(Ok(ExportOutcome::Published { webhook_error: Some(_) })));
        self.message = match outcome {
            Ok(Ok(ExportOutcome::Published { webhook_error: None })) => "Map exported".into(),
            Ok(Ok(ExportOutcome::Published { webhook_error: Some(error) })) => format!("Map exported. {error}"),
            Ok(Ok(ExportOutcome::Cancelled)) => "Map export cancelled".into(),
            Ok(Err(error)) => format!("Map export failed. {error}"),
            Err(TryRecvError::Disconnected) => "Map export worker stopped".into(),
            Err(TryRecvError::Empty) => unreachable!(),
        };
        self.exported = completed.clone().map(export_panel::ExportedFile::new);
        completed
    }

    pub fn export_message(&self) -> &str {
        &self.message
    }

    pub fn dismiss_export_message(&mut self) {
        if !self.exporting() {
            self.message.clear();
            self.exported = None;
        }
    }

    pub fn show_export(&mut self, ui: &mut egui::Ui) {
        let cancellable = self.export.as_ref().filter(|export| export.signal.can_cancel());
        let message = match (&self.export, cancellable) {
            (Some(export), None) if export.signal.cancelled() => "Cancelling map export…",
            (Some(_), None) => "Finishing map export…",
            _ => &self.message,
        };
        if !message.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(message).small()).truncate())
                .on_hover_text(message);
        }
        if self.message_is_error {
            crate::diagnostic_ui::copy_error(ui, &self.message, None);
        }
        if let Some(exported) = &mut self.exported {
            exported.show(ui);
        }
        if let Some(export) = cancellable
            && ui.button("Cancel map export").clicked()
        {
            export.signal.cancel();
        }
    }
}

pub(super) struct Assets {
    pub root: PathBuf,
    pub catalog: Arc<mapper_studio::prefab_assets::Catalog>,
    pub identities: Option<Arc<mapper_studio::client_preparation::ReadyIdentities>>,
}

fn build_archive(
    project: &Project,
    maps: &Composed,
    solution: &papyrus_terrain::placement::Solution,
    assets: &Assets,
    cancel: &AtomicBool,
) -> Result<Option<Arc<Archive>>, String> {
    let input = mapper_studio::map_export::Input {
        project,
        maps,
        solution,
        root: &assets.root,
        catalog: &assets.catalog,
    };
    let cancelled = || cancel.load(Ordering::Relaxed);
    let Some(file) = mapper_studio::map_export::retain(
        input, assets.identities.as_deref(), cancelled,
    )? else {
        return Ok(None);
    };
    let (image, image_issue) = match mapper_studio::image_snapshot::Snapshot::capture(maps, cancelled) {
        Ok(value) => (value, String::new()),
        Err(error) => (None, error),
    };
    if cancelled() {
        return Ok(None);
    }
    Archive::new(Arc::new(file), image, image_issue).map(Some)
}

fn copy_archive(
    archive: &Archive,
    destination: &std::path::Path,
    cancel: impl Fn() -> bool,
    begin_publish: impl FnOnce() -> bool,
) -> Result<bool, String> {
    use std::io::{Read, Write};
    const CHANGED: &str = "The retained map changed after it was accepted";
    if cancel() {
        return Ok(false);
    }
    let mut input = std::fs::File::open(archive.file.as_ref()).map_err(|e| e.to_string())?;
    archive.file.check(&input)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut buffer = vec![0; 256 * 1024];
    let mut remaining = archive.file.byte_len();
    while remaining != 0 {
        if cancel() {
            return Ok(false);
        }
        let count = remaining.min(buffer.len() as u64) as usize;
        let chunk = &mut buffer[..count];
        input.read_exact(chunk).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                CHANGED.into()
            } else {
                error.to_string()
            }
        })?;
        output.write_all(chunk).map_err(|e| e.to_string())?;
        remaining -= count as u64;
    }
    if cancel() {
        return Ok(false);
    }
    archive.file.check(&input)?;
    output.as_file().sync_all().map_err(|e| e.to_string())?;
    if !begin_publish() {
        return Ok(false);
    }
    output
        .persist(destination)
        .map_err(|e| e.error.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }

    #[test]
    fn archive_jobs_require_the_exact_recipe_world_and_source() {
        use mapper_studio::installation::{Installation, InstallationKind, SourceFile};

        let ctx = egui::Context::default();
        let project = Arc::new(Project { size: 1000, ..Project::default() });
        let mut composition = composition_panel::CompositionPanel::default();
        composition.seed_test_view(&ctx, &project);
        let maps = composition.view(&project).unwrap().maps.clone();
        let mut assets = Assets {
            root: "rust".into(),
            catalog: Arc::new(mapper_studio::prefab_assets::Catalog {
                sha256: "a".repeat(64),
                installation: Installation {
                    kind: InstallationKind::Client,
                    native: SourceFile {
                        relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                        sha256: "b".repeat(64),
                        bytes: 1,
                    },
                    scene_manifest: None,
                },
                entries: Default::default(),
                sources: Vec::new(),
            }),
            identities: None,
        };
        let key = BuildKey {
            id: 1,
            recipe: composition.view(&project).unwrap().recipe().clone(),
            maps: Arc::downgrade(&maps),
            root: assets.root.clone(),
            catalog_sha256: assets.catalog.sha256.clone(),
        };
        let mut panel = Panel::default();
        let file = Arc::new(tempfile::NamedTempFile::new().unwrap().into_temp_path());
        let archive = Archive::new(file, None, String::new()).unwrap();
        panel.ready = Some((key.clone(), Ok(archive.clone())));
        let Some(Poll::Ready(Ok(result))) = panel.poll_build(1, &project, &maps, &assets, &ctx) else {
            panic!("Current archive was discarded");
        };
        assert!(Arc::ptr_eq(&archive, &result));
        let mut changed = project.clone();
        Arc::make_mut(&mut changed).terrain_overrides = Some(mapper_studio::native_profile::TerrainOverrides {
            tier_angle: 0,
            tier_weights: [3000, 3000, 4000],
        });
        assert_eq!(changed.terrain_key(), project.terrain_key());
        assert!(panel.poll_build(1, &changed, &maps, &assets, &ctx).is_none());
        assert!(panel.ready.is_none());

        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Some(archive))).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        panel.build = Some(Build { key: key.clone(), cancel: cancel.clone(), receiver });
        composition.seed_test_view(&ctx, &project);
        let replaced_maps = &composition.view(&project).unwrap().maps;
        assert!(panel.poll_build(1, &project, replaced_maps, &assets, &ctx).is_none());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!panel.preparing());

        panel.ready = Some((key, Err("Archive for the previous source".into())));
        assets.root = "different-rust".into();
        assert!(panel.poll_build(1, &project, &maps, &assets, &ctx).is_none());
        assert!(panel.ready.is_none());
    }

    #[test]
    fn map_export_status_follows_the_worker_with_queue_closed() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.);
        let mut app = crate::tests::fixture(&ctx);
        let queue = app.dock.find_tab(&Tab::Queue).unwrap();
        app.dock.remove_tab(queue);
        let signal = Arc::new(project_io::TaskSignal::default());
        let (sender, receiver) = mpsc::channel();
        app.map_archive.export = Some(Export {
            signal: signal.clone(),
            receiver,
            path: PathBuf::from("generated.map"),
        });
        app.map_archive.message = "Exporting map…".into();

        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let render = |app: &mut Studio, events| {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(720., 260.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::top("toolbar")
                        .frame(egui::Frame::NONE.fill(ui.visuals().extreme_bg_color)
                            .inner_margin(egui::Margin::symmetric(6, 3)))
                        .show(ui, |ui| app.toolbar(ui));
                },
            );
            textures.borrow_mut().update(&frame.textures_delta);
            frame.textures_delta.clear();
            frame
        };
        let position = |frame: &egui::FullOutput, label: &str| {
            frame.shapes.iter().find_map(|shape| {
                let egui::Shape::Text(text) = &shape.shape else { return None };
                let center = text.pos + text.galley.rect.center().to_vec2();
                (text.galley.job.text == label && shape.clip_rect.contains(center)).then_some(center)
            }).unwrap_or_else(|| panic!("Missing visible export control: {label}"))
        };
        let click = |app: &mut Studio, pos| {
            for pressed in [true, false] {
                render(app, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };
        let frame = render(&mut app, vec![]);
        click(&mut app, position(&frame, "Exporting…"));
        let opened = render(&mut app, vec![]);
        if let Some(directory) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
            let path = PathBuf::from(directory).join("map-export-status.png");
            let image = textures.borrow().raster(&ctx, opened.clone(), [720, 260]);
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
        click(&mut app, position(&opened, "Cancel map export"));
        assert!(signal.cancelled());
        position(&render(&mut app, vec![]), "Cancelling map export…");
        app.poll_exports();
        assert!(app.map_archive.exporting());

        assert!(!signal.begin_publish());
        sender.send(Ok(ExportOutcome::Cancelled)).unwrap();
        app.poll_exports();
        assert!(!app.map_archive.exporting());
        assert_eq!(app.map_archive.export_message(), "Map export cancelled");
        assert!(app.dock.find_tab(&Tab::Queue).is_none());
        position(&render(&mut app, vec![]), "Export status");
        assert!(app.preferences.export_folder.is_none());

        let folder = tempfile::tempdir().unwrap();
        let location = folder.path().join("moved-export");
        let (sender, receiver) = mpsc::channel();
        app.map_archive.export = Some(Export {
            signal: Arc::new(project_io::TaskSignal::default()),
            receiver,
            path: location.join("standard-4000.map"),
        });
        sender.send(Ok(ExportOutcome::Published { webhook_error: None })).unwrap();
        app.poll_exports();
        assert_eq!(app.preferences.export_folder.as_deref(), Some(location.as_path()));

        click(&mut app, Pos2::new(20., 220.));
        let frame = render(&mut app, vec![]);
        click(&mut app, position(&frame, "Export status"));
        let frame = render(&mut app, vec![]);
        let _ = position(&frame, "standard-4000.map");
        let open_folder = position(&frame, "Open folder");
        if let Some(directory) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
            let path = PathBuf::from(directory).join("map-export-complete.png");
            let image = textures.borrow().raster(&ctx, frame, [720, 260]);
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
        click(&mut app, open_folder);
        let frame = render(&mut app, vec![]);
        position(&frame, "The export folder is unavailable.");
        click(&mut app, position(&frame, "Dismiss"));
        assert!(app.map_archive.export_message().is_empty());
    }

    #[test]
    fn export_cancellation_and_failed_reads_preserve_existing_destination() {
        let folder = tempfile::tempdir().unwrap();
        let source = tempfile::NamedTempFile::new_in(folder.path()).unwrap().into_temp_path();
        let destination = folder.path().join("output.map");
        std::fs::write(&source, vec![7; 600_000]).unwrap();
        std::fs::write(&destination, b"original").unwrap();
        let accepted = Archive::new(Arc::new(source), None, String::new()).unwrap();
        let reads = std::cell::Cell::new(0);
        assert!(
            !copy_archive(&accepted, &destination, || {
                reads.set(reads.get() + 1);
                reads.get() > 2
            }, || true)
            .unwrap()
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        assert!(!copy_archive(&accepted, &destination, || false, || false).unwrap());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        assert!(copy_archive(&accepted, &destination, || false, || true).unwrap());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            std::fs::read(accepted.file.as_ref()).unwrap()
        );
        let placeholder = tempfile::NamedTempFile::new_in(folder.path())
            .unwrap()
            .into_temp_path();
        let retained_path = placeholder.to_path_buf();
        assert!(copy_archive(&accepted, &retained_path, || false, || true).unwrap());
        let archive = Archive::new(Arc::new(placeholder), None, String::new()).unwrap();
        assert_eq!(
            std::fs::read(archive.file.as_ref()).unwrap(),
            std::fs::read(accepted.file.as_ref()).unwrap()
        );
        drop(archive);
        assert!(!retained_path.exists());
        std::fs::remove_file(accepted.file.as_ref()).unwrap();
        assert!(copy_archive(&accepted, &destination, || false, || true).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), vec![7; 600_000]);
    }

    #[test]
    fn changed_archive_cannot_replace_an_existing_map() {
        let folder = tempfile::tempdir().unwrap();
        let source = Arc::new(tempfile::NamedTempFile::new_in(folder.path()).unwrap().into_temp_path());
        let destination = folder.path().join("output.map");
        std::fs::write(&destination, b"original").unwrap();
        for (change_before_copy, modified_len) in [(true, 10), (false, 10), (false, 700_000)] {
            std::fs::write(source.as_ref(), vec![7; 600_000]).unwrap();
            let accepted = Archive::new(source.clone(), None, String::new()).unwrap();
            let change = || {
                std::fs::OpenOptions::new().write(true).open(source.as_ref()).unwrap()
                    .set_len(modified_len).unwrap();
            };
            if change_before_copy {
                change();
            }
            let checks = std::cell::Cell::new(0);
            let result = copy_archive(&accepted, &destination, || {
                checks.set(checks.get() + 1);
                if !change_before_copy && checks.get() == 3 {
                    change();
                }
                false
            }, || panic!("A changed archive reached publication"));
            assert_eq!(result.unwrap_err(), "The retained map changed after it was accepted");
            assert_eq!(std::fs::read(&destination).unwrap(), b"original");
            assert_eq!(folder.path().read_dir().unwrap().count(), 2);
        }
    }
}
