use super::*;
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
    pub(super) extra: Option<Extra>,
}

impl QueueExecution {
    pub(super) fn extra_id(&self) -> Option<u64> {
        self.extra.as_ref().map(|job| job.id)
    }
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
            .extra
            .as_ref()
            .is_some_and(|job| job.id == id && (self.generation.queue_paused || job.paused))
    }

    pub(super) fn queue_pause_caption(&self, id: u64) -> &'static str {
        let frozen = if self.queue_primary_id() == Some(id) {
            self.generation.execution.frozen_primary.is_some()
        } else {
            self.generation
                .execution
                .extra
                .as_ref()
                .is_some_and(|job| job.id == id && job.frozen.is_some())
        };
        if frozen { "Paused" } else { "Pausing…" }
    }

    pub(super) fn queue_elapsed(&self, id: u64) -> Option<Duration> {
        if let Some(job) = self
            .generation
            .execution
            .extra
            .as_ref()
            .filter(|job| job.id == id)
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
            .extra
            .as_ref()
            .filter(|job| job.id == id)
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
            .extra
            .as_ref()
            .filter(|job| job.id == id)
            .and_then(|job| job.live.as_ref())
    }

    pub(super) fn primary_waits_for_extra(&self) -> bool {
        self.generation.execution.extra.is_some()
            && self.preferences.generation_resources.effective_workers() < 2
    }

    fn extra_lane_available(&self) -> bool {
        !self.generation.queue_paused
            && self.generation.execution.extra.is_none()
            && self.preferences.generation_resources.effective_workers() >= 2
    }

    pub(crate) fn run_blocker(&self, count: usize) -> Option<&'static str> {
        if self.generation.queue_paused {
            return Some("Resume the queue before starting maps.");
        }
        let slot_available = if self.generation.busy() {
            self.extra_lane_available()
        } else {
            !self.primary_waits_for_extra()
        };
        (count == 1 && !slot_available).then_some("No generation slot is available. Use Queue to add this map.")
    }

    pub(super) fn can_start_queue_job(&self, id: u64) -> bool {
        self.extra_lane_available()
            && self.generation.queue.iter().any(|job| job.id == id && job.state == State::Waiting)
            // Applying a map edit uses the accepted route layout owned by the editor.
            && self.generation.edits.values().all(|edit| edit.job != Some(id))
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
        if let Err(error) = self.generation_preflight(&recipe, false) {
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
        let terrain = Task::start(
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
        );
        self.generation.execution.extra = Some(Extra {
            id,
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
            .extra
            .as_mut()
            .filter(|job| job.id == id)
        {
            job.paused ^= true;
        }
        ctx.request_repaint();
    }

    pub(super) fn cancel_extra_queue_job(&mut self, id: u64) -> bool {
        if self
            .generation
            .execution
            .extra
            .as_ref()
            .is_none_or(|job| job.id != id)
        {
            return false;
        }
        let job = self.generation.execution.extra.take().unwrap();
        self.generation.elapsed.insert(id, job.elapsed());
        self.generation.revision += 1;
        if let Some(item) = self.generation.queue.iter_mut().find(|item| item.id == id) {
            item.state = State::Cancelled;
        }
        true
    }

    pub(super) fn tick_queue_execution(&mut self, ctx: &egui::Context) {
        if self.queue_primary_paused() {
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
        } else {
            if let Some(at) = self.generation.execution.frozen_primary.take()
                && let Some(start) = &mut self.generation.started
            {
                *start += at.elapsed();
            }
            if self.generation.execution.start_pending.is_some() {
                if !self.primary_waits_for_extra() {
                    let reuse_render = self.generation.execution.start_pending.take().unwrap();
                    self.start_generation_candidate(ctx, reuse_render);
                }
            } else if std::mem::take(&mut self.generation.execution.advance_pending) {
                self.advance_generation(ctx);
            } else if std::mem::take(&mut self.generation.execution.placement_pending) {
                self.start_placement(ctx);
            }
        }
        if let Some(mut job) = self.generation.execution.extra.take() {
            let live = self.live_generation && self.visible_generated_tab() == Some(job.id);
            match job.tick(ctx, self.generation.queue_paused, live) {
                Ok(None) => {
                    ctx.request_repaint_after(Duration::from_millis(120));
                    self.generation.execution.extra = Some(job);
                }
                outcome => self.finish_extra_queue_job(job, outcome, ctx),
            }
        }
        self.start_requested_parallel(ctx);
        let execution = &self.generation.execution;
        if !self.generation.queue_paused && execution.extra.is_none()
            && (execution.start_pending.is_some() || execution.advance_pending)
        {
            ctx.request_repaint();
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
        if !self.extra_lane_available() {
            return;
        }
        let execution = &self.generation.execution;
        let requested = self.generation.queue.iter().find(|item| {
            execution.requested_parallel.contains(&item.id) && self.can_start_queue_job(item.id)
        });
        let selected = requested.or_else(|| {
            if execution.drain_parallel {
                self.generation.queue.iter().find(|item| self.can_start_queue_job(item.id))
            } else {
                None
            }
        }).map(|item| item.id);
        if let Some(id) = selected {
            self.generation.execution.requested_parallel.remove(&id);
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
        assert!(app.generation.execution.extra.is_none());

        app.submit_generation(recipe_id, batch(3, 733), Submission::Run, &ctx);
        let run: Vec<_> = app.generation.queue[2..].iter().map(|item| item.id).collect();
        assert_eq!(app.generation.execution.extra_id(), Some(run[0]));
        assert_eq!(app.queue_primary_id(), Some(first));
        assert_eq!(app.generation.queue[1].state, State::Waiting);

        app.submit_generation(recipe_id, BatchPlan::exact(recipe.clone()), Submission::Run, &ctx);
        assert_eq!(app.generation.queue.len(), 5);
        assert!(app.status.contains("No generation slot"));

        assert!(app.cancel_extra_queue_job(run[0]));
        app.start_requested_parallel(&ctx);
        assert_eq!(app.generation.execution.extra_id(), Some(run[1]));
        assert!(app.generation.execution.requested_parallel.contains(&run[2]));
        assert_eq!(app.generation.queue[1].id, ordinary);
        assert_eq!(app.generation.queue[1].state, State::Waiting);

        app.cancel_queued_map(first, &ctx);
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
        assert_eq!(app.generation.execution.extra_id(), Some(ids[2]));
        assert_eq!(app.queue_primary_id(), Some(ids[0]));
        assert_eq!(app.project.master_seed, first_seed);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert!(!app.can_start_queue_job(ids[1]));

        app.toggle_queue_job_pause(ids[2], &ctx);
        app.apply_queue_action(Some(QueueAction::ToggleQueuePause), &ctx);
        assert!(app.queue_job_paused(ids[0]));
        assert!(app.queue_job_paused(ids[2]));
        app.cancel_queued_map(ids[0], &ctx);
        assert!(app.generation.execution.advance_pending);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        app.apply_queue_action(Some(QueueAction::ToggleQueuePause), &ctx);
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
        while app.generation.busy() || app.generation.execution.extra.is_some() {
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
