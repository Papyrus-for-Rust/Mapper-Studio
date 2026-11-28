use super::*;
use mapper_studio::generation_resources::{JobBudget, Limits, with_job_budget};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Default)]
pub(super) struct QueueExecution {
    pub(super) drain_parallel: bool,
    pub(super) requested_parallel: std::collections::BTreeSet<u64>,
    pub(super) next_primary: Option<u64>,
    pub(super) primary_paused: bool,
    pub(super) advance_pending: bool,
    pub(super) start_pending: Option<bool>,
    pub(super) placement_pending: bool,
    pub(super) frozen_primary: Option<Instant>,
    pub(super) jobs: std::collections::BTreeMap<u64, Extra>,
    pub(crate) primary_budget: Option<JobBudget>,
    pub(crate) retiring: Vec<JobBudget>,
}

// Reserve working terrain layers, temporary copies, and space for a preview.
fn terrain_reservation(recipe: &Project) -> usize {
    const MIB: usize = 1024 * 1024;
    let Ok((height, surface)) = papyrus_terrain::native_layers::dimensions(recipe.size) else {
        return 64;
    };
    if !mapper_studio::native_preview::is_native(&recipe.preview_engine) {
        return 64;
    }
    let layers = height * height * 2 + surface * surface * (5 + 8 + 4);
    let erosion = layers + height * height * (44 + 4);
    (4 * layers).max(erosion).div_ceil(MIB) + 128
}

fn admission_blocker(
    limits: Limits,
    active_mib: impl Iterator<Item = usize>,
    request_mib: usize,
    automatic: bool,
) -> Option<&'static str> {
    let (active, memory) = active_mib.fold((0, 0usize), |(count, sum), bytes| (count + 1, sum + bytes));
    if limits.concurrent_maps.is_some_and(|maximum| active >= maximum) {
        return Some("Waiting for a generation slot");
    }
    if active >= limits.effective_workers() {
        return Some("Waiting for CPU workers");
    }
    // Keep outstanding reservations while jobs allocate their buffers. Auto
    // waits even when idle, since other processes may already be near the target.
    if (active != 0 || automatic) && memory.saturating_add(request_mib) > limits.terrain_memory_mib {
        return Some(if automatic { "Waiting for system RAM" } else { "Waiting for terrain memory" });
    }
    None
}

impl Generation {
    pub(crate) fn defer_queue_placement(&mut self) {
        self.execution.placement_pending = true;
    }
}

struct Assets {
    root: Option<PathBuf>,
    catalog: Option<Arc<mapper_studio::prefab_assets::Catalog>>,
    bindings: Option<Arc<mapper_studio::prefab_textures::Bindings>>,
    identities: Option<Arc<mapper_studio::client_preparation::ReadyIdentities>>,
    monuments: Option<Arc<mapper_studio::monument_source::Prepared>>,
}

type Overview = Arc<tempfile::TempPath>;
type OverviewResult = Result<Option<Overview>, String>;

pub(super) struct Extra {
    id: u64,
    budget: JobBudget,
    project: Project,
    assets: Assets,
    started: Instant,
    paused: bool,
    frozen: Option<Instant>,
    terrain: Option<Task>,
    preview: Option<Preview>,
    placement: Option<LayoutTask>,
    layout: Option<Layout>,
    composition: composition_panel::CompositionPanel,
    archive: map_archive::Panel,
    colors: color_panel::Panel,
    overview: Option<Receiver<OverviewResult>>,
    retained_overview: Option<Overview>,
    stop_overview: Arc<AtomicBool>,
    stage: Cow<'static, str>,
    live: Option<egui::TextureHandle>,
    color_preferences: mapper_studio::map_colors::Preferences,
}

impl Drop for Extra {
    fn drop(&mut self) {
        if let Some(task) = &self.placement {
            task.cancel.store(true, Ordering::Relaxed);
        }
        self.stop_overview.store(true, Ordering::Relaxed);
    }
}

fn receive<T>(rx: &Receiver<Result<Option<T>, String>>) -> Result<Option<T>, String> {
    match rx.try_recv() {
        Ok(Ok(None)) => Err("Generation stopped before producing a result".into()),
        Ok(result) => result,
        Err(TryRecvError::Empty) => Ok(None),
        Err(TryRecvError::Disconnected) => Err("Generation worker stopped unexpectedly".into()),
    }
}

impl Extra {
    fn elapsed(&self) -> Duration {
        self.frozen
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started)
    }

    fn in_worker(&self) -> bool {
        self.terrain.is_some()
            || self.placement.is_some()
            || self.composition.running()
            || self.archive.preparing()
            || self.colors.running()
            || self.overview.is_some()
    }

    fn set_live(&mut self, image: egui::ColorImage, ctx: &egui::Context) {
        let options = egui::TextureOptions::LINEAR;
        if let Some(texture) = &mut self.live {
            texture.set(image, options);
        } else {
            self.live = Some(ctx.load_texture(format!("queue-live-{}", self.id), image, options));
        }
    }

    fn tick(
        &mut self,
        ctx: &egui::Context,
        queue_paused: bool,
        live: bool,
    ) -> Result<Option<Completed>, String> {
        let pause = queue_paused || self.paused;
        if !pause && let Some(at) = self.frozen.take() {
            self.started += at.elapsed();
        }

        if let Some(task) = &self.terrain {
            if let Some(progress) = task.progress.lock().ok().and_then(|mut slot| slot.take()) {
                self.stage = progress.label.into();
                let image = live.then_some(progress.image).flatten();
                let preview = receive(&task.receiver)?;
                if let Some(image) = image {
                    self.set_live(image, ctx);
                }
                if let Some(preview) = preview {
                    self.terrain = None;
                    self.preview = Some(preview);
                }
            } else if let Some(preview) = receive(&task.receiver)? {
                self.terrain = None;
                self.preview = Some(preview);
            }
        }
        if let Some(task) = &self.placement
            && let Some(layout) = receive(&task.receiver)?
        {
            self.placement = None;
            self.layout = Some(layout);
        }
        if let Some(rx) = &self.overview
            && let Some(overview) = receive(rx)?
        {
            self.overview = None;
            self.retained_overview = Some(overview);
        }
        self.composition.set_live_preview(live);
        if self.composition.running() && pause {
            self.composition.poll(ctx);
        }
        if live && let Some(image) = self.composition.take_live_image() {
            self.set_live(image, ctx);
        }
        if pause && !self.in_worker() {
            self.frozen.get_or_insert_with(Instant::now);
        }
        if self.terrain.is_some() || self.placement.is_some() || (pause && !self.in_worker()) {
            return Ok(None);
        }

        let preview = self
            .preview
            .as_ref()
            .ok_or("Terrain preview is unavailable")?;
        let selects = mapper_studio::composition::selects_monument_layout(&self.project);
        if !selects && self.layout.is_none() && self.composition.view(&self.project).is_none() {
            if !pause {
                self.stage = "Placing monuments".into();
                self.placement = Some(LayoutTask::start(
                    self.project.clone(),
                    preview,
                    ctx.clone(),
                    self.assets
                        .catalog
                        .clone()
                        .zip(self.assets.bindings.clone()),
                    self.assets.monuments.clone(),
                ));
            }
            return Ok(None);
        }
        let compose = self.project.composition_engine.is_some()
            && mapper_studio::native_preview::is_native(&self.project.preview_engine);
        if compose {
            self.stage = "Composing map".into();
            if !pause {
                self.composition.tick(
                    &self.project,
                    composition_panel::Inputs {
                        source: Some(preview),
                        solution: self.layout.as_ref().map(|layout| &layout.solution),
                        catalog: self.assets.catalog.clone(),
                        bindings: self.assets.bindings.clone(),
                        monuments: self.assets.monuments.clone(),
                        identities: self.assets.identities.clone(),
                        road_layout: None,
                    },
                    ctx,
                );
            }
            if !self.composition.completion(&self.project)? {
                return Ok(None);
            }
        }

        let view = self.composition.view(&self.project);
        let source = if let Some(view) = view {
            view.palette.as_ref().map(|palette| {
                (
                    color_panel::Base::Composed(view.maps.clone()),
                    palette.clone(),
                )
            })
        } else {
            preview
                .native_layers
                .as_ref()
                .zip(preview.palette.as_ref())
                .map(|(layers, palette)| {
                    (
                        color_panel::Base::Native(layers.clone(), preview.ocean.clone()),
                        palette.clone(),
                    )
                })
        };
        if let Some((base, palette)) = source.as_ref() {
            self.colors
                .tick_with_clean(source.clone(), None, &self.color_preferences, ctx);
            if !self
                .colors
                .completion_for(base, palette, &self.color_preferences)?
            {
                self.stage = "Rendering map colors".into();
                return Ok(None);
            }
        }
        let image = source
            .as_ref()
            .and_then(|(base, palette)| {
                self.colors.view_for(base, palette, &self.color_preferences)
            })
            .map(|paint| paint.image.clone())
            .or_else(|| view.map(|view| view.image.clone()))
            .unwrap_or_else(|| preview.viewport_image().clone());
        if pause && !self.archive.preparing() {
            if self.overview.is_none() {
                self.frozen.get_or_insert_with(Instant::now);
            }
            return Ok(None);
        }
        let archive = if let Some(view) = view {
            let assets = map_archive::Assets {
                root: self
                    .assets
                    .root
                    .clone()
                    .ok_or("Rust installation is unavailable")?,
                catalog: self
                    .assets
                    .catalog
                    .clone()
                    .ok_or("Verified assets are unavailable")?,
                identities: self.assets.identities.clone(),
            };
            let Some(archive) = self.archive.prepare(self.id, view, assets, ctx) else {
                self.stage = "Saving map".into();
                return Ok(None);
            };
            archive.map(Some)
        } else {
            Ok(None)
        };
        if pause {
            if self.overview.is_none() {
                self.frozen.get_or_insert_with(Instant::now);
            }
            return Ok(None);
        }
        if self.overview.is_none() && self.retained_overview.is_none() {
            let signal = self.stop_overview.clone();
            let raster = image.clone();
            let receiver = preview::start_worker("mapper-queue-overview", ctx.clone(), move |_| {
                mapper_studio::image_export::retain_overview(&raster, || {
                    signal.load(Ordering::Relaxed)
                })
                .map(|file| file.map(Arc::new))
            });
            self.overview = Some(receiver);
            self.stage = "Retaining map overview".into();
        }
        let Some(overview) = self.retained_overview.take() else {
            return Ok(None);
        };
        let solution = view
            .map(|view| &view.solution)
            .or_else(|| self.layout.as_ref().map(|layout| &layout.solution))
            .ok_or("Monument layout is unavailable")?;
        let mut monuments = map_monuments::initial(&self.project, solution);
        let (routes, road_layout) = view.map_or_else(
            || (Vec::new(), None),
            |view| {
                monuments.extend(view.generated_monuments.iter().cloned());
                (
                    mapper_studio::path_edit::capture(&self.project, &view.maps),
                    view.accepted_road_layout(),
                )
            },
        );
        let (archive, archive_issue) = match archive {
            Ok(archive) => (archive, String::new()),
            Err(error) => (None, error),
        };
        let missing = solution.required_missing;
        let issue =
            mapper_studio::generation_validation::required_monuments_issue(&self.project, solution)
                .or_else(|| {
                    completion_issue(&self.project, missing, archive.is_some(), &archive_issue)
                });
        let background = ocean_background(
            source.as_ref().map(|(_, palette)| palette.as_ref()),
            &self.color_preferences,
        );
        let palette = source.map(|(_, p)| Arc::new(p.with_display_colors(&self.color_preferences)));
        Ok(Some(Completed {
            image,
            overview,
            archive,
            issue,
            missing,
            monuments,
            routes,
            road_layout,
            palette,
            background,
        }))
    }
}

struct Completed {
    image: Arc<egui::ColorImage>,
    overview: Overview,
    archive: Option<Arc<map_archive::Archive>>,
    issue: Option<String>,
    missing: usize,
    monuments: Vec<map_monuments::Monument>,
    routes: Vec<mapper_studio::path_edit::Route>,
    road_layout: Option<mapper_studio::source_road_planning::AcceptedRoadLayout>,
    palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    background: Color32,
}

impl Studio {
    fn queue_primary_id(&self) -> Option<u64> {
        self.generation.run.as_ref().map(|run| run.active_id)
    }

    pub(crate) fn queue_primary_paused(&self) -> bool {
        self.generation.busy()
            && (self.generation.queue_paused || self.generation.execution.primary_paused)
    }

    pub(super) fn queue_job_paused(&self, id: u64) -> bool {
        if self.queue_primary_id() == Some(id) {
            return self.queue_primary_paused();
        }
        self.generation
            .execution
            .jobs.get(&id)
            .is_some_and(|job| self.generation.queue_paused || job.paused)
    }

    pub(super) fn queue_pause_caption(&self, id: u64) -> &'static str {
        let frozen = if self.queue_primary_id() == Some(id) {
            self.generation.execution.frozen_primary.is_some()
        } else {
            self.generation
                .execution
                .jobs.get(&id)
            .is_some_and(|job| job.frozen.is_some())
        };
        if frozen { "Paused" } else { "Pausing…" }
    }

    pub(super) fn queue_elapsed(&self, id: u64) -> Option<Duration> {
        if let Some(job) = self
            .generation
            .execution
            .jobs.get(&id)
        {
            return Some(job.elapsed());
        }
        if self.queue_primary_id() == Some(id)
            && let Some(start) = self.generation.started
        {
            return Some(
                self.generation
                    .execution
                    .frozen_primary
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(start),
            );
        }
        self.generation.elapsed.get(&id).copied()
    }

    pub(super) fn extra_queue_stage(&self, id: u64) -> (Cow<'static, str>, Option<f32>) {
        let Some(job) = self
            .generation
            .execution
            .jobs.get(&id)
        else {
            return ("Preparing map".into(), None);
        };
        if let Some((label, fraction)) = job.composition.progress() {
            return (label.into(), fraction);
        }
        (job.stage.clone(), None)
    }

    pub(super) fn extra_live_texture(&self, id: u64) -> Option<&egui::TextureHandle> {
        self.generation
            .execution
            .jobs.get(&id)
            .and_then(|job| job.live.as_ref())
    }

    fn capacity_blocker(&self, recipe: &Project, include_primary: bool) -> Option<&'static str> {
        let execution = &self.generation.execution;
        let primary = execution.primary_budget.as_ref().filter(|_| include_primary);
        let auto_headroom = self.preferences.auto_terrain_headroom();
        let mut limits = self.preferences.generation_resources;
        if let Some(headroom) = auto_headroom {
            limits.terrain_memory_mib = headroom;
        }
        admission_blocker(
            limits,
            execution.jobs.values().map(|job| &job.budget)
                .chain(execution.retiring.iter().filter(|budget| budget.has_workers()))
                .chain(primary)
                .map(JobBudget::terrain_mib),
            self.terrain_request(recipe, auto_headroom),
            auto_headroom.is_some(),
        )
    }

    fn reserve_terrain(&self, recipe: &Project) -> usize {
        self.terrain_request(recipe, self.preferences.auto_terrain_headroom())
    }

    fn terrain_request(&self, recipe: &Project, auto_headroom: Option<usize>) -> usize {
        let manual = self.preferences.generation_resources.terrain_memory_mib;
        // A custom graph can have an arbitrary number of live terrain buffers.
        if recipe.terrain_graph.is_some() {
            auto_headroom.map_or(manual, |headroom| headroom.clamp(256, 16384))
        } else if auto_headroom.is_some() {
            terrain_reservation(recipe)
        } else {
            terrain_reservation(recipe).min(manual)
        }
    }

    pub(super) fn reserve_primary_budget(&mut self) {
        self.generation.execution.primary_budget = Some(JobBudget::new(self.reserve_terrain(&self.project)));
    }

    pub(super) fn retire_primary_budget(&mut self) {
        if let Some(budget) = self.generation.execution.primary_budget.take()
            && budget.has_workers()
        {
            self.generation.execution.retiring.push(budget);
        }
    }

    pub(super) fn primary_waits_for_capacity(&self, recipe: &Project) -> bool {
        self.capacity_blocker(recipe, false).is_some()
    }

    pub(crate) fn run_blocker(&self) -> Option<&'static str> {
        self.generation.queue_paused.then_some("Resume the queue before starting maps.")
    }

    fn eligible_queue_job(&self, id: u64) -> bool {
        self.generation.queue.iter().any(|job| job.id == id && job.state == State::Waiting)
            // Applying a map edit uses the accepted route layout owned by the editor.
            && self.generation.edits.values().all(|edit| edit.job != Some(id))
    }

    pub(super) fn can_start_queue_job(&self, id: u64) -> bool {
        !self.generation.queue_paused && self.eligible_queue_job(id)
    }

    pub(super) fn queue_wait_caption(&self, id: u64) -> Option<&'static str> {
        let execution = &self.generation.execution;
        if self.generation.queue_paused {
            return Some("Queue paused");
        }
        if self.queue_primary_id() == Some(id) && execution.start_pending.is_some() {
            return self.capacity_blocker(&self.project, false);
        }
        if execution.drain_parallel || execution.requested_parallel.contains(&id) {
            let item = self.generation.queue.iter().find(|item| item.id == id && item.state == State::Waiting)?;
            return self.capacity_blocker(&item.recipe, true);
        }
        None
    }

    pub(super) fn request_queue_job(&mut self, id: u64, ctx: &egui::Context) {
        if self.can_start_queue_job(id) {
            self.generation.execution.requested_parallel.insert(id);
            self.generation.execution.next_primary = Some(id);
            self.start_requested_parallel(ctx);
            ctx.request_repaint();
        }
    }

    pub(super) fn start_queue_job(&mut self, id: u64, ctx: &egui::Context) {
        if !self.can_start_queue_job(id) {
            return;
        }
        let item = self
            .generation
            .queue
            .iter()
            .find(|item| item.id == id)
            .unwrap();
        let recipe = item.recipe.clone();
        if self.capacity_blocker(&recipe, true).is_some() {
            return;
        }
        if let Err(error) = self.generation_preflight(&recipe, false) {
            if let Some(item) = self.generation.queue.iter_mut().find(|item| item.id == id) {
                item.state = State::Failed;
                item.detail = error.clone();
            }
            self.generation.revision += 1;
            self.status = error;
            return;
        }
        let root = self.source.bound_root(&recipe);
        let palette = self.prefabs.prepared_palette(&recipe, root.as_deref());
        let prepared = self
            .prefabs
            .catalog(&recipe)
            .zip(self.prefabs.bindings(&recipe));
        let identities = root
            .as_ref()
            .and_then(|path| self.prefabs.prepared_identities(&recipe, path));
        let (catalog, bindings) = prepared.map_or((None, None), |(catalog, bindings)| {
            (Some(catalog), Some(bindings))
        });
        let budget = JobBudget::new(self.reserve_terrain(&recipe));
        let terrain = with_job_budget(Some(budget.clone()), || Task::start(
            &recipe,
            preview::Presentation {
                live: self.live_generation,
                detailed: true,
            },
            root.clone(),
            self.preview.as_ref(),
            palette,
            true,
            ctx.clone(),
        ));
        self.generation.execution.jobs.insert(id, Extra {
            id,
            budget,
            project: recipe,
            assets: Assets {
                root,
                catalog,
                bindings,
                identities,
                monuments: self.monuments.prepared.clone(),
            },
            started: Instant::now(),
            paused: false,
            frozen: None,
            terrain: Some(terrain),
            preview: None,
            placement: None,
            layout: None,
            composition: Default::default(),
            archive: Default::default(),
            colors: Default::default(),
            overview: None,
            retained_overview: None,
            stop_overview: Default::default(),
            stage: "Preparing terrain".into(),
            live: None,
            color_preferences: self.preferences.map_colors.clone(),
        });
        self.generation
            .queue
            .iter_mut()
            .find(|item| item.id == id)
            .unwrap()
            .state = State::Running;
        self.generation.revision += 1;
        ctx.request_repaint();
    }

    pub(super) fn toggle_queue_job_pause(&mut self, id: u64, ctx: &egui::Context) {
        if self.generation.queue_paused {
            return;
        }
        if self.queue_primary_id() == Some(id) {
            self.generation.execution.primary_paused ^= true;
        } else if let Some(job) = self
            .generation
            .execution
            .jobs.get_mut(&id)
        {
            job.paused ^= true;
        }
        ctx.request_repaint();
    }

    pub(super) fn cancel_extra_queue_job(&mut self, id: u64) -> bool {
        let Some(job) = self.generation.execution.jobs.remove(&id) else {
            return false;
        };
        self.generation.elapsed.insert(id, job.elapsed());
        self.generation.execution.retiring.push(job.budget.clone());
        drop(job);
        self.generation.revision += 1;
        if let Some(item) = self.generation.queue.iter_mut().find(|item| item.id == id) {
            item.state = State::Cancelled;
        }
        true
    }

    pub(super) fn tick_queue_execution(&mut self, ctx: &egui::Context) {
        self.generation.execution.retiring.retain(JobBudget::has_workers);
        if !self.generation.execution.retiring.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        if self.queue_primary_paused() {
            let budget = self.generation.execution.primary_budget.clone();
            with_job_budget(budget, || self.poll_paused_primary(ctx));
        } else {
            if let Some(at) = self.generation.execution.frozen_primary.take()
                && let Some(start) = &mut self.generation.started
            {
                *start += at.elapsed();
            }
            if self.generation.execution.start_pending.is_some() {
                if !self.primary_waits_for_capacity(&self.project) {
                    let reuse_render = self.generation.execution.start_pending.take().unwrap();
                    self.start_generation_candidate(ctx, reuse_render);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(500));
                }
            } else if std::mem::take(&mut self.generation.execution.advance_pending) {
                self.advance_generation(ctx);
            } else if std::mem::take(&mut self.generation.execution.placement_pending) {
                self.start_placement(ctx);
            }
        }
        let jobs = std::mem::take(&mut self.generation.execution.jobs);
        for (id, mut job) in jobs {
            let live = self.live_generation && self.visible_generated_tab() == Some(id);
            let outcome = with_job_budget(Some(job.budget.clone()), || {
                job.tick(ctx, self.generation.queue_paused, live)
            });
            match outcome {
                Ok(None) => {
                    ctx.request_repaint_after(Duration::from_millis(120));
                    self.generation.execution.jobs.insert(id, job);
                }
                outcome => {
                    self.generation.execution.retiring.push(job.budget.clone());
                    self.finish_extra_queue_job(job, outcome, ctx);
                }
            }
        }
        self.start_requested_parallel(ctx);
    }

    fn poll_paused_primary(&mut self, ctx: &egui::Context) {
        if self.composition.running() {
            self.composition.poll(ctx);
            self.publish_composed_layout();
        }
        if self.map_colors.running() {
            self.refresh_map_colors(ctx);
        }
        if self.map_archive.preparing()
            && let (Some(id), Some(view), Some(root), Some(catalog)) = (
                self.queue_primary_id(),
                self.composition.view(&self.project),
                self.source.bound_root_ref(&self.project),
                self.prefabs.catalog(&self.project),
            )
        {
            let _ = self.map_archive.prepare(
                id,
                view,
                map_archive::Assets {
                    root: root.to_owned(),
                    catalog,
                    identities: self.prefabs.prepared_identities(&self.project, root),
                },
                ctx,
            );
        }
        if self.generation.overview.is_some()
            && let Some(id) = self.queue_primary_id()
        {
            let _ = self.retain_generated_overview(id, ctx);
        }
        if self.task.is_none()
            && self.placement_task.is_none()
            && !self.composition.running()
            && self.generation.overview.is_none()
            && !self.map_archive.preparing()
            && !self.map_colors.running()
        {
            self.generation
                .execution
                .frozen_primary
                .get_or_insert_with(Instant::now);
        }
    }

    pub(super) fn start_requested_parallel(&mut self, ctx: &egui::Context) {
        let execution = &mut self.generation.execution;
        let queue = &self.generation.queue;
        execution.requested_parallel.retain(|id| {
            queue.iter().any(|item| item.id == *id && item.state == State::Waiting)
        });
        if !queue.iter().any(|item| item.state == State::Waiting) {
            execution.drain_parallel = false;
            return;
        }
        if self.generation.queue_paused || execution.start_pending.is_some() || execution.advance_pending {
            return;
        }
        loop {
            let execution = &self.generation.execution;
            let queue = &self.generation.queue;
            let requested = execution.next_primary
                .filter(|id| execution.requested_parallel.contains(id))
                .and_then(|id| queue.iter().find(|item| item.id == id && self.eligible_queue_job(id)))
                .or_else(|| queue.iter().find(|item| {
                    execution.requested_parallel.contains(&item.id) && self.eligible_queue_job(item.id)
                }));
            let selected = requested.or_else(|| {
                execution.drain_parallel.then(|| queue.iter().find(|item| self.eligible_queue_job(item.id))).flatten()
            });
            let Some(item) = selected else { break; };
            if self.capacity_blocker(&item.recipe, true).is_some() {
                ctx.request_repaint_after(Duration::from_millis(500));
                break;
            }
            let id = item.id;
            self.generation.execution.requested_parallel.remove(&id);
            if self.generation.execution.next_primary == Some(id) {
                self.generation.execution.next_primary = None;
            }
            self.start_queue_job(id, ctx);
        }
    }

    fn finish_extra_queue_job(
        &mut self,
        job: Extra,
        outcome: Result<Option<Completed>, String>,
        ctx: &egui::Context,
    ) {
        let id = job.id;
        self.generation.revision += 1;
        self.generation.elapsed.insert(id, job.elapsed());
        let outcome =
            outcome.and_then(|done| done.ok_or("Generation ended without an output".into()));
        let full = self.generation.results.len() >= MAX_BATCH;
        let eviction = if full {
            self.map_eviction_index()
        } else {
            None
        };
        let outcome = if full && eviction.is_none() {
            Err("Map storage is full. Apply or discard a draft to free a slot.".into())
        } else {
            outcome
        };
        let Some(item) = self.generation.queue.iter_mut().find(|item| item.id == id) else {
            return;
        };
        let mut done = match outcome {
            Ok(done) => done,
            Err(error) => {
                item.state = State::Failed;
                item.detail = error;
                retain_failed_recipe(self.settings_path.as_deref(), item);
                return;
            }
        };
        item.state = if done.issue.is_some() {
            State::Incomplete
        } else {
            State::Ready
        };
        item.detail = done.issue.clone().unwrap_or_default();
        if item.state == State::Incomplete {
            retain_failed_recipe(self.settings_path.as_deref(), item);
        } else if let Some(layout) = done.road_layout.take() {
            self.generation.road_layouts.insert(id, Arc::new(layout));
        }
        let label = item.label.clone();
        for m in &mut done.monuments {
            m.name = self.monument_icons.name(m.prefab.as_deref(), &m.name);
        }
        let placed = done
            .monuments
            .iter()
            .filter(|m| !m.is_scenery_marker())
            .count();
        if let Some(index) = eviction {
            let old = self.generation.results.remove(index);
            self.generation.edits.remove(&old.id);
            self.generation.road_layouts.remove(&old.id);
            self.generation
                .resident_previews
                .retain(|&saved| saved != old.id);
            self.remove_generated_tab(old.id);
        }
        self.generation.results.push(Candidate {
            identity: None,
            overview: Some(done.overview),
            archive: done.archive,
            archive_issue: done.issue.unwrap_or_default(),
            id,
            label,
            recipe: job.project.clone(),
            image: Some(Ok(ctx.load_texture(
                format!("generated-map-{id}"),
                done.image,
                egui::TextureOptions::LINEAR,
            ))),
            zoom: 1.,
            pan: Vec2::ZERO,
            placed,
            missing: done.missing,
            monuments: done.monuments,
            routes: done.routes,
            restrictions: self.restrictions.for_export(),
            grid: self.grid,
            no_build: self.show_no_build,
            map_font: self
                .monument_icons
                .labels
                .source(job.project.installation.as_ref()),
            background: done.background,
            palette: done.palette,
        });
        // Evicted results cannot advertise a map whose retained data was removed.
        for item in &mut self.generation.queue {
            if item.state.retains_result()
                && !self.generation.results.iter().any(|map| map.id == item.id)
            {
                item.state = State::Failed;
                item.detail = "Map result was removed from workspace storage".into();
            }
        }
        self.generation.remember_preview(id);
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_cancelled_workers(app: &Studio) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.generation.execution.retiring.iter().any(JobBudget::has_workers) {
            assert!(Instant::now() < deadline, "Cancelled map worker did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn map_admission_respects_cpu_memory_and_explicit_cap() {
        let limits = Limits { cpu_workers: 1, ..Limits::default() };
        assert_eq!(limits.concurrent_maps, None);
        assert_eq!(admission_blocker(limits, [].into_iter(), 64, false), None);
        assert_eq!(admission_blocker(limits, [64].into_iter(), 64, false), Some("Waiting for CPU workers"));

        let capped = Limits { concurrent_maps: Some(1), ..limits };
        assert_eq!(admission_blocker(capped, [64].into_iter(), 64, false), Some("Waiting for a generation slot"));

        if mapper_studio::generation_resources::available_workers() >= 4 {
            let uncapped = Limits { cpu_workers: 4, terrain_memory_mib: 256, ..limits };
            assert_eq!(admission_blocker(uncapped, [64, 64, 64].into_iter(), 64, false), None);
            assert_eq!(admission_blocker(uncapped, [64, 64, 64].into_iter(), 65, false), Some("Waiting for terrain memory"));
        }
    }

    #[test]
    fn automatic_admission_waits_at_the_ram_target_and_reopens_with_headroom() {
        let limits = Limits { cpu_workers: 8, terrain_memory_mib: 990, ..Limits::default() };
        assert_eq!(
            admission_blocker(limits, std::iter::empty(), 997, true),
            Some("Waiting for system RAM"),
        );
        let limits = Limits { terrain_memory_mib: 4096, ..limits };
        if mapper_studio::generation_resources::available_workers() >= 4 {
            assert_eq!(admission_blocker(limits, [997; 3].into_iter(), 997, true), None);
        }
    }

    #[test]
    fn cancelled_map_keeps_capacity_until_its_worker_releases_the_budget() {
        use mapper_studio::generation_resources::{current, job_worker};

        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        app.preferences.generation_resources.concurrent_maps = Some(1);
        let recipe = app.document_recipe().clone();
        let budget = JobBudget::new(384);
        let worker = with_job_budget(Some(budget.clone()), || job_worker(|| current().terrain_memory_mib));
        app.generation.execution.retiring.push(budget);
        assert_eq!(app.capacity_blocker(&recipe, true), Some("Waiting for a generation slot"));
        assert_eq!(worker(), 384);
        assert_eq!(app.capacity_blocker(&recipe, true), None);
    }

    #[test]
    fn uncapped_run_admits_more_than_two_and_lowering_cap_keeps_active_maps() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        if mapper_studio::generation_resources::available_workers() < 4 {
            return;
        }
        app.preferences.generation_resources = Limits {
            cpu_workers: 4, terrain_memory_mib: 256, ..Limits::default()
        };
        let recipe = app.document_recipe().clone();
        app.submit_generation(app.recipe_tabs.active(), BatchPlan {
            count: 5, nonce: 483, random_master: true, ..BatchPlan::exact(recipe.clone())
        }, Submission::Run, &ctx);
        assert_eq!(app.generation.execution.jobs.len(), 3);
        assert!(app.generation.execution.primary_budget.is_some());
        let pending = app.generation.queue.last().unwrap().id;
        assert_eq!(app.queue_wait_caption(pending), Some("Waiting for CPU workers"));

        app.preferences.generation_resources.concurrent_maps = Some(2);
        app.start_requested_parallel(&ctx);
        assert_eq!(app.generation.execution.jobs.len(), 3);
        assert_eq!(app.queue_wait_caption(pending), Some("Waiting for a generation slot"));
        app.cancel_generation();
    }

    #[test]
    fn run_batch_uses_free_lanes_without_draining_the_ordinary_queue() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        if mapper_studio::generation_resources::available_workers() < 2 {
            return;
        }
        app.preferences.generation_resources.cpu_workers = 2;
        let recipe_id = app.recipe_tabs.active();
        let recipe = app.document_recipe().clone();
        let batch = |count, nonce| BatchPlan {
            count,
            nonce,
            random_master: true,
            ..BatchPlan::exact(recipe.clone())
        };
        app.submit_generation(recipe_id, batch(2, 181), Submission::Queue, &ctx);
        let first = app.generation.queue[0].id;
        let ordinary = app.generation.queue[1].id;
        assert_eq!(app.queue_primary_id(), Some(first));
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert!(app.generation.execution.jobs.is_empty());

        app.submit_generation(recipe_id, batch(3, 733), Submission::Run, &ctx);
        let run: Vec<_> = app.generation.queue[2..].iter().map(|item| item.id).collect();
        assert!(app.generation.execution.jobs.contains_key(&run[0]));
        assert_eq!(app.queue_primary_id(), Some(first));
        assert_eq!(app.generation.queue[1].state, State::Waiting);

        assert!(app.cancel_extra_queue_job(run[0]));
        wait_for_cancelled_workers(&app);
        app.start_requested_parallel(&ctx);
        assert!(app.generation.execution.jobs.contains_key(&run[1]));
        assert!(app.generation.execution.requested_parallel.contains(&run[2]));
        assert_eq!(app.generation.queue[1].id, ordinary);
        assert_eq!(app.generation.queue[1].state, State::Waiting);

        app.cancel_queued_map(first, &ctx);
        wait_for_cancelled_workers(&app);
        if app.generation.execution.advance_pending {
            app.advance_generation(&ctx);
        }
        assert_eq!(app.queue_primary_id(), Some(run[2]));
        assert!(app.generation.queue.iter().any(|item| item.id == ordinary && item.state == State::Waiting));
        assert!(!app.generation.execution.requested_parallel.contains(&run[2]));

        app.submit_generation(recipe_id, batch(2, 812), Submission::Run, &ctx);
        let waiting = app.generation.queue.last().unwrap().id;
        assert!(app.generation.execution.requested_parallel.contains(&waiting));
        app.cancel_queued_map(waiting, &ctx);
        assert!(!app.generation.execution.requested_parallel.contains(&waiting));
        app.run_queued_map_next(ordinary, &ctx);
        app.cancel_queued_map(run[2], &ctx);
        wait_for_cancelled_workers(&app);
        if app.generation.execution.advance_pending {
            app.advance_generation(&ctx);
        }
        assert_eq!(app.queue_primary_id(), Some(ordinary));
        app.cancel_generation();
        assert!(app.generation.execution.requested_parallel.is_empty());
    }

    #[test]
    fn paused_queue_accepts_a_batch_without_starting_or_partially_adding_it() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        app.generation.queue_paused = true;
        let recipe_id = app.recipe_tabs.active();
        let recipe = app.document_recipe().clone();
        let batch = |count| BatchPlan {
            count,
            nonce: 181,
            random_master: true,
            ..BatchPlan::exact(recipe.clone())
        };
        app.submit_generation(recipe_id, batch(2), Submission::Run, &ctx);
        assert!(app.generation.queue.is_empty());
        assert!(app.generation.queue_paused);
        app.submit_generation(recipe_id, batch(2), Submission::Queue, &ctx);
        assert_eq!(app.generation.queue.len(), 2);
        assert!(app.task.is_none());
        app.submit_generation(recipe_id, batch(MAX_BATCH - 1), Submission::Queue, &ctx);
        assert_eq!(app.generation.queue.len(), 2);
        assert!(app.status.contains("too few queue slots"));
        assert!(app.generation.queue_paused);
        app.cancel_generation();
    }

    #[test]
    fn queue_pause_holds_a_new_batch_until_resumed() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        app.generation.queue_paused = true;
        let recipe = app.project.clone();
        app.begin_generation(BatchPlan::exact(recipe), &ctx);
        assert!(app.generation.busy());
        assert!(app.task.is_none());
        app.tick_generation(&ctx);
        assert!(app.task.is_none());
        app.apply_queue_action(Some(QueueAction::ToggleQueuePause), &ctx);
        app.tick_generation(&ctx);
        assert!(app.task.is_some());
        app.cancel_generation();
    }

    #[test]
    fn queue_starts_an_independent_map_and_respects_global_and_individual_pause() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        if mapper_studio::generation_resources::available_workers() < 2 {
            return;
        }
        app.preferences.generation_resources.cpu_workers = 2;
        let recipe = app.project.clone();
        app.begin_generation(
            BatchPlan {
                count: 3,
                nonce: 717,
                random_master: true,
                ..BatchPlan::exact(recipe.clone())
            },
            &ctx,
        );
        let ids: Vec<_> = app.generation.queue.iter().map(|job| job.id).collect();
        let first_seed = app.project.master_seed;
        app.start_queue_job(ids[2], &ctx);
        assert!(app.generation.execution.jobs.contains_key(&ids[2]));
        assert_eq!(app.queue_primary_id(), Some(ids[0]));
        assert_eq!(app.project.master_seed, first_seed);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert!(app.capacity_blocker(&recipe, true).is_some());

        app.toggle_queue_job_pause(ids[2], &ctx);
        app.apply_queue_action(Some(QueueAction::ToggleQueuePause), &ctx);
        assert!(app.queue_job_paused(ids[0]));
        assert!(app.queue_job_paused(ids[2]));
        app.cancel_queued_map(ids[0], &ctx);
        assert!(app.generation.execution.advance_pending);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        app.apply_queue_action(Some(QueueAction::ToggleQueuePause), &ctx);
        wait_for_cancelled_workers(&app);
        app.tick_queue_execution(&ctx);
        assert_eq!(app.queue_primary_id(), Some(ids[1]));
        assert!(!app.queue_job_paused(ids[1]));
        assert!(app.queue_job_paused(ids[2]));
        app.cancel_queued_map(ids[2], &ctx);
        assert_eq!(app.generation.queue[2].state, State::Cancelled);
        assert_eq!(app.generation.queue[1].state, State::Running);
        app.cancel_generation();
        assert!(
            app.generation
                .queue
                .iter()
                .all(|job| job.state == State::Cancelled)
        );
        assert_eq!(app.document_recipe(), &recipe);
    }

    #[test]
    fn parallel_maps_publish_separate_results_and_skip_the_already_started_waiting_job() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        if mapper_studio::generation_resources::available_workers() < 2 {
            return;
        }
        app.preferences.generation_resources.cpu_workers = 2;
        let recipe = app.project.clone();
        app.begin_generation(
            BatchPlan {
                count: 2,
                nonce: 303,
                random_master: true,
                ..BatchPlan::exact(recipe.clone())
            },
            &ctx,
        );
        let seeded: HashMap<_, _> = app
            .generation
            .queue
            .iter()
            .map(|item| (item.id, item.recipe.master_seed))
            .collect();
        app.apply_queue_action(Some(QueueAction::StartAll), &ctx);
        app.tick_queue_execution(&ctx);
        assert_eq!(
            app.generation
                .queue
                .iter()
                .filter(|item| item.state == State::Running)
                .count(),
            2
        );
        let deadline = Instant::now() + Duration::from_secs(55);
        while app.generation.busy() || !app.generation.execution.jobs.is_empty() {
            let mut frame = ctx.run_ui(egui::RawInput::default(), |_| {
                app.poll(&ctx);
                app.poll_placement(&ctx);
                app.tick_generation(&ctx);
            });
            frame.textures_delta.clear();
            assert!(
                Instant::now() < deadline,
                "generation stalled: {}",
                app.status
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(app.generation.results.len(), 2, "{}", app.status);
        for result in &app.generation.results {
            assert_eq!(result.recipe.master_seed, seeded[&result.id]);
            assert!(result.overview.is_some());
        }
        assert_eq!(app.document_recipe(), &recipe);
    }
}
